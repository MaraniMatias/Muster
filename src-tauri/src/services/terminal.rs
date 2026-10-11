use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{
    domain::workspace::{Session, SessionStatus, SessionType, WorkspaceState},
    persistence::{timestamp, Database},
    terminal::{OutputSink, SpawnOptions, TerminalBackend},
};

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

/// Long enough for a branch-like name, short enough that a sidebar row never has to scroll
/// to show what a terminal is.
const MAX_NAME_CHARS: usize = 60;

pub struct CreatedTerminal {
    pub session: Session,
    pub workspace: WorkspaceState,
}

pub struct TerminalOptions {
    pub cols: u16,
    pub rows: u16,
    pub prompt: Option<String>,
}

pub fn create(
    database: &Database,
    backend: &TerminalBackend,
    checkout_id: &str,
    cols: u16,
    rows: u16,
    output: OutputSink,
) -> Result<CreatedTerminal, String> {
    create_with_options(
        database,
        backend,
        checkout_id,
        TerminalOptions {
            cols,
            rows,
            prompt: None,
        },
        output,
    )
}

pub fn create_with_options(
    database: &Database,
    backend: &TerminalBackend,
    checkout_id: &str,
    options: TerminalOptions,
    output: OutputSink,
) -> Result<CreatedTerminal, String> {
    create_with_settings(
        database,
        backend,
        checkout_id,
        options,
        &None,
        &None,
        output,
    )
}

/// `shell_integration` is read from the settings file rather than passed from the frontend, so that
/// turning it off needs no IPC change at all and cannot be bypassed by a caller that forgot.
///
/// `None` means "no settings file was reachable", which is treated as the default: the feature ships
/// on, and a settings file that cannot be read is a settings dialog problem rather than a reason to
/// start every terminal uninstrumented.
///
/// `script_dir` is where the shell-integration script is written, and is `None` when this app has no
/// folder in the user's home to write it into — a terminal opened then is simply uninstrumented
/// rather than pointed at a file that is not there.
pub fn create_with_settings(
    database: &Database,
    backend: &TerminalBackend,
    checkout_id: &str,
    options: TerminalOptions,
    shell_integration: &Option<bool>,
    script_dir: &Option<PathBuf>,
    output: OutputSink,
) -> Result<CreatedTerminal, String> {
    let TerminalOptions { cols, rows, prompt } = options;
    let cwd = database.terminal_checkout_path(checkout_id)?;
    reject_prompt(prompt.as_deref())?;
    let mut program = inherited_shell();
    let name = program
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "shell".into());
    let session = Session {
        id: format!(
            "session:terminal:{}-{}-{}",
            timestamp(),
            std::process::id(),
            NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed)
        ),
        session_type: SessionType::Shell,
        checkout_id: checkout_id.to_string(),
        name,
        created_at: timestamp(),
        status: SessionStatus::Active,
    };

    let mut args = inherited_shell_args(&program);
    // Resolved before the spawn because `program` is moved into it below, and the hook text is a
    // property of the shell that is being started rather than of the PTY that is about to exist.
    // Read here rather than at the prompt so that turning the setting off leaves every terminal that
    // is already open exactly as it was, and turning it on does not retrofit one.
    //
    // The script is written here, before the spawn, and not at launch: that is what makes turning the
    // setting on part-way through a session work, without the file having to exist for a setting that
    // was off when the app started. A failure is swallowed — a terminal with no markers is still a
    // usable terminal, which is the whole fallback.
    let integration = if shell_integration.unwrap_or(true) {
        install_shell_integration(&program, script_dir.as_deref())
    } else {
        None
    };
    let mut child_env = Vec::new();
    if let Some(integration) = integration {
        program = integration.program;
        args = integration.args;
        child_env = integration.env;
    }
    backend.spawn(
        session.id.clone(),
        SpawnOptions {
            program,
            args,
            cwd,
            cols,
            rows,
            env: child_env,
        },
        output,
    )?;

    match database.add_terminal_session(&session) {
        Ok(workspace) => Ok(CreatedTerminal { session, workspace }),
        Err(error) => {
            let _ = backend.close(&session.id);
            Err(error)
        }
    }
}

/// Renames a session, so a row the user cannot tell apart from its neighbours gets a name.
///
/// The name is a label and nothing more: it is never a path, an argument or a command, so the
/// only rules are that it is something to read and short enough for a sidebar row.
pub fn rename(database: &Database, session_id: &str, name: &str) -> Result<WorkspaceState, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a session needs a name".into());
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(format!(
            "a session name is at most {MAX_NAME_CHARS} characters"
        ));
    }
    // Control characters would break the row they are drawn in, and a newline would draw a
    // second one. Neither is a name anyone means to type.
    if name.chars().any(char::is_control) {
        return Err("a session name cannot contain control characters".into());
    }
    database.rename_terminal_session(session_id, name)
}

pub fn close(
    database: &Database,
    backend: &TerminalBackend,
    session_id: &str,
) -> Result<WorkspaceState, String> {
    backend.close(session_id)?;
    database.remove_terminal_session(session_id)
}

fn inherited_shell() -> PathBuf {
    env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .or_else(|| {
            ["/bin/zsh", "/bin/bash", "/bin/sh"]
                .into_iter()
                .map(PathBuf::from)
                .find(|path| path.is_file())
        })
        .unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

fn inherited_shell_args(program: &Path) -> Vec<String> {
    let mut args = vec!["-l".into()];
    if program.file_name().and_then(|name| name.to_str()) == Some("zsh") {
        args.extend(["-o".into(), "AUTO_MENU".into()]);
    }
    args
}

/// The OSC 133 markers the sourced script below makes the shell print, spelled once.
///
/// `D;<code>` is the shell's own `$?` after a command, and `C` marks command execution start, which
/// tells the row to stop being red. OSC 133 is the ident every shell-integration script uses, and xterm.js
/// ships no handler for it, so nothing else contends for these bytes.
///
/// The hooks emit `C` from zsh's `preexec_functions` and bash's `PS0`, both of which fire immediately
/// before command execution. `B` is the prompt end, which says where the next line begins being
/// editable, and `A` — the prompt start — is the half of that boundary this consumer has no use
/// for, so it is still not emitted. This is not the full OSC 133 prompt lifecycle.
///
/// `the_started_marker_fires_for_builtins_and_before_their_output` pins the timing for zsh; the bash
/// test does the same where that bash supports `PS0`.
///
/// `print -P` rather than `printf` for zsh, which takes the escapes as two characters where `printf`
/// needs four: `\e` against `\033` and `\a` against `\007`.
const OSC_STARTED: &str = r"\e]133;C\a";

/// The same two markers for bash, which has no `print` and so spells the escapes the long way.
const OSC_EXIT_BASH: &str = r"\033]133;D;%s\007";
const OSC_STARTED_BASH: &str = r"\033]133;C\007";

/// Where the shell's line editor starts, which is the *end* of the prompt rather than its beginning:
/// everything from here to the cursor is what the person typed, and a click outside that is a click
/// on a prompt or on output.
///
/// It rides at the tail of `PS1` because the prompt is the last thing a shell prints before the next
/// line is typed, and a prompt is the only thing it expands on its way out. Printing it from
/// `precmd` or `PROMPT_COMMAND` would put it *before* the prompt, which is the other boundary, and
/// the tail of a wrapped prompt is the only place the cell it names is right.
const OSC_INPUT_STARTED_ZSH: &str = r"\e]133;B\a";

/// The same marker for bash, already wearing the `\[` … `\]` that tell it to count for no columns.
/// Without them the shell's prompt believes it is two cells wider than it looks, and every cell
/// after it is two columns left of where it was drawn — which moves the cursor to the wrong place.
const OSC_INPUT_STARTED_BASH: &str = r"\\[\033]133;B\007\\]";

/// Hooks run after user startup, before the first prompt; no PTY input is injected.
fn shell_integration_script(program: &Path) -> Option<(&'static str, String)> {
    match program.file_name().and_then(|name| name.to_str()) {
        Some("zsh") => Some((
            "hook.zsh",
            format!(
                r#"# Written by Muster.
if [[ -z ${{__muster_integrated-}} ]]; then
  typeset -g __muster_integrated=1 __muster_pending=0
  muster_pc() {{
    local __muster_status=$?
    (( __muster_pending )) || __muster_status=0
    __muster_pending=0
    printf '{OSC_EXIT_BASH}' "$__muster_status"
    # This runs after user prompt hooks, so it marks their final PS1 rather than a value they replace.
    # Guarded because this runs once per prompt, and a second copy would start the input one cell short
    # of where it really begins. The literal string says the bytes take up no columns, which keeps the
    # line editor's idea of the prompt width right.
    #
    # It is left unterminated on purpose. With the closing sequence written out, this shell prints a
    # brace after the marker instead of consuming it, at the end of every prompt.
    # the_prompt_end_marker_rides_at_the_end_of_the_prompt is what fails if that ever comes back.
    local __muster_input=$'{OSC_INPUT_STARTED_ZSH}'
    [[ $PS1 == *"$__muster_input"* ]] || PS1="$PS1%{{$__muster_input"
  }}
  muster_px() {{ __muster_pending=1; print -Pn "{OSC_STARTED}"; }}
  precmd_functions=(${{precmd_functions:#muster_pc}} muster_pc)
  preexec_functions+=(muster_px)
fi
"#
            ),
        )),
        Some("bash") => Some((
            "hook.bash",
            format!(
                r#"# Written by Muster.
if [[ -z ${{__muster_integrated-}} ]]; then
  __muster_integrated=1
  __muster_first_prompt=1
  __muster_prompt_begin() {{
    local __muster_status=$?
    if [[ $__muster_first_prompt == 1 ]]; then
      if [[ ${{__muster_restore_posix-}} == off ]]; then set +o posix; fi
      unset __muster_restore_posix
      __muster_status=0
      __muster_first_prompt=0
    fi
    __muster_command_status=$__muster_status
    return "$__muster_status"
  }}
  __muster_prompt_end() {{
    printf '{OSC_EXIT_BASH}' "$__muster_command_status"
    # User prompt hooks may replace PS1, so append the marker after they have all run.
    local __muster_input=$'{OSC_INPUT_STARTED_BASH}'
    [[ ${{PS1-}} == *"$__muster_input"* ]] || PS1="${{PS1-}}$__muster_input"
  }}
  if [[ ${{BASH_VERSINFO[0]}} -gt 5 || ${{BASH_VERSINFO[0]}} -eq 5 && ${{BASH_VERSINFO[1]}} -ge 1 ]] && [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a'* ]]; then
    PROMPT_COMMAND=(__muster_prompt_begin "${{PROMPT_COMMAND[@]}}" __muster_prompt_end)
  else
    PROMPT_COMMAND="__muster_prompt_begin${{PROMPT_COMMAND:+; $PROMPT_COMMAND}}
__muster_prompt_end"
  fi
  printf -v PS0 '%s' '{OSC_STARTED_BASH}'"${{PS0-}}"
fi
"#
            ),
        )),
        _ => None,
    }
}

struct ShellIntegration {
    program: PathBuf,
    args: Vec<String>,
    env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn install_shell_integration(
    program: &Path,
    script_dir: Option<&Path>,
) -> Option<ShellIntegration> {
    let (name, contents) = shell_integration_script(program)?;
    let dir = script_dir?;
    let bash = name == "hook.bash";
    // Preserve deliberately POSIX-configured shells instead of imposing ordinary Bash mode.
    if bash && env::var_os("POSIXLY_CORRECT").is_some() {
        return None;
    }
    let mut child_program = program.to_path_buf();
    let script = dir.join(name);
    let mut files = vec![(script.clone(), contents)];
    let mut overrides = Vec::new();
    let mut args = inherited_shell_args(program);
    if bash {
        // Bash ignores --rcfile for login shells; Bash 3.2 also skips ENV unless invoked as sh.
        // A private symlink retains the selected executable and native login/logout state. ENV
        // restores ordinary Bash mode before profiles, and the first prompt restores it again
        // after sh startup's late POSIX reset, before user prompt hooks or queued input.
        #[cfg(unix)]
        {
            use std::{
                hash::{Hash, Hasher},
                os::unix::fs::symlink,
            };
            let target = fs::canonicalize(program).ok()?;
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            target.hash(&mut hash);
            let link_dir = dir.join(format!("bash-{:016x}", hash.finish()));
            fs::create_dir_all(&link_dir).ok()?;
            let link = link_dir.join("sh");
            if fs::read_link(&link).as_ref().ok() != Some(&target) {
                // Never replace an unexpected file or another selected shell's link.
                if symlink(&target, &link).is_err() && fs::read_link(&link).ok() != Some(target) {
                    return None;
                }
            }
            child_program = link;
        }
        #[cfg(not(unix))]
        {
            return None;
        }
        let bootstrap = dir.join("bootstrap.bash");
        files.push((
            bootstrap.clone(),
            format!(
                r#"# Written by Muster.
set +o posix
BASH=$__MUSTER_BASH
if (( BASH_VERSINFO[0] >= 5 )); then BASH_ARGV0=$BASH; fi
unset __MUSTER_BASH
if [[ $__MUSTER_ENV_SET == 1 ]]; then export ENV=$__MUSTER_ENV; else unset ENV; fi
unset __MUSTER_ENV_SET __MUSTER_ENV
[[ ! -r /etc/profile ]] || . /etc/profile
if [[ -r $HOME/.bash_profile ]]; then . "$HOME/.bash_profile"
elif [[ -r $HOME/.bash_login ]]; then . "$HOME/.bash_login"
elif [[ -r $HOME/.profile ]]; then . "$HOME/.profile"
fi
if shopt -qo posix; then __muster_restore_posix=on; else __muster_restore_posix=off; fi
. {}
"#,
                shell_quote(&script)
            ),
        ));
        overrides.extend([
            (
                "__MUSTER_ENV_SET".into(),
                if env::var_os("ENV").is_some() {
                    "1"
                } else {
                    "0"
                }
                .into(),
            ),
            (
                "__MUSTER_ENV".into(),
                env::var_os("ENV").unwrap_or_default(),
            ),
            ("__MUSTER_BASH".into(), program.as_os_str().into()),
            ("ENV".into(), bootstrap.into_os_string()),
        ]);
        args.insert(0, "--noprofile".into());
    } else {
        let bootstrap = dir.join("zsh");
        overrides.extend([
            (
                "__MUSTER_ZDOTDIR_SET".into(),
                if env::var_os("ZDOTDIR").is_some() {
                    "1"
                } else {
                    "0"
                }
                .into(),
            ),
            (
                "__MUSTER_ZDOTDIR".into(),
                env::var_os("ZDOTDIR").unwrap_or_default(),
            ),
            ("ZDOTDIR".into(), bootstrap.clone().into_os_string()),
        ]);
        for file in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
            let mut text = String::from("# Written by Muster; forward the user's startup files.\n");
            if file == ".zshenv" {
                text.push_str("typeset -g __muster_bootstrap=$ZDOTDIR\n");
            }
            text.push_str("if [[ $__MUSTER_ZDOTDIR_SET == 1 ]]; then export ZDOTDIR=$__MUSTER_ZDOTDIR; else unset ZDOTDIR; fi\n");
            text.push_str(&format!(
                "[[ ! -r ${{ZDOTDIR-$HOME}}/{file} ]] || source \"${{ZDOTDIR-$HOME}}/{file}\"\n"
            ));
            text.push_str("__MUSTER_ZDOTDIR_SET=${+ZDOTDIR}\n__MUSTER_ZDOTDIR=${ZDOTDIR-}\n");
            if file == ".zlogin" {
                text.push_str(&format!(
                    "unset __MUSTER_ZDOTDIR_SET __MUSTER_ZDOTDIR __muster_bootstrap\nsource {}\n",
                    shell_quote(&script)
                ));
            } else if file == ".zshrc" {
                text.push_str(&format!("if [[ -o login && -o rcs ]]; then\n  export ZDOTDIR=$__muster_bootstrap\nelse\n  unset __MUSTER_ZDOTDIR_SET __MUSTER_ZDOTDIR __muster_bootstrap\n  source {}\nfi\n", shell_quote(&script)));
            } else {
                text.push_str("if [[ -o rcs ]]; then\n  export ZDOTDIR=$__muster_bootstrap\nelse\n  unset __MUSTER_ZDOTDIR_SET __MUSTER_ZDOTDIR __muster_bootstrap\nfi\n");
            }
            files.push((bootstrap.join(file), text));
        }
    }
    for (path, contents) in files {
        if fs::read_to_string(&path).is_ok_and(|existing| existing == contents) {
            continue;
        }
        let written = fs::create_dir_all(path.parent()?)
            .map_err(|error| error.to_string())
            .and_then(|()| {
                crate::services::files::atomic_write(
                    &path,
                    contents.as_bytes(),
                    script_permissions(&path),
                )
                .map_err(|error| error.message)
            });
        if let Err(error) = written {
            log::warn!(
                "terminal {} could not be integrated: {error}",
                program.display()
            );
            return None;
        }
    }
    Some(ShellIntegration {
        program: child_program,
        args,
        env: overrides,
    })
}

/// The mode the script is written with, or the one it already has.
///
/// Private to its owner, like `config.yml` and for the same reason: the script is code this app asks
/// somebody's shell to run, so it is not a file to leave readable by anyone else. One the person
/// chose is left alone, because a write is not a reason to undo a decision made outside this app.
fn script_permissions(script: &Path) -> fs::Permissions {
    if let Ok(metadata) = fs::metadata(script) {
        return metadata.permissions();
    }
    private_permissions()
}

#[cfg(unix)]
fn private_permissions() -> fs::Permissions {
    use std::os::unix::fs::PermissionsExt;
    fs::Permissions::from_mode(0o600)
}

#[cfg(not(unix))]
fn private_permissions() -> fs::Permissions {
    fs::Permissions::default()
}

/// Terminals take no prompt any more: the review goes to the agent bridge, not a PTY.
fn reject_prompt(prompt: Option<&str>) -> Result<(), String> {
    match prompt {
        None => Ok(()),
        Some(_) => Err("a terminal session cannot carry a prompt".into()),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        process::Command,
        sync::Arc,
        time::Duration,
    };

    use tempfile::tempdir;

    use crate::{
        domain::workspace::Repo,
        persistence::Database,
        terminal::{wait_for_output, OutputSink, SpawnOptions, TerminalBackend},
    };

    use super::{
        create_with_options, create_with_settings, inherited_shell_args, install_shell_integration,
        rename, shell_integration_script, shell_quote, ShellIntegration, TerminalOptions,
    };

    fn plain_repo(path: &Path) -> Repo {
        Repo::plain(path, "now").unwrap()
    }

    #[test]
    fn enables_native_zsh_menu_completion_and_leaves_startup_files_untouched() {
        assert_eq!(
            inherited_shell_args(Path::new("/bin/zsh")),
            vec!["-l", "-o", "AUTO_MENU"]
        );
        assert_eq!(inherited_shell_args(Path::new("/bin/bash")), vec!["-l"]);
        if Command::new("zsh").arg("--version").output().is_err() {
            return;
        }

        let directory = tempdir().unwrap();
        let startup_files = [
            (".zshenv", "export MUSTER_ZSHENV=loaded\n"),
            (".zprofile", "export MUSTER_ZPROFILE=loaded\n"),
            (".zshrc", "export MUSTER_ZSHRC=loaded\n"),
        ];
        for (name, contents) in startup_files {
            fs::write(directory.path().join(name), contents).unwrap();
        }
        let before: Vec<_> = startup_files
            .iter()
            .map(|(name, _)| fs::read(directory.path().join(name)).unwrap())
            .collect();

        // Skip distro-global rc files; the test asserts the user startup files are preserved.
        let args = inherited_shell_args(Path::new("zsh"));
        let output = Command::new("zsh")
            .args(&args)
            .arg("-d")
            .args([
                "-i",
                "-c",
                "[[ -o AUTO_MENU && $MUSTER_ZSHENV == loaded && $MUSTER_ZPROFILE == loaded && $MUSTER_ZSHRC == loaded ]]",
            ])
            .env("HOME", directory.path())
            .env("ZDOTDIR", directory.path())
            .env("HISTFILE", directory.path().join(".zsh_history"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "zsh did not preserve menu completion/startup semantics: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        for ((name, _), original) in startup_files.iter().zip(before) {
            assert_eq!(fs::read(directory.path().join(name)).unwrap(), original);
        }
    }

    fn script_in(directory: &Path, shell: &str) -> ShellIntegration {
        let line = install_shell_integration(Path::new(shell), Some(directory))
            .unwrap_or_else(|| panic!("{shell} was expected to be integrated"));
        line
    }

    fn assert_shell_reports_a_failing_command(shell: &str, session: &str) {
        assert_script_installs(shell, session, tempdir().unwrap().path());
    }

    fn assert_script_installs(shell: &str, session: &str, script_dir: &Path) {
        shell_stream(
            shell,
            session,
            script_dir,
            &[b"false\n"],
            b"\x1b]133;D;1\x07",
            |_| (),
        );
    }

    fn shell_stream(
        shell: &str,
        session: &str,
        script_dir: &Path,
        commands: &[&[u8]],
        ends_with: &[u8],
        check: impl Fn(&str),
    ) {
        let mut integration = script_in(script_dir, shell);
        // These tests cover user startup; distro-global compinit can prompt on insecure fpath
        // entries and consume the first command sent through the test PTY.
        if shell.ends_with("zsh") {
            integration.args.insert(0, "-d".into());
        }
        let home = tempdir().unwrap();
        integration.env.extend([
            ("HOME".into(), home.path().as_os_str().into()),
            ("__MUSTER_ZDOTDIR_SET".into(), "0".into()),
        ]);
        let backend = TerminalBackend::default();
        let (sender, receiver) = std::sync::mpsc::channel();
        let collected: Arc<std::sync::Mutex<Vec<u8>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let kept = Arc::clone(&collected);
        let output: OutputSink = Box::new(move |bytes| {
            kept.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend_from_slice(bytes);
            sender.send(bytes.to_vec()).map_err(|e| e.to_string())
        });
        backend
            .spawn(
                session.to_string(),
                SpawnOptions {
                    program: integration.program,
                    args: integration.args,
                    cwd: std::env::current_dir().unwrap(),
                    cols: 80,
                    rows: 24,
                    env: integration.env,
                },
                output,
            )
            .unwrap();

        // The script's own last command reports a clean exit, so this wait also proves the shell read
        // the file rather than being handed a line it could not find.
        wait_for_output(&receiver, b"\x1b]133;D;0\x07", Duration::from_secs(15));
        for command in commands {
            backend.write(session, command).unwrap();
        }
        // The last prompt drawn is where the effects of those commands are all in the stream, and which
        // marker ends it depends on the command: `false` ends a prompt with `D;1`, `true` with `D;0`.
        wait_for_output(&receiver, ends_with, Duration::from_secs(10));
        backend.close(session).unwrap();
        let stream = collected
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        check(&String::from_utf8_lossy(&stream));
    }

    #[test]
    fn a_failing_command_in_a_live_shell_reports_its_exit_code_over_osc_133() {
        for shell in ["/bin/zsh", "/bin/bash"] {
            if Command::new(shell).arg("--version").output().is_err() {
                continue;
            }
            assert_shell_reports_a_failing_command(shell, shell);
        }
    }

    #[test]
    fn the_started_marker_fires_for_builtins_and_before_their_output() {
        // What `echo` prints, chosen so that finding it means finding that command's output rather than
        // the line editor drawing the typed command back.
        const OUTPUT: &str = "MUSTER_MARKER_ORDERING_PROBE";
        for shell in ["/bin/zsh"] {
            if Command::new(shell).arg("--version").output().is_err() {
                continue;
            }
            shell_stream(
                shell,
                &format!("ordering:{shell}"),
                tempdir().unwrap().path(),
                &[
                    b"cd /tmp\n".as_slice(),
                    b"[ -d /tmp ]\n".as_slice(),
                    format!("echo {OUTPUT}\n").as_bytes(),
                ],
                b"\x1b]133;D;0\x07",
                |stream| {
                    // Counted from the point the script was sourced: everything before that is the shell
                    // starting up rather than a command anybody ran.
                    let after = stream;
                    let started = after.matches("\x1b]133;C\x07").count();
                    assert!(
                        started >= 3,
                        "{shell}: three commands produced {started} started markers:\n{stream}"
                    );
                    // Cut at the command's output, because a marker after it belongs to the *next*
                    // command — which is exactly the confusion this assertion is about.
                    let before_output =
                        &after[..after.rfind(OUTPUT).expect("the command produced no output")];
                    let (echoed, marker) = (
                        before_output
                            .rfind("echo ")
                            .expect("the command line was never echoed"),
                        before_output
                            .rfind("\x1b]133;C\x07")
                            .expect("no marker came before the command's output"),
                    );
                    assert!(
                        echoed < marker,
                        "{shell}: the marker is not between the command being typed and its output, \
                         so it is not marking the command starting:\n{stream}"
                    );
                },
            );
        }
    }

    #[test]
    fn the_prompt_end_marker_rides_at_the_end_of_the_prompt() {
        const INPUT_STARTED: &str = "\x1b]133;B\x07";
        for shell in ["/bin/zsh"] {
            if Command::new(shell).arg("--version").output().is_err() {
                continue;
            }
            shell_stream(
                shell,
                "prompt-end:zsh",
                tempdir().unwrap().path(),
                &[
                    b"false\n".as_slice(),
                    b"echo muster-prompt-end-probe\n".as_slice(),
                ],
                b"\x1b]133;D;0\x07",
                |stream| {
                    // One marker for every prompt, which is what catches the hook appending to PS1
                    // again on every prompt instead of checking whether it has already done so: a
                    // second copy says the input starts one cell before it really does, and a
                    // prompt drawn without one is a shell whose line cannot be clicked into.
                    // Either count, because the stream is read as soon as the last command reported
                    // and the prompt drawn after it may not have been written yet. Twice either
                    // count is the failure worth catching, and neither is a multiple.
                    let (prompts, reported) = (
                        stream.matches(INPUT_STARTED).count(),
                        stream.matches("\x1b]133;D;").count(),
                    );
                    assert!(
                        prompts == reported || prompts == reported + 1,
                        "{shell}: {prompts} markers for {reported} prompts, so a prompt is missing \
                         one or carries more:\n{stream}"
                    );
                    // And it is the end of the prompt rather than its beginning. A prompt is drawn
                    // after the previous command reported and before the next one starts, and the
                    // line is typed in between, so a marker inside that window is one that names
                    // the cell the first keystroke of the next command is written to.
                    let finished = stream.find("\x1b]133;D;1\x07").unwrap_or_else(|| {
                        panic!("{shell}: the failing command reported nothing:\n{stream}")
                    });
                    let ended = stream[finished..]
                        .find(INPUT_STARTED)
                        .map(|at| finished + at)
                        .unwrap_or_else(|| {
                            panic!(
                                "{shell}: the prompt drawn after that command has no end marker:\n{stream}"
                            )
                        });
                    let typed = stream[ended..].find("\x1b]133;C\x07").map(|at| ended + at);
                    assert!(
                        typed.is_some_and(|started| finished < ended && ended < started),
                        "{shell}: the marker is not between the prompt being drawn and the line \
                         being typed, so it is not marking where the input begins:\n{stream}"
                    );
                    assert_no_prompt_leftovers(stream, INPUT_STARTED);
                },
            );
        }
        for bash in bashes() {
            // A prompt of a known shape, so the marker can be looked for after its last character.
            // Looking for it in the stream rather than in PS1 is the point: it is what says the
            // marker did not move the prompt two columns to the left, which is what happens without
            // the `\[` and `\]` that make it take up no room.
            let stream = bash_session_after_install(
                &bash,
                "PS1='MUSTER_PROMPT> '",
                &[b"false\n"],
                b"\x1b]133;D;1\x07",
            );
            assert!(
                stream.contains(&format!("MUSTER_PROMPT> {INPUT_STARTED}")),
                "{bash}: the marker is not at the end of the prompt, or it moved the prompt:\n{stream}"
            );
            // One per prompt, or one fewer if the stream was read before the last prompt was
            // written; twice either count is the failure worth catching, and neither is a multiple.
            let (prompts, reported) = (
                stream.matches(INPUT_STARTED).count(),
                stream.matches("\x1b]133;D;").count(),
            );
            assert!(
                prompts == reported || prompts == reported + 1,
                "{bash}: {prompts} markers for {reported} prompts, so a prompt is missing one or \
                 carries more:\n{stream}"
            );
            assert_no_prompt_leftovers(&stream, INPUT_STARTED);
        }
    }

    /// Asserts that no part of the marker survives into the prompt as text.
    ///
    /// Wrapping the marker in the prompt escape that says it takes up no room is what keeps the
    /// line editor's idea of the prompt width right, and a shell that does not recognise the pair
    /// prints the closing half of it instead of consuming it: a brace at the end of every prompt,
    /// and a prompt one character wider than the one on screen.
    fn assert_no_prompt_leftovers(stream: &str, marker: &str) {
        for (at, _) in stream.match_indices(marker) {
            let after = &stream[at + marker.len()..];
            assert!(
                !after.starts_with('}'),
                "a brace follows the marker, so the closing half of the prompt escape was printed \
                 rather than consumed:\n{stream}"
            );
        }
    }

    #[test]
    fn the_zsh_script_reports_a_failure_in_a_zsh_with_no_startup_files() {
        if Command::new("/bin/zsh").arg("--version").output().is_err() {
            return;
        }
        assert_script_installs("/bin/zsh", "session:zsh-no-rc", tempdir().unwrap().path());
    }

    #[test]
    fn a_path_with_a_space_in_it_still_installs_the_script() {
        if Command::new("/bin/zsh").arg("--version").output().is_err() {
            return;
        }
        let directory = tempdir().unwrap();
        let with_a_space = directory.path().join("a folder of mine");
        fs::create_dir(&with_a_space).unwrap();
        assert_script_installs("/bin/zsh", "session:spaced", &with_a_space);
    }

    #[test]
    fn a_path_with_a_quote_in_it_is_quoted_the_way_a_shell_needs() {
        let line = shell_quote(Path::new("/home/someone/it's mine/hook.zsh"));
        assert_eq!(
            line, "'/home/someone/it'\\''s mine/hook.zsh'",
            "the quote must be closed, escaped and reopened, and the whole path wrapped once"
        );
        assert!(!line.contains('\n'));
    }

    #[test]
    fn the_script_is_written_next_to_the_settings_and_only_when_it_is_wanted() {
        let directory = tempdir().unwrap();
        let zsh = Path::new("/bin/zsh");
        let integration = install_shell_integration(zsh, Some(directory.path())).unwrap();
        assert_eq!(integration.args, inherited_shell_args(zsh));
        assert!(directory.path().join("hook.zsh").is_file());
        // The two shells get their own files rather than one that dispatches on `$0`, and only the one
        // a terminal actually asked for is written.
        assert!(!directory.path().join("hook.bash").exists());
        let bash = Path::new("/bin/bash");
        if shell_integration_script(bash).is_some() {
            install_shell_integration(bash, Some(directory.path())).unwrap();
            assert!(directory.path().join("hook.bash").is_file());
            assert_ne!(
                fs::read_to_string(directory.path().join("hook.zsh")).unwrap(),
                fs::read_to_string(directory.path().join("hook.bash")).unwrap()
            );
        }
        // It is private to its owner, because it is code this app asks somebody's shell to run.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(directory.path().join("hook.zsh"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "the script must not be readable by anyone else"
            );
        }
        // No folder to write into means no integration rather than a line pointing at nothing.
        assert!(install_shell_integration(zsh, None).is_none());
        assert!(shell_integration_script(Path::new("/bin/sh")).is_none());
    }

    fn bashes() -> Vec<String> {
        let mut found: Vec<String> = std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .map(|directory| format!("{directory}/bash"))
            .filter(|path| Path::new(path).is_file())
            .filter(|path| {
                Command::new(path)
                    .arg("--version")
                    .output()
                    .is_ok_and(|out| out.status.success())
            })
            .collect();
        found.dedup();
        if !found.iter().any(|path| path == "/bin/bash") {
            found.insert(0, "/bin/bash".into());
        }
        found
    }

    fn bash_honours_prompt_command_arrays(bash: &str) -> bool {
        let run = "\
PROMPT_COMMAND=(probe_one probe_two)
probe_one() { printf '[ONE]'; }
probe_two() { printf '[TWO]'; }
";
        // `true` is there to draw one more prompt, and the answer is on it.
        bash_session_with(bash, run, &[b"true\n"], b"\x1b]133;D;0\x07").contains("[ONE][TWO]")
    }

    fn bash_expands_ps0(bash: &str) -> bool {
        let run = "PS0='<MUSTER_PS0_PROBE>'\n";
        // `true` is there to draw one more prompt, and the answer is on it.
        bash_session_with(bash, run, &[b"true\n"], b"\x1b]133;D;0\x07")
            .contains("<MUSTER_PS0_PROBE>")
    }

    fn bash_session_after_install(
        bash: &str,
        rc: &str,
        commands: &[&[u8]],
        ends_with: &[u8],
    ) -> String {
        const SENTINEL: &str = "MUSTER_AFTER_INSTALL";
        let sentinel = format!("printf '{SENTINEL}\\n'\n");
        let mut typed: Vec<&[u8]> = vec![sentinel.as_bytes()];
        typed.extend_from_slice(commands);
        let stream = bash_session_with(bash, rc, &typed, ends_with);
        let (_, after) = stream
            .split_once(SENTINEL)
            .expect("the sentinel never printed, so nothing after the install can be told apart");
        after.to_string()
    }

    fn bash_session_with(bash: &str, rc: &str, commands: &[&[u8]], ends_with: &[u8]) -> String {
        let directory = tempdir().unwrap();
        let rcfile = directory.path().join("bashrc");
        let (name, script) = shell_integration_script(Path::new(bash)).unwrap();
        let hook = directory.path().join(name);
        fs::write(&hook, script).unwrap();
        fs::write(&rcfile, format!("{rc}\n. {}\n", shell_quote(&hook))).unwrap();
        let backend = TerminalBackend::default();
        let (sender, receiver) = std::sync::mpsc::channel();
        // Every chunk is kept rather than drained at the end, because `wait_for_output` consumes the
        // channel to find its marker and what came before that marker is the subject here.
        let collected: Arc<std::sync::Mutex<Vec<u8>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let kept = Arc::clone(&collected);
        let output: OutputSink = Box::new(move |bytes| {
            kept.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend_from_slice(bytes);
            sender.send(bytes.to_vec()).map_err(|e| e.to_string())
        });
        backend
            .spawn(
                "bash:existing".into(),
                SpawnOptions {
                    program: PathBuf::from(bash),
                    args: vec![
                        "--noprofile".into(),
                        "--rcfile".into(),
                        rcfile.to_string_lossy().into_owned(),
                        "-i".into(),
                    ],
                    cwd: std::env::current_dir().unwrap(),
                    cols: 80,
                    rows: 24,
                    env: vec![("HOME".into(), directory.path().as_os_str().into())],
                },
                output,
            )
            .unwrap();
        wait_for_output(&receiver, b"\x1b]133;D;0\x07", Duration::from_secs(15));
        for command in commands {
            backend.write("bash:existing", command).unwrap();
        }
        // The last prompt drawn is where the effects of those commands are all in the stream, so the
        // wait is on that command's own status: `false` ends a prompt with `D;1`, not `D;0`.
        wait_for_output(&receiver, ends_with, Duration::from_secs(10));
        backend.close("bash:existing").unwrap();
        let stream = collected
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        String::from_utf8_lossy(&stream).into_owned()
    }

    #[test]
    fn the_bash_script_keeps_a_prompt_that_was_already_there() {
        // The same three prompt commands as an array, which is what bash 5.1 and later use, and as a
        // string, which is what every bash before it uses and what a person who set one by hand has.
        const FUNCTIONS: &str = "\
user_prompt_one() { printf '[ONE:%s]' \"$?\"; }
user_prompt_two() { printf '[TWO:%s]' \"$?\"; }
user_prompt_three() { printf '[THREE:%s]' \"$?\"; }
";
        for bash in bashes() {
            let arrays_honoured = bash_honours_prompt_command_arrays(&bash);
            let rc = format!(
                "{FUNCTIONS}PROMPT_COMMAND=(user_prompt_one user_prompt_two user_prompt_three)\n"
            );
            let stream = bash_session_after_install(
                &bash,
                &rc,
                &[b"declare -p PROMPT_COMMAND\n", b"false\n"],
                b"\x1b]133;D;1\x07",
            );
            // Still an array, with the user's entries where they were. A prompt framework that reads
            // or rewrites `PROMPT_COMMAND` is looking at exactly this.
            assert!(
                stream.contains("declare -a PROMPT_COMMAND"),
                "{bash}: PROMPT_COMMAND stopped being the array the user had:\n{stream}"
            );
            for entry in ["user_prompt_one", "user_prompt_two", "user_prompt_three"] {
                assert!(
                    stream.contains(entry),
                    "{bash}: PROMPT_COMMAND lost {entry}:\n{stream}"
                );
            }
            // The begin/end hooks are separate elements around the user's entries on shells that run
            // every array element. On older bash only element 0 runs, so the string case below covers it.
            if arrays_honoured {
                assert!(
                    stream.contains("[0]=\"__muster_prompt_begin\"")
                        && stream.contains("[4]=\"__muster_prompt_end\""),
                    "{bash}: the integration hooks do not bracket the user's entries:\n{stream}"
                );
            }

            // The string form, which every bash runs in full — a bash before 5.1 runs only element 0 of
            // an array, so this is the one shape where all three can be watched running everywhere.
            let rc = format!(
                "{FUNCTIONS}PROMPT_COMMAND='user_prompt_one; user_prompt_two; user_prompt_three'\n"
            );
            let stream = bash_session_after_install(&bash, &rc, &[b"false\n"], b"\x1b]133;D;1\x07");
            for prompt in ["[ONE:", "[TWO:", "[THREE:"] {
                assert!(
                    stream.contains(prompt),
                    "{bash}: installing the integration dropped {prompt} from PROMPT_COMMAND:\n{stream}"
                );
            }
            // And the status is the command's rather than ours: `false` leaves 1, and a user's prompt
            // colouring is exactly the thing that reads it. Only the first of theirs can be holding
            // it — in a `;`-separated string each command resets `$?` for the next, which is what a
            // bare `PROMPT_COMMAND='a; b'` does with no integration installed either — so this is the
            // one assertion about `$?` that is about what this script does.
            assert!(
                stream.contains("[ONE:1]"),
                "{bash}: the first prompt command after ours saw $? = 0 rather than the command's status:\n{stream}"
            );
        }
    }

    #[test]
    fn prompt_end_markers_follow_dynamic_user_prompts_and_keep_exit_status() {
        const INPUT_STARTED: &str = "\x1b]133;B\x07";
        let check = |shell: &str, hook: &str, prompt: &str, stream: &str| {
            let hook_at = stream.rfind(hook).unwrap_or_else(|| {
                panic!("{shell}: user prompt hook did not report status:\n{stream}")
            });
            let finished_at = stream.rfind("\x1b]133;D;1\x07").unwrap_or_else(|| {
                panic!("{shell}: failed command exit marker is missing:\n{stream}")
            });
            let prompt_at = stream
                .rfind(prompt)
                .unwrap_or_else(|| panic!("{shell}: dynamic PS1 was not printed:\n{stream}"));
            assert!(
                hook_at < finished_at && finished_at < prompt_at,
                "{shell}: exit status was not reported after hooks and before the final prompt:\n{stream}"
            );
            let final_prompt = &stream[prompt_at..];
            assert!(
                final_prompt.starts_with(&format!("{prompt}{INPUT_STARTED}")),
                "{shell}: the marker is missing from the dynamic prompt:\n{stream}"
            );
            assert_eq!(
                final_prompt.matches(INPUT_STARTED).count(),
                1,
                "{shell}: dynamic prompt has duplicate input markers:\n{stream}"
            );
            let (prompts, reports) = (
                stream.matches(INPUT_STARTED).count(),
                stream.matches("\x1b]133;D;").count(),
            );
            assert!(
                prompts == reports || prompts == reports + 1,
                "{shell}: {prompts} input markers for {reports} prompt reports:\n{stream}"
            );
            assert_no_prompt_leftovers(stream, INPUT_STARTED);
        };

        for bash in bashes() {
            let home = tempdir().unwrap();
            let prompt_command = if bash_honours_prompt_command_arrays(&bash) {
                "PROMPT_COMMAND=(user_prompt)"
            } else {
                "PROMPT_COMMAND=user_prompt"
            };
            fs::write(
                home.path().join(".bash_profile"),
                format!(
                    "user_prompt() {{ printf '[BASH_HOOK:%s]' \"$?\"; PS1='MUSTER_DYNAMIC_BASH> '; }}\n{prompt_command}\n"
                ),
            )
            .unwrap();
            let scripts = home.path().join("scripts");
            let stream = native_session(
                &bash,
                home.path(),
                &scripts,
                false,
                &[b"false\n"],
                b"\x1b]133;D;1\x07",
                &[],
            );
            check(&bash, "[BASH_HOOK:1]", "MUSTER_DYNAMIC_BASH> ", &stream);
        }

        if Command::new("/bin/zsh")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            let home = tempdir().unwrap();
            fs::write(
                home.path().join(".zshrc"),
                "user_prompt() { printf '[ZSH_HOOK:%s]' \"$?\"; PS1='MUSTER_DYNAMIC_ZSH> '; }\nprecmd_functions=(user_prompt)\n",
            )
            .unwrap();
            let scripts = home.path().join("scripts");
            let stream = native_session(
                "/bin/zsh",
                home.path(),
                &scripts,
                false,
                &[b"false\n"],
                b"\x1b]133;D;1\x07",
                &[],
            );
            check("/bin/zsh", "[ZSH_HOOK:1]", "MUSTER_DYNAMIC_ZSH> ", &stream);
        }
    }

    #[test]
    fn the_bash_started_marker_fires_before_the_command_it_marks() {
        const USER_PS0: &str = "PS0='[USER:%s:%d]'\n";
        for bash in bashes() {
            let expands_ps0 = bash_expands_ps0(&bash);
            // `cd` and `[ … ]` are the two that would be missed by anything watching for a process,
            // and `false` is the one the feature exists for.
            let stream = bash_session_after_install(
                &bash,
                USER_PS0,
                &[
                    b"cd /tmp\n",
                    b"[ -f /etc/hosts ]\n",
                    b"false\n",
                    b"echo \"probe$((6*7))\"\n",
                ],
                b"\x1b]133;D;1\x07",
            );

            if expands_ps0 {
                for command in ["cd /tmp", "[ -f /etc/hosts ]", "false"] {
                    assert!(
                        stream.contains(command),
                        "{bash}: {command} never ran, so this says nothing about a marker for it:\n{stream}"
                    );
                }
                // One marker per command, the sentinel included. Counting is what catches a hook that
                // fires for `echo` and not for a builtin, which is the failure this script had with
                // the trap it replaced: `cd` and `[ … ]` are builtins and `false` is the one the
                // feature is for.
                assert!(
                    stream.matches("\x1b]133;C\x07").count() >= 5,
                    "{bash}: {} markers for 5 commands, so PS0 is not firing for every command:\n{stream}",
                    stream.matches("\x1b]133;C\x07").count()
                );
                // And it lands where `preexec` does in zsh: after the line that was typed, before
                // anything the command prints. The output is searched for as `probe42` rather than as
                // the command text, because the terminal echoes the command back and a search for the
                // text itself finds the echo and settles the question backwards.
                let echoed = stream.find("probe$((6*7))").unwrap_or_else(|| {
                    panic!(
                        "{bash}: the probe command was never echoed, so its output cannot be told \
                             from its command line:\n{stream}"
                    )
                });
                let marker = stream[echoed..]
                    .find("\x1b]133;C\x07")
                    .map(|at| echoed + at)
                    .unwrap_or_else(|| {
                        panic!("{bash}: PS0 is expanded but the probe command got no marker:\n{stream}")
                    });
                let output = stream.find("probe42").unwrap_or_else(|| {
                    panic!("{bash}: the probe command produced no output:\n{stream}")
                });
                assert!(
                    echoed < marker && marker < output,
                    "{bash}: the started marker is not between the command being typed and its \
                     output, so it is not marking the command starting:\n{stream}"
                );
                // And the value we prepended to is still the user's, firing after ours.
                assert!(
                    stream.contains("[USER:%s:%d]"),
                    "{bash}: prepending the marker to PS0 altered the user's format string:\n{stream}"
                );
            } else {
                // A bash without `PS0` reports the end of a command and not the start of one, which is
                // the documented cost of not taking a `DEBUG` trap it cannot chain. What must still
                // hold is that nothing of the user's is harmed and the failure report still works.
                assert!(
                    !stream.contains("\x1b]133;C\x07"),
                    "{bash}: a started marker appeared although this bash does not expand PS0:\n{stream}"
                );
                assert!(
                    !stream.contains("[USER_PS0]"),
                    "{bash}: PS0 fired on a bash that has no PS0, so the user lost something that \
                     was never theirs to lose:\n{stream}"
                );
                assert!(
                    stream.contains("\x1b]133;D;1\x07"),
                    "{bash}: the exit code is missing on a bash without PS0, so a failure would not \
                     turn the row red:\n{stream}"
                );
            }
        }
    }

    #[test]
    fn the_script_is_written_once_and_reused() {
        let directory = tempdir().unwrap();
        let zsh = Path::new("/bin/zsh");
        let script = directory.path().join("hook.zsh");
        install_shell_integration(zsh, Some(directory.path())).unwrap();
        let written_at = fs::metadata(&script).unwrap().modified().unwrap();
        // A file whose contents no longer match must be replaced, or a change to the script would
        // never reach a terminal opened after it.
        fs::write(&script, "stale\n").unwrap();
        install_shell_integration(zsh, Some(directory.path())).unwrap();
        assert_ne!(
            fs::read_to_string(&script).unwrap(),
            "stale\n",
            "a script that no longer matches what this build writes must be replaced"
        );
        // And a second terminal with the script already right must leave it alone.
        let settled = fs::metadata(&script).unwrap().modified().unwrap();
        install_shell_integration(zsh, Some(directory.path())).unwrap();
        assert_eq!(fs::metadata(&script).unwrap().modified().unwrap(), settled);
        assert!(written_at <= settled);
    }

    fn opening_a_terminal_writes(script_dir: &Path, setting: Option<bool>) -> bool {
        let directory = tempdir().unwrap();
        let checkout = directory.path().join("checkout");
        fs::create_dir(&checkout).unwrap();
        let database = Database::open(directory.path().join("workspace.sqlite3")).unwrap();
        let repo = plain_repo(&checkout);
        database.register_plain_repo(repo.clone()).unwrap();
        let backend = TerminalBackend::default();
        let (sender, receiver) = std::sync::mpsc::channel();
        let output: OutputSink =
            Box::new(move |bytes| sender.send(bytes.to_vec()).map_err(|e| e.to_string()));

        let created = create_with_settings(
            &database,
            &backend,
            &repo.checkouts[0].id,
            TerminalOptions {
                cols: 80,
                rows: 24,
                prompt: None,
            },
            &setting,
            &Some(script_dir.to_path_buf()),
            output,
        )
        .unwrap();

        // A marker cannot match the tty echo: this proves startup completed and input executed,
        // whether integration was enabled, disabled or preparation failed.
        backend
            .write(&created.session.id, b"printf '\\036MUSTERPROBE\\037'\n")
            .unwrap();
        wait_for_output(&receiver, b"\x1eMUSTERPROBE\x1f", Duration::from_secs(15));
        assert!(backend.status(&created.session.id).is_ok());
        backend.close(&created.session.id).unwrap();
        script_dir.join("hook.zsh").is_file() || script_dir.join("hook.bash").is_file()
    }

    #[test]
    fn the_setting_decides_whether_a_new_terminal_is_integrated() {
        if Command::new("/bin/zsh").arg("--version").output().is_err() {
            return;
        }
        assert!(opening_a_terminal_writes(
            tempdir().unwrap().path(),
            Some(true)
        ));
        let off = tempdir().unwrap();
        assert!(!opening_a_terminal_writes(off.path(), Some(false)));
        assert!(
            !off.path().join("hook.zsh").exists(),
            "nothing may be written when the setting is off"
        );
        assert!(opening_a_terminal_writes(tempdir().unwrap().path(), None));
    }

    #[test]
    #[ignore = "regenerates committed fixtures; see MUSTER_CAPTURE_FIXTURES"]
    fn captures_a_real_session_for_the_render_test() {
        let directory = PathBuf::from(
            std::env::var_os("MUSTER_CAPTURE_FIXTURES").expect("set MUSTER_CAPTURE_FIXTURES"),
        );
        fs::create_dir_all(&directory).unwrap();
        let bash = "/bin/bash";
        for (name, shell, banner) in [
            ("zsh-login", "/bin/zsh", true),
            ("zsh-bare", "/bin/zsh", false),
            ("bash-login", bash, true),
        ] {
            let home = tempdir().unwrap();
            let scripts = tempdir().unwrap();
            let rc = "printf 'USER STARTUP BANNER\n'\nPS1='muster-test> '\n";
            if shell.ends_with("zsh") {
                fs::write(
                    home.path().join(".zshrc"),
                    if banner { rc } else { "PS1='muster-test> '\n" },
                )
                .unwrap();
            } else {
                fs::write(home.path().join(".bash_profile"), rc).unwrap();
            }
            let stream = native_session(
                shell,
                home.path(),
                scripts.path(),
                false,
                &[b"false\n"],
                b"\x1b]133;D;1\x07",
                &[],
            );
            fs::write(directory.join(format!("{name}.bin")), &stream).unwrap();
        }
    }

    fn native_session(
        shell: &str,
        home: &Path,
        scripts: &Path,
        immediate: bool,
        commands: &[&[u8]],
        end: &[u8],
        extra_env: &[(std::ffi::OsString, std::ffi::OsString)],
    ) -> String {
        let mut integration = script_in(scripts, shell);
        // Isolate user startup from distro-global interactive completion setup.
        if shell.ends_with("zsh") {
            integration.args.insert(0, "-d".into());
        }
        integration.env.extend([
            ("HOME".into(), home.as_os_str().into()),
            ("__MUSTER_ZDOTDIR_SET".into(), "0".into()),
        ]);
        integration.env.extend_from_slice(extra_env);
        let backend = TerminalBackend::default();
        let (sender, receiver) = std::sync::mpsc::channel();
        let collected = Arc::new(std::sync::Mutex::new(Vec::new()));
        let kept = Arc::clone(&collected);
        backend
            .spawn(
                "native".into(),
                SpawnOptions {
                    program: integration.program,
                    args: integration.args,
                    cwd: home.into(),
                    cols: 80,
                    rows: 24,
                    env: integration.env,
                },
                Box::new(move |bytes| {
                    kept.lock().unwrap().extend_from_slice(bytes);
                    sender.send(bytes.to_vec()).map_err(|e| e.to_string())
                }),
            )
            .unwrap();
        if !immediate {
            wait_for_output(&receiver, b"\x1b]133;D;0\x07", Duration::from_secs(15));
            // Fixture input models a person typing after readline is ready, not a redraw race.
            while receiver.recv_timeout(Duration::from_millis(100)).is_ok() {}
        }
        for command in commands {
            backend.write("native", command).unwrap();
        }
        wait_for_output(&receiver, end, Duration::from_secs(15));
        if !immediate {
            while receiver.recv_timeout(Duration::from_millis(100)).is_ok() {}
        }
        backend.close("native").unwrap();
        let bytes = collected.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn native_login_startup_preserves_order_banners_aliases_hooks_and_queued_input() {
        let mut shells = vec!["/bin/zsh".to_string()];
        shells.extend(bashes());
        for shell in shells {
            let home = tempdir().unwrap();
            let scripts = home.path().join("it's my scripts");
            fs::create_dir(&scripts).unwrap();
            let log = home.path().join("order");
            let history = home.path().join("history");
            if shell.ends_with("zsh") {
                let redirected = home.path().join("it's my config");
                fs::create_dir(&redirected).unwrap();
                fs::write(
                    home.path().join(".zshenv"),
                    format!(
                        "echo env >> {}; export ZDOTDIR={}\n",
                        shell_quote(&log),
                        shell_quote(&redirected)
                    ),
                )
                .unwrap();
                fs::write(
                    redirected.join(".zprofile"),
                    format!("echo profile >> {}\n", shell_quote(&log)),
                )
                .unwrap();
                fs::write(redirected.join(".zshrc"), format!("echo rc >> {}\nprintf 'USER STARTUP BANNER\n'\nalias native_alias='printf ALIAS_OK'\nHISTFILE={}; HISTSIZE=100; SAVEHIST=100\nuser_prompt() {{ printf USER_HOOK; return 0; }}\nprecmd_functions=(user_prompt)\nfalse\n", shell_quote(&log), shell_quote(&history))).unwrap();
                fs::write(
                    redirected.join(".zlogin"),
                    format!("echo login >> {}; false\n", shell_quote(&log)),
                )
                .unwrap();
                fs::write(redirected.join(".zlogout"), "printf '\\036LOGOUT\\037'\n").unwrap();
            } else {
                fs::write(
                    home.path().join(".bash_profile"),
                    format!(
                        "echo profile >> {}; . \"$HOME/.bashrc\"\n",
                        shell_quote(&log)
                    ),
                )
                .unwrap();
                fs::write(home.path().join(".bash_login"), "echo WRONG_PROFILE\n").unwrap();
                fs::write(home.path().join(".profile"), "echo WRONG_PROFILE\n").unwrap();
                fs::write(home.path().join(".bashrc"), format!("echo rc >> {}\nprintf 'USER STARTUP BANNER\n'\nalias native_alias='printf ALIAS_OK'\nHISTFILE={}\nPROMPT_COMMAND='printf USER_HOOK'\nfalse\n", shell_quote(&log), shell_quote(&history))).unwrap();
                fs::write(
                    home.path().join(".bash_logout"),
                    "printf '\\036LOGOUT\\037'\n",
                )
                .unwrap();
            }
            let stream = native_session(
                &shell,
                home.path(),
                &scripts,
                true,
                &[b"native_alias\n", b"false\n", b"exit\n"],
                b"\x1eLOGOUT\x1f",
                &[],
            );
            assert!(
                stream.contains("USER STARTUP BANNER")
                    && stream.contains("ALIAS_OK")
                    && stream.contains("USER_HOOK"),
                "{shell}: {stream}"
            );
            assert!(stream.contains("\x1b]133;D;1\x07"), "{shell}: {stream}");
            assert!(
                !stream.contains("WRONG_PROFILE")
                    && !stream.contains("hook.")
                    && !stream.contains("\x1b[2J"),
                "{shell}: {stream}"
            );
            assert_eq!(
                fs::read_to_string(log).unwrap(),
                if shell.ends_with("zsh") {
                    "env\nprofile\nrc\nlogin\n"
                } else {
                    "profile\nrc\n"
                }
            );
            assert!(fs::read_to_string(history)
                .unwrap()
                .contains("native_alias"));
            let first = stream.find("\x1b]133;D;").unwrap();
            assert!(
                stream[first..].starts_with("\x1b]133;D;0\x07"),
                "startup failure leaked: {stream}"
            );
        }
    }

    #[test]
    fn zsh_keeps_preexisting_and_mutated_zdotdir_and_installs_hooks_once() {
        let home = tempdir().unwrap();
        let original = home.path().join("original config");
        let changed = home.path().join("it's my changed config");
        fs::create_dir(&original).unwrap();
        fs::create_dir(&changed).unwrap();
        let scripts = home.path().join("scripts");
        fs::write(home.path().join(".zshenv"), "printf WRONG_HOME_CONFIG\n").unwrap();
        fs::write(
            original.join(".zshenv"),
            format!("export ZDOTDIR={}\n", shell_quote(&changed)),
        )
        .unwrap();
        fs::write(changed.join(".zprofile"), "printf 'USER PROFILE\n'\n").unwrap();
        fs::write(changed.join(".zshrc"), "PS1='user> '\n").unwrap();
        fs::write(changed.join(".zlogin"), "printf 'USER LOGIN\n'\n").unwrap();
        let hook = scripts.join("hook.zsh");
        let command = format!("source {}; source {}; [[ $ZDOTDIR == {} && -o AUTO_MENU && -o login && ${{#precmd_functions}} == 1 && ${{#preexec_functions}} == 1 && -z ${{__MUSTER_ZDOTDIR_SET+x}} ]] && printf '\\036PRESERVED\\037'\n", shell_quote(&hook), shell_quote(&hook), shell_quote(&changed));
        let stream = native_session(
            "/bin/zsh",
            home.path(),
            &scripts,
            false,
            &[command.as_bytes()],
            b"\x1ePRESERVED\x1f",
            &[
                ("__MUSTER_ZDOTDIR_SET".into(), "1".into()),
                ("__MUSTER_ZDOTDIR".into(), original.into_os_string()),
            ],
        );
        assert!(stream.contains("USER PROFILE") && stream.contains("USER LOGIN"));
        assert!(!stream.contains("WRONG_HOME_CONFIG"));
    }

    #[test]
    fn native_bash_keeps_login_state_normal_mode_env_and_user_debug_trap() {
        for bash in bashes() {
            let home = tempdir().unwrap();
            let scripts = home.path().join("scripts");
            fs::write(
                home.path().join(".bash_profile"),
                format!("shopt -q login_shell && ! shopt -qo posix && [[ $BASH == {} ]] && printf STARTUP_MODE_OK\ntrap 'printf USER_DEBUG' DEBUG\nPS1='user> '\n", shell_quote(Path::new(&bash))),
            )
            .unwrap();
            let hook = scripts.join("hook.bash");
            let command = format!(". {}; . {}; shopt -q login_shell && ! shopt -qo posix && [[ $ENV == 'user env' && -z ${{__MUSTER_ENV_SET+x}} ]] && printf '\\036PRESERVED\\037'\n", shell_quote(&hook), shell_quote(&hook));
            let stream = native_session(
                &bash,
                home.path(),
                &scripts,
                false,
                &[command.as_bytes()],
                b"\x1ePRESERVED\x1f",
                &[
                    ("__MUSTER_ENV_SET".into(), "1".into()),
                    ("__MUSTER_ENV".into(), "user env".into()),
                ],
            );
            assert!(
                stream.contains("USER_DEBUG") && stream.contains("STARTUP_MODE_OK"),
                "{stream}"
            );
            assert_eq!(
                stream.matches("\x1b]133;C\x07").count(),
                usize::from(bash_expands_ps0(&bash)),
                "{stream}"
            );
            let typed = stream.rfind(". ").expect("the command was not echoed");
            assert!(
                stream[typed..].contains("USER_DEBUG"),
                "user DEBUG trap was lost: {stream}"
            );
            assert_eq!(stream.matches("\x1b]133;D;0\x07").count(), 2, "{stream}");
        }
    }

    #[test]
    fn zsh_reports_failures_without_skipping_the_users_prompt_hooks() {
        let home = tempdir().unwrap();
        fs::write(home.path().join(".zshrc"), "precmd() { printf '[SPECIAL:%s]' \"$?\"; }\nuser_prompt() { printf '[HOOK:%s]' \"$?\"; }\nprecmd_functions=(user_prompt)\n").unwrap();
        let stream = native_session(
            "/bin/zsh",
            home.path(),
            tempdir().unwrap().path(),
            false,
            &[b"false\n"],
            b"\x1b]133;D;1\x07",
            &[],
        );
        assert!(stream.contains("[SPECIAL:1]"), "{stream}");
        assert!(stream.contains("[HOOK:1]"), "{stream}");
    }

    #[test]
    fn slow_native_startup_is_not_abandoned_after_five_seconds() {
        let home = tempdir().unwrap();
        fs::write(
            home.path().join(".zshrc"),
            "printf 'SLOW STARTUP\n'; sleep 5.2; printf 'STARTUP FINISHED\n'\n",
        )
        .unwrap();
        let stream = native_session(
            "/bin/zsh",
            home.path(),
            tempdir().unwrap().path(),
            true,
            &[b"false\n"],
            b"\x1b]133;D;1\x07",
            &[],
        );
        assert!(stream.contains("SLOW STARTUP") && stream.contains("STARTUP FINISHED"));
        assert!(!stream.contains("hook.") && !stream.contains("\x1b[2J"));
    }

    #[test]
    fn failed_preparation_leaves_a_usable_uninstrumented_login_shell() {
        let directory = tempdir().unwrap();
        let blocked = directory.path().join("blocked");
        fs::write(&blocked, "not a directory").unwrap();
        assert!(install_shell_integration(Path::new("/bin/zsh"), Some(&blocked)).is_none());
        assert!(!opening_a_terminal_writes(&blocked, Some(true)));
    }

    #[test]
    fn a_rename_is_a_label_and_nothing_else() {
        let directory = tempdir().unwrap();
        let checkout = directory.path().join("checkout");
        fs::create_dir(&checkout).unwrap();
        let database = Database::open(directory.path().join("workspace.sqlite3")).unwrap();
        let repo = plain_repo(&checkout);
        database.register_plain_repo(repo.clone()).unwrap();
        let session = crate::domain::workspace::Session {
            id: "session:rename".into(),
            session_type: crate::domain::workspace::SessionType::Shell,
            checkout_id: repo.checkouts[0].id.clone(),
            name: "zsh".into(),
            created_at: "now".into(),
            status: crate::domain::workspace::SessionStatus::Active,
        };
        database.add_terminal_session(&session).unwrap();

        // What the user typed around a name is not part of it.
        let renamed = rename(&database, &session.id, "  build logs \n").unwrap();
        assert_eq!(renamed.repos[0].checkouts[0].sessions[0].name, "build logs");

        // An empty name would leave a row with nothing on it to click.
        assert!(rename(&database, &session.id, "   ").is_err());
        // A newline would draw a second row inside the first.
        assert!(rename(&database, &session.id, "logs\nrm -rf /").is_err());
        // A name long enough to be a payload is refused, not truncated.
        assert!(rename(&database, &session.id, &"x".repeat(61)).is_err());
        assert!(rename(&database, &session.id, &"x".repeat(60)).is_ok());
        // A session the database does not hold is not renamed, whatever it is called.
        assert!(rename(&database, "session:unknown", "anything").is_err());
    }

    #[test]
    fn terminals_reject_a_prompt_because_the_review_goes_to_the_agent_bridge() {
        let directory = tempdir().unwrap();
        let checkout = directory.path().join("checkout");
        fs::create_dir(&checkout).unwrap();
        let database = Database::open(directory.path().join("workspace.sqlite3")).unwrap();
        let repo = plain_repo(&checkout);
        database.register_plain_repo(repo.clone()).unwrap();
        let backend = TerminalBackend::default();

        assert!(create_with_options(
            &database,
            &backend,
            &repo.checkouts[0].id,
            TerminalOptions {
                cols: 80,
                rows: 24,
                prompt: Some("injected".into()),
            },
            Box::new(|_| Ok(())),
        )
        .is_err());
    }
}
