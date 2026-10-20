//! One shell, for its whole life, and the threads that keep it alive.
//!
//! `TerminalCore` knows how to turn bytes into a grid. It has no idea how those
//! bytes arrive, how keys leave, or when the shell behind them is gone, and it
//! cannot know any of that: it is owned by one thread and it is handed a slice.
//! This is the other half. It owns the reused PTY backend, hands the reader's
//! bytes straight to the parser, and answers the four questions the view keeps
//! asking -- what is on screen, what is in front of the shell, is the stream
//! still alive, may I still type.
//!
//! The shape of it, which is the whole point of the file:
//!
//! ```text
//! PTY reader thread (the backend's own)
//!   └─ output sink ──> TerminalCore::feed, then its events
//!                        ├─ protocol replies ──> WriteQueue ──> backend.write
//!                        ├─ clipboard, colours, sizes ──> host callbacks
//!                        └─ one coalesced wake ──> the view
//! resize worker        newest size only, dropped if its generation is stale
//! status poller        the child's own answers, on its own cadence
//! ```
//!
//! No thread here blocks forever on something that has gone away: every worker
//! waits on a condvar it is woken from and every send is a `try_send` on a
//! channel with room for one. That is not tidiness. A reader thread that blocks
//! on a full channel stops draining the PTY, and a PTY that stops being drained
//! stops a child mid-write, which is exactly the hang this design exists to
//! avoid.
//!
//! The three pieces of state that are easy to collapse into one boolean and must
//! not be: whether the *shell* has exited, whether something is running *in
//! front of* the shell, and whether the *output stream* has ended. A child can
//! exit while its last output is still draining, a reader can stop while the
//! child runs on, and only the backend knows which of the two happened.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::core::{CursorSnapshot, PromptAnchor, RowSnapshot, TerminalCore, TerminalEvent};
use super::keys::{encode_key, KeyPress};
use super::modes::TerminalModes;
use super::paste::{encode_paste, encode_paste_confirmed, PasteError};
use super::pi_status::{self, PiTaskState, PiTracker};
use super::write_queue::{EnqueueError, WriteQueue};
use crate::pty::{OutputSink, ReaderEnd, ReaderEndSink, SpawnOptions, TerminalBackend};
use crate::workspace::TerminalProcessState;

/// The argument that makes a shell a *login* shell on every shell this app can
/// spawn: `zsh`, `bash`, `sh` and `fish` all take it and none of them treats it
/// as an interactive-shell request.
const LOGIN_FLAG: &str = "-l";

/// Scrollback when the host does not choose.
pub const DEFAULT_SCROLLBACK_LINES: usize = 10_000;

/// How often the child's status is read when the host does not choose.
///
/// Short enough that a shell exiting is noticed while the person is still
/// looking at it, long enough that asking the OS about the foreground group
/// twice a second is not a cost anybody would notice.
pub const DEFAULT_STATUS_INTERVAL: Duration = Duration::from_millis(250);

/// How long a close waits for input and resizes already in flight.
///
/// The same second the Tauri frontend spends draining its own input queue, and
/// bounded on purpose: a child that has stopped reading its input must not be
/// able to hold a window open.
pub const DEFAULT_DRAIN_DEADLINE: Duration = Duration::from_secs(1);

/// How long a protocol reply waits for room in the shared byte budget.
///
/// A reply the child is blocked on is not allowed to be dropped, and it is not
/// allowed to bypass the budget either -- bypassing it is how a paste of a
/// megabyte and a size answer in the same instant turn into a PTY write that
/// neither of them survives. Waiting is the answer; giving up after a second is
/// the bound on that waiting, because the child that asked is the one that will
/// hang, and it hangs on a PTY that is closing.
const REPLY_ROOM_DEADLINE: Duration = Duration::from_secs(1);

/// How often a reply looks for room again.
const REPLY_ROOM_RETRY: Duration = Duration::from_millis(2);

/// How often a close looks at what is still in flight.
const DRAIN_POLL: Duration = Duration::from_millis(2);

/// How many program-status replies one session hands to its child.
///
/// The reply is a handshake, and a handshake is not a conversation: a program
/// that keeps asking is not a program asking once. The bound is what keeps a
/// terminal from being written to by its own output, and it is far past the
/// number of times a person starts Pi in one shell.
const MAX_PI_STATUS_REPLIES: u64 = 64;

// ---------------------------------------------------------------------------------------------
// What the host has to answer
// ---------------------------------------------------------------------------------------------

/// A cell's size in pixels, as the child measuring its text area in pixels will
/// want it.
///
/// The host's because a font is the view's: the parser knows the grid is 80 by
/// 24 and nothing at all about how wide those 80 columns are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellMetrics {
    pub width: u16,
    pub height: u16,
}

/// Everything the controller cannot answer for itself.
///
/// Injected rather than imported, so this file has no idea whether the host
/// writes to a real clipboard, whether it has a palette, or how it measures a
/// cell -- and so a headless test can answer all three without a window.
pub struct TerminalHost {
    /// OSC 52. Handed the payload, which is already bounded by the parser.
    ///
    /// Nothing here decides whether the write is allowed; the host is called
    /// with the text and is free to refuse it, which is the whole reason this is
    /// a callback rather than a call into a clipboard.
    pub store_clipboard: Box<dyn Fn(&str) + Send + Sync>,
    /// One xterm palette entry by index, as `(r, g, b)`.
    ///
    /// Called for every `ColorRequest`, including the ones the parser could
    /// have answered itself: the terminal's own palette is a preference, and
    /// only the host holds it.
    pub resolve_color: Box<dyn Fn(usize) -> (u8, u8, u8) + Send + Sync>,
    /// The current cell size, asked again for every query rather than cached:
    /// a font size can change under a window that is still open.
    pub cell_metrics: Box<dyn Fn() -> CellMetrics + Send + Sync>,
}

impl Default for TerminalHost {
    /// A host that writes nothing anywhere and claims a cell size nothing has
    /// measured.
    ///
    /// Useful in a test, useless in an app: the defaults answer the queries a
    /// child makes so it stops waiting, and refuse every clipboard write, which
    /// is the only safe answer to give by default.
    fn default() -> Self {
        Self {
            store_clipboard: Box::new(|_| {}),
            resolve_color: Box::new(|_| (0, 0, 0)),
            cell_metrics: Box::new(|| CellMetrics {
                width: 8,
                height: 16,
            }),
        }
    }
}

// ---------------------------------------------------------------------------------------------

const OSC_STARTED: &str = r"\e]133;C\a";
const OSC_EXIT_BASH: &str = r"\033]133;D;%s\007";
const OSC_STARTED_BASH: &str = r"\033]133;C\007";
const OSC_INPUT_STARTED_ZSH: &str = r"\e]133;B\a";
const OSC_INPUT_STARTED_BASH: &str = r"\\[\033]133;B\007\\]";

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

type ShellSpawn = (PathBuf, Vec<String>, Vec<(OsString, OsString)>);

fn shell_integration(
    program: &Path,
    config: &TerminalSessionConfig,
) -> Option<(ShellSpawn, tempfile::TempDir)> {
    if config.args.is_some() {
        return None;
    }
    let shell = program.file_name()?.to_str()?;
    if shell == "bash" && std::env::var_os("POSIXLY_CORRECT").is_some() {
        return None;
    }
    let (filename, script) = match shell {
        "zsh" => ("hook.zsh", format!("if [[ -z ${{__muster_integrated-}} ]]; then\ntypeset -g __muster_integrated=1 __muster_pending=0\nmuster_pc() {{ local __muster_status=$?; (( __muster_pending )) || __muster_status=0; __muster_pending=0; printf '{OSC_EXIT_BASH}' \"$__muster_status\"; local input=$'{OSC_INPUT_STARTED_ZSH}'; [[ $PS1 == *\"$input\"* ]] || PS1=\"$PS1%{{$input%}}\"; }}\nmuster_px() {{ __muster_pending=1; print -Pn \"{OSC_STARTED}\"; }}\nprecmd_functions=(${{precmd_functions:#muster_pc}} muster_pc)\npreexec_functions+=(muster_px)\nfi\n")),
        "bash" if std::env::var_os("POSIXLY_CORRECT").is_none() => ("hook.bash", format!("if [[ -z ${{__muster_integrated-}} ]]; then\n__muster_integrated=1; __muster_first_prompt=1\n__muster_prompt_begin() {{ local status=$?; if [[ $__muster_first_prompt == 1 ]]; then [[ ${{__muster_restore_posix-}} != on ]] || set -o posix; unset __muster_restore_posix; status=0; __muster_first_prompt=0; fi; __muster_command_status=$status; return \"$status\"; }}\n__muster_prompt_end() {{ printf '{OSC_EXIT_BASH}' \"$__muster_command_status\"; local input=$'{OSC_INPUT_STARTED_BASH}'; [[ ${{PS1-}} == *\"$input\"* ]] || PS1=\"${{PS1-}}$input\"; }}\nif [[ ${{BASH_VERSINFO[0]}} -gt 5 || ${{BASH_VERSINFO[0]}} -eq 5 && ${{BASH_VERSINFO[1]}} -ge 1 ]] && [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a'* ]]; then PROMPT_COMMAND=(__muster_prompt_begin \"${{PROMPT_COMMAND[@]}}\" __muster_prompt_end); else PROMPT_COMMAND=\"__muster_prompt_begin${{PROMPT_COMMAND:+; $PROMPT_COMMAND}}; __muster_prompt_end\"; fi\nprintf -v PS0 '%s' '{OSC_STARTED_BASH}'\"${{PS0-}}\"\nfi\n")),
        _ => return None,
    };
    let temp_dir = tempfile::TempDir::new().ok()?;
    let dir = temp_dir.path();
    let script_path = dir.join(filename);
    fs::write(&script_path, script).ok()?;
    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o600)).ok()?;
    let mut args = vec![LOGIN_FLAG.into()];
    let mut env = config.env.clone();
    if shell == "zsh" {
        let bootstrap = dir.join("zsh");
        for file in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
            let mut text = String::new();
            if file == ".zshenv" {
                text.push_str(
                    "typeset -g __muster_bootstrap=$ZDOTDIR __muster_zdotdir=$__MUSTER_ZDOTDIR\n",
                );
            }
            text.push_str("if [[ $__MUSTER_ZDOTDIR_SET == 1 ]]; then export ZDOTDIR=$__MUSTER_ZDOTDIR; else unset ZDOTDIR; fi\n");
            text.push_str(&format!(
                "[[ ! -r ${{ZDOTDIR-$HOME}}/{file} ]] || source \"${{ZDOTDIR-$HOME}}/{file}\"\n"
            ));
            text.push_str("__MUSTER_ZDOTDIR_SET=${+ZDOTDIR}\n__MUSTER_ZDOTDIR=${ZDOTDIR-}\n");
            if file == ".zlogin" {
                text.push_str(&format!(
                    "unset __MUSTER_ZDOTDIR_SET __MUSTER_ZDOTDIR\nsource {}\n",
                    shell_quote(&script_path)
                ));
            } else if file == ".zshrc" {
                text.push_str(&format!("if [[ -o login && -o rcs ]]; then export ZDOTDIR=$__muster_bootstrap; else unset __MUSTER_ZDOTDIR_SET __MUSTER_ZDOTDIR; source {}; fi\n", shell_quote(&script_path)));
            } else {
                text.push_str("if [[ -o rcs ]]; then export ZDOTDIR=$__muster_bootstrap; else unset __MUSTER_ZDOTDIR_SET __MUSTER_ZDOTDIR __muster_bootstrap; fi\n");
            }
            let path = bootstrap.join(file);
            if file == ".zshenv" {
                fs::create_dir(&bootstrap).ok()?;
            }
            fs::write(&path, text).ok()?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).ok()?;
        }
        let zdotdir = config
            .env
            .iter()
            .find(|(key, _)| key == "ZDOTDIR")
            .map(|(_, value)| value.clone())
            .or_else(|| std::env::var_os("ZDOTDIR"));
        let mut zdotdir_env = vec![
            (
                OsString::from("__MUSTER_ZDOTDIR_SET"),
                if zdotdir.is_some() { "1" } else { "0" }.into(),
            ),
            (
                OsString::from("__MUSTER_ZDOTDIR"),
                zdotdir.unwrap_or_default(),
            ),
            (OsString::from("ZDOTDIR"), bootstrap.into_os_string()),
        ];
        env.append(&mut zdotdir_env);
        Some(((program.to_path_buf(), args, env), temp_dir))
    } else {
        #[cfg(unix)]
        {
            let link = dir.join("bash-sh");
            let target = fs::canonicalize(program).ok()?;
            if fs::read_link(&link).as_ref().ok() != Some(&target)
                && symlink(&target, &link).is_err()
                && fs::read_link(&link).ok() != Some(target)
            {
                return None;
            }
        }
        #[cfg(not(unix))]
        {
            return None;
        }
        let bootstrap = dir.join("bootstrap.bash");
        let text = format!("# Private Bash startup bridge.\nset +o posix\nBASH=$__MUSTER_BASH\nif (( BASH_VERSINFO[0] >= 5 )); then BASH_ARGV0=$BASH; fi\nunset __MUSTER_BASH\nif [[ $__MUSTER_ENV_SET == 1 ]]; then export ENV=$__MUSTER_ENV; else unset ENV; fi\nunset __MUSTER_ENV_SET __MUSTER_ENV\n[[ ! -r /etc/profile ]] || . /etc/profile\nif [[ -r $HOME/.bash_profile ]]; then . $HOME/.bash_profile; elif [[ -r $HOME/.bash_login ]]; then . $HOME/.bash_login; elif [[ -r $HOME/.profile ]]; then . $HOME/.profile; fi\nif shopt -qo posix; then __muster_restore_posix=on; else __muster_restore_posix=off; fi\n. {}\n", shell_quote(&script_path));
        if fs::write(&bootstrap, text).is_err()
            || fs::set_permissions(&bootstrap, fs::Permissions::from_mode(0o600)).is_err()
        {
            return None;
        }
        let environment = config
            .env
            .iter()
            .find(|(key, _)| key == "ENV")
            .map(|(_, value)| value.clone())
            .or_else(|| std::env::var_os("ENV"));
        env.extend([
            (
                OsString::from("__MUSTER_ENV_SET"),
                if environment.is_some() { "1" } else { "0" }.into(),
            ),
            (
                OsString::from("__MUSTER_ENV"),
                environment.unwrap_or_default(),
            ),
            (
                OsString::from("__MUSTER_BASH"),
                program.as_os_str().to_owned(),
            ),
            (OsString::from("ENV"), bootstrap.clone().into_os_string()),
        ]);
        args.clear();
        args.extend([
            "--noprofile".into(),
            "--rcfile".into(),
            bootstrap.to_string_lossy().into_owned(),
        ]);
        Some(((dir.join("bash-sh"), args, env), temp_dir))
    }
}
// Configuration
// ---------------------------------------------------------------------------------------------

/// Everything a session needs to exist. Nothing here has a default that could
/// be wrong silently.
pub struct TerminalSessionConfig {
    /// Stable for the whole session, and the backend's key for it.
    pub id: String,
    /// Where the shell starts.
    ///
    /// Never guessed from this process's own directory: a terminal that opens
    /// wherever the app was launched from is not a terminal, it is a log.
    pub cwd: PathBuf,
    pub cols: u16,
    pub rows: u16,
    /// The program to spawn. `None` is [`login_shell`].
    pub shell: Option<PathBuf>,
    /// Its arguments. `None` is `[LOGIN_FLAG]`, because a login shell is the
    /// point: it is what reads the person's `.zprofile`.
    pub args: Option<Vec<String>>,
    /// Extra environment for this child only.
    pub env: Vec<(OsString, OsString)>,
    pub scrollback_lines: usize,
    /// How often the child's status is read. Never on the caller's thread.
    pub status_interval: Duration,
    /// How long a close waits for input and resizes in flight.
    pub drain_deadline: Duration,
    /// What only the host can answer.
    pub host: TerminalHost,
}

impl TerminalSessionConfig {
    /// A session of `cols` by `rows` in `cwd`, running `shell` as a login
    /// shell. Everything else is the default.
    pub fn new(id: String, cwd: PathBuf, cols: u16, rows: u16, shell: Option<PathBuf>) -> Self {
        Self {
            id,
            cwd,
            cols,
            rows,
            shell,
            args: None,
            env: Vec::new(),
            scrollback_lines: DEFAULT_SCROLLBACK_LINES,
            status_interval: DEFAULT_STATUS_INTERVAL,
            drain_deadline: DEFAULT_DRAIN_DEADLINE,
            host: TerminalHost::default(),
        }
    }
}

/// The shell this person actually uses.
///
/// `$SHELL` when the environment has one, because that is the answer the login
/// manager and every other terminal on the machine would give, and `zsh` on
/// macOS or `bash` elsewhere when it does not -- an empty or missing `$SHELL` is
/// what a GUI app launched from a desktop file inherits, and there is no version
/// of that where spawning nothing is the right answer.
pub fn login_shell() -> PathBuf {
    std::env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                PathBuf::from("/bin/zsh")
            } else {
                PathBuf::from("/bin/bash")
            }
        })
}

// ---------------------------------------------------------------------------------------------
// State the host reads
// ---------------------------------------------------------------------------------------------

/// Whether the shell itself has exited.
///
/// Separate from [`StreamState`] because the two do not happen together: a shell
/// can exit with its last output still in the pipe, and a reader can stop with
/// the shell perfectly alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShellState {
    /// No successful read has happened yet, or the last one failed.
    ///
    /// Not "idle". A read that failed says nothing about the child, and
    /// treating it as an answer is how a killed build gets reported as a
    /// finished one.
    #[default]
    Unknown,
    Running,
    Exited {
        /// The shell's own exit code. This is the shell exiting, not a command
        /// the shell ran returning non-zero: the backend has no opinion about
        /// the second and this does not invent one.
        code: Option<u32>,
    },
}

/// What is running in front of the shell.
///
/// The answer to "is it safe to close this window", which is a question for the
/// person in front of it and not for this file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Foreground {
    /// A process other than the shell holds the terminal.
    pub running: bool,
    /// Its name, when the OS will say.
    pub app: Option<String>,
}

/// Whether what is in front of the shell is Pi.
///
/// The operating system's own answer, and the only one that counts: a terminal
/// may print anything at all, and what says a report came from Pi is that the
/// OS names Pi as the process in front of the shell.
fn foreground_is_pi(foreground: &Foreground) -> bool {
    foreground.running
        && foreground
            .app
            .as_deref()
            .is_some_and(muster_core::pi::is_pi)
}

/// How the output stream ended, if it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamState {
    /// The reader is still delivering bytes.
    Open,
    /// The reader stopped. The reason is kept because "output ended" and
    /// "output failed" are different problems, and a frozen grid that looks
    /// healthy is worse than either.
    Ended(ReaderEnd),
}

/// Everything the host can know about this session right now.
///
/// Deliberately several separate questions. Collapsing them into a
/// `busy: bool` is the mistake this type exists to prevent: a shell that
/// exited, a command that returned non-zero while the shell lives, a TUI in
/// front of the shell and a dead reader all look like one bit to code that only
/// needs one.
///
/// The second of those -- a *command* returning non-zero while its shell lives
/// -- has no field here on purpose. Neither the reused backend nor the parser
/// reports it: the backend knows the shell's own exit code, and OSC 133 is not
/// something `TerminalCore` translates. Adding a slot nothing writes would be a
/// claim about a measurement nobody has made, and the host that eventually gets
/// it from a shell-integration script should own it next to [`ShellState`],
/// not inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStatus {
    /// The generation this was read under. A reading older than this is a
    /// reading about a window that is gone.
    pub generation: u64,
    /// (a) The shell.
    pub shell: ShellState,
    /// (c) What is in front of the shell.
    pub foreground: Foreground,
    /// (d) The output stream.
    pub stream: StreamState,
    /// The last status read that failed, kept so a failure stays visible instead
    /// of being reported as an idle terminal.
    pub poll_error: Option<String>,
    /// What Pi says about the task in front of it, if Pi is what is in front.
    ///
    /// `None` is most of the time, and it means two different things that are
    /// deliberately the same: nothing is running, and what is running is not
    /// Pi. The panel draws a row the same either way, and the difference is
    /// only in what may be believed.
    pub pi_task: Option<PiTaskState>,
    /// Where the shell is, as the operating system records it.
    ///
    /// The shell's own directory and not the foreground program's: a program in
    /// front changes its own directory for reasons that have nothing to do with
    /// where the terminal is. This is a reading, never an instruction: nothing
    /// here feeds a directory to the shell.
    pub working_directory: Option<String>,
}

/// One grid cell as `(row, column)`, in visible screen coordinates.
pub type GridPoint = (usize, usize);
/// An inclusive selection as its two endpoints, first cell included.
pub type GridRange = (GridPoint, GridPoint);

/// What the view draws, gathered in one pass so a frame is one lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSnapshot {
    pub rows: Vec<RowSnapshot>,
    /// Images visible in this viewport, decoded on the reader thread.
    pub graphics: Vec<super::GraphicSnapshot>,
    /// Text selected in the terminal grid, if any.
    pub selection: Option<String>,
    /// Inclusive visible selection range (row, column) endpoints.
    pub selection_range: Option<GridRange>,
    pub cursor: CursorSnapshot,
    pub modes: TerminalModes,
    pub title: Option<String>,
    pub alt_screen: bool,
    pub mouse_tracking: bool,
    pub display_offset: usize,
    pub history_size: usize,
    pub background: Option<(u8, u8, u8)>,
    pub prompt_anchor: Option<PromptAnchor>,
}

/// Why the controller refused something.
///
/// Every variant names what it was refusing rather than returning a bare `bool`,
/// because "input refused" and "the window measured zero by zero" are different
/// bugs and the caller can only report the one it can see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// The backend refused to create the child. Never logged and swallowed: a
    /// terminal that did not open is not a terminal that closed.
    Spawn(String),
    /// A close has begun. Nothing new goes in.
    Closing,
    /// A window measured nothing. An invisible or minimised one measures zero,
    /// and zero is not a size a PTY can be told about.
    InvalidSize { cols: u16, rows: u16 },
    /// The shared input budget was full, or the queue had stopped.
    Queue(EnqueueError),
    /// The paste was refused before it reached the queue.
    Paste(PasteError),
    /// The child's status could not be read.
    ///
    /// Carries the backend's own words, because this is the one failure a
    /// close must not survive: a status read that failed is not a status that
    /// said nothing was running.
    StatusUnavailable(String),
    /// The backend refused a request this session made.
    Backend(String),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "the terminal could not be spawned: {error}"),
            Self::Closing => write!(f, "the terminal session is closing"),
            Self::InvalidSize { cols, rows } => {
                write!(
                    f,
                    "the window measured {cols} by {rows}, which is not a terminal size"
                )
            }
            Self::Queue(error) => write!(f, "the terminal refused queued input: {error:?}"),
            Self::Paste(error) => write!(f, "the paste was refused: {error:?}"),
            Self::StatusUnavailable(error) => {
                write!(f, "the terminal child's status could not be read: {error}")
            }
            Self::Backend(error) => write!(f, "the terminal backend refused: {error}"),
        }
    }
}

impl std::error::Error for SessionError {}

/// What a close found, and what it did.
///
/// The foreground report is here rather than decided here: whether a running
/// `vim` is worth a question before it is killed is the host's call, and this is
/// the information it makes that call with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseReport {
    pub id: String,
    /// The fresh read taken as the close began.
    pub shell: ShellState,
    pub foreground: Foreground,
    pub stream: StreamState,
    /// Whether input and resizes already in flight finished inside the deadline.
    /// `false` means they were invalidated instead, which is a bounded answer
    /// and not a failure.
    pub drained: bool,
    /// What the backend said. `false` means there was nothing left to close,
    /// which is a normal answer for a second close.
    pub closed: bool,
}

// ---------------------------------------------------------------------------------------------
// The wake
// ---------------------------------------------------------------------------------------------

/// The host's end of the coalesced wake channel.
///
/// One wake at a time is the whole contract: a `yes` in the shell is a burst of
/// 64 KiB reads, and one UI task per read is one UI task per 64 KiB for as long
/// as the person keeps it running.
#[derive(Debug)]
pub struct TerminalWake(mpsc::Receiver<()>, Arc<Mutex<Option<std::task::Waker>>>);

impl TerminalWake {
    /// Wait up to `timeout` for the reader to say something changed.
    ///
    /// `false` on timeout and on a closed session, which a UI loop has to treat
    /// as "stop repainting" rather than as an error.
    pub fn wait(&self, timeout: Duration) -> bool {
        matches!(self.0.recv_timeout(timeout), Ok(()))
    }

    /// Take a wake that is already outstanding, without waiting for one.
    pub fn take(&self) -> bool {
        matches!(self.0.try_recv(), Ok(()))
    }

    /// Await a change without occupying a worker or polling an idle terminal.
    pub async fn recv(&self) -> bool {
        std::future::poll_fn(|cx| {
            let mut waker = self.1.lock().expect("terminal wake poisoned");
            match self.0.try_recv() {
                Ok(()) => std::task::Poll::Ready(true),
                Err(mpsc::TryRecvError::Disconnected) => std::task::Poll::Ready(false),
                Err(mpsc::TryRecvError::Empty) => {
                    *waker = Some(cx.waker().clone());
                    std::task::Poll::Pending
                }
            }
        })
        .await
    }
}

impl Drop for TerminalWake {
    fn drop(&mut self) {
        self.1.lock().expect("terminal wake poisoned").take();
    }
}

/// The producer's end.
///
/// A channel with room for one token, and a `try_send` that counts a full
/// channel as success. That is the entire coalescing mechanism: the pending
/// token *is* the promise that there is unsampled output, so a second one would
/// say nothing the first did not, and dropping it is not losing a wake.
#[derive(Debug)]
struct Wake {
    sender: Mutex<Option<mpsc::SyncSender<()>>>,
    waker: Arc<Mutex<Option<std::task::Waker>>>,
    released: AtomicBool,
    /// Wakes that found a token already pending. Diagnostic, and the number
    /// that says whether coalescing is doing anything at all.
    coalesced: AtomicU64,
}

impl Wake {
    fn new() -> (Self, TerminalWake) {
        let (sender, receiver) = mpsc::sync_channel(1);
        let waker = Arc::new(Mutex::new(None));
        (
            Self {
                sender: Mutex::new(Some(sender)),
                waker: Arc::clone(&waker),
                released: AtomicBool::new(false),
                coalesced: AtomicU64::new(0),
            },
            TerminalWake(receiver, waker),
        )
    }

    fn signal(&self) {
        // Checked before the lock: a released session is one where every further
        // signal is work nobody will ever read.
        if self.released.load(Ordering::Acquire) {
            return;
        }
        let held = self.sender.lock().expect("terminal wake poisoned");
        let Some(sender) = held.as_ref() else {
            return;
        };
        match sender.try_send(()) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                self.coalesced.fetch_add(1, Ordering::Relaxed);
            }
            // The view is gone. Nobody to wake, and nothing to report: a
            // session whose window closed must still drain its PTY.
            Err(mpsc::TrySendError::Disconnected(_)) => {}
        }
        drop(held);
        let waker = self.waker.lock().expect("terminal wake poisoned").take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Stop signalling, and disconnect the view's end.
    ///
    /// Dropping the sender is what releases the *consumer*: a view task blocked
    /// in [`TerminalWake::wait`] gets its disconnection instead of sitting on
    /// its timeout for the rest of the session's life, and the reader that
    /// delivers after this cannot stall anything.
    fn release(&self) {
        self.released.store(true, Ordering::Release);
        self.sender.lock().expect("terminal wake poisoned").take();
        let waker = self.waker.lock().expect("terminal wake poisoned").take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------------------------

/// State the reader writes and everyone else reads.
struct Shared {
    /// The parser and the grid.
    ///
    /// Behind a lock rather than shared by hand because three threads need it
    /// and only one of them may *parse* into it: the reader, the resize worker
    /// and the view. The lock is the price of letting the other two look, and it
    /// is a short one -- a feed, a resize, a snapshot copy -- which is what keeps
    /// the reader from waiting behind a repaint.
    parser: Mutex<TerminalCore>,
    /// Monotonic. Work carries the generation it was issued under and is thrown
    /// away when it lands after the counter moved, which is what stops a resize
    /// measured for a window that has been replaced from reaching the PTY of the
    /// window that took its place.
    generation: AtomicU64,
    /// A close has begun: no new input, no new resizes.
    closing: AtomicBool,
    /// (d) Written by the backend's reader thread through `reader_end`.
    stream: Mutex<StreamState>,
    /// (a) and (c), as of the last successful read.
    status: Mutex<PolledStatus>,
    /// A program asked to read the clipboard and was not answered.
    ///
    /// Surfaced, never satisfied: a program asking to read what is on the
    /// system clipboard is a claim about the user's data that this crate cannot
    /// check, so the request becomes a state the host can see and refuse.
    clipboard_requested: AtomicBool,
    /// A reply could not be delivered. Sticky across otherwise successful polls:
    /// the child may still be waiting and must not appear healthy again.
    protocol_error: Mutex<Option<String>>,
    /// Bells, counted rather than flagged so a host can rate-limit them.
    bell: AtomicU64,
    /// The size the child is to believe, which is what a `TextAreaSizeRequest`
    /// is answered from while a resize is still on its way to the PTY.
    requested: Mutex<(u16, u16)>,
    /// The last size the PTY itself reported, which is the OS's answer and not
    /// this crate's bookkeeping.
    applied: Mutex<(u16, u16)>,
    /// Output chunks parsed. The unit the wake coalescing is measured in.
    chunks: AtomicU64,
    /// What Pi has said about the task in front of it.
    ///
    /// Written by the reader thread and read by the view, so it is behind a
    /// lock like the rest of the state two threads share.
    pi: Mutex<PiTracker>,
    /// How many program-status replies this session has handed to its child.
    pi_replies: AtomicU64,
}

impl Shared {
    fn new(cols: u16, rows: u16, scrollback_lines: usize) -> Self {
        Self {
            parser: Mutex::new(TerminalCore::new(cols, rows, scrollback_lines)),
            generation: AtomicU64::new(1),
            closing: AtomicBool::new(false),
            stream: Mutex::new(StreamState::Open),
            status: Mutex::new(PolledStatus::default()),
            clipboard_requested: AtomicBool::new(false),
            protocol_error: Mutex::new(None),
            bell: AtomicU64::new(0),
            requested: Mutex::new((cols, rows)),
            applied: Mutex::new((cols, rows)),
            chunks: AtomicU64::new(0),
            pi: Mutex::new(PiTracker::default()),
            pi_replies: AtomicU64::new(0),
        }
    }

    /// The parser, for the threads that look at it without parsing into it.
    ///
    /// One place that knows how the lock is poisoned, and one short hold: a
    /// feed on the reader, a resize on the worker, a snapshot on the view.
    fn core(&self) -> MutexGuard<'_, TerminalCore> {
        self.parser.lock().expect("terminal parser poisoned")
    }

    /// The size a `TextAreaSizeRequest` is answered from.
    fn requested(&self) -> (u16, u16) {
        *self.requested.lock().expect("terminal size poisoned")
    }

    fn set_requested(&self, cols: u16, rows: u16) {
        *self.requested.lock().expect("terminal size poisoned") = (cols, rows);
    }

    /// The size the PTY itself last reported.
    fn applied(&self) -> (u16, u16) {
        *self.applied.lock().expect("terminal size poisoned")
    }

    fn record_applied(&self, cols: u16, rows: u16) {
        *self.applied.lock().expect("terminal size poisoned") = (cols, rows);
    }

    /// Whether a program asked to read the clipboard. Never answered here.
    fn clipboard_requested(&self) -> bool {
        self.clipboard_requested.load(Ordering::Acquire)
    }

    fn bells(&self) -> u64 {
        self.bell.load(Ordering::Relaxed)
    }

    fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::Acquire) == generation
    }
}

/// (a) and (c), as last read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct PolledStatus {
    generation: u64,
    shell: ShellState,
    foreground: Foreground,
    /// Where the shell is, as the operating system recorded it at the last read.
    working_directory: Option<String>,
    /// `Default` gives `None`, which is not the same as "the read succeeded":
    /// it is the state before the first read, and the state a failed read
    /// leaves behind when it refuses to guess.
    poll_error: Option<String>,
}

/// The stop signal and the resize hand-off, shared by both workers.
#[derive(Default)]
struct Control {
    state: Mutex<ControlState>,
    changed: Condvar,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

#[derive(Default)]
struct ControlState {
    /// This session's own threads are being told to end. Not the same as a
    /// close having begun: a dropped session stops its workers and leaves the
    /// PTY to [`shutdown`].
    stopping: bool,
    /// The newest requested size, and nothing else. A pointer move across a
    /// window sends dozens of sizes a second and the PTY wants the last one.
    pending: Option<ResizeRequest>,
}

#[derive(Debug, Clone, Copy)]
struct ResizeRequest {
    generation: u64,
    cols: u16,
    rows: u16,
}

impl Control {
    fn request_resize(&self, request: ResizeRequest) {
        let mut state = self.state.lock().expect("terminal control poisoned");
        if state.stopping {
            return;
        }
        state.pending = Some(request);
        self.changed.notify_all();
    }

    fn has_pending_resize(&self) -> bool {
        self.state
            .lock()
            .expect("terminal control poisoned")
            .pending
            .is_some()
    }

    /// Take the newest requested size, if there is one.
    fn take_resize(&self) -> Option<ResizeRequest> {
        self.state
            .lock()
            .expect("terminal control poisoned")
            .pending
            .take()
    }

    /// Block until there is a resize to apply, or until this session is told to
    /// end. `false` means stop.
    fn wait_for_resize(&self) -> bool {
        let mut state = self.state.lock().expect("terminal control poisoned");
        loop {
            if state.stopping {
                return false;
            }
            if state.pending.is_some() {
                return true;
            }
            state = self.changed.wait(state).expect("terminal control poisoned");
        }
    }

    /// Wait up to `timeout`, returning `true` if the caller should carry on.
    fn keep_going(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().expect("terminal control poisoned");
        loop {
            if state.stopping {
                return false;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return true;
            }
            let (guard, _) = self
                .changed
                .wait_timeout(state, remaining)
                .expect("terminal control poisoned");
            state = guard;
        }
    }

    fn spawn_worker(
        &self,
        name: &str,
        worker: impl FnOnce() + Send + 'static,
    ) -> std::io::Result<()> {
        let handle = thread::Builder::new().name(name.to_owned()).spawn(worker)?;
        self.workers
            .lock()
            .expect("terminal control poisoned")
            .push(handle);
        Ok(())
    }

    /// Tell every worker to end and wait for it.
    ///
    /// Bounded by each worker's own wait, which is a condvar wait this call is
    /// the one waking. Idempotent, so a close that runs after a dropped session
    /// finds nothing to join.
    fn stop(&self) {
        {
            let mut state = self.state.lock().expect("terminal control poisoned");
            state.stopping = true;
            state.pending = None;
            self.changed.notify_all();
        }
        let workers = std::mem::take(&mut *self.workers.lock().expect("terminal control poisoned"));
        for worker in workers {
            if worker.join().is_err() {
                log::warn!("a terminal worker thread panicked");
            }
        }
    }
}

/// What the reader thread reaches for. Held apart from [`Inner`] so a reader
/// that outlives the handle does not keep the handle's threads alive.
#[derive(Clone)]
struct ReaderSide {
    shared: Arc<Shared>,
    queue: Arc<WriteQueue>,
    host: Arc<TerminalHost>,
    wake: Arc<Wake>,
}

impl ReaderSide {
    fn deliver_events(&self, events: Vec<TerminalEvent>) -> Result<(), String> {
        let result = handle_events(self, events);
        if let Err(error) = &result {
            *self
                .shared
                .protocol_error
                .lock()
                .expect("terminal protocol error poisoned") = Some(error.clone());
        }
        self.wake.signal();
        result
    }

    fn flush_sync(&self, finished: bool) {
        let events = {
            let mut core = self.shared.core();
            if finished {
                core.flush_sync();
            } else if !core.flush_sync_timeout(Instant::now()) {
                return;
            }
            core.take_events()
        };
        let _ = self.deliver_events(events);
    }
}

// ---------------------------------------------------------------------------------------------
// The session
// ---------------------------------------------------------------------------------------------

struct Inner {
    backend: Arc<TerminalBackend>,
    _integration_dir: Option<tempfile::TempDir>,
    id: String,
    side: ReaderSide,
    control: Arc<Control>,
    status_interval: Duration,
    drain_deadline: Duration,
    /// The view's end of the wake channel. A receiver cannot be cloned or
    /// shared, so it is stored here and handed over once, by `take_wake`.
    wake_receiver: Mutex<Option<TerminalWake>>,
}

impl Inner {
    /// The state the reader writes and everyone reads.
    ///
    /// A method rather than a field so that every `session.shared().whatever`
    /// reads as one hop instead of a path four hops long.
    fn shared(&self) -> &Shared {
        &self.side.shared
    }

    /// Hand bytes to the session's one writer.
    fn push_input(&self, bytes: Vec<u8>) -> Result<(), SessionError> {
        self.side.queue.push(bytes).map_err(SessionError::Queue)
    }

    /// The reader's half of the pipeline, as the backend wants it: bytes in,
    /// parsed on this thread, and one wake for the whole burst.
    fn output_sink(side: ReaderSide) -> OutputSink {
        Box::new(move |bytes: &[u8]| {
            side.shared.chunks.fetch_add(1, Ordering::Relaxed);

            // Capture events in order under the parser lock, then release it
            // before invoking the host or waiting for writer budget. Both the
            // view and resize worker must be able to read while replies wait.
            let events = {
                let mut core = side.shared.core();
                core.feed(bytes);
                core.take_events()
            };
            side.deliver_events(events)
        })
    }

    /// The reader's last word, which the backend calls exactly once from the
    /// reader thread however that thread ends.
    ///
    /// Recorded as its own state rather than as an exit status, because a child
    /// can exit while its last output is still being drained and a reader can
    /// stop while the child is perfectly alive.
    fn reader_end(side: ReaderSide) -> ReaderEndSink {
        Box::new(move |end: ReaderEnd| {
            side.flush_sync(true);
            let first = {
                let mut stream = side.shared.stream.lock().expect("terminal stream poisoned");
                let first = *stream == StreamState::Open;
                *stream = StreamState::Ended(end);
                first
            };
            if first {
                side.wake.signal();
            }
        })
    }

    /// Read the child's status once, and record it.
    ///
    /// Never on a view thread: this asks the OS about a process group and waits
    /// on a pipe, and a view that blocks here is a window that stops painting.
    fn poll_once(&self) -> Result<SessionStatus, String> {
        let issued = self.side.shared.generation.load(Ordering::Acquire);
        let status = self.backend.status(&self.id)?;

        // Read under a generation and recorded under another would be a reading
        // about a window that has been replaced, which is the one thing a
        // status is not allowed to be.
        if !self.side.shared.is_current(issued) {
            return Err(format!(
                "terminal {}: the status read was overtaken by a newer generation",
                self.id
            ));
        }

        let polled = PolledStatus {
            generation: issued,
            shell: match status.state {
                TerminalProcessState::Running => ShellState::Running,
                TerminalProcessState::Exited => ShellState::Exited {
                    code: status.exit_code,
                },
            },
            foreground: Foreground {
                running: status.foreground_process,
                app: status.foreground_app,
            },
            working_directory: status.working_directory,
            poll_error: None,
        };

        // The OS has said what is in front of the shell, so the doubt a
        // command start raised is answered, and a report that named a process
        // which is no longer there is forgotten.
        let pi_is_foreground = foreground_is_pi(&polled.foreground);
        let changed = {
            let mut stored = self
                .side
                .shared
                .status
                .lock()
                .expect("terminal status poisoned");
            let changed = *stored != polled;
            *stored = polled;
            changed
        };
        self.side
            .shared
            .pi
            .lock()
            .expect("terminal pi state poisoned")
            .confirmed(pi_is_foreground);
        if changed {
            // Only when the answer actually changed. A poll that wakes a view
            // every 250 ms is a view that repaints for nothing four times a
            // second, which is the same cost this file exists to avoid.
            self.side.wake.signal();
        }
        Ok(self.status())
    }

    /// One poll with its failure recorded, which is what the worker runs.
    ///
    /// Recording lives here rather than in the loop so that "the last read
    /// failed" is a property of the read and not of whoever happened to make
    /// it.
    fn poll_and_record(&self) -> Result<SessionStatus, String> {
        let result = self.poll_once();
        if let Err(error) = &result {
            self.record_failure(error.clone());
        }
        result
    }

    fn status(&self) -> SessionStatus {
        let polled = self
            .side
            .shared
            .status
            .lock()
            .expect("terminal status poisoned")
            .clone();
        let stream = self
            .side
            .shared
            .stream
            .lock()
            .expect("terminal stream poisoned")
            .clone();
        let pi_task = self
            .side
            .shared
            .pi
            .lock()
            .expect("terminal pi state poisoned")
            .task(foreground_is_pi(&polled.foreground));
        SessionStatus {
            generation: polled.generation,
            shell: polled.shell,
            foreground: polled.foreground,
            stream,
            poll_error: side_protocol_error(&self.side).or(polled.poll_error),
            pi_task,
            working_directory: polled.working_directory,
        }
    }

    /// Record a failed read without disturbing the last good one.
    fn record_failure(&self, error: String) {
        let mut stored = self
            .side
            .shared
            .status
            .lock()
            .expect("terminal status poisoned");
        // The child is not "idle" and the foreground is not "empty" because a
        // read failed; the last reading stands and the failure is kept beside it.
        let changed = stored.poll_error.as_ref() != Some(&error);
        stored.poll_error = Some(error);
        drop(stored);
        if changed {
            self.side.wake.signal();
        }
    }

    /// Steps 1 to 4 of a close. Nothing has been killed when this returns.
    fn begin_close(&self) -> Result<CloseReport, SessionError> {
        let already_closing = self.side.shared.closing.swap(true, Ordering::AcqRel);

        // Fresh, never the last poll. The poll is a picture of the child as of
        // up to one interval ago and the entire question here is what is running
        // right now.
        let status = match self.poll_once() {
            Ok(status) => status,
            Err(error) => {
                // A read that failed is not a read that said nothing is running.
                // The session reopens, but only if this call is what closed it:
                // a close that was already under way stays under way, because
                // the earlier one may already have stopped writing.
                if !already_closing {
                    self.side.shared.closing.store(false, Ordering::Release);
                }
                return Err(SessionError::StatusUnavailable(error));
            }
        };

        let drained = self.drain();
        self.control.stop();

        Ok(CloseReport {
            id: self.id.clone(),
            shell: status.shell,
            foreground: status.foreground,
            stream: status.stream,
            drained,
            closed: false,
        })
    }

    /// Wait, within the deadline, for input and resizes already in flight.
    ///
    /// On the deadline the pending generation is invalidated instead of waited
    /// on: a resize for a window that is closing is not worth an unbounded wait
    /// on a child that has stopped reading.
    fn drain(&self) -> bool {
        let deadline = Instant::now() + self.drain_deadline;
        while self.in_flight() && Instant::now() < deadline {
            thread::sleep(DRAIN_POLL);
        }
        if !self.in_flight() {
            return true;
        }
        self.side.shared.next_generation();
        log::warn!("terminal {} closed with input still in flight", self.id);
        false
    }

    /// Work that was accepted and has not reached the PTY yet.
    fn in_flight(&self) -> bool {
        self.side.queue.queued_requests() > 0 || self.control.has_pending_resize()
    }

    /// The whole close, on the caller's thread. [`TerminalSession::close`] runs
    /// this somewhere else.
    fn close_sequence(&self) -> Result<CloseReport, SessionError> {
        let mut report = self.begin_close()?;

        // The wake consumer is released before the PTY goes, so a view task is
        // never left waiting on a channel whose producer is about to be killed.
        self.side.wake.release();

        // Then the writer: after this nothing can reach the PTY at all, which is
        // what lets `backend.close` assume it is the last writer.
        self.side.queue.stop();

        report.closed = self
            .backend
            .close(&self.id)
            .map_err(SessionError::Backend)?;
        Ok(report)
    }
}

/// Apply one resize, or drop it for one of the three reasons below.
///
/// A function and not a loop body so the rule is testable on its own: the
/// generation check is the entire difference between a resize for the window on
/// screen and a resize for the one that was.
fn apply_resize(inner: &Inner, request: ResizeRequest) {
    // A request whose window has been replaced is history, not a smaller size.
    if !inner.side.shared.is_current(request.generation) {
        return;
    }
    // Nothing is resized after a close has begun: the PTY is being torn down
    // and a resize arriving now would be a use-after-close by another name.
    if inner.side.shared.closing.load(Ordering::Acquire) {
        return;
    }
    // `resize` refuses a zero measurement already. Refused again here because
    // this is the last place before the PTY hears it, and a PTY told to be zero
    // columns wide is a child writing into a hole.
    if request.cols == 0 || request.rows == 0 {
        return;
    }

    // The grid first, then the PTY. A child that redraws the instant it is
    // resized is then drawing for a grid that is already the right shape, and
    // the two agree from the first byte of that redraw.
    {
        let metrics = (inner.side.host.cell_metrics)();
        let mut core = inner.shared().core();
        core.set_graphics_cell_metrics(metrics.width, metrics.height);
        core.resize(request.cols, request.rows);
    }
    match inner.backend.resize(&inner.id, request.cols, request.rows) {
        // The backend answers with the size the PTY itself reports, so this is
        // the OS's answer rather than this crate's arithmetic.
        Ok((cols, rows)) => inner.shared().record_applied(cols, rows),
        Err(error) => log::warn!("terminal {} could not be resized: {error}", inner.id),
    }
}

fn side_protocol_error(side: &ReaderSide) -> Option<String> {
    side.shared
        .protocol_error
        .lock()
        .expect("terminal protocol error poisoned")
        .clone()
}

/// Dispatch drained events in order, with no parser guard held. Complete before
/// the wake so callbacks and delivery failures are visible to the next frame.
fn handle_events(side: &ReaderSide, events: Vec<TerminalEvent>) -> Result<(), String> {
    // Closing suppresses child-driven host effects but still allows tail parsing.
    if side.shared.closing.load(Ordering::Acquire) {
        return Ok(());
    }
    if let Some(error) = side_protocol_error(side) {
        return Err(error);
    }
    for event in events {
        if side.shared.closing.load(Ordering::Acquire) {
            return Ok(());
        }
        match event {
            // Never dropped, and never given a private path around the queue:
            // a reply that is late is survivable, one that is dropped is a
            // child that waits for a second it will never be sent.
            TerminalEvent::PtyWrite(reply) => push_reply(side, reply.into_bytes())?,
            // The core is the authority on the title; it already stored the
            // sanitised one while translating, and `snapshot` reads it there.
            TerminalEvent::TitleChanged(_) => {}
            TerminalEvent::ClipboardStore(text) => (side.host.store_clipboard)(&text),
            // Recorded, never answered. Silence is not a refusal the host gets
            // to make, so this is state rather than a dropped event.
            TerminalEvent::ClipboardRequested => {
                side.shared
                    .clipboard_requested
                    .store(true, Ordering::Release);
            }
            TerminalEvent::ColorRequested { index, respond } => {
                // The parser counts palette entries in `usize` and a palette has
                // 256. An index past the end is a program asking for an entry
                // that does not exist, and it is still blocked on an answer, so
                // it gets one rather than silence.
                let (r, g, b) = (side.host.resolve_color)(index);
                push_reply(side, respond(r, g, b).into_bytes())?;
            }
            // Answered from the session's own size, because a child asking how
            // big its text area is is asking how big it will be.
            TerminalEvent::TextAreaSizeRequested { respond } => {
                let (cols, rows) = side.shared.requested();
                let metrics = (side.host.cell_metrics)();
                push_reply(
                    side,
                    respond(cols, rows, metrics.width, metrics.height).into_bytes(),
                )?;
            }
            // The handshake, answered the only way it can be: the same
            // question back. A program that asked is told the host listens,
            // and from then on it reports.
            TerminalEvent::PiStatusQuery => {
                if side.shared.pi_replies.fetch_add(1, Ordering::Relaxed) < MAX_PI_STATUS_REPLIES {
                    push_reply(side, pi_status::PROGRAM_STATUS_REPLY.as_bytes().to_vec())?;
                }
            }
            TerminalEvent::PiTaskState(state) => {
                side.shared
                    .pi
                    .lock()
                    .expect("terminal pi state poisoned")
                    .report(state);
            }
            TerminalEvent::PiProgress(working) => {
                side.shared
                    .pi
                    .lock()
                    .expect("terminal pi state poisoned")
                    .progress(working);
            }
            // The shell's own boundaries: what a program said before them was
            // said about the command that was running then.
            TerminalEvent::CommandStarted => {
                side.shared
                    .pi
                    .lock()
                    .expect("terminal pi state poisoned")
                    .command_started();
            }
            TerminalEvent::CommandFinished => {
                side.shared
                    .pi
                    .lock()
                    .expect("terminal pi state poisoned")
                    .command_finished();
            }
            TerminalEvent::Bell => {
                side.shared.bell.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    Ok(())
}

/// Hand a protocol reply to the write queue, waiting its turn for room.
///
/// The reply shares the byte budget with every keystroke and paste, and the only
/// two ways out of a full budget are to wait or to exceed it. Waiting is what a
/// blocked child needs; a deadline on that waiting is what stops a closing
/// session from waiting for a child that will never read.
fn push_reply(side: &ReaderSide, reply: Vec<u8>) -> Result<(), String> {
    let deadline = Instant::now() + REPLY_ROOM_DEADLINE;
    loop {
        if side.shared.closing.load(Ordering::Acquire) {
            return Ok(());
        }
        match side.queue.push(reply.clone()) {
            Ok(()) => return Ok(()),
            Err(EnqueueError::Stopped) => {
                return Err("terminal protocol reply failed: the writer stopped".to_owned());
            }
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "terminal protocol reply queue timed out: {error:?}"
                    ));
                }
                thread::sleep(REPLY_ROOM_RETRY);
            }
        }
    }
}

/// The host's handle on one shell.
///
/// Cheap to call from a view thread: everything here either takes a short lock
/// or pushes onto a queue, and nothing here blocks on a child, a pipe or the OS.
pub struct TerminalSession {
    inner: Arc<Inner>,
    lifetime: Arc<WorkerLifetime>,
}

/// Workers own `Inner`, but never this external-handle lifetime.
struct WorkerLifetime(Arc<Inner>);

impl Drop for WorkerLifetime {
    fn drop(&mut self) {
        self.0.control.stop();
    }
}

/// The selection as the viewport sees it, in grid coordinates.
///
/// One definition for the two callers that need it: the frame that paints the
/// selection and the drag that updates it. They have to agree on every part of
/// it -- the display offset that moves viewport row zero, a start above the
/// viewport clamped onto row zero at column zero, and a selection that ended
/// above the viewport being nothing on screen -- so the conversion lives here
/// rather than being written out twice.
fn viewport_selection(core: &TerminalCore) -> Option<GridRange> {
    let range = core.selection_range()?;
    let offset = core.display_offset() as i32;
    let start_row = range.start.line.0 + offset;
    let end_row = range.end.line.0 + offset;
    (end_row >= 0).then_some((
        (
            start_row.max(0) as usize,
            if start_row < 0 {
                0
            } else {
                range.start.column.0
            },
        ),
        (end_row.max(0) as usize, range.end.column.0),
    ))
}

/// Two handles on one shell, not two shells.
///
/// The window needs one to close the session through and another to hand to the
/// view that draws it, and it has to be able to take the first *before* it has
/// the second: a session that only the view owns is a shell nobody can end if
/// the view is never mounted.
impl Clone for TerminalSession {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            lifetime: Arc::clone(&self.lifetime),
        }
    }
}

impl TerminalSession {
    /// Start `config`'s shell, and every thread that keeps it alive.
    ///
    /// The failure is returned rather than logged: a terminal that did not open
    /// is something the window has to say out loud, and a log line in a worker
    /// thread is something nobody will ever read.
    pub fn spawn(
        backend: Arc<TerminalBackend>,
        config: TerminalSessionConfig,
    ) -> Result<Self, SessionError> {
        if config.cols == 0 || config.rows == 0 {
            return Err(SessionError::InvalidSize {
                cols: config.cols,
                rows: config.rows,
            });
        }

        let program = config.shell.clone().unwrap_or_else(login_shell);
        let args = config
            .args
            .clone()
            .unwrap_or_else(|| vec![LOGIN_FLAG.to_owned()]);
        let (spawn, integration_dir) = shell_integration(&program, &config)
            .map(|(spawn, dir)| (spawn, Some(dir)))
            .unwrap_or(((program, args, config.env.clone()), None));
        let (program, args, mut env) = spawn;
        // Pi currently detects graphics from terminal names, not a probe.
        // Name our actual capability without impersonating Kitty/Ghostty, and
        // preserve explicit user choices (including disabling images).
        if std::env::var_os("PI_IMAGE_PROTOCOL").is_none()
            && !env.iter().any(|(key, _)| key == "PI_IMAGE_PROTOCOL")
        {
            env.push(("PI_IMAGE_PROTOCOL".into(), "kitty".into()));
        }

        let shared = Arc::new(Shared::new(
            config.cols,
            config.rows,
            config.scrollback_lines,
        ));
        let host = Arc::new(config.host);
        let id = config.id.clone();
        let (wake, wake_receiver) = Wake::new();
        let wake = Arc::new(wake);

        let side = ReaderSide {
            shared: Arc::clone(&shared),
            // One writer for the whole session, whatever produced the bytes.
            // The sink owns a copy of the id because it outlives this call.
            queue: Arc::new(
                WriteQueue::spawn({
                    let backend = Arc::clone(&backend);
                    let id = id.clone();
                    let shared = Arc::clone(&shared);
                    let wake = Arc::clone(&wake);
                    move |bytes| {
                        let result = backend.write(&id, bytes);
                        if let Err(error) = &result {
                            if !shared.closing.load(Ordering::Acquire) {
                                shared
                                    .protocol_error
                                    .lock()
                                    .expect("terminal protocol error poisoned")
                                    .get_or_insert_with(|| {
                                        format!("terminal PTY write failed: {error}")
                                    });
                                wake.signal();
                            }
                        }
                        result
                    }
                })
                .map_err(|error| SessionError::Spawn(error.to_string()))?,
            ),
            host,
            wake,
        };
        let control = Arc::new(Control::default());

        let inner = Arc::new(Inner {
            backend: Arc::clone(&backend),
            _integration_dir: integration_dir,
            id,
            side,
            control: Arc::clone(&control),
            status_interval: config.status_interval,
            drain_deadline: config.drain_deadline,
            wake_receiver: Mutex::new(Some(wake_receiver)),
        });

        backend
            .spawn(
                config.id,
                SpawnOptions {
                    program,
                    args,
                    cwd: config.cwd,
                    cols: config.cols,
                    rows: config.rows,
                    env,
                    reader_end: Some(Inner::reader_end(inner.side.clone())),
                },
                Inner::output_sink(inner.side.clone()),
            )
            .map_err(SessionError::Spawn)?;

        // Both workers start only once the child exists: a resize or a status
        // read for a session that never spawned has nothing to talk to.
        let resizer = Arc::clone(&inner);
        let workers = (|| {
            control.spawn_worker("terminal-resize", move || {
                while resizer.control.wait_for_resize() {
                    match resizer.control.take_resize() {
                        Some(request) => apply_resize(&resizer, request),
                        // Nothing left to take: another wake for a request already
                        // applied, and applying nothing is the whole answer.
                        None => continue,
                    }
                }
            })?;

            let poller = Arc::clone(&inner);
            control.spawn_worker("terminal-status", move || loop {
                poller.side.flush_sync(false);
                let _ = poller.poll_and_record();
                if !poller.control.keep_going(poller.status_interval) {
                    return;
                }
            })
        })();
        if let Err(error) = workers {
            control.stop();
            inner.side.wake.release();
            inner.side.queue.stop();
            if let Err(close_error) = backend.close(&inner.id) {
                log::error!("terminal startup cleanup failed: {close_error}");
            }
            return Err(SessionError::Spawn(error.to_string()));
        }

        Ok(Self {
            lifetime: Arc::new(WorkerLifetime(Arc::clone(&inner))),
            inner,
        })
    }

    pub fn id(&self) -> &str {
        &self.inner.id
    }

    /// The generation everything issued now is accounted to.
    pub fn generation(&self) -> u64 {
        self.inner.shared().generation.load(Ordering::Acquire)
    }

    /// Take a new generation, invalidating everything already in flight.
    ///
    /// The window's call for work it is about to throw away: a resize measured
    /// for a window that has been replaced must not land on the PTY of the
    /// window that took its place.
    pub fn next_generation(&self) -> u64 {
        self.inner.shared().next_generation()
    }

    /// Whether a close has begun.
    pub fn is_closing(&self) -> bool {
        self.inner.shared().closing.load(Ordering::Acquire)
    }

    /// The host's end of the wake channel, once.
    pub fn take_wake(&self) -> Option<TerminalWake> {
        self.inner
            .wake_receiver
            .lock()
            .expect("terminal wake poisoned")
            .take()
    }

    /// What the view draws. One lock, one frame.
    pub fn snapshot(&self) -> TerminalSnapshot {
        self.snapshot_inner(true)
    }

    pub(crate) fn painting_snapshot(&self) -> TerminalSnapshot {
        self.snapshot_inner(false)
    }

    /// The mouse-reporting inputs used by pointer events, without building a grid snapshot.
    pub(crate) fn mouse_state(&self) -> (bool, bool, bool) {
        let core = self.inner.shared().core();
        (
            core.is_alt_screen(),
            core.is_mouse_tracking(),
            core.modes().alternate_scroll,
        )
    }

    /// The cursor geometry requested by the native input system, without copying visible rows.
    pub(crate) fn cursor_snapshot(&self) -> CursorSnapshot {
        self.inner.shared().core().cursor()
    }

    /// Read the current title without copying the grid.
    pub fn title(&self) -> Option<String> {
        self.inner.shared().core().title().map(str::to_owned)
    }

    /// Extract selected text only when a caller needs to copy it.
    pub fn selection_text(&self) -> Option<String> {
        self.inner.shared().core().selection_text()
    }

    /// The selection the viewport sees, without copying the grid.
    ///
    /// The drag handler asks this on every mouse-move frame and paints nothing
    /// from it, so it reads the selection and the scroll offset and stops: no
    /// rows, no selected text.
    pub fn selection_range(&self) -> Option<GridRange> {
        viewport_selection(&self.inner.shared().core())
    }

    fn snapshot_inner(&self, selection: bool) -> TerminalSnapshot {
        let core = self.inner.shared().core();
        TerminalSnapshot {
            rows: core.visible_rows(),
            graphics: core.graphic_snapshots(),
            selection: selection.then(|| core.selection_text()).flatten(),
            selection_range: viewport_selection(&core),
            cursor: core.cursor(),
            modes: core.modes(),
            title: core.title().map(str::to_owned),
            alt_screen: core.is_alt_screen(),
            mouse_tracking: core.is_mouse_tracking(),
            display_offset: core.display_offset(),
            history_size: core.history_size(),
            background: core.background(),
            prompt_anchor: core.prompt_anchor(),
        }
    }

    /// Update terminal text selection from viewport grid coordinates.
    pub fn select(&self, start: (usize, usize), end: (usize, usize)) {
        self.inner.shared().core().select(start, end);
    }

    pub fn start_selection(&self, point: (usize, usize), kind: super::SelectionKind) {
        self.inner.shared().core().start_selection(point, kind);
    }

    pub fn update_selection(&self, point: (usize, usize)) {
        self.inner.shared().core().update_selection(point);
    }

    pub fn scroll_by(&self, lines: i32) {
        let mut core = self.inner.shared().core();
        if !core.is_alt_screen() {
            core.scroll_by(lines);
        }
    }

    pub fn set_display_offset(&self, offset: usize) {
        let mut core = self.inner.shared().core();
        if !core.is_alt_screen() {
            core.set_display_offset(offset);
        }
    }

    /// Queue the entire movement from a single grid/mode read, or nothing.
    pub fn move_caret_to(&self, row: usize, col: usize) -> Result<(), SessionError> {
        self.accept_new_work()?;
        let bytes = {
            let core = self.inner.shared().core();
            let Some((count, right)) = core.prompt_caret_move(row, col) else {
                return Ok(());
            };
            let arrow = if right { b'C' } else { b'D' };
            let sequence = [
                0x1b,
                if core.modes().app_cursor { b'O' } else { b'[' },
                arrow,
            ];
            let mut bytes = Vec::with_capacity(count * sequence.len());
            for _ in 0..count {
                bytes.extend_from_slice(&sequence);
            }
            bytes
        };
        self.inner.push_input(bytes)
    }

    pub fn send_mouse(&self, report: super::MouseReport) -> Result<(), SessionError> {
        self.accept_new_work()?;
        let bytes = {
            let core = self.inner.shared().core();
            if report.row >= usize::from(core.rows()) || report.col >= usize::from(core.cols()) {
                return Ok(());
            }
            super::input::encode_mouse(report, &core.modes())
        };
        if let Some(bytes) = bytes {
            self.inner.push_input(bytes)?;
        }
        Ok(())
    }

    pub fn send_focus(&self, focused: bool) -> Result<(), SessionError> {
        self.accept_new_work()?;
        if self.modes().focus_reporting {
            self.inner
                .push_input(if focused { b"\x1b[I" } else { b"\x1b[O" }.to_vec())?;
        }
        Ok(())
    }

    pub fn send_text(&self, text: &str) -> Result<(), SessionError> {
        self.accept_new_work()?;
        self.inner.push_input(text.as_bytes().to_vec())?;
        self.inner.shared().core().scroll_to_bottom();
        Ok(())
    }

    pub fn clear_selection(&self) {
        self.inner.shared().core().clear_selection();
    }

    pub fn move_prompt_caret(&self, row: usize, column: usize) -> Option<usize> {
        self.inner.shared().core().move_prompt_caret(row, column)
    }

    pub fn invalidate_prompt_anchor(&self) {
        self.inner.shared().core().invalidate_prompt_anchor();
    }

    /// The exit status the last finished command reported, as OSC 133 `D`.
    ///
    /// Display data, and only that: it is what a child printed, so it is a claim
    /// about a terminal's contents and never an input to a decision about the
    /// agent, a permission, or anything else that acts on a person's behalf.
    ///
    /// `None` is one honest answer for "nothing has finished", "a new command
    /// started" and "this shell reports nothing at all" -- three states the
    /// parser cannot tell apart, so a sidebar must not pick one of them.
    pub fn last_command_exit(&self) -> Option<i32> {
        self.inner.shared().core().command_exit()
    }

    /// Everything known about the child. The last poll's answers, never a fresh
    /// read: this is a view thread's question and asking the OS blocks.
    pub fn status(&self) -> SessionStatus {
        self.inner.status()
    }

    /// The size the PTY itself last reported.
    pub fn applied_size(&self) -> (u16, u16) {
        self.inner.shared().applied()
    }

    /// Output chunks parsed since the shell started. The unit the wake
    /// coalescing is measured in.
    pub fn output_chunks(&self) -> u64 {
        self.inner.shared().chunks.load(Ordering::Relaxed)
    }

    /// Wakes that arrived while another wake was already pending.
    pub fn coalesced_wakes(&self) -> u64 {
        self.inner.side.wake.coalesced.load(Ordering::Relaxed)
    }

    /// True once a program asked to read the clipboard. Never answered here.
    pub fn clipboard_requested(&self) -> bool {
        self.inner.shared().clipboard_requested()
    }

    /// Bells so far.
    pub fn bell_count(&self) -> u64 {
        self.inner.shared().bells()
    }

    /// Measure the window, and tell the PTY.
    ///
    /// Coalesced to the newest: a pointer drag sends sizes faster than the PTY
    /// can be resized and only the last one is ever the right one. The size the
    /// child is told about is recorded before the resize is applied, because a
    /// child that asks how big its text area is during the wait deserves the
    /// size the window is on its way to.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), SessionError> {
        // A window that measures zero is invisible or minimised, and neither is
        // a size. Refused here so the worker's guard is a second line rather
        // than the only one.
        if cols == 0 || rows == 0 {
            return Err(SessionError::InvalidSize { cols, rows });
        }
        self.accept_new_work()?;

        // A resize is work issued under a fresh generation, so anything already
        // waiting for the PTY is by definition older than this request.
        let generation = self.inner.shared().next_generation();
        self.inner.shared().set_requested(cols, rows);
        self.inner.control.request_resize(ResizeRequest {
            generation,
            cols,
            rows,
        });
        Ok(())
    }

    /// Encode one key press against the modes the child negotiated, and queue it.
    ///
    /// `None` from the encoder is not an error: a press with no terminal meaning
    /// is a press this terminal does not send, and the caller drops it having
    /// been told.
    pub fn send_key(&self, press: &KeyPress) -> Result<(), SessionError> {
        self.accept_new_work()?;
        let modes = self.modes();
        let Some(bytes) = encode_key(press, &modes) else {
            return Ok(());
        };
        // Retire the editable prompt before submitting it, including on older
        // Bash versions that do not support the pre-execution PS0 marker.
        if press.key == super::keys::Key::Enter {
            self.invalidate_prompt_anchor();
        }
        self.inner.push_input(bytes)?;
        self.inner.shared().core().scroll_to_bottom();
        Ok(())
    }

    /// Encode a paste against the child's bracketed-paste mode, and queue it.
    pub fn paste(&self, text: &str) -> Result<(), SessionError> {
        self.paste_inner(text, false)
    }

    /// Paste after explicit user approval of possible command execution.
    /// Approval never bypasses size limits or control-character sanitization.
    pub fn paste_confirmed(&self, text: &str) -> Result<(), SessionError> {
        self.paste_inner(text, true)
    }

    fn paste_inner(&self, text: &str, confirmed: bool) -> Result<(), SessionError> {
        self.accept_new_work()?;
        let modes = self.modes();
        let bytes = if confirmed {
            encode_paste_confirmed(text, &modes)
        } else {
            encode_paste(text, &modes)
        }
        .map_err(SessionError::Paste)?;
        if bytes.is_empty() {
            return Ok(());
        }
        self.inner.push_input(bytes)?;
        self.inner.shared().core().scroll_to_bottom();
        Ok(())
    }

    fn modes(&self) -> TerminalModes {
        self.inner.shared().core().modes()
    }

    fn accept_new_work(&self) -> Result<(), SessionError> {
        if self.is_closing() {
            return Err(SessionError::Closing);
        }
        Ok(())
    }

    /// Steps 1 to 4: refuse new work, read the child fresh, report what is in
    /// front of the shell, drain what is in flight.
    ///
    /// Nothing is killed. This is the call that lets a host ask the person
    /// whether their editor is worth closing before it is closed.
    pub fn begin_close(&self) -> Result<CloseReport, SessionError> {
        self.inner.begin_close()
    }

    /// The rest of the close, on its own thread.
    ///
    /// Off the caller's thread because the backend's close is allowed a grace
    /// period and then a kill: a view that waited for that would freeze for as
    /// long as a stubborn child takes to die.
    pub fn close(&self) -> CloseHandle {
        let inner = Arc::clone(&self.inner);
        let worker = thread::Builder::new()
            .name("terminal-close".to_owned())
            .spawn(move || inner.close_sequence());
        let (worker, result) = match worker {
            Ok(worker) => (Some(worker), None),
            // An explicit close must still close if its helper cannot start.
            Err(_) => (None, Some(self.inner.close_sequence())),
        };
        CloseHandle { worker, result }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Threads first: the resize worker and the poller hold the backend and
        // must not outlive the session that owns it. Then the writer, then the
        // wake.
        self.control.stop();
        self.side.wake.release();
        self.side.queue.stop();
        // The PTY itself is not closed here, and deliberately: `backend.close`
        // is a decision a host makes with the person in front of it, and
        // `shutdown` is the one that ends everything at once. The private
        // integration files remain until this point, after the session workers
        // have stopped.
    }
}

/// The close, running somewhere else. Wait for it, or let it finish on its own.
pub struct CloseHandle {
    worker: Option<JoinHandle<Result<CloseReport, SessionError>>>,
    result: Option<Result<CloseReport, SessionError>>,
}

impl CloseHandle {
    /// Wait for the close to finish.
    pub fn wait(mut self) -> Result<CloseReport, SessionError> {
        if let Some(result) = self.result.take() {
            return result;
        }
        let Some(worker) = self.worker.take() else {
            return Err(SessionError::Backend(
                "the terminal close thread was already waited on".into(),
            ));
        };
        worker.join().unwrap_or_else(|_| {
            Err(SessionError::Backend(
                "the terminal close thread panicked".into(),
            ))
        })
    }
}

impl Drop for CloseHandle {
    fn drop(&mut self) {
        // Detached on purpose: the close runs to its end either way, and the
        // handle is dropped by hosts that do not care what it reported.
        let _ = self.worker.take();
    }
}

/// End every terminal this app started.
///
/// The one place this happens, and not something left to a `Drop`: the event
/// loop hands the process to `exit` rather than unwinding, so a managed value
/// that waits for its destructor leaks every shell it was holding.
pub fn shutdown(backend: &TerminalBackend) {
    backend.shutdown();
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::AtomicU32;

    use tempfile::TempDir;

    use super::super::keys::Key;
    use super::*;

    /// How long a test waits for a child to get on with it. Generous, because a
    /// test that fails on a busy CI machine is a test about scheduling.
    const PATIENCE: Duration = Duration::from_secs(10);

    /// How often a test looks.
    const POLL: Duration = Duration::from_millis(10);

    /// Backscroll small enough that a burst test is parsing a grid rather than
    /// an archive.
    const TEST_SCROLLBACK: usize = 500;

    static NEXT_ID: AtomicU32 = AtomicU32::new(0);

    fn next_id() -> String {
        format!("session-test-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// `/bin/sh` running `script`.
    fn config(dir: &Path, script: &str) -> TerminalSessionConfig {
        let mut config = TerminalSessionConfig::new(
            next_id(),
            dir.to_path_buf(),
            80,
            24,
            Some(PathBuf::from("/bin/sh")),
        );
        config.args = Some(vec!["-c".to_owned(), script.to_owned()]);
        assert!(shell_integration(&PathBuf::from("/bin/bash"), &config).is_none());
        config.scrollback_lines = TEST_SCROLLBACK;
        config.status_interval = Duration::from_millis(20);
        config.drain_deadline = Duration::from_millis(500);
        config
    }

    /// `/bin/sh` itself, reading commands from the PTY the way a person's shell
    /// does. What a test needs when the point is to ask the child something.
    fn interactive_config(dir: &Path) -> TerminalSessionConfig {
        let mut config = TerminalSessionConfig::new(
            next_id(),
            dir.to_path_buf(),
            80,
            24,
            Some(PathBuf::from("/bin/sh")),
        );
        config.args = Some(Vec::new());
        config.scrollback_lines = TEST_SCROLLBACK;
        config.status_interval = Duration::from_millis(20);
        config.drain_deadline = Duration::from_millis(500);
        config
    }

    #[cfg(unix)]
    #[test]
    fn real_zsh_and_bash_startup_markers_and_caret_movement_edit_command() {
        use std::os::unix::fs::PermissionsExt;
        for shell in ["/bin/zsh", "/bin/bash"] {
            if !Path::new(shell).is_file() {
                continue;
            }
            let home = TempDir::new().expect("isolated home");
            let scripts = TempDir::new().expect("private integration directory");
            let marker_file = home.path().join("startup-sourced");
            let startup = format!(
                "printf sourced > '{}'\nPS1='test> '\n",
                marker_file.display()
            );
            if shell.ends_with("zsh") {
                std::fs::write(home.path().join(".zshenv"), "export ZDOTDIR=$HOME\n").unwrap();
                std::fs::write(home.path().join(".zshrc"), &startup).unwrap();
            } else {
                std::fs::write(home.path().join(".bash_profile"), &startup).unwrap();
            }
            let mut config = TerminalSessionConfig::new(
                next_id(),
                home.path().to_path_buf(),
                80,
                24,
                Some(PathBuf::from(shell)),
            );
            config.env.extend([
                (OsString::from("HOME"), home.path().as_os_str().to_owned()),
                (
                    OsString::from("ZDOTDIR"),
                    home.path().as_os_str().to_owned(),
                ),
                (
                    OsString::from("XDG_RUNTIME_DIR"),
                    scripts.path().as_os_str().to_owned(),
                ),
            ]);
            config.status_interval = Duration::from_millis(20);
            let (backend, session, _) = start(config);
            let deadline = Instant::now() + PATIENCE;
            while Instant::now() < deadline
                && !(marker_file.exists() && session.snapshot().prompt_anchor.is_some())
            {
                thread::sleep(POLL);
            }
            assert!(
                marker_file.exists(),
                "startup not sourced for {shell}: {}",
                grid(&session)
            );
            assert!(
                session.snapshot().prompt_anchor.is_some(),
                "no OSC 133 B for {shell}: {}",
                grid(&session)
            );
            let initial = session.snapshot();
            assert_eq!(
                initial.cursor.col,
                "test> ".len(),
                "prompt width for {shell}"
            );
            assert_eq!(
                initial
                    .prompt_anchor
                    .expect("OSC 133 B anchors prompt")
                    .column,
                "test> ".len()
            );
            assert!(
                grid(&session).contains("test> "),
                "user PS1 preserved for {shell}: {}",
                grid(&session)
            );
            let script_dir = session
                .inner
                ._integration_dir
                .as_ref()
                .expect("default shell has private integration directory")
                .path();
            assert_eq!(
                std::fs::metadata(script_dir.join(if shell.ends_with("zsh") {
                    "hook.zsh"
                } else {
                    "hook.bash"
                }))
                .unwrap()
                .permissions()
                .mode()
                    & 0o077,
                0
            );
            let probe = home.path().join("executed-command");
            let command = "printf '%s' 'beforeafter' > executed-command";
            session.paste(command).unwrap();
            wait_for("command echo", || grid(&session).contains(command));
            let current = session.snapshot();
            let anchor = current.prompt_anchor.expect("editable prompt");
            let column = anchor.column + "printf '%s' 'before".len();
            let count = session
                .move_prompt_caret(current.cursor.row, column)
                .expect("click inside editable input");
            for _ in 0..count {
                session.send_key(&press(Key::Left)).unwrap();
            }
            session.paste("-inserted-").unwrap();
            session.send_key(&press(Key::Enter)).unwrap();
            wait_for("edited command execution", || probe.exists());
            assert_eq!(
                std::fs::read_to_string(&probe).unwrap(),
                "before-inserted-after"
            );
            wait_for("next prompt reacquires anchor", || {
                session.snapshot().prompt_anchor.is_some()
            });
            session.close().wait().unwrap();
            backend.shutdown();
        }
    }

    fn start(
        config: TerminalSessionConfig,
    ) -> (Arc<TerminalBackend>, TerminalSession, TerminalWake) {
        let backend = Arc::new(TerminalBackend::default());
        let session =
            TerminalSession::spawn(Arc::clone(&backend), config).expect("the session spawns");
        let wake = session
            .take_wake()
            .expect("a fresh session has a wake to take");
        (backend, session, wake)
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if done() {
                return;
            }
            thread::sleep(POLL);
        }
        panic!("gave up after {PATIENCE:?} waiting for {what}");
    }

    /// Everything on screen, as text. One line per row.
    fn grid(session: &TerminalSession) -> String {
        session
            .snapshot()
            .rows
            .iter()
            .map(|row| row.cells.iter().map(|cell| cell.c).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn occurrences(haystack: &str, needle: &str) -> usize {
        haystack.matches(needle).count()
    }

    fn press(key: Key) -> KeyPress {
        KeyPress {
            key,
            shift: false,
            ctrl: false,
            alt: false,
            meta: false,
            text: None,
        }
    }

    /// Ask the child what its terminal measures, and wait for the answer.
    ///
    /// The child is the only witness that cannot be wrong about its own PTY: it
    /// is asking the kernel, and this crate's own record of what it asked for is
    /// exactly the thing a bug would have got wrong.
    ///
    /// The marker is spelled `PROB%s` and printed as `PROBE` on purpose. The
    /// line discipline echoes what is typed, so a marker that appears in the
    /// command is a marker that has already arrived before the command ran.
    fn probe_size(session: &TerminalSession) -> String {
        let before = occurrences(&grid(session), "PROBE");
        session
            .paste_confirmed("stty size; printf 'PROB%s\\n' E\n")
            .expect("the probe is written");
        wait_for("the child to answer the probe", || {
            occurrences(&grid(session), "PROBE") > before
        });
        grid(session)
    }

    /// The panic hook this crate installed, restored on the way out.
    type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync>;

    /// Keep a panic out of the test output.
    ///
    /// Only the printing is silenced: libtest reports a failing test from its
    /// own bookkeeping, so nothing that would fail is hidden by this. Restored
    /// by `Drop` so a test that panics mid-window does not leave the suite
    /// mute.
    struct QuietPanics(Option<PanicHook>);

    impl QuietPanics {
        fn new() -> Self {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            Self(Some(previous))
        }
    }

    impl Drop for QuietPanics {
        fn drop(&mut self) {
            if let Some(previous) = self.0.take() {
                std::panic::set_hook(previous);
            }
        }
    }

    fn process_is_gone(pid: i32) -> bool {
        // `ESRCH` and not merely a non-zero answer: a permission error would
        // look like a live process, and this test is about a process that is not
        // there.
        if unsafe { libc::kill(pid, 0) } != -1 {
            return false;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    #[test]
    fn last_external_handle_releases_workers_without_closing_the_child() {
        let dir = tempfile::tempdir().unwrap();
        let (backend, session, wake) = start(config(dir.path(), "sleep 30"));
        let id = session.id().to_owned();
        let inner = Arc::downgrade(&session.inner);
        let control = Arc::clone(&session.inner.control);
        let queue = Arc::clone(&session.inner.side.queue);
        let clone = session.clone();
        drop(session);
        assert!(inner.upgrade().is_some());
        assert!(!control.state.lock().unwrap().stopping);
        drop(clone);
        assert!(inner.upgrade().is_none(), "workers retained the session");
        assert!(control.state.lock().unwrap().stopping);
        assert!(control.workers.lock().unwrap().is_empty());
        assert_eq!(queue.push(vec![b'x']), Err(EnqueueError::Stopped));
        while wake.take() {}
        assert!(matches!(
            wake.0.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        assert_eq!(
            backend.status(&id).unwrap().state,
            TerminalProcessState::Running
        );
        shutdown(&backend);
    }

    #[test]
    fn lightweight_metadata_and_selection_do_not_extract_text_for_painting() {
        let dir = tempfile::tempdir().unwrap();
        let (backend, session, _) = start(config(dir.path(), "sleep 30"));
        assert_eq!(session.title(), None);
        assert_eq!(session.selection_text(), None);
        session
            .inner
            .shared()
            .core()
            .feed(b"\x1b]2;editor\x07hello");
        session.select((0, 0), (0, 4));
        assert_eq!(session.title().as_deref(), Some("editor"));
        assert_eq!(session.selection_text().as_deref(), Some("hello"));
        let painted = session.painting_snapshot();
        assert!(painted.selection.is_none());
        assert!(painted.selection_range.is_some());
        assert_eq!(session.snapshot().selection.as_deref(), Some("hello"));
        session.clear_selection();
        assert_eq!(session.selection_text(), None);
        session.close().wait().unwrap();
        shutdown(&backend);
    }

    #[test]
    fn the_selection_range_query_agrees_with_the_frame_it_feeds() {
        let dir = tempfile::tempdir().unwrap();
        let (backend, session, _) = start(config(dir.path(), "sleep 30"));
        session
            .inner
            .shared()
            .core()
            .feed(b"alpha beta\r\ngamma delta\r\nepsilon");

        // No selection is no range, said the same way by both callers.
        assert_eq!(session.selection_range(), None);
        assert_eq!(session.snapshot().selection_range, None);

        // A simple selection on one row.
        session.select((0, 0), (0, 4));
        assert_eq!(session.selection_range(), Some(((0, 0), (0, 4))));
        assert_eq!(
            session.selection_range(),
            session.snapshot().selection_range
        );
        assert_eq!(
            session.selection_range(),
            session.painting_snapshot().selection_range
        );

        // The same drag across rows, and then backwards from the other corner.
        for (start, end) in [((0, 3), (2, 3)), ((2, 3), (0, 3))] {
            session.select(start, end);
            assert_eq!(
                session.selection_range(),
                session.snapshot().selection_range,
                "{start:?}..{end:?}"
            );
            assert_eq!(
                session.selection_range(),
                session.painting_snapshot().selection_range,
                "{start:?}..{end:?}"
            );
            assert_eq!(session.selection_range(), Some(((0, 3), (2, 3))));
        }
        assert_eq!(
            session.selection_text().as_deref(),
            Some("ha beta\ngamma delta\nepsi")
        );
        session.close().wait().unwrap();
        shutdown(&backend);
    }

    #[test]
    fn a_selection_whose_start_is_above_the_viewport_is_clamped_to_its_first_row() {
        let dir = tempfile::tempdir().unwrap();
        let (backend, session) = scrolled_session(dir.path());

        // Anchored with the viewport ten lines further down the history, so the
        // selection is where the person left it.
        session.set_display_offset(10);
        session.select((3, 4), (15, 8));
        assert_eq!(session.selection_range(), Some(((3, 4), (15, 8))));

        // Scrolling back to the bottom leaves the first row above the viewport:
        // the frame paints a selection from the first visible column, so that
        // is the start it is given.
        session.set_display_offset(0);
        assert_eq!(session.selection_range(), Some(((0, 0), (5, 8))));
        assert_eq!(
            session.selection_range(),
            session.snapshot().selection_range
        );
        session.close().wait().unwrap();
        shutdown(&backend);
    }

    #[test]
    fn a_selection_that_ended_above_the_viewport_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let (backend, session) = scrolled_session(dir.path());

        session.set_display_offset(20);
        session.select((0, 2), (2, 6));
        assert_eq!(session.selection_range(), Some(((0, 2), (2, 6))));

        session.set_display_offset(0);
        assert_eq!(session.selection_range(), None);
        assert_eq!(session.snapshot().selection_range, None);
        assert_eq!(session.painting_snapshot().selection_range, None);
        session.close().wait().unwrap();
        shutdown(&backend);
    }

    /// A silent shell with sixty numbered lines above a twenty-four row screen,
    /// so a test has history to scroll back through.
    fn scrolled_session(dir: &Path) -> (Arc<TerminalBackend>, TerminalSession) {
        let (backend, session, _) = start(config(dir, "sleep 30"));
        let mut core = session.inner.shared().core();
        for row in 0..60 {
            core.feed(format!("line {row:02}\r\n").as_bytes());
        }
        assert!(
            core.history_size() >= 20,
            "a fixture with no scrollback: {}",
            core.history_size()
        );
        drop(core);
        (backend, session)
    }

    #[test]
    fn async_wakes_coalesce_disconnect_and_release_the_consumer() {
        use std::future::Future;
        use std::task::{Context, Poll, Waker};

        #[derive(Default)]
        struct Count(AtomicU64);
        impl std::task::Wake for Count {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let count = Arc::new(Count::default());
        let waker = Waker::from(Arc::clone(&count));
        let mut cx = Context::from_waker(&waker);
        let (producer, receiver) = Wake::new();
        {
            let mut recv = std::pin::pin!(receiver.recv());
            assert_eq!(recv.as_mut().poll(&mut cx), Poll::Pending);
            for _ in 0..100 {
                producer.signal();
            }
            assert_eq!(count.0.load(Ordering::Relaxed), 1);
            assert_eq!(producer.coalesced.load(Ordering::Relaxed), 99);
            assert_eq!(recv.as_mut().poll(&mut cx), Poll::Ready(true));
        }
        {
            let mut recv = std::pin::pin!(receiver.recv());
            assert_eq!(recv.as_mut().poll(&mut cx), Poll::Pending);
            producer.release();
            assert_eq!(count.0.load(Ordering::Relaxed), 2);
            assert_eq!(recv.as_mut().poll(&mut cx), Poll::Ready(false));
        }
        let (producer, receiver) = Wake::new();
        {
            let mut recv = std::pin::pin!(receiver.recv());
            assert_eq!(recv.as_mut().poll(&mut cx), Poll::Pending);
        }
        drop(receiver);
        producer.signal();
        assert_eq!(count.0.load(Ordering::Relaxed), 2);
        assert!(producer.waker.lock().unwrap().is_none());
    }

    #[test]
    fn host_callbacks_can_read_the_parser_and_events_keep_their_order() {
        let shared = Arc::new(Shared::new(80, 24, TEST_SCROLLBACK));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let callback = |kind: &'static str| {
            let shared = Arc::clone(&shared);
            let calls = Arc::clone(&calls);
            move || {
                assert!(
                    shared.parser.try_lock().is_ok(),
                    "host held the parser lock"
                );
                calls.lock().unwrap().push(kind);
            }
        };
        let clipboard = callback("clipboard");
        let color = callback("color");
        let metrics = callback("metrics");
        let (wake, _) = Wake::new();
        let side = ReaderSide {
            shared,
            queue: Arc::new(WriteQueue::spawn(|_| Ok(())).unwrap()),
            host: Arc::new(TerminalHost {
                store_clipboard: Box::new(move |_| clipboard()),
                resolve_color: Box::new(move |_| {
                    color();
                    (1, 2, 3)
                }),
                cell_metrics: Box::new(move || {
                    metrics();
                    CellMetrics {
                        width: 8,
                        height: 16,
                    }
                }),
            }),
            wake: Arc::new(wake),
        };
        let mut sink = Inner::output_sink(side.clone());
        sink(b"\x1b]52;c;aGVsbG8=\x07\x1b]4;1;?\x07\x1b[14t").unwrap();
        assert_eq!(*calls.lock().unwrap(), ["clipboard", "color", "metrics"]);
        side.queue.stop();
    }

    #[test]
    fn a_reply_wait_does_not_lock_the_parser_and_failure_is_recorded() {
        use std::sync::mpsc;

        let shared = Arc::new(Shared::new(80, 24, TEST_SCROLLBACK));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut first = true;
        let queue = Arc::new(
            WriteQueue::spawn(move |_| {
                if first {
                    first = false;
                    started_tx.send(()).unwrap();
                    let _ = release_rx.recv();
                }
                Ok(())
            })
            .unwrap(),
        );
        queue.push(vec![b'x']).unwrap();
        started_rx.recv_timeout(PATIENCE).unwrap();
        for _ in 0..super::super::write_queue::MAX_QUEUED_REQUESTS {
            queue.push(Vec::new()).unwrap();
        }
        let (callback_tx, callback_rx) = mpsc::channel();
        let (wake, _) = Wake::new();
        let side = ReaderSide {
            shared: Arc::clone(&shared),
            queue: Arc::clone(&queue),
            host: Arc::new(TerminalHost {
                cell_metrics: Box::new(move || {
                    callback_tx.send(()).unwrap();
                    CellMetrics {
                        width: 8,
                        height: 16,
                    }
                }),
                ..TerminalHost::default()
            }),
            wake: Arc::new(wake),
        };
        let reader_side = side.clone();
        let reader = thread::spawn(move || {
            let mut sink = Inner::output_sink(reader_side);
            sink(b"\x1b[14t")
        });
        let callback_seen = callback_rx.recv_timeout(PATIENCE).is_ok();
        let parser_available = shared.parser.try_lock().is_ok();
        // Keep the queue full until the reply deadline, without holding the
        // parser. Release the writer before asserting so cleanup cannot hang.
        let result = reader.join().unwrap();
        release_tx.send(()).unwrap();
        queue.stop();
        assert!(callback_seen);
        assert!(parser_available, "reply wait held the parser lock");
        assert!(result.is_err());
        assert!(side_protocol_error(&side).is_some_and(|error| error.contains("queue timed out")));
    }

    #[test]
    fn a_queued_reply_write_failure_is_sticky_and_wakes_the_view() {
        let dir = TempDir::new().unwrap();
        let (backend, session, wake) = start(config(dir.path(), "printf ready; sleep 30"));
        wait_for("the child to print", || grid(&session).contains("ready"));
        // Remove the PTY without closing the session: enqueue still succeeds,
        // but the asynchronous backend write must fail.
        assert!(backend.close(&session.inner.id).unwrap());
        wake.take();
        push_reply(&session.inner.side, b"\x1b[0n".to_vec()).unwrap();
        wait_for("the writer to record its failure and wake the view", || {
            side_protocol_error(&session.inner.side).is_some() && wake.take()
        });
        let error = session.status().poll_error.unwrap();
        assert!(error.starts_with("terminal PTY write failed:"));
        // A later successful status poll must not erase a lost reply.
        session.inner.shared().status.lock().unwrap().poll_error = None;
        assert_eq!(session.status().poll_error.as_deref(), Some(error.as_str()));
        let _ = session.close().wait();
    }

    #[test]
    fn a_shell_prints_and_the_grid_shows_it() {
        let dir = TempDir::new().expect("a temporary directory");
        let (_backend, session, wake) =
            start(config(dir.path(), "printf 'muster-marker\\n'; sleep 30"));

        wait_for("the shell's output to reach the grid", || {
            grid(&session).contains("muster-marker")
        });

        // Proof the reader woke the view rather than the view polling: one
        // outstanding wake is all the burst above could have produced.
        assert!(
            wake.wait(Duration::from_secs(1)),
            "the reader signalled the view"
        );

        let snapshot = session.snapshot();
        assert_eq!(occurrences(&grid(&session), "muster-marker"), 1);
        assert_eq!(
            snapshot.cursor.row, 1,
            "the marker is the first line, so the cursor is the second"
        );
        assert!(!snapshot.alt_screen);
        assert!(!snapshot.mouse_tracking);

        session.close().wait().expect("the session closes");
    }

    #[test]
    fn a_resize_that_measures_zero_never_reaches_the_pty() {
        let dir = TempDir::new().expect("a temporary directory");
        let (_backend, session, _wake) = start(interactive_config(dir.path()));

        assert!(
            probe_size(&session).contains("24 80"),
            "the child starts 24 by 80"
        );

        // Every shape a window that is not on screen takes.
        for (cols, rows) in [(0, 0), (0, 40), (120, 0)] {
            assert!(
                matches!(
                    session.resize(cols, rows),
                    Err(SessionError::InvalidSize { .. })
                ),
                "{cols} by {rows} is refused rather than guessed at"
            );
        }

        let after = probe_size(&session);
        assert_eq!(
            occurrences(&after, "24 80"),
            2,
            "the child reported the same size twice"
        );
        assert!(
            !after.contains("0 80") && !after.contains("0 40"),
            "the PTY was never told zero"
        );
        assert_eq!(session.applied_size(), (80, 24));

        session.close().wait().expect("the session closes");
    }

    #[test]
    fn a_late_resize_from_an_old_generation_is_dropped() {
        let dir = TempDir::new().expect("a temporary directory");
        let (_backend, session, _wake) = start(interactive_config(dir.path()));
        assert!(probe_size(&session).contains("24 80"));

        // The worker really does apply what it is handed.
        session.resize(100, 30).expect("a real size is accepted");
        wait_for("the resize to reach the PTY", || {
            session.applied_size() == (100, 30)
        });
        assert!(probe_size(&session).contains("30 100"));

        // And a request whose window has been replaced does not.
        let stale = session.next_generation();
        let current = session.next_generation();
        assert_ne!(stale, current);
        apply_resize(
            &session.inner,
            ResizeRequest {
                generation: stale,
                cols: 140,
                rows: 50,
            },
        );
        assert_eq!(
            session.applied_size(),
            (100, 30),
            "the stale resize never reached the PTY"
        );
        assert!(!probe_size(&session).contains("50 140"));

        // The same request under the current generation is the same request, so
        // the difference above is the generation and nothing else.
        apply_resize(
            &session.inner,
            ResizeRequest {
                generation: current,
                cols: 140,
                rows: 50,
            },
        );
        assert_eq!(session.applied_size(), (140, 50));
        assert!(probe_size(&session).contains("50 140"));

        session.close().wait().expect("the session closes");
    }

    #[test]
    fn the_wake_signal_coalesces_under_a_flood() {
        let dir = TempDir::new().expect("a temporary directory");
        // A finite burst, about 1.4 MB, which is at least twenty 64 KiB reads.
        let script = "i=0; while [ $i -lt 20000 ]; do printf '%s ----\\n' \"$i\"; i=$((i+1)); done";
        let (_backend, session, wake) = start(config(dir.path(), script));

        // Nothing drains the wake while this runs, which is the whole point: at
        // most one wake can be outstanding, so a burst that would otherwise
        // queue a task per chunk has one token waiting by the end of it.
        wait_for("the child to finish its burst", || {
            matches!(session.status().stream, StreamState::Ended(_))
        });
        let chunks = session.output_chunks();
        assert!(
            chunks > 10,
            "the burst arrived in {chunks} reads, too few to prove anything"
        );

        let mut wakes = 0;
        while wake.take() {
            wakes += 1;
        }
        assert!(wakes >= 1, "a burst of {chunks} reads woke the view at all");
        // One token for the burst, and at most one more for the shell's exit
        // being reported afterwards. Not one per read, which is the failure.
        assert!(
            wakes <= 2,
            "{chunks} reads reached the view as {wakes} wakes"
        );
        assert!(
            chunks > 10 * wakes,
            "far smaller than the number of chunks is the point"
        );
        assert!(
            session.coalesced_wakes() > 0,
            "every read after the first found a wake pending"
        );
        assert!(
            grid(&session).contains("19999"),
            "the whole burst was parsed"
        );

        session.close().wait().expect("the session closes");
    }

    #[test]
    fn a_reader_that_stops_is_distinguished_from_a_shell_that_exits() {
        // The reader unwinds. The backend reports that as `ReaderStopped` from
        // the reader thread's own `Drop`, which is the case a stopped reader has
        // to survive: output stops arriving while the child is perfectly alive,
        // and nothing about the child says so.
        let dir = TempDir::new().expect("a temporary directory");
        let mut shell = config(dir.path(), "printf '\\033]52;c;aGVsbG8=\\a'; sleep 30");
        shell.host = TerminalHost {
            store_clipboard: Box::new(|_| panic!("the reader is asked to stop")),
            ..TerminalHost::default()
        };
        let (backend, session, _wake) = start(shell);

        let quiet = QuietPanics::new();
        wait_for("the reader to stop", || {
            matches!(session.status().stream, StreamState::Ended(_))
        });
        drop(quiet);

        let status = session.status();
        assert_eq!(status.stream, StreamState::Ended(ReaderEnd::ReaderStopped));
        assert_eq!(
            status.shell,
            ShellState::Running,
            "the child outlived the reader"
        );
        assert!(
            status.poll_error.is_none(),
            "a child with no reader is still a readable child"
        );

        // A read that fails is not a read that said nothing was running.
        backend
            .close(session.id())
            .expect("the backend session closes");
        assert!(
            session.inner.poll_and_record().is_err(),
            "a closed backend session has no status to give"
        );
        let after_failure = session.status();
        assert!(
            after_failure.poll_error.is_some(),
            "the failure stays visible"
        );
        assert_eq!(
            after_failure.shell,
            ShellState::Running,
            "the last good reading stands; a failed read is not an idle terminal"
        );
        assert!(!after_failure.foreground.running);

        // And a close that cannot make that read refuses to go on rather than
        // killing a child it knows nothing about.
        assert!(matches!(
            session.close().wait(),
            Err(SessionError::StatusUnavailable(_))
        ));
        assert!(!session.is_closing(), "the session is left as it was");

        // The other direction: a shell that exits on its own, with the reader ending
        // for its own reason and carrying it. Nothing here is derived from the
        // exit -- the first half of this test is a stream that ended while the
        // shell was Running, and neither half could be written as the other.
        let exited = TempDir::new().expect("a temporary directory");
        let (_backend, session, _wake) = start(config(exited.path(), "printf 'bye\\n'; exit 3"));
        wait_for("the shell to exit", || {
            matches!(session.status().shell, ShellState::Exited { .. })
        });
        assert_eq!(session.status().shell, ShellState::Exited { code: Some(3) });
        wait_for("its output to drain", || {
            session.status().stream == StreamState::Ended(ReaderEnd::EndOfStream)
        });
        assert!(session.status().poll_error.is_none());

        session.close().wait().expect("the session closes");
    }

    /// A command's exit status and the shell's own are two different facts, and
    /// only the first one is something a sidebar may show. Here the shell exits
    /// 3 while the last command it ran reported 1, and neither overwrites the
    /// other: nothing is read off the child's wait status, and nothing is
    /// inferred from a foreground process.
    #[test]
    fn a_shell_exit_status_is_never_read_as_a_command_exit_status() {
        let dir = TempDir::new().expect("a temporary directory");
        let (_backend, session, _wake) =
            start(config(dir.path(), "printf '\\033]133;D;1\\a'; exit 3"));
        wait_for("the shell to exit", || {
            matches!(session.status().shell, ShellState::Exited { .. })
        });
        wait_for("its output to drain", || {
            session.status().stream == StreamState::Ended(ReaderEnd::EndOfStream)
        });

        assert_eq!(session.status().shell, ShellState::Exited { code: Some(3) });
        assert_eq!(
            session.last_command_exit(),
            Some(1),
            "the marker the shell printed, not the status it died with"
        );

        session.close().wait().expect("the session closes");
    }

    #[test]
    fn close_refuses_later_input() {
        let dir = TempDir::new().expect("a temporary directory");
        let (backend, session, _wake) = start(config(dir.path(), "sleep 30"));

        let report = session.begin_close().expect("the close begins");
        assert!(
            session.is_closing(),
            "a close that has begun is visible as one"
        );
        assert!(
            !report.closed,
            "asking to close has not closed anything yet"
        );

        assert!(matches!(
            session.send_key(&press(Key::Enter)),
            Err(SessionError::Closing)
        ));
        assert!(matches!(session.paste("hello"), Err(SessionError::Closing)));
        assert!(matches!(
            session.paste_confirmed("hello\n"),
            Err(SessionError::Closing)
        ));
        assert!(matches!(
            session.resize(100, 40),
            Err(SessionError::Closing)
        ));

        let report = session.close().wait().expect("the session closes");
        assert!(
            report.closed,
            "the backend closed the session it was asked to"
        );
        assert!(
            backend.status(session.id()).is_err(),
            "the backend session is gone"
        );
    }

    #[test]
    fn every_session_is_gone_after_shutdown() {
        let dir = TempDir::new().expect("a temporary directory");
        let backend = Arc::new(TerminalBackend::default());

        // The child publishes its own pid, which is the only way a test can hold
        // a number the OS will answer for.
        let mut pids = Vec::new();
        let mut sessions = Vec::new();
        for index in 0..2 {
            let pid_file = dir.path().join(format!("{index}.pid"));
            let mut shell = config(
                dir.path(),
                &format!("echo $$ > {}; sleep 30", pid_file.display()),
            );
            shell.id = format!("shutdown-test-{index}");
            sessions.push(
                TerminalSession::spawn(Arc::clone(&backend), shell).expect("the session spawns"),
            );

            let path = pid_file.clone();
            wait_for("the child to publish its pid", || path.exists());
            pids.push(
                std::fs::read_to_string(&pid_file)
                    .expect("the pid file reads")
                    .trim()
                    .parse::<i32>()
                    .expect("a pid"),
            );
        }

        // Dropped rather than closed: a dropped session stops its own threads
        // and leaves the PTY, which is exactly the state shutdown is for.
        drop(sessions);
        shutdown(&backend);

        for pid in pids {
            wait_for(&format!("process {pid} to be gone"), || {
                process_is_gone(pid)
            });
        }
    }

    #[test]
    fn a_terminal_reply_the_child_asked_for_is_sent_and_is_not_dropped() {
        let dir = TempDir::new().expect("a temporary directory");
        let reply_file = dir.path().join("reply.bin");

        // Raw mode, or the line discipline would hold a reply that has no
        // newline until something else arrives. `dd bs=1` writes each byte as it
        // reads it, so the file is the child's own record of what it was sent
        // and not a buffer that a kill would take with it.
        let script = format!(
            "stty raw -echo; printf '\\033[?u'; dd bs=1 of='{}' 2>/dev/null",
            reply_file.display()
        );
        let (_backend, session, _wake) = start(config(dir.path(), &script));

        let read = || std::fs::read(&reply_file).unwrap_or_default();
        wait_for("the child's keyboard-protocol query to be answered", || {
            let bytes = read();
            // `CSI ? <flags> u`: the mode the terminal has pushed, which is
            // nothing at all for a child that only asked.
            bytes.starts_with(b"\x1b[?") && bytes.ends_with(b"u") && bytes.len() > 4
        });

        let answer = read();
        session.close().wait().expect("the session closes");
        assert!(
            answer.starts_with(b"\x1b[?"),
            "the reply is the child's, not ours"
        );
        assert!(answer.ends_with(b"u"));
        assert!(
            answer[3..answer.len() - 1].iter().all(u8::is_ascii_digit),
            "a mode flag list, not something half-written: {answer:?}"
        );
    }

    /// The shell under Pi's name.
    ///
    /// The operating system names a process by the file it was started from --
    /// its first argument on macOS, its `comm` on Linux -- so a link called `pi`
    /// is a process the panel will call Pi. That is the whole of what the
    /// foreground identity is: the OS's own answer, and not anything the
    /// terminal was told.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn fake_pi(dir: &Path) -> PathBuf {
        let pi = dir.join("pi");
        std::os::unix::fs::symlink("/bin/sh", &pi).expect("the shell is linked");
        pi
    }

    #[test]
    fn a_program_status_query_is_answered_with_the_same_question() {
        let dir = TempDir::new().expect("a temporary directory");
        let reply_file = dir.path().join("reply.bin");

        // Raw mode, or the line discipline would hold a reply that has no
        // newline until something else arrives.
        let script = format!(
            "stty raw -echo; printf '\\033]7501;?\\033\\\\'; dd bs=1 of='{}' 2>/dev/null",
            reply_file.display()
        );
        let (_backend, session, _wake) = start(config(dir.path(), &script));

        let read = || std::fs::read(&reply_file).unwrap_or_default();
        wait_for("the program-status query to be answered", || {
            read() == b"\x1b]7501;?\x1b\\"
        });

        session.close().wait().expect("the session closes");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_program_status_is_believed_while_pi_is_in_front_and_forgotten_when_it_is_not() {
        let dir = TempDir::new().expect("a temporary directory");
        let pi = fake_pi(dir.path());
        // An interactive shell, because job control is what puts a command in
        // front of the shell: a shell that runs a script has no foreground
        // group of its own to give away.
        let (_backend, session, _wake) = start(interactive_config(dir.path()));
        // The report is written again and again: a report that arrives before
        // the operating system has said what is in front is not believed yet,
        // and the next one is.
        session
            .send_text(&format!(
                "{} -c 'for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do printf \"\\033]7501;state=working\\007\"; sleep 0.1; done'\n",
                pi.display()
            ))
            .expect("the command is sent");

        wait_for("the program status to be believed", || {
            session.status().pi_task == Some(PiTaskState::Working)
        });
        // The process is gone, so what it said is about a process that is not
        // there any more, and the row must not still be claiming a task.
        wait_for("the status to be forgotten when Pi is gone", || {
            session.status().pi_task.is_none()
        });

        session.close().wait().expect("the session closes");
    }

    #[test]
    fn a_spawn_that_cannot_open_is_reported_rather_than_swallowed() {
        let dir = TempDir::new().expect("a temporary directory");
        let mut shell = config(dir.path(), "sleep 30");
        shell.cwd = dir.path().join("not-a-directory");
        let backend = Arc::new(TerminalBackend::default());
        assert!(matches!(
            TerminalSession::spawn(backend, shell),
            Err(SessionError::Spawn(_))
        ));

        let mut zero = config(dir.path(), "sleep 30");
        zero.cols = 0;
        let backend = Arc::new(TerminalBackend::default());
        assert!(matches!(
            TerminalSession::spawn(backend, zero),
            Err(SessionError::InvalidSize { cols: 0, .. })
        ));
    }
}
