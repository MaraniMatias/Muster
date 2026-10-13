//! Native workspace window. Terminal entities are retained by session identity;
//! navigation and folding never own (or restart) a PTY.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc, Mutex, Weak,
};
use std::thread;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::base::Button as BaseButton;
use gpui_kit::component::{
    button::{Button, ButtonCustomVariant, ButtonVariants},
    checkbox::Checkbox,
    dialog::{Dialog, DialogContent, DialogFooter},
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem},
    resizable::{h_resizable, resizable_panel, ResizableState},
    scroll::ScrollableElement,
    Disableable, Icon, Sizable,
};
use gpui_kit::{
    canvas, div, prelude::FluentBuilder, px, size, AnyElement, App, AppContext, Bounds, Context,
    Entity, Focusable, InteractiveElement, IntoElement, KeyDownEvent, Length, MouseButton,
    MouseDownEvent, MouseExitEvent, MouseMoveEvent, MouseUpEvent, ParentElement, PathPromptOptions,
    Pixels, Point, PromptLevel, Render, ScrollHandle, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Task, TestSupportExt, Window, WindowBounds, WindowOptions,
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

/// The family the sidebar draws with, and the one its labels are measured in.
///
/// One name in both places on purpose: a path elided against a different face
/// than the one that draws it is a fit that is a guess. This is the family the
/// bundled faces declare in their own `name` table, not a browser alias.
pub(crate) const SIDEBAR_FONT_FAMILY: &str = "Fira Code";

/// Register the bundled faces on the application's text system.
///
/// Called before the first window opens, because a family that is missing when
/// the first frame is laid out is one GPUI quietly answers with a fallback.
/// The regular face and the bold face both go in: the rows set a weight.
pub fn load_fonts(cx: &App) {
    if let Err(error) = cx.text_system().add_fonts(vec![
        Cow::Borrowed(include_bytes!("../assets/fonts/FiraCode-Regular.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../assets/fonts/FiraCode-Bold.ttf").as_slice()),
    ]) {
        log::error!("muster-native: bundled sidebar fonts could not load: {error}");
    }
}

fn sidebar_text_width(window: &Window, text: &str, size: f32) -> gpui_kit::Pixels {
    let style = gpui_kit::TextStyle {
        font_family: SIDEBAR_FONT_FAMILY.into(),
        font_size: px(size).into(),
        ..Default::default()
    };
    let run = style.to_run(text.len());
    window
        .text_system()
        .shape_line(text.into(), px(size), &[run], None)
        .width()
}

/// Keep the identifying tail of a namespaced branch or session title.
fn sidebar_name(window: &Window, name: &str, room: f32) -> String {
    if sidebar_text_width(window, name, 13.).as_f32() <= room {
        return name.to_owned();
    }
    let segments = name
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let mut candidate = name.to_owned();
    for start in 1..segments.len() {
        candidate = format!("…/{}", segments[start..].join("/"));
        if sidebar_text_width(window, &candidate, 13.).as_f32() <= room {
            break;
        }
    }
    candidate
}

/// A non-tab-stop focus scope lets sibling controls reveal a row's trailing slot.
fn sidebar_row_focus(window: &mut Window, cx: &mut App, id: &str) -> (gpui_kit::FocusHandle, bool) {
    let state = window.use_keyed_state(
        SharedString::from(format!("row-focus-{id}")),
        cx,
        |window, cx| {
            let handle = cx.focus_handle().tab_index(-1);
            let enter = cx.weak_entity();
            let leave = enter.clone();
            let entered = window.on_focus_in(&handle, cx, move |_, cx| {
                let _ = enter.update(cx, |_, cx| cx.notify());
            });
            let left = window.on_focus_out(&handle, cx, move |_, _, cx| {
                let _ = leave.update(cx, |_, cx| cx.notify());
            });
            (handle, entered, left)
        },
    );
    let handle = state.read(cx).0.clone();
    let focused = handle.contains_focused(window, cx);
    (handle, focused)
}

/// The chrome a sidebar control wears.
///
/// A custom variant keeps its own hover surface — the reference draws a
/// neutral `#2d323c` behind a row action, not the accent wash a ghost button
/// paints — but it is neither text nor link, so Kit hands it the default arrow.
/// The row and the control inside it are one target, and the hand is how that
/// says so.
fn sidebar_action_style(cx: &App) -> ButtonCustomVariant {
    ButtonCustomVariant::new(cx)
        .foreground(gpui_kit::rgb(0x8b909c).into())
        .hover(gpui_kit::rgb(0x2d323c).into())
        .active(gpui_kit::rgb(0x2d323c).into())
}

/// The faint ink a sidebar control wears at rest, `--muster-text-muted`.
///
/// Set on the control rather than in a variant so the variant's hover ink can
/// win: GPUI refines a hover style after the rest style, which is what makes
/// the reference's `color: var(--muster-text)` lift happen here at all.
fn sidebar_faint_ink() -> gpui_kit::Hsla {
    gpui_kit::rgb(0x8b909c).into()
}

fn sidebar_repo_path(
    window: &Window,
    root: &str,
    name: &str,
    width: f32,
    has_menu: bool,
) -> String {
    let segments = root
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if segments.is_empty() {
        return root.to_owned();
    }
    let fixed =
        18. + sidebar_text_width(window, name, 14.).as_f32() + 8. + if has_menu { 28. } else { 0. };
    let available = px((width - fixed).max(0.));
    for count in (1..=segments.len()).rev() {
        let candidate = if count == segments.len() {
            root.to_owned()
        } else {
            format!("…/{}", segments[segments.len() - count..].join("/"))
        };
        if sidebar_text_width(window, &candidate, 11.) <= available {
            return candidate;
        }
    }
    String::new()
}
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
// expansion metadata describes the real focused control, not a second node.
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
    checkout_id: String,
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
struct WorktreeRemovalDraft {
    request_id: u64,
    repo_id: String,
    checkout_id: String,
    info: Option<muster_core::navigation::WorktreeRemovalInfo>,
    loading: bool,
    busy: bool,
    dirty_confirmed: bool,
    unmerged_confirmed: bool,
    delete_branch: bool,
    error: Option<String>,
}

struct WorktreeDraft {
    request_id: u64,
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
    loading_defaults: bool,
    busy: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

const SIDEBAR_DRAG_THRESHOLD: f32 = 5.0;
const SIDEBAR_SCROLL_EDGE: f32 = 36.0;

fn sidebar_scroll_step(distance: f32) -> f32 {
    distance.signum() * (distance.abs() / 2.).clamp(2., 14.)
}

#[derive(Clone, PartialEq, Eq)]
struct SidebarDropTarget {
    checkout_id: String,
    index: usize,
}

struct SidebarDrag {
    session_id: String,
    source_checkout: String,
    origin: (f32, f32),
    position: (f32, f32),
    started: bool,
    target: Option<SidebarDropTarget>,
}

#[derive(Clone)]
enum SidebarDropRegion {
    Session { checkout_id: String, index: usize },
    Append { checkout_id: String, index: usize },
    Insertion { checkout_id: String, index: usize },
}

struct SidebarRowBounds {
    region: SidebarDropRegion,
    bounds: Bounds<Pixels>,
}

// Record laid-out, window-local bounds so insertion targets stay correct while scrolling.
fn sidebar_drag_geometry(
    region: SidebarDropRegion,
    rows: Rc<RefCell<Vec<SidebarRowBounds>>>,
) -> AnyElement {
    canvas(
        move |bounds, _, _| rows.borrow_mut().push(SidebarRowBounds { region, bounds }),
        |_, _, _, _| {},
    )
    .absolute()
    .left(px(0.))
    .right(px(0.))
    .top(px(0.))
    .bottom(px(0.))
    .into_any_element()
}

fn sidebar_drop_marker(
    target: SidebarDropTarget,
    rows: Rc<RefCell<Vec<SidebarRowBounds>>>,
) -> AnyElement {
    div()
        .id(SharedString::from(format!(
            "drop-slot-{}-{}",
            target.checkout_id, target.index
        )))
        .relative()
        .flex()
        .items_center()
        .h(px(26.))
        .pl(px(12.))
        .gap(px(7.))
        .border_1()
        .border_dashed()
        .border_color(gpui_kit::rgb(0x5b97ff))
        .text_color(gpui_kit::rgb(0x5b97ff))
        .text_size(px(11.))
        .child(Icon::new(IconName::SquareTerminal).size(px(14.)))
        .child("Drop terminal here")
        .child(sidebar_drag_geometry(
            SidebarDropRegion::Insertion {
                checkout_id: target.checkout_id,
                index: target.index,
            },
            rows,
        ))
        .into_any_element()
}

// Window events track movement past row boundaries; MouseExitEvent cancels an abandoned gesture.
fn sidebar_drag_dispatcher(register: impl FnOnce(&mut Window, &mut App) + 'static) -> AnyElement {
    canvas(|_, _, _| (), move |_, _, window, cx| register(window, cx))
        .absolute()
        .left(px(0.))
        .right(px(0.))
        .top(px(0.))
        .bottom(px(0.))
        .into_any_element()
}

pub struct TerminalWindow {
    navigation: Navigation,
    terminals: HashMap<String, LiveTerminal>,
    /// Line counts per checkout, absent when nothing was counted yet.
    ///
    /// Absent means "not counted", never "no changes": a checkout Git could
    /// not be asked about draws nothing rather than a confident zero.
    diff_totals: HashMap<String, DiffTotals>,
    diff_refresh_running: bool,
    diff_refresh_again: bool,
    worktree: Option<WorktreeDraft>,
    worktree_removal: Option<WorktreeRemovalDraft>,
    owner: Arc<SessionOwner>,
    failure: Option<String>,
    notice: Option<String>,
    opening: bool,
    open_repo_menu: Option<String>,
    closing: bool,
    closing_sessions: std::collections::HashSet<String>,
    removing_checkouts: HashSet<String>,
    membership_revision: u64,
    latest_terminal_request: Option<String>,
    editing: Option<Rename>,
    sidebar_drag: Option<SidebarDrag>,
    suppress_sidebar_click: bool,
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
            diff_refresh_running: false,
            diff_refresh_again: false,
            worktree: None,
            worktree_removal: None,
            owner: SessionOwner::new(backend, owns_backend),
            failure: None,
            notice: None,
            opening: false,
            open_repo_menu: None,
            closing: false,
            closing_sessions: Default::default(),
            removing_checkouts: Default::default(),
            membership_revision: 0,
            latest_terminal_request: None,
            editing: None,
            sidebar_drag: None,
            suppress_sidebar_click: false,
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
                app.refresh_diff_totals(cx);
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
                checkout_id: String::new(),
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
        if self.removing_checkouts.contains(checkout_id) {
            self.failure =
                Some("This worktree is being removed. Wait before opening a terminal.".into());
            cx.notify();
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
                        let terminal_checkout_id = checkout_id.clone();
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
                                    checkout_id: terminal_checkout_id,
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
                                app.refresh_diff_totals(cx);
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
                let Ok((epoch, repos, closing)) = view.update(cx, |app, cx| {
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
                    if ticks.is_multiple_of(COUNT_EVERY_TICKS) {
                        app.refresh_diff_totals(cx);
                    }
                    (
                        app.membership_revision,
                        app.navigation
                            .repos
                            .iter()
                            .filter(|repo| repo.kind == RepoKind::Git)
                            .map(|repo| (repo.id.clone(), repo.root.clone()))
                            .collect::<Vec<_>>(),
                        app.closing,
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
                        let previous = app.diff_checkout_paths();
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
                        if previous != app.diff_checkout_paths() {
                            app.refresh_diff_totals(cx);
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

    fn diff_checkout_paths(&self) -> Vec<(String, String)> {
        self.navigation
            .repos
            .iter()
            .filter(|repo| repo.kind == RepoKind::Git)
            .flat_map(|repo| &repo.checkouts)
            .filter(|checkout| {
                !checkout.is_missing && !self.navigation.archived.contains(&checkout.id)
            })
            .map(|checkout| (checkout.id.clone(), checkout.path.clone()))
            .collect()
    }

    /// Selection and membership changes refresh immediately, without overlapping Git sweeps.
    fn refresh_diff_totals(&mut self, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        if self.diff_refresh_running {
            self.diff_refresh_again = true;
            return;
        }
        let paths = self.diff_checkout_paths();
        self.diff_totals
            .retain(|id, _| paths.iter().any(|(current, _)| current == id));
        if paths.is_empty() {
            return;
        }
        let revision = self.membership_revision;
        self.diff_refresh_running = true;
        let worker = cx.background_executor().spawn(async move {
            paths
                .into_iter()
                .map(|(id, path)| {
                    let total = navigation::diff_totals(Path::new(&path))
                        .map_err(|error| log::debug!("muster-native: {path}: {error}"));
                    (id, path, total)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |view, cx| {
            let totals = worker.await;
            let _ = view.update(cx, |app, cx| {
                app.diff_refresh_running = false;
                app.apply_diff_totals(revision, totals, cx);
                if std::mem::take(&mut app.diff_refresh_again)
                    || revision != app.membership_revision
                {
                    app.refresh_diff_totals(cx);
                }
            });
        })
        .detach();
    }

    fn apply_diff_totals(
        &mut self,
        revision: u64,
        totals: Vec<(String, String, Result<DiffTotals, ()>)>,
        cx: &mut Context<Self>,
    ) {
        if self.closing || self.membership_revision != revision {
            return;
        }
        for (id, path, total) in totals {
            let is_current = !self.navigation.archived.contains(&id)
                && self
                    .navigation
                    .checkout(&id)
                    .is_some_and(|checkout| !checkout.is_missing && checkout.path == path);
            if !is_current {
                continue;
            }
            match total {
                Ok(total) => {
                    self.diff_totals.insert(id, total);
                }
                Err(()) => {
                    self.diff_totals.remove(&id);
                }
            }
        }
        cx.notify();
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
            let mut input =
                InputState::new(window, cx).validate(|value, _| value.chars().count() <= 60);
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
        if self.closing || self.worktree.is_some() || self.worktree_removal.is_some() {
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
        if repo.kind != RepoKind::Git
            || !repo
                .checkouts
                .iter()
                .any(|item| item.is_primary && !item.is_missing)
        {
            self.failure = Some("Worktrees need an available primary Git checkout.".into());
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
        task.update(cx, |input, cx| {
            input.set_validator(|value, _| value.chars().count() <= 120, cx)
        });
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
            cx.subscribe_in(&branch, window, |app, _, event, window, cx| {
                if matches!(
                    event,
                    InputEvent::PressEnter {
                        secondary: false,
                        shift: false
                    }
                ) {
                    app.submit_worktree(window, cx);
                }
            }),
            cx.subscribe_in(&base, window, |app, _, event, window, cx| {
                if matches!(
                    event,
                    InputEvent::PressEnter {
                        secondary: false,
                        shift: false
                    }
                ) {
                    app.submit_worktree(window, cx);
                } else if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        task.read(cx).focus_handle(cx).focus(window, cx);
        let repo_id = repo_id.to_owned();
        let request_id = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
        self.worktree = Some(WorktreeDraft {
            request_id,
            repo_id: repo_id.clone(),
            task,
            branch,
            base,
            // `repo.root` is the canonical primary checkout from discovery.
            // Show its destination immediately, even if Git cannot guess a base branch.
            location: Some(PathBuf::from(&repo.root).join(".worktrees")),
            branch_edited: false,
            syncing_branch: false,
            loading_defaults: true,
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
                app.apply_worktree_defaults(&repo_id, request_id, result, window, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn apply_worktree_defaults(
        &mut self,
        repo_id: &str,
        request_id: u64,
        result: Result<navigation::WorktreeDefaults, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(draft) = self
            .worktree
            .as_mut()
            .filter(|draft| draft.repo_id == repo_id && draft.request_id == request_id)
        else {
            return;
        };
        draft.loading_defaults = false;
        match result {
            Ok(defaults) => {
                if draft.base.read(cx).value().trim().is_empty() {
                    let entity = draft.base.clone();
                    entity.update(cx, |input, cx| {
                        input.set_value(defaults.default_branch.clone(), window, cx)
                    });
                }
                draft.location = Some(defaults.location);
                draft.error = None;
            }
            Err(error) => {
                draft.error = Some(format!(
                    "Could not load Git defaults: {error}. Enter an existing branch or ref to continue."
                ));
            }
        }
        cx.notify();
    }

    fn open_worktree_removal_dialog(
        &mut self,
        repo_id: &str,
        checkout_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.closing || self.worktree.is_some() || self.worktree_removal.is_some() {
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
        let Some(checkout) = repo
            .checkouts
            .iter()
            .find(|item| item.id == checkout_id)
            .cloned()
        else {
            return;
        };
        if repo.kind != RepoKind::Git {
            self.failure = Some("Only a Git worktree can be archived or deleted here.".into());
            cx.notify();
            return;
        }
        if checkout.is_primary || self.navigation.home.as_deref() == Some(checkout_id) {
            self.failure =
                Some("The primary checkout and Home cannot be archived or deleted.".into());
            cx.notify();
            return;
        }
        if checkout.is_missing {
            self.failure = Some(
                "This worktree is missing. Hide it to remove it from the panel; its Git metadata and branch are retained.".into(),
            );
            cx.notify();
            return;
        }

        let request_id = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
        self.worktree_removal = Some(WorktreeRemovalDraft {
            request_id,
            repo_id: repo.id.clone(),
            checkout_id: checkout.id.clone(),
            info: None,
            loading: true,
            busy: false,
            dirty_confirmed: false,
            unmerged_confirmed: false,
            delete_branch: false,
            error: None,
        });
        let worker = cx
            .background_executor()
            .spawn(async move { navigation::worktree_removal_info(&repo, &checkout) });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update_in(cx, |app, _, cx| {
                let Some(draft) = app
                    .worktree_removal
                    .as_mut()
                    .filter(|draft| draft.request_id == request_id)
                else {
                    return;
                };
                draft.loading = false;
                match result {
                    Ok(info) => draft.info = Some(info),
                    Err(error) => draft.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn checkout_has_terminal_activity(&self, checkout_id: &str) -> bool {
        self.navigation
            .checkout(checkout_id)
            .is_some_and(|checkout| !checkout.sessions.is_empty())
            || self
                .navigation
                .pending
                .values()
                .any(|pending| pending == checkout_id)
            || self
                .terminals
                .values()
                .any(|terminal| terminal.checkout_id == checkout_id)
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
        if draft.busy || draft.loading_defaults {
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
                    app.refresh_diff_totals(cx);
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
        let ready = draft.location.is_some()
            && !draft.busy
            && !draft.loading_defaults
            && !draft.task.read(cx).value().trim().is_empty()
            && !draft.branch.read(cx).value().trim().is_empty()
            && !draft.base.read(cx).value().trim().is_empty();
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
        let loading_defaults = draft.loading_defaults;
        let context = self
            .navigation
            .repos
            .iter()
            .find(|repo| repo.id == draft.repo_id)
            .map(|repo| {
                let root = repo.checkouts.iter().find(|checkout| checkout.is_primary);
                let context = root
                    .map(|checkout| checkout.branch.as_deref().unwrap_or(&checkout.path))
                    .unwrap_or(&repo.root);
                format!("{context} · {}", repo.name)
            });
        let cancel_button = cx.weak_entity();
        let cancel_dialog = cx.weak_entity();
        let submit_button = cx.weak_entity();
        Dialog::new(cx)
            .width(px(440.))
            .title("Create worktree")
            .content(move |content: DialogContent, _, _| {
                let content = content.when_some(context.clone(), |content, context| {
                    content.child(
                        div()
                            .text_size(px(12.))
                            .text_color(gpui_kit::rgb(0x8b909c))
                            .child(context),
                    )
                });
                if loading_defaults {
                    return content.child(
                        div()
                            .py_4()
                            .text_size(px(12.))
                            .child("Checking Git worktree…"),
                    );
                }
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
                            .on_click(move |_, window, cx| {
                                let _ = submit_button
                                    .update(cx, |app, cx| app.submit_worktree(window, cx));
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

    fn worktree_removal_dialog(
        &self,
        draft: &WorktreeRemovalDraft,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = draft.checkout_id.clone();
        let request_id = draft.request_id;
        let info = draft.info.clone();
        let loading = draft.loading;
        let busy = draft.busy;
        let dirty_confirmed = draft.dirty_confirmed;
        let unmerged_confirmed = draft.unmerged_confirmed;
        let delete_branch = draft.delete_branch;
        let error = draft.error.clone();
        let checkout = self.navigation.checkout(&id);
        let repo = self
            .navigation
            .repos
            .iter()
            .find(|repo| repo.id == draft.repo_id);
        let branch = info
            .as_ref()
            .and_then(|info| info.branch.as_deref())
            .or_else(|| checkout.and_then(|checkout| checkout.branch.as_deref()))
            .unwrap_or("detached HEAD");
        let label = format!(
            "{} · {}",
            branch,
            repo.map_or("Git repository", |repo| repo.name.as_str())
        );
        let path = info
            .as_ref()
            .map(|info| info.path.clone())
            .or_else(|| checkout.map(|checkout| checkout.canonical_path.clone()))
            .unwrap_or_else(|| id.clone());
        let valid_checkout = repo.is_some_and(|repo| repo.kind == RepoKind::Git)
            && checkout.is_some_and(|checkout| {
                !checkout.is_primary
                    && !checkout.is_missing
                    && self.navigation.home.as_deref() != Some(&id)
            });
        let session_names = checkout
            .map(|checkout| {
                checkout
                    .sessions
                    .iter()
                    .map(|session| session.name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let retained_terminals = self
            .terminals
            .iter()
            .filter(|(session_id, terminal)| {
                terminal.checkout_id == id && self.navigation.session(session_id).is_none()
            })
            .map(|(session_id, _)| format!("Terminal {session_id}"))
            .collect::<Vec<_>>();
        let pending_ids = self
            .navigation
            .pending
            .iter()
            .filter(|(_, pending)| *pending == &id)
            .map(|(request, _)| request.clone())
            .collect::<Vec<_>>();
        let has_terminal = self.checkout_has_terminal_activity(&id);
        let has_pending = !pending_ids.is_empty();
        let can_archive = !busy && valid_checkout && !has_terminal;
        let can_delete = !loading
            && !busy
            && valid_checkout
            && !has_terminal
            && info.as_ref().is_some_and(|info| {
                (info.dirty_files.is_empty() || dirty_confirmed)
                    && (info.branch.is_some() || info.unmerged_commits == 0 || unmerged_confirmed)
            });
        let shell_checkout_id = id.clone();
        let open_shell = cx.weak_entity();
        let toggle_dirty = cx.weak_entity();
        let toggle_unmerged = cx.weak_entity();
        let toggle_branch = cx.weak_entity();
        let archive = cx.weak_entity();
        let delete = cx.weak_entity();
        let close_for_cancel = cx.weak_entity();
        let close_for_escape = cx.weak_entity();
        Dialog::new(cx)
            .width(px(520.))
            .title("Archive or delete worktree")
            .content(move |mut content: DialogContent, _, _| {
                content = content
                    .child(
                        div()
                            .text_size(px(12.))
                            .child("Archive hides this worktree, keeps every file and branch, and does not run Git. Delete removes this selected worktree directory and its Git registration."),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_size(px(11.))
                            .text_color(gpui_kit::rgb(0x8b909c))
                            .child(format!("{label} · {}", escape_git_path(&path))),
                    );
                if loading {
                    content = content.child(
                        div()
                            .mt_3()
                            .text_size(px(12.))
                            .text_color(gpui_kit::rgb(0x8b909c))
                            .child("Checking Git worktree…"),
                    );
                }
                if let Some(info) = info.clone() {
                    if info.dirty_files.is_empty() {
                        content = content.child(
                            div()
                                .mt_3()
                                .text_size(px(12.))
                                .text_color(gpui_kit::rgb(0x8b909c))
                                .child("No uncommitted files."),
                        );
                    } else {
                        let toggle_dirty = toggle_dirty.clone();
                        let mut preview = div()
                            .mt_3()
                            .p_2()
                            .bg(gpui_kit::rgb(0x252932))
                            .text_size(px(12.))
                            .child(format!(
                                "{} uncommitted file(s) may be lost if you delete this worktree:",
                                info.dirty_files.len()
                            ));
                        for file in info.dirty_files.iter().take(8) {
                            preview = preview.child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(gpui_kit::rgb(0x8b909c))
                                    .child(escape_git_path(file)),
                            );
                        }
                        if info.dirty_files.len() > 8 {
                            preview = preview.child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(gpui_kit::rgb(0x8b909c))
                                    .child(format!("and {} more…", info.dirty_files.len() - 8)),
                            );
                        }
                        let open_shell = open_shell.clone();
                        let shell_checkout_id = shell_checkout_id.clone();
                        content = content
                            .child(preview)
                            .child(Button::new("worktree-commit-shell")
                                .text()
                                .label("Open a shell to commit or stash first")
                                .disabled(busy)
                                .on_click(move |_, window, cx| {
                                    let _ = open_shell.update(cx, |app, cx| {
                                        close_worktree_dialog(app, cx);
                                        app.request_terminal(&shell_checkout_id, window, cx);
                                    });
                                }))
                            .child(
                                Checkbox::new("worktree-confirm-dirty")
                                    .checked(dirty_confirmed)
                                    .label("I understand uncommitted files may be lost; delete anyway.")
                                    .on_change(move |checked, _, cx| {
                                        let _ = toggle_dirty.update(cx, |app, cx| {
                                            if let Some(draft) = app
                                                .worktree_removal
                                                .as_mut()
                                                .filter(|draft| draft.request_id == request_id)
                                            {
                                                draft.dirty_confirmed = *checked;
                                                draft.error = None;
                                            }
                                            cx.notify();
                                        });
                                    }),
                            );
                    }
                    if info.unmerged_commits > 0 {
                        let warning = if info.branch.is_some() {
                            format!(
                                "Branch {} has {} unmerged commit(s) versus {}. The branch is kept unless you choose to delete it.",
                                info.branch.as_deref().unwrap_or_default(),
                                info.unmerged_commits,
                                info.default_branch.as_deref().unwrap_or("the default branch")
                            )
                        } else {
                            format!(
                                "Detached HEAD has {} commit(s) unreachable from branches, remotes, or tags. Deleting this worktree may lose them.",
                                info.unmerged_commits
                            )
                        };
                        content = content.child(
                            div()
                                .mt_2()
                                .p_2()
                                .bg(gpui_kit::rgb(0x252932))
                                .text_size(px(12.))
                                .child(warning),
                        );
                        if info.branch.is_none() {
                            let toggle_unmerged = toggle_unmerged.clone();
                            content = content.child(
                                Checkbox::new("worktree-confirm-unmerged")
                                    .checked(unmerged_confirmed)
                                    .label("I understand these detached commits may be lost; delete anyway.")
                                    .on_change(move |checked, _, cx| {
                                        let _ = toggle_unmerged.update(cx, |app, cx| {
                                            if let Some(draft) = app
                                                .worktree_removal
                                                .as_mut()
                                                .filter(|draft| draft.request_id == request_id)
                                            {
                                                draft.unmerged_confirmed = *checked;
                                                draft.error = None;
                                            }
                                            cx.notify();
                                        });
                                    }),
                            );
                        }
                    }
                    if let Some(branch) = info.branch.as_deref() {
                        let toggle_branch = toggle_branch.clone();
                        content = content.child(
                            Checkbox::new("worktree-delete-branch")
                                .checked(delete_branch)
                                .label(format!(
                                    "Delete local branch {}{}",
                                    branch,
                                    if info.unmerged_commits > 0 {
                                        format!(" and its {} unmerged commit(s)", info.unmerged_commits)
                                    } else {
                                        String::new()
                                    }
                                ))
                                .on_change(move |checked, _, cx| {
                                    let _ = toggle_branch.update(cx, |app, cx| {
                                        if let Some(draft) = app
                                            .worktree_removal
                                            .as_mut()
                                            .filter(|draft| draft.request_id == request_id)
                                        {
                                            draft.delete_branch = *checked;
                                            draft.error = None;
                                        }
                                        cx.notify();
                                    });
                                }),
                        );
                    }
                }
                if has_terminal {
                    let mut sessions = div()
                        .mt_2()
                        .text_size(px(12.))
                        .text_color(gpui_kit::rgb(0xe3b341))
                        .child("Close these terminal sessions before archiving or deleting:");
                    for name in &session_names {
                        sessions = sessions.child(div().text_size(px(11.)).child(name.clone()));
                    }
                    for name in &retained_terminals {
                        sessions = sessions.child(div().text_size(px(11.)).child(name.clone()));
                    }
                    content = content.child(sessions);
                }
                if has_pending {
                    let mut pending = div()
                        .text_size(px(12.))
                        .text_color(gpui_kit::rgb(0xe3b341))
                        .child("Wait for terminal creation to finish before continuing:");
                    for request in &pending_ids {
                        pending = pending.child(div().text_size(px(11.)).child(request.clone()));
                    }
                    content = content.child(pending);
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
                        Button::new("worktree-removal-cancel")
                            .ghost()
                            .label("Cancel")
                            .disabled(busy)
                            .on_click(move |_, _, cx| {
                                let _ = close_for_cancel.update(cx, |app, cx| {
                                    close_worktree_dialog(app, cx);
                                });
                            }),
                    )
                    .child(
                        Button::new("worktree-archive")
                            .ghost()
                            .label(if busy { "Archiving…" } else { "Archive" })
                            .disabled(!can_archive)
                            .on_click(move |_, _, cx| {
                                let _ = archive.update(cx, |app, cx| app.submit_worktree_archive(cx));
                            }),
                    )
                    .child(
                        Button::new("worktree-delete")
                            .danger()
                            .label(if busy { "Deleting…" } else { "Delete worktree" })
                            .disabled(!can_delete)
                            .on_click(move |_, window, cx| {
                                let _ = delete.update(cx, |app, cx| {
                                    app.submit_worktree_delete(window, cx)
                                });
                            }),
                    ),
            )
            .on_cancel(move |_, _, cx| {
                let _ = close_for_escape.update(cx, close_worktree_dialog);
                false
            })
            .into_any_element()
    }

    fn can_drag_terminal(&self, id: &str) -> bool {
        let Some(session) = self.navigation.session(id) else {
            return false;
        };
        let source = session.checkout_id.as_str();
        if self.removing_checkouts.contains(source) {
            return false;
        }
        let reorder = self
            .navigation
            .checkout(source)
            .is_some_and(|checkout| checkout.sessions.iter().any(|session| session.id != id));
        let move_elsewhere = self
            .navigation
            .repos
            .iter()
            .flat_map(|repo| &repo.checkouts)
            .any(|checkout| {
                checkout.id != source
                    && !checkout.is_missing
                    && !self.navigation.archived.contains(&checkout.id)
                    && !self.removing_checkouts.contains(&checkout.id)
            });
        reorder || move_elsewhere
    }

    fn begin_sidebar_drag(&mut self, id: &str, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.suppress_sidebar_click = false;
        let Some(source_checkout) = self
            .navigation
            .session(id)
            .map(|session| session.checkout_id.clone())
        else {
            return;
        };
        if self.closing
            || self.closing_sessions.contains(id)
            || self.editing.as_ref().is_some_and(|edit| edit.id == id)
            || !self.can_drag_terminal(id)
        {
            self.sidebar_drag = None;
            return;
        }
        let point = (position.x.as_f32(), position.y.as_f32());
        self.sidebar_drag = Some(SidebarDrag {
            session_id: id.into(),
            source_checkout,
            origin: point,
            position: point,
            started: false,
            target: None,
        });
        cx.notify();
    }

    fn cancel_sidebar_drag(&mut self) {
        if self.sidebar_drag.take().is_some() {
            self.suppress_sidebar_click = true;
        }
    }

    /// All insertion indices exclude the dragged session, just like the final list.
    fn sidebar_drop_slot(&self, checkout_id: &str, index: usize) -> usize {
        let source_index = self
            .sidebar_drag
            .as_ref()
            .filter(|drag| drag.started && drag.source_checkout == checkout_id)
            .and_then(|drag| {
                self.navigation.checkout(checkout_id).and_then(|checkout| {
                    checkout
                        .sessions
                        .iter()
                        .position(|session| session.id == drag.session_id)
                })
            });
        index - usize::from(source_index.is_some_and(|source| source < index))
    }

    fn sidebar_drop_target_at(
        &self,
        position: Point<Pixels>,
        rows: &Rc<RefCell<Vec<SidebarRowBounds>>>,
    ) -> Option<SidebarDropTarget> {
        let drag = self.sidebar_drag.as_ref()?;
        let rows = rows.borrow();
        let row = rows.iter().find(|row| {
            position.x >= row.bounds.left()
                && position.x <= row.bounds.right()
                && position.y >= row.bounds.top()
                && position.y <= row.bounds.bottom()
        })?;
        let target = match &row.region {
            SidebarDropRegion::Insertion { checkout_id, index } => SidebarDropTarget {
                checkout_id: checkout_id.clone(),
                index: *index,
            },
            SidebarDropRegion::Append { checkout_id, index } => SidebarDropTarget {
                checkout_id: checkout_id.clone(),
                index: self.sidebar_drop_slot(checkout_id, *index),
            },
            SidebarDropRegion::Session { checkout_id, index } => SidebarDropTarget {
                checkout_id: checkout_id.clone(),
                index: self.sidebar_drop_slot(
                    checkout_id,
                    index + usize::from(position.y >= row.bounds.center().y),
                ),
            },
        };
        let checkout = self.navigation.checkout(&target.checkout_id)?;
        if checkout.is_missing
            || self.navigation.archived.contains(&target.checkout_id)
            || self.removing_checkouts.contains(&target.checkout_id)
            || self.removing_checkouts.contains(&drag.source_checkout)
            || (target.checkout_id == drag.source_checkout
                && !checkout
                    .sessions
                    .iter()
                    .any(|session| session.id != drag.session_id))
        {
            return None;
        }
        if target.checkout_id == drag.source_checkout
            && checkout
                .sessions
                .iter()
                .position(|session| session.id == drag.session_id)
                == Some(target.index)
        {
            return None;
        }
        Some(target)
    }

    fn update_sidebar_drag(
        &mut self,
        position: Point<Pixels>,
        pressed_button: Option<MouseButton>,
        rows: &Rc<RefCell<Vec<SidebarRowBounds>>>,
        cx: &mut Context<Self>,
    ) {
        if pressed_button != Some(MouseButton::Left) {
            if self.sidebar_drag.is_some() {
                self.cancel_sidebar_drag();
                cx.notify();
            }
            return;
        }
        let point = (position.x.as_f32(), position.y.as_f32());
        let Some(drag) = self.sidebar_drag.as_mut() else {
            return;
        };
        let position_changed = drag.position != point;
        drag.position = point;
        let started_now = !drag.started
            && (point.0 - drag.origin.0).hypot(point.1 - drag.origin.1) >= SIDEBAR_DRAG_THRESHOLD;
        drag.started |= started_now;
        if !drag.started {
            return;
        }
        let target = self.sidebar_drop_target_at(position, rows);
        let drag = self.sidebar_drag.as_mut().expect("active sidebar drag");
        let changed = drag.target != target;
        drag.target = target;
        if changed || started_now || position_changed {
            cx.notify();
        }
    }

    fn finish_sidebar_drag(
        &mut self,
        position: Point<Pixels>,
        rows: &Rc<RefCell<Vec<SidebarRowBounds>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_sidebar_drag(position, Some(MouseButton::Left), rows, cx);
        let Some(drag) = self.sidebar_drag.take() else {
            return;
        };
        if drag.started {
            self.suppress_sidebar_click = true;
            if let Some(target) = drag.target {
                let source_index =
                    self.navigation
                        .checkout(&drag.source_checkout)
                        .and_then(|checkout| {
                            checkout
                                .sessions
                                .iter()
                                .position(|session| session.id == drag.session_id)
                        });
                if let Some(source_index) = source_index {
                    let index = target.index;
                    if target.checkout_id != drag.source_checkout || index != source_index {
                        self.move_terminal(
                            &drag.session_id,
                            &target.checkout_id,
                            index,
                            window,
                            cx,
                        );
                    }
                }
            }
        }
        cx.notify();
    }

    fn tick_sidebar_drag(
        &mut self,
        rows: &Rc<RefCell<Vec<SidebarRowBounds>>>,
        cx: &mut Context<Self>,
    ) {
        let Some((x, y)) = self
            .sidebar_drag
            .as_ref()
            .filter(|drag| drag.started)
            .map(|drag| drag.position)
        else {
            return;
        };
        let position = gpui_kit::point(px(x), px(y));
        let target = self.sidebar_drop_target_at(position, rows);
        let drag = self.sidebar_drag.as_mut().expect("active sidebar drag");
        let mut changed = drag.target != target;
        drag.target = target;

        let viewport = self.scroll.bounds();
        if x >= viewport.left().as_f32()
            && x <= viewport.right().as_f32()
            && y >= viewport.top().as_f32()
            && y <= viewport.bottom().as_f32()
        {
            let offset = self.scroll.offset();
            let max = self.scroll.max_offset().y.as_f32();
            let next = if y < viewport.top().as_f32() + SIDEBAR_SCROLL_EDGE {
                (offset.y.as_f32()
                    + sidebar_scroll_step(viewport.top().as_f32() + SIDEBAR_SCROLL_EDGE - y))
                .min(0.0)
            } else if y > viewport.bottom().as_f32() - SIDEBAR_SCROLL_EDGE {
                (offset.y.as_f32()
                    + sidebar_scroll_step(viewport.bottom().as_f32() - SIDEBAR_SCROLL_EDGE - y))
                .max(-max)
            } else {
                offset.y.as_f32()
            };
            if next != offset.y.as_f32() {
                self.scroll.set_offset(gpui_kit::point(offset.x, px(next)));
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn move_terminal(
        &mut self,
        id: &str,
        target: &str,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source = self
            .navigation
            .session(id)
            .map(|session| session.checkout_id.clone());
        if self.removing_checkouts.contains(target)
            || source
                .as_deref()
                .is_some_and(|source| self.removing_checkouts.contains(source))
        {
            self.failure =
                Some("A worktree being deleted cannot receive or move terminal sessions.".into());
            cx.notify();
            return;
        }
        let cross_checkout = source.as_deref().is_some_and(|source| source != target);
        if let Err(error) = self.navigation.move_session(id, target, index) {
            self.failure = Some(error);
        } else {
            if let Some(terminal) = self.terminals.get_mut(id) {
                terminal.checkout_id = target.into();
            }
            if cross_checkout {
                self.notice = Some("Terminal moved. Its process and working directory were preserved; no cd was sent.".into());
                self.focus_terminal(window, cx);
            }
            self.persist();
        }
        cx.notify();
    }

    fn request_dismiss_missing_checkout(
        &mut self,
        checkout_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.closing {
            return;
        }
        let Some(checkout) = self.navigation.checkout(checkout_id).filter(|checkout| {
            checkout.is_missing
                && !checkout.is_primary
                && self.navigation.home.as_deref() != Some(checkout_id)
        }) else {
            return;
        };
        let question = format!(
            "Hide missing checkout “{}” from the sidebar?",
            checkout.path
        );
        let prompt = window.prompt(
            PromptLevel::Warning,
            &question,
            Some("No files will be deleted. Git metadata and the branch are kept, and the repository menu can restore this row."),
            &["Cancel", "Hide checkout"],
            cx,
        );
        let id = checkout_id.to_owned();
        cx.spawn_in(window, async move |view, cx| {
            if prompt.await == Ok(1) {
                let _ = view.update(cx, |app, cx| app.dismiss_missing_checkout(&id, cx));
            }
        })
        .detach();
    }

    fn dismiss_missing_checkout(&mut self, checkout_id: &str, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        if self.checkout_has_terminal_activity(checkout_id) {
            self.failure = Some("Close this worktree's terminal sessions before hiding it.".into());
        } else {
            match self.navigation.dismiss_missing_checkout(checkout_id) {
                Ok(()) => {
                    self.diff_totals.remove(checkout_id);
                    self.membership_revision += 1;
                    self.persist();
                    self.refresh_diff_totals(cx);
                    self.notice = Some(
                        "Missing checkout hidden. No files were deleted; Git metadata and branch were retained."
                            .into(),
                    );
                }
                Err(error) => self.failure = Some(error),
            }
        }
        cx.notify();
    }

    fn worktree_removal_error(&mut self, request_id: u64, error: String, cx: &mut Context<Self>) {
        if let Some(draft) = self
            .worktree_removal
            .as_mut()
            .filter(|draft| draft.request_id == request_id)
        {
            draft.error = Some(error);
            draft.busy = false;
        }
        cx.notify();
    }

    fn submit_worktree_archive(&mut self, cx: &mut Context<Self>) {
        let Some((request_id, id, busy)) = self
            .worktree_removal
            .as_ref()
            .map(|draft| (draft.request_id, draft.checkout_id.clone(), draft.busy))
        else {
            return;
        };
        if busy {
            return;
        }
        if self.checkout_has_terminal_activity(&id) {
            self.worktree_removal_error(
                request_id,
                "Close this worktree's terminal sessions before archiving it.".into(),
                cx,
            );
            return;
        }
        match self.navigation.archive(&id) {
            Ok(()) => {
                self.worktree_removal = None;
                self.membership_revision += 1;
                self.persist();
                self.refresh_diff_totals(cx);
                cx.notify();
            }
            Err(error) => self.worktree_removal_error(request_id, error, cx),
        }
    }

    fn submit_worktree_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((request_id, repo_id, id, info, confirm_dirty, confirm_unmerged, delete_branch)) =
            self.worktree_removal.as_ref().map(|draft| {
                (
                    draft.request_id,
                    draft.repo_id.clone(),
                    draft.checkout_id.clone(),
                    draft.info.clone(),
                    draft.dirty_confirmed,
                    draft.unmerged_confirmed,
                    draft.delete_branch,
                )
            })
        else {
            return;
        };
        if self
            .worktree_removal
            .as_ref()
            .is_some_and(|draft| draft.busy || draft.loading)
        {
            return;
        }
        let Some(info) = info else {
            self.worktree_removal_error(
                request_id,
                "Git has not returned a removal preview.".into(),
                cx,
            );
            return;
        };
        let Some(repo) = self
            .navigation
            .repos
            .iter()
            .find(|repo| repo.id == repo_id)
            .cloned()
        else {
            self.worktree_removal_error(
                request_id,
                "This repository is no longer open.".into(),
                cx,
            );
            return;
        };
        let Some(checkout) = repo
            .checkouts
            .iter()
            .find(|checkout| checkout.id == id)
            .cloned()
        else {
            self.worktree_removal_error(request_id, "This worktree is no longer open.".into(), cx);
            return;
        };
        if repo.kind != RepoKind::Git
            || checkout.is_primary
            || checkout.is_missing
            || self.navigation.home.as_deref() == Some(&id)
            || info.checkout_id != id
            || info.path != checkout.canonical_path
        {
            self.worktree_removal_error(
                request_id,
                "This is no longer the selected available non-primary worktree. Refresh and retry."
                    .into(),
                cx,
            );
            return;
        }
        if self.checkout_has_terminal_activity(&id) {
            self.worktree_removal_error(
                request_id,
                "Close every terminal session and wait for pending shell creation before deleting this worktree.".into(),
                cx,
            );
            return;
        }
        if !info.dirty_files.is_empty() && !confirm_dirty {
            self.worktree_removal_error(
                request_id,
                "Confirm that uncommitted files may be lost before deleting this worktree.".into(),
                cx,
            );
            return;
        }
        if info.branch.is_none() && info.unmerged_commits > 0 && !confirm_unmerged {
            self.worktree_removal_error(
                request_id,
                "Confirm that detached commits may be lost before deleting this worktree.".into(),
                cx,
            );
            return;
        }
        if delete_branch && info.branch.is_none() {
            self.worktree_removal_error(
                request_id,
                "This worktree has no local branch to delete.".into(),
                cx,
            );
            return;
        }
        if !self.removing_checkouts.insert(id.clone()) {
            self.worktree_removal_error(
                request_id,
                "This worktree is already being removed.".into(),
                cx,
            );
            return;
        }
        let confirmation = muster_core::navigation::WorktreeRemovalConfirmation {
            checkout_id: info.checkout_id.clone(),
            path: info.path.clone(),
            branch: info.branch.clone(),
            head: info.head.clone(),
            dirty_files: info.dirty_files.clone(),
            default_ref: info.default_ref.clone(),
            unmerged_commits: info.unmerged_commits,
            confirm_dirty,
            confirm_unmerged,
            delete_branch,
        };
        if let Some(draft) = self
            .worktree_removal
            .as_mut()
            .filter(|draft| draft.request_id == request_id)
        {
            draft.busy = true;
            draft.error = None;
        }
        let worker = cx
            .background_executor()
            .spawn(async move { navigation::remove_worktree(&repo, &checkout, &confirmation) });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update_in(cx, |app, window, cx| {
                match result {
                    Ok(removed) => match app.navigation.forget_removed_worktree(&id) {
                        Ok(()) => {
                            if app
                                .worktree_removal
                                .as_ref()
                                .is_some_and(|draft| draft.request_id == request_id)
                            {
                                app.worktree_removal = None;
                            }
                            app.diff_totals.remove(&id);
                            app.membership_revision += 1;
                            app.persist();
                            app.refresh_diff_totals(cx);
                            app.focus_terminal(window, cx);
                            if let Some(warning) = removed.warning {
                                app.notice = Some(warning);
                            }
                        }
                        Err(error) => app.worktree_removal_error(
                            request_id,
                            format!("Git removed the worktree, but the sidebar could not update: {error}"),
                            cx,
                        ),
                    },
                    Err(error) => app.worktree_removal_error(request_id, error, cx),
                }
                app.removing_checkouts.remove(&id);
                cx.notify();
            });
        })
        .detach();
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
        let message = format!(
            "Restore {} archived {}?",
            ids.len(),
            if ids.len() == 1 {
                "worktree"
            } else {
                "worktrees"
            }
        );
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
                    app.refresh_diff_totals(cx);
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
                        app.membership_revision += 1; app.persist(); app.refresh_diff_totals(cx);
                    } else { app.failure = Some("The repository now has a terminal session. Close it before removing the directory.".into()); }
                    cx.notify();
                });
            }
        }).detach();
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" && self.sidebar_drag.is_some() {
            self.cancel_sidebar_drag();
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let dialog_open = self.worktree.is_some() || self.worktree_removal.is_some();
        if event.keystroke.key == "escape" && (self.editing.is_some() || dialog_open) {
            if self.worktree.as_ref().is_some_and(|draft| draft.busy) {
                self.failure =
                    Some("This worktree is still being created. Wait for it to finish.".into());
                cx.notify();
                return;
            }
            if self
                .worktree_removal
                .as_ref()
                .is_some_and(|draft| draft.busy)
            {
                if let Some(draft) = self.worktree_removal.as_mut() {
                    draft.error =
                        Some("Worktree deletion is in progress. Wait for Git to finish.".into());
                }
                cx.notify();
                return;
            }
            self.editing = None;
            self.worktree = None;
            self.worktree_removal = None;
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
        let can_add = repo.kind == RepoKind::Git
            && repo
                .checkouts
                .iter()
                .any(|item| item.is_primary && !item.is_missing);
        let can_close = !repo
            .checkouts
            .iter()
            .any(|checkout| self.navigation.home.as_deref() == Some(&checkout.id));
        move |menu, _, _| {
            let mut menu = menu.min_w(px(224.));
            if can_add {
                let id = id.clone();
                let view = view.clone();
                menu = menu.item(
                    PopupMenuItem::new("New worktree")
                        .icon(IconName::GitBranchPlus)
                        .on_click(move |_, _window, cx| {
                            let _ = view.update_in(cx, |app, window, cx| {
                                app.open_worktree_dialog(&id, window, cx)
                            });
                        }),
                );
            }
            if can_add && archived > 0 {
                let id = id.clone();
                let view = view.clone();
                menu = menu.item(
                    PopupMenuItem::new(format!("Restore archived worktrees ({archived})"))
                        .icon(IconName::ArchiveRestore)
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
                    PopupMenuItem::new("Remove from panel")
                        .icon(IconName::FolderMinus)
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

    fn sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.weak_entity();
        let drag_rows = Rc::new(RefCell::new(Vec::new()));
        let drag_dispatcher = self.sidebar_drag.is_some().then(|| {
            let move_view = weak.clone();
            let move_rows = drag_rows.clone();
            let up_view = weak.clone();
            let up_rows = drag_rows.clone();
            let exit_view = weak.clone();
            let auto_scroll = self.sidebar_drag.as_ref().is_some_and(|drag| {
                if !drag.started {
                    return false;
                }
                let bounds = self.scroll.bounds();
                let (x, y) = drag.position;
                x >= bounds.left().as_f32()
                    && x <= bounds.right().as_f32()
                    && y >= bounds.top().as_f32()
                    && y <= bounds.bottom().as_f32()
                    && (y < bounds.top().as_f32() + SIDEBAR_SCROLL_EDGE
                        || y > bounds.bottom().as_f32() - SIDEBAR_SCROLL_EDGE)
            });
            let scroll_view = weak.clone();
            let scroll_rows = drag_rows.clone();
            sidebar_drag_dispatcher(move |window, _| {
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase == gpui_kit::DispatchPhase::Capture {
                        let position = event.position;
                        let pressed_button = event.pressed_button;
                        let _ = move_view.update(cx, |app, cx| {
                            app.update_sidebar_drag(position, pressed_button, &move_rows, cx)
                        });
                    }
                });
                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                    if phase == gpui_kit::DispatchPhase::Capture
                        && event.button == MouseButton::Left
                    {
                        let position = event.position;
                        let _ = up_view.update(cx, |app, cx| {
                            app.finish_sidebar_drag(position, &up_rows, window, cx)
                        });
                    }
                });
                window.on_mouse_event(move |_: &MouseExitEvent, phase, _, cx| {
                    if phase == gpui_kit::DispatchPhase::Capture {
                        let _ = exit_view.update(cx, |app, cx| {
                            if app.sidebar_drag.is_some() {
                                app.cancel_sidebar_drag();
                                cx.notify();
                            }
                        });
                    }
                });
                if auto_scroll {
                    window.on_next_frame(move |_, cx| {
                        let _ = scroll_view
                            .update(cx, |app, cx| app.tick_sidebar_drag(&scroll_rows, cx));
                    });
                }
            })
        });
        let mut rows = Vec::<AnyElement>::new();
        for repo in &self.navigation.repos {
            let id = repo.id.clone();
            let folded = self.navigation.folded_repos.contains(&id);
            let toggle_id = id.clone();
            let has_menu = (repo.kind == RepoKind::Git
                && repo
                    .checkouts
                    .iter()
                    .any(|checkout| checkout.is_primary && !checkout.is_missing))
                || !repo
                    .checkouts
                    .iter()
                    .any(|checkout| self.navigation.home.as_deref() == Some(&checkout.id));
            let heading = div()
                .id(SharedString::from(format!("heading-{id}")))
                .group(SharedString::from(format!("repo-heading-{id}")))
                .relative()
                .flex()
                .items_end()
                .w_full()
                .cursor_pointer()
                .hover(|style| style.bg(gpui_kit::rgb(0x2d323c)))
                .pl(px(10.))
                .pr(px(8.))
                .pt(px(8.))
                .pb(px(4.))
                .child(
                    Button::new(SharedString::from(format!("fold-repo-{id}")))
                        .text()
                        .rounded(px(0.))
                        .absolute()
                        .left(px(10.))
                        .right(px(if has_menu { 36. } else { 8. }))
                        .top(px(8.))
                        .h(px(17.))
                        .p_0()
                        .aria_expanded(!folded)
                        .tooltip(repo.root.clone())
                        .accessibility_label(format!(
                            "{} repository {} — {}",
                            if folded { "Expand" } else { "Collapse" },
                            repo.name,
                            repo.root
                        ))
                        .on_click(cx.listener(move |app, _, _, cx| {
                            toggle(&mut app.navigation.folded_repos, &toggle_id);
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        // Bottom, not baseline: the flex engine is handed no
                        // baseline to align, so `items_baseline` degrades to the
                        // top, and a 14px name and an 11px path each sit half
                        // their own leading below the line they share. Measured
                        // against the reference, the path hung about 2.5px low;
                        // aligning their boxes at the foot puts the two
                        // baselines within a fifth of a pixel.
                        .items_end()
                        .gap(px(8.))
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
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
                                .child(sidebar_repo_path(
                                    window,
                                    &repo.root,
                                    &repo.name,
                                    self.navigation.sidebar_width,
                                    has_menu,
                                )),
                        ),
                )
                .when(has_menu, |heading| {
                    let menu_id = id.clone();
                    let menu_view = weak.clone();
                    heading.child(
                        div()
                            .absolute()
                            .right(px(8.))
                            .top(px(8.))
                            .w(px(20.))
                            .h(px(20.))
                            // The trigger stays lit while its menu is open, on the
                            // surface the button paints rather than on the
                            // button: a hover refines after the rest style and
                            // would paint the trigger's own transparent hover
                            // back over it.
                            .when(self.open_repo_menu.as_deref() == Some(&id), |trigger| {
                                trigger.bg(gpui_kit::rgb(0x2d323c))
                            })
                            .child(
                                Button::new(SharedString::from(format!("actions-{id}")))
                                    .text()
                                    .text_color(sidebar_faint_ink())
                                    .opacity(if self.open_repo_menu.as_deref() == Some(&id) {
                                        1.
                                    } else {
                                        0.
                                    })
                                    .group_hover(
                                        SharedString::from(format!("repo-heading-{id}")),
                                        |style| style.opacity(1.),
                                    )
                                    .focus(|style| style.opacity(1.))
                                    .rounded(px(0.))
                                    .h(px(20.))
                                    .w(px(20.))
                                    .p_0()
                                    .icon(Icon::new(IconName::Ellipsis).size(px(14.)))
                                    .accessibility_label(format!("Actions for {}", repo.name))
                                    .dropdown_menu(self.repo_menu(id.clone(), weak.clone()))
                                    .on_open_change(move |open, _, cx| {
                                        let _ = menu_view.update(cx, |app, cx| {
                                            if *open {
                                                app.open_repo_menu = Some(menu_id.clone());
                                            } else if app.open_repo_menu.as_deref()
                                                == Some(&menu_id)
                                            {
                                                app.open_repo_menu = None;
                                            }
                                            cx.notify();
                                        });
                                    }),
                            ),
                    )
                });
            rows.push(if has_menu {
                heading
                    .context_menu(self.repo_menu(id, weak.clone()))
                    .into_any_element()
            } else {
                heading.into_any_element()
            });
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
                let has_close = repo.kind == RepoKind::Git
                    && !checkout.is_primary
                    && self.navigation.home.as_deref() != Some(&id);
                let line_slot = Self::line_slot(self.diff_totals.get(&id));
                let figures_width = line_slot.map_or(0., |(added, removed)| {
                    let text = match (added > 0, removed > 0) {
                        (true, true) => format!("+{added} −{removed}"),
                        (true, false) => format!("+{added}"),
                        _ => format!("−{removed}"),
                    };
                    sidebar_text_width(window, &text, 11.).as_f32()
                });
                let name = sidebar_name(
                    window,
                    &title,
                    self.navigation.sidebar_width
                        - 57.
                        - figures_width
                        - if has_close { 28. } else { 0. },
                );
                let icon = if checkout.is_missing {
                    IconName::FolderX
                } else if self.navigation.home.as_deref() == Some(&id) {
                    IconName::House
                } else if repo.kind == RepoKind::Plain {
                    IconName::Folder
                } else if checkout.is_primary {
                    IconName::FolderGit2
                } else {
                    IconName::GitBranch
                };
                let row_group = SharedString::from(format!("checkout-row-{id}"));
                let (row_focus, row_focused) = sidebar_row_focus(window, cx, &id);
                let mut row = div()
                    .id(row_group.clone())
                    .track_focus(&row_focus)
                    .group(row_group.clone())
                    .flex()
                    .items_center()
                    .relative()
                    .h(px(26.))
                    .gap(px(7.))
                    .px(px(8.))
                    .cursor_pointer()
                    .when(checkout.is_missing, |row| row.cursor(gpui_kit::CursorStyle::OperationNotAllowed))
                    .hover(|style| style.bg(gpui_kit::rgb(0x2d323c)))
                    .when(selected, |row| row.bg(gpui_kit::rgb(0x2c3b57)))
                    .when(selected, |row| {
                        row.child(
                            div()
                                .absolute()
                                .left(px(0.))
                                .top(px(0.))
                                .bottom(px(0.))
                                .w(px(2.))
                                .bg(gpui_kit::rgb(0x5b97ff)),
                        )
                    })
                    .on_click(cx.listener({
                        let row_select_id = select_id.clone();
                        move |app, _, window, cx| {
                            if app.navigation.select_checkout(&row_select_id) {
                                app.persist();
                                app.refresh_diff_totals(cx);
                                app.focus_terminal(window, cx);
                                cx.notify();
                            }
                        }
                    }))
                    .child(
                        Button::new(SharedString::from(format!("fold-checkout-{id}")))
                            .text()
                            .text_color(sidebar_faint_ink())
                            .small()
                            .compact()
                            .rounded(px(0.))
                            .h(px(26.))
                            .w(px(16.))
                            .ml(px(-4.))
                            .p_0()
                            .text_color(gpui_kit::rgb(0x8b909c))
                            .icon(
                                Icon::new(if folded {
                                    IconName::ChevronRight
                                } else {
                                    IconName::ChevronDown
                                })
                                .size(px(14.)),
                            )
                            .aria_expanded(!folded)
                            .accessibility_label(format!(
                                "{} {title}",
                                if folded { "Expand" } else { "Collapse" }
                            ))
                            .on_click(cx.listener(move |app, _, _, cx| {
                                cx.stop_propagation();
                                toggle(&mut app.navigation.folded_checkouts, &fold_id);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("select-checkout-{id}")))
                            .text()
                            .rounded(px(0.))
                            .absolute()
                            .left(px(27.))
                            .top(px(0.))
                            .bottom(px(0.))
                            .right(px(8.))
                            .p_0()
                            .aria_selected(selected)
                            .accessibility_label(if checkout.is_missing {
                                format!("Unavailable worktree {title}")
                            } else {
                                format!("Select worktree {title}")
                            })
                            .tooltip(format!("{title} — {}", checkout.path))
                            .disabled(checkout.is_missing)
                            .on_click(cx.listener({
                                let click_id = select_id.clone();
                                move |app, _, window, cx| {
                                    cx.stop_propagation();
                                    if app.navigation.select_checkout(&click_id) {
                                        app.persist();
                                        app.refresh_diff_totals(cx);
                                        app.focus_terminal(window, cx);
                                        cx.notify();
                                    }
                                }
                            })),
                    )
                    .child(Icon::new(icon).size(px(14.)).text_color(gpui_kit::rgb(
                        if checkout.is_missing {
                            0x565b67
                        } else {
                            0x8b909c
                        },
                    )))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .when(checkout.is_missing, |label| label.text_color(gpui_kit::rgb(0x565b67)))
                            .child(name),
                    )
                    .when(repo.kind == RepoKind::Git && !checkout.is_missing, |row| {
                        match Self::line_slot(self.diff_totals.get(&id)) {
                            Some((added, removed)) => row.child(
                                div()
                                    .id(SharedString::from(format!("diff-{id}")))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .text_size(px(11.))
                                    .when(has_close && row_focused, |slot| slot.mr(px(28.)))
                                    .when(has_close, |slot| slot.group_hover(row_group.clone(), |style| style.mr(px(28.))))
                                    // At rest the slot holds a bare `+`/`−`, which says the row
                                    // has changes in the width this panel has ever reserved for
                                    // it. The figures are one pointer away.
                                    .when(added > 0, |slot| {
                                        slot.child(
                                            div()
                                                .id(SharedString::from(format!("diff-mark-add-{id}")))
                                                .test_support()
                                                .text_color(gpui_kit::rgb(0xa1c181))
                                                .when(row_focused, |mark| mark.w(px(0.)))
                                                .overflow_hidden()
                                                .group_hover(row_group.clone(), |style| {
                                                style.w(px(0.))
                                            })
                                                .cursor_pointer()
                                                .child("+"),
                                        )
                                    })
                                    .when(removed > 0, |slot| {
                                        slot.child(
                                            div()
                                                .id(SharedString::from(format!("diff-mark-del-{id}")))
                                                .test_support()
                                                .text_color(gpui_kit::rgb(0xd97f84))
                                                .when(row_focused, |mark| mark.w(px(0.)))
                                                .overflow_hidden()
                                                .group_hover(row_group.clone(), |style| {
                                                    style.w(px(0.))
                                                })
                                                .cursor_pointer()
                                                .child("\u{2212}"),
                                        )
                                    })
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("diff-figures-{id}")))
                                            .test_support()
                                            .flex()
                                            .items_center()
                                            .gap(px(5.))
                                            .min_w_0()
                                            .overflow_hidden()
                                            .w(if row_focused { Length::Auto } else { px(0.).into() })
                                            .group_hover(row_group.clone(), |style| {
                                                style.w(Length::Auto).ml(px(5.))
                                            })
                                            .when(added > 0, |figures| {
                                                figures.child(
                                                    div()
                                                        .text_color(gpui_kit::rgb(0xa1c181))
                                                        .child(format!("+{added}")),
                                                )
                                            })
                                            .when(removed > 0, |figures| {
                                                figures.child(
                                                    div()
                                                        .text_color(gpui_kit::rgb(0xd97f84))
                                                        .child(format!("\u{2212}{removed}")),
                                                )
                                            }),
                                    ),
                            ),
                            None => row,
                        }
                    })
                    .when(checkout.is_missing, |row| {
                        row.child(
                            div()
                                .text_size(px(11.))
                                .text_color(gpui_kit::rgb(0xf7adb1))
                                .when(has_close && row_focused, |note| note.mr(px(28.)))
                                .when(has_close, |note| note.group_hover(row_group.clone(), |style| style.mr(px(28.))))
                                .child("Directory missing"),
                        )
                    })
                    .when(has_close && checkout.is_missing, |row| {
                        let dismiss_id = id.clone();
                        row.child(
                            Button::new(SharedString::from(format!("dismiss-missing-{id}")))
                                .custom(sidebar_action_style(cx))
                                .cursor_pointer()
                                .rounded(px(0.))
                                .absolute()
                                .right(px(8.))
                                .top(px(3.))
                                .h(px(20.))
                                .w(px(20.))
                                .p_0()
                                .opacity(if row_focused { 1. } else { 0. })
                                .group_hover(row_group.clone(), |style| style.opacity(1.))
                                .focus(|style| style.opacity(1.))
                                .icon(Icon::new(IconName::X).size(px(14.)))
                                .accessibility_label(format!(
                                    "Dismiss missing checkout {title}; no files are deleted; Git metadata and branch are retained"
                                ))
                                .tooltip(
                                    "Hide this missing checkout only; no files are deleted, and Git metadata and branch are retained",
                                )
                                .on_click(cx.listener(move |app, _, window, cx| {
                                    cx.stop_propagation();
                                    app.request_dismiss_missing_checkout(&dismiss_id, window, cx)
                                })),
                        )
                    })
                    .when(
                        has_close && !checkout.is_missing,
                        |row| {
                            let actions_id = id.clone();
                            let actions_repo_id = repo.id.clone();
                            // The contextual lifecycle dialog names both reversible archive
                            // and destructive deletion; no row action deletes immediately.
                            row.child(
                                Button::new(SharedString::from(format!("worktree-actions-{id}")))
                                    .custom(sidebar_action_style(cx))
                                    .cursor_pointer()
                                    .rounded(px(0.))
                                    .absolute()
                                    .right(px(8.))
                                    .top(px(3.))
                                    .h(px(20.))
                                    .w(px(20.))
                                    .p_0()
                                    .opacity(if row_focused { 1. } else { 0. })
                                    .group_hover(row_group.clone(), |style| style.opacity(1.))
                                    .focus(|style| style.opacity(1.))
                                    .icon(Icon::new(IconName::X).size(px(14.)))
                                    .accessibility_label(format!(
                                        "Archive or delete worktree {title}"
                                    ))
                                    .tooltip("Archive or delete worktree")
                                    .on_click(cx.listener(move |app, _, window, cx| {
                                        cx.stop_propagation();
                                        app.open_worktree_removal_dialog(
                                            &actions_repo_id,
                                            &actions_id,
                                            window,
                                            cx,
                                        )
                                    })),
                            )
                        },
                    );
                let folded_append_marker = folded
                    && self.sidebar_drag.as_ref().is_some_and(|drag| {
                        drag.started
                            && drag.target.as_ref().is_some_and(|target| {
                                target.checkout_id == id
                                    && target.index
                                        == self.sidebar_drop_slot(
                                            &checkout.id,
                                            checkout.sessions.len(),
                                        )
                            })
                    });
                row = row.when(
                    self.sidebar_drag.as_ref().is_some_and(|drag| {
                        drag.started
                            && drag
                                .target
                                .as_ref()
                                .is_some_and(|target| target.checkout_id == id)
                    }),
                    |row| {
                        row.child(
                            div()
                                .absolute()
                                .left(px(0.))
                                .top(px(0.))
                                .bottom(px(0.))
                                .w(px(2.))
                                .bg(gpui_kit::rgb(0x5b97ff)),
                        )
                    },
                );
                if !checkout.is_missing && self.sidebar_drag.is_some() {
                    row = row.child(sidebar_drag_geometry(
                        SidebarDropRegion::Append {
                            checkout_id: id.clone(),
                            index: checkout.sessions.len(),
                        },
                        drag_rows.clone(),
                    ));
                }
                if folded || checkout.is_missing {
                    rows.push(row.into_any_element());
                    if folded_append_marker {
                        rows.push(sidebar_drop_marker(
                            self.sidebar_drag
                                .as_ref()
                                .and_then(|drag| drag.target.clone())
                                .unwrap(),
                            drag_rows.clone(),
                        ));
                    }
                    continue;
                }
                let checkout_row = row.into_any_element();
                let mut checkout_children = Vec::<AnyElement>::new();
                for (session_index, session) in checkout.sessions.iter().enumerate() {
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
                    let failed = !agent
                        && matches!(status.shell, ShellState::Running)
                        && terminal
                            .shell
                            .session
                            .last_command_exit()
                            .is_some_and(|code| code != 0);
                    let id = session.id.clone();
                    let click_id = id.clone();
                    let drag_id = id.clone();
                    let close_id = id.clone();
                    let row_click_id = id.clone();
                    let state_label = format!(
                        "Terminal session: {title}{}",
                        if unknown {
                            ", state unavailable"
                        } else if failed {
                            ", last command failed"
                        } else if matches!(status.shell, ShellState::Exited { .. }) {
                            ", shell exited"
                        } else if agent {
                            ", agent session state not identified"
                        } else if busy {
                            ", process running"
                        } else {
                            ", idle"
                        }
                    );
                    let renaming = self.editing.as_ref().is_some_and(|edit| edit.id == id);
                    let content =
                        if let Some(editing) = self.editing.as_ref().filter(|edit| edit.id == id) {
                            div()
                                .flex()
                                .items_center()
                                .gap(px(7.))
                                .h(px(26.))
                                .w_full()
                                .child(
                                    Icon::new(if agent {
                                        IconName::Sparkles
                                    } else {
                                        IconName::SquareTerminal
                                    })
                                    .size(px(14.))
                                    .text_color(gpui_kit::rgb(0x8b909c)),
                                )
                                .child(
                                    Input::new(&editing.input)
                                        .id(SharedString::from(format!("rename-session-{id}")))
                                        .appearance(false)
                                        .p_0()
                                        .aria_label("Terminal session name")
                                        .h(px(26.))
                                        .w_full()
                                        .min_w_0(),
                                )
                                .into_any_element()
                        } else {
                            div()
                                .flex()
                                .items_center()
                                .h(px(26.))
                                .w_full()
                                .min_w_0()
                                .gap(px(7.))
                                .child(
                                    Icon::new(if agent {
                                        IconName::Sparkles
                                    } else {
                                        IconName::SquareTerminal
                                    })
                                    .size(px(14.))
                                    .text_color(
                                        gpui_kit::rgb(if unknown {
                                            0xe3b341
                                        } else if failed {
                                            0xf7adb1
                                        } else if busy && !agent {
                                            0x5fc98a
                                        } else {
                                            0x8b909c
                                        }),
                                    ),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(sidebar_name(
                                            window,
                                            &title,
                                            self.navigation.sidebar_width - 73.,
                                        )),
                                )
                                .into_any_element()
                        };
                    let row_group = SharedString::from(format!("session-row-{id}"));
                    let (row_focus, row_focused) = sidebar_row_focus(window, cx, &id);
                    let row = div()
                        .id(row_group.clone())
                        .track_focus(&row_focus)
                        .group(row_group.clone())
                        .flex()
                        .items_center()
                        .relative()
                        .pl(px(12.))
                        .pr(px(8.))
                        .cursor_pointer()
                        .when(
                            self.sidebar_drag
                                .as_ref()
                                .is_some_and(|drag| drag.started && drag.session_id == id),
                            |row| row.opacity(0.42),
                        )
                        .hover(|style| style.bg(gpui_kit::rgb(0x2d323c)))
                        .h(px(26.))
                        .when(selected, |row| row.bg(gpui_kit::rgb(0x2c3b57)))
                        .when(!self.closing_sessions.contains(&id) && !renaming, |row| {
                            row.on_click(cx.listener(
                                move |app, event: &gpui_kit::ClickEvent, window, cx| {
                                    if app.suppress_sidebar_click {
                                        app.suppress_sidebar_click = false;
                                        return;
                                    }
                                    if event.click_count() == 2 {
                                        app.start_rename(&row_click_id, window, cx);
                                    } else if app.navigation.select_session(&row_click_id) {
                                        app.persist();
                                        app.refresh_diff_totals(cx);
                                        app.focus_terminal(window, cx);
                                        cx.notify();
                                    }
                                },
                            ))
                        })
                        .when(selected, |row| {
                            row.child(
                                div()
                                    .absolute()
                                    .left(px(0.))
                                    .top(px(0.))
                                    .bottom(px(0.))
                                    .w(px(2.))
                                    .bg(gpui_kit::rgb(0x5b97ff)),
                            )
                        })
                        .when(!renaming, |row| {
                            row.child(
                                Button::new(SharedString::from(format!("select-session-{id}")))
                                    .text()
                                    .rounded(px(0.))
                                    .absolute()
                                    .left(px(0.))
                                    .right(px(0.))
                                    .top(px(0.))
                                    .bottom(px(0.))
                                    .p_0()
                                    .aria_selected(selected)
                                    .tooltip(state_label.clone())
                                    .accessibility_label(state_label)
                                    .disabled(self.closing_sessions.contains(&id))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |app, event: &MouseDownEvent, _, cx| {
                                            app.begin_sidebar_drag(&drag_id, event.position, cx);
                                        }),
                                    )
                                    .on_click(cx.listener(
                                        move |app, event: &gpui_kit::ClickEvent, window, cx| {
                                            cx.stop_propagation();
                                            if app.suppress_sidebar_click {
                                                app.suppress_sidebar_click = false;
                                                return;
                                            }
                                            if event.click_count() == 2 {
                                                app.start_rename(&click_id, window, cx);
                                            } else if app.navigation.select_session(&click_id) {
                                                app.persist();
                                                app.refresh_diff_totals(cx);
                                                app.focus_terminal(window, cx);
                                                cx.notify();
                                            }
                                        },
                                    )),
                            )
                        })
                        .child(div().flex_1().min_w_0().pr(px(20.)).child(content))
                        .child(
                            Button::new(SharedString::from(format!("close-session-{id}")))
                                .custom(sidebar_action_style(cx))
                                .cursor_pointer()
                                .rounded(px(0.))
                                .absolute()
                                .right(px(8.))
                                .top(px(3.))
                                .h(px(20.))
                                .w(px(20.))
                                .p_0()
                                .opacity(if row_focused { 1. } else { 0. })
                                .group_hover(row_group.clone(), |style| style.opacity(1.))
                                .focus(|style| style.opacity(1.))
                                .icon(Icon::new(IconName::X).size(px(14.)))
                                .tooltip("Close terminal")
                                .accessibility_label(format!("Close terminal session: {title}"))
                                .disabled(self.closing_sessions.contains(&id))
                                .on_click(cx.listener(move |app, _, window, cx| {
                                    cx.stop_propagation();
                                    app.request_close_terminal(&close_id, window, cx)
                                })),
                        );
                    let marker_before = self.sidebar_drag.as_ref().is_some_and(|drag| {
                        drag.started
                            && drag.target.as_ref().is_some_and(|target| {
                                target.checkout_id == checkout.id
                                    && target.index
                                        == self.sidebar_drop_slot(&checkout.id, session_index)
                                    && drag.session_id != session.id
                            })
                    });
                    if marker_before {
                        checkout_children.push(sidebar_drop_marker(
                            self.sidebar_drag
                                .as_ref()
                                .and_then(|drag| drag.target.clone())
                                .unwrap(),
                            drag_rows.clone(),
                        ));
                    }
                    checkout_children.push(
                        row.when(
                            self.sidebar_drag
                                .as_ref()
                                .is_some_and(|drag| drag.session_id != session.id),
                            |row| {
                                row.child(sidebar_drag_geometry(
                                    SidebarDropRegion::Session {
                                        checkout_id: checkout.id.clone(),
                                        index: session_index,
                                    },
                                    drag_rows.clone(),
                                ))
                            },
                        )
                        .context_menu(self.terminal_menu(id, weak.clone()))
                        .into_any_element(),
                    );
                }
                for _ in self
                    .navigation
                    .pending
                    .iter()
                    .filter(|(_, pending)| *pending == &checkout.id)
                {
                    checkout_children.push(
                        div()
                            .pl(px(12.))
                            .h(px(26.))
                            .flex()
                            .items_center()
                            .gap(px(7.))
                            .text_color(gpui_kit::rgb(0x8b909c))
                            .child(Icon::new(IconName::LoaderCircle).size(px(14.)))
                            .child(format!("Starting terminal in {title}…"))
                            .into_any_element(),
                    );
                }
                let create_id = checkout.id.clone();
                let append_marker = self.sidebar_drag.as_ref().is_some_and(|drag| {
                    drag.started
                        && drag.target.as_ref().is_some_and(|target| {
                            target.checkout_id == checkout.id
                                && target.index
                                    == self.sidebar_drop_slot(&checkout.id, checkout.sessions.len())
                        })
                });
                if append_marker {
                    checkout_children.push(sidebar_drop_marker(
                        self.sidebar_drag
                            .as_ref()
                            .and_then(|drag| drag.target.clone())
                            .unwrap(),
                        drag_rows.clone(),
                    ));
                }
                // The row that adds a terminal is centred across the panel's
                // own width rather than sitting on the terminals' glyph axis: it
                // is the one row in a group that is not about one terminal, so
                // it reads as the group's last line instead of as another
                // terminal. It therefore spans the wrapper rather than the
                // indented column the terminals are drawn in.
                let new_terminal_row = div()
                    .relative()
                    .w_full()
                    .hover(|style| style.bg(gpui_kit::rgb(0x2d323c)))
                    .when(self.sidebar_drag.is_some(), |row| {
                        row.child(sidebar_drag_geometry(
                            SidebarDropRegion::Append {
                                checkout_id: checkout.id.clone(),
                                index: checkout.sessions.len(),
                            },
                            drag_rows.clone(),
                        ))
                    })
                    .child(
                        // The component's Button centres its content in a
                        // full-size box, which is what this row wants. The base
                        // button lays children out from the leading edge, so it
                        // carries the centring itself; still a button, so it is
                        // still focusable and still a click.
                        BaseButton::new(SharedString::from(format!(
                            "new-terminal-{}",
                            checkout.id
                        )))
                        .h(px(26.))
                        .w_full()
                        .gap(px(7.))
                        .justify_center()
                        .cursor_pointer()
                        .text_color(sidebar_faint_ink())
                        .hover(|style| style.text_color(gpui_kit::rgb(0xd7dae0)))
                        .child(Icon::new(IconName::Plus).size(px(14.)))
                        .child("New terminal")
                        .accessibility_label(format!("New terminal for {title}"))
                        .on_click(cx.listener(
                            move |app, _, window, cx| app.request_terminal(&create_id, window, cx),
                        )),
                    )
                    .into_any_element();
                let guide = div()
                    .ml(px(14.))
                    .border_l_1()
                    .border_color(gpui_kit::rgb(0x1c1f26))
                    .group_hover(
                        SharedString::from(format!("checkout-children-{}", checkout.id)),
                        |style| style.border_color(gpui_kit::rgb(0x2c313b)),
                    )
                    .children(checkout_children);
                rows.push(
                    div()
                        .group(SharedString::from(format!(
                            "checkout-children-{}",
                            checkout.id
                        )))
                        .flex()
                        .flex_col()
                        .w_full()
                        .child(checkout_row)
                        .child(guide)
                        .child(new_terminal_row)
                        .into_any_element(),
                );
            }
            rows.push(div().h(px(8.)).into_any_element());
        }
        if rows.is_empty() {
            rows.push(
                div()
                    .id("sidebar-empty")
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p_6()
                    .text_center()
                    .text_color(gpui_kit::rgb(0x8b909c))
                    .child("Your opened folders will appear here. Use Open directory below to add one.")
                    .into_any_element(),
            );
        }
        let ghost = self
            .sidebar_drag
            .as_ref()
            .filter(|drag| drag.started)
            .and_then(|drag| {
                self.navigation.session(&drag.session_id).map(|session| {
                    let title = self
                        .terminals
                        .get(&session.id)
                        .map(|terminal| {
                            let status = terminal.shell.session.status();
                            let snapshot = terminal.shell.session.snapshot();
                            session_title(
                                &session.name,
                                status.foreground.app.as_deref(),
                                snapshot.title.as_deref(),
                            )
                        })
                        .unwrap_or_else(|| session.name.clone());
                    (drag.position, title)
                })
            });
        div()
            .id("navigation-panel")
            .relative()
            .h_full()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .min_h_0()
            .text_size(px(13.))
            .font_family(SIDEBAR_FONT_FAMILY)
            .text_color(gpui_kit::rgb(0xd7dae0))
            .when(
                self.sidebar_drag.as_ref().is_some_and(|drag| drag.started),
                |panel| panel.cursor(gpui_kit::CursorStyle::ClosedHand),
            )
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
                    .child(
                        Button::new("open-directory")
                            .text()
                            .text_color(sidebar_faint_ink())
                            .small()
                            .compact()
                            .rounded(px(0.))
                            .w_full()
                            .h(px(26.))
                            .icon(IconName::FolderPlus)
                            .label("Open directory")
                            .accessibility_label("Open directory")
                            .tooltip("Open directory")
                            .disabled(self.opening || self.closing)
                            .on_click(
                                cx.listener(|app, _, window, cx| app.choose_directory(window, cx)),
                            ),
                    ),
            )
            .when_some(drag_dispatcher, |panel, dispatcher| panel.child(dispatcher))
            .when_some(ghost, |panel, (position, name)| {
                panel.child(
                    gpui_kit::deferred(
                        gpui_kit::anchored()
                            .position(gpui_kit::point(px(position.0 + 14.), px(position.1 + 14.)))
                            .snap_to_window()
                            .child(
                                div()
                                    .id("terminal-drag-ghost")
                                    .font_family(SIDEBAR_FONT_FAMILY)
                                    .text_color(gpui_kit::rgb(0xd7dae0))
                                    .max_w(px(240.))
                                    .h(px(26.))
                                    .flex()
                                    .items_center()
                                    .gap(px(7.))
                                    .text_size(px(13.))
                                    .px(px(12.))
                                    .border_1()
                                    .border_color(gpui_kit::rgb(0x5b97ff))
                                    .bg(gpui_kit::rgb(0x252932))
                                    .shadow_md()
                                    .child(Icon::new(IconName::SquareTerminal).size(px(14.)))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .child(name),
                                    ),
                            ),
                    )
                    .with_priority(1),
                )
            })
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
            let sidebar = self.sidebar(window, cx);
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
            .map(|draft| self.worktree_dialog(draft, cx))
            .or_else(|| {
                self.worktree_removal
                    .as_ref()
                    .map(|draft| self.worktree_removal_dialog(draft, cx))
            });
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

fn escape_git_path(path: &str) -> String {
    path.chars()
        .map(|character| {
            if character.is_control() {
                character.escape_default().to_string()
            } else {
                character.to_string()
            }
        })
        .collect()
}

/// Close a worktree dialog only when its non-cancellable Git mutation is idle.
fn close_worktree_dialog(app: &mut TerminalWindow, cx: &mut Context<TerminalWindow>) {
    if let Some(draft) = app.worktree.as_mut() {
        if draft.busy {
            draft.error =
                Some("This worktree is still being created. Wait for it to finish.".into());
        } else {
            app.worktree = None;
        }
    } else if let Some(draft) = app.worktree_removal.as_mut() {
        if draft.busy {
            draft.error = Some("Worktree deletion is in progress. Wait for Git to finish.".into());
        } else {
            app.worktree_removal = None;
        }
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
    let active_checkout_id = state.active_checkout_id.clone();
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
    // Launching a directory selects it by default; a persisted available checkout
    // overrides that fallback after all registered roots have been discovered.
    match navigation::open_directory(cwd) {
        Ok(opened) => nav.merge(opened, true),
        Err(error) => {
            errors.push(error);
            if let Some(home) = nav.home.clone() {
                nav.select_checkout(&home);
            }
        }
    }
    nav.restore_active_checkout(active_checkout_id.as_deref());
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
        resolve_color: Box::new(|index| match index {
            0..=255 => xterm_256(index as u8),
            257 => Palette::default().background,
            258 => Palette::default().cursor,
            _ => Palette::default().foreground,
        }),
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
