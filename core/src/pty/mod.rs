use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::{
    collections::HashMap,
    ffi::OsString,
    io::{self, Read, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use crate::workspace::{TerminalProcessState, TerminalSessionStatus};

pub mod process;

pub type OutputSink = Box<dyn FnMut(&[u8]) -> Result<(), String> + Send + 'static>;

/// How a session's reader thread stopped taking bytes.
///
/// A child exiting is not this: the shell can be gone while its last output is
/// still being drained, and the reverse is possible too. A consumer that cannot
/// tell the three apart has no way to tell a quiet terminal from a dead reader,
/// and will keep showing a frozen grid as though it were healthy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReaderEnd {
    /// The PTY was closed cleanly. Every byte the child wrote has been delivered.
    EndOfStream,
    /// The PTY could not be read any further. Bytes already delivered stand, but
    /// the reason is a failure and not the end of the session.
    ReadFailed(String),
    /// The reader itself stopped, which means output stopped being delivered while
    /// the child may well still be running.
    ReaderStopped,
}

pub type ReaderEndSink = Box<dyn FnOnce(ReaderEnd) + Send + 'static>;

pub struct SpawnOptions {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub cols: u16,
    pub rows: u16,
    /// Environment overrides for this child only.
    pub env: Vec<(OsString, OsString)>,
    /// Told exactly once, from the reader thread, when that thread stops: end of
    /// stream, a read failure, or an unwinding reader. `None` — what a consumer
    /// that only wants bytes passes — leaves the reader as silent as it was.
    pub reader_end: Option<ReaderEndSink>,
}

/// Reports the reader's fate exactly once, whichever way the thread ends.
///
/// The normal paths take the callback and call it with the real reason. What is
/// left behind runs while the thread unwinds, which is how a panic inside the
/// reader still reaches the owner instead of being indistinguishable from a
/// quiet PTY. Catching the unwind is deliberately not done: the report says the
/// reader stopped, and the cause of a panic stays where it happened.
struct ReaderCompletion(Option<ReaderEndSink>);

impl ReaderCompletion {
    fn finish(&mut self, end: ReaderEnd) {
        if let Some(sink) = self.0.take() {
            sink(end);
        }
    }
}

impl Drop for ReaderCompletion {
    fn drop(&mut self) {
        self.finish(ReaderEnd::ReaderStopped);
    }
}

struct Session {
    master: Mutex<Box<dyn MasterPty + Send>>,
    master_fd: libc::c_int,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<ChildState>,
    process_id: Option<u32>,
    #[cfg(unix)]
    terminal_session_id: Option<u32>,
    closing: AtomicBool,
}

struct ChildState {
    child: Option<Box<dyn Child + Send + Sync>>,
    exit_code: Option<u32>,
    child_released: bool,
}

enum SessionEntry {
    Active(Arc<Session>),
    Closing(Arc<Session>),
    CleanupFailed(Arc<Session>),
}

impl SessionEntry {
    fn session(&self) -> &Arc<Session> {
        match self {
            Self::Active(session) | Self::Closing(session) | Self::CleanupFailed(session) => {
                session
            }
        }
    }
}

#[derive(Default)]
pub struct TerminalBackend {
    sessions: Mutex<HashMap<String, SessionEntry>>,
}

impl TerminalBackend {
    pub fn spawn(
        &self,
        id: String,
        options: SpawnOptions,
        mut output: OutputSink,
    ) -> Result<(), String> {
        if options.cols == 0 || options.rows == 0 {
            return Err("PTY dimensions must be non-zero".into());
        }
        if !options.cwd.is_dir() {
            return Err(format!(
                "PTY working directory is not a directory: {}",
                options.cwd.display()
            ));
        }

        let system = native_pty_system();
        let pair = system
            .openpty(PtySize {
                rows: options.rows,
                cols: options.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| error.to_string())?;
        #[cfg(unix)]
        let master_fd = pair
            .master
            .as_raw_fd()
            .ok_or_else(|| "PTY master descriptor is unavailable".to_string())?;
        #[cfg(unix)]
        set_nonblocking(master_fd)
            .map_err(|error| format!("could not configure PTY input: {error}"))?;
        #[cfg(not(unix))]
        let master_fd = -1;

        let mut command = CommandBuilder::new(options.program);
        command.args(options.args);
        command.cwd(options.cwd);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env("TERM_PROGRAM", "Muster");
        for (key, value) in options.env {
            command.env(key, value);
        }

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| error.to_string())?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| error.to_string())?;
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| error.to_string())?;
        let process_id = child.process_id();
        #[cfg(unix)]
        let terminal_session_id = process_session_id(process_id);
        drop(pair.slave);

        let session = Arc::new(Session {
            master: Mutex::new(pair.master),
            master_fd,
            writer: Mutex::new(writer),
            child: Mutex::new(ChildState {
                child: Some(child),
                exit_code: None,
                child_released: false,
            }),
            process_id,
            #[cfg(unix)]
            terminal_session_id,
            closing: AtomicBool::new(false),
        });
        let reader_session = Arc::clone(&session);
        let reader_fd = session.master_fd;
        let reader_end = options.reader_end;
        if let Err(error) = thread::Builder::new()
            .name("muster-pty-reader".into())
            .spawn(move || {
                let _session = reader_session;
                let mut completion = ReaderCompletion(reader_end);
                read_output(reader, reader_fd, &mut output, &mut completion)
            })
        {
            let _ = terminate_session(&session, &id);
            return Err(error.to_string());
        }
        let inserted = match self.sessions.lock() {
            Ok(mut sessions) if !sessions.contains_key(&id) => {
                sessions.insert(id.clone(), SessionEntry::Active(Arc::clone(&session)));
                Ok(())
            }
            Ok(_) => Err("terminal session already exists".to_string()),
            Err(error) => Err(error.to_string()),
        };
        if let Err(error) = inserted {
            let _ = terminate_session(&session, &id);
            return Err(error);
        }
        Ok(())
    }

    fn session(&self, id: &str) -> Result<Arc<Session>, String> {
        let sessions = self.sessions.lock().map_err(|error| error.to_string())?;
        match sessions.get(id) {
            Some(SessionEntry::Active(session)) => Ok(Arc::clone(session)),
            Some(SessionEntry::Closing(_) | SessionEntry::CleanupFailed(_)) => {
                Err(format!("terminal session {id} is closing"))
            }
            None => Err(format!("unknown terminal session {id}")),
        }
    }

    pub fn write(&self, id: &str, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > MAX_TERMINAL_INPUT_BYTES {
            return Err(format!(
                "terminal input exceeds the {}-byte limit",
                MAX_TERMINAL_INPUT_BYTES
            ));
        }
        let session = self.session(id)?;
        write_to_session(&session, id, bytes)
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(u16, u16), String> {
        if cols == 0 || rows == 0 {
            return Err("PTY dimensions must be non-zero".into());
        }
        let session = self.session(id)?;
        let master = session.master.lock().map_err(|error| error.to_string())?;
        if session.closing.load(Ordering::Acquire) {
            return Err(format!("terminal session {id} is closing"));
        }
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| error.to_string())?;
        let size = master.get_size().map_err(|error| error.to_string())?;
        Ok((size.cols, size.rows))
    }

    pub fn status(&self, id: &str) -> Result<TerminalSessionStatus, String> {
        let session = self.session(id)?;
        let mut process = session.child.lock().map_err(|error| error.to_string())?;
        if session.closing.load(Ordering::Acquire) {
            return Err(format!("terminal session {id} is closing"));
        }
        if process.exit_code.is_none() {
            if let Some(status) = process
                .child
                .as_mut()
                .ok_or_else(|| format!("terminal session {id} is closing"))?
                .try_wait()
                .map_err(|error| error.to_string())?
            {
                process.exit_code = Some(status.exit_code());
            }
        }
        let exit_code = process.exit_code;
        drop(process);
        Ok(match exit_code {
            Some(exit_code) => TerminalSessionStatus {
                state: TerminalProcessState::Exited,
                exit_code: Some(exit_code),
                foreground_process: false,
                foreground_app: None,
                working_directory: None,
            },
            None => TerminalSessionStatus {
                state: TerminalProcessState::Running,
                exit_code: None,
                foreground_process: session.foreground_process(),
                foreground_app: session.foreground_app(),
                working_directory: session.working_directory(),
            },
        })
    }

    pub fn close(&self, id: &str) -> Result<bool, String> {
        self.close_with(id, || {})
    }

    fn close_with(&self, id: &str, on_master_contention: impl FnMut()) -> Result<bool, String> {
        let session = {
            let mut sessions = self.sessions.lock().map_err(|error| error.to_string())?;
            let Some(entry) = sessions.get(id) else {
                return Ok(false);
            };
            let session = match entry {
                SessionEntry::Active(session) | SessionEntry::CleanupFailed(session) => {
                    Arc::clone(session)
                }
                SessionEntry::Closing(_) => {
                    return Err(format!("terminal session {id} is already closing"));
                }
            };
            sessions.insert(id.to_string(), SessionEntry::Closing(Arc::clone(&session)));
            session
        };
        if let Err(error) = terminate_session_with(&session, id, on_master_contention) {
            let mut sessions = self
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if matches!(sessions.get(id), Some(SessionEntry::Closing(current)) if Arc::ptr_eq(current, &session))
            {
                sessions.insert(
                    id.to_string(),
                    SessionEntry::CleanupFailed(Arc::clone(&session)),
                );
            }
            return Err(error);
        }
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(sessions.get(id), Some(SessionEntry::Closing(current)) if Arc::ptr_eq(current, &session))
        {
            sessions.remove(id);
        }
        Ok(true)
    }
}

impl Session {
    fn foreground_process(&self) -> bool {
        self.foreground_group()
            .zip(self.process_id)
            .is_some_and(|(group, shell)| group != shell)
    }

    /// The program the foreground group is running, when that group is not the shell itself.
    ///
    /// A job-control shell gives every job its own group led by the job's first process, so
    /// the group's id *is* that process's pid and the OS can name it. Resolving the group
    /// rather than a fixed pid is what keeps the answer right after a job is replaced.
    fn foreground_app(&self) -> Option<String> {
        let (group, shell) = self.foreground_group().zip(self.process_id)?;
        (group != shell)
            .then(|| process::executable_name(group))
            .flatten()
    }

    /// The shell's own directory, which is the only one that says where this terminal is working.
    ///
    /// Asked of the shell rather than of the foreground group: a program in front changes its own
    /// directory for reasons that have nothing to do with where the terminal is, and a terminal
    /// whose row moved because a build ran with a `-C` flag would be moved by nothing the person
    /// did.
    fn working_directory(&self) -> Option<String> {
        let shell = self.process_id?;
        process::current_directory(shell).map(|directory| directory.to_string_lossy().into_owned())
    }

    fn foreground_group(&self) -> Option<u32> {
        self.master
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .process_group_leader()
            .and_then(|group| u32::try_from(group).ok())
    }
}

impl TerminalBackend {
    /// Stops every session, which is what leaves no PTY behind when the app goes away.
    ///
    /// It is called from the event loop rather than left to `Drop` below, because the loop never
    /// unwinds: `App::run` hands the process to `std::process::exit` once the loop is done, so a
    /// `Drop` in the managed state never runs and a `Drop` alone would leak every shell.
    pub fn shutdown(&self) {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (id, session) in &*sessions {
            if let Err(error) = terminate_session(session.session(), id) {
                log::warn!("terminal {id} did not stop with the app: {error}");
            }
        }
    }
}

impl Drop for TerminalBackend {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub const MAX_TERMINAL_INPUT_BYTES: usize = 1024 * 1024;
const TERMINAL_WRITE_TIMEOUT: Duration = Duration::from_secs(1);
const TERMINATE_GRACE: Duration = Duration::from_millis(200);
const TERMINATE_TIMEOUT: Duration = Duration::from_secs(1);
const REAP_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[cfg(unix)]
fn set_nonblocking(fd: libc::c_int) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn write_pty_input(
    fd: libc::c_int,
    bytes: &[u8],
    closing: &AtomicBool,
    id: &str,
) -> Result<(), String> {
    let deadline = Instant::now() + TERMINAL_WRITE_TIMEOUT;
    let mut written = 0;
    while written < bytes.len() {
        if closing.load(Ordering::Acquire) {
            return Err(format!(
                "terminal {id} input cancelled after writing {written} of {} bytes: session is closing",
                bytes.len()
            ));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "terminal {id} input timed out after writing {written} of {} bytes",
                bytes.len()
            ));
        }
        let result =
            unsafe { libc::write(fd, bytes[written..].as_ptr().cast(), bytes.len() - written) };
        if result > 0 {
            written += result as usize;
            continue;
        }
        if result == 0 {
            return Err(format!(
                "terminal {id} input stopped after writing {written} of {} bytes",
                bytes.len()
            ));
        }

        let error = io::Error::last_os_error();
        match error.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => {
                let mut descriptor = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let timeout_ms = remaining.as_millis().max(1).min(i32::MAX as u128) as i32;
                let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
                if ready == 0 {
                    return Err(format!(
                        "terminal {id} input timed out after writing {written} of {} bytes",
                        bytes.len()
                    ));
                }
                if ready < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(format!(
                        "terminal {id} input failed after writing {written} of {} bytes: {error}",
                        bytes.len()
                    ));
                }
                if descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                    return Err(format!(
                        "terminal {id} PTY stopped accepting input after writing {written} of {} bytes",
                        bytes.len()
                    ));
                }
            }
            _ => {
                return Err(format!(
                    "terminal {id} input failed after writing {written} of {} bytes: {error}",
                    bytes.len()
                ));
            }
        }
    }
    Ok(())
}

fn terminate_session(session: &Session, id: &str) -> Result<(), String> {
    terminate_session_with(session, id, || {})
}

fn terminate_session_with(
    session: &Session,
    id: &str,
    mut on_master_contention: impl FnMut(),
) -> Result<(), String> {
    session.closing.store(true, Ordering::Release);
    let mut process = session
        .child
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if process.exit_code.is_none() {
        if let Some(child) = process.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => process.exit_code = Some(status.exit_code()),
                Ok(None) => {}
                Err(error) => log::warn!("could not inspect terminal {id} before closing: {error}"),
            }
        }
    }

    #[cfg(unix)]
    {
        if let Err(error) = signal_foreground_group(
            session,
            libc::SIGTERM,
            id,
            TERMINATE_GRACE,
            &mut on_master_contention,
        ) {
            log::warn!("terminal {id} foreground TERM deferred: {error}");
        }
        if process.exit_code.is_none() {
            if let Err(error) = signal_shell_group(session, libc::SIGTERM, id) {
                log::warn!("terminal {id} shell TERM failed; child kill remains armed: {error}");
            }
        }
    }
    #[cfg(not(unix))]
    if process.exit_code.is_none() {
        if let Some(child) = process.child.as_mut() {
            if let Err(error) = child.kill() {
                log::warn!("could not stop terminal {id} before escalation: {error}");
            }
        }
    }

    if process.exit_code.is_none() {
        if let Err(error) = wait_for_exit(&mut process, TERMINATE_GRACE) {
            log::warn!("could not inspect terminal {id} during graceful close: {error}");
        }
    }
    #[cfg(unix)]
    let foreground_cleanup_error = signal_foreground_group(
        session,
        libc::SIGKILL,
        id,
        TERMINATE_TIMEOUT,
        &mut on_master_contention,
    )
    .err();
    #[cfg(unix)]
    if process.exit_code.is_none() {
        if let Err(error) = signal_shell_group(session, libc::SIGKILL, id) {
            log::warn!(
                "terminal {id} shell SIGKILL failed; portable child kill remains armed: {error}"
            );
        }
    }

    let child_result = finish_child(&mut process, id);
    #[cfg(unix)]
    if let Some(error) = foreground_cleanup_error {
        return Err(format!(
            "terminal {id} child cleanup: {}; foreground cleanup failed: {error}",
            child_result.err().unwrap_or_else(|| "reaped".into())
        ));
    }
    child_result
}

fn finish_child(process: &mut ChildState, id: &str) -> Result<(), String> {
    if process.child_released {
        return Ok(());
    }
    if process.exit_code.is_none() {
        if let Some(child) = process.child.as_mut() {
            if let Err(error) = child.kill() {
                log::warn!("could not kill terminal {id}: {error}");
            }
        }
        if process.child.is_some() {
            if let Err(error) = wait_for_exit(process, TERMINATE_TIMEOUT) {
                log::warn!("could not inspect terminal {id} after kill: {error}");
            }
        }
    }
    if process.exit_code.is_some() {
        return Ok(());
    }
    match start_child_reaper(process, |reaper| {
        thread::Builder::new()
            .name("muster-pty-reaper".into())
            .spawn(reaper)
            .map(drop)
    }) {
        Ok(()) => Err(format!(
            "terminal {id} did not exit after kill; reaping continues in background"
        )),
        Err(error) => Err(format!(
            "terminal {id} did not exit; reaper could not start: {error}"
        )),
    }
}

fn start_child_reaper(
    process: &mut ChildState,
    spawn: impl FnOnce(Box<dyn FnOnce() + Send + 'static>) -> std::io::Result<()>,
) -> std::io::Result<()> {
    if process.child_released {
        return Ok(());
    }
    let Some(child) = process.child.take() else {
        return Err(std::io::Error::other("terminal child handle is missing"));
    };
    let child = Arc::new(Mutex::new(Some(child)));
    let reaper_child = Arc::clone(&child);
    let reaper = Box::new(move || {
        let mut child = reaper_child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(child) = child.as_mut() {
            if let Err(error) = child.wait() {
                log::warn!("terminal child reaper failed: {error}");
            }
        }
    });
    match spawn(reaper) {
        Ok(()) => {
            process.child_released = true;
            Ok(())
        }
        Err(error) => {
            process.child = child
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            Err(error)
        }
    }
}

fn wait_for_exit(process: &mut ChildState, timeout: Duration) -> Result<bool, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if process.exit_code.is_some() {
            return Ok(true);
        }
        let child = process
            .child
            .as_mut()
            .ok_or_else(|| "terminal child handle was transferred to its reaper".to_string())?;
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            process.exit_code = Some(status.exit_code());
            return Ok(true);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        thread::sleep(remaining.min(REAP_POLL_INTERVAL));
    }
}

#[cfg(unix)]
fn signal_foreground_group(
    session: &Session,
    signal: libc::c_int,
    id: &str,
    timeout: Duration,
    on_contention: &mut impl FnMut(),
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let master = loop {
        match session.master.try_lock() {
            Ok(master) => break master,
            Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => on_contention(),
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("could not reacquire the PTY master".into());
        }
        thread::sleep(remaining.min(REAP_POLL_INTERVAL));
    };
    let Some(group) = master
        .process_group_leader()
        .and_then(|group| u32::try_from(group).ok())
        .filter(|group| *group > 0)
    else {
        return Ok(());
    };
    let expected_session = session
        .terminal_session_id
        .and_then(|sid| libc::pid_t::try_from(sid).ok())
        .ok_or_else(|| format!("terminal {id} session could not be verified"))?;
    let fd = master
        .as_raw_fd()
        .ok_or_else(|| format!("terminal {id} PTY descriptor is unavailable"))?;
    // tcgetpgrp's result belongs to the PTY's controlling session; tcgetsid verifies that
    // session without requiring the foreground process-group leader to still exist.
    let actual_session = unsafe { libc::tcgetsid(fd) };
    if actual_session < 0 {
        return Err(format!(
            "could not verify terminal {id} PTY session: {}",
            std::io::Error::last_os_error()
        ));
    }
    let outcome = signal_group_with(
        Some(group),
        u32::try_from(actual_session).ok(),
        signal,
        |_, sid| sid == expected_session as u32,
        |group, signal| {
            if unsafe { libc::kill(group, signal) } == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        },
    );
    if outcome == GroupSignal::Unverified && process_group_exists(group) {
        return Err(format!(
            "terminal {id} foreground group is not verified in its original PTY session"
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn signal_shell_group(session: &Session, signal: libc::c_int, id: &str) -> Result<(), String> {
    signal_process_group(session.process_id, session.terminal_session_id, signal, id)?;
    signal_process(session.process_id, session.terminal_session_id, signal, id)
}

#[cfg(unix)]
fn process_session_id(pid: Option<u32>) -> Option<u32> {
    let pid = libc::pid_t::try_from(pid?).ok()?;
    let session_id = unsafe { libc::getsid(pid) };
    (session_id > 0)
        .then(|| u32::try_from(session_id).ok())
        .flatten()
}

#[cfg(unix)]
fn process_group_in_session(group: u32, session_id: u32) -> bool {
    let (Ok(group), Ok(session_id)) = (
        libc::pid_t::try_from(group),
        libc::pid_t::try_from(session_id),
    ) else {
        return false;
    };
    group > 0
        && session_id > 0
        && unsafe { libc::getpgid(group) == group && libc::getsid(group) == session_id }
}

#[cfg(unix)]
fn process_group_exists(group: u32) -> bool {
    let Ok(group) = libc::pid_t::try_from(group) else {
        return false;
    };
    if group <= 0 || unsafe { libc::kill(-group, 0) } == 0 {
        return group > 0;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Whether a refusal to signal a group is that platform's way of saying the group is over.
///
/// Darwin's `kill` reports the group it could signal nothing in, and a process that has already
/// exited takes no more signals, so a group made of zombies answers EPERM there -- the same news
/// ESRCH carries. Everywhere else it is the opposite: a signal aimed at a zombie is delivered and
/// then dropped, so a group of zombies is a group the signal went through and an EPERM is a member
/// the caller has no right to touch. Belonging to a session says nothing about the credentials of
/// everything in it -- a privileged process that inherited this session's group is such a member,
/// and it is very much alive -- so the refusal is only an answer about liveness where the kernel
/// makes it one.
#[cfg(all(unix, target_os = "macos"))]
const REFUSAL_IS_AN_EMPTY_GROUP: bool = true;
#[cfg(all(unix, not(target_os = "macos")))]
const REFUSAL_IS_AN_EMPTY_GROUP: bool = false;

/// What a signal aimed at a process group achieved.
///
/// `Gone` and `Unverified` both mean no signal was sent, and they are not the same news: the
/// first is a group with nothing left in it, the second is a group nobody could show belongs to
/// this session, or one that refused the signal in a way that is not an answer about liveness.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum GroupSignal {
    Delivered,
    Gone,
    Unverified,
}

#[cfg(unix)]
fn signal_group_with(
    group: Option<u32>,
    session_id: Option<u32>,
    signal: libc::c_int,
    belongs_to_session: impl FnOnce(u32, u32) -> bool,
    send: impl FnOnce(libc::pid_t, libc::c_int) -> Result<(), std::io::Error>,
) -> GroupSignal {
    let (Some(group), Some(session_id)) = (group, session_id) else {
        return if group.is_some() {
            // A group whose session is unknown is a group nobody could check.
            GroupSignal::Unverified
        } else {
            GroupSignal::Gone
        };
    };
    if group == 0 || !belongs_to_session(group, session_id) {
        return GroupSignal::Unverified;
    }
    let Ok(group) = libc::pid_t::try_from(group) else {
        return GroupSignal::Gone;
    };
    match send(-group, signal) {
        Ok(()) => GroupSignal::Delivered,
        // ESRCH is a group that is not there. An EPERM is that same news where the kernel makes it
        // one -- Darwin, and only Darwin, and `REFUSAL_IS_AN_EMPTY_GROUP` is where that is kept.
        // Anywhere else a refusal is not an answer about liveness: the group is there with a
        // member that could not be signalled, which is a group somebody is alive in, so it stays
        // loud and the caller keeps an error to retry with. Any other errno is not an answer about
        // liveness either, and stays loud.
        //
        // Membership verification and `kill` are separate syscalls. A foreground group can exit
        // between them and its numeric ID can be reused; the kernel's permission check is about
        // credentials, not session membership, so it does not protect a same-user replacement.
        // This is therefore a best-effort check immediately before signalling, not an atomic proof
        // of the target at delivery time. The owned shell child is held unreaped by the caller, but
        // that does not pin an arbitrary foreground process-group ID.
        Err(error)
            if error.raw_os_error() == Some(libc::ESRCH)
                || (REFUSAL_IS_AN_EMPTY_GROUP && error.raw_os_error() == Some(libc::EPERM)) =>
        {
            GroupSignal::Gone
        }
        Err(_) => GroupSignal::Unverified,
    }
}

#[cfg(unix)]
fn signal_process_group(
    group: Option<u32>,
    session_id: Option<u32>,
    signal: libc::c_int,
    id: &str,
) -> Result<(), String> {
    let Some(group) = group else {
        return Ok(());
    };
    let outcome = signal_group_with(
        Some(group),
        session_id,
        signal,
        process_group_in_session,
        |group, signal| {
            if unsafe { libc::kill(group, signal) } == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        },
    );
    if outcome == GroupSignal::Unverified && process_group_exists(group) {
        return Err(format!(
            "terminal {id} process group could not be verified in its original session"
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn signal_process(
    pid: Option<u32>,
    session_id: Option<u32>,
    signal: libc::c_int,
    id: &str,
) -> Result<(), String> {
    let Some(pid) = pid.and_then(|pid| libc::pid_t::try_from(pid).ok()) else {
        return Ok(());
    };
    let Some(session_id) = session_id.and_then(|id| libc::pid_t::try_from(id).ok()) else {
        return Err(format!("terminal {id} shell session could not be verified"));
    };
    let actual_session = unsafe { libc::getsid(pid) };
    if actual_session < 0 {
        let error = std::io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(format!(
                "could not verify terminal {id} shell session: {error}"
            ))
        };
    }
    if actual_session != session_id {
        return Err(format!(
            "terminal {id} shell PID no longer belongs to its session"
        ));
    }
    if unsafe { libc::kill(pid, signal) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(format!("could not signal terminal {id} shell: {error}"))
    }
}

#[cfg(unix)]
fn wait_pty_readable(fd: libc::c_int) -> io::Result<()> {
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let ready = unsafe { libc::poll(&mut descriptor, 1, -1) };
        if ready > 0 {
            if descriptor.revents & libc::POLLNVAL != 0 {
                return Err(io::Error::other("PTY reader descriptor became invalid"));
            }
            if descriptor.revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                return Ok(());
            }
            if descriptor.revents & libc::POLLERR != 0 {
                return Err(io::Error::other("PTY reader poll reported an error"));
            }
        }
        let error = io::Error::last_os_error();
        if ready < 0 && error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(error);
    }
}

fn read_output(
    mut reader: Box<dyn Read + Send>,
    master_fd: libc::c_int,
    output: &mut OutputSink,
    completion: &mut ReaderCompletion,
) {
    let mut buffer = [0_u8; 64 * 1024];
    // Said once. A window that is gone refuses every chunk from here to the end of the session,
    // and a line per chunk would bury everything else the log is for.
    let mut reported = false;
    loop {
        #[cfg(unix)]
        if let Err(error) = wait_pty_readable(master_fd) {
            log::warn!("terminal PTY reader poll failed: {error}");
            completion.finish(ReaderEnd::ReadFailed(error.to_string()));
            break;
        }
        #[cfg(not(unix))]
        let _ = master_fd;

        match reader.read(&mut buffer) {
            Ok(0) => {
                completion.finish(ReaderEnd::EndOfStream);
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(error) => {
                completion.finish(ReaderEnd::ReadFailed(error.to_string()));
                break;
            }
            Ok(count) => {
                // A disconnected renderer must not stop draining the PTY and deadlock its child.
                if let Err(error) = output(&buffer[..count]) {
                    if !reported {
                        reported = true;
                        log::warn!(
                            "terminal output stopped being delivered, dropping the rest: {error}"
                        );
                    }
                }
            }
        }
    }
}

/// What the window is allowed to owe the terminal before the reader stops taking more.
///
/// The window between the PTY and the renderer is a queue, and a real terminal bounds it in the
/// kernel: a process that writes faster than the screen can draw blocks in `write` instead of
/// filling the machine's memory. A reader here cannot make the renderer's parser slower, so
/// without a bound the queue is the whole burst — a `cat` of a large file, a `yes`, a TUI
/// redrawing — and the app pays for it in memory and in a keyboard that stops answering.
pub const OUTPUT_HIGH_WATER_BYTES: usize = 2 * 1024 * 1024;

/// Where the gate opens again, well under the mark that closed it.
///
/// Two marks and not one. A single mark is either too eager, in which case the reader and the
/// window take turns for the rest of the session and the terminal is slower than no gate at all,
/// or too late, in which case nothing is bounded.
pub const OUTPUT_LOW_WATER_BYTES: usize = 512 * 1024;

const _: () = assert!(OUTPUT_LOW_WATER_BYTES < OUTPUT_HIGH_WATER_BYTES);

/// How long a hold waits for an answer that never arrives.
///
/// Failing open, on purpose. A window that stops answering must not be able to stop a terminal:
/// the gate is an optimisation and a lost answer is not a reason to lose output, so the hold ends
/// by itself, the debt it was counting is cleared, and the next answer starts counting again. A
/// window that never answers costs the unbounded queue this gate was added for — the behaviour
/// from before it, which is a floor and not a worse one.
pub const OUTPUT_RESUME_TIMEOUT: Duration = Duration::from_secs(5);

/// The reader's half of the flow control with the window that draws its output.
///
/// xterm parses asynchronously and does not say how far through its own write buffer it is, so
/// the window reports how much of its output it has parsed and this holds the reader while the
/// difference between that and what it has sent is over the high water mark. Nothing is dropped
/// here: a held chunk is a chunk the PTY has not been drained for, so the process behind it
/// blocks in `write` exactly as it would behind a full terminal.
///
/// Both halves are cumulative totals rather than a running debt, because the two sides are never
/// looking at the same set of bytes: the reader counts a chunk the moment it hands it over, and
/// the window only counts what has arrived. A difference of totals is the one figure that
/// includes the bytes still travelling, and totals also make an answer that is late or repeated
/// harmless instead of a rewind — it can only ever move the count forwards.
///
/// The gate is held inside the output sink rather than around the read, which costs at most one
/// buffer of overrun past the mark. That is what keeps the reader loop above untouched, and with
/// it the two things it guarantees: a disconnected renderer still drains the PTY to the end of
/// the session, and it is still said once.
pub struct OutputGate {
    state: Mutex<GateState>,
    resumed: Condvar,
    resume_timeout: Duration,
}

#[derive(Default)]
struct GateState {
    /// Bytes on their way to the window, counted as they are handed over and never rewound.
    delivered: usize,
    /// How much of those the window has said it has parsed, counted the same way.
    parsed: usize,
    /// Whether this gate has already closed, because where it opens again is read against the low
    /// mark rather than the high one and the debt alone cannot tell the two situations apart.
    paused: bool,
    /// Set when nothing is going to answer any more, so this is no longer a gate.
    released: bool,
}

impl GateState {
    /// What the window still owes, which is not what it has not received: the bytes on their way to
    /// it are owed too, and leaving them out of the count is what would let the reader outrun the
    /// window by whatever it had in flight every time the window answered.
    fn debt(&self) -> usize {
        self.delivered.saturating_sub(self.parsed)
    }
}

impl OutputGate {
    pub fn new(resume_timeout: Duration) -> Self {
        Self {
            state: Mutex::new(GateState::default()),
            resumed: Condvar::new(),
            resume_timeout,
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Counts bytes on their way to the window, which is what the window's own total is measured
    /// against.
    ///
    /// Called before the hand-off rather than after it, and that order is the whole point: a window
    /// can be parsing a chunk and answering about it before the send carrying it has returned, and
    /// a total that arrives ahead of its own bytes would clamp itself to a count those bytes are
    /// then counted outside of. A send that fails counts bytes nobody will ever parse, which is why
    /// the failure path releases the gate instead of unwinding this one.
    pub fn delivered(&self, bytes: usize) {
        let mut state = self.state();
        if state.released {
            return;
        }
        state.delivered = state.delivered.saturating_add(bytes);
    }

    /// Takes the window's total of what it has parsed as the truth.
    ///
    /// Never backwards and never past what has been delivered. A report that is late, or the same
    /// report twice because two chunks crossed in the channel, leaves the reader counting the same
    /// debt it was counting a moment ago instead of forgetting the bytes it is still waiting on.
    pub fn acknowledge(&self, parsed: usize) {
        let mut state = self.state();
        if state.released {
            return;
        }
        state.parsed = state.parsed.max(parsed).min(state.delivered);
        drop(state);
        self.resumed.notify_all();
    }

    /// Stops being a gate for good, because there is nothing left that could open it.
    pub fn release(&self) {
        let mut state = self.state();
        state.released = true;
        drop(state);
        self.resumed.notify_all();
    }

    /// Waits while the window is too far behind to be handed another chunk.
    ///
    /// The two marks are read from opposite ends of the decision, and comparing against only one of
    /// them is either no bound at all or a traffic jam: it takes the high mark to close, and once
    /// closed it stays closed until the window is back under the low one. `paused` is what tells
    /// the two apart, because the debt cannot — a window that comes back down into the middle of
    /// the band has to be let through it.
    pub fn hold(&self) {
        let mut state = self.state();
        loop {
            if state.released {
                return;
            }
            let debt = state.debt();
            if state.paused {
                if debt <= OUTPUT_LOW_WATER_BYTES {
                    state.paused = false;
                    return;
                }
            } else if debt < OUTPUT_HIGH_WATER_BYTES {
                return;
            } else {
                state.paused = true;
            }
            let (after, timeout) = self
                .resumed
                .wait_timeout(state, self.resume_timeout)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = after;
            // Nobody answered for the whole wait, so the last count is the one number here that
            // nobody can vouch for. Reading is the safe direction: the debt goes, the gate reopens,
            // and the queue starts over from a count that is known rather than from one that is
            // inherited. The window's total moves up to what it was given rather than back to
            // zero, so bytes counted after this wait are still measured from here and an answer
            // that arrives for them counts where this one left off.
            if timeout.timed_out() {
                state.parsed = state.delivered;
                state.paused = false;
                return;
            }
        }
    }
}

/// The one way bytes reach a shell, so `closing` is checked once and everything that writes goes
/// through the same refusal.
fn write_to_session(session: &Session, id: &str, bytes: &[u8]) -> Result<(), String> {
    let writer = match session.writer.try_lock() {
        Ok(writer) => writer,
        Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => {
            return Err(format!(
                "terminal session {id} already has input being written"
            ));
        }
    };
    if session.closing.load(Ordering::Acquire) {
        return Err(format!("terminal session {id} is closing"));
    }
    #[cfg(unix)]
    {
        let _writer = writer;
        write_pty_input(session.master_fd, bytes, &session.closing, id)
    }
    #[cfg(not(unix))]
    {
        let mut writer = writer;
        writer.write_all(bytes).map_err(|error| error.to_string())?;
        writer.flush().map_err(|error| error.to_string())
    }
}

/// Helpers that exist for tests, in a build that is not a test.
///
/// Not `#[cfg(test)]`, and that is the whole reason this module exists rather
/// than a re-export out of the test module below: the gate applies to the crate
/// being tested, never to a crate that depends on it, so a caller whose tests
/// call `wait_for_output` would find nothing there. Public because that caller
/// is a different crate. Hidden from the docs because it is not an API.
#[doc(hidden)]
pub mod test_support {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    pub fn wait_for_output(receiver: &mpsc::Receiver<Vec<u8>>, marker: &[u8], timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut actual = Vec::new();
        while !actual.windows(marker.len()).any(|window| window == marker) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "terminal output marker timed out");
            match receiver.recv_timeout(remaining) {
                Ok(bytes) => actual.extend(bytes),
                Err(error) => panic!("terminal output marker timed out: {error}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::wait_for_output;
    use std::{
        io,
        path::PathBuf,
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc, Arc, Mutex,
        },
        thread,
        time::{Duration, Instant},
    };

    use portable_pty::{Child, ChildKiller, ExitStatus};
    use tempfile::tempdir;

    #[cfg(unix)]
    use std::os::unix::process::CommandExt;

    use super::process::wait_status_pid;
    #[cfg(unix)]
    use super::{process_group_exists, signal_group_with, GroupSignal, REAP_POLL_INTERVAL};
    use super::{
        start_child_reaper, ChildState, OutputGate, OutputSink, ReaderEnd, SpawnOptions,
        TerminalBackend, MAX_TERMINAL_INPUT_BYTES, OUTPUT_HIGH_WATER_BYTES, OUTPUT_LOW_WATER_BYTES,
        OUTPUT_RESUME_TIMEOUT,
    };

    /// A reader end that reports once, so a test can tell "told me" from "told me twice".
    fn reader_end_sink() -> (super::ReaderEndSink, mpsc::Receiver<ReaderEnd>) {
        let (sender, receiver) = mpsc::channel();
        (
            Box::new(move |end| {
                let _ = sender.send(end);
            }),
            receiver,
        )
    }

    fn spawn(
        backend: &TerminalBackend,
        id: &str,
        program: &str,
        args: &[&str],
        output: OutputSink,
    ) {
        backend
            .spawn(
                id.to_string(),
                SpawnOptions {
                    program: PathBuf::from(program),
                    args: args.iter().map(|arg| (*arg).to_string()).collect(),
                    cwd: std::env::current_dir().unwrap(),
                    cols: 80,
                    rows: 24,
                    reader_end: None,
                    env: vec![],
                },
                output,
            )
            .unwrap();
    }

    fn sink() -> (OutputSink, mpsc::Receiver<Vec<u8>>) {
        let (sender, receiver) = mpsc::channel();
        (
            Box::new(move |bytes| sender.send(bytes.to_vec()).map_err(|e| e.to_string())),
            receiver,
        )
    }

    // Shared with `services::terminal`, whose shell-integration test drives a real shell through the
    // same two steps: get it to a prompt, then look for a marker in what it printed.
    pub(crate) fn synchronize_shell(
        backend: &TerminalBackend,
        receiver: &mpsc::Receiver<Vec<u8>>,
        id: &str,
    ) {
        backend
            .write(id, b"printf '\\036MUSTER_READY\\037\\n'\n")
            .unwrap();
        wait_for_output(receiver, b"\x1eMUSTER_READY\x1f", Duration::from_secs(5));
    }

    /// Waits until the write of a session is in flight, which is the state the caller closes in.
    ///
    /// This sleeps between attempts rather than spinning, and that is the whole fix. `write` takes
    /// this same lock with `try_lock` and answers an error instead of waiting for it, so a loop that
    /// keeps taking the lock is taking it from the write it is waiting for: the write is refused
    /// before it ever starts, and a wait for a write that will not begin runs out its deadline
    /// saying it never began. Sleeping hands the lock back between attempts, which is what lets the
    /// write take it and hold it for as long as `TERMINAL_WRITE_TIMEOUT` while this looks on.
    ///
    /// The deadline is longer than the write's own for the same reason: the write has to be scheduled
    /// before it can hold anything, and how long that takes is the machine's answer, not this test's.
    fn wait_for_input_writer(session: &super::Session) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match session.writer.try_lock() {
                Ok(writer) => drop(writer),
                Err(std::sync::TryLockError::WouldBlock) => return,
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    panic!("terminal writer lock poisoned")
                }
            }
            assert!(
                Instant::now() < deadline,
                "PTY input did not enter the writer"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn preserves_binary_output_and_reports_exit_code() {
        let backend = TerminalBackend::default();
        let (output, receiver) = sink();
        spawn(
            &backend,
            "binary",
            "/bin/sh",
            &["-c", r"printf '\303\251\033[31mred\033[0m'; exit 7"],
            output,
        );
        let mut actual = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !actual.windows(3).any(|window| window == b"red") {
            actual.extend(
                receiver
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .unwrap(),
            );
        }
        assert!(actual.starts_with("é\x1b[31m".as_bytes()));
        assert!(actual.ends_with(b"\x1b[0m"));

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(status) = backend.status("binary") {
                if status.state == TerminalProcessState::Exited {
                    assert_eq!(status.exit_code, Some(7));
                    break;
                }
            }
            assert!(Instant::now() < deadline, "shell exit status timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// A shell that ends is the ordinary way a terminal session is used up — `exit`, `:q`, a
    /// logout — and the app that hosted it has to still be there afterwards. That is the invariant
    /// this pins: the process that spawned the shell outlives it, the ending is *reported* rather
    /// than the session quietly vanishing, and the next session in the same backend works. A
    /// backend that mistook its shell's exit for its own would take this test process down with
    /// it, so the assertions after the `exit` are the ones that carry the weight.
    #[cfg(unix)]
    #[test]
    fn a_shell_that_exits_leaves_the_backend_running_and_the_next_session_usable() {
        let backend = TerminalBackend::default();
        let (output, _receiver) = sink();
        spawn(&backend, "exiting", "/bin/sh", &["-i"], output);

        backend.write("exiting", b"exit\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = backend.status("exiting").unwrap();
            if status.state == TerminalProcessState::Exited {
                assert_eq!(status.exit_code, Some(0));
                break;
            }
            assert!(Instant::now() < deadline, "shell exit status timed out");
            thread::sleep(Duration::from_millis(10));
        }
        // Still a session to close, which is what the panel does when it reads that exit.
        assert!(backend.close("exiting").unwrap());

        let (output, echoed) = sink();
        spawn(&backend, "next", "/bin/cat", &[], output);
        backend.write("next", b"still here\n").unwrap();
        let written = echoed.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(written.windows(10).any(|window| window == b"still here"));
        assert_eq!(
            backend.status("next").unwrap().state,
            TerminalProcessState::Running
        );
        backend.close("next").unwrap();
    }

    #[test]
    fn shell_receives_the_requested_working_directory_and_terminal_environment() {
        let backend = TerminalBackend::default();
        let directory = tempdir().unwrap();
        let (output, receiver) = sink();
        backend
            .spawn(
                "environment".into(),
                SpawnOptions {
                    program: PathBuf::from("/bin/sh"),
                    args: vec![
                        "-c".into(),
                        r#"pwd; printf '%s\n' "$TERM" "$COLORTERM""#.into(),
                    ],
                    cwd: directory.path().to_path_buf(),
                    cols: 80,
                    rows: 24,
                    reader_end: None,
                    env: vec![],
                },
                output,
            )
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut actual = Vec::new();
        loop {
            match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(bytes) => actual.extend(bytes),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(error) => panic!("shell output timed out: {error}"),
            }
        }
        let output = String::from_utf8_lossy(&actual);
        assert!(output.contains(
            &directory
                .path()
                .canonicalize()
                .unwrap()
                .display()
                .to_string()
        ));
        assert!(output.contains("xterm-256color"));
        assert!(output.contains("truecolor"));
        backend.close("environment").unwrap();
    }

    #[test]
    fn sessions_have_independent_input_and_dimensions_and_close_running_processes() {
        let backend = TerminalBackend::default();
        let (first_sink, first_output) = sink();
        let (second_sink, second_output) = sink();
        spawn(&backend, "first", "/bin/cat", &[], first_sink);
        spawn(&backend, "second", "/bin/cat", &[], second_sink);

        let initial_size = backend.sessions.lock().unwrap()["first"]
            .session()
            .master
            .lock()
            .unwrap()
            .get_size()
            .unwrap();
        assert_eq!((initial_size.cols, initial_size.rows), (80, 24));
        assert_eq!(backend.resize("first", 91, 31).unwrap(), (91, 31));
        backend.write("first", b"one\n").unwrap();
        backend.write("second", b"two\n").unwrap();
        let first = first_output.recv_timeout(Duration::from_secs(3)).unwrap();
        let second = second_output.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(first.windows(3).any(|window| window == b"one"));
        assert!(second.windows(3).any(|window| window == b"two"));
        assert_eq!(
            backend.status("first").unwrap().state,
            TerminalProcessState::Running
        );
        assert!(backend.close("first").unwrap());
        assert!(backend.close("second").unwrap());
        assert!(!backend.close("second").unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn nonreading_process_times_out_input_and_other_sessions_remain_responsive() {
        let backend = Arc::new(TerminalBackend::default());
        let (blocked_output, blocked_receiver) = sink();
        let (other_output, other_receiver) = sink();
        spawn(&backend, "blocked", "/bin/sh", &["-i"], blocked_output);
        spawn(&backend, "other", "/bin/cat", &[], other_output);
        backend
            .write(
                "blocked",
                b"stty -echo -icanon; printf '\\036MUSTER_BLOCKED\\037\\n'; sleep 30\n",
            )
            .unwrap();
        wait_for_output(
            &blocked_receiver,
            b"\x1eMUSTER_BLOCKED\x1f",
            Duration::from_secs(5),
        );

        let (write_done, write_result) = mpsc::channel();
        let write_backend = Arc::clone(&backend);
        let write_thread = thread::spawn(move || {
            let result = write_backend.write("blocked", &vec![b'x'; MAX_TERMINAL_INPUT_BYTES]);
            let _ = write_done.send(result);
        });
        backend.write("other", b"still responsive\n").unwrap();
        wait_for_output(&other_receiver, b"still responsive", Duration::from_secs(2));
        let error = write_result
            .recv_timeout(Duration::from_secs(3))
            .expect("PTY input write exceeded its deadline")
            .unwrap_err();
        assert!(error.contains("input timed out"), "{error}");
        assert!(error.contains("of 1048576 bytes"), "{error}");
        let written = error
            .strip_prefix("terminal blocked input timed out after writing ")
            .and_then(|details| details.split_once(" of ").map(|(written, _)| written))
            .and_then(|written| written.parse::<usize>().ok())
            .expect("timeout reports how many input bytes were accepted");
        assert!(written > 0 && written < MAX_TERMINAL_INPUT_BYTES);
        write_thread.join().unwrap();

        assert!(backend.close("blocked").unwrap());
        assert!(backend.close("other").unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn close_does_not_wait_for_an_inflight_nonblocking_write() {
        let backend = Arc::new(TerminalBackend::default());
        let (output, receiver) = sink();
        spawn(&backend, "closing-write", "/bin/sh", &["-i"], output);
        backend
            .write(
                "closing-write",
                b"stty -echo -icanon; printf '\\036MUSTER_CLOSING\\037\\n'; sleep 30\n",
            )
            .unwrap();
        wait_for_output(&receiver, b"\x1eMUSTER_CLOSING\x1f", Duration::from_secs(5));
        let session = backend.sessions.lock().unwrap()["closing-write"]
            .session()
            .clone();

        let (write_done, write_result) = mpsc::channel();
        let write_backend = Arc::clone(&backend);
        // Retried while this test's own wait is holding the lock, because the refusal that says so is
        // the wait's doing and not the terminal's: losing that race once is not a fact about a write
        // that is still to come. Only that refusal is retried; any other is the answer under test.
        let write_thread = thread::spawn(move || {
            let mut result =
                write_backend.write("closing-write", &vec![b'x'; MAX_TERMINAL_INPUT_BYTES]);
            while let Err(error) = &result {
                if !error.contains("already has input being written") {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
                result =
                    write_backend.write("closing-write", &vec![b'x'; MAX_TERMINAL_INPUT_BYTES]);
            }
            let _ = write_done.send(result);
        });
        wait_for_input_writer(&session);
        assert!(matches!(
            write_result.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        let close_started = Instant::now();
        assert!(backend.close("closing-write").unwrap());
        assert!(close_started.elapsed() < Duration::from_secs(2));
        let error = write_result
            .recv_timeout(Duration::from_secs(2))
            .expect("close did not release the active PTY write")
            .unwrap_err();
        assert!(error.contains("terminal closing-write"), "{error}");
        write_thread.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn close_retries_foreground_lookup_after_transient_master_contention() {
        let backend = Arc::new(TerminalBackend::default());
        let (output, receiver) = sink();
        spawn(&backend, "contended", "/bin/sh", &["-i"], output);
        synchronize_shell(&backend, &receiver, "contended");
        backend.write("contended", b"sleep 30\n").unwrap();

        let deadline = Instant::now() + Duration::from_secs(3);
        let (session, group) = loop {
            let status = backend.status("contended").unwrap();
            if status.foreground_app.as_deref() == Some("sleep") {
                let session = backend.sessions.lock().unwrap()["contended"]
                    .session()
                    .clone();
                let group = session
                    .master
                    .lock()
                    .unwrap()
                    .process_group_leader()
                    .unwrap();
                break (session, group);
            }
            assert!(
                Instant::now() < deadline,
                "foreground sleep was not observed"
            );
            thread::sleep(Duration::from_millis(10));
        };
        let master_guard = session.master.lock().unwrap();
        let (contention_tx, contention_rx) = mpsc::channel();
        let (close_tx, close_rx) = mpsc::channel();
        let close_backend = Arc::clone(&backend);
        let close_thread = thread::spawn(move || {
            let mut sent_contention = Some(contention_tx);
            let _ = close_tx.send(close_backend.close_with("contended", || {
                if let Some(sender) = sent_contention.take() {
                    let _ = sender.send(());
                }
            }));
        });
        contention_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("close did not observe the held master lock");
        drop(master_guard);
        assert!(close_rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap());
        close_thread.join().unwrap();

        assert_ne!(unsafe { libc::kill(-group, 0) }, 0);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[cfg(unix)]
    #[test]
    fn process_group_signals_reject_gone_or_reused_foreign_groups() {
        let mut signaled = Vec::new();
        let first = signal_group_with(
            Some(123),
            Some(77),
            libc::SIGTERM,
            |group, session_id| group == 123 && session_id == 77,
            |target, _| {
                signaled.push(target);
                Ok(())
            },
        );
        let escalation = signal_group_with(
            Some(456),
            Some(77),
            libc::SIGKILL,
            |_, _| false,
            |target, _| {
                signaled.push(target);
                Ok(())
            },
        );
        let foreign_session = signal_group_with(
            Some(123),
            Some(78),
            libc::SIGKILL,
            |_, session_id| session_id == 77,
            |target, _| {
                signaled.push(target);
                Ok(())
            },
        );
        let gone = signal_group_with(
            Some(123),
            Some(77),
            libc::SIGKILL,
            |_, _| true,
            |_, _| Err(io::Error::from_raw_os_error(libc::ESRCH)),
        );

        assert_eq!(first, GroupSignal::Delivered);
        assert_eq!(escalation, GroupSignal::Unverified);
        assert_eq!(foreign_session, GroupSignal::Unverified);
        assert_eq!(gone, GroupSignal::Gone);
        assert_eq!(signaled, vec![-123]);
    }

    #[cfg(unix)]
    #[test]
    fn a_refusal_that_is_not_an_answer_about_liveness_stays_unverified() {
        // A refusal is an answer about liveness on Darwin, where a group of processes that have
        // already exited answers EPERM, and nowhere else.
        let refused_group = signal_group_with(
            Some(123),
            Some(77),
            libc::SIGKILL,
            |_, _| true,
            |_, _| Err(io::Error::from_raw_os_error(libc::EPERM)),
        );
        // Nothing else is. A refusal nobody can read as liveness keeps the caller's voice.
        let refused = signal_group_with(
            Some(123),
            Some(77),
            libc::SIGTERM,
            |_, _| true,
            |_, _| Err(io::Error::from_raw_os_error(libc::EINVAL)),
        );
        // A group with no session to check it against is a group nobody checked.
        let unverifiable = signal_group_with(
            Some(123),
            None,
            libc::SIGTERM,
            |_, _| panic!("a group with no session must not be signalled"),
            |_, _| Ok(()),
        );
        // A group with no id is nothing to report.
        let nothing = signal_group_with(
            None,
            Some(77),
            libc::SIGTERM,
            |_, _| panic!("a group with no id must not be signalled"),
            |_, _| Ok(()),
        );

        #[cfg(target_os = "macos")]
        assert_eq!(refused_group, GroupSignal::Gone);
        // A privileged member this process cannot signal is not a group that is over, and calling
        // it one is what let a process that was never reached be reported as dead.
        #[cfg(not(target_os = "macos"))]
        assert_eq!(refused_group, GroupSignal::Unverified);
        assert_eq!(refused, GroupSignal::Unverified);
        assert_eq!(unverifiable, GroupSignal::Unverified);
        assert_eq!(nothing, GroupSignal::Gone);
    }

    #[cfg(unix)]
    #[test]
    fn a_group_of_exited_processes_is_a_group_that_is_over() {
        let mut child = unsafe {
            std::process::Command::new("/bin/sh")
                .args(["-c", "exit 0"])
                .pre_exec(|| {
                    // setsid and nothing else: a session leader is already the leader of a group
                    // of its own, so its group id is its own pid and nothing else joins that group
                    // by accident.
                    if libc::setsid() < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                })
                .spawn()
        }
        .unwrap();
        let group = child.id();
        let send = |target, signal| {
            if unsafe { libc::kill(target, signal) } == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        };
        let outcome =
            |signal| signal_group_with(Some(group), Some(group), signal, |_, _| true, send);

        // Alive: the group takes the signal, and the cheap group probe says it is there.
        assert_eq!(outcome(0), GroupSignal::Delivered);
        assert!(process_group_exists(group));

        // Exited and not reaped, which is what a shell killed a moment before the escalation is:
        // its group is a zombie, and what the kernel answers for one is the platform's own --
        // macOS answers EPERM to anything aimed at that group, Linux signals the zombie and drops
        // it. Either way the group has nothing left in it to signal.
        // waitid with WNOWAIT reports the exit without reaping, which is what leaves the child as
        // a zombie for the assertions below. waitpid has no WNOWAIT on macOS. Zeroed rather than
        // uninitialized because a kernel with nothing to report leaves the structure as it found
        // it, and this reads it either way.
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        let deadline = Instant::now() + Duration::from_secs(5);
        while unsafe {
            libc::waitid(
                libc::P_PID,
                group as libc::id_t,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOWAIT,
            )
        } != 0
            // SAFETY: `waitid` wrote the report into `info`, or wrote nothing and left the
            // structure zeroed, which is the same initialised structure either way.
            || wait_status_pid(unsafe { info.assume_init_ref() }) != group as libc::pid_t
        {
            assert!(Instant::now() < deadline, "the child never exited");
            thread::sleep(REAP_POLL_INTERVAL);
        }
        #[cfg(target_os = "macos")]
        assert_eq!(
            unsafe { libc::kill(-(group as libc::pid_t), 0) },
            -1,
            "macOS stopped answering EPERM for a group of zombies"
        );
        // The other half of the same fact, and the premise the classification rests on: a zombie
        // there takes the signal and drops it, so the group answers as a group that was signalled.
        #[cfg(not(target_os = "macos"))]
        assert_eq!(unsafe { libc::kill(-(group as libc::pid_t), 0) }, 0);
        // Either way the caller is told the same thing: there is nothing left in the group to
        // signal, which is what the refusal only means on one of the two.
        #[cfg(target_os = "macos")]
        assert_eq!(outcome(libc::SIGKILL), GroupSignal::Gone);
        #[cfg(not(target_os = "macos"))]
        assert_eq!(outcome(libc::SIGKILL), GroupSignal::Delivered);
        assert!(process_group_exists(group));

        // Reaped: the group is not there at all, which the probe can see and which is the same
        // outcome for the caller.
        drop(child.wait());
        assert_eq!(outcome(libc::SIGKILL), GroupSignal::Gone);
        assert!(!process_group_exists(group));
    }

    #[test]
    fn failed_reaper_spawn_retains_child_for_retry() {
        #[derive(Clone, Debug)]
        struct ReaperChild(Arc<AtomicUsize>);

        impl ChildKiller for ReaperChild {
            fn kill(&mut self) -> io::Result<()> {
                Ok(())
            }

            fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
                Box::new(self.clone())
            }
        }

        impl Child for ReaperChild {
            fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
                Ok(None)
            }

            fn wait(&mut self) -> io::Result<ExitStatus> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(ExitStatus::with_exit_code(0))
            }

            fn process_id(&self) -> Option<u32> {
                None
            }
        }

        let waited = Arc::new(AtomicUsize::new(0));
        let mut process = ChildState {
            child: Some(Box::new(ReaperChild(Arc::clone(&waited)))),
            exit_code: None,
            child_released: false,
        };
        let error = start_child_reaper(&mut process, |_| Err(io::Error::other("injected")))
            .expect_err("injected reaper spawn should fail");
        assert_eq!(error.to_string(), "injected");
        assert!(
            process.child.is_some(),
            "failed spawn discarded the child handle"
        );
        assert!(!process.child_released);

        start_child_reaper(&mut process, |reaper| {
            thread::Builder::new().spawn(reaper).map(drop)
        })
        .unwrap();
        assert!(process.child.is_none());
        assert!(process.child_released);
        let deadline = Instant::now() + Duration::from_secs(2);
        while waited.load(Ordering::SeqCst) == 0 {
            assert!(
                Instant::now() < deadline,
                "retry reaper did not wait for child"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(waited.load(Ordering::SeqCst), 1);
    }

    #[cfg(unix)]
    #[test]
    fn close_kills_foreground_group_when_its_leader_has_exited() {
        let backend = TerminalBackend::default();
        let (output, receiver) = sink();
        spawn(&backend, "leaderless", "/bin/sh", &["-i"], output);
        synchronize_shell(&backend, &receiver, "leaderless");
        backend.write("leaderless", b"true | sleep 30\n").unwrap();

        let deadline = Instant::now() + Duration::from_secs(3);
        let group = loop {
            let session = backend.sessions.lock().unwrap()["leaderless"]
                .session()
                .clone();
            let group = session
                .master
                .lock()
                .unwrap()
                .process_group_leader()
                .map(|group| group as libc::pid_t);
            if let Some(group) = group {
                let group_exists = unsafe { libc::kill(-group, 0) } == 0;
                let leader_reaped = unsafe { libc::getpgid(group) } < 0
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
                if group_exists && leader_reaped {
                    break group;
                }
            }
            assert!(
                Instant::now() < deadline,
                "pipeline leader did not exit while its process group remained alive"
            );
            thread::sleep(Duration::from_millis(10));
        };

        assert!(backend.close("leaderless").unwrap());
        let deadline = Instant::now() + Duration::from_secs(3);
        while unsafe { libc::kill(-group, 0) } == 0 {
            assert!(
                Instant::now() < deadline,
                "leaderless foreground group survived close"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[cfg(unix)]
    #[test]
    fn close_kills_the_foreground_process_group() {
        let backend = TerminalBackend::default();
        let (output, receiver) = sink();
        spawn(&backend, "closing-foreground", "/bin/sh", &["-i"], output);
        synchronize_shell(&backend, &receiver, "closing-foreground");
        backend.write("closing-foreground", b"sleep 30\n").unwrap();

        let deadline = Instant::now() + Duration::from_secs(3);
        let group = loop {
            let status = backend.status("closing-foreground").unwrap();
            if status.foreground_app.as_deref() == Some("sleep") {
                break backend.sessions.lock().unwrap()["closing-foreground"]
                    .session()
                    .master
                    .lock()
                    .unwrap()
                    .process_group_leader()
                    .unwrap();
            }
            assert!(
                Instant::now() < deadline,
                "foreground sleep was not observed"
            );
            thread::sleep(Duration::from_millis(10));
        };

        assert!(backend.close("closing-foreground").unwrap());
        let deadline = Instant::now() + Duration::from_secs(3);
        while unsafe { libc::kill(-group, 0) } == 0 {
            assert!(
                Instant::now() < deadline,
                "foreground process group was orphaned"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    /// The exit hook is what ends a shell, because the app's exit never unwinds.
    ///
    /// `App::run` hands the process to `std::process::exit` once the loop is done, so the `Drop`
    /// that used to be this code never runs in a shipped app and every terminal the app opened
    /// outlived it. This is the call that replaced it, and it has to reach the shells rather than
    /// only mark the sessions closed.
    #[cfg(unix)]
    #[test]
    fn shutdown_ends_every_shell_the_app_started() {
        let backend = TerminalBackend::default();
        let (output, receiver) = sink();
        spawn(&backend, "shutdown", "/bin/sh", &["-i"], output);
        synchronize_shell(&backend, &receiver, "shutdown");
        let group = {
            let sessions = backend.sessions.lock().unwrap();
            let group = sessions["shutdown"]
                .session()
                .master
                .lock()
                .unwrap()
                .process_group_leader()
                .unwrap();
            group
        };

        backend.shutdown();

        let deadline = Instant::now() + Duration::from_secs(3);
        while unsafe { libc::kill(-group, 0) } == 0 {
            assert!(
                Instant::now() < deadline,
                "the shell was still running after the app shut down"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        // And it is safe to say it twice: the hook and the `Drop` can both reach it.
        backend.shutdown();
    }

    #[cfg(unix)]
    #[test]
    fn foreground_activity_excludes_an_idle_shell_and_tracks_a_foreground_command() {
        let backend = TerminalBackend::default();
        let (output, receiver) = sink();
        spawn(&backend, "foreground", "/bin/sh", &["-i"], output);
        synchronize_shell(&backend, &receiver, "foreground");

        let resting = backend.status("foreground").unwrap();
        assert!(!resting.foreground_process);
        // A shell in front of itself is the resting state and has nothing to name.
        assert_eq!(resting.foreground_app, None);

        backend.write("foreground", b"sleep 10\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let status = backend.status("foreground").unwrap();
            if status.foreground_process && status.foreground_app.as_deref() == Some("sleep") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "foreground sleep was not observed"
            );
            thread::sleep(Duration::from_millis(10));
        }
        // The row in the sidebar shows this, so it has to be the job's own name.
        assert_eq!(
            backend
                .status("foreground")
                .unwrap()
                .foreground_app
                .as_deref(),
            Some("sleep")
        );

        // Keep the foreground-name polling: the child name can settle after the foreground group
        // is first observed.
        let deadline = Instant::now() + Duration::from_secs(3);
        while backend
            .status("foreground")
            .unwrap()
            .foreground_app
            .as_deref()
            != Some("sleep")
        {
            assert!(
                Instant::now() < deadline,
                "the foreground process was not named after the job"
            );
            thread::sleep(Duration::from_millis(10));
        }

        backend.write("foreground", b"\x03").unwrap();
        synchronize_shell(&backend, &receiver, "foreground");
        let resting = backend.status("foreground").unwrap();
        assert!(!resting.foreground_process);
        assert_eq!(resting.foreground_app, None);
        backend.close("foreground").unwrap();
    }

    /// A hold that is going to stay shut is given this long to prove it. Every one of these holds
    /// is bounded by its own `resume_timeout`, so a gate that opens early sends on this channel
    /// well inside it and a gate that stays shut sends nothing.
    const HOLD_WINDOW: Duration = Duration::from_millis(200);

    /// Starts a reader parked in `hold()` and hands back the channel it comes back on.
    ///
    /// The count is put in place before the thread starts, so what the reader finds on its first
    /// pass is the count the test set rather than whatever the thread got to first. "Still waiting"
    /// is then a wait on that channel and not a sleep.
    fn parked_reader(gate: &Arc<OutputGate>) -> (thread::JoinHandle<()>, mpsc::Receiver<()>) {
        let (done_tx, done_rx) = mpsc::channel();
        let gate = Arc::clone(gate);
        let reader = thread::spawn(move || {
            gate.hold();
            let _ = done_tx.send(());
        });
        (reader, done_rx)
    }

    /// The gate is two marks and not one, and this is what pins both: the reader is not stopped
    /// before the high mark, is stopped at it, and once stopped stays stopped until the window is
    /// back under the low one, so a window that keeps reporting the same crowded number cannot make
    /// the two of them take turns forever.
    #[test]
    fn the_gate_closes_at_the_high_mark_and_opens_under_the_low_one() {
        let gate = Arc::new(OutputGate::new(Duration::from_secs(30)));
        // Not one byte under the allowance closes it: a queue the size this gate allows is the
        // queue it was added for, and stopping the reader earlier than that only makes a burst
        // arrive in pieces.
        gate.delivered(OUTPUT_HIGH_WATER_BYTES);
        let (reader, done) = parked_reader(&gate);
        assert!(
            done.recv_timeout(HOLD_WINDOW).is_err(),
            "the gate opened at the high water mark"
        );

        // The window has parsed a quarter of the burst, which leaves most of the allowance owed.
        // Reopening here is what a window that is merely draining would cause, over and over.
        gate.acknowledge(OUTPUT_HIGH_WATER_BYTES / 4);
        assert!(
            done.recv_timeout(HOLD_WINDOW).is_err(),
            "the gate opened between the two water marks"
        );

        // Down to the low mark and the reader goes.
        gate.acknowledge(OUTPUT_HIGH_WATER_BYTES - OUTPUT_LOW_WATER_BYTES);
        done.recv_timeout(Duration::from_secs(3))
            .expect("the gate stayed shut under the low water mark");
        reader.join().unwrap();
    }

    /// The other end of the same pair, and the half that is easy to get wrong in the other
    /// direction: a window that is behind but not as behind as the whole allowance is a window
    /// that is working, and holding the reader for it trades a bounded queue for a terminal that
    /// crawls.
    #[test]
    fn a_gate_under_the_high_mark_never_waits_how_crowded_the_window_is() {
        let gate = Arc::new(OutputGate::new(Duration::from_secs(30)));
        let debt = OUTPUT_LOW_WATER_BYTES + OUTPUT_HIGH_WATER_BYTES / 2;
        gate.delivered(debt);

        let (reader, done) = parked_reader(&gate);
        done.recv_timeout(Duration::from_secs(3))
            .expect("the gate closed on a debt under the high water mark");
        reader.join().unwrap();
    }

    /// What the reader waits for is what the window has not parsed, and that includes the bytes
    /// still travelling to it. The window's own count can only see what has arrived, so the reader
    /// has to keep counting from the moment it hands a chunk over: counted the other way round,
    /// every answer describes a queue the window has already drained while a burst is still on its
    /// way, and nothing is ever held.
    #[test]
    fn output_still_on_its_way_to_the_window_is_not_forgotten_when_an_answer_arrives() {
        let gate = Arc::new(OutputGate::new(Duration::from_secs(30)));
        // A whole mark's worth is on its way and the window has parsed none of it yet.
        let chunk = 64 * 1024;
        gate.delivered(OUTPUT_HIGH_WATER_BYTES);

        let (reader, done) = parked_reader(&gate);
        assert!(
            done.recv_timeout(HOLD_WINDOW).is_err(),
            "the gate opened at the high water mark"
        );

        // The window answers from inside the burst, having parsed its first chunk, while every
        // other chunk is still in the channel. Counted the other way round — against what the
        // window had received — this is the answer that says the queue is empty.
        gate.acknowledge(chunk);
        assert_eq!(
            gate.state().debt(),
            OUTPUT_HIGH_WATER_BYTES - chunk,
            "a chunk still in transit fell out of the reader's count"
        );
        assert!(
            done.recv_timeout(HOLD_WINDOW).is_err(),
            "the gate opened while most of the queue was unparsed"
        );

        // The same answer again, which is what a repeated report and a second chunk crossing it in
        // the channel look like from here. Neither moves the count back to where it was.
        gate.acknowledge(chunk);
        assert_eq!(
            gate.state().debt(),
            OUTPUT_HIGH_WATER_BYTES - chunk,
            "a repeated answer moved the reader's count backwards"
        );
        assert!(
            done.recv_timeout(HOLD_WINDOW).is_err(),
            "a repeated answer opened the gate"
        );

        // An answer that claims more than was ever handed over is clamped to what was delivered,
        // rather than taken at its word, so it cannot swallow the counting of the next chunk.
        gate.acknowledge(OUTPUT_HIGH_WATER_BYTES - OUTPUT_LOW_WATER_BYTES);
        done.recv_timeout(Duration::from_secs(3))
            .expect("the gate stayed shut under the low water mark");
        reader.join().unwrap();
    }

    /// A window that reports more than it was ever given is a window whose answer crossed the
    /// reader's own count, which the reader cannot check for by waiting. It is clamped to what was
    /// delivered rather than taken at its word, so it cannot swallow the counting of the chunk that
    /// comes after it.
    #[test]
    fn an_answer_ahead_of_the_readers_own_count_is_clamped_to_it() {
        let gate = OutputGate::new(Duration::from_secs(30));
        let chunk = 64 * 1024;
        gate.delivered(OUTPUT_HIGH_WATER_BYTES);

        gate.acknowledge(OUTPUT_HIGH_WATER_BYTES * 2);
        assert_eq!(
            gate.state().debt(),
            0,
            "the gate kept a debt nobody was owed"
        );
        gate.delivered(chunk);
        assert_eq!(
            gate.state().debt(),
            chunk,
            "an answer ahead of the reader's own count swallowed the next chunk"
        );
    }

    /// An answer that never comes has to cost throughput and nothing else. Failing open is what
    /// stops a lost answer from turning into a terminal nobody can type into.
    #[test]
    fn a_window_that_never_answers_opens_the_gate_by_itself() {
        let gate = OutputGate::new(Duration::from_millis(50));
        gate.delivered(OUTPUT_HIGH_WATER_BYTES);

        let started = Instant::now();
        gate.hold();
        assert!(
            started.elapsed() >= Duration::from_millis(40),
            "the gate gave up before it had waited"
        );
        // And the debt it was counting went with it: the reader is not left owing two megabytes on
        // the strength of a count nobody confirmed, which is what would make every chunk after this
        // wait out the timeout again, one chunk at a time. The window's total moved up to what it
        // was sent rather than back to zero, so what arrives next is counted from here.
        assert_eq!(gate.state().debt(), 0, "the timeout left the debt standing");
        gate.delivered(OUTPUT_LOW_WATER_BYTES);
        assert_eq!(
            gate.state().debt(),
            OUTPUT_LOW_WATER_BYTES,
            "the timeout forgot the bytes that came after it"
        );

        // An answer that turns up after all of that is out of date rather than wrong: it describes
        // a queue from before the timeout, and it cannot rewind the reader's count back to it.
        gate.acknowledge(OUTPUT_HIGH_WATER_BYTES);
        assert_eq!(
            gate.state().debt(),
            OUTPUT_LOW_WATER_BYTES,
            "a late answer rewound the count the timeout had settled"
        );
        let started = Instant::now();
        gate.hold();
        assert!(
            started.elapsed() < Duration::from_millis(40),
            "the debt the timeout left behind is still holding the reader"
        );
    }

    /// A window that is gone will never answer, so a gate that waited for one would hold the
    /// reader until the timeout for the rest of the session.
    #[test]
    fn a_released_gate_never_holds_the_reader_again() {
        let gate = OutputGate::new(Duration::from_secs(30));
        gate.delivered(OUTPUT_HIGH_WATER_BYTES);
        gate.release();

        let started = Instant::now();
        gate.hold();
        assert!(started.elapsed() < Duration::from_millis(50));
        // An answer that arrives late, from a window that was on its way out, changes nothing.
        gate.delivered(OUTPUT_HIGH_WATER_BYTES);
        gate.acknowledge(OUTPUT_HIGH_WATER_BYTES);
        let started = Instant::now();
        gate.hold();
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    /// The whole point of the gate, measured against a real PTY and a real process behind it.
    ///
    /// The window here is as slow as the worst one and the burst is one chunk larger than the
    /// queue is allowed to be, so the reader has to stop mid-burst — which is what makes the child
    /// block in `write` instead of the app growing. What is asserted afterwards is the thing that
    /// makes the gate safe to have at all: every byte of the burst, in order, with none of it
    /// duplicated, dropped or reordered across the pause.
    #[test]
    fn a_slow_window_stops_the_reader_without_losing_a_byte() {
        let directory = tempdir().unwrap();
        // Twice the allowance: the reader is meant to stop in the middle of this burst, and a burst
        // that fits under the mark would arrive whole with the gate never once closing.
        let payload: Vec<u8> = (0..2 * OUTPUT_HIGH_WATER_BYTES)
            .map(|index| b'a' + (index % 26) as u8)
            .collect();
        let source = directory.path().join("burst.bin");
        std::fs::write(&source, &payload).unwrap();

        let backend = TerminalBackend::default();
        let gate = Arc::new(OutputGate::new(OUTPUT_RESUME_TIMEOUT));
        let delivered = Arc::new(Mutex::new(Vec::<u8>::new()));
        let (chunk_tx, chunk_rx) = mpsc::channel();
        let output: OutputSink = {
            let gate = Arc::clone(&gate);
            let delivered = Arc::clone(&delivered);
            Box::new(move |bytes| {
                gate.hold();
                // Counted before the hand-off, as the real sink does: the reader's total is what
                // the window's answers are measured against, so a chunk still in transit has to be
                // in it or the burst is never waited for.
                gate.delivered(bytes.len());
                delivered
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .extend_from_slice(bytes);
                let _ = chunk_tx.send(());
                Ok(())
            })
        };
        spawn(
            &backend,
            "slow-window",
            "/bin/cat",
            &[source.to_str().unwrap()],
            output,
        );

        // Let the burst run until the window is as far behind as the allowance allows, and keep
        // answering as a window that has parsed nothing does. Everything already in the pipe lands
        // first, so what follows is the reader actually waiting rather than a chunk it had not
        // reached yet.
        let settled = loop {
            assert!(
                delivered
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .len()
                    < payload.len(),
                "the whole burst arrived without the reader ever being held"
            );
            gate.acknowledge(0);
            let before = chunk_rx.recv_timeout(Duration::from_secs(3)).ok();
            if before.is_none() {
                break delivered
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .len();
            }
        };

        thread::sleep(Duration::from_millis(200));
        assert_eq!(
            delivered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            settled,
            "the reader kept draining the PTY while the window was behind"
        );

        // The window catches up with everything it was given, and the rest of the burst arrives whole
        // and in order. A total larger than the burst is the shape of a window that has parsed all
        // of it: the gate clamps it to what was delivered, which is the same answer.
        gate.acknowledge(usize::MAX);
        let deadline = Instant::now() + Duration::from_secs(10);
        while delivered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
            < payload.len()
        {
            assert!(Instant::now() < deadline, "the held output never arrived");
            let _ = chunk_rx.recv_timeout(Duration::from_millis(100));
        }
        assert_eq!(
            &*delivered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            &payload,
            "output was lost or reordered across the hold"
        );
        backend.close("slow-window").unwrap();
    }

    /// The gate sits between the reader and everything else, so a close has to get past it.
    ///
    /// The window in this test never answers at all, which is the case where the reader is held
    /// for the whole life of the session. `close` does not wait for the reader — it signals the
    /// process group and reaps the child — so a terminal that is publishing faster than anybody
    /// reads still closes on time instead of waiting out the safety timeout first.
    #[test]
    fn a_session_publishing_intensely_closes_without_waiting_for_the_reader() {
        let backend = Arc::new(TerminalBackend::default());
        let gate = Arc::new(OutputGate::new(OUTPUT_RESUME_TIMEOUT));
        let (chunk_tx, chunk_rx) = mpsc::channel();
        let output: OutputSink = {
            let gate = Arc::clone(&gate);
            Box::new(move |bytes| {
                // A window that never answers, which in the cumulative protocol is a window that
                // has parsed nothing: every answer leaves the whole queue owed.
                gate.acknowledge(0);
                gate.hold();
                gate.delivered(bytes.len());
                let _ = chunk_tx.send(());
                Ok(())
            })
        };
        spawn(
            &backend,
            "flood",
            "/bin/sh",
            &["-c", "while :; do printf 'flooding\\n'; done"],
            output,
        );
        chunk_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("the flood never produced output");

        let (close_tx, close_rx) = mpsc::channel();
        let closing = Arc::clone(&backend);
        let close = thread::spawn(move || {
            let _ = close_tx.send(closing.close("flood"));
        });
        let closed = close_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("close waited on a reader held by a window that never answers");
        assert!(closed.unwrap());
        close.join().unwrap();
    }

    use crate::workspace::TerminalProcessState;

    #[test]
    fn a_reader_that_reaches_the_end_of_the_pty_says_so_exactly_once() {
        let backend = TerminalBackend::default();
        let (sink, receiver) = sink();
        let (end_sink, ends) = reader_end_sink();
        backend
            .spawn(
                "ends:stream".into(),
                SpawnOptions {
                    program: PathBuf::from("/bin/sh"),
                    args: vec!["-c".into(), "exit 0".into()],
                    cwd: std::env::current_dir().unwrap(),
                    cols: 80,
                    rows: 24,
                    env: vec![],
                    reader_end: Some(end_sink),
                },
                sink,
            )
            .unwrap();

        assert_eq!(
            ends.recv_timeout(Duration::from_secs(5))
                .expect("no reader end was reported"),
            ReaderEnd::EndOfStream,
            "a shell that exits cleanly ends the stream, which is not a read failure",
        );
        assert!(
            ends.recv_timeout(Duration::from_millis(200)).is_err(),
            "the reader reported its end more than once",
        );
        drop(receiver);
        backend.shutdown();
    }

    #[test]
    fn a_reader_that_unwinds_says_the_reader_stopped_rather_than_going_quiet() {
        let backend = TerminalBackend::default();
        let (end_sink, ends) = reader_end_sink();
        // A child that outlives the unwind, so a silent reader here would leave a live
        // process with no reader and nothing to say so.
        backend
            .spawn(
                "ends:panic".into(),
                SpawnOptions {
                    program: PathBuf::from("/bin/sh"),
                    args: vec!["-c".into(), "printf x; sleep 30".into()],
                    cwd: std::env::current_dir().unwrap(),
                    cols: 80,
                    rows: 24,
                    env: vec![],
                    reader_end: Some(end_sink),
                },
                Box::new(|_| panic!("the renderer went away mid-chunk")),
            )
            .unwrap();

        assert_eq!(
            ends.recv_timeout(Duration::from_secs(5))
                .expect("an unwinding reader stayed quiet"),
            ReaderEnd::ReaderStopped,
        );
        backend.shutdown();
    }
}
