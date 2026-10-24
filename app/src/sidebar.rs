//! The left panel: repositories, their worktrees, and the terminals in them.
//!
//! The sidebar is one list drawn four ways — a repository heading, a worktree
//! row, a terminal row, and the rows naming the files and change sets this window
//! has open in that workdir — and it is also the window's drag surface: a
//! terminal can be picked up and dropped into another worktree, and the drop slot
//! has to appear where the row would have been, not where the row happens to be.
//!
//! So the drag is not a decoration on top of the list. Both halves live here:
//! the rows, and the geometry the gesture is resolved against.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::base::Button as BaseButton;
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    input::Input,
    menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem},
    scroll::ScrollableElement,
    spinner::Spinner,
    Disableable, Icon, Sizable,
};
use gpui_kit::{
    canvas, div, prelude::FluentBuilder, px, AnyElement, App, Bounds, Context, Hsla,
    InteractiveElement, IntoElement, Length, MouseButton, MouseDownEvent, MouseExitEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, SharedString, StatefulInteractiveElement,
    Styled, TestSupportExt, Window,
};
use muster_core::workspace::RepoKind;

use crate::agent_watch::AgentRowState;
use crate::app::{
    baseline_from_bottom, sidebar_faint_ink, sidebar_name, sidebar_repo_path, sidebar_row_focus,
    sidebar_text_width, toggle, ButtonA11y, TerminalWindow, ViewKind, UI_FONT_FAMILY,
};
use crate::file_icons;
use crate::navigation::{checkout_title, session_title, ForegroundProgram};
use crate::terminal::{PiTaskState, ShellState};
use crate::theme::Roles;
use crate::tooltip::RestingTooltip;

pub(crate) const SIDEBAR_DRAG_THRESHOLD: f32 = 5.0;
pub(crate) const SIDEBAR_SCROLL_EDGE: f32 = 36.0;

/// The sizes the repository heading's name and path are drawn at.
///
/// The two are one line of text, so their baselines have to meet: the sizes
/// are named here because the baseline of the pair is computed from them.
pub(crate) const HEADING_NAME_SIZE: f32 = 14.;
pub(crate) const HEADING_PATH_SIZE: f32 = 11.;

pub(crate) fn foreground_program_icon(
    program: ForegroundProgram,
    size: Pixels,
    color: Hsla,
) -> AnyElement {
    match program {
        ForegroundProgram::OpenCode => Icon::new(IconName::Sparkles)
            .size(size)
            .text_color(color)
            .into_any_element(),
        ForegroundProgram::Pi => div()
            .flex()
            .flex_shrink_0()
            .size(size)
            .items_center()
            .justify_center()
            .font_family(UI_FONT_FAMILY)
            .text_size(size)
            .line_height(size)
            .text_color(color)
            .child("π")
            .into_any_element(),
    }
}

/// The glyph a terminal row wears in front of its name.
///
/// The program's own, unless a turn is running: that is the one state that
/// moves, and a static list cannot say that about itself. A session that is
/// waiting for an answer wears a question instead, because the answer is the
/// only thing the person can give it.
fn session_glyph(
    program: Option<ForegroundProgram>,
    working: bool,
    asking: bool,
    size: Pixels,
    color: Hsla,
) -> AnyElement {
    if working {
        return Spinner::new()
            .with_size(size)
            .color(color)
            .into_any_element();
    }
    if asking {
        return Icon::new(IconName::BadgeQuestionMark)
            .size(size)
            .text_color(color)
            .into_any_element();
    }
    program.map_or_else(
        || {
            Icon::new(IconName::SquareTerminal)
                .size(size)
                .text_color(color)
                .into_any_element()
        },
        |program| foreground_program_icon(program, size, color),
    )
}

/// The ink an OpenCode row's glyph wears.
///
/// The agent's own colour on every state that is the agent's: idle, working and
/// waiting. A question is the agent at work, so the question wears the agent's
/// colour too rather than the panel's warning: the glyph says what happened and
/// the colour says who. A failed turn keeps the error ink, because that one is
/// the row's own news, and a state nobody could read keeps the warning one.
pub(crate) fn agent_row_ink(roles: &Roles, state: Option<&AgentRowState>) -> Hsla {
    match state {
        Some(state) if state.failed => roles.status.danger_text,
        Some(state) if state.unavailable => roles.status.warning,
        Some(state) => state
            .tint
            .as_deref()
            .and_then(crate::theme::service_tint)
            .unwrap_or(roles.text_muted),
        None => roles.text_muted,
    }
}

/// What a terminal row announces.
///
/// The name, the agent's name and how long ago the service last touched the
/// session, and the state in words: the state is drawn as a colour on a glyph,
/// and a colour is not something a screen reader reads, so it is spelled out
/// here. The words are the ones the row shows, so the row and the name it
/// announces cannot disagree about what the session is doing.
pub(crate) fn session_row_label(
    title: &str,
    agent_suffix: Option<&str>,
    state_note: &str,
) -> String {
    let mut said = format!("Terminal session: {title}");
    if let Some(suffix) = agent_suffix.filter(|suffix| !suffix.is_empty()) {
        said.push_str(", ");
        said.push_str(suffix);
    }
    said.push_str(", ");
    said.push_str(state_note);
    said
}

/// What a terminal row wearing Pi's glyph says about the task in front of it.
///
/// Three answers and two inks: a turn that is running moves and wears the
/// accent, a turn that failed wears the danger ink, and everything else is
/// neutral. A turn that stopped to ask something is neutral too: the panel has
/// no shape for a question this program asked, so the row says nothing about
/// it rather than inventing a state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PiRowState {
    /// The words the row says beside its name.
    pub(crate) note: &'static str,
    /// Whether a turn is running, which is the one state that moves.
    pub(crate) working: bool,
    /// Whether the last turn failed.
    pub(crate) failed: bool,
}

/// What a row says about the task in front of it, given what Pi reported.
pub(crate) fn pi_row_state(task: Option<PiTaskState>) -> PiRowState {
    let note = crate::terminal::pi_task_note(task);
    match task {
        Some(PiTaskState::Working) => PiRowState {
            note,
            working: true,
            failed: false,
        },
        Some(PiTaskState::Error) => PiRowState {
            note,
            working: false,
            failed: true,
        },
        _ => PiRowState {
            note,
            working: false,
            failed: false,
        },
    }
}

pub(crate) fn sidebar_scroll_step(distance: f32) -> f32 {
    distance.signum() * (distance.abs() / 2.).clamp(2., 14.)
}

/// What kind of work an item is.
///
/// The same path is two different things when it is being edited and when its
/// change is being read, so an item says which: a draft in `src/main.rs` and a
/// review of it are two rows, not two names for one.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenKind {
    Document,
    Diff,
}

/// One piece of work this window is in the middle of.
///
/// Not something a person opened and not something they are looking at now: a
/// file they only read is not unfinished work, and neither is a file they are
/// editing somewhere else. `count` is what is waiting on it — a draft is one
/// thing being typed, and a review is however many notes it holds.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct OpenItem {
    pub(crate) checkout_id: String,
    pub(crate) kind: OpenKind,
    pub(crate) path: String,
    pub(crate) count: usize,
}

impl OpenItem {
    /// The name the row wears: the item, and what is waiting on it.
    ///
    /// One string, not a label with a badge beside it, so `notes.txt (note)` is
    /// what a shortened name, a tooltip and a screen reader all say.
    fn label(&self) -> String {
        match (self.kind, self.count) {
            (OpenKind::Document, _) => format!("{} (edited)", self.path),
            (OpenKind::Diff, 1) => format!("{} (note)", self.path),
            (OpenKind::Diff, count) => format!("{} ({count} notes)", self.path),
        }
    }

    /// The file whose icon the row wears, from the name it resolves.
    fn file(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// Whether the pane is showing this item right now.
    ///
    /// The reading still in flight counts as showing it: the document pane is
    /// on screen with its file being read, so the terminal underneath it is not
    /// what this window is looking at.
    fn shown_by(&self, app: &TerminalWindow) -> bool {
        let document = app
            .document_target
            .as_ref()
            .map(|(checkout, path)| (checkout, path.as_str()))
            .or_else(|| {
                app.open_document
                    .as_ref()
                    .map(|(checkout, preview)| (checkout, preview.path.as_str()))
            });
        let active = app.navigation.active_checkout.as_deref() == Some(&self.checkout_id);
        match self.kind {
            OpenKind::Document => {
                active
                    && document.is_some_and(|(checkout, path)| {
                        checkout == &self.checkout_id && path == self.path
                    })
            }
            OpenKind::Diff => {
                active
                    && app.open_diff.as_ref().is_some_and(|(checkout, path)| {
                        checkout == &self.checkout_id && path.as_deref() == Some(&self.path)
                    })
            }
        }
    }
}

impl TerminalWindow {
    /// The work in `checkout` that the panel lists, in the order it draws it:
    /// what is being typed, then what is being reviewed.
    ///
    /// A read of live state rather than a list kept in step with it, which is
    /// what makes "clicking through files adds nothing" true by construction
    /// rather than by remembering not to add: a file is here while the document
    /// holds something the file does not, and a diff is here while it holds a
    /// note. Clicking ten files and typing in none of them adds nothing because
    /// there is nothing to add.
    ///
    /// A whole change set is not a row of its own. Its notes belong to the files
    /// they were written on, and naming each of those is what a person can click
    /// back into.
    fn items_in(&self, checkout: &str) -> Vec<OpenItem> {
        let active = self.navigation.active_checkout.as_deref() == Some(checkout);
        let mut items = Vec::new();
        let document = self
            .document_target
            .as_ref()
            .map(|(id, path)| (id, path.as_str()))
            .or_else(|| {
                self.open_document
                    .as_ref()
                    .map(|(id, preview)| (id, preview.path.as_str()))
            });
        if active
            && self
                .document_view
                .as_ref()
                .is_some_and(|view| view.is_dirty())
        {
            if let Some((_, path)) = document.filter(|(id, _)| id.as_str() == checkout) {
                items.push(OpenItem {
                    checkout_id: checkout.to_owned(),
                    kind: OpenKind::Document,
                    path: path.to_owned(),
                    count: 1,
                });
            }
        }
        items.extend(
            self.review
                .noted_paths(checkout)
                .into_iter()
                .map(|(path, count)| OpenItem {
                    checkout_id: checkout.to_owned(),
                    kind: OpenKind::Diff,
                    path: path.to_owned(),
                    count,
                }),
        );
        items
    }

    /// The item the pane is showing, if it is showing one of the panel's rows.
    ///
    /// Every checkout is asked, because the pane only shows the active one and a
    /// row under any other workdir is by definition not it.
    fn shown_item(&self) -> Option<OpenItem> {
        let checkout = self.navigation.active_checkout.as_deref()?;
        self.items_in(checkout)
            .into_iter()
            .find(|item| item.shown_by(self))
    }

    /// Open an item from the panel.
    ///
    /// The same gesture the person used to get there the first time, and
    /// through the same decision: leaving a draft for a review asks about the
    /// draft exactly as leaving it for a terminal does.
    pub(crate) fn request_open_item(
        &mut self,
        item: OpenItem,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        if item.shown_by(self) {
            return;
        }
        self.request_document_transition(
            Box::new(move |app, window, cx| app.open_item_now(item, window, cx)),
            window,
            cx,
        );
    }

    /// Open the view the row names.
    ///
    /// The row is the way back to a file that was open, so a click on it is
    /// the same gesture that opened it the first time. A view the pane is
    /// already showing is left alone: there is nowhere to go.
    pub(crate) fn open_pinned_view(
        &mut self,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        if self.shows_pinned_view() {
            return;
        }
        let Some(pin) = self.view_pin.clone() else {
            return;
        };
        match pin.kind {
            ViewKind::Preview => {
                let Some(path) = pin.path.clone() else {
                    return;
                };
                let Some((repo, checkout)) = self.document_source(&pin.checkout_id) else {
                    return;
                };
                // The pane only shows a view of the workdir it is on, so the
                // row takes the pane there first.
                self.navigation.select_checkout(&pin.checkout_id);
                self.persist();
                self.request_document(repo, checkout, path, window, cx);
            }
            ViewKind::Diff => {
                self.request_open_changes(&pin.checkout_id, pin.path.clone(), window, cx);
            }
        }
    }

    /// Close the view the row names.
    ///
    /// The pane goes back to the terminal when it was showing it, and the row
    /// goes with it: a close is a close. A view the pane has already left is
    /// only the row, so closing it closes nothing else.
    pub(crate) fn close_pinned_view(
        &mut self,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pin) = self.view_pin.clone() else {
            return;
        };
        if !self.shows_pinned_view() {
            self.view_pin = None;
            cx.notify();
            return;
        }
        match pin.kind {
            ViewKind::Preview => {
                // Leaving a draft asks about the draft, exactly as the pane's
                // own close does.
                self.request_document_transition(
                    Box::new(|app, window, cx| {
                        app.view_pin = None;
                        app.clear_document();
                        app.focus_terminal(window, cx);
                        cx.notify();
                    }),
                    window,
                    cx,
                );
            }
            ViewKind::Diff => {
                self.view_pin = None;
                self.open_diff = None;
                self.diff_view = None;
                self.prune_diff_cache();
                self.focus_terminal(window, cx);
                cx.notify();
            }
        }
    }

    fn open_item_now(
        &mut self,
        item: OpenItem,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        let Some((repo, checkout)) = self.document_source(&item.checkout_id) else {
            return;
        };
        match item.kind {
            OpenKind::Document => self.request_document(repo, checkout, item.path, window, cx),
            OpenKind::Diff => {
                self.request_open_changes(&item.checkout_id, Some(item.path), window, cx)
            }
        }
    }
}

/// What the panel's drag is carrying.
///
/// One gesture and two things: a terminal that can be dropped into another
/// workdir, and a workdir that can be moved up or down the panel.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum SidebarDragKind {
    Terminal {
        session_id: String,
        source_checkout: String,
    },
    Workdir {
        repo_id: String,
    },
}

pub(crate) struct SidebarDrag {
    pub(crate) kind: SidebarDragKind,
    pub(crate) origin: (f32, f32),
    pub(crate) position: (f32, f32),
    pub(crate) started: bool,
    pub(crate) target: Option<SidebarDropTarget>,
}

impl SidebarDrag {
    /// Whether the drag is carrying this terminal.
    pub(crate) fn carries_terminal(&self, id: &str) -> bool {
        matches!(&self.kind, SidebarDragKind::Terminal { session_id, .. } if session_id == id)
    }

    /// Whether the drag is carrying a terminal at all.
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self.kind, SidebarDragKind::Terminal { .. })
    }
}

/// Where a drop lands.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum SidebarDropTarget {
    /// A terminal slot inside a workdir.
    Terminal { checkout_id: String, index: usize },
    /// The boundary a workdir lands on: the number of workdirs that stay
    /// above it, counted in the panel's order as it is drawn.
    Workdir { boundary: usize },
}

#[derive(Clone)]
pub(crate) enum SidebarDropRegion {
    Session { checkout_id: String, index: usize },
    Append { checkout_id: String, index: usize },
    Insertion { checkout_id: String, index: usize },
    // A workdir heading: the pointer's half of it says whether the workdir
    // lands above the heading or below it.
    Workdir { index: usize },
}

pub(crate) struct SidebarRowBounds {
    pub(crate) region: SidebarDropRegion,
    pub(crate) bounds: Bounds<Pixels>,
}

// Record laid-out, window-local bounds so insertion targets stay correct while scrolling.
pub(crate) fn sidebar_drag_geometry(
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

pub(crate) fn sidebar_drop_marker(
    cx: &App,
    checkout_id: String,
    index: usize,
    rows: Rc<RefCell<Vec<SidebarRowBounds>>>,
) -> AnyElement {
    let roles = Roles::of(cx);
    div()
        .id(SharedString::from(format!(
            "drop-slot-{checkout_id}-{index}"
        )))
        .relative()
        .flex()
        .items_center()
        .h(px(26.))
        .pl(px(12.))
        .gap(px(7.))
        .border_1()
        .border_dashed()
        .border_color(roles.drag)
        .bg(roles.surface_inset)
        .text_color(roles.drag)
        .text_size(px(11.))
        .child(Icon::new(IconName::SquareTerminal).size(px(14.)))
        .child("Drop terminal here")
        .child(sidebar_drag_geometry(
            SidebarDropRegion::Insertion { checkout_id, index },
            rows,
        ))
        .into_any_element()
}

// Window events track movement past row boundaries; MouseExitEvent cancels an abandoned gesture.
pub(crate) fn sidebar_drag_dispatcher(
    register: impl FnOnce(&mut Window, &mut App) + 'static,
) -> AnyElement {
    canvas(|_, _, _| (), move |_, _, window, cx| register(window, cx))
        .absolute()
        .left(px(0.))
        .right(px(0.))
        .top(px(0.))
        .bottom(px(0.))
        .into_any_element()
}

impl TerminalWindow {
    fn select_session_after_document_decision(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.navigation.select_session(id) {
            self.clear_document();
            self.open_diff = None;
            self.persist();
            self.refresh_diff_totals(cx);
            self.focus_terminal(window, cx);
            cx.notify();
        }
    }

    pub(crate) fn repo_menu(
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
            .filter(|checkout| {
                !checkout.is_missing && self.navigation.archived.contains(&checkout.id)
            })
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
        move |menu, _, _cx| {
            let mut menu = menu.min_w(px(224.));
            // A separator is a line between two groups of rows, so it is only
            // drawn between two groups that are both there.
            let mut grouped = false;
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
                grouped = true;
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
                grouped = true;
            }
            if can_close {
                let id = id.clone();
                let view = view.clone();
                if grouped {
                    menu = menu.separator();
                }
                menu = menu.item(
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

    pub(crate) fn terminal_menu(
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
        move |mut menu, _, cx| {
            let roles = Roles::of(cx);
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
                // Legacy dims a row that cannot be chosen with its disabled
                // token; the toolkit's own disabled row keeps the secondary
                // foreground, so the two rows that can be out of range are
                // drawn here instead of asked of the toolkit.
                let ink = if available {
                    roles.text
                } else {
                    roles.text_disabled
                };
                menu = menu.item(
                    PopupMenuItem::element(move |_, _| {
                        // The toolkit wraps every row in its own hover and
                        // keyboard-selected wash, so this only decides the ink
                        // and whether the pointer says the row can be chosen.
                        div()
                            .flex_1()
                            .text_color(ink)
                            .when(!available, |row| {
                                row.cursor(gpui_kit::CursorStyle::OperationNotAllowed)
                            })
                            .child(label)
                    })
                    .disabled(!available)
                    .on_click(move |_, window, cx| {
                        let _ = view.update(cx, |app, cx| {
                            app.move_terminal(&id, &source, target_index, window, cx)
                        });
                    }),
                );
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

    pub(crate) fn sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let roles = Roles::of(cx);
        let weak = cx.weak_entity();
        let dragging = self.sidebar_drag.as_ref().is_some_and(|drag| drag.started);
        // The two gestures share one drag surface, so each of them asks what
        // the panel is carrying.
        let dragging_terminal = self
            .sidebar_drag
            .as_ref()
            .is_some_and(|drag| drag.is_terminal());
        let dragging_workdir = self
            .sidebar_drag
            .as_ref()
            .is_some_and(|drag| !drag.is_terminal());
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
        // The heading's name and path are one line of text, and one line of
        // text has one baseline. The two boxes are aligned at the foot, and a
        // smaller text's baseline sits nearer its own foot, so the path
        // hangs below the name by the difference of the two. This is that
        // difference, and it lifts the path back onto the name's baseline.
        let path_lift =
            baseline_from_bottom(window, HEADING_NAME_SIZE, gpui_kit::FontWeight::MEDIUM)
                - baseline_from_bottom(window, HEADING_PATH_SIZE, gpui_kit::FontWeight::NORMAL);
        // The boundary the carried workdir lands on: the number of workdirs
        // that stay above it, in the panel's order as it is drawn.
        let insertion = match self
            .sidebar_drag
            .as_ref()
            .and_then(|drag| drag.target.as_ref())
        {
            Some(SidebarDropTarget::Workdir { boundary }) => Some(*boundary),
            _ => None,
        };
        for (repo_index, repo) in self.navigation.repos.iter().enumerate() {
            let id = repo.id.clone();
            let folded = self.navigation.folded_repos.contains(&id);
            let toggle_id = id.clone();
            let drag_id = id.clone();
            let has_menu = (repo.kind == RepoKind::Git
                && repo
                    .checkouts
                    .iter()
                    .any(|checkout| checkout.is_primary && !checkout.is_missing))
                || !repo
                    .checkouts
                    .iter()
                    .any(|checkout| self.navigation.home.as_deref() == Some(&checkout.id));
            // The workdir the gesture is carrying: it is still on the panel
            // while it is held, and it is dimmed until it is put down.
            let carried = self.sidebar_drag.as_ref().is_some_and(|drag| {
                drag.started
                    && matches!(&drag.kind, SidebarDragKind::Workdir { repo_id } if repo_id == &id)
            });
            let heading = div()
                .id(SharedString::from(format!("heading-{id}")))
                .group(SharedString::from(format!("repo-heading-{id}")))
                .relative()
                .flex()
                .items_end()
                .w_full()
                .hover(|style| style.bg(roles.hover))
                .pl(px(10.))
                .pr(px(8.))
                .pt(px(8.))
                .pb(px(4.))
                // The workdir that is being carried is still on the panel, and
                // it is dimmed while it is held.
                .when(carried, |heading| heading.opacity(0.42))
                // The line is the boundary the workdir lands on, and it is the
                // top of the workdir that moves down.
                .when(insertion == Some(repo_index), |heading| {
                    heading.child(
                        div()
                            .absolute()
                            .left(px(0.))
                            .right(px(0.))
                            .top_0()
                            .h(px(2.))
                            .bg(roles.accent),
                    )
                })
                .when(dragging_workdir, |heading| {
                    heading.child(sidebar_drag_geometry(
                        SidebarDropRegion::Workdir { index: repo_index },
                        drag_rows.clone(),
                    ))
                })
                .child(
                    Button::new(SharedString::from(format!("fold-repo-{id}")))
                        .text()
                        // The toolkit's text variant asks for a pointing hand,
                        // which is for links. This is a command.
                        .cursor_default()
                        .absolute()
                        .left(px(10.))
                        .right(px(if has_menu { 36. } else { 8. }))
                        .top_0()
                        .bottom_0()
                        .p_0()
                        .when(dragging, |button| {
                            button.cursor(gpui_kit::CursorStyle::ClosedHand)
                        })
                        .aria_expanded(!folded)
                        .resting_tooltip(repo.root.clone())
                        .accessibility_label(format!(
                            "{} repository {} — {}",
                            if folded { "Expand" } else { "Collapse" },
                            repo.name,
                            repo.root
                        ))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener({
                                let drag_id = drag_id.clone();
                                move |app, event: &MouseDownEvent, _, cx| {
                                    app.begin_workdir_drag(&drag_id, event.position, cx);
                                }
                            }),
                        )
                        .on_click(cx.listener(move |app, _, _, cx| {
                            // A row that was picked up and put down is not a
                            // row that was folded.
                            if app.suppress_sidebar_click {
                                app.suppress_sidebar_click = false;
                                return;
                            }
                            toggle(&mut app.navigation.folded_repos, &toggle_id);
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("heading-content-{id}")))
                        .test_support()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .justify_start()
                        // Bottom, not baseline: the flex engine is handed no
                        // baseline to align, so `items_baseline` degrades to the
                        // top, and a 14px name and an 11px path each sit half
                        // their own leading below the line they share. The two
                        // boxes are aligned at the foot here, and the path's own
                        // margin is what puts its baseline on the name's.
                        .items_end()
                        .gap(px(8.))
                        .child(
                            div()
                                .id(SharedString::from(format!("repo-name-{id}")))
                                .test_support()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                // One line, always: a name that does not fit is
                                // cut with an ellipsis, never wrapped onto a
                                // second line that spills out of the row.
                                .whitespace_nowrap()
                                .text_size(px(HEADING_NAME_SIZE))
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .child(repo.name.clone()),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("repo-path-{id}")))
                                .test_support()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(px(HEADING_PATH_SIZE))
                                .text_color(roles.text_muted)
                                // Onto the name's baseline, which the foot of
                                // this box is not: the path is smaller, and a
                                // smaller text's baseline is nearer its own
                                // foot. See `path_lift`.
                                .mb(px(path_lift))
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
                                trigger.bg(roles.hover)
                            })
                            .child(
                                Button::new(SharedString::from(format!("actions-{id}")))
                                    .text()
                                    .cursor_default()
                                    .text_color(sidebar_faint_ink(cx))
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
                                    .h(px(20.))
                                    .w(px(20.))
                                    .p_0()
                                    .when(dragging, |button| {
                                        button.cursor(gpui_kit::CursorStyle::ClosedHand)
                                    })
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
                    && self.navigation.active_session.is_none()
                    // The workdir row stands in for "this is what the pane is
                    // showing", which it can only be when nothing inside it is:
                    // with an open item the item's own row wears the selection,
                    // and so does the row that names the view on screen.
                    && self.shown_item().is_none()
                    && !self.shows_pinned_view();
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
                    .when(
                        !dragging && checkout.is_missing,
                        |row| row.cursor(gpui_kit::CursorStyle::OperationNotAllowed),
                    )
                    // Hover refines after the rest style, so a selected row
                    // answers the pointer with its own state instead of the
                    // plain one and stays legible as the open row.
                    .hover(|style| style.bg(roles.row_hover(selected)))
                    .when(selected, |row| row.bg(roles.row_rest(true)))
                    .on_click(cx.listener({
                        let row_select_id = select_id.clone();
                        move |app, _, window, cx| app.select_checkout_row(&row_select_id, window, cx)
                    }))
                    .child(
                        Button::new(SharedString::from(format!("fold-checkout-{id}")))
                            .text()
                            .cursor_default()
                            .text_color(sidebar_faint_ink(cx))
                            .small()
                            .compact()
                            .h(px(26.))
                            .w(px(16.))
                            .ml(px(-4.))
                            .p_0()
                            .when(dragging, |button| button.cursor(gpui_kit::CursorStyle::ClosedHand))
                            .text_color(roles.text_muted)
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
                            .cursor_default()
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
                            .resting_tooltip(format!("{title} — {}", checkout.path))
                            .disabled(checkout.is_missing)
                            .when(dragging && !checkout.is_missing, |button| button.cursor(gpui_kit::CursorStyle::ClosedHand))
                            .when(checkout.is_missing, |button| button.cursor(gpui_kit::CursorStyle::OperationNotAllowed))
                            .on_click(cx.listener({
                                let click_id = select_id.clone();
                                move |app, _, window, cx| {
                                    cx.stop_propagation();
                                    app.select_checkout_row(&click_id, window, cx);
                                }
                            })),
                    )
                    .child(Icon::new(icon).size(px(14.)).text_color(if checkout.is_missing {
                        roles.text_disabled
                    } else {
                        roles.text_muted
                    }))
                    .child(
                        div()
                            .id(SharedString::from(format!("checkout-name-{id}")))
                            .test_support()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            // One line, always: the pointer on the row takes
                            // the room the figures and the close button need,
                            // and a name that no longer fits is cut with an
                            // ellipsis rather than wrapped onto a second line
                            // that spills out of the row and over the one
                            // below it.
                            .whitespace_nowrap()
                            .when(checkout.is_missing, |label| {
                                label.text_color(roles.text_disabled)
                            })
                            .child(name),
                    )
                    .when(repo.kind == RepoKind::Git && !checkout.is_missing, |row| {
                        match Self::line_slot(self.diff_totals.get(&id)) {
                            Some((added, removed)) => row.child(
                                div()
                                    .id(SharedString::from(format!("diff-{id}")))
                                    .on_mouse_down(gpui_kit::MouseButton::Left, cx.listener({ let id=id.clone(); move |app,_,window,cx| { cx.stop_propagation(); app.select_checkout_row(&id, window, cx); } }))
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
                                                .text_color(roles.status.added)
                                                .when(row_focused, |mark| mark.w(px(0.)))
                                                .overflow_hidden()
                                                .group_hover(row_group.clone(), |style| {
                                                style.w(px(0.))
                                            })
                                                .child("+"),
                                        )
                                    })
                                    .when(removed > 0, |slot| {
                                        slot.child(
                                            div()
                                                .id(SharedString::from(format!("diff-mark-del-{id}")))
                                                .test_support()
                                                .text_color(roles.status.removed)
                                                .when(row_focused, |mark| mark.w(px(0.)))
                                                .overflow_hidden()
                                                .group_hover(row_group.clone(), |style| {
                                                    style.w(px(0.))
                                                })
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
                                                        .text_color(roles.status.added)
                                                        .child(format!("+{added}")),
                                                )
                                            })
                                            .when(removed > 0, |figures| {
                                                figures.child(
                                                    div()
                                                        .text_color(roles.status.removed)
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
                                .text_color(roles.status.danger)
                                .when(has_close && row_focused, |note| note.mr(px(28.)))
                                .when(has_close, |note| note.group_hover(row_group.clone(), |style| style.mr(px(28.))))
                                .child("Directory missing"),
                        )
                    })
                    .when(has_close && checkout.is_missing, |row| {
                        let dismiss_id = id.clone();
                        row.child(
                            Button::new(SharedString::from(format!("dismiss-missing-{id}")))
                                .ghost()
                                .when(dragging, |button| button.cursor(gpui_kit::CursorStyle::ClosedHand))
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
                                .resting_tooltip(
                                    "Remove this missing checkout from the panel; it cannot be restored here. No files are deleted, and Git metadata and branch are retained",
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
                                    .ghost()
                                    .when(dragging, |button| button.cursor(gpui_kit::CursorStyle::ClosedHand))
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
                                    .resting_tooltip("Archive or delete worktree")
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
                                matches!(target, SidebarDropTarget::Terminal { checkout_id, index }
                                if checkout_id == &id
                                    && *index
                                        == self.sidebar_drop_slot(
                                            &checkout.id,
                                            checkout.sessions.len(),
                                        ))
                            })
                    });
                row = row.when(
                    self.sidebar_drag.as_ref().is_some_and(|drag| {
                        drag.started
                            && drag.target.as_ref().is_some_and(|target| {
                                matches!(target, SidebarDropTarget::Terminal { checkout_id, .. }
                                        if checkout_id == &id)
                            })
                    }),
                    |row| {
                        row.child(
                            div()
                                .absolute()
                                .left(px(0.))
                                .top(px(0.))
                                .bottom(px(0.))
                                .w(px(2.))
                                .bg(roles.accent),
                        )
                    },
                );
                if !checkout.is_missing && dragging_terminal {
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
                        if let Some(SidebarDropTarget::Terminal { checkout_id, index }) = self
                            .sidebar_drag
                            .as_ref()
                            .and_then(|drag| drag.target.clone())
                        {
                            rows.push(sidebar_drop_marker(
                                cx,
                                checkout_id,
                                index,
                                drag_rows.clone(),
                            ));
                        }
                    }
                    continue;
                }
                let checkout_row = row.into_any_element();
                let mut checkout_children = Vec::<AnyElement>::new();
                // What the centre pane is showing, if it is showing one of this
                // window's items. It is one question asked once per workdir: a
                // row is drawn under the workdir that owns it, and the pane only
                // shows what its own active workdir is looking at.
                let open_is_shown = self.shown_item().is_some();
                // The view on screen is a row of its own, so the terminal rows
                // give the selection up to it: a terminal left marked while a
                // file is being read says the pane is somewhere else.
                let view_is_shown = self.shows_pinned_view();
                for (session_index, session) in checkout.sessions.iter().enumerate() {
                    let Some(terminal) = self.terminals.get(&session.id) else {
                        continue;
                    };
                    let status = terminal.shell.session.status();
                    // The title alone: a full snapshot would build the whole
                    // visible grid to read one string the grid already holds.
                    let title = session_title(
                        &session.name,
                        status.foreground.app.as_deref(),
                        terminal.shell.session.title().as_deref(),
                    );
                    // A file or a change set is what the pane is showing, so a
                    // terminal does not wear the selection with it: one open
                    // item leaves one row reading as open, and the session that
                    // still holds its shell is not what this window is looking at.
                    let selected = self.navigation.active_session.as_deref() == Some(&session.id)
                        && !open_is_shown
                        && !view_is_shown;
                    let busy = status.foreground.running;
                    let unknown =
                        status.poll_error.is_some() || status.shell == ShellState::Unknown;
                    let program =
                        crate::navigation::foreground_program(status.foreground.app.as_deref());
                    let agent = program == Some(ForegroundProgram::OpenCode);
                    // What the terminal itself reported about the session it is
                    // showing, and what the service says about that session.
                    // The report comes first: it is about this terminal, and it
                    // names the session, which the title alone can only guess.
                    let reported = status.opencode.as_ref();
                    let agent_state = agent.then(|| {
                        self.opencode_row_state(
                            &session.checkout_id,
                            terminal.shell.session.title().as_deref(),
                            reported,
                        )
                    });
                    // The agent's name and how long ago the service last touched the
                    // session, in the row's trailing slot.
                    let agent_suffix = agent_state.as_ref().and_then(|state| {
                        let mut parts: Vec<String> = Vec::new();
                        if let Some(label) = state.label.as_ref() {
                            parts.push(label.clone());
                        }
                        let elapsed = crate::agent_watch::elapsed_label(state.elapsed);
                        if !elapsed.is_empty() {
                            parts.push(elapsed);
                        }
                        (!parts.is_empty()).then(|| parts.join(" · "))
                    });
                    // The trailing slot takes its room from the name, so a long name
                    // is cut rather than pushed under the slot.
                    let name_room = (self.navigation.sidebar_width
                        - 73.
                        - agent_suffix.as_ref().map_or(0., |suffix| {
                            sidebar_text_width(window, suffix, 10.).as_f32() + 10.
                        }))
                    .max(24.);
                    // Pi's own state, read only while Pi is what is in front of
                    // the shell: a report left over from a process that is gone
                    // is not a report about this row.
                    let pi = program == Some(ForegroundProgram::Pi);
                    let pi_task = pi.then_some(status.pi_task).flatten();
                    let pi_state = pi_row_state(pi_task);
                    let pi_note = pi_state.note;
                    let failed = !agent
                        && !pi
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
                    // The state, in the words the row is read out in. The colour
                    // and the glyph say the same thing, and a colour is not
                    // something a screen reader reads.
                    let state_note: &str = if unknown {
                        "state unavailable"
                    } else if failed {
                        "last command failed"
                    } else if matches!(status.shell, ShellState::Exited { .. }) {
                        "shell exited"
                    } else if !pi_note.is_empty() {
                        // What Pi is doing. A blocked turn says nothing here:
                        // the panel has no shape for a question this program
                        // asked, so the row stays neutral.
                        pi_note
                    } else if let Some(state) = agent_state.as_ref() {
                        state.note.as_str()
                    } else if busy {
                        "process running"
                    } else {
                        "idle"
                    };
                    let state_label =
                        session_row_label(&title, agent_suffix.as_deref(), state_note);
                    let closing = self.closing_sessions.contains(&id);
                    let renaming = self.editing.as_ref().is_some_and(|edit| edit.id == id);
                    let row_clickable = !closing && !renaming;
                    let content = if let Some(editing) =
                        self.editing.as_ref().filter(|edit| edit.id == id)
                    {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(7.))
                            .h(px(26.))
                            .w_full()
                            .child(program.map_or_else(
                                || {
                                    Icon::new(IconName::SquareTerminal)
                                        .size(px(14.))
                                        .text_color(roles.text_muted)
                                        .into_any_element()
                                },
                                |program| {
                                    foreground_program_icon(program, px(14.), roles.text_muted)
                                },
                            ))
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
                            .child({
                                let color = if unknown {
                                    roles.status.warning
                                } else if pi {
                                    // What Pi is doing, in the panel's own
                                    // colours: the accent for a turn that is
                                    // running, the danger ink for one that
                                    // failed, and the muted ink for
                                    // everything else.
                                    if pi_state.working {
                                        roles.accent
                                    } else if pi_state.failed {
                                        roles.status.danger_text
                                    } else {
                                        roles.text_muted
                                    }
                                } else if agent {
                                    agent_row_ink(&roles, agent_state.as_ref())
                                } else if failed {
                                    roles.status.danger_text
                                } else if busy {
                                    roles.status.success
                                } else {
                                    roles.text_muted
                                };
                                let working = (pi && pi_state.working)
                                    || agent_state.as_ref().is_some_and(|state| state.working);
                                let asking = agent_state.as_ref().is_some_and(|state| state.asking);
                                session_glyph(program, working, asking, px(14.), color)
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    // One line, always: a name that does not
                                    // fit is cut with an ellipsis, never
                                    // wrapped onto a second line that spills
                                    // out of the row.
                                    .whitespace_nowrap()
                                    .child(sidebar_name(window, &title, name_room)),
                            )
                            .when_some(agent_suffix, |row, suffix| {
                                row.child(
                                    div()
                                        .id(SharedString::from(format!("session-agent-{id}")))
                                        .test_support()
                                        .flex_shrink_0()
                                        .text_size(px(10.))
                                        .text_color(roles.text_muted)
                                        .child(suffix),
                                )
                            })
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
                        .when(
                            self.sidebar_drag
                                .as_ref()
                                .is_some_and(|drag| drag.started && drag.carries_terminal(&id)),
                            |row| row.opacity(0.42),
                        )
                        // Hover refines after the rest style: a selected row
                        // answers the pointer with its own state rather than
                        // losing the selection while the pointer is on it.
                        .hover(|style| style.bg(roles.row_hover(selected)))
                        .h(px(26.))
                        .when(selected, |row| row.bg(roles.row_rest(true)))
                        .when(row_clickable, |row| {
                            row.on_click(cx.listener(
                                move |app, event: &gpui_kit::ClickEvent, window, cx| {
                                    if app.suppress_sidebar_click {
                                        app.suppress_sidebar_click = false;
                                        return;
                                    }
                                    if event.click_count() == 2 {
                                        app.start_rename(&row_click_id, window, cx);
                                    } else {
                                        let id = row_click_id.clone();
                                        app.request_document_transition(
                                            Box::new(move |app, window, cx| {
                                                app.select_session_after_document_decision(
                                                    &id, window, cx,
                                                );
                                            }),
                                            window,
                                            cx,
                                        );
                                    }
                                },
                            ))
                        })
                        .when(!renaming, |row| {
                            row.child(
                                Button::new(SharedString::from(format!("select-session-{id}")))
                                    .text()
                                    .cursor_default()
                                    .absolute()
                                    .left(px(0.))
                                    .right(px(0.))
                                    .top(px(0.))
                                    .bottom(px(0.))
                                    .p_0()
                                    .aria_selected(selected)
                                    .resting_tooltip(state_label.clone())
                                    .accessibility_label(state_label)
                                    .disabled(closing)
                                    .when(dragging && !closing, |button| {
                                        button.cursor(gpui_kit::CursorStyle::ClosedHand)
                                    })
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
                                            } else {
                                                let id = click_id.clone();
                                                app.request_document_transition(
                                                    Box::new(move |app, window, cx| {
                                                        app.select_session_after_document_decision(
                                                            &id, window, cx,
                                                        );
                                                    }),
                                                    window,
                                                    cx,
                                                );
                                            }
                                        },
                                    )),
                            )
                        })
                        .child(div().flex_1().min_w_0().pr(px(20.)).child(content))
                        .child(
                            Button::new(SharedString::from(format!("close-session-{id}")))
                                .ghost()
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
                                .resting_tooltip("Close terminal")
                                .accessibility_label(format!("Close terminal session: {title}"))
                                .disabled(closing)
                                .when(dragging && !closing, |button| {
                                    button.cursor(gpui_kit::CursorStyle::ClosedHand)
                                })
                                .on_click(cx.listener(move |app, _, window, cx| {
                                    cx.stop_propagation();
                                    app.request_close_terminal(&close_id, window, cx)
                                })),
                        );
                    let marker_before = self.sidebar_drag.as_ref().is_some_and(|drag| {
                        drag.started
                            && !drag.carries_terminal(&session.id)
                            && drag.target.as_ref().is_some_and(|target| {
                                matches!(target, SidebarDropTarget::Terminal { checkout_id, index }
                                    if checkout_id == &checkout.id
                                        && *index
                                            == self.sidebar_drop_slot(&checkout.id, session_index))
                            })
                    });
                    if marker_before {
                        if let Some(SidebarDropTarget::Terminal { checkout_id, index }) = self
                            .sidebar_drag
                            .as_ref()
                            .and_then(|drag| drag.target.clone())
                        {
                            checkout_children.push(sidebar_drop_marker(
                                cx,
                                checkout_id,
                                index,
                                drag_rows.clone(),
                            ));
                        }
                    }
                    checkout_children.push(
                        row.when(
                            self.sidebar_drag.as_ref().is_some_and(|drag| {
                                dragging_terminal && !drag.carries_terminal(&session.id)
                            }),
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
                // The work this workdir is in the middle of, named in the workdir
                // it belongs to: what is being typed here, and what is being
                // reviewed here. Nothing else is a row — a file somebody only
                // read is not unfinished work, and the row above says which
                // file the pane is on.
                let items = self.items_in(&id);
                // The view the centre pane is on, when it is not showing a
                // terminal: a file being read, or a change being read. It is
                // one row however many files were clicked through, because the
                // pane shows one thing at a time, and it is the row that says
                // the pane is not on a terminal.
                if let Some(pin) = self.view_pin.as_ref().filter(|pin| pin.checkout_id == id) {
                    // A row of unfinished work that names the same file is the
                    // pane's row already: a draft of the file being previewed,
                    // or notes on the file being diffed. Two rows for one file
                    // would say the same thing twice.
                    let named = match (pin.kind, pin.path.as_deref()) {
                        (ViewKind::Preview, Some(path)) => items
                            .iter()
                            .any(|item| item.kind == OpenKind::Document && item.path == path),
                        (ViewKind::Diff, Some(path)) => items
                            .iter()
                            .any(|item| item.kind == OpenKind::Diff && item.path == path),
                        _ => false,
                    };
                    if !named {
                        let word = match pin.kind {
                            ViewKind::Preview => "Preview",
                            ViewKind::Diff => "Diff",
                        };
                        let path = pin.path.clone().unwrap_or_default();
                        // The row is named after what it names, so a panel
                        // that has one view row and a panel that has three
                        // are not the same panel.
                        let view_key = if path.is_empty() {
                            "changes".to_owned()
                        } else {
                            path.clone()
                        };
                        // The room the name has is what is left of the panel
                        // once the icon, the word, the close button and the
                        // row's own padding have taken theirs.
                        let name = if path.is_empty() {
                            String::new()
                        } else {
                            let room = (self.navigation.sidebar_width
                                - 68.
                                - sidebar_text_width(window, word, 13.).as_f32())
                            .max(24.);
                            sidebar_name(window, &path, room)
                        };
                        let description = match (pin.kind, pin.path.as_deref()) {
                            (ViewKind::Preview, Some(path)) => {
                                format!("Preview of {path} in {title}")
                            }
                            (ViewKind::Preview, None) => format!("Preview in {title}"),
                            (ViewKind::Diff, Some(path)) => format!("Diff of {path} in {title}"),
                            (ViewKind::Diff, None) => format!("Diff of the changes in {title}"),
                        };
                        let selected = view_is_shown;
                        let row_group = SharedString::from("view-row");
                        let (row_focus, row_focused) = sidebar_row_focus(window, cx, "view");
                        let icon: AnyElement = match pin.path.as_deref() {
                            Some(path) => file_icons::element(
                                "view-icon",
                                path.rsplit('/').next().unwrap_or(path),
                                false,
                                px(14.),
                                &roles,
                            )
                            .into_any_element(),
                            None => Icon::new(IconName::GitCompare)
                                .size(px(14.))
                                .text_color(roles.text_muted)
                                .into_any_element(),
                        };
                        checkout_children.push(
                            div()
                                .id(row_group.clone())
                                .test_support()
                                .track_focus(&row_focus)
                                .group(row_group.clone())
                                .flex()
                                .items_center()
                                .h(px(26.))
                                .pl(px(12.))
                                .pr(px(8.))
                                .gap(px(7.))
                                // The same retained-while-hovered selection the
                                // terminal rows wear, and for the same reason:
                                // this row is what the pane is showing.
                                .hover(|style| style.bg(roles.row_hover(selected)))
                                .when(selected, |row| row.bg(roles.row_rest(true)))
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "select-view-{view_key}"
                                    )))
                                    .text()
                                    .cursor_default()
                                    .absolute()
                                    .left(px(0.))
                                    .right(px(0.))
                                    .top(px(0.))
                                    .bottom(px(0.))
                                    .p_0()
                                    .aria_selected(selected)
                                    .resting_tooltip(description.clone())
                                    .accessibility_label(description)
                                    .on_click(cx.listener(|app, _, window, cx| {
                                        cx.stop_propagation();
                                        app.open_pinned_view(window, cx);
                                    })),
                                )
                                .child(icon)
                                .child(div().flex_none().child(word))
                                .when(!name.is_empty(), |row| {
                                    row.child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .pr(px(20.))
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .text_color(roles.text_muted)
                                            .child(name),
                                    )
                                })
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "close-view-{view_key}"
                                    )))
                                    .ghost()
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
                                    .resting_tooltip("Close this view")
                                    .accessibility_label("Close this view")
                                    .on_click(cx.listener(|app, _, window, cx| {
                                        cx.stop_propagation();
                                        app.close_pinned_view(window, cx);
                                    })),
                                )
                                .into_any_element(),
                        );
                    }
                }
                for item in items {
                    let label = item.label();
                    let name = sidebar_name(window, &label, self.navigation.sidebar_width - 41.);
                    // The whole name is what the row answers to on hover and to a
                    // screen reader, so a path the panel had to shorten is still
                    // the path it names.
                    let what = match item.kind {
                        OpenKind::Document => "unsaved edits",
                        OpenKind::Diff => "review notes",
                    };
                    let description = format!("{what} in {title}: {}", item.label());
                    let selected = item.shown_by(self);
                    let row_group = SharedString::from(format!("item-row-{label}"));
                    checkout_children.push(
                        div()
                            .id(row_group.clone())
                            .test_support()
                            .group(row_group)
                            .flex()
                            .items_center()
                            .h(px(26.))
                            .pl(px(12.))
                            .pr(px(8.))
                            .gap(px(7.))
                            // The same retained-while-hovered selection the
                            // terminal rows wear, and for the same reason: this
                            // row is what the pane is showing, so the pointer
                            // crossing it must not make it look like anything else.
                            .hover(|style| style.bg(roles.row_hover(selected)))
                            .when(selected, |row| row.bg(roles.row_rest(true)))
                            // The same overlay the session rows use: the row is
                            // one target with a focus ring, a tooltip and an
                            // accessible name, not a label and a neighbour that
                            // happen to agree.
                            .child(
                                Button::new(SharedString::from(format!("select-item-{label}")))
                                    .text()
                                    .cursor_default()
                                    .absolute()
                                    .left(px(0.))
                                    .right(px(0.))
                                    .top(px(0.))
                                    .bottom(px(0.))
                                    .p_0()
                                    .aria_selected(selected)
                                    .resting_tooltip(description.clone())
                                    .accessibility_label(description)
                                    .on_click(cx.listener({
                                        let item = item.clone();
                                        move |app, _, window, cx| {
                                            cx.stop_propagation();
                                            app.request_open_item(item.clone(), window, cx)
                                        }
                                    })),
                            )
                            .child(file_icons::element(
                                format!("item-icon-{label}"),
                                item.file(),
                                false,
                                px(14.),
                                &roles,
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(name),
                            )
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
                            .text_color(roles.text_muted)
                            .child(Icon::new(IconName::LoaderCircle).size(px(14.)))
                            .child(
                                div()
                                    .id(SharedString::from(format!("pending-{id}")))
                                    .test_support()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(format!("Starting terminal in {title}…")),
                            )
                            .into_any_element(),
                    );
                }
                let create_id = checkout.id.clone();
                let append_marker = self.sidebar_drag.as_ref().is_some_and(|drag| {
                    drag.started
                        && drag.target.as_ref().is_some_and(|target| {
                            matches!(target, SidebarDropTarget::Terminal { checkout_id, index }
                            if checkout_id == &checkout.id
                                && *index
                                    == self.sidebar_drop_slot(
                                        &checkout.id,
                                        checkout.sessions.len(),
                                    ))
                        })
                });
                if append_marker {
                    if let Some(SidebarDropTarget::Terminal { checkout_id, index }) = self
                        .sidebar_drag
                        .as_ref()
                        .and_then(|drag| drag.target.clone())
                    {
                        checkout_children.push(sidebar_drop_marker(
                            cx,
                            checkout_id,
                            index,
                            drag_rows.clone(),
                        ));
                    }
                }
                // Centred across the panel rather than on the terminal rows'
                // glyph axis: it is the one row in a group that is not about one
                // terminal, so it reads as the group's last line instead of as
                // another terminal.
                let new_terminal_row = div()
                    .relative()
                    .w_full()
                    .hover(|style| style.bg(roles.hover))
                    .when(dragging_terminal, |row| {
                        row.child(sidebar_drag_geometry(
                            SidebarDropRegion::Append {
                                checkout_id: checkout.id.clone(),
                                index: checkout.sessions.len(),
                            },
                            drag_rows.clone(),
                        ))
                    })
                    .child(
                        // Keep the full row clickable while the label group is
                        // centred in it.
                        BaseButton::new(SharedString::from(format!(
                            "new-terminal-{}",
                            checkout.id
                        )))
                        .h(px(26.))
                        .w_full()
                        .p_0()
                        .justify_center()
                        .when(dragging, |button| {
                            button.cursor(gpui_kit::CursorStyle::ClosedHand)
                        })
                        .text_color(sidebar_faint_ink(cx))
                        .hover(|style| style.text_color(roles.text))
                        .child(
                            div()
                                .id(SharedString::from(format!(
                                    "new-terminal-content-{}",
                                    checkout.id
                                )))
                                .test_support()
                                .flex()
                                .items_center()
                                .gap(px(7.))
                                .child(Icon::new(IconName::Plus).size(px(14.)))
                                .child("New terminal"),
                        )
                        .accessibility_label(format!("New terminal for {title}"))
                        .on_click(cx.listener(
                            move |app, _, window, cx| app.request_terminal(&create_id, window, cx),
                        )),
                    )
                    .into_any_element();
                let guide = div()
                    .ml(px(14.))
                    .border_l_1()
                    .border_color(roles.surface)
                    .group_hover(
                        SharedString::from(format!("checkout-children-{}", checkout.id)),
                        |style| style.border_color(roles.border),
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
        // The last boundary is below the last workdir, and there is no
        // heading there to draw the line on: the line is the end of the panel.
        if insertion == Some(self.navigation.repos.len()) {
            rows.push(div().h(px(2.)).w_full().bg(roles.accent).into_any_element());
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
                    .text_color(roles.text_muted)
                    .child("Your opened folders will appear here. Use Open directory below to add one.")
                    .into_any_element(),
            );
        }
        let ghost = self
            .sidebar_drag
            .as_ref()
            .filter(|drag| drag.started)
            .and_then(|drag| {
                let name = match &drag.kind {
                    SidebarDragKind::Terminal { session_id, .. } => {
                        let session = self.navigation.session(session_id)?;
                        let title = self
                            .terminals
                            .get(&session.id)
                            .map(|terminal| {
                                let status = terminal.shell.session.status();
                                // The title alone: a full snapshot would build the
                                // whole visible grid to read one string the grid
                                // already holds.
                                session_title(
                                    &session.name,
                                    status.foreground.app.as_deref(),
                                    terminal.shell.session.title().as_deref(),
                                )
                            })
                            .unwrap_or_else(|| session.name.clone());
                        title
                    }
                    SidebarDragKind::Workdir { repo_id } => self
                        .navigation
                        .repos
                        .iter()
                        .find(|repo| &repo.id == repo_id)
                        .map(|repo| repo.name.clone())?,
                };
                Some((drag.position, name, drag.is_terminal()))
            });
        div()
            .id("navigation-panel")
            .test_support()
            .relative()
            .h_full()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .min_h_0()
            .text_size(px(13.))
            .font_family(UI_FONT_FAMILY)
            .text_color(roles.text)
            .when(dragging, |panel| {
                panel.cursor(gpui_kit::CursorStyle::ClosedHand)
            })
            .bg(roles.surface)
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
                div().border_t_1().border_color(roles.border).child(
                    Button::new("open-directory")
                        .text()
                        .cursor_default()
                        .text_color(sidebar_faint_ink(cx))
                        .small()
                        .compact()
                        .w_full()
                        .h(px(26.))
                        .icon(IconName::FolderPlus)
                        .label("Open directory")
                        .accessibility_label("Open directory")
                        .resting_tooltip("Open directory")
                        .disabled(self.opening || self.closing)
                        .when(dragging && !self.opening && !self.closing, |button| {
                            button.cursor(gpui_kit::CursorStyle::ClosedHand)
                        })
                        .on_click(
                            cx.listener(|app, _, window, cx| app.choose_directory(window, cx)),
                        ),
                ),
            )
            .when_some(drag_dispatcher, |panel, dispatcher| panel.child(dispatcher))
            .when_some(ghost, |panel, (position, name, terminal)| {
                panel.child(
                    gpui_kit::deferred(
                        gpui_kit::anchored()
                            .position(gpui_kit::point(px(position.0 + 14.), px(position.1 + 14.)))
                            .snap_to_window()
                            .child(
                                div()
                                    .id("terminal-drag-ghost")
                                    .font_family(UI_FONT_FAMILY)
                                    .text_color(roles.text)
                                    .max_w(px(240.))
                                    .h(px(26.))
                                    .rounded(px(4.))
                                    .flex()
                                    .items_center()
                                    .gap(px(7.))
                                    .text_size(px(13.))
                                    .px(px(12.))
                                    .border_1()
                                    .border_color(roles.accent)
                                    .bg(roles.surface_raised)
                                    .shadow_md()
                                    .child(
                                        Icon::new(if terminal {
                                            IconName::SquareTerminal
                                        } else {
                                            IconName::Folder
                                        })
                                        .size(px(14.)),
                                    )
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
