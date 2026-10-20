//! The preferences panel: what a person set once, drawn as a draft they can throw away.
//!
//! Legacy generated every row of its settings dialog from a schema, so the dialog held no
//! per-preference branch at all, which is the only reason it stayed true as the list grew.
//! The schema is `muster_core::settings`: the defaults, the ranges and the shape live there,
//! and the rows below are the whole panel.
//!
//! ## Why a draft
//!
//! Nothing here reaches the app or the file until Apply. That is what makes Cancel possible
//! at all, and it is why a size somebody is still typing is never the size the window is
//! drawn at. The draft is the dialog's own state and is built whole on every open, so a
//! half-typed font size left behind last time is not what the next open shows.
//!
//! ## Why the sizes are read rather than held
//!
//! A toggle and a select report their change, so the draft can hold them. A number field does
//! not: it is a text field, and what it holds is a person mid-thought. So the four sizes live
//! in their fields and are read when the draft is compared or written. The range is applied
//! on the way in, and the clamped number is written back into the field, so the panel closes
//! on numbers that are the ones on file rather than the ones somebody typed.
//!
//! ## What Apply does not do
//!
//! It writes the file and the review location. It does not repaint a live terminal at the new
//! size: nothing here can change a running grid's font, cursor or scrollback yet, and a panel
//! that pretended otherwise would lie about what a preference does.

use gpui_kit::component::{
    button::ButtonVariants,
    dialog::{Dialog, DialogFooter},
    input::{Input, InputState},
    select::{Select, SelectEvent, SelectState},
    Disableable, IndexPath, Sizable,
};
use gpui_kit::{
    div, prelude::FluentBuilder, px, AnyElement, App, AppContext, Context, Div, Entity,
    InteractiveElement, IntoElement, ParentElement, SharedString, Styled, Subscription,
    TestSupportExt, Window,
};
use muster_core::review::ReviewStorage;
use muster_core::settings::{
    self, AppSettings, EditorSettings, FollowWorktree, IndentationSettings, SizeLimits,
    TerminalCursorStyle, TerminalScrollbarMode, TerminalSettings, UiSettings,
    EDITOR_FONT_SIZE_LIMITS, INDENTATION_SIZE_LIMITS, TERMINAL_FONT_SIZE_LIMITS,
    UI_FONT_SIZE_LIMITS,
};
use std::path::PathBuf;

use crate::app::TerminalWindow;
use crate::theme::{checkbox, zed_button, Roles};
use crate::tooltip::RestingTooltip;

/// The options a closed setting offers, and the row the current one sits on.
///
/// The picker's items are the words a person reads, and the choice is told apart by the row it
/// sits on rather than by the word: a label can be reworded without stranding the value it
/// stands for.
fn picker_state(
    labels: &[&'static str],
    current: usize,
    window: &mut Window,
    cx: &mut Context<TerminalWindow>,
) -> Entity<SelectState<Vec<SharedString>>> {
    let items: Vec<SharedString> = labels.iter().copied().map(SharedString::from).collect();
    cx.new(|cx| SelectState::new(items, Some(IndexPath::default().row(current)), window, cx))
}

fn number_field(
    value: impl std::fmt::Display,
    window: &mut Window,
    cx: &mut Context<TerminalWindow>,
) -> Entity<InputState> {
    cx.new(|cx| {
        let mut input = InputState::new(window, cx);
        input.set_value(value.to_string(), window, cx);
        input
    })
}

/// What a size field says right now, brought inside its range.
///
/// A field that is empty or mid-keystroke is not a preference, so it reads as the number a
/// Reset would write for it, rather than as a zero nobody asked for.
fn sized(input: &Entity<InputState>, limits: SizeLimits, default: f64, cx: &App) -> f64 {
    input
        .read(cx)
        .value()
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(limits.min, limits.max))
        .unwrap_or(default)
}

/// Which row of a picker's words a setting is on.
fn cursor_row(style: TerminalCursorStyle) -> usize {
    match style {
        TerminalCursorStyle::Block => 0,
        TerminalCursorStyle::Bar => 1,
        TerminalCursorStyle::Underline => 2,
    }
}

fn cursor_style(row: usize) -> TerminalCursorStyle {
    match row {
        1 => TerminalCursorStyle::Bar,
        2 => TerminalCursorStyle::Underline,
        _ => TerminalCursorStyle::Block,
    }
}

fn scrollbar_row(mode: TerminalScrollbarMode) -> usize {
    match mode {
        TerminalScrollbarMode::Hidden => 0,
        TerminalScrollbarMode::Auto => 1,
        TerminalScrollbarMode::Always => 2,
    }
}

fn storage_row(storage: ReviewStorage) -> usize {
    match storage {
        ReviewStorage::Default => 0,
        ReviewStorage::Workdir => 1,
    }
}

/// Which row of the follow picker a setting is on.
fn follow_row(mode: FollowWorktree) -> usize {
    match mode {
        FollowWorktree::Off => 0,
        FollowWorktree::Cd => 1,
        FollowWorktree::Agent => 2,
        FollowWorktree::Both => 3,
    }
}

/// The follow mode a row of [`FOLLOW_WORDS`] stands for.
fn follow_for_row(row: usize) -> FollowWorktree {
    match row {
        1 => FollowWorktree::Cd,
        2 => FollowWorktree::Agent,
        3 => FollowWorktree::Both,
        _ => FollowWorktree::Off,
    }
}

fn review_storage(row: usize) -> ReviewStorage {
    match row {
        0 => ReviewStorage::Default,
        _ => ReviewStorage::Workdir,
    }
}

/// The review location a row of [`STORAGE_WORDS`] stands for.
///
/// Named apart from the field it fills so the closure that fills that field does not shadow
/// the function it calls.
fn storage_for_row(row: usize) -> ReviewStorage {
    review_storage(row)
}

const CURSOR_WORDS: [&str; 3] = ["Block", "Bar", "Underline"];
const SCROLLBAR_WORDS: [&str; 3] = ["Hidden", "While scrolling", "Always"];
const INDENT_WORDS: [&str; 2] = ["Spaces", "Tabs"];
const STORAGE_WORDS: [&str; 2] = ["App cache", "This checkout"];
/// The words a follow setting is chosen with.
const FOLLOW_WORDS: [&str; 4] = ["Off", "Shell directory", "Agent work", "Both"];

/// The draft this window's panel is editing, if it has one open.
pub(crate) struct SettingsDraft {
    /// Where this draft is written when it is applied.
    ///
    /// Resolved once, when the panel opens, so that Apply cannot land somewhere the panel
    /// was not showing. A path is also the only seam the panel's tests need: they hand it a
    /// temporary one rather than reaching a person's own settings.
    pub(crate) path: PathBuf,
    /// Everything a control reported. The four sizes are not here; see the module note.
    pub(crate) draft: AppSettings,
    /// Where an exported review round is written.
    ///
    /// A draft field even though `core::navigation` is what persists it, because it is chosen
    /// in this dialog like every other preference and must not take effect before Apply.
    pub(crate) review_storage: ReviewStorage,
    pub(crate) ui_font_size: Entity<InputState>,
    pub(crate) terminal_font_size: Entity<InputState>,
    pub(crate) editor_font_size: Entity<InputState>,
    pub(crate) indent_size: Entity<InputState>,
    pub(crate) cursor: Entity<SelectState<Vec<SharedString>>>,
    pub(crate) scrollbar: Entity<SelectState<Vec<SharedString>>>,
    pub(crate) indent_with: Entity<SelectState<Vec<SharedString>>>,
    pub(crate) review_storage_picker: Entity<SelectState<Vec<SharedString>>>,
    /// What a terminal follows when its work moves to another worktree.
    pub(crate) follow_worktree: Entity<SelectState<Vec<SharedString>>>,
    pub(crate) error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsDraft {
    /// The whole set as it would be written.
    pub(crate) fn resolve(&self, cx: &App) -> AppSettings {
        AppSettings {
            ui: UiSettings {
                font_size: sized(&self.ui_font_size, UI_FONT_SIZE_LIMITS, 14., cx),
                tree_sticky_scroll: self.draft.ui.tree_sticky_scroll,
            },
            terminal: TerminalSettings {
                font_size: sized(&self.terminal_font_size, TERMINAL_FONT_SIZE_LIMITS, 16., cx),
                ligatures: self.draft.terminal.ligatures,
                cursor_style: self.draft.terminal.cursor_style,
                cursor_blink: self.draft.terminal.cursor_blink,
                selection_copy: self.draft.terminal.selection_copy,
                scrollbar: self.draft.terminal.scrollbar,
                shell_integration: self.draft.terminal.shell_integration,
                follow_worktree: self.draft.terminal.follow_worktree,
            },
            editor: EditorSettings {
                font_size: sized(&self.editor_font_size, EDITOR_FONT_SIZE_LIMITS, 13., cx),
                ligatures: self.draft.editor.ligatures,
                cursor_blink: self.draft.editor.cursor_blink,
                indentation: IndentationSettings {
                    use_spaces: self.draft.editor.indentation.use_spaces,
                    size: sized(&self.indent_size, INDENTATION_SIZE_LIMITS, 2., cx) as u32,
                },
            },
        }
        .clamped()
    }
}

/// One preference: what it is called, what it changes, and the control that changes it.
///
/// A row is a pair rather than a column, so a setting reads as one thing instead of being
/// matched by eye against a list of labels beside a list of controls.
fn settings_row(
    label: &'static str,
    description: &'static str,
    detail: Option<(&'static str, String)>,
    roles: Roles,
    control: impl IntoElement,
) -> AnyElement {
    div()
        .flex()
        .items_start()
        .gap(px(16.))
        .py(px(5.))
        .child(settings_row_text(label, description, detail, roles))
        .child(
            div()
                .flex_shrink_0()
                .w(px(132.))
                .pt(px(1.))
                .flex()
                .justify_end()
                .child(control),
        )
        .into_any_element()
}

/// The half of a row that says what the setting is.
///
/// `detail` is where the setting writes right now — a resolved folder — carried under
/// its help text in the same muted ink, so a path belongs to the control that chooses
/// it rather than floating under the row on its own line. It is cut to the column and
/// named in full once the pointer rests on it, because the tail is the half of a path
/// that identifies it.
fn settings_row_text(
    label: &'static str,
    description: &'static str,
    detail: Option<(&'static str, String)>,
    roles: Roles,
) -> Div {
    div()
        .flex_1()
        .min_w_0()
        // Reserve the label and help-text lines so the control column stays on a grid.
        .min_h(px(33.))
        .child(
            div()
                .text_size(px(12.))
                .line_height(px(16.))
                .text_color(roles.text)
                .child(label),
        )
        .when(!description.is_empty(), |row| {
            row.child(
                div()
                    .mt(px(2.))
                    .text_size(px(11.))
                    .line_height(px(15.))
                    .text_color(roles.text_muted)
                    .child(description),
            )
        })
        .when_some(detail, |row, (id, detail)| {
            row.child(
                div()
                    .id(id)
                    .test_support()
                    .mt(px(2.))
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.))
                    .line_height(px(15.))
                    .text_color(roles.text_muted)
                    .child(detail.clone())
                    .resting_tooltip(detail),
            )
        })
}

/// A row whose control is a group of actions.
///
/// The actions sit on one line of their own, right-aligned and as wide as they read.
/// As rows of their own they would each be stretched across the panel, which is what a
/// column of full-width text buttons is: a toolbar at the panel's width, making a
/// maintenance command look like the panel's main one.
fn settings_actions(
    label: &'static str,
    description: &'static str,
    roles: Roles,
    actions: Vec<AnyElement>,
) -> AnyElement {
    div()
        .mt(px(2.))
        .border_t_1()
        .border_color(roles.border)
        .pt(px(5.))
        .child(settings_row_text(label, description, None, roles))
        .child(
            div()
                .mt(px(8.))
                .flex()
                .gap(px(8.))
                .justify_end()
                .children(actions),
        )
        .into_any_element()
}

/// A titled run of rows.
fn settings_section(title: &'static str, roles: Roles, rows: Vec<AnyElement>) -> AnyElement {
    div()
        .mt_4()
        .child(
            div()
                .mb_1()
                .text_size(px(11.))
                .text_color(roles.text_muted)
                .child(title),
        )
        .children(rows)
        .into_any_element()
}

/// A row whose control is a checkbox, named by the field it writes.
///
/// The field is a function rather than a path string so a preference added here is a row and a
/// closure, not a branch in a template that has to be read to find out what a control does.
#[allow(clippy::too_many_arguments)]
fn toggle_row(
    view: gpui_kit::WeakEntity<TerminalWindow>,
    id: &'static str,
    label: &'static str,
    description: &'static str,
    roles: Roles,
    checked: bool,
    set: fn(&mut AppSettings, bool),
) -> AnyElement {
    settings_row(
        label,
        description,
        None,
        roles,
        checkbox(id, roles)
            .checked(checked)
            .small()
            .accessibility_label(label)
            .on_change(move |checked, _, cx| {
                let _ = view.update(cx, |app, cx| {
                    app.edit_settings_draft(cx, move |settings| set(settings, *checked));
                });
            }),
    )
}

fn select_row(
    id: &'static str,
    label: &'static str,
    description: &'static str,
    detail: Option<(&'static str, String)>,
    roles: Roles,
    picker: &Entity<SelectState<Vec<SharedString>>>,
) -> AnyElement {
    settings_row(
        label,
        description,
        detail,
        roles,
        Select::new(picker)
            .id(id)
            .small()
            .rounded_none()
            .bg(roles.surface_base)
            .accessibility_label(label),
    )
}

fn size_row(
    id: &'static str,
    label: &'static str,
    description: &'static str,
    roles: Roles,
    input: &Entity<InputState>,
) -> AnyElement {
    settings_row(
        label,
        description,
        None,
        roles,
        div().w(px(96.)).flex().items_center().gap(px(6.)).child(
            Input::new(input)
                .id(id)
                .small()
                .bg(roles.surface_base)
                .rounded_none()
                .aria_label(label)
                .w_full(),
        ),
    )
}

impl TerminalWindow {
    /// Open the panel on what is saved.
    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match settings::settings_path() {
            Ok(path) => self.open_settings_at(path, window, cx),
            Err(error) => {
                // No location is an error the panel still opens on: it would otherwise be a
                // button that does nothing, with nothing on screen saying why.
                self.settings_error = Some(error);
                cx.notify();
            }
        }
    }

    /// Open the panel on the file at `path`.
    ///
    /// A file that does not parse opens the panel on the defaults *with the failure shown*,
    /// rather than not opening: whoever edited it by hand is the one who can fix it, and a
    /// panel that refuses to appear only says something went wrong somewhere.
    #[cfg_attr(not(test), allow(clippy::too_many_arguments))]
    pub(crate) fn open_settings_at(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (saved, failure) = match settings::load(&path) {
            Ok(saved) => (saved, self.settings_error.take()),
            Err(error) => (AppSettings::default(), Some(error)),
        };
        let ui_font_size = number_field(saved.ui.font_size, window, cx);
        let terminal_font_size = number_field(saved.terminal.font_size, window, cx);
        let editor_font_size = number_field(saved.editor.font_size, window, cx);
        let indent_size = number_field(saved.editor.indentation.size, window, cx);
        let cursor = picker_state(
            &CURSOR_WORDS,
            cursor_row(saved.terminal.cursor_style),
            window,
            cx,
        );
        let scrollbar = picker_state(
            &SCROLLBAR_WORDS,
            scrollbar_row(saved.terminal.scrollbar),
            window,
            cx,
        );
        let indent_with = picker_state(
            &INDENT_WORDS,
            usize::from(!saved.editor.indentation.use_spaces),
            window,
            cx,
        );
        let review_storage = self.navigation.review_storage;
        let review_storage_picker =
            picker_state(&STORAGE_WORDS, storage_row(review_storage), window, cx);
        let follow_worktree = picker_state(
            &FOLLOW_WORDS,
            follow_row(saved.terminal.follow_worktree),
            window,
            cx,
        );

        let mut subscriptions = Vec::new();
        // Each picker reports the word that was chosen and nothing else; turning a word into a
        // setting belongs to the draft, which is the one thing both the panel and the file read.
        subscriptions.push(cx.subscribe_in(&cursor, window, |app, _, event, _, cx| {
            let SelectEvent::Confirm(Some(word)) = event else {
                return;
            };
            let Some(row) = CURSOR_WORDS.iter().position(|name| *name == word.as_ref()) else {
                return;
            };
            app.edit_settings_draft(cx, move |settings| {
                settings.terminal.cursor_style = cursor_style(row);
            });
        }));
        subscriptions.push(cx.subscribe_in(&scrollbar, window, |app, _, event, _, cx| {
            let SelectEvent::Confirm(Some(word)) = event else {
                return;
            };
            let Some(row) = SCROLLBAR_WORDS
                .iter()
                .position(|name| *name == word.as_ref())
            else {
                return;
            };
            app.edit_settings_draft(cx, move |settings| {
                settings.terminal.scrollbar = match row {
                    1 => TerminalScrollbarMode::Auto,
                    2 => TerminalScrollbarMode::Always,
                    _ => TerminalScrollbarMode::Hidden,
                };
            });
        }));
        subscriptions.push(
            cx.subscribe_in(&indent_with, window, |app, _, event, _, cx| {
                let SelectEvent::Confirm(Some(word)) = event else {
                    return;
                };
                let Some(row) = INDENT_WORDS.iter().position(|name| *name == word.as_ref()) else {
                    return;
                };
                app.edit_settings_draft(cx, move |settings| {
                    settings.editor.indentation.use_spaces = row == 0;
                });
            }),
        );
        subscriptions.push(cx.subscribe_in(
            &review_storage_picker,
            window,
            |app, _, event, _, cx| {
                let SelectEvent::Confirm(Some(word)) = event else {
                    return;
                };
                let Some(row) = STORAGE_WORDS.iter().position(|name| *name == word.as_ref()) else {
                    return;
                };
                app.edit_review_storage_draft(storage_for_row(row), cx);
            },
        ));

        subscriptions.push(
            cx.subscribe_in(&follow_worktree, window, |app, _, event, _, cx| {
                let SelectEvent::Confirm(Some(word)) = event else {
                    return;
                };
                let Some(row) = FOLLOW_WORDS.iter().position(|name| *name == word.as_ref()) else {
                    return;
                };
                app.edit_settings_draft(cx, move |settings| {
                    settings.terminal.follow_worktree = follow_for_row(row);
                });
            }),
        );

        self.settings = Some(SettingsDraft {
            path,
            draft: saved,
            review_storage,
            ui_font_size,
            terminal_font_size,
            editor_font_size,
            indent_size,
            cursor,
            scrollbar,
            indent_with,
            review_storage_picker,
            follow_worktree,
            error: failure,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    /// One reported change, written into the draft and nowhere else.
    ///
    /// The four sizes are not read from here — they live in their fields, and
    /// [`SettingsDraft::resolve`] is what turns those into numbers. A size written through this
    /// closure would be a preference no keyboard can reach, so the size fields are the only
    /// way to set one.
    pub(crate) fn edit_settings_draft(
        &mut self,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut AppSettings),
    ) {
        if let Some(draft) = self.settings.as_mut() {
            edit(&mut draft.draft);
            draft.error = None;
            cx.notify();
        }
    }

    /// The review location is chosen here like any other preference, so it is a draft too.
    pub(crate) fn edit_review_storage_draft(
        &mut self,
        storage: ReviewStorage,
        cx: &mut Context<Self>,
    ) {
        if let Some(draft) = self.settings.as_mut() {
            draft.review_storage = storage;
            draft.error = None;
            cx.notify();
        }
    }

    /// Replace the whole draft with the defaults, controls included.
    ///
    /// It replaces rather than edits, which is why the controls are told as well: a Reset that
    /// changed only the model would leave four numbers on screen that Apply does not mean.
    pub(crate) fn reset_settings_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let defaults = AppSettings::default();
        let Some(draft) = self.settings.as_ref() else {
            return;
        };
        for (input, value) in [
            (&draft.ui_font_size, defaults.ui.font_size.to_string()),
            (
                &draft.terminal_font_size,
                defaults.terminal.font_size.to_string(),
            ),
            (
                &draft.editor_font_size,
                defaults.editor.font_size.to_string(),
            ),
            (
                &draft.indent_size,
                defaults.editor.indentation.size.to_string(),
            ),
        ] {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        let picks = [
            (&draft.cursor, 0usize),
            (&draft.scrollbar, 0usize),
            (&draft.indent_with, 0usize),
            (
                &draft.review_storage_picker,
                storage_row(ReviewStorage::default()),
            ),
            (
                &draft.follow_worktree,
                follow_row(FollowWorktree::default()),
            ),
        ];
        for (picker, row) in picks {
            picker.update(cx, |picker, cx| {
                picker.set_selected_index(Some(IndexPath::default().row(row)), window, cx)
            });
        }
        let draft = self
            .settings
            .as_mut()
            .expect("the draft was there a line ago");
        draft.draft = defaults;
        draft.review_storage = ReviewStorage::default();
        draft.error = None;
        cx.notify();
    }

    /// Write the draft: the preferences file first, then the review location, and only then
    /// close.
    ///
    /// In that order because the file is the whole set and the review location is one field of
    /// it: a failure to write it has to leave the panel open with the person's choices still
    /// in them, which is the only state from which they can be applied again.
    pub(crate) fn apply_settings_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.settings.as_ref() else {
            return;
        };
        let next = draft.resolve(cx);
        let path = draft.path.clone();
        if let Err(error) = settings::save(&path, &next) {
            let draft = self.settings.as_mut().expect("the draft was just read");
            draft.error = Some(format!("Settings were not saved. {error}"));
            cx.notify();
            return;
        }
        // The fields are told what was written, so the panel closes on numbers that are the
        // ones on file rather than the ones somebody typed before the range was applied.
        for (input, value) in [
            (&draft.ui_font_size, next.ui.font_size.to_string()),
            (
                &draft.terminal_font_size,
                next.terminal.font_size.to_string(),
            ),
            (&draft.editor_font_size, next.editor.font_size.to_string()),
            (&draft.indent_size, next.editor.indentation.size.to_string()),
        ] {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        let storage = draft.review_storage;
        self.set_follow_worktree(next.terminal.follow_worktree);
        self.settings = None;
        if storage != self.navigation.review_storage {
            self.set_review_storage(storage, cx);
        }
        cx.notify();
    }

    /// The panel, drawn as the conversation it is.
    pub(crate) fn settings_dialog(
        &self,
        draft: &SettingsDraft,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let roles = Roles::of(cx);
        let view = cx.weak_entity();
        let reset = cx.weak_entity();
        let apply = cx.weak_entity();
        let clear = cx.weak_entity();
        let forget = cx.weak_entity();
        let cancel = cx.weak_entity();

        // The body builder runs again on every repaint and has to own everything it draws, so
        // the draft's entities are taken out here rather than borrowed into it.
        let ui_font_size = draft.ui_font_size.clone();
        let terminal_font_size = draft.terminal_font_size.clone();
        let editor_font_size = draft.editor_font_size.clone();
        let indent_size = draft.indent_size.clone();
        let cursor = draft.cursor.clone();
        let scrollbar = draft.scrollbar.clone();
        let indent_with = draft.indent_with.clone();
        let storage_picker = draft.review_storage_picker.clone();
        let follow_worktree = draft.follow_worktree.clone();
        let terminal = draft.draft.terminal;
        let editor = draft.draft.editor;
        let sticky = draft.draft.ui.tree_sticky_scroll;
        let storage = draft.review_storage;
        let error = draft.error.clone();
        let is_default =
            draft.resolve(cx) == AppSettings::default() && storage == ReviewStorage::default();
        // Where a round would be written for the mode this row is showing, which is not
        // necessarily the one on file yet: it is a draft, and the row answers for the draft.
        let folder = self
            .open_diff
            .as_ref()
            .map(|(id, _)| id.as_str())
            .or(self.navigation.active_checkout.as_deref())
            .and_then(|id| self.document_source(id))
            .map(|(repo, checkout)| {
                muster_core::files::review_export_folder(&repo, &checkout, storage)
            })
            .map(|result| match result {
                Ok(path) => path.display().to_string(),
                Err(error) => format!("unavailable: {error}"),
            });
        // The default cache is one folder under the home directory that every checkout
        // writes to, so what clearing it needs is a checkout rather than a diff open:
        // the panel is opened from the title bar, where no diff is open.
        let can_clear = storage == ReviewStorage::Default
            && (self.open_diff.is_some() || self.navigation.active_checkout.is_some());
        // A control that cannot be pressed says why: this one is unavailable for two
        // different reasons, and a person who cannot press it has no other way to ask.
        let clear_reason = match (storage, can_clear) {
            (ReviewStorage::Workdir, _) => {
                "Rounds are written inside the checkout, so there is no cache to clear."
            }
            (_, false) => "Select a checkout first.",
            _ => "",
        };
        let can_forget = !self.review.forgetting;

        Dialog::new(cx)
            .bg(roles.surface_raised)
            .border_color(roles.border_strong)
            .width(px(520.))
            .title("Settings")
            // The rows are taller than the dialog is allowed to be, so the body goes in as a
            // child rather than through `content`: a child is the dialog's own scroll area,
            // and a content column is only ever cut off at the footer.
            .child({
                let mut body = div().flex_col().gap(px(16.)).child(
                    div()
                        .text_size(px(11.))
                        .text_color(roles.text_muted)
                        .child("Saved to ~/.muster/settings.json"),
                );
                body = body.child(settings_section(
                    "Interface",
                    roles,
                    vec![
                        size_row(
                            "settings-ui-font-size",
                            "Font size",
                            "Scales every size the interface draws itself in.",
                            roles,
                            &ui_font_size,
                        ),
                        toggle_row(
                            view.clone(),
                            "settings-ui-sticky",
                            "Sticky folder rows",
                            "Keep parent folders visible while scrolling the Files tree.",
                            roles,
                            sticky,
                            |settings, checked| settings.ui.tree_sticky_scroll = checked,
                        ),
                    ],
                ));
                body = body.child(settings_section(
                    "Terminal",
                    roles,
                    vec![
                        size_row(
                            "settings-terminal-font-size",
                            "Font size",
                            "",
                            roles,
                            &terminal_font_size,
                        ),
                        toggle_row(
                            view.clone(),
                            "settings-terminal-ligatures",
                            "Ligatures",
                            "Joins the sequences the font draws as one glyph, such as => or !=.",
                            roles,
                            terminal.ligatures,
                            |settings, checked| settings.terminal.ligatures = checked,
                        ),
                        select_row(
                            "settings-terminal-cursor",
                            "Cursor",
                            "The cell it fills, a line beside it, or a line under it.",
                            None,
                            roles,
                            &cursor,
                        ),
                        toggle_row(
                            view.clone(),
                            "settings-terminal-cursor-blink",
                            "Blink cursor",
                            "",
                            roles,
                            terminal.cursor_blink,
                            |settings, checked| settings.terminal.cursor_blink = checked,
                        ),
                        toggle_row(
                            view.clone(),
                            "settings-terminal-selection-copy",
                            "Copy on selection",
                            "Puts what a selection gesture selects in the terminal on the clipboard.",
                            roles,
                            terminal.selection_copy,
                            |settings, checked| settings.terminal.selection_copy = checked,
                        ),
                        select_row(
                            "settings-terminal-scrollbar",
                            "Scrollbar",
                            "",
                            None,
                            roles,
                            &scrollbar,
                        ),
                        toggle_row(
                            view.clone(),
                            "settings-terminal-shell-integration",
                            "Report failed commands",
                            "Reports failures in zsh and bash without clearing startup output. Applies to new terminals only.",
                            roles,
                            terminal.shell_integration,
                            |settings, checked| settings.terminal.shell_integration = checked,
                        ),
                        select_row(
                            "settings-terminal-follow-worktree",
                            "Follow worktree",
                            "Moves a terminal's row when its work moves to another worktree. The process and its scrollback stay as they are.",
                            None,
                            roles,
                            &follow_worktree,
                        ),
                    ],
                ));
                body = body.child(settings_section(
                    "Editor",
                    roles,
                    vec![
                        size_row(
                            "settings-editor-font-size",
                            "Font size",
                            "",
                            roles,
                            &editor_font_size,
                        ),
                        toggle_row(
                            view.clone(),
                            "settings-editor-ligatures",
                            "Ligatures",
                            "",
                            roles,
                            editor.ligatures,
                            |settings, checked| settings.editor.ligatures = checked,
                        ),
                        toggle_row(
                            view.clone(),
                            "settings-editor-cursor-blink",
                            "Blink cursor",
                            "",
                            roles,
                            editor.cursor_blink,
                            |settings, checked| settings.editor.cursor_blink = checked,
                        ),
                        select_row(
                            "settings-editor-indent-with",
                            "Indent with",
                            "",
                            None,
                            roles,
                            &indent_with,
                        ),
                        size_row(
                            "settings-editor-indent-size",
                            "Indent size",
                            "Columns one level of indent is.",
                            roles,
                            &indent_size,
                        ),
                    ],
                ));

                let clear_button = clear.clone();
                let forget_button = forget.clone();
                body = body.child(settings_section(
                    "Review notes",
                    roles,
                    vec![
                        select_row(
                            "settings-review-storage",
                            "Exported notes",
                            "Where a review round is written when it is sent as a file rather than to an agent.",
                            folder.map(|folder| ("settings-review-folder", folder)),
                            roles,
                            &storage_picker,
                        ),
                        settings_actions(
                            "Stored rounds",
                            "Cached exports and delivered rounds stay where they are until one of these removes them.",
                            roles,
                            vec![
                                zed_button("settings-review-clear")
                                    .small()
                                    .label("Clear cached exports")
                                    .disabled(!can_clear)
                                    .when(!can_clear, |button| {
                                        button
                                            .text_color(roles.text_disabled)
                                            .resting_tooltip(clear_reason)
                                    })
                                    .on_click(move |_, _, cx| {
                                        let _ = clear_button.update(cx, |app, cx| {
                                            app.open_review_clear_confirmation(cx)
                                        });
                                    })
                                    .into_any_element(),
                                zed_button("settings-review-forget")
                                    .small()
                                    .label("Forget delivered reviews")
                                    .disabled(!can_forget)
                                    .when(!can_forget, |button| {
                                        button.text_color(roles.text_disabled).resting_tooltip(
                                            "Forgetting delivered reviews…",
                                        )
                                    })
                                    .on_click(move |_, _, cx| {
                                        let _ = forget_button.update(cx, |app, cx| {
                                            app.open_opencode_forget_confirmation(cx)
                                        });
                                    })
                                    .into_any_element(),
                            ],
                        ),
                    ],
                ));

                if let Some(error) = error.clone() {
                    body = body.child(
                        div()
                            .mt_3()
                            .text_size(px(12.))
                            .line_height(px(16.))
                            .text_color(roles.status.danger_text)
                            .child(error),
                    );
                }
                body
            })
            .footer(
                DialogFooter::new()
                    .child(
                        zed_button("settings-reset")
                            .text()
                            .label("Reset to defaults")
                            .disabled(is_default)
                            .when(is_default, |button| button.text_color(roles.text_disabled))
                            .on_click(move |_, window, cx| {
                                let _ = reset.update(cx, |app, cx| {
                                    app.reset_settings_draft(window, cx)
                                });
                            }),
                    )
                    .child(div().flex_1())
                    .child({
                        let cancel = cancel.clone();
                        zed_button("settings-cancel")
                            .custom(crate::app::sidebar_action_style(cx).foreground(roles.text))
                            .label("Cancel")
                            .on_click(move |_, _, cx| {
                                let _ = cancel.update(cx, |app, cx| {
                                    app.settings = None;
                                    cx.notify();
                                });
                            })
                    })
                    .child(
                        zed_button("settings-apply")
                            .primary()
                            .label("Apply")
                            .on_click(move |_, window, cx| {
                                let _ = apply.update(cx, |app, cx| {
                                    app.apply_settings_draft(window, cx)
                                });
                            }),
                    ),
            )
            .on_cancel(move |_, _, cx| {
                let _ = cancel.update(cx, |app, cx| {
                    app.settings = None;
                    cx.notify();
                });
                true
            })
            .into_any_element()
    }
}
