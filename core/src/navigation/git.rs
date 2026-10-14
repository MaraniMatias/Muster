use super::*;

/// One wall-clock deadline shared by a read-only discovery/counting operation.
/// Reuse across repositories to bound an entire sweep, not each Git subprocess.
#[derive(Clone, Copy, Debug)]
pub struct GitReadBudget(std::time::Instant);

impl GitReadBudget {
    pub fn new(within: Duration) -> Self {
        Self(std::time::Instant::now() + within)
    }

    pub fn remaining(self) -> Result<Duration, String> {
        let remaining = self.0.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            Err("Git read operation timed out".into())
        } else {
            Ok(remaining)
        }
    }

    pub(super) fn deadline(self) -> std::time::Instant {
        self.0
    }
}

impl Default for GitReadBudget {
    fn default() -> Self {
        Self::new(GIT_READ_TIMEOUT)
    }
}

impl From<Duration> for GitReadBudget {
    fn from(within: Duration) -> Self {
        Self::new(within)
    }
}

pub(super) fn output_text(output: &Output) -> Result<&str, String> {
    let text = std::str::from_utf8(&output.stdout).map_err(|_| "Git returned non-UTF-8 output")?;
    Ok(text.strip_suffix('\n').unwrap_or(text))
}

pub(super) fn is_plain_directory(path: &Path, output: &Output) -> Result<bool, String> {
    if output.status.code() != Some(128)
        || String::from_utf8_lossy(&output.stderr).trim_end()
            != "fatal: not a git repository (or any of the parent directories): .git"
    {
        return Ok(false);
    }
    for ancestor in path.ancestors() {
        let marker = ancestor.join(".git");
        match fs::symlink_metadata(&marker) {
            // A `.git` file is a worktree or submodule link, and a broken one
            // still fails closed. A `.git` directory only counts when Git would
            // have called it a checkout: a stub holding nothing but `info/`,
            // which some tool leaves in a home directory, is not a repository
            // Git refused, and reading it as one would refuse every plain
            // folder underneath it.
            Ok(metadata) if !metadata.is_dir() || is_git_directory(&marker)? => return Ok(false),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("could not inspect Git metadata: {error}")),
        }
    }
    Ok(true)
}

/// `HEAD` with `objects` and `refs` is what makes a `.git` directory a
/// repository to Git; without them there was never one to break.
pub(super) fn is_git_directory(marker: &Path) -> Result<bool, String> {
    for entry in ["HEAD", "objects", "refs"] {
        match fs::symlink_metadata(marker.join(entry)) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(format!("could not inspect Git metadata: {error}")),
        }
    }
    Ok(true)
}

pub(super) fn optional_ref(
    cwd: &Path,
    args: &[&str],
    within: impl Into<GitReadBudget>,
) -> Result<Option<String>, String> {
    let output = git_output(cwd, args, within)?;
    if output.status.success() {
        return Ok(Some(output_text(&output)?.to_owned()));
    }
    if output.status.code() == Some(1) && output.stdout.is_empty() && output.stderr.is_empty() {
        return Ok(None);
    }
    Err(git_error("could not read Git reference", &output))
}

pub(super) fn git_success(
    cwd: &Path,
    args: &[&str],
    within: impl Into<GitReadBudget>,
) -> Result<Output, String> {
    let output = git_output(cwd, args, within)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(git_error("Git read failed", &output))
    }
}

pub(super) fn git_error(context: &str, output: &Output) -> String {
    format!(
        "{context} ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim_end()
    )
}

pub(super) fn git_command(cwd: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(cwd)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env_remove("LANGUAGE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The opened path, not a parent process's Git invocation, owns the operation.
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
    ] {
        command.env_remove(name);
    }
    command
}

/// Bounded, nonblocking pipe collection on the supported macOS/Linux targets.
/// The child stays unreaped until its pipes close, reserving the process-group
/// identity so timeout/overflow cleanup cannot signal a recycled PID.
#[cfg(unix)]
pub(super) fn git_output(
    cwd: &Path,
    args: &[&str],
    within: impl Into<GitReadBudget>,
) -> Result<Output, String> {
    use std::{
        os::{fd::AsRawFd, unix::process::CommandExt},
        process::Child,
        thread,
        time::Instant,
    };

    struct ReadChild(Child, bool);
    impl Drop for ReadChild {
        fn drop(&mut self) {
            if !self.1 {
                // process_group(0) gave this still-unreaped child its own group.
                unsafe { libc::kill(-(self.0.id() as i32), libc::SIGKILL) };
                let _ = self.0.wait();
            }
        }
    }
    fn nonblocking(pipe: &impl AsRawFd) -> Result<(), String> {
        let fd = pipe.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(format!(
                "could not read Git's pipes: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
    fn drain(pipe: &mut impl Read, bytes: &mut Vec<u8>, cap: usize) -> Result<bool, String> {
        let mut buffer = [0; 8192];
        // Yield to the deadline even when a process produces output continuously.
        for _ in 0..32 {
            match pipe.read(&mut buffer) {
                Ok(0) => return Ok(true),
                Ok(count) if bytes.len() + count > cap => {
                    return Err(format!("Git output exceeded the {cap}-byte limit"));
                }
                Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(format!("could not read Git output: {error}")),
            }
        }
        Ok(false)
    }

    let budget = within.into();
    let within = budget.remaining()?;
    let mut command = git_command(cwd, args);
    command.process_group(0);
    let mut child = ReadChild(
        command
            .spawn()
            .map_err(|error| format!("could not start Git: {error}"))?,
        false,
    );
    let mut stdout = child
        .0
        .stdout
        .take()
        .ok_or("Git stdout pipe is unavailable")?;
    let mut stderr = child
        .0
        .stderr
        .take()
        .ok_or("Git stderr pipe is unavailable")?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let deadline = budget.deadline();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (mut out_closed, mut err_closed) = (false, false);
    loop {
        if !out_closed {
            out_closed = drain(&mut stdout, &mut out, GIT_OUTPUT_CAP)?;
        }
        if !err_closed {
            err_closed = drain(&mut stderr, &mut err, GIT_DIAGNOSTIC_CAP)?;
        }
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let waited = unsafe {
            libc::waitid(
                libc::P_PID,
                child.0.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if waited < 0 {
            return Err(format!(
                "could not inspect Git's exit: {}",
                std::io::Error::last_os_error()
            ));
        }
        if unsafe { info.si_pid() } != 0 && out_closed && err_closed {
            budget.remaining()?;
            let status = child
                .0
                .wait()
                .map_err(|error| format!("could not reap Git: {error}"))?;
            child.1 = true;
            return Ok(Output {
                status,
                stdout: out,
                stderr: err,
            });
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "Git read timed out after {:.1}s: {}",
                within.as_secs_f64(),
                String::from_utf8_lossy(&err).trim_end()
            ));
        }
        thread::sleep(remaining.min(Duration::from_millis(5)));
    }
}

#[cfg(not(unix))]
pub(super) fn git_output(
    _cwd: &Path,
    _args: &[&str],
    _within: impl Into<GitReadBudget>,
) -> Result<Output, String> {
    Err("native Git discovery is supported on macOS and Linux only".into())
}
/// Mutations have no deadline and are never killed for overflowing diagnostics.
/// Drain both pipes fully, retaining only a bounded diagnostic prefix.
pub(super) fn git_write(root: &Path, args: &[&str]) -> Result<Output, String> {
    fn collect(mut pipe: impl Read, cap: usize) -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            let read = match pipe.read(&mut buffer) {
                Ok(read) => read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            if read == 0 {
                return Ok(bytes);
            }
            let keep = read.min(cap.saturating_sub(bytes.len()));
            bytes.extend_from_slice(&buffer[..keep]);
        }
    }
    let mut child = git_command(root, args)
        .spawn()
        .map_err(|error| format!("could not start Git mutation: {error}"))?;
    let out = child.stdout.take().ok_or("Git stdout unavailable")?;
    let err = child.stderr.take().ok_or("Git stderr unavailable")?;
    std::thread::scope(|scope| {
        let stdout = scope.spawn(move || collect(out, GIT_OUTPUT_CAP));
        let stderr = scope.spawn(move || collect(err, GIT_DIAGNOSTIC_CAP));
        let status = child.wait().map_err(|error| {
            format!("Git mutation exit is unknown; inspect repository before retrying: {error}")
        })?;
        let stdout = stdout
            .join()
            .map_err(|_| "Git mutation stdout reader failed")?
            .map_err(|error| error.to_string())?;
        let stderr = stderr
            .join()
            .map_err(|_| "Git mutation stderr reader failed")?
            .map_err(|error| error.to_string())?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    })
}
