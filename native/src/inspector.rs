//! Lazy checkout Files and Git Changes inspector.
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::Icon;
use gpui_kit::component::Sizable;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    div, point, px, AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, TestSupportExt,
};
use muster_core::workspace::{Checkout, Repo, RepoKind};

use crate::{
    app::{measured_width, ButtonA11y, InspectorTab, TerminalWindow, WidthCache},
    file_icons,
    theme::{zed_button, Roles},
};

/// The size the panel's own rows draw their names at.
const ROW_TEXT_PX: f32 = 13.;
/// The size a Git status letter is drawn at, which is its own column's size.
const STATUS_TEXT_PX: f32 = 10.;

/// The letter a Git status answers with, out of both porcelain columns.
///
/// Map porcelain status to the deliberately limited marker shown in the file tree.
pub(crate) fn file_marker(status: &str) -> &'static str {
    match status {
        "UU" | "AA" | "DD" | "DU" | "UD" | "AU" | "UA" => "U",
        _ if status.contains('A') => "A",
        _ if status.contains('D') => "D",
        _ if status.contains('M') => "M",
        _ => "",
    }
}

/// The wash a row wears, and the bar the open one carries.
///
/// Hover refines after the rest style, so one closure has to answer both: a row
/// that answers the pointer with the plain hover stops looking open for
/// exactly as long as the pointer rests on it. The bar is drawn inside the
/// row's own box rather than as a border, so opening a row does not move the
/// name it holds.
fn row_chrome(
    row: gpui_kit::Stateful<gpui_kit::Div>,
    roles: Roles,
    selected: bool,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    // Every row is a fixed height, and a flex column will otherwise shrink them to
    // fit: the tree then scrolls by less than it claims and the last rows are
    // unreachable. The height has to be the row's own.
    let row = row.flex_none();
    row.hover(|style| style.bg(roles.row_hover(selected)).text_color(roles.text))
        .when(selected, |row| {
            row.bg(roles.row_rest(true)).text_color(roles.text)
        })
        .when(selected, |row| {
            row.child(
                div()
                    .absolute()
                    .left(px(0.))
                    .top(px(0.))
                    .bottom(px(0.))
                    .w(px(2.))
                    .bg(roles.accent),
            )
        })
}

/// How wide one tree row wants to be, from its indent to the far end of its columns.
///
/// The file tree is Legacy's `max-content`: the tree scrolls sideways, so a long
/// name is readable rather than shortened to fit. This is that width: the gutter,
/// the fold slot, the icon, the name, and whatever Git printed after it. The
/// control is laid over the row and takes none of this, which is why it is not
/// here. The change set is the other rule and does not come through here: its rows
/// are the panel's width and shorten their names instead.
fn row_width(
    widths: &mut WidthCache,
    window: &gpui_kit::Window,
    depth: usize,
    name: &str,
    status: &str,
    additions: Option<u64>,
    deletions: Option<u64>,
) -> f32 {
    let mut width = 8. + depth as f32 * 14. + 16. + 7. + 14. + 7.;
    width += measured_width(widths, window, name, ROW_TEXT_PX);
    if !status.is_empty() {
        width += 7. + measured_width(widths, window, status, STATUS_TEXT_PX);
    }
    for (sign, count) in [('+', additions), ('-', deletions)] {
        if let Some(count) = count {
            width += 7. + measured_width(widths, window, &format!("{sign}{count}"), ROW_TEXT_PX);
        }
    }
    width + 8.
}

/// A folder's state, drawn as a row of the tree.
///
/// It carries the row's chrome and nothing else: no icon, no fold, no Git marker,
/// because none of those belong to a directory that has not answered yet. The retry
/// is the one control a note has, and it asks for the listing again rather than the
/// read, because a directory is what is on screen.
fn note_row(
    path: &str,
    message: &str,
    depth: usize,
    width: gpui_kit::Pixels,
    of: (Repo, Checkout),
    weak: gpui_kit::WeakEntity<TerminalWindow>,
    roles: Roles,
) -> AnyElement {
    let (repo, checkout) = of;
    let retry_path = path.to_owned();
    row_chrome(
        div()
            .id(format!("file-note-{path}"))
            .flex()
            .items_center()
            .min_w(width)
            .gap(px(7.))
            .h(px(26.))
            .pl(px(8. + depth as f32 * 14.))
            .pr(px(8.))
            .child(
                div()
                    .id(format!("file-note-{path}-text"))
                    .test_support()
                    .flex_1()
                    .whitespace_nowrap()
                    .text_color(roles.text_muted)
                    .child(message.to_owned()),
            )
            .child(
                zed_button(format!("retry-list-{path}"))
                    .ghost()
                    .small()
                    .compact()
                    .label("Retry")
                    .accessibility_label(format!("Retry listing {path}"))
                    .on_click(move |_, window, cx| {
                        let _ = weak.update(cx, |app, cx| {
                            app.request_directory(
                                repo.clone(),
                                checkout.clone(),
                                retry_path.clone(),
                                window,
                                cx,
                            );
                            cx.notify();
                        });
                    }),
            ),
        roles,
        false,
    )
    .into_any_element()
}

/// A toolkit button laid over the row with none of its own chrome. It carries
/// the focus ring, accessible name, tooltip and key without masking row states.
fn row_control(
    id: impl Into<gpui_kit::ElementId>,
    label: String,
    tooltip: Option<String>,
    selected: bool,
    expanded: Option<bool>,
    on_click: impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
) -> AnyElement {
    zed_button(id)
        .text()
        .rounded(px(0.))
        .absolute()
        .inset_0()
        .p_0()
        .aria_selected(selected)
        .when_some(expanded, |button, expanded| button.aria_expanded(expanded))
        .accessibility_label(label)
        .when_some(tooltip, |button, tooltip| button.tooltip(tooltip))
        .on_click(on_click)
        .into_any_element()
}

/// Whether a click asked for the row to be opened somewhere else instead of acted on.
///
/// Meta is accepted beside ctrl because this app runs on both, and on macOS the
/// muscle memory for "open it in its own window" is cmd rather than ctrl. The
/// toolkit's own `Modifiers::secondary_key` is the same question with the
/// platform's answer baked in, and this is where the answer is asked.
pub(crate) fn opens_elsewhere(event: &gpui_kit::ClickEvent) -> bool {
    let modifiers = event.modifiers();
    modifiers.control || modifiers.platform
}

/// The fold of a stacked folder: a transparent control over the same 16px slot
/// the tree draws a folder's fold in, so the glyph is the tree's own and the
/// click is the fold's.
///
/// The click is stopped on its way out, because the stacked row answers it
/// too: the fold closes the folder and the row brings it into view, and one
/// gesture cannot be both.
fn fold_control(
    id: impl Into<gpui_kit::ElementId>,
    label: String,
    left: gpui_kit::Pixels,
    on_click: impl Fn(&mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
) -> AnyElement {
    zed_button(id)
        .text()
        .rounded(px(0.))
        .absolute()
        .left(left)
        .top(px((ROW_HEIGHT - 16.) / 2.))
        .w(px(16.))
        .h(px(16.))
        .p_0()
        .aria_expanded(true)
        .accessibility_label(label)
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            on_click(window, cx);
        })
        .into_any_element()
}

/// One tab of the inspector's strip.
///
/// Legacy's tab is a label on the strip's own hairline: 11px in the secondary
/// ink, the open one lifted to primary ink with the accent under it, and
/// nothing painted behind either. The negative margin puts that underline on
/// the strip's rule, so an open tab is one line and not two.
fn inspector_tab(
    id: &'static str,
    label: String,
    count: usize,
    selected: bool,
    roles: Roles,
    on_click: impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
) -> AnyElement {
    div()
        .group(id)
        .relative()
        .child(
            div()
                .id(format!("{id}-label"))
                .test_support()
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(6.))
                .py(px(7.))
                .mb(px(-1.))
                .border_b_1()
                .border_color(if selected {
                    roles.accent
                } else {
                    gpui_kit::transparent_black()
                })
                .text_size(px(11.))
                .text_color(if selected {
                    roles.text
                } else {
                    roles.text_muted
                })
                .group_hover(id, |style| style.text_color(roles.text))
                .group_active(id, |style| style.bg(roles.pressed).text_color(roles.text))
                .child(label.clone())
                .when(count > 0, |tab| {
                    tab.child(
                        div()
                            .id(format!("{id}-count"))
                            .test_support()
                            .text_size(px(10.))
                            .text_color(roles.text_muted)
                            .group_active(id, |style| style.text_color(roles.text))
                            .child(count.to_string()),
                    )
                }),
        )
        .child(
            zed_button(id)
                .text()
                .rounded(px(0.))
                .absolute()
                .inset_0()
                .p_0()
                .aria_selected(selected)
                .accessibility_label(label)
                .on_click(on_click),
        )
        .into_any_element()
}

/// The ink a Git status letter wears, in Legacy's four rules: primary ink for a
/// modified file, its success and danger inks for the two states a change
/// creates, and the secondary ink for a conflict and for everything else.
fn status_ink(marker: &str, roles: &Roles) -> gpui_kit::Hsla {
    match marker {
        "A" => roles.status.success,
        "D" => roles.status.danger_text,
        "M" => roles.text,
        _ => roles.text_muted,
    }
}

/// Show the status Changes received from Git, rather than collapsing uncommon codes.
pub(crate) fn change_marker(status: &str) -> String {
    status.trim().to_owned()
}

/// The status letter a row carries, in Legacy's column, ink and size.
///
/// The key names the row: an element id is a window-wide identity, so a letter
/// alone would put the same id on every modified row in the list.
fn status_chip(key: &str, marker: &str, roles: Roles) -> impl IntoElement {
    div()
        .id(format!("row-{key}-status"))
        .test_support()
        .flex_shrink_0()
        .text_size(px(STATUS_TEXT_PX))
        .text_color(status_ink(marker, &roles))
        .child(marker.to_owned())
}

/// The `+n` and `−n` a row ends with, at the row's own size.
fn count_chips(
    key: &str,
    (added, removed): (Option<u64>, Option<u64>),
    roles: Roles,
) -> impl IntoElement {
    let count = |kind: &str, text: String, ink: gpui_kit::Hsla| {
        div()
            .id(format!("row-{key}-{kind}"))
            .test_support()
            .flex_shrink_0()
            .text_size(px(ROW_TEXT_PX))
            .text_color(ink)
            .child(text)
    };
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(7.))
        .when_some(added.filter(|n| *n != 0), |tail, n| {
            tail.child(count("add", format!("+{n}"), roles.status.success))
        })
        .when_some(removed.filter(|n| *n != 0), |tail, n| {
            tail.child(count("del", format!("−{n}"), roles.status.danger_text))
        })
}

enum ChangeRow {
    Directory(String),
    File(muster_core::navigation::ChangedFile),
}

/// A folder open above a row, and how deep in the tree it sits.
///
/// Every row carries its own chain rather than the stack being worked out when the tree
/// is scrolled. Legacy scans the rows above the viewport on every scroll event, which
/// costs one pass over the tree, and a tree of ten thousand is exactly the tree where
/// the stack matters. Carrying it makes the same rule an index lookup.
#[derive(Clone)]
pub(crate) struct Ancestor {
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) depth: usize,
    /// Where the folder's own row is, so bringing it back into view is one lookup.
    pub(crate) index: usize,
}

/// One line of the tree.
///
/// An entry carries its own icon, its Git markers and its controls. A note stands in
/// for a directory that is still answering, and is a row of the same height, which is
/// why it lives in the list: a note drawn outside the list takes its height from the
/// box above it and moves every row under it every time a folder is opened.
///
/// A row borrows the listing it came from rather than owning a copy of it. The whole
/// tree is walked on every render and sixty-odd rows are drawn from it, so a tree that
/// owned its rows was copying every name in the checkout to show the ones on screen.
pub(crate) enum TreeRow<'a> {
    Entry {
        entry: &'a muster_core::files::FileEntry,
        depth: usize,
        /// The folders open at and above this row, itself included when it is one.
        ancestors: Rc<[Ancestor]>,
    },
    Note {
        /// The directory the note is about, which is also what a retry reads.
        path: &'a str,
        message: &'a str,
        depth: usize,
        /// The folders open above it, which is its parent's chain: a note stands where
        /// its folder's children would be, so it does not interrupt the chain.
        ancestors: Rc<[Ancestor]>,
    },
}

/// What Git did to one path, as the file tree draws it.
///
/// The letter is already [`file_marker`]'s answer: turning a porcelain code into a
/// letter is a drawing decision, and it is made once for an answer rather than once
/// for every row of every render.
#[derive(Clone, Copy, Default)]
pub(crate) struct FileStatus {
    /// Empty for a path Git said nothing about, which is a row with no marker.
    pub(crate) marker: &'static str,
    pub(crate) additions: Option<u64>,
    pub(crate) deletions: Option<u64>,
}

/// One checkout's changed files, by path.
///
/// An index rather than a second copy of the status: a row reads its own answer in
/// one hash lookup, and the window builds this once per answer instead of once per
/// render.
pub(crate) type StatusIndex = HashMap<String, FileStatus>;

/// Every changed path of a status, with the letter each one wears.
pub(crate) fn status_index(status: &muster_core::navigation::GitStatus) -> StatusIndex {
    status
        .files
        .iter()
        .map(|file| {
            (
                file.path.clone(),
                FileStatus {
                    marker: file_marker(&file.status),
                    additions: file.additions,
                    deletions: file.deletions,
                },
            )
        })
        .collect()
}

/// What Git did to `path`, or the empty answer for a path it said nothing about.
fn status_of(index: Option<&StatusIndex>, path: &str) -> FileStatus {
    index
        .and_then(|index| index.get(path))
        .copied()
        .unwrap_or_default()
}

/// How tall one row of the tree is.
///
/// Legacy names this `TREE_ROW_HEIGHT` and builds the whole tree's geometry on it,
/// so the folder stack divides by it too.
const ROW_HEIGHT: f32 = 26.;

/// The folders open above the first row on screen, oldest first.
///
/// This is Legacy's rule with its arithmetic intact. The stack is drawn over the rows it
/// names, so those rows are hidden, and the folders *they* sit under are what belongs in
/// the stack: repeating until the row stops moving is the fixed point Legacy's own tests
/// pin against a reference that rescans the whole tree on every scroll.
///
/// Empty at the very top of the tree, which the arithmetic alone would not give: the row
/// on screen is then the folder itself, drawn where it is, and it has nothing to say about
/// what is above it. Legacy's test that pins this passes for the wrong reason — it runs
/// in a DOM whose `clientHeight` is zero, so `maxStack` is zero there — and the rule it
/// is pinning is the one written above.
///
/// Each pass measures what the stack hides from the row *on screen*, which never moves,
/// and not from the row the last pass reached: Legacy's `nextTop` is the scroll offset
/// plus the height of the stack, and the scroll offset is the one the tree was scrolled
/// to. Accumulating instead walks down the tree until it runs out of rows and answers
/// with whatever chain the last row it reached carries — nothing at all for a tree whose
/// last row is a file at its root, which is what most trees end with.
pub(crate) fn sticky_ancestors(rows: &[TreeRow<'_>], top: usize, stack: usize) -> Vec<Ancestor> {
    let screen = top.min(rows.len());
    if screen == 0 {
        return Vec::new();
    }
    let ancestors = |at: usize| -> Vec<Ancestor> {
        at.checked_sub(1)
            .and_then(|row| rows.get(row))
            .map(|row| match row {
                TreeRow::Entry { ancestors, .. } | TreeRow::Note { ancestors, .. } => {
                    ancestors.to_vec()
                }
            })
            .unwrap_or_default()
    };
    let mut at = screen;
    let mut held = ancestors(at);
    for _ in 0..=stack {
        let hidden = stack.min(held.len()) + 1;
        let next = screen + hidden;
        if next <= at || next > rows.len() {
            break;
        }
        at = next;
        held = ancestors(at);
    }
    // Trimmed to what the panel holds, keeping the folders nearest the row on screen.
    held.split_off(held.len().saturating_sub(stack))
}

/// How many rows are built either side of the tree's top, and how far the ones that
/// were not built sit from it.
///
/// Legacy names these `TREE_WINDOW_SIZE` and `TREE_OVERSCAN`. The rows that were not
/// built are still there as space above and below, so the scrollbar is the whole tree's
/// and not the window's — which is the part that matters to somebody dragging it.
const WINDOW_ROWS: usize = 64;
const OVERSCAN_ROWS: usize = 10;

/// The rows to build, and the space the rest of the tree takes above and below them.
pub(crate) fn tree_window(total: usize, top: usize) -> (Range<usize>, f32, f32) {
    let start = total
        .saturating_sub(WINDOW_ROWS)
        .min(top.saturating_sub(OVERSCAN_ROWS));
    let end = start.saturating_add(WINDOW_ROWS).min(total);
    (
        start..end,
        start as f32 * ROW_HEIGHT,
        total.saturating_sub(end) as f32 * ROW_HEIGHT,
    )
}

/// How many stacked folders the tree has room for.
///
/// Legacy measures the tree's own viewport and keeps eight pixels clear at the top.
/// The tree is drawn under a tab strip and its padding, neither of which is the
/// tree's own box, and the tree's box is what the stack is laid inside and clipped by
/// — which is what keeps a deep chain from covering the rows it names rather than this
/// number. The caller passes the scroller's own bounds, which is where the tree was a
/// frame ago, the same one-frame-old reading its offset is.
pub(crate) fn stack_room(window_height: gpui_kit::Pixels) -> usize {
    ((window_height.as_f32() - 8.).max(0.) / ROW_HEIGHT).floor() as usize
}

/// One folder of the stack drawn over the tree.
///
/// The name is a button that brings the folder back into view, which is the whole point
/// of the stack: without it a tree deep enough to need one has no way back up. The row
/// the folder is drawn on is its own top-level ancestor, so scrolling there leaves the
/// whole chain below it on screen — Legacy's `index - depth`.
///
/// The row carries its own click and its fold carries its own, which is the part that
/// used to be missing: a stack drawn over the rows it names had no hitbox of its own, so
/// a click on a stacked folder's name landed on the row hidden underneath it and opened
/// that one instead — measured on `src/nested/f12.txt`, a file scrolled off the top of
/// the tree. Legacy's own note is the rule: the sticky rows keep their own clicks.
fn sticky_row(
    ancestor: &Ancestor,
    checkout: &str,
    width: gpui_kit::Pixels,
    weak: gpui_kit::WeakEntity<TerminalWindow>,
    roles: Roles,
) -> AnyElement {
    let path = ancestor.path.clone();
    let name = ancestor.name.clone();
    let depth = ancestor.depth;
    let index = ancestor.index;
    let indent = px(8. + depth as f32 * 14.);
    let reveal = weak.clone();
    let fold_checkout = checkout.to_owned();
    row_chrome(
        div()
            .id(format!("file-stack-{path}"))
            .flex()
            .items_center()
            .min_w(width)
            .gap(px(7.))
            .h(px(ROW_HEIGHT))
            .pl(indent)
            .pr(px(8.))
            .cursor_pointer()
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(16.))
                    .h(px(16.))
                    .child(Icon::new(IconName::ChevronDown)),
            )
            .child(file_icons::element(
                format!("file-stack-icon-{path}"),
                &name,
                true,
                px(14.),
                &roles,
            ))
            .child(
                div()
                    .id(format!("file-stack-name-{path}"))
                    .test_support()
                    .flex_1()
                    .whitespace_nowrap()
                    .text_color(roles.text)
                    .child(name.clone()),
            )
            .child(fold_control(
                format!("file-stack-fold:{path}"),
                format!("Collapse {name}"),
                indent,
                move |_, cx| {
                    let _ = weak.update(cx, |app, cx| {
                        app.collapse_directory(&fold_checkout, &path);
                        cx.notify();
                    });
                },
            ))
            // The row itself is the way back up, so the click is the row's and not
            // only the name's: a folder is 16 pixels of indent wide before it says
            // anything, and that indent is part of what is being pointed at.
            .on_click(move |_, _, cx| {
                // This row is drawn over the rows it names, and the ones it covers
                // are still under the pointer: without this the same click would
                // also be answered by the file hidden underneath it.
                cx.stop_propagation();
                // The folder's own top-level ancestor: scrolling there leaves the
                // whole chain between it and the tree's root on screen. The offset
                // is set outright rather than aimed at a row, because a row's index
                // depends on the window being drawn and this does not.
                // The offset counts downwards, so bringing a folder to the top
                // of the tree is that many pixels from it.
                let target = index.saturating_sub(depth) as f32 * ROW_HEIGHT;
                let _ = reveal.update(cx, |app, _| {
                    app.files_scroll.set_offset(point(px(0.), px(-target)));
                });
            }),
        roles,
        false,
    )
    .into_any_element()
}

/// How many diff reads may exist at once.
///
/// `count_sweep`'s rule applied to one view: enough to overlap the Git reads,
/// few enough that a worktree with three hundred changed files cannot fork three
/// hundred `git diff` processes the moment somebody opens the change set.
const DIFF_CONCURRENCY: usize = 4;

/// How many more may be waiting for a slot at the same time.
///
/// Bounded too, because the queue is what a burst of expansions turns into. A
/// refusal is not a failure: the read is simply not wanted yet, and the row
/// says so and offers Retry rather than pretending something is on its way.
const DIFF_QUEUE_LIMIT: usize = 32;

/// One diff read, resolved against the checkout that asked for it.
struct DiffWork {
    key: (String, String),
    token: u64,
    path: String,
    repo: Repo,
    checkout: Checkout,
}

/// The diff reads this window wants, and how many are running.
///
/// Three rules, and each answers one way this used to fork without limit. A
/// read runs in the order it was wanted and only while a slot is free, so the
/// number of live reads is the limit and nothing else. A read whose generation
/// has moved on is dropped instead of run, so a refresh cannot double up on a
/// path another read is already carrying. And a read that has started keeps its
/// slot until it actually finishes, so an answer nobody is waiting for still
/// costs what an answer somebody is waiting for costs.
#[derive(Default)]
pub(crate) struct DiffQueue {
    waiting: VecDeque<DiffWork>,
    running: usize,
}

impl DiffQueue {
    /// Want `work`, unless too much is already waiting.
    fn want(&mut self, work: DiffWork) -> bool {
        if self.waiting.len() >= DIFF_QUEUE_LIMIT {
            return false;
        }
        self.waiting.push_back(work);
        true
    }

    /// The next read that is still wanted, or `None` while the slots are full.
    fn take(&mut self, wanted: &HashMap<(String, String), u64>) -> Option<DiffWork> {
        // A read whose generation has moved on is not work anybody is waiting
        // for, and a queue padded with it is a queue that cannot answer: it is
        // dropped here rather than stepped over later, so collapsing a row
        // gives its room back at once.
        self.waiting
            .retain(|work| wanted.get(&work.key) == Some(&work.token));
        if self.running >= DIFF_CONCURRENCY {
            return None;
        }
        // Pushed last: a pump that hands out nothing has started nothing, so it
        // must not spend a slot. A slot spent here is one a wanted read can
        // never have.
        let work = self.waiting.pop_front()?;
        self.running += 1;
        Some(work)
    }

    /// A read finished, whether or not anybody wanted its answer.
    fn finished(&mut self) {
        self.running = self.running.saturating_sub(1);
    }

    /// Stop wanting one path. A read already running still finishes.
    pub(crate) fn forget(&mut self, key: &(String, String)) {
        self.waiting.retain(|work| &work.key != key);
    }

    #[cfg(test)]
    pub(crate) fn running(&self) -> usize {
        self.running
    }

    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.waiting.len()
    }
}

/// Whether a checkout-relative path is `directory` or sits under it.
///
/// The prefix has to carry the separator: `src` is not an ancestor of `src-tauri`,
/// and a plain `starts_with` would collapse one when the other closed. The root
/// is spelled as the empty path, and everything is under it.
pub(crate) fn within(candidate: &str, directory: &str) -> bool {
    directory.is_empty()
        || candidate
            .strip_prefix(directory)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// What the tree is drawn from.
///
/// The four collections and the checkout they belong to travel together everywhere the
/// walk goes, and passing them one at a time is how a caller ends up pairing one
/// checkout's listings with another's expansion.
pub(crate) struct TreeSource<'a> {
    pub(crate) checkout: &'a str,
    pub(crate) entries: &'a HashMap<(String, String), Vec<muster_core::files::FileEntry>>,
    pub(crate) loading: &'a HashSet<(String, String)>,
    pub(crate) errors: &'a HashMap<(String, String), String>,
    pub(crate) expanded: &'a HashSet<(String, String)>,
    /// This checkout's changed files by path, or nothing while Git has not answered.
    pub(crate) statuses: Option<&'a StatusIndex>,
}

/// A folder is empty only after its successful listing arrived; an unread folder
/// keeps its disclosure control so it can be opened.
fn listed_directory_is_empty(
    entries: &HashMap<(String, String), Vec<muster_core::files::FileEntry>>,
    key: &(String, String),
) -> bool {
    entries.get(key).is_some_and(Vec::is_empty)
}

/// Every visible row of the tree, in the order it is drawn.
///
/// A directory that is still answering contributes a note where its children would be,
/// at the depth it would have shown them at, and stands in for them until it answers.
/// That is Legacy's arrangement, and it is why a folder being opened cannot move the
/// rows around it: a note drawn outside the list takes its height from the box above it
/// and moves everything under it every time a folder is opened.
fn flatten<'a>(
    source: &TreeSource<'a>,
    directory: &str,
    depth: usize,
    ancestors: Rc<[Ancestor]>,
    rows: &mut Vec<TreeRow<'a>>,
) {
    let children = match source
        .entries
        .get(&(source.checkout.to_owned(), directory.to_owned()))
    {
        Some(children) => children,
        None => return,
    };
    for entry in children {
        let index = rows.len();
        // One key per folder rather than one per row. Only a folder can be open,
        // answering or failed, so the key those three are read under is not worth
        // building for a file, and a tree is mostly files.
        let key = entry
            .directory
            .then(|| (source.checkout.to_owned(), entry.path.clone()));
        let open = key
            .as_ref()
            .is_some_and(|key| source.expanded.contains(key));
        // One chain per open folder, shared by every row under it, so a tree of any
        // depth costs one allocation per folder rather than one per row.
        let chain: Rc<[Ancestor]> = if open {
            let mut chain = ancestors.to_vec();
            chain.push(Ancestor {
                path: entry.path.clone(),
                name: entry.name.clone(),
                depth,
                index,
            });
            chain.into()
        } else {
            ancestors.clone()
        };
        rows.push(TreeRow::Entry {
            entry,
            depth,
            ancestors: chain.clone(),
        });
        if !open {
            continue;
        }
        let Some(key) = key else {
            continue;
        };
        if source.loading.contains(&key) {
            rows.push(TreeRow::Note {
                path: &entry.path,
                message: "Loading folder…",
                depth: depth + 1,
                ancestors: chain.clone(),
            });
            continue;
        }
        if let Some(error) = source.errors.get(&key) {
            rows.push(TreeRow::Note {
                path: &entry.path,
                message: error,
                depth: depth + 1,
                ancestors: chain.clone(),
            });
            continue;
        }
        flatten(source, &entry.path, depth + 1, chain, rows);
    }
}

/// Everything a render of the tree works out before it builds a row.
///
/// What Git did to each row, every visible row of the checkout's tree, and how
/// wide the widest of them wants to be. The width is the widest row in the whole
/// tree and not the widest row on screen: Legacy's rows are `max-content` and the
/// tree scrolls sideways, so a name is readable rather than shortened to fit a
/// panel that can be resized anyway — which is the only reason a deep tree loses
/// its tail today. A tree measured by what happens to be on screen grows as it is
/// scrolled, which moves the geometry while somebody is dragging the scrollbar
/// that is meant to be the tree's.
pub(crate) struct PreparedTree<'a> {
    pub(crate) rows: Vec<TreeRow<'a>>,
    pub(crate) widest: f32,
    /// What Git did to each path of this checkout, which every row reads as it is
    /// built rather than by scanning every changed file once per row.
    pub(crate) statuses: Option<&'a StatusIndex>,
}

/// One render's whole preparation, from the window's own collections to the rows
/// it is about to draw.
///
/// Nothing here is copied on the way in: the listings, the folders that are open,
/// the ones still answering and the ones that failed are read where they already
/// are, because a parent render repeats this on every frame the tree is on screen
/// and none of it changes between two of them.
///
/// Deliberately the whole tree and not the window of it: the rows that were not
/// built are still there as space above and below, so the scrollbar is the whole
/// tree's and not the window's.
pub(crate) fn prepare_tree<'a>(
    source: &TreeSource<'a>,
    widths: &mut WidthCache,
    window: &gpui_kit::Window,
) -> PreparedTree<'a> {
    let mut rows = Vec::new();
    flatten(source, "", 0, Rc::new([]), &mut rows);
    // No floor at the panel's width: a row shorter than the panel is stretched to
    // it by the layout, so a tree that fits scrolls nowhere without this having to
    // know how wide the panel is inside its border.
    let widest = rows
        .iter()
        .map(|row| match row {
            TreeRow::Entry { entry, depth, .. } => {
                let FileStatus {
                    marker,
                    additions,
                    deletions,
                } = status_of(source.statuses, &entry.path);
                row_width(
                    widths,
                    window,
                    *depth,
                    &entry.name,
                    marker,
                    additions,
                    deletions,
                )
            }
            TreeRow::Note { message, depth, .. } => {
                row_width(widths, window, *depth, message, "", None, None)
            }
        })
        .fold(0., f32::max);
    PreparedTree {
        rows,
        widest,
        statuses: source.statuses,
    }
}

impl TerminalWindow {
    pub(super) fn request_directory(
        &mut self,
        repo: Repo,
        checkout: Checkout,
        path: String,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        let checkout_id = checkout.id.clone();
        let key = (checkout_id.clone(), path.clone());
        let token = self.next_file_request;
        self.next_file_request += 1;
        self.file_request.insert(key.clone(), token);
        self.file_loading.insert(key.clone());
        self.file_errors.remove(&key);
        let worker = cx
            .background_executor()
            .spawn(async move { muster_core::files::list_directory(&repo, &checkout, &path) });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update_in(cx, |app, _, cx| {
                if app.file_request.get(&key) != Some(&token) {
                    cx.notify();
                    return;
                }
                app.file_loading.remove(&key);
                match result {
                    Ok(entries) => {
                        app.file_errors.remove(&key);
                        // A directory collapsed while its listing was in flight keeps
                        // nothing: the answer is dropped with it, so a tree walked wide
                        // and closed again does not stay resident. The root is never
                        // "expanded" — it is what everything else hangs from — so it
                        // always keeps its listing.
                        //
                        // Only *this* directory is judged. Clearing every other expansion
                        // on arrival is what made opening a nested folder collapse
                        // everything above it: the subtree you were walking disappeared
                        // the moment its child finished loading.
                        if key.1.is_empty() || app.expanded_files.contains(&key) {
                            app.file_entries.insert(key, entries);
                        }
                    }
                    Err(error) => {
                        app.file_errors.insert(key.clone(), error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn request_document(
        &mut self,
        repo: Repo,
        checkout: Checkout,
        path: String,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_document();
        let token = self.document_request;
        let checkout_id = checkout.id.clone();
        self.document_target = Some((checkout_id.clone(), path.clone()));
        window.blur(cx);
        self.open_diff = None;
        self.focus_pending = false;
        self.document_errors
            .remove(&(checkout_id.clone(), path.clone()));
        let key = (checkout_id.clone(), path.clone());
        self.file_loading.insert(key.clone());
        let worker = cx
            .background_executor()
            .spawn(async move { muster_core::files::read_preview(&repo, &checkout, &path) });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update_in(cx, |app, _, cx| {
                if app.document_request != token {
                    cx.notify();
                    return;
                }
                app.file_loading.remove(&key);
                match result {
                    Ok(preview) => app.open_document = Some((checkout_id, preview)),
                    Err(error) => {
                        app.open_document = None;
                        app.document_errors.insert(key, error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn request_git_status(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.git_status_running.contains(id) {
            // Coalesce new intent behind the in-flight snapshot instead of
            // losing refreshes, including Files → Changes.
            if let Some(token) = self.git_status_request.get_mut(id) {
                *token = token.wrapping_add(1);
            }
            return;
        }
        let Some(checkout) = self.navigation.checkout(id).cloned() else {
            return;
        };
        let Some(repo) = self
            .navigation
            .repos
            .iter()
            .find(|repo| repo.checkouts.iter().any(|c| c.id == id))
            .cloned()
        else {
            return;
        };
        if repo.kind != RepoKind::Git {
            return;
        }
        self.git_status_running.insert(id.to_owned());
        let token = self
            .git_status_request
            .get(id)
            .copied()
            .unwrap_or(0)
            .wrapping_add(1);
        self.git_status_request.insert(id.to_owned(), token);
        let key = id.to_owned();
        let worker = cx.background_executor().spawn(async move {
            muster_core::navigation::status(
                &repo,
                &checkout,
                muster_core::navigation::GitReadBudget::new(Duration::from_secs(20)),
            )
        });
        cx.spawn(async move |view, cx| {
            let result = worker.await;
            let _ = view.update(cx, |app, cx| {
                app.git_status_running.remove(&key);
                if app.git_status_request.get(&key) != Some(&token) {
                    app.request_git_status(&key, cx);
                    return;
                }
                match result {
                    Ok(status) => {
                        let paths: std::collections::HashSet<_> =
                            status.files.iter().map(|f| f.path.as_str()).collect();
                        app.expanded_diffs
                            .retain(|(owner, path)| owner != &key || paths.contains(path.as_str()));
                        app.accept_git_status(&key, status);
                        app.prune_diff_cache();
                        if let Some((owner, Some(path))) = app.open_diff.clone() {
                            if owner == key {
                                app.queue_file_diff(&owner, &path, true, cx);
                            }
                        }
                        // An expanded row only draws inside the central change
                        // set, so its diff is only worth reading there. Asking
                        // for them while nothing shows them would fork one Git
                        // read per expanded path in the workspace on a refresh
                        // nobody is waiting to look at.
                        if app
                            .open_diff
                            .as_ref()
                            .is_some_and(|(owner, path)| owner == &key && path.is_none())
                        {
                            let expanded: Vec<_> = app
                                .expanded_diffs
                                .iter()
                                .filter(|(owner, _)| owner == &key)
                                .cloned()
                                .collect();
                            for (owner, path) in expanded {
                                app.queue_file_diff(&owner, &path, true, cx);
                            }
                        }
                        app.git_status_errors.remove(&key);
                    }
                    Err(error) => {
                        // The answer the window already had is the answer it keeps:
                        // a read that failed says nothing new about the checkout, and
                        // a tree that emptied itself because a refresh failed would be
                        // reporting the failure as a clean checkout. The index follows
                        // the status it was built from, so it is kept with it.
                        app.git_status_errors.insert(key.clone(), error);
                    }
                }
                // A checkout row may be waiting on exactly this read to learn
                // whether it has a change set. This is the only place that
                // answer arrives, so the decision is settled here.
                app.settle_row_choice(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Take a checkout's Git status as the answer, and index it for the file tree.
    ///
    /// One place accepts an answer and one place indexes it: the tree reads the
    /// index on every render it is on screen, and a status that arrived without one
    /// would leave the tree answering rows from a status it no longer holds.
    fn accept_git_status(&mut self, id: &str, status: muster_core::navigation::GitStatus) {
        self.status_index
            .insert(id.to_owned(), status_index(&status));
        self.git_status.insert(id.to_owned(), status);
    }

    pub(crate) fn request_file_diff(&mut self, id: &str, path: &str, cx: &mut Context<Self>) {
        self.queue_file_diff(id, path, false, cx);
    }

    /// Close a folder from the tree row or from a stacked row: the same gesture.
    ///
    /// A collapsed directory keeps nothing but its name, and neither does anything under
    /// it — the subtree's listings, its pending reads and its failures all go with it.
    /// Keeping them is what made a tree walked wide and closed again stay resident, and
    /// come back open.
    pub(crate) fn collapse_directory(&mut self, checkout: &str, path: &str) {
        let key = (checkout.to_owned(), path.to_owned());
        if self.expanded_files.remove(&key) {
            self.forget_subtree(&key);
        }
    }

    /// Forget everything the tree is holding for one directory and everything under it.
    ///
    /// `within` is what keeps a sibling like `src-tauri` out of `src`'s subtree, so this
    /// is one pass over each collection rather than a walk of the filesystem.
    fn forget_subtree(&mut self, key: &(String, String)) {
        let under = |other: &(String, String)| other.0 == key.0 && within(&other.1, &key.1);
        self.expanded_files.retain(|open| !under(open));
        self.file_entries.retain(|entry, _| !under(entry));
        self.file_request.retain(|entry, _| !under(entry));
        self.file_loading.retain(|entry| !under(entry));
        self.file_errors.retain(|entry, _| !under(entry));
    }

    /// Hand a file or a folder of this checkout to the application this machine has
    /// for it, which is the only way a `.psd`, an `.mp4` or a `.png` opens at all.
    ///
    /// The path is resolved through the checkout before anything is spawned, so what
    /// is handed over is one canonical path proved to be inside the checkout, not a
    /// name that happened to be spelled like one. The opener's exit is read either
    /// way, because a row that stayed where it was looks exactly like one that
    /// opened something: the person who asked is told, or told it did not open.
    pub(crate) fn open_externally(
        &mut self,
        repo: Repo,
        checkout: Checkout,
        path: String,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        let notice = (checkout.id.clone(), path.clone());
        self.external_notices.remove(&notice);
        let worker = cx.background_executor().spawn(async move {
            let resolved = muster_core::files::external_path(&repo, &checkout, &path)?;
            muster_core::opener::open_path(&resolved)
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update_in(cx, |app, _, cx| {
                if let Err(error) = result {
                    app.external_notices.insert(notice, error);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Open a link the way [`Self::open_externally`] opens a file: handed to the
    /// opener as one argument, refused there if it is not a web address, and its
    /// exit read either way.
    ///
    /// A refusal is stated in the window's own toast rather than under
    /// the file tree: a link is read in the document pane and not in the
    /// inspector, and the pane can be read with Files not even selected. The
    /// opener still runs on the background executor, and only the statement of
    /// what it said comes back to the window.
    pub(crate) fn open_link(
        &mut self,
        url: String,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        let worker = cx
            .background_executor()
            .spawn(async move { muster_core::opener::open_url(&url) });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update_in(cx, |app, _, cx| {
                if let Err(error) = result {
                    app.push_error(error, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn queue_file_diff(&mut self, id: &str, path: &str, refresh: bool, cx: &mut Context<Self>) {
        let key = (id.to_owned(), path.to_owned());
        if (!refresh
            && self.git_diff.contains_key(&key)
            && !self.git_diff_errors.contains_key(&key))
            || self.git_diff_request.contains_key(&key)
        {
            return;
        }
        let Some(checkout) = self.navigation.checkout(id).cloned() else {
            return;
        };
        let Some(repo) = self
            .navigation
            .repos
            .iter()
            .find(|repo| repo.checkouts.iter().any(|c| c.id == id))
            .cloned()
        else {
            return;
        };
        let token = self.next_file_request;
        self.next_file_request += 1;
        self.git_diff_request.insert(key.clone(), token);
        // The failure that sent somebody here is answered by this read, so it
        // stops being drawn while the answer is on its way.
        self.git_diff_errors.remove(&key);
        let wanted = self.diff_queue.want(DiffWork {
            key: key.clone(),
            token,
            path: path.to_owned(),
            repo,
            checkout,
        });
        if !wanted {
            self.git_diff_errors.insert(
                key.clone(),
                "Too many diffs are loading. Wait for a row to finish, then retry.".into(),
            );
            // The queue said no, so this path has no read behind it. Leaving the
            // request registered would draw a read that does not exist.
            self.git_diff_request.remove(&key);
            return;
        }
        self.pump_diff_reads(cx);
    }

    /// Start as many wanted reads as there are free slots for.
    ///
    /// The queue is the only thing that hands work out, so this is the only
    /// place a `git diff` process is forked and the only place that decides how
    /// many may exist. A read that finishes pumps the next one, which is what
    /// makes the bound a bound rather than a batch.
    fn pump_diff_reads(&mut self, cx: &mut Context<Self>) {
        while let Some(work) = self.diff_queue.take(&self.git_diff_request) {
            let key = work.key.clone();
            let token = work.token;
            let worker = cx.background_executor().spawn(async move {
                muster_core::navigation::file_diff(
                    &work.repo,
                    &work.checkout,
                    &work.path,
                    muster_core::navigation::GitReadBudget::new(Duration::from_secs(20)),
                )
            });
            cx.spawn(async move |view, cx| {
                let result = worker.await;
                let _ = view.update(cx, |app, cx| {
                    app.diff_queue.finished();
                    if app.git_diff_request.get(&key) == Some(&token) {
                        app.git_diff_request.remove(&key);
                        match result {
                            Ok(diff) => {
                                let retained: usize = app.git_diff.iter().filter(|(cached, _)| *cached != &key).map(|(_, diff)| diff.patch.len()).sum();
                                if retained.saturating_add(diff.patch.len()) > crate::diff::MAX_RETAINED_PATCH_BYTES {
                                    app.git_diff_errors.insert(key.clone(), "Too many expanded diffs. Collapse another file or open this change individually.".into());
                                    app.pump_diff_reads(cx);
                                    cx.notify();
                                    return;
                                }
                                app.git_diff.insert(key.clone(), diff);
                                app.git_diff_errors.remove(&key);
                            }
                            Err(error) => {
                                app.git_diff_errors.insert(key.clone(), error);
                            }
                        }
                    }
                    app.pump_diff_reads(cx);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    pub(crate) fn files_inspector(
        &mut self,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let roles = Roles::of(cx);
        let Some(id) = self.navigation.active_checkout.as_deref() else {
            return div().into_any_element();
        };
        let Some(checkout) = self.navigation.checkout(id).cloned() else {
            return div().into_any_element();
        };
        let Some(repo) = self
            .navigation
            .repos
            .iter()
            .find(|repo| repo.checkouts.iter().any(|c| c.id == id))
            .cloned()
        else {
            return div().into_any_element();
        };
        if checkout.is_missing || self.navigation.archived.contains(id) {
            return div().into_any_element();
        }
        let checkout_id = id.to_owned();
        // The panel's width is its slot's, not the width a drag last ended on:
        // the group rescales its slots when the window resizes, and a panel that
        // kept the remembered width would leave the rest of the slot unpainted.
        let mut panel = div()
            .id("inspector-panel")
            .test_support()
            .w_full()
            .min_w_0()
            .h_full()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(roles.surface)
            .text_size(px(ROW_TEXT_PX))
            .border_l_1()
            .border_color(roles.border);
        let files_tab = self.inspector_tab == InspectorTab::Files || repo.kind != RepoKind::Git;
        let mut tabs = div()
            .flex()
            .flex_none()
            .border_b_1()
            .border_color(roles.border);
        tabs = tabs.child(inspector_tab(
            "inspector-files",
            "Files".to_owned(),
            0,
            files_tab,
            roles,
            cx.listener(|app, _, _, cx| {
                app.inspector_tab = InspectorTab::Files;
                cx.notify();
            }),
        ));
        if repo.kind == RepoKind::Git {
            let id = checkout_id.clone();
            let count = self.git_status.get(&id).map(|s| s.files.len()).unwrap_or(0);
            tabs = tabs.child(inspector_tab(
                "inspector-changes",
                "Changes".to_owned(),
                count,
                !files_tab,
                roles,
                cx.listener(move |app, _, _, cx| {
                    app.inspector_tab = InspectorTab::Changes;
                    app.request_git_status(&id, cx);
                    cx.notify();
                }),
            ));
        }
        panel = panel.child(tabs);
        if !files_tab {
            if repo.kind != RepoKind::Git {
                return panel
                    .child(
                        div()
                            .px_3()
                            .child("Changes are available for Git checkouts only."),
                    )
                    .into_any_element();
            }
            if !self.git_status.contains_key(&checkout_id)
                && !self.git_status_errors.contains_key(&checkout_id)
                && !self.git_status_running.contains(&checkout_id)
            {
                self.request_git_status(&checkout_id, cx);
            }
            let all_selected = self
                .open_diff
                .as_ref()
                .is_some_and(|(id, path)| id == &checkout_id && path.is_none());
            let id = checkout_id.clone();
            let weak = cx.weak_entity();
            let all_changes = row_chrome(
                div()
                    .id("all-changes")
                    .flex()
                    .items_center()
                    .relative()
                    .h(px(26.))
                    .gap(px(7.))
                    .px(px(8.))
                    .text_color(if all_selected {
                        roles.text
                    } else {
                        roles.text_muted
                    })
                    .child(Icon::new(IconName::SquareArrowOutUpRight).size(px(14.)))
                    .child("All changes")
                    .child(row_control(
                        "all-changes",
                        "All changes".to_owned(),
                        None,
                        all_selected,
                        None,
                        move |_, _, cx| {
                            let _ = weak.update(cx, |app, cx| app.open_changes(&id, None, cx));
                        },
                    )),
                roles,
                all_selected,
            );
            let mut body = div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .py(px(2.))
                .child(all_changes);
            if let Some(error) = self.git_status_errors.get(&checkout_id) {
                let id = checkout_id.clone();
                body = body
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .text_color(roles.status.warning)
                            .child(error.clone()),
                    )
                    .child(
                        zed_button("retry-status")
                            .ghost()
                            .small()
                            .label("Retry")
                            .on_click(
                                cx.listener(move |app, _, _, cx| app.request_git_status(&id, cx)),
                            ),
                    );
            }
            if let Some(status) = self.git_status.get(&checkout_id) {
                if status.files.is_empty() {
                    body = body.child(
                        div()
                            .px_3()
                            .py_2()
                            .text_color(roles.text_muted)
                            .child("No changed files."),
                    );
                } else {
                    let mut rows = Vec::new();
                    let mut parent = None;
                    for file in &status.files {
                        let directory = file
                            .path
                            .rsplit_once('/')
                            .map(|(dir, _)| dir)
                            .unwrap_or("/");
                        if parent != Some(directory) {
                            rows.push(ChangeRow::Directory(directory.to_owned()));
                            parent = Some(directory);
                        }
                        rows.push(ChangeRow::File(file.clone()));
                    }
                    let id = checkout_id.clone();
                    let selected = self.open_diff.clone();
                    let weak = cx.weak_entity();
                    // The change set is read in a panel that can be resized and
                    // is never wider than it is drawn: a row that wanted its
                    // whole name made the panel scroll sideways to find the rest
                    // of it. It is the panel's width now, and the row spends it
                    // in Legacy's column order — the letter and the counts, which
                    // answer what Git did, keep their room, and the name takes
                    // what is left and ends in an ellipsis.
                    let list =
                        gpui_kit::uniform_list("changes-list", rows.len(), move |range, _, _cx| {
                            range
                                .map(|index| match &rows[index] {
                                    // A heading and not a row: no surface at
                                    // rest, nothing to choose, set apart by its
                                    // ink rather than by a box.
                                    ChangeRow::Directory(directory) => div()
                                        .id(format!("change-group-{directory}"))
                                        .test_support()
                                        .flex()
                                        .items_center()
                                        .w_full()
                                        .h(px(26.))
                                        .px(px(8.))
                                        .text_color(roles.text_muted)
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .child(directory.clone()),
                                        )
                                        .into_any_element(),
                                    ChangeRow::File(file) => {
                                        let path = file.path.clone();
                                        let selected =
                                            selected.as_ref().is_some_and(|(owner, selected)| {
                                                owner == &id && selected.as_ref() == Some(&path)
                                            });
                                        let name =
                                            path.rsplit('/').next().unwrap_or(&path).to_owned();
                                        let id = id.clone();
                                        let weak = weak.clone();
                                        let tooltip = file
                                            .old_path
                                            .as_ref()
                                            .map(|old| format!("{old} → {path}"))
                                            .unwrap_or_else(|| path.clone());
                                        row_chrome(
                                            div()
                                                .id(format!("change-{path}"))
                                                .group(format!("change-row-{path}"))
                                                .flex()
                                                .items_center()
                                                .relative()
                                                .w_full()
                                                .h(px(26.))
                                                .gap(px(7.))
                                                // One level in from the heading
                                                // above it: the panel's own 8px
                                                // and the step the file tree
                                                // indents by, so a change row and
                                                // a file row sit at one depth.
                                                .pl(px(22.))
                                                .pr(px(8.))
                                                .child(status_chip(
                                                    &path,
                                                    &change_marker(&file.status),
                                                    roles,
                                                ))
                                                .child(
                                                    div()
                                                        .id(format!("change-name-{path}"))
                                                        .test_support()
                                                        .flex_1()
                                                        // `flex_1` alone will not shrink
                                                        // past a name's own width: the
                                                        // row would push its counts off
                                                        // the panel rather than shorten
                                                        // the name.
                                                        .min_w_0()
                                                        .truncate()
                                                        .text_color(if selected {
                                                            roles.text
                                                        } else {
                                                            roles.text_muted
                                                        })
                                                        .group_hover(
                                                            format!("change-row-{path}"),
                                                            |style| style.text_color(roles.text),
                                                        )
                                                        .child(name),
                                                )
                                                .child(count_chips(
                                                    &path,
                                                    (file.additions, file.deletions),
                                                    roles,
                                                ))
                                                .child(row_control(
                                                    format!("change-control-{path}"),
                                                    tooltip.clone(),
                                                    Some(tooltip),
                                                    selected,
                                                    None,
                                                    move |_, _, cx| {
                                                        let _ = weak.update(cx, |app, cx| {
                                                            app.open_changes(
                                                                &id,
                                                                Some(path.clone()),
                                                                cx,
                                                            )
                                                        });
                                                    },
                                                )),
                                            roles,
                                            selected,
                                        )
                                        .into_any_element()
                                    }
                                })
                                .collect::<Vec<_>>()
                        })
                        .flex_1()
                        .min_h_0()
                        .w_full();
                    body = body.child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .py(px(4.))
                            .child(list),
                    );
                }
            } else if !self.git_status_errors.contains_key(&checkout_id) {
                body = body.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_color(roles.text_muted)
                        .child("Loading changes…"),
                );
            }
            return panel.child(body).into_any_element();
        }

        if !self
            .file_entries
            .contains_key(&(checkout_id.clone(), String::new()))
            && !self
                .file_loading
                .contains(&(checkout_id.clone(), String::new()))
            && !self
                .file_errors
                .contains_key(&(checkout_id.clone(), String::new()))
        {
            self.request_directory(repo.clone(), checkout.clone(), String::new(), window, cx);
        }
        if repo.kind == RepoKind::Git
            && !self.git_status.contains_key(&checkout_id)
            && !self.git_status_errors.contains_key(&checkout_id)
            && !self.git_status_running.contains(&checkout_id)
        {
            self.request_git_status(&checkout_id, cx);
        }
        let selected = self.selected_file.get(&checkout_id).cloned();
        let PreparedTree {
            rows,
            widest: content_width,
            statuses,
        } = prepare_tree(
            &TreeSource {
                checkout: &checkout_id,
                entries: &self.file_entries,
                loading: &self.file_loading,
                errors: &self.file_errors,
                expanded: &self.expanded_files,
                statuses: self.status_index.get(&checkout_id),
            },
            &mut self.measured_widths,
            window,
        );
        let total = rows.len();
        let weak = cx.weak_entity();
        let stack_weak = weak.clone();
        let repo_for = repo.clone();
        let checkout_for = checkout.clone();
        let scroll = self.files_scroll.clone();
        // Before this frame's layout, the offset and bounds describe the previous
        // frame. Use that observation for the row window and sticky stack rather
        // than laying out the tree twice.
        let scrolled = -scroll.offset().y;
        let top_row = (scrolled.as_f32() / ROW_HEIGHT).floor().max(0.) as usize;
        // The stack is laid inside the tree's box and clipped by it, so what it may cover
        // is that box's height: the window's is taller, and a stack sized to it hides
        // more of the tree than it has room for.
        let stacked = sticky_ancestors(&rows, top_row, stack_room(scroll.bounds().size.height));
        let (drawn, above, below) = tree_window(total, top_row);
        let list = div()
            .id("files-tree")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_scroll()
            .track_scroll(&scroll)
            .child(div().flex_none().h(px(above)))
            .children(drawn.map(|index| {
                match &rows[index] {
                    TreeRow::Note {
                        path,
                        message,
                        depth,
                        ..
                    } => note_row(
                        path,
                        message,
                        *depth,
                        px(content_width),
                        (repo_for.clone(), checkout_for.clone()),
                        weak.clone(),
                        roles,
                    ),
                    TreeRow::Entry { entry, depth, .. } => {
                        let path = entry.path.clone();
                        let directory = entry.directory;
                        let key = (checkout_for.id.clone(), path.clone());
                        let open = self.expanded_files.contains(&key);
                        let empty =
                            directory && listed_directory_is_empty(&self.file_entries, &key);
                        let FileStatus {
                            marker,
                            additions,
                            deletions,
                        } = status_of(statuses, &path);
                        let (added, removed) = (additions, deletions);
                        let selected = selected.as_deref() == Some(path.as_str());
                        let weak = weak.clone();
                        let repo = repo_for.clone();
                        let checkout = checkout_for.clone();
                        // A dotfile is quieted rather than recoloured: a colour of
                        // its own would win over the one the row hands down.
                        let quiet = entry.name.starts_with('.');
                        row_chrome(
                            div()
                                .id(format!("file-row-{path}"))
                                .group(format!("file-row-{path}"))
                                .flex()
                                .items_center()
                                .relative()
                                .min_w(px(content_width))
                                .h(px(26.))
                                .gap(px(7.))
                                .pl(px(8. + *depth as f32 * 14.))
                                .pr(px(8.))
                                // The fold's slot, held at its width on a file so
                                // a folder's glyph and a file's icon land on one
                                // column and a name does not move under the
                                // pointer.
                                .child(
                                    div()
                                        .id(format!("file-slot-{path}"))
                                        .test_support()
                                        .flex_shrink_0()
                                        .w(px(16.))
                                        .h(px(16.))
                                        .when(directory && !empty, |slot| {
                                            slot.child(Icon::new(if open {
                                                IconName::ChevronDown
                                            } else {
                                                IconName::ChevronRight
                                            }))
                                        }),
                                )
                                .child(file_icons::element(
                                    format!("file-icon-{path}"),
                                    &entry.name,
                                    directory,
                                    px(14.),
                                    &roles,
                                ))
                                .child(
                                    div()
                                        .id(format!("file-name-{path}"))
                                        .test_support()
                                        .flex_1()
                                        .whitespace_nowrap()
                                        .text_color(if selected || directory {
                                            roles.text
                                        } else {
                                            roles.text_muted
                                        })
                                        .group_hover(format!("file-row-{path}"), |style| {
                                            style.text_color(roles.text)
                                        })
                                        .when(quiet, |name| name.opacity(0.72))
                                        .child(entry.name.clone()),
                                )
                                // Legacy's order: the fold, the icon, the name,
                                // then what Git did to it and what that cost.
                                .when(!marker.is_empty(), |row| {
                                    row.child(status_chip(&path, marker, roles))
                                })
                                .child(count_chips(&path, (added, removed), roles))
                                .child(row_control(
                                    format!("file-control:{path}"),
                                    path.clone(),
                                    Some(path.clone()),
                                    selected,
                                    (directory && !empty).then_some(open),
                                    move |event, window, cx| {
                                        let _ = weak.update(cx, |app, cx| {
                                            // Ctrl on Linux and Windows, cmd on
                                            // macOS: the modifier that means "open
                                            // this somewhere else". It is the row's
                                            // only other answer, and it never is both
                                            // at once — a row that opened somewhere
                                            // else and also changed this view would
                                            // be answering a gesture nobody asked.
                                            if opens_elsewhere(event) {
                                                app.open_externally(
                                                    repo.clone(),
                                                    checkout.clone(),
                                                    path.clone(),
                                                    window,
                                                    cx,
                                                );
                                                return;
                                            }
                                            if directory {
                                                if empty {
                                                    return;
                                                }
                                                let key = (checkout.id.clone(), path.clone());
                                                if !app.expanded_files.remove(&key) {
                                                    app.expanded_files.insert(key.clone());
                                                    if !app.file_entries.contains_key(&key)
                                                        && !app.file_loading.contains(&key)
                                                    {
                                                        app.request_directory(
                                                            repo.clone(),
                                                            checkout.clone(),
                                                            path.clone(),
                                                            window,
                                                            cx,
                                                        );
                                                    }
                                                } else {
                                                    app.forget_subtree(&key);
                                                }
                                            } else {
                                                app.selected_file
                                                    .insert(checkout.id.clone(), path.clone());
                                                app.request_document(
                                                    repo.clone(),
                                                    checkout.clone(),
                                                    path.clone(),
                                                    window,
                                                    cx,
                                                );
                                            }
                                            cx.notify();
                                        });
                                    },
                                )),
                            roles,
                            selected,
                        )
                        .into_any_element()
                    }
                }
            }))
            .child(div().flex_none().h(px(below)));
        let root = (checkout_id.clone(), String::new());
        let mut body = div().flex_1().min_h_0().flex().flex_col();
        // The stack is drawn over the rows it names rather than above them, which is
        // what Legacy's negative margin buys in CSS and what a second child of the same
        // box buys here: the tree's geometry does not move when the stack appears.
        let stacked_for = stacked
            .iter()
            .map(|ancestor| {
                sticky_row(
                    ancestor,
                    &checkout_for.id,
                    px(content_width),
                    stack_weak.clone(),
                    roles,
                )
            })
            .collect::<Vec<_>>();
        let has_stack = !stacked.is_empty();
        body = body.child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .relative()
                .overflow_hidden()
                .py(px(4.))
                .child(list.flex_1().min_h_0().w_full())
                .when(has_stack, |tree| {
                    tree.child(
                        div()
                            .id("file-stack")
                            .test_support()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .flex()
                            .flex_col()
                            .bg(roles.surface)
                            .children(stacked_for),
                    )
                }),
        );
        // The root's own state is drawn below the tree rather than in it: there is
        // no directory row for a note to sit under. Everything a folder below the
        // root has to say about itself is a row inside the list, where it can be
        // scrolled to and cannot resize the list around it.
        if let Some(error) = self.file_errors.get(&root) {
            body = body.child(
                div()
                    .px_3()
                    .py_1()
                    .text_color(roles.status.warning)
                    .child(format!("root/: {error}")),
            );
            let rrepo = repo.clone();
            let rcheckout = checkout.clone();
            body = body.child(
                zed_button("retry-list-root")
                    .ghost()
                    .small()
                    .label("Retry root/")
                    .accessibility_label("Retry listing the checkout root")
                    .on_click(cx.listener(move |app, _, window, cx| {
                        app.request_directory(
                            rrepo.clone(),
                            rcheckout.clone(),
                            String::new(),
                            window,
                            cx,
                        );
                    })),
            );
        }
        if self.file_loading.contains(&root) {
            body = body.child(
                div()
                    .px_3()
                    .py_1()
                    .text_color(roles.text_muted)
                    .child("Loading files…"),
            );
        }
        if self.file_entries.get(&root).is_some_and(Vec::is_empty)
            && !self.file_loading.contains(&root)
            && !self.file_errors.contains_key(&root)
        {
            body = body.child(div().px_3().child("No files"));
        }
        // What the opener said about a file of *this* checkout that would not
        // open. It is drawn under the tree for the same reason the root's own
        // failure is: a row that stayed where it was looks exactly like one that
        // opened something, so the refusal is stated where the gesture was made.
        for ((notice_checkout, path), error) in &self.external_notices {
            if notice_checkout != &checkout_id {
                continue;
            }
            let message = error.clone();
            let dismiss = (notice_checkout.clone(), path.clone());
            body = body.child(
                div()
                    .px_3()
                    .py_1()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .text_color(roles.status.warning)
                    .child(div().min_w_0().truncate().child(message))
                    .child(
                        zed_button(format!("dismiss-external-{path}"))
                            .text()
                            .small()
                            .rounded(px(0.))
                            .label("Dismiss")
                            .accessibility_label("Dismiss the message about this file")
                            .on_click(cx.listener(move |app, _, _, _| {
                                app.external_notices.remove(&dismiss);
                            })),
                    ),
            );
        }
        panel.child(body).into_any_element()
    }
}

/// The queue is arithmetic, so it is checked without a window.
///
/// The three properties are the ones that keep a workspace from forking Git
/// once per changed file: the number running is the limit whatever is queued
/// behind it, a read nobody wants any more is dropped instead of run, and a
/// read that has started keeps its slot until it has actually finished, so a
/// stale answer still costs what a wanted one costs.
#[cfg(test)]
mod tests {
    use super::*;

    /// Closing a directory forgets its subtree and nothing else. The separator is the whole
    /// test: `src-tauri` shares a prefix with `src` and has nothing to do with it.
    #[test]
    fn within_spans_a_directory_and_its_descendants_only() {
        assert!(within("outer", "outer"));
        assert!(within("outer/inner", "outer"));
        assert!(within("outer/inner/deep.txt", "outer"));
        assert!(!within("src-tauri", "src"));
        assert!(!within("outermost", "outer"));
        assert!(!within("", "outer"));
        assert!(within("outer", ""));
    }

    #[test]
    fn a_folder_is_empty_only_after_an_empty_listing_arrives() {
        let empty = ("checkout".into(), "empty".into());
        let populated = ("checkout".into(), "populated".into());
        let entries = HashMap::from([
            (empty.clone(), Vec::new()),
            (
                populated.clone(),
                vec![muster_core::files::FileEntry {
                    name: "file".into(),
                    path: "populated/file".into(),
                    directory: false,
                }],
            ),
        ]);
        assert!(listed_directory_is_empty(&entries, &empty));
        assert!(!listed_directory_is_empty(&entries, &populated));
        assert!(!listed_directory_is_empty(
            &entries,
            &("checkout".into(), "unread".into())
        ));
    }

    #[test]
    fn files_show_filtered_markers_from_both_porcelain_columns() {
        for (status, marker) in [
            (" M", "M"),
            ("MM", "M"),
            ("AM", "A"),
            (" D", "D"),
            ("UU", "U"),
            ("AA", "U"),
            ("??", ""),
            ("R ", ""),
        ] {
            assert_eq!(file_marker(status), marker);
        }
    }

    #[test]
    fn a_diff_queue_runs_so_many_reads_and_drops_the_rest() {
        let directory = tempfile::tempdir().unwrap();
        let repo = Repo::plain(directory.path(), "queue").unwrap();
        let checkout = repo.checkouts[0].clone();
        let work = |index: usize| DiffWork {
            key: ("checkout".to_owned(), format!("file-{index}")),
            token: index as u64,
            path: format!("file-{index}"),
            repo: repo.clone(),
            checkout: checkout.clone(),
        };
        let mut queue = DiffQueue::default();
        let mut wanted = HashMap::new();
        let offered = DIFF_CONCURRENCY + 3;
        for index in 0..offered {
            let work = work(index);
            wanted.insert(work.key.clone(), work.token);
            assert!(queue.want(work), "a queue with room refused a read");
        }
        for _ in 0..DIFF_CONCURRENCY {
            assert!(queue.take(&wanted).is_some());
        }
        assert_eq!(queue.running(), DIFF_CONCURRENCY);
        assert!(
            queue.take(&wanted).is_none(),
            "the queue started more reads than the limit"
        );
        assert_eq!(queue.waiting(), offered - DIFF_CONCURRENCY);
        // A read invalidated while it waited gives its room back and drops out,
        // and the wanted read behind it runs in its place.
        let stale = work(DIFF_CONCURRENCY).key.clone();
        wanted.remove(&stale);
        for _ in 0..DIFF_CONCURRENCY {
            queue.finished();
        }
        let next = queue
            .take(&wanted)
            .expect("a wanted read behind a stale one");
        assert_ne!(next.key, stale);
        assert_eq!(queue.running(), 1);
        assert_eq!(queue.waiting(), offered - DIFF_CONCURRENCY - 2);
        // Stopping wanting a path takes it out of the queue, and nothing else:
        // the read already running keeps its slot until it finishes.
        let dropped = work(501).key.clone();
        for read in [work(500), work(501)] {
            wanted.insert(read.key.clone(), read.token);
            assert!(queue.want(read));
        }
        let before = queue.waiting();
        queue.forget(&dropped);
        assert_eq!(queue.waiting(), before - 1);
        assert_eq!(queue.running(), 1);
        // A refresh of a path whose read is still running is another entry, not
        // another slot: the running read still counts, and topping the queue up
        // past that still does not start more than the limit.
        let refreshed = DiffWork {
            key: next.key.clone(),
            token: wanted[&next.key] + 100,
            ..work(0)
        };
        wanted.insert(next.key.clone(), refreshed.token);
        assert!(queue.want(refreshed));
        while queue.running() < DIFF_CONCURRENCY {
            assert!(queue.take(&wanted).is_some());
            let filler = work(600 + queue.running());
            wanted.insert(filler.key.clone(), filler.token);
            assert!(queue.want(filler));
        }
        assert!(
            queue.take(&wanted).is_none(),
            "a same-path refresh started more reads than the limit"
        );
        assert_eq!(queue.running(), DIFF_CONCURRENCY);
        for _ in 0..DIFF_CONCURRENCY {
            queue.finished();
        }
        // Every wanted read runs once the slots come back, and the queue does
        // not keep anything the window has moved on from.
        while queue.take(&wanted).is_some() {}
        assert_eq!(queue.waiting(), 0);
        // A pump with nothing left to hand out spends nothing: a slot is only
        // ever held by a read that exists.
        let settled = queue.running();
        assert!(queue.take(&wanted).is_none());
        assert_eq!(queue.running(), settled);
        let mut full = DiffQueue::default();
        for index in 0..DIFF_QUEUE_LIMIT {
            assert!(full.want(work(index)));
        }
        assert!(
            !full.want(work(DIFF_QUEUE_LIMIT)),
            "the queue grew past its limit"
        );
        assert_eq!(full.waiting(), DIFF_QUEUE_LIMIT);
    }
}
