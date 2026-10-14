//! The crumb line, and the searchable menu each crumb opens.
//!
//! The bar across the top of the window is the sidebar read sideways: a crumb
//! names one level of it and opens the list of that level. So this module owns
//! three things that belong together — the crumb, the menu behind it, and the
//! rule that says how much room a name may take before it gives up segments.
//!
//! The bar's own chrome — the traffic-light room, the client-side frame, the
//! controls, the drag of empty space — is `TitleBar`'s, and is not restyled here.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    button::{Button, ButtonCustomVariant, ButtonVariants},
    command::{Command, CommandGroup, CommandItem, CommandState},
    popover::Popover,
    Disableable, Icon, IndexPath, Sizable, TitleBar,
};
use gpui_kit::{
    div, prelude::FluentBuilder, px, AnyElement, App, Context, Entity, Focusable,
    InteractiveElement, IntoElement, MouseButton, ParentElement, SharedString, Styled,
    TestSupportExt, Window,
};
use muster_core::workspace::{Repo, RepoKind};

use crate::app::{
    fitted_name, sidebar_faint_ink, sidebar_text_width, TerminalWindow, SIDEBAR_FONT_FAMILY,
};
use crate::navigation::checkout_title;
use crate::theme::Roles;
fn crumb_separator(cx: &App) -> AnyElement {
    div()
        .font_family(SIDEBAR_FONT_FAMILY)
        .text_size(px(14.))
        .line_height(px(18.))
        .text_color(sidebar_faint_ink(cx))
        .child("/")
        .into_any_element()
}

/// The glyph a crumb wears: the one the sidebar gives the row it names, so the
/// line reads as that panel sideways instead of as a row of text.
///
/// The workdir wears none. It is the name that is always there, and a glyph on
/// it would only repeat a word that never changes.
pub(crate) fn crumb_icon(crumb: Crumb) -> Option<IconName> {
    match crumb {
        Crumb::Workdir => None,
        Crumb::Worktree => Some(IconName::GitBranch),
        Crumb::Item => Some(IconName::SquareTerminal),
    }
}

/// The button a crumb is drawn as.
///
/// The fitted name is what the line has room for at the current width; the
/// whole name is what the crumb answers to on hover and to a screen reader, so
/// a crumb that gave up segments is still the name it names.
fn titlebar_crumb(
    window: &Window,
    id: &'static str,
    label: &str,
    budget: f32,
    icon: Option<IconName>,
    color: gpui_kit::Hsla,
) -> Button {
    // Account for the button's inset and icon before fitting the text.
    let icon_width = if icon.is_some() { 20. } else { 0. };
    let fitted = fitted_name(window, label, (budget - 8. - icon_width).max(0.), 14.);
    let crumb = Button::new(id)
        .text()
        .small()
        .rounded(px(0.))
        .h(px(22.))
        .px(px(4.))
        .font_family(SIDEBAR_FONT_FAMILY)
        .text_size(px(14.))
        .line_height(px(18.))
        // Held to its share, so a name the line cannot hold is clipped here
        // rather than pushing the actions at the far end out of the window.
        .max_w(px(budget))
        .text_color(color)
        .tooltip(label.to_owned())
        .accessibility_label(label.to_owned())
        .cursor_pointer();
    match icon {
        Some(icon) => crumb.icon(Icon::new(icon).size(px(12.))).label(fitted),
        None => crumb.label(fitted),
    }
}

fn titlebar_crumb_width(window: &Window, label: &str, icon: Option<IconName>) -> f32 {
    8. + sidebar_text_width(window, label, 14.).as_f32() + if icon.is_some() { 20. } else { 0. }
}

fn titlebar_crumb_min_width(icon: Option<IconName>) -> f32 {
    16. + if icon.is_some() { 20. } else { 0. }
}

fn titlebar_worktree_title(repo: &Repo, checkout: &muster_core::workspace::Checkout) -> String {
    if repo.kind == RepoKind::Git && checkout.branch.is_none() {
        "Detached".into()
    } else {
        checkout_title(repo, checkout)
    }
}

fn shrink_titlebar_crumb(width: &mut f32, minimum: f32, excess: &mut f32) {
    let removed = (*width - minimum).max(0.).min(*excess);
    *width -= removed;
    *excess -= removed;
}

/// The search state of one crumb menu.
///
/// One state per crumb rather than one per bar: two menus sharing a query would
/// show each other's search in the other's field.
fn titlebar_command_state(window: &mut Window, cx: &mut App, key: &str) -> Entity<CommandState> {
    window.use_keyed_state(
        SharedString::from(format!("titlebar-{key}")),
        cx,
        CommandState::new,
    )
}

fn titlebar_menu_popover(cx: &App, id: &'static str) -> Popover {
    let roles = Roles::of(cx);
    Popover::new(id)
        .appearance(false)
        .w(px(288.))
        .bg(roles.surface_raised)
        .border_1()
        .border_color(roles.border_strong)
        .rounded(px(0.))
        .font_family(SIDEBAR_FONT_FAMILY)
        .text_size(px(13.))
        .line_height(px(18.))
        .text_color(roles.text)
}

/// One crumb menu opening or closing.
///
/// The open name lives on the window rather than in the menu, so opening one
/// crumb closes the others instead of stacking them. A close only clears the
/// name if it is this crumb's: two popovers report their state in the same
/// gesture sometimes, and the last one to answer must not erase the other's.
fn crumb_opened(
    crumb: Crumb,
    view: gpui_kit::WeakEntity<TerminalWindow>,
    focus: gpui_kit::FocusHandle,
) -> impl Fn(&bool, &mut Window, &mut App) {
    move |open, window, cx| {
        let _ = view.update(cx, |app, cx| {
            if *open {
                app.open_crumb = Some(crumb);
            } else if app.open_crumb == Some(crumb) {
                app.open_crumb = None;
            }
            cx.notify();
        });
        if *open {
            focus.focus(window, cx);
        }
    }
}

/// A crumb's searchable menu: one group of rows under a search field, with the
/// action that makes another one at its foot.
///
/// The action is in the footer rather than among the rows because a search must
/// not be able to filter away the one row that is not a choice between the
/// things on the list.
fn crumb_menu(
    state: &Entity<CommandState>,
    rows: Rc<Vec<CrumbRow>>,
    placeholder: &'static str,
    group: &'static str,
    action: Option<Rc<dyn Fn() -> Button>>,
    view: gpui_kit::WeakEntity<TerminalWindow>,
    cx: &App,
) -> Command {
    let roles = Roles::of(cx);
    let selected_row = state
        .read(cx)
        .selected_index()
        .map(|index| index.row)
        .filter(|index| rows.get(*index).is_some_and(|row| !row.disabled))
        .or_else(|| rows.iter().position(|row| !row.disabled));
    let selected_row = Rc::new(Cell::new(selected_row));
    let items = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let label = row.label.clone();
            let hint = row.hint.clone();
            let icon = row.icon;
            let checked = row.checked;
            let disabled = row.disabled;
            let selected_row = selected_row.clone();
            CommandItem::new()
                .label(label.clone())
                .keywords(row.keywords.clone())
                .disabled(disabled)
                .child(move |_, _| {
                    let selected = selected_row.get() == Some(index);
                    div()
                        .id(SharedString::from(format!("crumb-menu-row-{index}")))
                        .test_support()
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .mx(px(-8.))
                        .my(px(-6.))
                        .px(px(8.))
                        .py(px(5.))
                        .rounded(px(0.))
                        .border_1()
                        .border_color(if selected {
                            roles.accent
                        } else {
                            roles.surface_raised
                        })
                        .bg(if selected && !disabled {
                            roles.hover
                        } else {
                            roles.surface_raised
                        })
                        .items_center()
                        .gap(px(16.))
                        .when(disabled, |row| {
                            row.cursor(gpui_kit::CursorStyle::OperationNotAllowed)
                        })
                        .when(!disabled, |row| row.cursor_pointer())
                        // Hover refines after the rest style, so a selected row
                        // needs its own answer or it stops being selected for
                        // exactly as long as the pointer is on it.
                        .when(!disabled, |row| row.hover(|style| style.bg(roles.hover)))
                        .child(Icon::new(icon).size_4().text_color(if disabled {
                            roles.text_disabled
                        } else {
                            roles.text_muted
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_ellipsis()
                                .font_family(SIDEBAR_FONT_FAMILY)
                                .text_size(px(13.))
                                .line_height(px(18.))
                                .text_color(if disabled {
                                    roles.text_disabled
                                } else {
                                    roles.text
                                })
                                .child(label.clone()),
                        )
                        .when_some(hint.clone(), |this, hint| {
                            this.child(
                                div()
                                    .ml_auto()
                                    .min_w_0()
                                    .max_w(px(112.))
                                    .text_ellipsis()
                                    .font_family(SIDEBAR_FONT_FAMILY)
                                    .text_size(px(11.))
                                    .line_height(px(14.))
                                    .text_color(roles.text_muted)
                                    .child(hint),
                            )
                        })
                        .when(checked, |this| {
                            this.child(Icon::new(IconName::Check).size_3().text_color(roles.accent))
                        })
                })
        })
        .collect::<Vec<_>>();
    // Command reports the original item index within this one group; filtering
    // changes visibility, not the row used to resolve the target.
    let targets = rows
        .iter()
        .map(|row| row.target.clone())
        .collect::<Vec<_>>();
    let mut menu = Command::new(state)
        .placeholder(placeholder)
        .max_h(px(300.))
        .bordered(false)
        .font_family(SIDEBAR_FONT_FAMILY)
        .text_size(px(13.))
        .line_height(px(18.))
        .bg(roles.surface_raised)
        .text_color(roles.text)
        .group(CommandGroup::new().label(group).items(items));
    menu = match action {
        Some(action) => menu.footer(move |_, _, cx| {
            div()
                .w_full()
                .px(px(4.))
                .pt(px(4.))
                .pb(px(4.))
                .child(
                    div()
                        .id("crumb-menu-separator")
                        .test_support()
                        .w_full()
                        .h(px(1.))
                        .bg(roles.border),
                )
                .child(
                    action()
                        .compact()
                        .custom(
                            ButtonCustomVariant::new(cx)
                                .foreground(roles.text_muted)
                                .hover(roles.hover)
                                .active(roles.pressed),
                        )
                        .cursor_pointer()
                        .w_full()
                        .justify_start()
                        .h(px(28.))
                        .rounded(px(0.))
                        .px(px(8.))
                        .font_family(SIDEBAR_FONT_FAMILY)
                        .text_size(px(13.))
                        .line_height(px(18.)),
                )
        }),
        None => menu,
    };
    let selected_row = selected_row.clone();
    menu.on_select(move |index: IndexPath, _, _| selected_row.set(Some(index.row)))
        .on_confirm(move |index: IndexPath, window, cx| {
            let Some(target) = targets.get(index.row) else {
                return;
            };
            let target = target.clone();
            let _ = view.update(cx, |app, cx| app.select_crumb_target(&target, window, cx));
        })
}

/// What a crumb names, and so which of the three menus is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Crumb {
    /// The workdir: the repository, or the plain directory it is.
    Workdir,
    /// The worktree of that repository, which only a Git one has.
    Worktree,
    /// What is open inside the workdir.
    Item,
}

/// One row of a searchable crumb menu.
///
/// The words behind the name are what a search matches on: two worktrees of one
/// repository have the same kind of name and no other thing to tell them apart.
pub(crate) struct CrumbRow {
    label: String,
    hint: Option<String>,
    icon: IconName,
    keywords: Vec<String>,
    checked: bool,
    disabled: bool,
    target: String,
}

impl TerminalWindow {
    /// Searchable choices of live sessions in the active checkout.
    fn session_rows(&self, checkout_id: &str) -> Rc<Vec<CrumbRow>> {
        Rc::new(
            self.navigation
                .checkout(checkout_id)
                .into_iter()
                .flat_map(|checkout| &checkout.sessions)
                .filter(|session| self.terminals.contains_key(&session.id))
                .map(|session| CrumbRow {
                    label: self.session_label(session),
                    hint: None,
                    icon: IconName::SquareTerminal,
                    keywords: vec![session.name.clone()],
                    checked: self.navigation.active_session.as_deref() == Some(session.id.as_str()),
                    disabled: false,
                    target: session.id.clone(),
                })
                .collect(),
        )
    }

    /// What a crumb menu does with the row it was opened on.
    ///
    /// The one path every crumb selection takes, so the header, the sidebar and
    /// the terminal cannot answer the same choice three different ways.
    fn close_crumb_menu(&mut self, cx: &mut Context<Self>) {
        self.open_crumb = None;
        cx.notify();
    }

    fn select_crumb_target(&mut self, target: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.close_crumb_menu(cx);
        if !self.navigation.select_session(target) && !self.navigation.select_checkout(target) {
            return;
        }
        self.persist();
        self.refresh_diff_totals(cx);
        self.focus_terminal(window, cx);
        cx.notify();
    }

    /// The bar across the top: the window's own chrome, the crumb line over it,
    /// and the actions at its far end.
    ///
    /// What belongs to the window rather than to the app is `TitleBar`'s: the
    /// macOS traffic-light room, the client-side frame and the controls on it,
    /// the drag of empty space and the double-click that zooms. The crumb line
    /// is the app's, and it is the sidebar read sideways — each crumb names one
    /// level of it and opens the list of that level.
    pub(crate) fn titlebar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let roles = Roles::of(cx);
        let view = cx.weak_entity();
        let active = self
            .navigation
            .active_checkout
            .as_deref()
            .and_then(|id| self.navigation.checkout(id));
        let repo = active.and_then(|checkout| {
            self.navigation
                .repos
                .iter()
                .find(|repo| repo.checkouts.iter().any(|item| item.id == checkout.id))
        });
        // Keep the breadcrumb line within the reference's 70% cap; each crumb
        // gets its measured content width, and the branch gives up space first.
        let share = px((window.viewport_size().width.as_f32() * 0.7).max(0.));
        let worktree_label = active
            .zip(repo)
            .filter(|(_, repo)| repo.kind == RepoKind::Git)
            .map(|(checkout, repo)| titlebar_worktree_title(repo, checkout));
        let item_label = self
            .terminal()
            .and(self.navigation.active_session.as_deref())
            .and_then(|id| self.navigation.session(id))
            .map(|session| self.session_label(session));
        let mut repo_budget = repo
            .map(|repo| titlebar_crumb_width(window, &repo.name, crumb_icon(Crumb::Workdir)))
            .unwrap_or(0.);
        let mut worktree_budget = worktree_label
            .as_deref()
            .map(|label| titlebar_crumb_width(window, label, crumb_icon(Crumb::Worktree)))
            .unwrap_or(0.);
        let mut item_budget = item_label
            .as_deref()
            .map(|label| titlebar_crumb_width(window, label, crumb_icon(Crumb::Item)))
            .unwrap_or(0.);
        let crumb_count = usize::from(repo.is_some())
            + usize::from(worktree_label.is_some())
            + usize::from(item_label.is_some());
        let separators_width = if crumb_count > 1 {
            (crumb_count - 1) as f32 * (sidebar_text_width(window, "/", 14.).as_f32() + 12.)
        } else {
            0.
        };
        let available = (share.as_f32() - separators_width).max(0.);
        let mut excess = (repo_budget + worktree_budget + item_budget - available).max(0.);
        if worktree_label.is_some() {
            shrink_titlebar_crumb(
                &mut worktree_budget,
                titlebar_crumb_min_width(crumb_icon(Crumb::Worktree)),
                &mut excess,
            );
        }
        if item_label.is_some() {
            shrink_titlebar_crumb(
                &mut item_budget,
                titlebar_crumb_min_width(crumb_icon(Crumb::Item)),
                &mut excess,
            );
        }
        if repo.is_some() {
            shrink_titlebar_crumb(
                &mut repo_budget,
                titlebar_crumb_min_width(crumb_icon(Crumb::Workdir)),
                &mut excess,
            );
        }
        let mut crumbs = Vec::<AnyElement>::new();
        if let (Some(checkout), Some(repo)) = (active, repo) {
            // Every checkout this window has open, named the way the sidebar
            // names its rows, with the path behind the name as the words a
            // search matches on.
            let workdirs = Rc::new(
                self.navigation
                    .repos
                    .iter()
                    .flat_map(|repo| {
                        repo.checkouts.iter().map(move |checkout| CrumbRow {
                            label: checkout_title(repo, checkout),
                            hint: Some(checkout.path.clone()),
                            icon: if repo.kind == RepoKind::Git {
                                IconName::FolderGit2
                            } else {
                                IconName::Folder
                            },
                            keywords: vec![checkout.path.clone(), repo.root.clone()],
                            checked: self.navigation.active_checkout.as_deref()
                                == Some(checkout.id.as_str()),
                            disabled: checkout.is_missing
                                || self.navigation.archived.contains(&checkout.id),
                            target: checkout.id.clone(),
                        })
                    })
                    .collect::<Vec<_>>(),
            );
            // The workdir wears no glyph: it is the name that is always there,
            // and one would only repeat a word that never changes.
            let workdir_state = titlebar_command_state(window, cx, "workdir");
            let workdir_focus = workdir_state.focus_handle(cx);
            crumbs.push(
                titlebar_menu_popover(cx, "popover:repo-crumb")
                    .open(self.open_crumb == Some(Crumb::Workdir))
                    .on_open_change(crumb_opened(
                        Crumb::Workdir,
                        view.clone(),
                        workdir_focus.clone(),
                    ))
                    .track_focus(&workdir_focus)
                    .trigger(titlebar_crumb(
                        window,
                        "repo-crumb",
                        &repo.name,
                        repo_budget,
                        crumb_icon(Crumb::Workdir),
                        roles.text,
                    ))
                    .content({
                        let rows = workdirs.clone();
                        let state = workdir_state.clone();
                        let menu = view.clone();
                        let chooser = view.clone();
                        move |_, _, cx| {
                            crumb_menu(
                                &state,
                                rows.clone(),
                                "Search workdirs…",
                                "This Window",
                                Some(Rc::new({
                                    let chooser = chooser.clone();
                                    move || {
                                        let chooser = chooser.clone();
                                        Button::new("workdir-open-directory")
                                            .text()
                                            .small()
                                            .label("Open directory")
                                            .icon(Icon::new(IconName::FolderPlus).size_4())
                                            .on_click(move |_, window, cx| {
                                                let _ = chooser.update(cx, |app, cx| {
                                                    app.close_crumb_menu(cx);
                                                    app.choose_directory(window, cx)
                                                });
                                            })
                                    }
                                })),
                                menu.clone(),
                                cx,
                            )
                        }
                    })
                    .into_any_element(),
            );
            if repo.kind == RepoKind::Git {
                // The worktree is the one level only a Git repository has, and
                // its detached state is still a name worth showing.
                crumbs.push(crumb_separator(cx));
                let worktrees = Rc::new(
                    repo.checkouts
                        .iter()
                        .map(|checkout| CrumbRow {
                            label: titlebar_worktree_title(repo, checkout),
                            hint: Some(checkout.path.clone()),
                            icon: if checkout.is_missing {
                                IconName::FolderX
                            } else if checkout.is_primary {
                                IconName::FolderGit2
                            } else {
                                IconName::GitBranch
                            },
                            keywords: vec![checkout.path.clone()],
                            checked: self.navigation.active_checkout.as_deref()
                                == Some(checkout.id.as_str()),
                            disabled: checkout.is_missing
                                || self.navigation.archived.contains(&checkout.id),
                            target: checkout.id.clone(),
                        })
                        .collect::<Vec<_>>(),
                );
                // A worktree is added to the repository's root, wherever the
                // active workdir is, so the action names the repository.
                let dialog = view.clone();
                let repo_id = repo.id.clone();
                let worktree_state = titlebar_command_state(window, cx, "worktree");
                let worktree_focus = worktree_state.focus_handle(cx);
                crumbs.push(
                    titlebar_menu_popover(cx, "popover:worktree-crumb")
                        .open(self.open_crumb == Some(Crumb::Worktree))
                        .on_open_change(crumb_opened(
                            Crumb::Worktree,
                            view.clone(),
                            worktree_focus.clone(),
                        ))
                        .track_focus(&worktree_focus)
                        .trigger(titlebar_crumb(
                            window,
                            "worktree-crumb",
                            worktree_label.as_deref().unwrap_or("Detached"),
                            worktree_budget,
                            crumb_icon(Crumb::Worktree),
                            sidebar_faint_ink(cx),
                        ))
                        .content({
                            let rows = worktrees.clone();
                            let state = worktree_state.clone();
                            let menu = view.clone();
                            let repo_id = repo_id.clone();
                            let dialog = dialog.clone();
                            move |_, _, cx| {
                                crumb_menu(
                                    &state,
                                    rows.clone(),
                                    "Search worktrees…",
                                    "Worktrees",
                                    Some(Rc::new({
                                        let dialog = dialog.clone();
                                        let repo_id = repo_id.clone();
                                        move || {
                                            let dialog = dialog.clone();
                                            let repo_id = repo_id.clone();
                                            Button::new("worktree-new")
                                                .text()
                                                .small()
                                                .label("New worktree")
                                                .icon(Icon::new(IconName::GitBranchPlus).size_4())
                                                .on_click(move |_, window, cx| {
                                                    let _ = dialog.update(cx, |app, cx| {
                                                        app.close_crumb_menu(cx);
                                                        app.open_worktree_dialog(
                                                            &repo_id, window, cx,
                                                        )
                                                    });
                                                })
                                        }
                                    })),
                                    menu.clone(),
                                    cx,
                                )
                            }
                        })
                        .into_any_element(),
                );
            }
            // Name only the terminal that is currently presented, not a saved
            // session whose process has no live terminal view.
            if let Some(label) = item_label.as_deref() {
                crumbs.push(crumb_separator(cx));
                let rows = self.session_rows(&checkout.id);
                let session_state = titlebar_command_state(window, cx, "session");
                let session_focus = session_state.focus_handle(cx);
                let add = view.clone();
                let checkout_id = checkout.id.clone();
                crumbs.push(
                    titlebar_menu_popover(cx, "popover:item-crumb")
                        .open(self.open_crumb == Some(Crumb::Item))
                        .on_open_change(crumb_opened(
                            Crumb::Item,
                            view.clone(),
                            session_focus.clone(),
                        ))
                        .track_focus(&session_focus)
                        .trigger(titlebar_crumb(
                            window,
                            "item-crumb",
                            label,
                            item_budget,
                            crumb_icon(Crumb::Item),
                            sidebar_faint_ink(cx),
                        ))
                        .content({
                            let rows = rows.clone();
                            let state = session_state.clone();
                            let menu = view.clone();
                            move |_, _, cx| {
                                crumb_menu(
                                    &state,
                                    rows.clone(),
                                    "Search terminals…",
                                    "Terminals",
                                    Some(Rc::new({
                                        let add = add.clone();
                                        let checkout_id = checkout_id.clone();
                                        move || {
                                            let add = add.clone();
                                            let checkout_id = checkout_id.clone();
                                            Button::new("item-new-terminal")
                                                .text()
                                                .small()
                                                .label("New terminal")
                                                .icon(Icon::new(IconName::Plus).size_4())
                                                .on_click(move |_, window, cx| {
                                                    let _ = add.update(cx, |app, cx| {
                                                        app.close_crumb_menu(cx);
                                                        app.request_terminal(
                                                            &checkout_id,
                                                            window,
                                                            cx,
                                                        );
                                                    });
                                                })
                                        }
                                    })),
                                    menu.clone(),
                                    cx,
                                )
                            }
                        })
                        .into_any_element(),
                );
            }
        }
        let mut bar: Vec<AnyElement> = Vec::new();
        if !crumbs.is_empty() {
            // The line is held to the share: a name it cannot hold is given up
            // by the crumb that wears it rather than by the window.
            bar.push(
                div()
                    .flex()
                    .items_center()
                    .min_w_0()
                    .max_w(share)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .min_w_0()
                            .gap(px(6.))
                            .children(crumbs),
                    )
                    .into_any_element(),
            );
        }
        // The room between the line and the actions is the window's to drag.
        bar.push(div().flex_1().min_w(px(8.)).into_any_element());
        bar.push(
            Button::new("toggle-panels")
                .custom(crate::app::sidebar_action_style(cx))
                .small()
                .compact()
                .h(px(20.))
                .cursor_pointer()
                .icon(IconName::PanelLeftClose)
                .accessibility_label("Toggle side panels")
                .tooltip("Toggle side panels — Cmd/Ctrl+/")
                // The bar moves the window from the press; a press on an action
                // is the action, not a drag that starts one.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|app, _, window, cx| {
                    app.navigation.panels_visible = !app.navigation.panels_visible;
                    app.focus_terminal(window, cx);
                    cx.notify();
                }))
                .into_any_element(),
        );
        let close = view.clone();
        TitleBar::new()
            .h(px(32.))
            .bg(roles.surface)
            .border_b_1()
            .border_color(roles.border)
            .children(bar)
            // Linux has no frame of its own, so the close it draws would
            // otherwise take the window down without closing what is running
            // in it. Only Linux draws these controls, and only Linux reads this.
            .on_close_window(move |_, _window, cx| {
                let _ = close.update_in(cx, |app, window, cx| app.request_window_close(window, cx));
            })
            .into_any_element()
    }
}
