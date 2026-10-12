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
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::core::{CursorSnapshot, PromptAnchor, RowSnapshot, TerminalCore, TerminalEvent};
use super::keys::{encode_key, KeyPress};
use super::modes::TerminalModes;
use super::paste::{encode_paste, PasteError};
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
    pub resolve_color: Box<dyn Fn(u8) -> (u8, u8, u8) + Send + Sync>,
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
}

/// One grid cell as `(row, column)`, in visible screen coordinates.
pub type GridPoint = (usize, usize);
/// An inclusive selection as its two endpoints, first cell included.
pub type GridRange = (GridPoint, GridPoint);

/// What the view draws, gathered in one pass so a frame is one lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSnapshot {
    pub rows: Vec<RowSnapshot>,
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
pub struct TerminalWake(mpsc::Receiver<()>);

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
    released: AtomicBool,
    /// Wakes that found a token already pending. Diagnostic, and the number
    /// that says whether coalescing is doing anything at all.
    coalesced: AtomicU64,
}

impl Wake {
    fn new() -> (Self, TerminalWake) {
        let (sender, receiver) = mpsc::sync_channel(1);
        (
            Self {
                sender: Mutex::new(Some(sender)),
                released: AtomicBool::new(false),
                coalesced: AtomicU64::new(0),
            },
            TerminalWake(receiver),
        )
    }

    fn signal(&self) {
        // Checked before the lock: a released session is one where every further
        // signal is work nobody will ever read.
        if self.released.load(Ordering::Acquire) {
            return;
        }
        let sender = self.sender.lock().expect("terminal wake poisoned");
        let Some(sender) = sender.as_ref() else {
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
            bell: AtomicU64::new(0),
            requested: Mutex::new((cols, rows)),
            applied: Mutex::new((cols, rows)),
            chunks: AtomicU64::new(0),
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

    fn spawn_worker(&self, name: &str, worker: impl FnOnce() + Send + 'static) {
        let handle = thread::Builder::new()
            .name(name.to_owned())
            .spawn(worker)
            .unwrap_or_else(|error| {
                panic!("the terminal {name} thread could not be spawned: {error}")
            });
        self.workers
            .lock()
            .expect("terminal control poisoned")
            .push(handle);
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

// ---------------------------------------------------------------------------------------------
// The session
// ---------------------------------------------------------------------------------------------

struct Inner {
    backend: Arc<TerminalBackend>,
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

            // The parser lock is the outermost lock in this file. The reader
            // takes it to parse and to answer the child's queries, the resize
            // worker takes it to resize the grid, and the view takes it to
            // snapshot. Nothing taken while holding it takes it again.
            {
                let mut core = side.shared.core();
                core.feed(bytes);
                handle_events(&side, &mut core);
            }

            // After the parse, never before: a wake the view acts on before the
            // bytes are parsed is a wake it renders nothing from.
            side.wake.signal();
            Ok(())
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
            poll_error: None,
        };

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
        SessionStatus {
            generation: polled.generation,
            shell: polled.shell,
            foreground: polled.foreground,
            stream,
            poll_error: polled.poll_error,
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
        stored.poll_error = Some(error);
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
        let mut core = inner.shared().core();
        core.resize(request.cols, request.rows);
    }
    match inner.backend.resize(&inner.id, request.cols, request.rows) {
        // The backend answers with the size the PTY itself reports, so this is
        // the OS's answer rather than this crate's arithmetic.
        Ok((cols, rows)) => inner.shared().record_applied(cols, rows),
        Err(error) => log::warn!("terminal {} could not be resized: {error}", inner.id),
    }
}

/// Act on what the parser reported, on the reader thread.
///
/// Everything here happens before the wake, because everything here is part of
/// what the view is about to be woken for.
fn handle_events(side: &ReaderSide, core: &mut TerminalCore) {
    for event in core.take_events() {
        match event {
            // Never dropped, and never given a private path around the queue:
            // a reply that is late is survivable, one that is dropped is a
            // child that waits for a second it will never be sent.
            TerminalEvent::PtyWrite(reply) => push_reply(&side.queue, reply.into_bytes()),
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
                let index = u8::try_from(index).unwrap_or(0);
                let (r, g, b) = (side.host.resolve_color)(index);
                push_reply(&side.queue, respond(r, g, b).into_bytes());
            }
            // Answered from the session's own size, because a child asking how
            // big its text area is is asking how big it will be.
            TerminalEvent::TextAreaSizeRequested { respond } => {
                let (cols, rows) = side.shared.requested();
                let metrics = (side.host.cell_metrics)();
                push_reply(
                    &side.queue,
                    respond(cols, rows, metrics.width, metrics.height).into_bytes(),
                );
            }
            TerminalEvent::Bell => {
                side.shared.bell.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Hand a protocol reply to the write queue, waiting its turn for room.
///
/// The reply shares the byte budget with every keystroke and paste, and the only
/// two ways out of a full budget are to wait or to exceed it. Waiting is what a
/// blocked child needs; a deadline on that waiting is what stops a closing
/// session from waiting for a child that will never read.
fn push_reply(queue: &WriteQueue, reply: Vec<u8>) {
    let deadline = Instant::now() + REPLY_ROOM_DEADLINE;
    loop {
        let outcome = queue.push(reply.clone());
        match outcome {
            Ok(()) => return,
            Err(EnqueueError::Stopped) => {
                // The session is over and the child is going with it. There is
                // nobody left to answer.
                log::warn!("a terminal reply was not sent because the session stopped writing");
                return;
            }
            Err(error) => {
                if Instant::now() >= deadline {
                    log::warn!(
                        "a terminal reply was dropped after waiting for queue room: {error:?}"
                    );
                    return;
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
        let env = config.env.clone();

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
            queue: Arc::new(WriteQueue::spawn({
                let backend = Arc::clone(&backend);
                let id = id.clone();
                move |bytes| backend.write(&id, bytes)
            })),
            host,
            wake,
        };
        let control = Arc::new(Control::default());

        let inner = Arc::new(Inner {
            backend: Arc::clone(&backend),
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
        control.spawn_worker("terminal-resize", move || {
            while resizer.control.wait_for_resize() {
                match resizer.control.take_resize() {
                    Some(request) => apply_resize(&resizer, request),
                    // Nothing left to take: another wake for a request already
                    // applied, and applying nothing is the whole answer.
                    None => continue,
                }
            }
        });

        let poller = Arc::clone(&inner);
        control.spawn_worker("terminal-status", move || loop {
            let _ = poller.poll_and_record();
            if !poller.control.keep_going(poller.status_interval) {
                return;
            }
        });

        Ok(Self { inner })
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
        let core = self.inner.shared().core();
        TerminalSnapshot {
            rows: core.visible_rows(),
            selection: core.selection_text(),
            selection_range: core.selection_range().and_then(|range| {
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
            }),
            cursor: core.cursor(),
            modes: core.modes(),
            title: core.title().map(str::to_owned),
            alt_screen: core.is_alt_screen(),
            mouse_tracking: core.is_mouse_tracking(),
            display_offset: core.display_offset(),
            prompt_anchor: core.prompt_anchor(),
        }
    }

    /// Update terminal text selection from viewport grid coordinates.
    pub fn select(&self, start: (usize, usize), end: (usize, usize)) {
        self.inner.shared().core().select(start, end);
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
        self.inner.push_input(bytes)
    }

    /// Encode a paste against the child's bracketed-paste mode, and queue it.
    pub fn paste(&self, text: &str) -> Result<(), SessionError> {
        self.accept_new_work()?;
        let modes = self.modes();
        let bytes = encode_paste(text, &modes).map_err(SessionError::Paste)?;
        self.inner.push_input(bytes)
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
            .spawn(move || inner.close_sequence())
            .expect("the terminal close thread could not be spawned");
        CloseHandle {
            worker: Some(worker),
        }
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
        // `shutdown` is the one that ends everything at once. A dropped session
        // leaves its shell to whichever of those happens next.
    }
}

/// The close, running somewhere else. Wait for it, or let it finish on its own.
pub struct CloseHandle {
    worker: Option<JoinHandle<Result<CloseReport, SessionError>>>,
}

impl CloseHandle {
    /// Wait for the close to finish.
    pub fn wait(mut self) -> Result<CloseReport, SessionError> {
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
            .paste("stty size; printf 'PROB%s\\n' E\n")
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
