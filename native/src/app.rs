//! Native workspace window. Terminal entities are retained by session identity;
//! navigation and folding never own (or restart) a PTY.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex, Weak,
};
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    button::{Button, ButtonCustomVariant, ButtonVariants},
    input::{InputEvent, InputState},
    resizable::{h_resizable, resizable_panel, ResizableState},
    window_border, Disableable, Sizable, TitleBar,
};
use gpui_kit::{
    div, prelude::FluentBuilder, px, size, AnyElement, App, AppContext, Bounds, Context, Entity,
    Focusable, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement,
    PathPromptOptions, Pixels, Point, PromptLevel, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowOptions,
};
// The frame is the app's own on the targets that do not draw one: server-side
// decorations would stack a caption and a set of window controls over the bar
// this window draws for itself.
#[cfg(any(target_os = "linux", target_os = "windows"))]
use gpui_kit::WindowDecorations;
use muster_core::files::FilePreview;
use muster_core::navigation::{self, DiffTotals, GitReadBudget};
use muster_core::workspace::{
    Checkout, Repo, RepoKind, Session, SessionStatus as SavedSessionStatus, SessionType,
};

use crate::navigation::{session_title, Navigation};
use crate::pty::TerminalBackend;
// The test suites drive these two through the window, so the parent module
// names them rather than each suite reaching into the module that owns them.
#[cfg(test)]
use crate::titlebar::{crumb_glyph, CrumbGlyph};
#[cfg(test)]
use crate::worktree_dialog::close_worktree_dialog;

use crate::session_owner::{OwnedShell, SessionOwner, UNMEASURED_CELL};
use crate::sidebar::{
    sidebar_scroll_step, SidebarDrag, SidebarDropRegion, SidebarDropTarget, SidebarRowBounds,
    SIDEBAR_DRAG_THRESHOLD, SIDEBAR_SCROLL_EDGE,
};
use crate::state_writer::StateWriter;
use crate::terminal::{ShellState, TerminalSession, TerminalSessionConfig, TerminalView};
use crate::theme::Roles;
use crate::titlebar::Crumb;
use crate::worktree_dialog::{WorktreeDraft, WorktreeRemovalDraft};

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
/// The regular face and the bold face both go in: the rows set a weight. The
/// icon face goes in with them, because the terminal draws its private use
/// glyphs with it from the first frame.
pub fn load_fonts(cx: &App) {
    if let Err(error) = cx.text_system().add_fonts(vec![
        Cow::Borrowed(include_bytes!("../assets/fonts/FiraCode-Regular.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../assets/fonts/FiraCode-Bold.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../assets/fonts/NerdSymbols.ttf").as_slice()),
    ]) {
        log::error!("muster: bundled sidebar fonts could not load: {error}");
    }
}

pub(crate) fn sidebar_text_width(window: &Window, text: &str, size: f32) -> gpui_kit::Pixels {
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

/// A width per text at one size, by the size it was measured at.
///
/// Two maps rather than one keyed by `(text, size)`: the tree asks for the same
/// name on every frame it draws, and a key spelled out of the name to look that
/// name up is an allocation per row per frame — which is most of what a remembered
/// width would cost.
pub(crate) type WidthCache = HashMap<u32, HashMap<String, f32>>;

/// The width a piece of text is at a size, remembered.
///
/// A panel that scrolls sideways has to know how wide its rows want to be, and it
/// asks every frame it draws. A name does not change between frames, so it is
/// measured once and read ever after: the key is the text and the size, because
/// those are what the width is a function of.
pub(crate) fn measured_width(
    widths: &mut WidthCache,
    window: &Window,
    text: &str,
    size: f32,
) -> f32 {
    let at_size = size.to_bits();
    if let Some(width) = widths.get(&at_size).and_then(|texts| texts.get(text)) {
        return *width;
    }
    let width = sidebar_text_width(window, text, size).as_f32();
    widths
        .entry(at_size)
        .or_default()
        .insert(text.to_owned(), width);
    width
}

/// Keep the identifying tail of a namespaced branch or session title.
pub(crate) fn fitted_name(window: &Window, name: &str, room: f32, size: f32) -> String {
    if sidebar_text_width(window, name, size).as_f32() <= room {
        return name.to_owned();
    }
    let segments = name
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let mut candidate = name.to_owned();
    for start in 1..segments.len() {
        candidate = format!("…/{}", segments[start..].join("/"));
        if sidebar_text_width(window, &candidate, size).as_f32() <= room {
            break;
        }
    }
    candidate
}

pub(crate) fn sidebar_name(window: &Window, name: &str, room: f32) -> String {
    fitted_name(window, name, room, 13.)
}

/// A non-tab-stop focus scope lets sibling controls reveal a row's trailing slot.
pub(crate) fn sidebar_row_focus(
    window: &mut Window,
    cx: &mut App,
    id: &str,
) -> (gpui_kit::FocusHandle, bool) {
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
/// A custom variant keeps its own hover and press surfaces — the same ramp a
/// hand-drawn row answers the pointer with, rather than the accent wash a ghost
/// button paints — but it is neither text nor link, so Kit hands it the default
/// arrow. The row and the control inside it are one target, and the hand is how
/// that says so.
pub(crate) fn sidebar_action_style(cx: &App) -> ButtonCustomVariant {
    let roles = Roles::of(cx);
    ButtonCustomVariant::new(cx)
        .foreground(roles.text_muted)
        .hover(roles.hover)
        .active(roles.pressed)
}

/// The ink a sidebar control wears at rest.
///
/// Set on the control rather than in a variant so the variant's hover ink can
/// win: GPUI refines a hover style after the rest style, which is what makes a
/// control lift to primary ink on hover happen here at all.
pub(crate) fn sidebar_faint_ink(cx: &App) -> gpui_kit::Hsla {
    Roles::of(cx).text_muted
}

pub(crate) fn sidebar_repo_path(
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

/// The `/` that separates one crumb's level from the next.
static OWNERS: Mutex<Vec<Weak<SessionOwner>>> = Mutex::new(Vec::new());
const INITIAL_COLUMNS: u16 = 80;
const INITIAL_ROWS: u16 = 24;
/// How often the window wakes up to consider what is due.
const TICK: Duration = Duration::from_secs(2);

/// How many maintenance ticks pass between two line-count passes, on screen.
///
/// Every tick re-reads Git's worktree list; a line count is a full read of
/// every changed file on top of that, so it runs once every ten seconds rather
/// than once every two.
const COUNT_EVERY_TICKS: u64 = 5;

/// The same interval for a window nobody is looking at.
///
/// A minute, not never: the counts still have to catch up with what Git says
/// while they are unwatched, so returning to a minimised workspace shows a
/// figure that is at most a minute old rather than one from whenever it
/// happened to last be on screen.
const HIDDEN_COUNT_EVERY_TICKS: u64 = 30;

/// The same interval for discovery, off screen.
const HIDDEN_DISCOVER_EVERY_TICKS: u64 = 30;

/// How long one background pass over the repositories may take.
///
/// One deadline for the pass rather than for each Git subprocess: a window
/// with twenty repositories spends twenty seconds, not twenty minutes, and a
/// pass that runs out reports that instead of numbers it did not finish
/// counting.
const SWEEP_BUDGET: Duration = Duration::from_secs(20);

/// How long one checkout's Git reads may take inside that pass.
///
/// The smaller of this and what is left of the sweep. A repository on a
/// network mount or a spinning disk can spend its whole slice and be cut off,
/// while every other checkout is still counted from what remains.
const CHECKOUT_BUDGET: Duration = Duration::from_secs(5);

/// How long one repository's discovery may take inside a pass.
///
/// The same shape as `CHECKOUT_BUDGET`, and for the same reason: the pass
/// shares one deadline, and without a slice of its own the first repository to
/// be slow would spend it on itself and starve every repository listed after it.
const REPO_BUDGET: Duration = Duration::from_secs(5);

/// How many checkouts one pass counts at the same time.
///
/// Enough to overlap the Git reads without asking Git for the whole worktree
/// list N times over: the pass is bounded by the slowest batch, and the batch
/// is bounded by the slowest checkout in it.
const SWEEP_CONCURRENCY: usize = 4;

/// How often the background reads run, in maintenance ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cadence {
    /// Ticks between repository discovery passes.
    pub(crate) discover_every: u64,
    /// Ticks between line-count passes.
    pub(crate) count_every: u64,
}

/// The cadence for a window in this state.
///
/// Hidden slows the reads down and stops the repaints; it does not stop them,
/// because the counts are a fact about the working tree rather than about the
/// window, and a number that only moves while somebody is looking at it is a
/// number that is wrong the moment they look away.
pub(crate) const fn cadence(visible: bool) -> Cadence {
    if visible {
        Cadence {
            discover_every: 1,
            count_every: COUNT_EVERY_TICKS,
        }
    } else {
        Cadence {
            discover_every: HIDDEN_DISCOVER_EVERY_TICKS,
            count_every: HIDDEN_COUNT_EVERY_TICKS,
        }
    }
}

/// One checkout waiting to be counted, with the repository it belongs to.
type CountWork = (String, Repo, Checkout);

/// Count every checkout, at most [`SWEEP_CONCURRENCY`] reads at a time.
///
/// Bounded in three ways that answer three different worries. One
/// [`GitReadBudget`] bounds the whole pass, so the wall-clock cost of the
/// workspace is capped whatever it contains. A per-checkout slice bounds one
/// slow repository, so it cannot spend the pass's time on itself. And the
/// batch size bounds how many Git subprocesses exist at once, so a workspace
/// with a hundred worktrees does not fork a hundred `git diff` processes.
///
/// Every checkout gets an answer, including the ones a deadline cut short: a
/// timeout is a reason to report "not counted", and reporting it is what keeps
/// an unfinished read from looking like an empty working tree.
async fn count_sweep(
    work: Vec<CountWork>,
    executor: gpui_kit::BackgroundExecutor,
) -> Vec<(String, String, Result<DiffTotals, String>)> {
    let sweep = GitReadBudget::new(SWEEP_BUDGET);
    let mut counted = Vec::with_capacity(work.len());
    let mut remaining = work.into_iter();
    loop {
        let batch = remaining
            .by_ref()
            .take(SWEEP_CONCURRENCY)
            .collect::<Vec<_>>();
        if batch.is_empty() {
            break;
        }
        let mut reads = Vec::with_capacity(batch.len());
        for item in batch {
            // The slice is taken before the read starts, so a checkout that
            // finds the sweep nearly over is told so rather than begun.
            let within =
                GitReadBudget::new(sweep.remaining().unwrap_or_default().min(CHECKOUT_BUDGET));
            reads.push(executor.spawn(async move {
                let total = navigation::diff_totals_for_checkout(&item.1, &item.2, within);
                (item.0, item.2.path, total)
            }));
        }
        for read in reads {
            counted.push(read.await);
        }
    }
    counted
}
#[cfg(test)]
pub(crate) use crate::session_owner::{end_shell, host, spawn_and_claim};
#[cfg(test)]
use crate::terminal::CellMetrics;

// Kit Button already has a stable ID, but only exposes the generic interactive
// trait. Use GPUI's native stateful setters on that same interactivity, so fold
// expansion metadata describes the real focused control, not a second node.
pub(crate) struct StatefulButton(Button);
impl InteractiveElement for StatefulButton {
    fn interactivity(&mut self) -> &mut gpui_kit::Interactivity {
        self.0.interactivity()
    }
}
impl StatefulInteractiveElement for StatefulButton {}
pub(crate) trait ButtonA11y {
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

pub(crate) struct LiveTerminal {
    pub(crate) checkout_id: String,
    pub(crate) view: Entity<TerminalView>,
    pub(crate) shell: Arc<OwnedShell>,
}
pub(crate) struct Rename {
    pub(crate) id: String,
    pub(crate) input: Entity<InputState>,
    _subscription: Subscription,
}

/// The workspace window.
///
/// Its state is split across this crate's modules by what a reader is looking
/// at — the sidebar, the crumb line, the worktree dialogs — so every field is
/// reachable from all of them. Splitting the *code* is not the same as
/// splitting the *state*: there is one window, and it has one navigation
/// projection, one terminal registry and one set of dialogs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum InspectorTab {
    #[default]
    Files,
    Changes,
}

pub struct TerminalWindow {
    pub(crate) navigation: Navigation,
    pub(crate) terminals: HashMap<String, LiveTerminal>,
    /// Line counts per checkout, absent when nothing was counted yet.
    ///
    /// Absent means "not counted", never "no changes": a checkout Git could
    /// not be asked about draws nothing rather than a confident zero.
    pub(crate) diff_totals: HashMap<String, DiffTotals>,
    pub(crate) diff_refresh_running: bool,
    pub(crate) diff_refresh_again: bool,
    pub(crate) file_entries: HashMap<(String, String), Vec<muster_core::files::FileEntry>>,
    pub(crate) expanded_files: HashSet<(String, String)>,
    pub(crate) file_loading: HashSet<(String, String)>,
    /// Failures from listing a directory. A read of a file's contents is a
    /// different operation with a different way back, so it owns its own
    /// failures: one key, one owner, and Retry does what actually failed.
    pub(crate) file_errors: HashMap<(String, String), String>,
    pub(crate) document_errors: HashMap<(String, String), String>,
    /// What the opener said about a file or a folder of a checkout that would not
    /// open, keyed by the `(checkout, path)` that was asked for. Kept rather than
    /// logged because the alternative is a gesture that appears to have done
    /// nothing, and drawn under the tree because that is where the row that
    /// asked is. A link is not filed here: it is read in the document pane, so
    /// its refusal belongs in the window's own failure banner.
    pub(crate) external_notices: HashMap<(String, String), String>,
    /// How wide a name is at a size, remembered: the tree is measured every
    /// frame it draws and a name is asked once.
    pub(crate) measured_widths: WidthCache,
    pub(crate) selected_file: HashMap<String, String>,
    pub(crate) open_document: Option<(String, FilePreview)>,
    pub(crate) document_target: Option<(String, String)>,
    pub(crate) document_view: Option<crate::document::DocumentView>,
    pub(crate) open_diff: Option<(String, Option<String>)>,
    pub(crate) expanded_diffs: HashSet<(String, String)>,
    pub(crate) file_request: HashMap<(String, String), u64>,
    pub(crate) document_request: u64,
    pub(crate) inspector_tab: InspectorTab,
    pub(crate) git_status: HashMap<String, navigation::GitStatus>,
    /// Each checkout's changed files by path, for the file tree.
    ///
    /// Built with the status it indexes and dropped with it, so the tree reads one
    /// row's answer in a hash lookup rather than walking every changed file in the
    /// workspace to find it. A refresh that failed keeps the answer the window
    /// already had, and this is that answer indexed.
    pub(crate) status_index: HashMap<String, crate::inspector::StatusIndex>,
    pub(crate) git_status_errors: HashMap<String, String>,
    pub(crate) git_status_request: HashMap<String, u64>,
    pub(crate) git_status_running: HashSet<String>,
    pub(crate) git_diff: HashMap<(String, String), navigation::FileDiff>,
    pub(crate) git_diff_errors: HashMap<(String, String), String>,
    pub(crate) git_diff_request: HashMap<(String, String), u64>,
    pub(crate) diff_view: Option<crate::diff::DiffView>,
    /// The diff reads this window still wants, and how many are running.
    pub(crate) diff_queue: crate::inspector::DiffQueue,
    pub(crate) inspector_width: f32,
    pub(crate) next_file_request: u64,
    pub(crate) worktree: Option<WorktreeDraft>,
    pub(crate) worktree_removal: Option<WorktreeRemovalDraft>,
    pub(crate) owner: Arc<SessionOwner>,
    pub(crate) failure: Option<String>,
    pub(crate) notice: Option<String>,
    pub(crate) opening: bool,
    pub(crate) open_repo_menu: Option<String>,
    /// Which crumb's menu is open, so opening one crumb closes the others.
    pub(crate) open_crumb: Option<Crumb>,
    pub(crate) closing: bool,
    pub(crate) closing_sessions: std::collections::HashSet<String>,
    pub(crate) removing_checkouts: HashSet<String>,
    pub(crate) membership_revision: u64,
    /// What the person last said this window should show.
    ///
    /// A checkout row cannot tell a dirty checkout from a clean one until Git
    /// answers, and the answer arrives after the click. Every path that
    /// replaces what the window shows clears the document first, so that is
    /// where this moves, and a decision made from a late answer is dropped when
    /// this no longer carries it.
    pub(crate) view_intent: u64,
    /// A checkout row click waiting for Git to say whether there is a change set.
    pub(crate) pending_row_choice: Option<crate::diff::RowChoice>,
    pub(crate) latest_terminal_request: Option<String>,
    pub(crate) editing: Option<Rename>,
    pub(crate) sidebar_drag: Option<SidebarDrag>,
    pub(crate) suppress_sidebar_click: bool,
    pub(crate) resize: Entity<ResizableState>,
    pub(crate) scroll: ScrollHandle,
    /// Where the file tree is scrolled to.
    ///
    /// Its own, apart from the sidebar's, because two things read it that nothing else
    /// writes: the folder stack has to know which row is on screen, and a click on one
    /// of those folders has to bring it back into view.
    pub(crate) files_scroll: ScrollHandle,
    pub(crate) focus_pending: bool,
    /// Whether the platform is presenting this window's frames.
    ///
    /// From the visibility observer rather than from a frame: a hidden window
    /// is not asked to draw, so reading it during render would pin it at
    /// whatever it last was.
    pub(crate) visible: bool,
    /// Injected-session tests exercise the same view/focus path without touching
    /// the person's state file, directory picker or session ownership.
    pub(crate) injected: bool,
    pub(crate) _maintenance: Option<Task<()>>,
    /// Keeps the visibility observer registered for this window's life.
    pub(crate) _visibility: Subscription,
}

impl TerminalWindow {
    fn empty(backend: Arc<TerminalBackend>, owns_backend: bool, cx: &mut Context<Self>) -> Self {
        Self {
            navigation: Navigation::new(),
            terminals: HashMap::new(),
            diff_totals: HashMap::new(),
            diff_refresh_running: false,
            diff_refresh_again: false,
            file_entries: HashMap::new(),
            expanded_files: HashSet::new(),
            file_loading: HashSet::new(),
            file_errors: HashMap::new(),
            document_errors: HashMap::new(),
            external_notices: HashMap::new(),
            measured_widths: HashMap::new(),
            selected_file: HashMap::new(),
            open_document: None,
            document_target: None,
            document_view: None,
            open_diff: None,
            expanded_diffs: HashSet::new(),
            file_request: HashMap::new(),
            document_request: 0,
            inspector_tab: InspectorTab::Files,
            git_status: HashMap::new(),
            status_index: HashMap::new(),
            git_status_errors: HashMap::new(),
            git_status_request: HashMap::new(),
            git_status_running: HashSet::new(),
            git_diff: HashMap::new(),
            git_diff_errors: HashMap::new(),
            git_diff_request: HashMap::new(),
            diff_view: None,
            diff_queue: Default::default(),
            inspector_width: 280.,
            next_file_request: 0,
            worktree: None,
            worktree_removal: None,
            owner: SessionOwner::new(backend, owns_backend),
            failure: None,
            notice: None,
            opening: false,
            open_repo_menu: None,
            open_crumb: None,
            closing: false,
            closing_sessions: Default::default(),
            removing_checkouts: Default::default(),
            membership_revision: 0,
            view_intent: 0,
            pending_row_choice: None,
            latest_terminal_request: None,
            editing: None,
            sidebar_drag: None,
            suppress_sidebar_click: false,
            resize: cx.new(|_| ResizableState::default()),
            scroll: ScrollHandle::new(),
            files_scroll: ScrollHandle::new(),
            focus_pending: true,
            visible: true,
            injected: false,
            _maintenance: None,
            _visibility: Subscription::new(|| {}),
        }
    }

    /// Watch whether the platform is presenting this window, and repaint on the
    /// way back in.
    ///
    /// Returning to a minimised window is the moment the numbers, the worktree
    /// list and the terminal rows are most likely to be out of date, and it is
    /// also the moment nothing else in this crate would ask for a frame.
    fn observe_visibility(&mut self, window: &Window, cx: &mut Context<Self>) {
        let weak = cx.weak_entity();
        self._visibility = window.observe_window_visibility(move |visibility, _, cx| {
            let _ = weak.update(cx, |app, cx| {
                app.visible = visibility.is_visible();
                cx.notify();
            });
        });
        self.visible = window.is_visible();
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
        app.observe_visibility(window, cx);
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
                    Ok(writer) => app.owner.install_writer(writer),
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
        app.attach_refusals(&view, cx);
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

    /// Route one terminal view's refusals into the banner this window draws.
    ///
    /// GPUI subscriptions hold a weak window reference; neither entity is kept
    /// alive by the event path. Messages never include the refused payload.
    fn attach_refusals(&mut self, view: &Entity<TerminalView>, cx: &mut Context<Self>) {
        cx.subscribe(view, |app, _, event: &crate::terminal::InputRefused, cx| {
            app.failure = Some(event.0.clone());
            cx.notify();
        })
        .detach();
    }

    pub(crate) fn persist(&self) {
        self.owner.persist(self.navigation.persistent_state());
    }

    pub(crate) fn request_terminal(
        &mut self,
        checkout_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        // Asking for a terminal is the intent to see one, so whatever central
        // view is open now is what the person chose to replace. The token moves
        // here rather than when the shell lands: a preview whose read is still
        // running must not come back after the terminal has taken its place.
        self.clear_document();
        self.open_diff = None;
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
                            app.attach_refusals(&terminal, cx);
                            app.terminals.insert(
                                id.clone(),
                                LiveTerminal {
                                    checkout_id: terminal_checkout_id,
                                    view: terminal,
                                    shell,
                                },
                            );
                            // Only while the terminal is still what is being
                            // asked for. A preview opened while this shell was
                            // spawning is what the person is looking at now, and
                            // a late answer never takes it back.
                            if app.navigation.active_session.as_deref() == Some(&id)
                                && app.open_document.is_none()
                                && app.document_target.is_none()
                                && app.open_diff.is_none()
                            {
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

    pub(crate) fn focus_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(terminal) = self.terminal() {
            terminal.read(cx).focus_handle(cx).focus(window, cx);
        }
        self.focus_pending = false;
    }

    pub(crate) fn choose_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    /// The window's background loop: one wake every [`TICK`], and on each wake
    /// whatever the window's current visibility says is due.
    ///
    /// Every Git read this window makes comes from here or from a change the
    /// person made, and each pass carries its own [`GitReadBudget`]. A pass that
    /// runs out of time reports that; it never reports a number it did not
    /// finish counting.
    fn start_maintenance(&mut self, cx: &mut Context<Self>) {
        let background = cx.background_executor().clone();
        self._maintenance = Some(cx.spawn(async move |view, cx| {
            let mut ticks: u64 = 0;
            loop {
                background.timer(TICK).await;
                ticks += 1;
                let Ok((epoch, roots, closing)) = view.update(cx, |app, cx| {
                    if let Some(error) = app.owner.writer_failure() {
                        app.failure = Some(error);
                        cx.notify();
                    }
                    let cadence = cadence(app.visible);
                    // A hidden window is not looked at, so it is not repainted
                    // on a timer either. It becomes visible again through the
                    // visibility observer, which repaints on the way back.
                    if app.visible {
                        // Status changes repaint inactive terminal rows too.
                        cx.notify();
                    }
                    // Counted far less often than membership is refreshed: a
                    // line total is a full Git read of every changed file, and
                    // this is a background refresh, not a Git watcher.
                    if ticks.is_multiple_of(cadence.count_every) {
                        app.refresh_diff_totals(cx);
                    }
                    if ticks.is_multiple_of(cadence.count_every) {
                        if let Some(id) = app.navigation.active_checkout.clone() {
                            if (app.navigation.panels_visible || app.open_diff.is_some())
                                && !app.git_status_running.contains(&id)
                            {
                                app.request_git_status(&id, cx);
                            }
                        }
                    }
                    (
                        app.membership_revision,
                        if ticks.is_multiple_of(cadence.discover_every) {
                            app.navigation
                                .repos
                                .iter()
                                .filter(|repo| repo.kind == RepoKind::Git)
                                .map(|repo| (repo.id.clone(), repo.root.clone()))
                                .collect::<Vec<_>>()
                        } else {
                            Vec::new()
                        },
                        app.closing,
                    )
                }) else {
                    return;
                };
                if closing {
                    return;
                }
                // An empty root list is a tick with no discovery due. It is also
                // how a hidden window spends its time: still awake, still
                // counting lines, but not asking Git for anything it has
                // already been told.
                if roots.is_empty() {
                    continue;
                }
                let sweep = GitReadBudget::new(SWEEP_BUDGET);
                let mut results = Vec::with_capacity(roots.len());
                for batch in roots.chunks(SWEEP_CONCURRENCY) {
                    let reads = batch
                        .iter()
                        .cloned()
                        .map(|(id, root)| {
                            let within = GitReadBudget::new(
                                sweep.remaining().unwrap_or_default().min(REPO_BUDGET),
                            );
                            background.spawn(async move {
                                (
                                    id,
                                    navigation::open_directory_with_budget(
                                        Path::new(&root),
                                        within,
                                    ),
                                )
                            })
                        })
                        .collect::<Vec<_>>();
                    for read in reads {
                        results.push(read.await);
                    }
                }
                if view
                    .update(cx, |app, cx| {
                        if app.closing || app.membership_revision != epoch {
                            return false;
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
                        true
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

    /// Count the line changes in every checkout this window can see.
    ///
    /// Each checkout is counted from the repository and the checkout this
    /// window already discovered: `diff_totals_for_checkout` revalidates the
    /// identity it is handed and never rediscovers, so a sweep is one Git read
    /// per checkout instead of a fresh `open_directory` per checkout first.
    ///
    /// Overlapping requests collapse into one more pass rather than a second
    /// sweep, and a pass whose result would describe a membership this window
    /// has since left is dropped by [`Self::apply_diff_totals`].
    pub(crate) fn refresh_diff_totals(&mut self, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        let work = self.diff_checkout_work();
        self.diff_totals
            .retain(|id, _| work.iter().any(|(checkout_id, ..)| checkout_id == id));
        let present = |id: &String| self.navigation.checkout(id).is_some();
        self.git_status.retain(|id, _| present(id));
        self.status_index.retain(|id, _| present(id));
        self.git_status_errors.retain(|id, _| present(id));
        self.git_status_request.retain(|id, _| present(id));
        if self.diff_refresh_running {
            self.diff_refresh_again = true;
            return;
        }
        if work.is_empty() {
            return;
        }
        let revision = self.membership_revision;
        self.diff_refresh_running = true;
        let executor = cx.background_executor().clone();
        let worker = executor.clone().spawn({
            let executor = executor.clone();
            async move { count_sweep(work, executor).await }
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

    /// The checkouts this window should be counting, each with the repository
    /// it belongs to and the path it was last known to be at.
    ///
    /// The open checkout is counted first: it is the one a person is looking
    /// at, so it is the one that must survive a sweep that runs out of time.
    fn diff_checkout_work(&self) -> Vec<(String, Repo, Checkout)> {
        let mut work = Vec::new();
        for repo in &self.navigation.repos {
            if repo.kind != RepoKind::Git {
                continue;
            }
            for checkout in &repo.checkouts {
                if checkout.is_missing || self.navigation.archived.contains(&checkout.id) {
                    continue;
                }
                work.push((checkout.id.clone(), repo.clone(), checkout.clone()));
            }
        }
        if let Some(open) = &self.navigation.active_checkout {
            if let Some(index) = work.iter().position(|(id, ..)| id == open) {
                work.swap(0, index);
            }
        }
        work
    }

    /// Record one pass's answers.
    ///
    /// A checkout that could not be counted loses its figure and draws
    /// nothing: an absent figure is "not counted", and a figure of zero is a
    /// claim about the working tree that a timed-out read is not entitled to
    /// make. Whatever was counted before a failure stays counted nowhere for
    /// that checkout until a later pass answers, so a row never shows a stale
    /// number as though it were this pass's.
    fn apply_diff_totals(
        &mut self,
        revision: u64,
        totals: Vec<(String, String, Result<DiffTotals, String>)>,
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
                Err(error) => {
                    log::debug!("muster: {path}: {error}");
                    self.diff_totals.remove(&id);
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn request_close_terminal(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    pub(crate) fn request_window_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(crate) fn start_rename(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(crate) fn open_worktree_dialog(
        &mut self,
        repo_id: &str,
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

    pub(crate) fn open_worktree_removal_dialog(
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

    pub(crate) fn checkout_has_terminal_activity(&self, checkout_id: &str) -> bool {
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
    pub(crate) fn submit_worktree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(crate) fn begin_sidebar_drag(
        &mut self,
        id: &str,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
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

    pub(crate) fn cancel_sidebar_drag(&mut self) {
        if self.sidebar_drag.take().is_some() {
            self.suppress_sidebar_click = true;
        }
    }

    /// All insertion indices exclude the dragged session, just like the final list.
    pub(crate) fn sidebar_drop_slot(&self, checkout_id: &str, index: usize) -> usize {
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

    pub(crate) fn update_sidebar_drag(
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

    pub(crate) fn finish_sidebar_drag(
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

    pub(crate) fn tick_sidebar_drag(
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

    pub(crate) fn move_terminal(
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

    pub(crate) fn request_dismiss_missing_checkout(
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

    pub(crate) fn submit_worktree_archive(&mut self, cx: &mut Context<Self>) {
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

    pub(crate) fn submit_worktree_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(crate) fn restore_archived(
        &mut self,
        repo_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    pub(crate) fn remove_repo(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
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
            self.navigation
                .pending
                .values()
                .any(|pending| pending == &checkout.id)
        }) {
            self.failure = Some("A worktree is still being created here. Wait for Git to finish before removing the directory.".into());
            cx.notify();
            return;
        }
        // The terminals come with the group. Asking for them to be closed first is
        // a detour that never ends: a launch opens one in the active workdir, and
        // this is that workdir, so the directory the person just opened is the one
        // they can never take back off the panel.
        let terminals = repo
            .checkouts
            .iter()
            .map(|checkout| checkout.sessions.len())
            .sum::<usize>();
        let message = format!("Remove “{}” from the panel?", repo.root);
        let detail = match terminals {
            0 => "No files will be deleted. Opening the directory again brings it back.".to_owned(),
            1 => "No files will be deleted. Its 1 running terminal will be closed. Opening the directory again brings it back.".into(),
            n => format!("No files will be deleted. Its {n} running terminals will be closed. Opening the directory again brings it back."),
        };
        let prompt = window.prompt(
            PromptLevel::Warning,
            &message,
            Some(&detail),
            &["Cancel", "Remove from panel"],
            cx,
        );
        let id = id.to_owned();
        cx.spawn_in(window, async move |view, cx| {
            if prompt.await == Ok(1) {
                let _ = view.update_in(cx, |app, window, cx| {
                    // Revalidate after the prompt: the group can be gone, or a
                    // worktree can be on its way into it, while it was open.
                    let (removed, sessions) = {
                        let Some(repo) = app.navigation.repos.iter().find(|repo| repo.id == id) else {
                            return;
                        };
                        if app.navigation.pending.values().any(|pending| repo.checkouts.iter().any(|checkout| checkout.id == *pending)) {
                            app.failure = Some("A worktree is still being created here. Wait for Git to finish before removing the directory.".into());
                            return;
                        }
                        (
                            repo.checkouts.iter().map(|checkout| checkout.id.clone()).collect::<Vec<_>>(),
                            repo.checkouts.iter().flat_map(|checkout| checkout.sessions.iter().map(|session| session.id.clone())).collect::<Vec<_>>(),
                        )
                    };
                    for session in &sessions { app.close_terminal(session, window, cx); }
                    app.navigation.repos.retain(|repo| repo.id != id);
                    for checkout in &removed { app.navigation.archived.remove(checkout); }
                    if app.navigation.active_checkout.as_ref().is_some_and(|checkout| removed.contains(checkout)) {
                        app.navigation.active_checkout = None; app.navigation.active_session = None;
                    }
                    app.navigation.revision += 1;
                    app.membership_revision += 1; app.persist(); app.refresh_diff_totals(cx);
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
        // After the two Escapes above, because both of them are about something
        // that is in front of the document: a drag in progress and a dialog are
        // the person's Escape, and a draft is only what is left over.
        if self.document_edit_key(&event.keystroke, window, cx) {
            cx.stop_propagation();
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
                if self.open_document.is_none()
                    && self.document_target.is_none()
                    && self.open_diff.is_none()
                {
                    self.focus_terminal(window, cx);
                }
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
    pub(crate) fn line_slot(counts: Option<&DiffTotals>) -> Option<(u64, u64)> {
        let totals = counts?;
        (totals.additions + totals.deletions > 0).then_some((totals.additions, totals.deletions))
    }

    /// A task name as the directory and branch it suggests.
    ///
    /// Legacy's rule, kept rather than invented: letters, digits, `_`, `.` and
    /// `-` survive, everything else collapses to one `-`, and a name that
    /// reduces to nothing falls back to `new-task` rather than creating a
    /// worktree called `.worktrees/`.
    pub(crate) fn slug_for(value: &str) -> String {
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

    /// The name a session is drawn under: what the live process says it is
    /// doing, or what it was launched as when nothing answers.
    ///
    /// The same name in the crumb and in the sidebar, so the menu of the open
    /// terminal and the row that opened it cannot say two different things
    /// about the same session.
    pub(crate) fn session_label(&self, session: &Session) -> String {
        self.terminals
            .get(&session.id)
            .map(|terminal| {
                let status = terminal.shell.session.status();
                // The title alone: a full snapshot would build the whole
                // visible grid to read one string the grid already holds.
                session_title(
                    &session.name,
                    status.foreground.app.as_deref(),
                    terminal.shell.session.title().as_deref(),
                )
            })
            .unwrap_or_else(|| session.name.clone())
    }
}

impl Render for TerminalWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let roles = Roles::of(cx);
        if self.focus_pending
            && self.open_document.is_none()
            && self.document_target.is_none()
            && self.open_diff.is_none()
        {
            self.focus_terminal(window, cx);
        }
        let background = roles.surface_base;
        let text = roles.text;
        let body: AnyElement = if let Some(diff) = self.diff_element(window, cx) {
            diff
        } else if let Some(document) = self.document_element(window, cx) {
            document
        } else if let Some(terminal) = self.terminal() {
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
                        .when(self.opening || self.closing, |button| {
                            button
                                .bg(roles.disabled_surface)
                                .text_color(roles.text_disabled)
                                .border_color(gpui_kit::transparent_black())
                        })
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
            let inspector = self.files_inspector(window, cx);
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
                .child(
                    resizable_panel()
                        .flex_none()
                        .size(px(self.inspector_width))
                        .size_range(px(200.)..px(480.))
                        .child(inspector),
                )
                .on_resize(cx.listener(|app, state: &Entity<ResizableState>, _, cx| {
                    let sizes = state.read(cx).sizes();
                    if let Some(width) = sizes.first() {
                        app.navigation.sidebar_width = width.as_f32().clamp(240., 500.);
                        app.persist();
                    }
                    if let Some(width) = sizes.get(2) {
                        app.inspector_width = width.as_f32().clamp(200., 480.);
                    }
                    cx.notify();
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
        // A failure and a notice are the same bar in the same place, and they
        // are not the same thing: one says something went wrong, the other says
        // something happened. Two inks, so they can be told apart without
        // reading them.
        let banner = self
            .failure
            .as_ref()
            .map(|message| (message.clone(), roles.status.warning))
            .or_else(|| {
                self.notice
                    .as_ref()
                    .map(|message| (message.clone(), roles.status.info))
            });
        // On a target whose window has no frame of its own, the frame is this
        // one: it insets the content out of the shadow, draws the border and
        // owns the resize band. It is a pass-through everywhere else.
        window_border().child(
            div()
                .id("workspace-root")
                .size_full()
                .flex()
                .flex_col()
                .bg(background)
                .text_color(text)
                .capture_key_down(cx.listener(Self::on_key))
                .child(self.titlebar(window, cx))
                // The one place this window says something went wrong.
                .when_some(banner, |root, (message, ink)| {
                    root.child(
                        div()
                            .flex()
                            .items_center()
                            .px_2()
                            .text_size(px(12.))
                            .text_color(ink)
                            .child(div().flex_1().child(message))
                            .child(
                                Button::new("dismiss-message")
                                    .custom(sidebar_action_style(cx))
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
                })
                .child(content)
                .when_some(dialog, |root, dialog| root.child(dialog)),
        )
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
pub(crate) fn toggle(set: &mut std::collections::HashSet<String>, id: &str) {
    if !set.remove(id) {
        set.insert(id.into());
    }
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
        // What the window may not go below: narrower than this and the sidebar
        // and a readable terminal cannot both keep a column.
        window_min_size: Some(size(px(900.), px(600.))),
        // The name the platform shows where there is no app-drawn bar to carry
        // it, over the transparent overlay the bar draws into.
        titlebar: Some(TitlebarOptions {
            title: Some("Muster".into()),
            ..TitleBar::title_bar_options()
        }),
        app_id: Some("dev.muster.workspace".into()),
        // Opaque from the first frame, in the terminal palette's own
        // background: a transparent launch shows the desktop through a window
        // that has nothing transparent to show.
        window_background: WindowBackgroundAppearance::Opaque,
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        window_decorations: Some(WindowDecorations::Client),
        // The bar draws and moves the window itself, so AppKit must not also
        // read a title bar there.
        ..TitleBar::window_options()
    }
}
#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "sidebar_tests.rs"]
mod sidebar_tests;

#[cfg(test)]
#[path = "titlebar_tests.rs"]
mod titlebar_tests;
