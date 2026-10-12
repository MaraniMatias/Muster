//! Native workspace window. Terminal entities are retained by session identity;
//! navigation and folding never own (or restart) a PTY.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc, Mutex, Weak,
};
use std::thread;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    dialog::{Dialog, DialogContent, DialogFooter},
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem},
    resizable::{h_resizable, resizable_panel, ResizableState},
    scroll::ScrollableElement,
    Disableable, Icon, Sizable,
};
use gpui_kit::{
    div, prelude::FluentBuilder, px, size, AnyElement, App, AppContext, Bounds, Context, Entity,
    Focusable, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, PathPromptOptions,
    PromptLevel, Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Task, Window, WindowBounds, WindowOptions,
};
use muster_core::navigation::{self, DiffTotals, NavigationState};
use muster_core::workspace::{
    Repo, RepoKind, Session, SessionStatus as SavedSessionStatus, SessionType,
};

use crate::navigation::{checkout_title, session_title, Navigation};
use crate::pty::TerminalBackend;
use crate::terminal::{
    shutdown, xterm_256, CellMetrics, ColorSnapshot, Palette, ShellState, TerminalHost,
    TerminalSession, TerminalSessionConfig, TerminalView,
};

#[cfg(test)]
use crate::terminal::{CloseReport, SessionError};

static NEXT_SESSION: AtomicU64 = AtomicU64::new(0);
static OWNERS: Mutex<Vec<Weak<SessionOwner>>> = Mutex::new(Vec::new());
const INITIAL_COLUMNS: u16 = 80;
const INITIAL_ROWS: u16 = 24;
/// How many maintenance ticks pass between two line-count passes.
///
/// Every tick re-reads Git's worktree list; a line count is a full read of
/// every changed file on top of that, so it runs once every ten seconds rather
/// than once every two.
const COUNT_EVERY_TICKS: u64 = 5;
const UNMEASURED_CELL: CellMetrics = CellMetrics {
    width: 8,
    height: 16,
};

struct OwnedShell {
    session: TerminalSession,
    metrics: Arc<Mutex<CellMetrics>>,
}
#[derive(Default)]
struct Ownership {
    closing: bool,
    shells: HashMap<String, Arc<OwnedShell>>,
}

/// Spawn and shutdown share a gate. A worker cannot publish a late PTY after
/// quit has drained the registry; ownership starts before any UI entity exists.
struct SessionOwner {
    backend: Arc<TerminalBackend>,
    owns_backend: bool,
    state: Mutex<Ownership>,
    stop_gate: Mutex<()>,
    writer: Mutex<Option<StateWriter>>,
}
impl SessionOwner {
    fn new(backend: Arc<TerminalBackend>, owns_backend: bool) -> Arc<Self> {
        Arc::new(Self {
            backend,
            owns_backend,
            state: Mutex::new(Ownership::default()),
            stop_gate: Mutex::new(()),
            writer: Mutex::new(None),
        })
    }
    fn spawn(&self, mut config: TerminalSessionConfig) -> Result<Arc<OwnedShell>, String> {
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
    fn close_one(&self, id: &str) -> Result<(), String> {
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
    fn stop(&self) {
        let _gate = self
            .stop_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let shells = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .shells
            .clear();
        self.writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}
impl Drop for SessionOwner {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One serial writer for this window. UI sends snapshots in order; shutdown
/// joins it so the last accepted snapshot isn't lost on process exit.
struct StateWriter {
    sender: Option<mpsc::Sender<NavigationState>>,
    thread: Option<thread::JoinHandle<()>>,
    error: Arc<Mutex<Option<String>>>,
}
impl StateWriter {
    fn new(path: PathBuf) -> Result<Self, String> {
        let (sender, receiver) = mpsc::channel::<NavigationState>();
        let error = Arc::new(Mutex::new(None));
        let failures = error.clone();
        let thread = thread::Builder::new()
            .name("muster-navigation-save".into())
            .spawn(move || {
                while let Ok(state) = receiver.recv() {
                    if let Err(cause) = navigation::save(&path, &state) {
                        *failures
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(cause);
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            sender: Some(sender),
            thread: Some(thread),
            error,
        })
    }
    fn send(&self, state: NavigationState) {
        if let Some(sender) = &self.sender {
            if let Err(error) = sender.send(state) {
                *self
                    .error
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error.to_string());
            }
        }
    }
}
impl Drop for StateWriter {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// Kit Button already has a stable ID, but only exposes the generic interactive
// trait. Use GPUI's native stateful setters on that same interactivity, so fold
// and selection metadata describe the real focused control, not a second node.
struct StatefulButton(Button);
impl InteractiveElement for StatefulButton {
    fn interactivity(&mut self) -> &mut gpui_kit::Interactivity {
        self.0.interactivity()
    }
}
impl StatefulInteractiveElement for StatefulButton {}
trait ButtonA11y {
    fn aria_expanded(self, expanded: bool) -> Self;
    fn aria_selected(self, selected: bool) -> Self;
}
impl ButtonA11y for Button {
    fn aria_expanded(self, expanded: bool) -> Self {
        StatefulButton(self).aria_expanded(expanded).0
    }
    fn aria_selected(self, selected: bool) -> Self {
        StatefulButton(self).aria_selected(selected).0
    }
}

struct LiveTerminal {
    view: Entity<TerminalView>,
    shell: Arc<OwnedShell>,
}
struct Rename {
    id: String,
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// The one worktree this window is creating, if it is creating one.
///
/// Its own struct rather than a field soup because the dialog is a modal
/// conversation: the three inputs, whether the branch is still the suggestion,
/// what Git answered about the starting point, and the failure that keeps the
/// dialog open. The mutation runs on a worker and cannot be rolled back, so
/// `busy` also refuses the dialog's own Cancel.
struct WorktreeDraft {
    repo_id: String,
    task: Entity<InputState>,
    branch: Entity<InputState>,
    /// The branch this worktree starts from, prefilled when Git named one.
    ///
    /// Not optional because a repository whose default branch Git will not
    /// guess is ordinary enough to meet on the first run, and the answer to
    /// that is a field to fill rather than a dead dialog. It is also the only
    /// answer creation uses: a field the person cleared is a starting point
    /// they refused, and a second, remembered answer would quietly override it.
    base: Entity<InputState>,
    /// Where worktrees of this repository go, once Git has answered.
    location: Option<PathBuf>,
    branch_edited: bool,
    /// Set while the task name is rewriting the branch, so a programmatic
    /// value is not read back as the person having taken the branch over.
    syncing_branch: bool,
    busy: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

pub struct TerminalWindow {
    navigation: Navigation,
    terminals: HashMap<String, LiveTerminal>,
    /// Line counts per checkout, absent when nothing was counted yet.
    ///
    /// Absent means "not counted", never "no changes": a checkout Git could
    /// not be asked about draws nothing rather than a confident zero.
    diff_totals: HashMap<String, DiffTotals>,
    worktree: Option<WorktreeDraft>,
    owner: Arc<SessionOwner>,
    failure: Option<String>,
    notice: Option<String>,
    opening: bool,
    closing: bool,
    closing_sessions: std::collections::HashSet<String>,
    membership_revision: u64,
    latest_terminal_request: Option<String>,
    editing: Option<Rename>,
    resize: Entity<ResizableState>,
    scroll: ScrollHandle,
    focus_pending: bool,
    palette: Palette,
    /// Injected-session tests exercise the same view/focus path without touching
    /// the person's state file, directory picker or session ownership.
    injected: bool,
    _maintenance: Option<Task<()>>,
}

impl TerminalWindow {
    fn empty(backend: Arc<TerminalBackend>, owns_backend: bool, cx: &mut Context<Self>) -> Self {
        Self {
            navigation: Navigation::new(),
            terminals: HashMap::new(),
            diff_totals: HashMap::new(),
            worktree: None,
            owner: SessionOwner::new(backend, owns_backend),
            failure: None,
            notice: None,
            opening: false,
            closing: false,
            closing_sessions: Default::default(),
            membership_revision: 0,
            latest_terminal_request: None,
            editing: None,
            resize: cx.new(|_| ResizableState::default()),
            scroll: ScrollHandle::new(),
            focus_pending: true,
            palette: Palette::default(),
            injected: false,
            _maintenance: None,
        }
    }

    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut app = Self::empty(Arc::new(TerminalBackend::default()), true, cx);
        OWNERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Arc::downgrade(&app.owner));
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            let _ = weak.update(cx, |app, cx| app.request_window_close(window, cx));
            false
        });
        app.opening = true;
        let cwd = working_directory();
        let worker = cx
            .background_executor()
            .spawn(async move { restore_navigation(&cwd) });
        cx.spawn_in(window, async move |view, cx| {
            let loaded = worker.await;
            let _ = view.update_in(cx, |app, window, cx| {
                app.opening = false;
                app.failure = loaded.errors.first().cloned();
                app.navigation = loaded.navigation;
                match loaded.state_path.map(StateWriter::new).transpose() {
                    Ok(writer) => {
                        *app.owner
                            .writer
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = writer
                    }
                    Err(error) => app.failure = Some(error),
                }
                app.persist();
                if let Some(id) = app.navigation.active_checkout.clone() {
                    app.request_terminal(&id, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
        app.start_maintenance(cx);
        app
    }

    pub fn with_session(
        session: TerminalSession,
        backend: Arc<TerminalBackend>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut app = Self::empty(backend, false, cx);
        app.injected = true;
        let shell = Arc::new(OwnedShell {
            session: session.clone(),
            metrics: Arc::new(Mutex::new(UNMEASURED_CELL)),
        });
        let id = session.id().to_owned();
        let view = cx.new(|cx| TerminalView::new(session, shell.metrics.clone(), cx));
        app.terminals.insert(
            id.clone(),
            LiveTerminal {
                view,
                shell: shell.clone(),
            },
        );
        app.navigation.active_session = Some(id);
        window.on_window_should_close(cx, move |_, _| {
            let status = shell.session.status();
            if status.foreground.running || status.poll_error.is_some() {
                return false;
            }
            shell.session.close();
            true
        });
        app
    }

    pub fn terminal(&self) -> Option<&Entity<TerminalView>> {
        self.navigation
            .active_session
            .as_ref()
            .and_then(|id| self.terminals.get(id))
            .map(|terminal| &terminal.view)
    }

    fn persist(&self) {
        if let Some(writer) = self
            .owner
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            writer.send(self.navigation.persistent_state());
        }
    }

    fn request_terminal(&mut self, checkout_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        let Some(checkout) = self.navigation.checkout(checkout_id) else {
            return;
        };
        if checkout.is_missing || self.navigation.archived.contains(checkout_id) {
            self.failure = Some("This directory is unavailable.".into());
            cx.notify();
            return;
        }
        let cwd = PathBuf::from(&checkout.path);
        let checkout_id = checkout_id.to_owned();
        let id = format!(
            "native-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        );
        let revision = self.navigation.revision;
        self.latest_terminal_request = Some(id.clone());
        self.navigation
            .pending
            .insert(id.clone(), checkout_id.clone());
        let config =
            TerminalSessionConfig::new(id.clone(), cwd, INITIAL_COLUMNS, INITIAL_ROWS, None);
        let name = crate::terminal::login_shell()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "shell".into());
        let owner = self.owner.clone();
        let worker = cx
            .background_executor()
            .spawn(async move { owner.spawn(config) });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update_in(cx, |app, window, cx| {
                app.navigation.pending.remove(&id);
                match result {
                    Ok(shell) => {
                        let accepted = !app.closing
                            && app.navigation.add_session(
                                Session {
                                    id: id.clone(),
                                    checkout_id,
                                    name,
                                    created_at: String::new(),
                                    session_type: SessionType::Shell,
                                    status: SavedSessionStatus::Active,
                                },
                                (app.latest_terminal_request.as_deref() == Some(&id))
                                    .then_some(revision),
                            );
                        if accepted {
                            let terminal = cx.new(|cx| {
                                TerminalView::new(shell.session.clone(), shell.metrics.clone(), cx)
                            });
                            app.terminals.insert(
                                id.clone(),
                                LiveTerminal {
                                    view: terminal,
                                    shell,
                                },
                            );
                            if app.navigation.active_session.as_deref() == Some(&id) {
                                app.focus_terminal(window, cx);
                            }
                            app.persist();
                        } else {
                            let owner = app.owner.clone();
                            cx.background_executor()
                                .spawn(async move {
                                    let _ = owner.close_one(&id);
                                })
                                .detach();
                        }
                    }
                    Err(error) => app.failure = Some(error),
                }
                cx.notify();
            });
            // If the entity disappeared, its owner stops every registered shell,
            // including this worker's result; no UI callback is needed for cleanup.
        })
        .detach();
        cx.notify();
    }

    fn focus_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(terminal) = self.terminal() {
            terminal.read(cx).focus_handle(cx).focus(window, cx);
        }
        self.focus_pending = false;
    }

    fn choose_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.opening || self.closing {
            return;
        }
        self.opening = true;
        let revision = self.navigation.revision;
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open directory".into()),
        });
        let background = cx.background_executor().clone();
        cx.spawn_in(window, async move |view, cx| {
            let result = match prompt.await {
                Ok(Ok(Some(paths))) if !paths.is_empty() => {
                    let path = paths[0].clone();
                    background.spawn(async move { navigation::open_directory(&path).map(Some) }).await
                }
                Ok(Ok(_)) => Ok(None),
                Ok(Err(error)) => Err(error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            let _ = view.update_in(cx, |app, window, cx| {
                app.opening = false;
                if !app.closing {
                    match result {
                        Ok(Some(opened)) => {
                            if app.navigation.archived.contains(&opened.checkout_id) {
                                app.failure = Some("This worktree is archived. Restore it from its repository menu first.".into());
                            } else {
                                let select = app.navigation.revision == revision;
                                app.navigation.merge(opened, select);
                                app.membership_revision += 1;
                                app.persist();
                                if select { app.focus_terminal(window, cx); }
                            }
                        }
                        Ok(None) => (),
                        Err(error) => app.failure = Some(error),
                    }
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    fn start_maintenance(&mut self, cx: &mut Context<Self>) {
        let background = cx.background_executor().clone();
        self._maintenance = Some(cx.spawn(async move |view, cx| {
            let mut ticks: u64 = 0;
            loop {
                background.timer(Duration::from_secs(2)).await;
                ticks += 1;
                let Ok((epoch, repos, closing, counts)) = view.update(cx, |app, cx| {
                    if let Some(error) = app
                        .owner
                        .writer
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_ref()
                        .and_then(|writer| writer.error.lock().ok()?.take())
                    {
                        app.failure = Some(error);
                        cx.notify();
                    }
                    // Status changes repaint inactive terminal rows too.
                    cx.notify();
                    // Counted far less often than membership is refreshed: a
                    // line total is a full Git read of every changed file, and
                    // this is a background refresh, not a Git watcher.
                    let counts = if ticks.is_multiple_of(COUNT_EVERY_TICKS) {
                        app.navigation
                            .repos
                            .iter()
                            .flat_map(|repo| repo.checkouts.iter())
                            .filter(|checkout| {
                                !checkout.is_missing
                                    && !app.navigation.archived.contains(&checkout.id)
                            })
                            .map(|checkout| (checkout.id.clone(), checkout.path.clone()))
                            .collect::<Vec<_>>()
                    } else {
                        Vec::new()
                    };
                    (
                        app.membership_revision,
                        app.navigation
                            .repos
                            .iter()
                            .filter(|repo| repo.kind == RepoKind::Git)
                            .map(|repo| (repo.id.clone(), repo.root.clone()))
                            .collect::<Vec<_>>(),
                        app.closing,
                        counts,
                    )
                }) else {
                    return;
                };
                if closing {
                    return;
                }
                let results = background
                    .spawn(async move {
                        repos
                            .into_iter()
                            .map(|(id, root)| (id, navigation::open_directory(Path::new(&root))))
                            .collect::<Vec<_>>()
                    })
                    .await;
                if view
                    .update(cx, |app, cx| {
                        if app.closing || app.membership_revision != epoch {
                            return;
                        }
                        for (id, result) in results {
                            if !app.navigation.repos.iter().any(|repo| repo.id == id) {
                                continue;
                            }
                            match result {
                                Ok(opened) => app.navigation.merge(opened, false),
                                Err(error) => {
                                    if let Some(repo) =
                                        app.navigation.repos.iter_mut().find(|repo| repo.id == id)
                                    {
                                        for checkout in &mut repo.checkouts {
                                            checkout.is_missing =
                                                !Path::new(&checkout.path).is_dir();
                                        }
                                    }
                                    app.failure = Some(error);
                                }
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
                // A checkout Git cannot be counted drops its total rather than
                // showing a zero it did not measure. The message goes to the log
                // because the row itself is honest by drawing nothing, and the
                // dialog that does need the answer asks Git for itself.
                if counts.is_empty() {
                    continue;
                }
                let totals = background
                    .spawn(async move {
                        counts
                            .into_iter()
                            .map(|(id, path)| {
                                let total = navigation::diff_totals(Path::new(&path))
                                    .map_err(|error| log::debug!("muster-native: {path}: {error}"));
                                (id, total)
                            })
                            .collect::<Vec<_>>()
                    })
                    .await;
                if view
                    .update(cx, |app, cx| {
                        for (id, total) in totals {
                            match total {
                                Ok(total) => {
                                    app.diff_totals.insert(id, total);
                                }
                                Err(()) => {
                                    app.diff_totals.remove(&id);
                                }
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        }));
    }

    fn request_close_terminal(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing_sessions.contains(id) || self.closing {
            return;
        }
        let Some(terminal) = self.terminals.get(id) else {
            return;
        };
        let status = terminal.shell.session.status();
        if status.foreground.running
            || status.poll_error.is_some()
            || matches!(status.shell, ShellState::Unknown)
        {
            let prompt = window.prompt(PromptLevel::Warning, "Close this terminal?",
                Some("A running process may be stopped. If its state is unavailable, closing may still interrupt work."), &["Cancel", "Close terminal"], cx);
            let id = id.to_owned();
            cx.spawn_in(window, async move |view, cx| {
                if prompt.await == Ok(1) {
                    let _ =
                        view.update_in(cx, |app, window, cx| app.close_terminal(&id, window, cx));
                }
            })
            .detach();
        } else {
            self.close_terminal(id, window, cx);
        }
    }

    fn close_terminal(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.closing_sessions.insert(id.into()) {
            return;
        }
        let owner = self.owner.clone();
        let id = id.to_owned();
        let closing_id = id.clone();
        let worker = cx
            .background_executor()
            .spawn(async move { owner.close_one(&closing_id) });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update_in(cx, |app, window, cx| {
                app.closing_sessions.remove(&id);
                match result {
                    Ok(()) => {
                        app.terminals.remove(&id);
                        app.navigation.remove_session(&id);
                        app.persist();
                        app.focus_terminal(window, cx);
                    }
                    Err(error) => app.failure = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn request_window_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        let busy = self.terminals.values().any(|terminal| {
            let status = terminal.shell.session.status();
            status.foreground.running
                || status.poll_error.is_some()
                || matches!(status.shell, ShellState::Unknown)
        }) || !self.navigation.pending.is_empty();
        if busy {
            let prompt = window.prompt(PromptLevel::Warning, "Close Muster and stop its terminals?",
                Some("All terminals in this window will be stopped, including terminals in other directories."), &["Cancel", "Stop and close"], cx);
            cx.spawn_in(window, async move |view, cx| {
                if prompt.await == Ok(1) {
                    let _ =
                        view.update_in(cx, |app, window, cx| app.finish_window_close(window, cx));
                }
            })
            .detach();
        } else {
            self.finish_window_close(window, cx);
        }
    }

    fn finish_window_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        self.closing = true;
        self.persist();
        let owner = self.owner.clone();
        let worker = cx.background_executor().spawn(async move {
            owner.stop();
        });
        cx.spawn_in(window, async move |view, cx| {
            worker.await;
            let _ = view.update_in(cx, |_, window, _| window.remove_window());
        })
        .detach();
        cx.notify();
    }

    fn start_rename(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.navigation.session(id) else {
            return;
        };
        let name = session.name.clone();
        let id = id.to_owned();
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_value(name, window, cx);
            input.select_all(window, cx);
            input
        });
        input.read(cx).focus_handle(cx).focus(window, cx);
        let subscription = cx.subscribe_in(&input, window, |app, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                app.commit_rename(window, cx);
            }
        });
        self.editing = Some(Rename {
            id,
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.editing.take() else {
            return;
        };
        let name = edit.input.read(cx).value();
        if !name.trim().is_empty() {
            if let Err(error) = self.navigation.rename(&edit.id, &name) {
                self.failure = Some(error);
            }
        }
        self.focus_terminal(window, cx);
        cx.notify();
    }

    /// Ask Git what a worktree of `repo_id` would be created from.
    ///
    /// The dialog opens first with the branch it suggests, so the answer is a
    /// line inside it rather than a spinner over the whole window. A repository
    /// with no resolvable default branch keeps the dialog open and says so: the
    /// starting point is the one thing that cannot be guessed.
    fn open_worktree_dialog(&mut self, repo_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing || self.worktree.is_some() {
            return;
        }
        let Some(repo) = self
            .navigation
            .repos
            .iter()
            .find(|repo| repo.id == repo_id)
            .cloned()
        else {
            return;
        };
        if repo.kind != RepoKind::Git || repo.checkouts.iter().any(|item| item.is_missing) {
            self.failure = Some("Worktrees need an available Git repository.".into());
            cx.notify();
            return;
        }
        let mut field = |value: &str, cx: &mut App| {
            cx.new(|cx| {
                let mut input = InputState::new(window, cx);
                input.set_value(value.to_owned(), window, cx);
                input.select_all(window, cx);
                input
            })
        };
        let task = field("new-task", cx);
        let branch = field("feature/new-task", cx);
        let base = field("", cx);
        let subscriptions = vec![
            cx.subscribe_in(&task, window, |app, _, event, window, cx| match event {
                InputEvent::Change => app.sync_worktree_branch(window, cx),
                InputEvent::PressEnter {
                    secondary: false,
                    shift: false,
                } => app.submit_worktree(window, cx),
                _ => {}
            }),
            cx.subscribe_in(&branch, window, |app, _, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    if let Some(draft) = app.worktree.as_mut() {
                        if !draft.syncing_branch {
                            draft.branch_edited = true;
                        }
                        cx.notify();
                    }
                }
            }),
        ];
        task.read(cx).focus_handle(cx).focus(window, cx);
        let repo_id = repo_id.to_owned();
        self.worktree = Some(WorktreeDraft {
            repo_id: repo_id.clone(),
            task,
            branch,
            base,
            location: None,
            branch_edited: false,
            syncing_branch: false,
            busy: false,
            error: None,
            _subscriptions: subscriptions,
        });
        let worker = cx
            .background_executor()
            .spawn(async move { navigation::worktree_defaults(&repo) });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update_in(cx, |app, window, cx| {
                // A closed dialog answers nothing; a reopened one is a different
                // conversation with its own repository.
                let Some(draft) = app.worktree.as_mut().filter(|d| d.repo_id == repo_id) else {
                    return;
                };
                match result {
                    Ok(defaults) => {
                        let entity = draft.base.clone();
                        let name = defaults.default_branch.clone();
                        entity.update(cx, |input, cx| input.set_value(name, window, cx));
                        draft.location = Some(defaults.location);
                    }
                    Err(_) => {
                        // Git would not name a default branch. That is a question
                        // for the person, not a failure: the dialog keeps the
                        // field and waits for it.
                        draft.error = None;
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Follow the task name into the branch, until the person takes the branch
    /// over by typing in it. Editing it is not reversible here: a rename would
    /// silently move a branch the person had already chosen.
    fn sync_worktree_branch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.worktree.as_ref() else {
            return;
        };
        if draft.branch_edited || draft.busy {
            return;
        }
        let suggested = format!("feature/{}", Self::slug_for(&draft.task.read(cx).value()));
        let entity = draft.branch.clone();
        if let Some(draft) = self.worktree.as_mut() {
            draft.syncing_branch = true;
        }
        entity.update(cx, |input, cx| input.set_value(suggested, window, cx));
        if let Some(draft) = self.worktree.as_mut() {
            draft.syncing_branch = false;
        }
    }

    /// Create the worktree on a worker and open a shell in it.
    ///
    /// Git's mutation has no deadline and no rollback: a failure that arrives
    /// after the branch and the directory exist says so and leaves them, because
    /// deleting a branch that now holds someone's commits is worse than a row
    /// the person has to reopen.
    fn submit_worktree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.worktree.as_ref() else {
            return;
        };
        if draft.busy {
            return;
        }
        let Some(repo) = self
            .navigation
            .repos
            .iter()
            .find(|repo| repo.id == draft.repo_id)
            .cloned()
        else {
            return;
        };
        let task = draft.task.read(cx).value().trim().to_owned();
        let branch = draft.branch.read(cx).value().trim().to_owned();
        let typed_base = draft.base.read(cx).value().trim().to_owned();
        let base = typed_base;
        if task.is_empty() || branch.is_empty() || base.is_empty() {
            if let Some(draft) = self.worktree.as_mut() {
                draft.error =
                    Some("Enter a task name, a new branch, and the branch it starts from.".into());
                cx.notify();
            }
            return;
        }
        if let Some(draft) = self.worktree.as_mut() {
            draft.busy = true;
            draft.error = None;
        }
        let worker = cx
            .background_executor()
            .spawn(async move { navigation::create_worktree(&repo, &task, &branch, &base) });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update_in(cx, |app, window, cx| {
                let mut created = None;
                if let Some(draft) = app.worktree.as_mut() {
                    draft.busy = false;
                    match result {
                        Ok(opened) => created = Some(opened),
                        Err(error) => draft.error = Some(error),
                    }
                }
                if let Some(opened) = created {
                    let checkout = opened.checkout_id.clone();
                    app.worktree = None;
                    app.membership_revision += 1;
                    app.navigation.merge(opened, true);
                    app.persist();
                    app.request_terminal(&checkout, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The worktree dialog, drawn over the window it belongs to.
    fn worktree_dialog(&self, draft: &WorktreeDraft, cx: &mut Context<Self>) -> AnyElement {
        let ready = draft.location.is_some() && !draft.busy;
        // The content builder is called again on every repaint, so it holds the
        // three input entities and builds the fields itself rather than closing
        // over elements it would have to hand out twice.
        let task = draft.task.clone();
        let branch = draft.branch.clone();
        let base = draft.base.clone();
        let origin = draft
            .location
            .as_ref()
            .map(|location| format!("Worktrees are created under {}", location.display()));
        let error = draft.error.clone();
        let cancel_button = cx.weak_entity();
        let cancel_dialog = cx.weak_entity();
        let submit_button = cx.weak_entity();
        Dialog::new(cx)
            .width(px(440.))
            .title("Create worktree")
            .content(move |content: DialogContent, _, _| {
                let mut content = content
                    .child(div().text_size(px(12.)).child("Task name"))
                    .child(Input::new(&task).aria_label("Task name").w_full())
                    // Legacy says this under the field, and the native dialog
                    // had dropped it: the task name is the directory on disk,
                    // and a person who does not know that will not predict the
                    // path their worktree lands in.
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(gpui_kit::rgb(0x8b909c))
                            .child("Used as directory name; unsafe characters are cleaned by Git."),
                    )
                    .child(div().mt_3().text_size(px(12.)).child("New branch"))
                    .child(Input::new(&branch).aria_label("New branch").w_full())
                    .child(
                        div()
                            .mt_3()
                            .text_size(px(12.))
                            .child("Starting from branch"),
                    )
                    .child(
                        Input::new(&base)
                            .aria_label("Starting from branch")
                            .w_full(),
                    );
                if let Some(origin) = origin.clone() {
                    content = content.child(
                        div()
                            .mt_2()
                            .text_size(px(11.))
                            .text_color(gpui_kit::rgb(0x8b909c))
                            .child(origin),
                    );
                }
                if let Some(error) = error.clone() {
                    content = content.child(
                        div()
                            .mt_2()
                            .text_size(px(12.))
                            .text_color(gpui_kit::rgb(0xd97f84))
                            .child(error),
                    );
                }
                content
            })
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new("worktree-cancel")
                            .ghost()
                            .label("Cancel")
                            .on_click(move |_, _window, cx| {
                                let _ = cancel_button.update(cx, |app, cx| {
                                    close_worktree_dialog(app, cx);
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        Button::new("worktree-create")
                            .primary()
                            .label(if draft.busy {
                                "Creating…"
                            } else {
                                "Create and open shell"
                            })
                            .disabled(!ready)
                            .on_click(move |_, _window, cx| {
                                let _ = submit_button.update_in(cx, |app, window, cx| {
                                    app.submit_worktree(window, cx)
                                });
                            }),
                    ),
            )
            .on_cancel(move |_, _, cx| {
                let _ = cancel_dialog.update(cx, |app, cx| {
                    close_worktree_dialog(app, cx);
                    cx.notify();
                });
                false
            })
            .into_any_element()
    }

    fn move_terminal(
        &mut self,
        id: &str,
        target: &str,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cross_checkout = self
            .navigation
            .session(id)
            .is_some_and(|session| session.checkout_id != target);
        if let Err(error) = self.navigation.move_session(id, target, index) {
            self.failure = Some(error);
        } else {
            if cross_checkout {
                self.notice = Some("Terminal moved. Its process and working directory were preserved; no cd was sent.".into());
            }
            self.persist();
            self.focus_terminal(window, cx);
        }
        cx.notify();
    }

    fn archive(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Err(error) = self.navigation.archive(id) {
            self.failure = Some(error);
        } else {
            self.membership_revision += 1;
            self.persist();
        }
        cx.notify();
    }

    fn restore_archived(&mut self, repo_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.navigation.repos.iter().find(|repo| repo.id == repo_id) else {
            return;
        };
        let ids = repo
            .checkouts
            .iter()
            .filter(|checkout| self.navigation.archived.contains(&checkout.id))
            .map(|checkout| checkout.id.clone())
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return;
        }
        let message = format!("Restore {} archived worktree(s)?", ids.len());
        let prompt = window.prompt(
            PromptLevel::Info,
            &message,
            Some("Their rows return to the sidebar. Nothing on disk changes."),
            &["Cancel", "Restore"],
            cx,
        );
        cx.spawn_in(window, async move |view, cx| {
            if prompt.await == Ok(1) {
                let _ = view.update(cx, |app, cx| {
                    for id in ids {
                        app.navigation.archived.remove(&id);
                    }
                    app.membership_revision += 1;
                    app.persist();
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn remove_repo(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.navigation.repos.iter().find(|repo| repo.id == id) else {
            return;
        };
        if repo
            .checkouts
            .iter()
            .any(|checkout| self.navigation.home.as_deref() == Some(&checkout.id))
        {
            return;
        }
        if repo.checkouts.iter().any(|checkout| {
            !checkout.sessions.is_empty()
                || self
                    .navigation
                    .pending
                    .values()
                    .any(|pending| pending == &checkout.id)
        }) {
            self.failure = Some("Close this repository's terminal sessions before removing it from the panel. No files have been deleted.".into());
            cx.notify();
            return;
        }
        let message = format!("Remove “{}” from the panel?", repo.root);
        let prompt = window.prompt(
            PromptLevel::Warning,
            &message,
            Some("No files will be deleted. Opening the directory again brings it back."),
            &["Cancel", "Remove from panel"],
            cx,
        );
        let id = id.to_owned();
        cx.spawn_in(window, async move |view, cx| {
            if prompt.await == Ok(1) {
                let _ = view.update(cx, |app, cx| {
                    // Revalidate after the prompt: a terminal may have opened while
                    // this confirmation was outstanding.
                    let safe = app.navigation.repos.iter().find(|repo| repo.id == id).is_some_and(|repo|
                        repo.checkouts.iter().all(|checkout| checkout.sessions.is_empty() && !app.navigation.pending.values().any(|pending| pending == &checkout.id)));
                    if safe {
                        let removed = app.navigation.repos.iter().find(|repo| repo.id == id).map(|repo| repo.checkouts.iter().map(|checkout| checkout.id.clone()).collect::<Vec<_>>()).unwrap_or_default();
                        app.navigation.repos.retain(|repo| repo.id != id);
                        for checkout in &removed { app.navigation.archived.remove(checkout); }
                        if app.navigation.active_checkout.as_ref().is_some_and(|checkout| removed.contains(checkout)) {
                            app.navigation.active_checkout = None; app.navigation.active_session = None;
                        }
                        app.navigation.revision += 1;
                        app.membership_revision += 1; app.persist();
                    } else { app.failure = Some("The repository now has a terminal session. Close it before removing the directory.".into()); }
                    cx.notify();
                });
            }
        }).detach();
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" && (self.editing.is_some() || self.worktree.is_some()) {
            if let Some(draft) = self.worktree.as_ref() {
                // Escape is not a way to make a half-made worktree disappear.
                if draft.busy {
                    self.failure =
                        Some("This worktree is still being created. Wait for it to finish.".into());
                    cx.notify();
                    return;
                }
            }
            self.editing = None;
            self.worktree = None;
            cx.stop_propagation();
            self.focus_terminal(window, cx);
            cx.notify();
            return;
        }
        let Some(shortcut) = shortcut(&event.keystroke) else {
            return;
        };
        if self.opening || self.closing || self.injected {
            return;
        }
        cx.stop_propagation();
        match shortcut {
            Shortcut::Panels => {
                self.navigation.panels_visible = !self.navigation.panels_visible;
                self.focus_terminal(window, cx);
            }
            Shortcut::Terminal => {
                if let Some(id) = self
                    .navigation
                    .active_checkout
                    .clone()
                    .or_else(|| self.navigation.home.clone())
                {
                    self.request_terminal(&id, window, cx);
                }
            }
        }
        cx.notify();
    }

    /// What a checkout's trailing slot says, or nothing at all.
    ///
    /// `None` both for a checkout nobody counted and for one counted clean,
    /// because the two say different things and the row can only stay silent
    /// about both: a `+0`/`−0` would claim a measurement nobody made, and the
    /// row is where a person looks to learn whether Git was asked at all.
    fn line_slot(counts: Option<&DiffTotals>) -> Option<(u64, u64)> {
        let totals = counts?;
        (totals.additions + totals.deletions > 0).then_some((totals.additions, totals.deletions))
    }

    /// A task name as the directory and branch it suggests.
    ///
    /// Legacy's rule, kept rather than invented: letters, digits, `_`, `.` and
    /// `-` survive, everything else collapses to one `-`, and a name that
    /// reduces to nothing falls back to `new-task` rather than creating a
    /// worktree called `.worktrees/`.
    fn slug_for(value: &str) -> String {
        let mut slug = String::new();
        let mut separator = false;
        for character in value.trim().chars() {
            if character.is_alphanumeric() || matches!(character, '_' | '.' | '-') {
                slug.push(character);
                separator = false;
            } else if !separator {
                slug.push('-');
                separator = true;
            }
        }
        let slug = slug.trim_matches('-').trim_start_matches('.');
        if slug.is_empty() {
            "new-task".into()
        } else {
            slug.to_owned()
        }
    }

    fn repo_menu(
        &self,
        id: String,
        view: gpui_kit::WeakEntity<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu {
        let repo = self
            .navigation
            .repos
            .iter()
            .find(|repo| repo.id == id)
            .expect("drawn repository");
        let archived = repo
            .checkouts
            .iter()
            .filter(|checkout| self.navigation.archived.contains(&checkout.id))
            .count();
        // A missing directory has no repository to ask about, and adding a
        // worktree is the one action that has to be a click away rather than a
        // click and a dialog further.
        let can_add =
            repo.kind == RepoKind::Git && !repo.checkouts.iter().any(|item| item.is_missing);
        let can_close = !repo
            .checkouts
            .iter()
            .any(|checkout| self.navigation.home.as_deref() == Some(&checkout.id));
        move |mut menu, _, _| {
            if can_add {
                let id = id.clone();
                let view = view.clone();
                menu = menu.item(PopupMenuItem::new("New worktree").on_click(
                    move |_, _window, cx| {
                        let _ = view.update_in(cx, |app, window, cx| {
                            app.open_worktree_dialog(&id, window, cx)
                        });
                    },
                ));
            }
            if archived > 0 {
                let id = id.clone();
                let view = view.clone();
                menu = menu.item(
                    PopupMenuItem::new(format!("Restore {archived} archived worktree(s)"))
                        .on_click(move |_, window, cx| {
                            let _ =
                                view.update(cx, |app, cx| app.restore_archived(&id, window, cx));
                        }),
                );
            }
            if can_close {
                let id = id.clone();
                let view = view.clone();
                menu = menu.separator().item(
                    PopupMenuItem::element(|_, _| {
                        div()
                            .text_color(gpui_kit::rgb(0xf07178))
                            .child("Remove from panel")
                    })
                    .on_click(move |_, window, cx| {
                        let _ = view.update(cx, |app, cx| app.remove_repo(&id, window, cx));
                    }),
                );
            }
            menu
        }
    }

    fn terminal_menu(
        &self,
        id: String,
        view: gpui_kit::WeakEntity<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu {
        let session = self.navigation.session(&id).expect("drawn session");
        let source = session.checkout_id.clone();
        let sessions = &self
            .navigation
            .checkout(&source)
            .expect("session checkout")
            .sessions;
        let index = sessions
            .iter()
            .position(|session| session.id == id)
            .unwrap_or(0);
        let count = sessions.len();
        let destinations = self
            .navigation
            .repos
            .iter()
            .flat_map(|repo| repo.checkouts.iter().map(move |checkout| (repo, checkout)))
            .filter(|(_, checkout)| {
                !checkout.is_missing
                    && !self.navigation.archived.contains(&checkout.id)
                    && checkout.id != source
            })
            .map(|(repo, checkout)| {
                (
                    checkout.id.clone(),
                    format!("Move to {} / {}", repo.name, checkout_title(repo, checkout)),
                    checkout.sessions.len(),
                )
            })
            .collect::<Vec<_>>();
        move |mut menu, _, _| {
            let rename_id = id.clone();
            let rename_view = view.clone();
            menu = menu.item(PopupMenuItem::new("Rename terminal").on_click(
                move |_, window, cx| {
                    let _ =
                        rename_view.update(cx, |app, cx| app.start_rename(&rename_id, window, cx));
                },
            ));
            for (label, target_index, available) in [
                ("Move up", index.saturating_sub(1), index > 0),
                ("Move down", index + 1, index + 1 < count),
            ] {
                let id = id.clone();
                let source = source.clone();
                let view = view.clone();
                menu = menu.item(PopupMenuItem::new(label).disabled(!available).on_click(
                    move |_, window, cx| {
                        let _ = view.update(cx, |app, cx| {
                            app.move_terminal(&id, &source, target_index, window, cx)
                        });
                    },
                ));
            }
            for (target, label, index) in &destinations {
                let id = id.clone();
                let target = target.clone();
                let index = *index;
                let view = view.clone();
                menu = menu.item(PopupMenuItem::new(label.clone()).on_click(
                    move |_, window, cx| {
                        let _ = view.update(cx, |app, cx| {
                            app.move_terminal(&id, &target, index, window, cx)
                        });
                    },
                ));
            }
            let id = id.clone();
            let view = view.clone();
            menu.separator()
                .item(
                    PopupMenuItem::new("Close terminal").on_click(move |_, window, cx| {
                        let _ =
                            view.update(cx, |app, cx| app.request_close_terminal(&id, window, cx));
                    }),
                )
        }
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.weak_entity();
        let mut rows = Vec::<AnyElement>::new();
        for repo in &self.navigation.repos {
            let id = repo.id.clone();
            let folded = self.navigation.folded_repos.contains(&id);
            let toggle_id = id.clone();
            let has_menu = repo.kind == RepoKind::Git
                && !repo.checkouts.iter().any(|checkout| checkout.is_missing)
                || repo
                    .checkouts
                    .iter()
                    .any(|checkout| self.navigation.archived.contains(&checkout.id))
                || !repo
                    .checkouts
                    .iter()
                    .any(|checkout| self.navigation.home.as_deref() == Some(&checkout.id));
            let heading = div()
                .id(SharedString::from(format!("heading-{id}")))
                .group(SharedString::from(format!("repo-heading-{id}")))
                .flex()
                .items_baseline()
                .w_full()
                .pl(px(10.))
                .pr(px(8.))
                .pt(px(8.))
                .pb(px(4.))
                .child(
                    Button::new(SharedString::from(format!("fold-repo-{id}")))
                        .ghost()
                        .small()
                        .compact()
                        .rounded(px(0.))
                        .flex_1()
                        .min_w_0()
                        .justify_start()
                        .h(px(22.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_baseline()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(14.))
                                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                                        .child(repo.name.clone()),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .text_size(px(11.))
                                        .text_color(gpui_kit::rgb(0x8b909c))
                                        .child(repo.root.clone()),
                                ),
                        )
                        .aria_expanded(!folded)
                        .tooltip(repo.root.clone())
                        .accessibility_label(format!(
                            "{} repository {}",
                            if folded { "Expand" } else { "Collapse" },
                            repo.name
                        ))
                        .on_click(cx.listener(move |app, _, _, cx| {
                            toggle(&mut app.navigation.folded_repos, &toggle_id);
                            cx.notify();
                        })),
                )
                .when(has_menu, |heading| {
                    heading.child(
                        Button::new(SharedString::from(format!("actions-{id}")))
                            .opacity(0.)
                            .group_hover(
                                SharedString::from(format!("repo-heading-{id}")),
                                |style| style.opacity(1.),
                            )
                            .focus(|style| style.opacity(1.))
                            .ghost()
                            .small()
                            .compact()
                            .icon(IconName::Ellipsis)
                            .accessibility_label(format!("Actions for {}", repo.name))
                            .dropdown_menu(self.repo_menu(id.clone(), weak.clone())),
                    )
                });
            rows.push(
                heading
                    .context_menu(self.repo_menu(id, weak.clone()))
                    .into_any_element(),
            );
            if folded {
                rows.push(div().h(px(8.)).into_any_element());
                continue;
            }
            for checkout in &repo.checkouts {
                if self.navigation.archived.contains(&checkout.id) {
                    continue;
                }
                let title = checkout_title(repo, checkout);
                let folded = self.navigation.folded_checkouts.contains(&checkout.id);
                let id = checkout.id.clone();
                let fold_id = id.clone();
                let select_id = id.clone();
                let selected = self.navigation.active_checkout.as_deref() == Some(&id)
                    && self.navigation.active_session.is_none();
                let icon = if checkout.is_missing {
                    IconName::TriangleAlert
                } else if self.navigation.home.as_deref() == Some(&id) {
                    IconName::House
                } else if repo.kind == RepoKind::Plain {
                    IconName::Folder
                } else {
                    IconName::GitBranch
                };
                let row = div()
                    .id(SharedString::from(format!("checkout-row-{id}")))
                    .flex()
                    .items_center()
                    .h(px(26.))
                    .gap(px(7.))
                    .px(px(8.))
                    .hover(|style| style.bg(gpui_kit::rgb(0x2d323c)))
                    .when(selected, |row| {
                        row.bg(gpui_kit::rgb(0x2c3b57))
                            .border_l_2()
                            .border_color(gpui_kit::rgb(0x5b97ff))
                    })
                    .child(
                        Button::new(SharedString::from(format!("fold-checkout-{id}")))
                            .ghost()
                            .small()
                            .compact()
                            .rounded(px(0.))
                            .h(px(26.))
                            .w(px(20.))
                            .icon(if folded {
                                IconName::ChevronRight
                            } else {
                                IconName::ChevronDown
                            })
                            .aria_expanded(!folded)
                            .accessibility_label(format!(
                                "{} {title}",
                                if folded { "Expand" } else { "Collapse" }
                            ))
                            .on_click(cx.listener(move |app, _, _, cx| {
                                toggle(&mut app.navigation.folded_checkouts, &fold_id);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("select-checkout-{id}")))
                            .ghost()
                            .small()
                            .compact()
                            .rounded(px(0.))
                            .h(px(26.))
                            .flex_1()
                            .min_w_0()
                            .icon(Icon::new(icon).size(px(14.)))
                            .justify_start()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(title.clone()),
                            )
                            .tooltip(format!("{title} — {}", checkout.path))
                            .aria_selected(selected)
                            .disabled(checkout.is_missing)
                            .on_click(cx.listener(move |app, _, window, cx| {
                                if app.navigation.select_checkout(&select_id) {
                                    app.persist();
                                    app.focus_terminal(window, cx);
                                    cx.notify();
                                }
                            })),
                    )
                    .when(repo.kind == RepoKind::Git && !checkout.is_missing, |row| {
                        match Self::line_slot(self.diff_totals.get(&id)) {
                            Some((added, removed)) => row.child(
                                div()
                                    .id(SharedString::from(format!("diff-{id}")))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .text_size(px(11.))
                                    .when(added > 0, |slot| {
                                        slot.child(
                                            div()
                                                .text_color(gpui_kit::rgb(0xa1c181))
                                                .child(format!("+{added}")),
                                        )
                                    })
                                    .when(removed > 0, |slot| {
                                        slot.child(
                                            div()
                                                .text_color(gpui_kit::rgb(0xd97f84))
                                                .child(format!("\u{2212}{removed}")),
                                        )
                                    }),
                            ),
                            None => row,
                        }
                    })
                    .when(checkout.is_missing, |row| {
                        row.child(
                            div()
                                .text_size(px(11.))
                                .text_color(gpui_kit::rgb(0xd97f84))
                                .child("Directory missing"),
                        )
                    })
                    .when(repo.kind == RepoKind::Git && !checkout.is_primary, |row| {
                        let archive_id = id.clone();
                        row.child(
                            Button::new(SharedString::from(format!("archive-{id}")))
                                .ghost()
                                .small()
                                .compact()
                                .icon(IconName::Archive)
                                .accessibility_label(format!("Archive worktree {title}"))
                                .tooltip("Archive worktree; keep every file")
                                .on_click(
                                    cx.listener(move |app, _, _, cx| app.archive(&archive_id, cx)),
                                ),
                        )
                    });
                rows.push(row.into_any_element());
                if folded || checkout.is_missing {
                    continue;
                }
                for session in &checkout.sessions {
                    let Some(terminal) = self.terminals.get(&session.id) else {
                        continue;
                    };
                    let status = terminal.shell.session.status();
                    let snapshot = terminal.shell.session.snapshot();
                    let title = session_title(
                        &session.name,
                        status.foreground.app.as_deref(),
                        snapshot.title.as_deref(),
                    );
                    let selected = self.navigation.active_session.as_deref() == Some(&session.id);
                    let busy = status.foreground.running;
                    let unknown =
                        status.poll_error.is_some() || status.shell == ShellState::Unknown;
                    let agent = status.foreground.app.as_deref() == Some("opencode");
                    let id = session.id.clone();
                    let click_id = id.clone();
                    let close_id = id.clone();
                    let content =
                        if let Some(editing) = self.editing.as_ref().filter(|edit| edit.id == id) {
                            Input::new(&editing.input)
                                .aria_label("Terminal session name")
                                .h(px(26.))
                                .into_any_element()
                        } else {
                            Button::new(SharedString::from(format!("select-session-{id}")))
                                .ghost()
                                .small()
                                .compact()
                                .rounded(px(0.))
                                .h(px(26.))
                                .w_full()
                                .min_w_0()
                                .icon(
                                    Icon::new(if agent {
                                        IconName::Sparkles
                                    } else {
                                        IconName::SquareTerminal
                                    })
                                    .size(px(14.))
                                    .text_color(
                                        gpui_kit::rgb(if unknown {
                                            0xe3b341
                                        } else if busy && !agent {
                                            0xa1c181
                                        } else {
                                            0xabb2bf
                                        }),
                                    ),
                                )
                                .justify_start()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(title.clone()),
                                )
                                .aria_selected(selected)
                                .accessibility_label(format!(
                                    "Terminal session: {title}{}",
                                    if unknown {
                                        ", state unavailable"
                                    } else if matches!(status.shell, ShellState::Exited { .. }) {
                                        ", shell exited"
                                    } else if agent {
                                        ", agent session state not identified"
                                    } else if busy {
                                        ", process running"
                                    } else {
                                        ", idle"
                                    }
                                ))
                                .tooltip(title.clone())
                                .disabled(self.closing_sessions.contains(&id))
                                .on_click(cx.listener(
                                    move |app, event: &gpui_kit::ClickEvent, window, cx| {
                                        if event.click_count() == 2 {
                                            app.start_rename(&click_id, window, cx);
                                        } else if app.navigation.select_session(&click_id) {
                                            app.persist();
                                            app.focus_terminal(window, cx);
                                            cx.notify();
                                        }
                                    },
                                ))
                                .into_any_element()
                        };
                    let row = div()
                        .id(SharedString::from(format!("session-row-{id}")))
                        .flex()
                        .items_center()
                        .ml(px(20.))
                        .pl(px(6.))
                        .pr(px(8.))
                        .hover(|style| style.bg(gpui_kit::rgb(0x2d323c)))
                        .h(px(26.))
                        .when(selected, |row| {
                            row.bg(gpui_kit::rgb(0x2c3b57))
                                .border_l_2()
                                .border_color(gpui_kit::rgb(0x5b97ff))
                        })
                        .child(div().flex_1().min_w_0().child(content))
                        .child(
                            Button::new(SharedString::from(format!("session-menu-{id}")))
                                .ghost()
                                .small()
                                .compact()
                                .icon(IconName::Ellipsis)
                                .accessibility_label(format!("Actions for terminal {title}"))
                                .dropdown_menu(self.terminal_menu(id.clone(), weak.clone())),
                        )
                        .child(
                            Button::new(SharedString::from(format!("close-session-{id}")))
                                .ghost()
                                .small()
                                .compact()
                                .icon(IconName::X)
                                .accessibility_label(format!("Close terminal session: {title}"))
                                .disabled(self.closing_sessions.contains(&id))
                                .on_click(cx.listener(move |app, _, window, cx| {
                                    app.request_close_terminal(&close_id, window, cx)
                                })),
                        );
                    rows.push(
                        row.context_menu(self.terminal_menu(id, weak.clone()))
                            .into_any_element(),
                    );
                }
                for (_, pending) in self
                    .navigation
                    .pending
                    .iter()
                    .filter(|(_, pending)| *pending == &checkout.id)
                {
                    rows.push(
                        div()
                            .ml(px(26.))
                            .h(px(26.))
                            .child(format!(
                                "Starting terminal in {}…",
                                pending.strip_prefix("checkout:").unwrap_or(pending)
                            ))
                            .into_any_element(),
                    );
                }
                let create_id = checkout.id.clone();
                rows.push(
                    div()
                        .ml(px(26.))
                        .child(
                            Button::new(SharedString::from(format!(
                                "new-terminal-{}",
                                checkout.id
                            )))
                            .ghost()
                            .small()
                            .compact()
                            .rounded(px(0.))
                            .h(px(26.))
                            .w_full()
                            .icon(IconName::Plus)
                            .label("New terminal")
                            .accessibility_label(format!("New terminal for {title}"))
                            .on_click(cx.listener(
                                move |app, _, window, cx| {
                                    app.request_terminal(&create_id, window, cx)
                                },
                            )),
                        )
                        .into_any_element(),
                );
            }
            rows.push(div().h(px(8.)).into_any_element());
        }
        if rows.is_empty() {
            rows.push(
                div()
                    .p_3()
                    .child("Your opened folders will appear here.")
                    .into_any_element(),
            );
        }
        div()
            .id("navigation-panel")
            .h_full()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .min_h_0()
            .text_size(px(13.))
            .bg(gpui_kit::rgb(0x1c1f26))
            .child(
                div()
                    .id("navigation-scroll")
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .w_full()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(rows)
                    .vertical_scrollbar(&self.scroll),
            )
            .child(
                div()
                    .border_t_1()
                    .border_color(gpui_kit::rgb(0x2c313b))
                    .p_1()
                    .child(
                        Button::new("open-directory")
                            .ghost()
                            .small()
                            .compact()
                            .rounded(px(0.))
                            .w_full()
                            .h(px(26.))
                            .icon(IconName::FolderPlus)
                            .label("Open directory")
                            .accessibility_label("Open directory")
                            .disabled(self.opening || self.closing)
                            .on_click(
                                cx.listener(|app, _, window, cx| app.choose_directory(window, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }
}

impl Render for TerminalWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.focus_pending {
            self.focus_terminal(window, cx);
        }
        let background = self.palette.background(ColorSnapshot::Default);
        let text = self.palette.foreground(ColorSnapshot::Default);
        let title = self
            .terminal()
            .map(|terminal| {
                terminal
                    .read(cx)
                    .session()
                    .snapshot()
                    .title
                    .unwrap_or_else(|| "Terminal".into())
            })
            .or_else(|| {
                self.navigation
                    .active_checkout
                    .as_deref()
                    .and_then(|id| self.navigation.checkout(id))
                    .map(|checkout| checkout.path.clone())
            })
            .unwrap_or_else(|| "Muster".into());
        let body: AnyElement = if let Some(terminal) = self.terminal() {
            terminal.clone().into_any_element()
        } else {
            let available = self
                .navigation
                .active_checkout
                .as_deref()
                .and_then(|id| self.navigation.checkout(id))
                .is_some_and(|checkout| !checkout.is_missing);
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .child(if self.opening {
                    "Opening workspace…"
                } else if available {
                    "No terminal is open in this directory."
                } else {
                    "Open a directory to get started."
                })
                .child(
                    Button::new("empty-action")
                        .label(if available {
                            "New terminal"
                        } else {
                            "Open directory"
                        })
                        .disabled(self.opening || self.closing)
                        .on_click(cx.listener(move |app, _, window, cx| {
                            if available {
                                if let Some(id) = app.navigation.active_checkout.clone() {
                                    app.request_terminal(&id, window, cx);
                                }
                            } else {
                                app.choose_directory(window, cx);
                            }
                        })),
                )
                .into_any_element()
        };
        let body = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(body)
            .into_any_element();
        let content = if !self.injected && self.navigation.panels_visible {
            let sidebar = self.sidebar(cx);
            h_resizable("workspace-panels")
                .with_state(&self.resize)
                .child(
                    resizable_panel()
                        .flex_none()
                        .size(px(self.navigation.sidebar_width))
                        .size_range(px(240.)..px(500.))
                        .child(sidebar),
                )
                .child(resizable_panel().child(body))
                .on_resize(cx.listener(|app, state: &Entity<ResizableState>, _, cx| {
                    if let Some(width) = state.read(cx).sizes().first() {
                        app.navigation.sidebar_width = width.as_f32().clamp(240., 500.);
                        app.persist();
                        cx.notify();
                    }
                }))
                .into_any_element()
        } else {
            body
        };
        let dialog = self
            .worktree
            .as_ref()
            .map(|draft| self.worktree_dialog(draft, cx));
        div()
            .id("workspace-root")
            .size_full()
            .flex()
            .flex_col()
            .bg(background)
            .text_color(text)
            .capture_key_down(cx.listener(Self::on_key))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_shrink_0()
                    .h(px(22.))
                    .px_2()
                    .overflow_hidden()
                    .text_xs()
                    .child(title)
                    .when(!self.injected, |row| {
                        row.child(
                            Button::new("toggle-panels")
                                .ghost()
                                .small()
                                .compact()
                                .h(px(20.))
                                .icon(IconName::PanelLeftClose)
                                .accessibility_label("Toggle side panels")
                                .tooltip("Toggle side panels — Cmd/Ctrl+/")
                                .on_click(cx.listener(|app, _, window, cx| {
                                    app.navigation.panels_visible = !app.navigation.panels_visible;
                                    app.focus_terminal(window, cx);
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .when_some(
                self.failure.clone().or_else(|| self.notice.clone()),
                |root, message| {
                    root.child(
                        div()
                            .flex()
                            .items_center()
                            .px_2()
                            .text_size(px(12.))
                            .text_color(gpui_kit::rgb(0xe3b341))
                            .child(div().flex_1().child(message))
                            .child(
                                Button::new("dismiss-message")
                                    .ghost()
                                    .small()
                                    .compact()
                                    .icon(IconName::X)
                                    .accessibility_label("Dismiss message")
                                    .on_click(cx.listener(|app, _, _, cx| {
                                        app.failure = None;
                                        app.notice = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                },
            )
            .child(content)
            .when_some(dialog, |root, dialog| root.child(dialog))
    }
}
impl Drop for TerminalWindow {
    fn drop(&mut self) {
        if !self.injected {
            self.owner.stop();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shortcut {
    Panels,
    Terminal,
}
fn shortcut(key: &gpui_kit::Keystroke) -> Option<Shortcut> {
    let app_modifier = if cfg!(target_os = "macos") {
        key.modifiers.platform && !key.modifiers.control
    } else {
        key.modifiers.control && !key.modifiers.platform
    };
    if !app_modifier || key.modifiers.alt {
        return None;
    }
    match key.key.to_lowercase().as_str() {
        "/" | "slash" => Some(Shortcut::Panels),
        "7" if key.modifiers.shift => Some(Shortcut::Panels),
        "n" => Some(Shortcut::Terminal),
        _ => None,
    }
}
fn toggle(set: &mut std::collections::HashSet<String>, id: &str) {
    if !set.remove(id) {
        set.insert(id.into());
    }
}

/// Give up on the worktree dialog, unless Git is already making one.
///
/// A creation that has reached Git owns a branch and a directory; cancelling
/// the form cannot undo either, and pretending it did is how a person loses a
/// worktree they were told had not been made.
fn close_worktree_dialog(app: &mut TerminalWindow, cx: &mut Context<TerminalWindow>) {
    match app.worktree.as_mut() {
        Some(draft) if draft.busy => {
            draft.error =
                Some("This worktree is still being created. Wait for it to finish.".into());
        }
        Some(_) => app.worktree = None,
        None => {}
    }
    cx.notify();
}

struct RestoredNavigation {
    navigation: Navigation,
    state_path: Option<PathBuf>,
    errors: Vec<String>,
}
fn restore_navigation(cwd: &Path) -> RestoredNavigation {
    let mut errors = Vec::new();
    let state_path = navigation::state_path()
        .map_err(|error| errors.push(error))
        .ok();
    let state = state_path
        .as_ref()
        .and_then(|path| {
            navigation::load(path)
                .map_err(|error| errors.push(error))
                .ok()
        })
        .unwrap_or_default();
    let mut nav = Navigation::new();
    nav.sidebar_width = state.sidebar_width;
    nav.archived = state.archived_checkout_ids.into_iter().collect();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        match Repo::plain(&home, "startup") {
            Ok(mut repo) => {
                repo.name = "Home".into();
                nav.home = Some(repo.checkouts[0].id.clone());
                nav.repos.push(repo);
            }
            Err(error) => errors.push(error),
        }
    }
    for root in &state.roots {
        if nav.repos.iter().any(|repo| &repo.root == root) {
            continue;
        }
        match navigation::open_directory(Path::new(root)) {
            Ok(opened) => nav.merge(opened, false),
            Err(error) => {
                errors.push(error);
                // Keep a missing registration available for removal instead of
                // silently erasing the saved directory on the next save.
                let id = muster_core::workspace::repo_id_for_path(root);
                let checkout_id = muster_core::workspace::checkout_id_for_path(root);
                nav.repos.push(Repo {
                    id: id.clone(),
                    kind: RepoKind::Plain,
                    name: Path::new(root)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| root.clone()),
                    root: root.clone(),
                    default_branch: None,
                    created_at: "startup".into(),
                    last_opened_at: "startup".into(),
                    checkouts: vec![muster_core::workspace::Checkout {
                        id: checkout_id,
                        repo_id: id,
                        path: root.clone(),
                        canonical_path: root.clone(),
                        is_primary: true,
                        branch: None,
                        head: None,
                        ahead_of_default: None,
                        changed_files: 0,
                        is_missing: true,
                        sessions: Vec::new(),
                    }],
                });
            }
        }
    }
    // The explicitly launched directory remains the first shell's cwd, as in
    // the terminal MVP. A restored selection is used when it is that directory.
    match navigation::open_directory(cwd) {
        Ok(opened) => nav.merge(opened, true),
        Err(error) => {
            errors.push(error);
            if let Some(home) = nav.home.clone() {
                nav.select_checkout(&home);
            }
        }
    }
    RestoredNavigation {
        navigation: nav,
        state_path,
        errors,
    }
}

pub fn install_quit_path(cx: &mut App) {
    cx.on_app_quit(|_| {
        let owners = OWNERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .filter_map(|owner| owner.upgrade())
            .collect::<Vec<_>>();
        for owner in owners {
            owner.stop();
        }
        async {}
    })
    .detach();
}
fn host(metrics: Arc<Mutex<CellMetrics>>) -> TerminalHost {
    TerminalHost {
        cell_metrics: Box::new(move || metrics.lock().map(|cell| *cell).unwrap_or(UNMEASURED_CELL)),
        resolve_color: Box::new(xterm_256),
        store_clipboard: Box::new(|_| {}),
    }
}
pub fn working_directory() -> PathBuf {
    directory_from(std::env::args_os())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}
fn directory_from(args: impl IntoIterator<Item = std::ffi::OsString>) -> Option<PathBuf> {
    args.into_iter()
        .skip(1)
        .map(PathBuf::from)
        .find(|path| path.is_dir())
}
pub fn window_bounds(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1280.), px(800.)),
            cx,
        ))),
        ..Default::default()
    }
}

// The original ownership regression test remains isolated from production
// window ownership. Injected views never claim or terminate somebody else's PTY.
#[cfg(test)]
static TEST_OWNER: Mutex<Option<Arc<SessionOwner>>> = Mutex::new(None);
#[cfg(test)]
fn spawn_and_claim(
    backend: Arc<TerminalBackend>,
    config: TerminalSessionConfig,
) -> Result<Arc<OwnedShell>, String> {
    let owner = SessionOwner::new(backend, true);
    let shell = owner.spawn(config)?;
    *TEST_OWNER.lock().unwrap() = Some(owner);
    Ok(shell)
}
#[cfg(test)]
fn end_shell() -> Option<Result<CloseReport, SessionError>> {
    let owner = TEST_OWNER.lock().ok()?.take()?;
    let shell = owner.state.lock().ok()?.shells.values().next()?.clone();
    let report = shell.session.close().wait();
    owner.stop();
    Some(report)
}
#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "sidebar_tests.rs"]
mod sidebar_tests;
