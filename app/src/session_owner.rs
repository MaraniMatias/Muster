//! Who owns a shell, and who is allowed to stop it.
//!
//! A PTY outlives the entity that drew it: the reader thread parses into the
//! grid, the session owns the process, and the window that showed it can be
//! gone while the process is not. So ownership does not live in the view.
//!
//! [`SessionOwner`] is that answer. One per window, created before the window
//! exists, holding the registry of live shells: spawn adds to it, close removes
//! from it, and quit drains it. A worker that finishes after quit has already
//! drained finds the registry closed and refuses, so a late PTY cannot outlive
//! the process that asked for it.
//!
//! It also owns this window's [`StateWriter`], because the state file is the
//! same kind of thing: work that belongs to the window's lifetime rather than
//! to its view, and that has to be drained on the same path.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::pty::TerminalBackend;
use crate::state_writer::StateWriter;
use crate::terminal::{
    shutdown, xterm_256, CellMetrics, Palette, TerminalHost, TerminalSession, TerminalSessionConfig,
};
use muster_core::navigation::NavigationState;

#[cfg(test)]
use crate::terminal::{CloseReport, SessionError};

/// The cell a child is told about before this window has drawn a frame.
pub(crate) const UNMEASURED_CELL: CellMetrics = CellMetrics {
    width: 8,
    height: 16,
};

/// A live shell, with the cell measurement its host publishes into.
pub(crate) struct OwnedShell {
    pub(crate) session: TerminalSession,
    pub(crate) metrics: Arc<Mutex<CellMetrics>>,
}

#[derive(Default)]
struct Ownership {
    closing: bool,
    shells: HashMap<String, Arc<OwnedShell>>,
}

/// Spawn and shutdown share a gate. A worker cannot publish a late PTY after
/// quit has drained the registry; ownership starts before any UI entity exists.
pub(crate) struct SessionOwner {
    backend: Arc<TerminalBackend>,
    owns_backend: bool,
    state: Mutex<Ownership>,
    stop_gate: Mutex<()>,
    writer: Mutex<Option<StateWriter>>,
}

impl SessionOwner {
    pub(crate) fn new(backend: Arc<TerminalBackend>, owns_backend: bool) -> Arc<Self> {
        Arc::new(Self {
            backend,
            owns_backend,
            state: Mutex::new(Ownership::default()),
            stop_gate: Mutex::new(()),
            writer: Mutex::new(None),
        })
    }

    pub(crate) fn spawn(
        &self,
        mut config: TerminalSessionConfig,
    ) -> Result<Arc<OwnedShell>, String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        if state.closing {
            return Err("The workspace is closing.".into());
        }
        let metrics = Arc::new(Mutex::new(UNMEASURED_CELL));
        config.host = host(Arc::clone(&metrics));
        let session = TerminalSession::spawn(Arc::clone(&self.backend), config)
            .map_err(|error| error.to_string())?;
        let shell = Arc::new(OwnedShell { session, metrics });
        state
            .shells
            .insert(shell.session.id().into(), shell.clone());
        Ok(shell)
    }

    /// The PTY backend this window's shells run in.
    ///
    /// The window asks it what is running when a worktree is about to go:
    /// a process that is still working in a directory is a fact the sidebar's
    /// own list of terminals cannot answer.
    pub(crate) fn backend(&self) -> &Arc<TerminalBackend> {
        &self.backend
    }

    pub(crate) fn close_one(&self, id: &str) -> Result<(), String> {
        // A per-window shutdown may be closing this same session. Serializing
        // both paths ensures exactly one caller begins the PTY close sequence;
        // the loser then observes the shell already removed rather than trying
        // to read a child the backend has marked as closing.
        let _gate = self
            .stop_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let shell = self
            .state
            .lock()
            .map_err(|error| error.to_string())?
            .shells
            .get(id)
            .cloned();
        if let Some(shell) = shell {
            shell
                .session
                .close()
                .wait()
                .map_err(|error| error.to_string())?;
            self.state
                .lock()
                .map_err(|error| error.to_string())?
                .shells
                .remove(id);
        }
        Ok(())
    }

    /// Install the state writer once the workspace file's path is known.
    ///
    /// A window with no state file still opens; it simply has nothing to save.
    pub(crate) fn install_writer(&self, writer: Option<StateWriter>) {
        *self.writer.lock().unwrap_or_else(PoisonError::into_inner) = writer;
    }

    /// Hand the window's newest state to the writer.
    pub(crate) fn persist(&self, state: NavigationState) {
        if let Some(writer) = self
            .writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            writer.send(state);
        }
    }

    /// The failure the last save reported, taken once so the window shows it
    /// and then stops asking about it on every tick.
    pub(crate) fn writer_failure(&self) -> Option<String> {
        self.writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(|writer| writer.take_error())
    }

    pub(crate) fn stop(&self) {
        let _gate = self
            .stop_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let shells = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.closing {
                return;
            }
            state.closing = true;
            state.shells.values().cloned().collect::<Vec<_>>()
        };
        // This is synchronous deliberately: GPUI exits the process after its
        // quit hook. A detached close thread would lose that race.
        if self.owns_backend {
            shutdown(&self.backend);
        }
        for shell in shells {
            if let Err(error) = shell.session.close().wait() {
                log::warn!("terminal cleanup failed: {error}");
            }
        }
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .shells
            .clear();
        // Dropping the writer is the flush: its `Drop` joins the save thread,
        // which drains the newest snapshot before it returns.
        self.writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }
}

impl Drop for SessionOwner {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The window's end of a PTY: what the child measures itself against, and how
/// it answers the colour indices a VT sequence asked for.
///
/// The palette half of this is the terminal's own contract and belongs to
/// [`Palette`]: an index a remote program chose is drawn as that index chose,
/// not as the application's chrome. Nothing here consults the app's theme.
pub(crate) fn host(metrics: Arc<Mutex<CellMetrics>>) -> TerminalHost {
    TerminalHost {
        cell_metrics: Box::new(move || metrics.lock().map(|cell| *cell).unwrap_or(UNMEASURED_CELL)),
        resolve_color: Box::new(|index| match index {
            0..=255 => xterm_256(index as u8),
            257 => Palette::default().background,
            258 => Palette::default().cursor,
            _ => Palette::default().foreground,
        }),
        store_clipboard: Box::new(|_| {}),
    }
}

// The original ownership regression test remains isolated from production
// window ownership. Injected views never claim or terminate somebody else's PTY.
#[cfg(test)]
static TEST_OWNER: Mutex<Option<Arc<SessionOwner>>> = Mutex::new(None);

#[cfg(test)]
pub(crate) fn spawn_and_claim(
    backend: Arc<TerminalBackend>,
    config: TerminalSessionConfig,
) -> Result<Arc<OwnedShell>, String> {
    let owner = SessionOwner::new(backend, true);
    let shell = owner.spawn(config)?;
    *TEST_OWNER.lock().unwrap() = Some(owner);
    Ok(shell)
}

#[cfg(test)]
pub(crate) fn end_shell() -> Option<Result<CloseReport, SessionError>> {
    let owner = TEST_OWNER.lock().ok()?.take()?;
    let shell = owner.state.lock().ok()?.shells.values().next()?.clone();
    let report = shell.session.close().wait();
    owner.stop();
    Some(report)
}
