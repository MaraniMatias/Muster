//! The window's line-anchored review drafts and their Markdown export.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::app::TerminalWindow;
use crate::theme::{zed_button, Roles};
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::dialog::{Dialog, DialogContent, DialogFooter};
use gpui_kit::component::diff::{DiffLineRange, DiffSide};
use gpui_kit::component::input::TextareaState;
use gpui_kit::component::select::{SelectEvent, SelectState};
use gpui_kit::component::IndexPath;
use gpui_kit::{
    div, px, AnyElement, AppContext, Context, Entity, Focusable, IntoElement, ParentElement,
    SharedString, Styled, Window,
};
use muster_core::review::{
    self, review_anchor_hash, ReviewContext, ReviewNote, ReviewSide, ReviewStorage,
    MAX_NOTE_CODE_BYTES, MAX_NOTE_CONTENT_BYTES, MAX_REVIEW_NOTES,
};

static NEXT_NOTE_ID: AtomicU64 = AtomicU64::new(0);

type AnchorHashes = HashMap<(ReviewSide, usize), Option<String>>;
type AnchorHashCache = HashMap<(String, String), (String, AnchorHashes)>;

/// Candidate first lines for a note range, limited to lines present on its side and not past its end.
pub(crate) fn review_start_lines(
    lines: &BTreeMap<(ReviewSide, usize), muster_core::review::DiffLine>,
    side: ReviewSide,
    end: usize,
) -> Vec<usize> {
    if end == 0 {
        return Vec::new();
    }
    let mut starts = Vec::new();
    let mut expected = end;
    let mut bytes = 0;
    for ((_, number), line) in lines.range((side, 1)..=(side, end)).rev() {
        if *number != expected {
            break;
        }
        bytes += line.text.len() + usize::from(!starts.is_empty());
        if bytes > MAX_NOTE_CODE_BYTES {
            break;
        }
        starts.push(*number);
        expected = number.saturating_sub(1);
    }
    starts.reverse();
    starts
}

pub(crate) fn cached_anchor_hashes(
    state: &mut ReviewState,
    checkout_id: &str,
    path: &str,
    revision: &str,
    patch: &str,
    notes: &[ReviewNote],
) -> AnchorHashes {
    let key = (checkout_id.to_owned(), path.to_owned());
    if let Some((cached_revision, hashes)) = state.anchor_hashes.get(&key) {
        if cached_revision == revision {
            return hashes.clone();
        }
    }
    let lines = review::read_diff_lines(patch);
    let hashes: AnchorHashes = notes
        .iter()
        .filter(|note| note.path == path)
        .map(|note| {
            (
                (note.side, note.line_start),
                lines
                    .get(&(note.side, note.line_start))
                    .map(|line| review_anchor_hash(&line.text)),
            )
        })
        .collect();
    state
        .anchor_hashes
        .insert(key, (revision.to_owned(), hashes.clone()));
    hashes
}

#[derive(Default)]
pub(crate) struct ReviewState {
    pub(crate) notes: HashMap<String, Vec<ReviewNote>>,
    pub(crate) anchor_hashes: AnchorHashCache,
    pub(crate) composer: Option<ReviewComposer>,
    pub(crate) include_outdated: bool,
    pub(crate) clear_confirmation: bool,
    pub(crate) clearing: bool,
    pub(crate) exporting: bool,
    pub(crate) opencode_busy: bool,
    pub(crate) opencode_loading: bool,
    pub(crate) opencode_destination: Option<muster_core::opencode::Destination>,
    pub(crate) opencode_rounds: Vec<muster_core::opencode::ReviewRound>,
    pub(crate) opencode_error: Option<String>,
    opencode_checkout: Option<String>,
    opencode_generation: u64,
}

pub(crate) fn can_start_opencode_round(rounds: &[muster_core::opencode::ReviewRound]) -> bool {
    !rounds.iter().any(|round| round.status.is_pending())
}

/// The notes a new OpenCode round would carry.
///
/// The window's own notes, minus the ones a round already sent as they stand: a review
/// that was delivered is not delivered again because the window was reopened, and a note
/// somebody edited since is a new comment and is kept.
pub(crate) fn opencode_send_notes(state: &ReviewState, checkout_id: &str) -> Vec<ReviewNote> {
    muster_core::opencode::unsent_notes(
        state
            .notes
            .get(checkout_id)
            .map(Vec::as_slice)
            .unwrap_or_default(),
        &state.opencode_rounds,
    )
}

/// How a round reads in the diff's own words, which is the only place its state is shown.
pub(crate) fn opencode_round_state(status: muster_core::opencode::RoundStatus) -> &'static str {
    use muster_core::opencode::RoundStatus;
    match status {
        RoundStatus::Queued => "queued",
        RoundStatus::Dispatching => "sending",
        // The one state that has to say itself: Muster will not resend it on its own.
        RoundStatus::Unknown => "not delivered, and Muster will not send it again",
        RoundStatus::Dispatched => "delivered",
        RoundStatus::Acked => "finished",
    }
}

pub(crate) struct ReviewComposer {
    pub(crate) checkout_id: String,
    pub(crate) path: String,
    pub(crate) side: ReviewSide,
    pub(crate) line_start: usize,
    pub(crate) line_end: usize,
    pub(crate) code: String,
    pub(crate) editing_id: Option<String>,
    pub(crate) input: Entity<TextareaState>,
    /// The toolkit select that moves where this note's range starts.
    pub(crate) start_picker: Entity<SelectState<Vec<SharedString>>>,
    _save_keys: gpui_kit::Subscription,
    _picker_events: gpui_kit::Subscription,
    _subscription: gpui_kit::Subscription,
}

/// A line as this app spells it: a removed line is a negative number.
///
/// The export says a note about a removed line with the same minus, so `{-3}`
/// means old line 3 and `{10}` means new line 10. One convention in the picker
/// and in the Markdown is what lets a reader of the export tell the two apart.
pub(crate) fn review_line_label(line: usize, side: ReviewSide) -> SharedString {
    SharedString::from(match side {
        ReviewSide::Old => format!("-{line}"),
        ReviewSide::New => line.to_string(),
    })
}

/// The line a picker item stands for, with the minus a removed line carries.
fn review_line_number(label: &str) -> Option<usize> {
    label.trim_start_matches('-').parse().ok()
}

/// The line numbers a note range could start on, and the one this note starts at.
///
/// The delegate is the toolkit's own `Vec<SharedString>`, so a candidate is a
/// line number and nothing else: the mark and a code preview would put a word
/// beside an address, and the header row already says which lines the range
/// covers.
fn review_start_choices(
    lines: &BTreeMap<(ReviewSide, usize), muster_core::review::DiffLine>,
    side: ReviewSide,
    start: usize,
    end: usize,
) -> (Vec<SharedString>, Option<IndexPath>) {
    let numbers = review_start_lines(lines, side, end);
    (
        numbers
            .iter()
            .map(|line| review_line_label(*line, side))
            .collect(),
        numbers
            .iter()
            .position(|line| *line == start)
            .map(|row| IndexPath::default().row(row)),
    )
}

/// A draft's range picker, and the route from what it confirms back to the draft.
///
/// The confirm carries a line number, so what choosing one means belongs to
/// [`TerminalWindow::set_review_note_start`]: the picker reports what was picked
/// and nothing else, which is what keeps the two one state rather than two.
fn review_start_picker(
    lines: &BTreeMap<(ReviewSide, usize), muster_core::review::DiffLine>,
    side: ReviewSide,
    start: usize,
    end: usize,
    window: &mut Window,
    cx: &mut Context<TerminalWindow>,
) -> (
    Entity<SelectState<Vec<SharedString>>>,
    gpui_kit::Subscription,
) {
    let (items, selected) = review_start_choices(lines, side, start, end);
    let picker = cx.new(|cx| SelectState::new(items, selected, window, cx));
    let events = cx.subscribe_in(&picker, window, |app, _, event, window, cx| {
        let SelectEvent::Confirm(Some(line)) = event else {
            return;
        };
        if let Some(line) = review_line_number(line) {
            app.set_review_note_start(line, window, cx);
        }
    });
    (picker, events)
}

/// The keystroke that finishes a note, registered for as long as the composer is open.
///
/// An interceptor rather than a key handler on the card because it runs before the
/// input's own Return: stopping it here is what keeps ⌘↵ from also inserting the
/// newline a plain Return would, and so keeps a trailing blank line out of the
/// saved note. Enter on its own still writes the newline Legacy's textarea did.
fn review_save_keys(
    input: &Entity<TextareaState>,
    cx: &mut Context<TerminalWindow>,
) -> gpui_kit::Subscription {
    let focus = input.read(cx).focus_handle(cx);
    let weak = cx.weak_entity();
    cx.intercept_keystrokes(move |event, window, cx| {
        if !focus.is_focused(window) || event.keystroke.key != "enter" {
            return;
        }
        let app_modifier = if cfg!(target_os = "macos") {
            event.keystroke.modifiers.platform
        } else {
            event.keystroke.modifiers.control
        };
        if !app_modifier || event.keystroke.modifiers.shift {
            return;
        }
        let _ = weak.update(cx, |app, cx| app.save_review_note(cx));
        cx.stop_propagation();
    })
}

impl TerminalWindow {
    pub(crate) fn begin_review_note(
        &mut self,
        range: &DiffLineRange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((checkout_id, _)) = self.open_diff.as_ref() else {
            return;
        };
        let path = range.path().to_string();
        let Some(diff) = self.git_diff.get(&(checkout_id.clone(), path.clone())) else {
            return;
        };
        let (side, start, end) = if range.side() == range.end_side() {
            let start = range.start().min(range.end());
            let end = range.start().max(range.end());
            (range.side(), start, end)
        } else {
            (range.end_side(), range.end(), range.end())
        };
        let side = match side {
            DiffSide::Original => ReviewSide::Old,
            DiffSide::Modified => ReviewSide::New,
        };
        // A range is written the way GitLab writes one: the `+` fixes where the
        // note ends, and a `+` clicked on a line above an open one says where it
        // starts. So the click above widens the note already being written and
        // keeps its text — it is the same comment about more of the same change —
        // while a click at or below it is a different line and opens a new note.
        // A note being edited is not a draft to widen: its range is what it was
        // saved against, so any `+` opens a new one instead.
        if self.review.composer.as_ref().is_some_and(|composer| {
            composer.editing_id.is_none()
                && composer.checkout_id == *checkout_id
                && composer.path == path
                && composer.side == side
                && start < composer.line_end
        }) {
            self.set_review_note_start(start, window, cx);
            return;
        }
        let lines = review::read_diff_lines(&diff.patch);
        let Some(code) = review::review_range_code(&lines, side, start, end) else {
            self.push_error("The selected diff lines are not loaded.", cx);
            return;
        };
        if code.len() > MAX_NOTE_CODE_BYTES {
            self.push_error("A review note cannot anchor more than 8 KiB of code.", cx);
            return;
        }
        let input = cx.new(|cx| {
            let mut input = TextareaState::new(window, cx).auto_grow(1, 6);
            input.set_placeholder("What should change here?", window, cx);
            input
        });
        input.read(cx).focus_handle(cx).focus(window, cx);
        let (start_picker, picker_events) =
            review_start_picker(&lines, side, start, end, window, cx);
        let save_keys = review_save_keys(&input, cx);
        let subscription = cx.observe(&input, |_, _, cx| cx.notify());
        self.review.composer = Some(ReviewComposer {
            checkout_id: checkout_id.clone(),
            path,
            side,
            line_start: start,
            line_end: end,
            code,
            editing_id: None,
            input,
            start_picker,
            _save_keys: save_keys,
            _picker_events: picker_events,
            _subscription: subscription,
        });
        self.sync_review_note_selection(cx);
        cx.notify();
    }

    pub(crate) fn edit_review_note(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(note) = self
            .review
            .notes
            .values()
            .flatten()
            .find(|note| note.id == id)
            .cloned()
        else {
            return;
        };
        let input = cx.new(|cx| {
            let mut input = TextareaState::new(window, cx).auto_grow(1, 6);
            input.set_placeholder("What should change here?", window, cx);
            input.set_value(note.content.clone(), window, cx);
            input
        });
        input.read(cx).focus_handle(cx).focus(window, cx);
        let lines = self
            .git_diff
            .get(&(note.checkout_id.clone(), note.path.clone()))
            .map(|diff| review::read_diff_lines(&diff.patch))
            .unwrap_or_default();
        let line_end = note.line_end.unwrap_or(note.line_start);
        let (start_picker, picker_events) =
            review_start_picker(&lines, note.side, note.line_start, line_end, window, cx);
        let save_keys = review_save_keys(&input, cx);
        let subscription = cx.observe(&input, |_, _, cx| cx.notify());
        self.review.composer = Some(ReviewComposer {
            checkout_id: note.checkout_id,
            path: note.path,
            side: note.side,
            line_start: note.line_start,
            line_end,
            code: note.code,
            editing_id: Some(note.id),
            input,
            start_picker,
            _save_keys: save_keys,
            _picker_events: picker_events,
            _subscription: subscription,
        });
        self.sync_review_note_selection(cx);
        cx.notify();
    }

    /// The lines a draft covers, washed in the diff's own selection.
    ///
    /// A note is about a range, and a range is invisible while the box that writes
    /// it floats over the diff: the start moves, the end does not, and nothing on
    /// screen says which lines the comment now covers. The toolkit's selection
    /// already paints that state, so the composer drives it rather than washing
    /// the lines a second time in a colour of its own. Called from where a draft
    /// is opened, moved or dropped, and from a diff that was rebuilt underneath
    /// one: a fresh state starts with nothing selected.
    pub(crate) fn sync_review_note_selection(&mut self, cx: &mut Context<Self>) {
        let range = self.review.composer.as_ref().map(|composer| {
            DiffLineRange::new(
                composer.path.clone(),
                match composer.side {
                    ReviewSide::Old => DiffSide::Original,
                    ReviewSide::New => DiffSide::Modified,
                },
                composer.line_start,
                composer.line_end,
            )
        });
        if let Some(view) = self.diff_view.as_ref() {
            view.state
                .update(cx, |state, cx| state.set_selected_lines(range, cx));
        }
    }

    pub(crate) fn set_review_note_start(
        &mut self,
        line_start: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(composer) = self.review.composer.as_mut() else {
            return;
        };
        if line_start > composer.line_end || line_start == 0 {
            return;
        }
        let Some(diff) = self
            .git_diff
            .get(&(composer.checkout_id.clone(), composer.path.clone()))
        else {
            return;
        };
        let lines = review::read_diff_lines(&diff.patch);
        let Some(code) =
            review::review_range_code(&lines, composer.side, line_start, composer.line_end)
        else {
            return;
        };
        if code.len() > MAX_NOTE_CODE_BYTES {
            return;
        }
        composer.line_start = line_start;
        composer.code = code;
        // The picker is how this line is chosen, so it is told what the draft now
        // says rather than left to agree with it by itself.
        let chosen = review_line_label(line_start, composer.side);
        composer.start_picker.update(cx, |picker, cx| {
            picker.set_selected_value(&chosen, window, cx)
        });
        self.review
            .anchor_hashes
            .remove(&(composer.checkout_id.clone(), composer.path.clone()));
        self.sync_review_note_selection(cx);
        cx.notify();
    }

    pub(crate) fn save_review_note(&mut self, cx: &mut Context<Self>) {
        let Some(composer) = self.review.composer.as_ref() else {
            return;
        };
        let content = composer.input.read(cx).value().to_string();
        if content.trim().is_empty() {
            return;
        }
        if content.len() > MAX_NOTE_CONTENT_BYTES {
            self.push_error("A review note cannot be longer than 8 KiB.", cx);
            return;
        }
        if composer.code.len() > MAX_NOTE_CODE_BYTES {
            self.push_error("A review note cannot anchor more than 8 KiB of code.", cx);
            return;
        }
        let composer = self.review.composer.take().expect("composer was checked");
        if composer.editing_id.is_none() {
            self.review
                .anchor_hashes
                .remove(&(composer.checkout_id.clone(), composer.path.clone()));
        }
        let notes = self
            .review
            .notes
            .entry(composer.checkout_id.clone())
            .or_default();
        if let Some(id) = composer.editing_id {
            if let Some(note) = notes.iter_mut().find(|note| note.id == id) {
                let line_end =
                    (composer.line_end != composer.line_start).then_some(composer.line_end);
                let reanchored = note.line_start != composer.line_start
                    || note.line_end != line_end
                    || note.code != composer.code;
                note.content = content;
                if reanchored {
                    note.line_start = composer.line_start;
                    note.line_end = line_end;
                    note.code_hash = review_anchor_hash(&composer.code);
                    note.code = composer.code;
                    note.outdated = false;
                }
            }
        } else {
            if notes.len() >= MAX_REVIEW_NOTES {
                self.review.composer = Some(composer);
                self.push_error("A checkout cannot hold more than 1000 review notes.", cx);
                return;
            }
            let serial = NEXT_NOTE_ID.fetch_add(1, Ordering::Relaxed);
            let epoch = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let id = format!("note-{}-{epoch}-{serial}", std::process::id());
            notes.push(ReviewNote {
                id,
                checkout_id: composer.checkout_id,
                path: composer.path,
                side: composer.side,
                line_start: composer.line_start,
                line_end: (composer.line_end != composer.line_start).then_some(composer.line_end),
                content,
                code_hash: review_anchor_hash(&composer.code),
                code: composer.code,
                outdated: false,
            });
        }
        self.sync_review_note_selection(cx);
        cx.notify();
    }

    pub(crate) fn cancel_review_note(&mut self, cx: &mut Context<Self>) {
        self.review.composer = None;
        self.sync_review_note_selection(cx);
        cx.notify();
    }

    pub(crate) fn delete_review_note(
        &mut self,
        checkout_id: &str,
        id: &str,
        cx: &mut Context<Self>,
    ) {
        let path = self
            .review
            .notes
            .get(checkout_id)
            .and_then(|notes| notes.iter().find(|note| note.id == id))
            .map(|note| note.path.clone());
        if let Some(path) = path {
            self.review
                .anchor_hashes
                .remove(&(checkout_id.to_owned(), path));
        }
        if let Some(notes) = self.review.notes.get_mut(checkout_id) {
            notes.retain(|note| note.id != id);
        }
        if self
            .review
            .composer
            .as_ref()
            .is_some_and(|draft| draft.editing_id.as_deref() == Some(id))
        {
            self.review.composer = None;
            self.sync_review_note_selection(cx);
        }
        cx.notify();
    }

    pub(crate) fn accept_outdated_note(
        &mut self,
        checkout_id: &str,
        id: &str,
        cx: &mut Context<Self>,
    ) {
        let current_hash = self
            .review
            .notes
            .get(checkout_id)
            .and_then(|notes| notes.iter().find(|note| note.id == id))
            .and_then(|note| {
                let diff = self
                    .git_diff
                    .get(&(checkout_id.to_owned(), note.path.clone()))?;
                self.review
                    .anchor_hashes
                    .get(&(checkout_id.to_owned(), note.path.clone()))
                    .filter(|(revision, _)| revision == &diff.revision)
                    .and_then(|(_, hashes)| hashes.get(&(note.side, note.line_start)))
                    .and_then(Option::as_ref)
                    .cloned()
                    .or_else(|| {
                        review::read_diff_lines(&diff.patch)
                            .get(&(note.side, note.line_start))
                            .map(|line| review_anchor_hash(&line.text))
                    })
            });
        if let Some(note) = self
            .review
            .notes
            .get_mut(checkout_id)
            .and_then(|notes| notes.iter_mut().find(|note| note.id == id))
        {
            if let Some(current_hash) = current_hash {
                note.code_hash = current_hash;
            }
            note.outdated = false;
            cx.notify();
        }
    }

    pub(crate) fn open_review_clear_confirmation(&mut self, cx: &mut Context<Self>) {
        if self.navigation.review_storage == ReviewStorage::Default {
            self.review.clear_confirmation = true;
            cx.notify();
        }
    }

    pub(crate) fn cancel_review_clear(&mut self, cx: &mut Context<Self>) {
        self.review.clear_confirmation = false;
        cx.notify();
    }

    pub(crate) fn clear_review_exports(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.review.clearing {
            return;
        }
        self.review.clear_confirmation = false;
        let Some((checkout_id, _)) = self.open_diff.as_ref() else {
            cx.notify();
            return;
        };
        let checkout_id = checkout_id.clone();
        let Some(checkout) = self.navigation.checkout(&checkout_id).cloned() else {
            self.push_error("The checkout for this review is no longer available.", cx);
            cx.notify();
            return;
        };
        let Some(repo) = self
            .navigation
            .repos
            .iter()
            .find(|repo| repo.id == checkout.repo_id)
            .cloned()
        else {
            self.push_error("The repository for this review is no longer available.", cx);
            cx.notify();
            return;
        };
        let storage = self.navigation.review_storage;
        self.review.clearing = true;
        cx.notify();
        let worker = cx.background_executor().spawn(async move {
            muster_core::files::clear_review_exports(&repo, &checkout, storage)
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update(cx, |app, cx| {
                app.review.clearing = false;
                match result {
                    Ok(count) => app.push_info(format!("Cleared {count} review export(s)."), cx),
                    Err(error) => app.push_error(error, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn review_clear_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.review.clear_confirmation {
            return None;
        }
        let roles = Roles::of(cx);
        let cancel = cx.weak_entity();
        let cancel_for_escape = cx.weak_entity();
        let clear = cx.weak_entity();
        Some(
            Dialog::new(cx)
                .bg(roles.surface_raised)
                .border_color(roles.border_strong)
                .width(px(440.))
                .title("Clear review exports?")
                .content(|content: DialogContent, _, _| {
                    content.child(
                        div().text_size(px(12.)).child(
                            "This deletes only timestamped Markdown exports with Muster's ownership marker in ~/.muster/tmp/reviews. Exports inside a checkout are never cleared here.",
                        ),
                    )
                })
                .footer(
                    DialogFooter::new()
                        .child(
                            zed_button("cancel-clear-review-exports")
                                .label("Cancel")
                                .on_click(move |_, _, cx| {
                                    let _ = cancel.update(cx, |app, cx| {
                                        app.cancel_review_clear(cx);
                                    });
                                }),
                        )
                        .child(
                            zed_button("confirm-clear-review-exports")
                                .danger()
                                .label("Delete exports")
                                .on_click(move |_, window, cx| {
                                    let _ = clear.update(cx, |app, cx| {
                                        app.clear_review_exports(window, cx);
                                    });
                                }),
                        ),
                )
                .on_cancel(move |_, _, cx| {
                    let _ = cancel_for_escape.update(cx, |app, cx| {
                        app.cancel_review_clear(cx);
                    });
                    false
                })
                .into_any_element(),
        )
    }

    pub(crate) fn set_review_storage(&mut self, storage: ReviewStorage, cx: &mut Context<Self>) {
        if self.navigation.review_storage == storage {
            return;
        }
        self.navigation.review_storage = storage;
        self.persist();
        cx.notify();
    }

    pub(crate) fn set_opencode_destination(
        &mut self,
        destination: muster_core::opencode::Destination,
        cx: &mut Context<Self>,
    ) {
        if self.review.opencode_busy || self.review.opencode_loading || self.review.exporting {
            return;
        }
        let Some((checkout_id, _)) = self.open_diff.as_ref() else {
            return;
        };
        let checkout_id = checkout_id.clone();
        self.review.opencode_busy = true;
        self.review.opencode_error = None;
        cx.notify();
        let Some(home) = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .filter(|path| path.is_absolute())
        else {
            self.review.opencode_busy = false;
            self.review.opencode_error =
                Some("HOME is unavailable for OpenCode review storage.".into());
            cx.notify();
            return;
        };
        let stored = destination.clone();
        let worker_checkout = checkout_id.clone();
        let worker = cx.background_executor().spawn(async move {
            muster_core::opencode::RoundStore::open(&home)?
                .set_destination(&worker_checkout, stored)
        });
        cx.spawn(async move |view, cx| {
            let result = worker.await;
            let _ = view.update(cx, |app, cx| {
                app.review.opencode_busy = false;
                cx.notify();
                if app.open_diff.as_ref().map(|(id, _)| id.as_str()) != Some(checkout_id.as_str()) {
                    if let Some((active, _)) = app.open_diff.clone() {
                        app.restore_opencode_review(&active, cx);
                    }
                    return;
                }
                app.review.opencode_generation = app.review.opencode_generation.wrapping_add(1);
                app.review.opencode_loading = false;
                match result {
                    Ok(()) => {
                        app.review.opencode_destination = Some(destination);
                        app.review.opencode_error = None;
                    }
                    Err(error) => app.review.opencode_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn toggle_include_outdated(&mut self, cx: &mut Context<Self>) {
        self.review.include_outdated = !self.review.include_outdated;
        cx.notify();
    }

    pub(crate) fn restore_opencode_review(&mut self, checkout: &str, cx: &mut Context<Self>) {
        let Some(home) = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute())
        else {
            self.review.opencode_error =
                Some("HOME is unavailable for OpenCode review storage.".into());
            return;
        };
        let checkout_id = checkout.to_owned();
        if self.review.opencode_checkout.as_deref() != Some(checkout) {
            self.review.opencode_generation = self.review.opencode_generation.wrapping_add(1);
            self.review.opencode_checkout = Some(checkout_id.clone());
            self.review.opencode_rounds.clear();
            self.review.opencode_loading = false;
            self.review.opencode_destination = None;
            self.review.opencode_error = None;
        }
        if self.review.opencode_loading {
            return;
        }
        self.review.opencode_loading = true;
        let generation = self.review.opencode_generation;
        let worker_checkout_id = checkout_id.clone();
        let worker = cx.background_executor().spawn(async move {
            let store = muster_core::opencode::RoundStore::open(&home)?;
            Ok::<_, muster_core::opencode::DispatchError>((
                store.destination(&worker_checkout_id)?,
                store.rounds(&worker_checkout_id)?,
            ))
        });
        cx.spawn(async move |view, cx| {
            let result = worker.await;
            let _ = view.update(cx, |app, cx| {
                if app.review.opencode_generation != generation
                    || app.review.opencode_checkout.as_deref() != Some(checkout_id.as_str())
                {
                    return;
                }
                app.review.opencode_loading = false;
                if app.open_diff.as_ref().map(|(id, _)| id.as_str()) != Some(checkout_id.as_str()) {
                    return;
                }
                match result {
                    Ok((destination, rounds)) => {
                        app.review.opencode_destination = destination;
                        // Restore immutable round snapshots without overwriting live edits
                        // or colliding with a draft id created since this read began.
                        let notes = app.review.notes.entry(checkout_id.clone()).or_default();
                        for note in rounds.iter().rev().flat_map(|round| round.notes.iter()) {
                            if !notes.iter().any(|existing| existing.id == note.id) {
                                notes.push(note.clone());
                            }
                        }
                        app.review.opencode_rounds = rounds;
                    }
                    Err(error) => app.review.opencode_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn send_review_to_opencode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.review.opencode_busy
            || self.review.opencode_loading
            || self.review.exporting
            || !can_start_opencode_round(&self.review.opencode_rounds)
        {
            return;
        }
        let Some((checkout_id, _)) = self.open_diff.as_ref() else {
            return;
        };
        let checkout_id = checkout_id.clone();
        let Some(checkout) = self.navigation.checkout(&checkout_id).cloned() else {
            self.review.opencode_error =
                Some("The checkout for this review is no longer available.".into());
            cx.notify();
            return;
        };
        let Some(repo) = self
            .navigation
            .repos
            .iter()
            .find(|repo| repo.id == checkout.repo_id)
            .cloned()
        else {
            self.review.opencode_error =
                Some("The repository for this review is no longer available.".into());
            cx.notify();
            return;
        };
        // A restored round's notes are in the window's list so they can be shown again;
        // what a new send carries is what no round has already taken, unchanged.
        let notes = opencode_send_notes(&self.review, &checkout_id)
            .into_iter()
            .filter(|note| self.review.include_outdated || !note.outdated)
            .collect::<Vec<_>>();
        if notes.is_empty() {
            self.review.opencode_error =
                Some("There are no review notes to send: every note here was already sent.".into());
            cx.notify();
            return;
        }
        let Some(home) = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute())
        else {
            self.review.opencode_error =
                Some("HOME is unavailable for OpenCode review storage.".into());
            cx.notify();
            return;
        };
        let (date, _) = match review::local_review_timestamp() {
            Ok(value) => value,
            Err(error) => {
                self.review.opencode_error = Some(error);
                cx.notify();
                return;
            }
        };
        let markdown = review::build_review_markdown(
            &notes,
            &ReviewContext {
                branch: checkout.branch.as_deref(),
                default_branch: repo.default_branch.as_deref(),
                date: &date,
                language: crate::document::syntax_language,
            },
        );
        let title = format!(
            "Muster review · {}",
            checkout.branch.as_deref().unwrap_or(&checkout.id)
        );
        self.review.opencode_busy = true;
        self.review.opencode_error = None;
        cx.notify();
        let worker = cx.background_executor().spawn(async move {
            use muster_core::opencode::{rounds::RoundRequest, Client, Destination, RoundStore};
            let store = RoundStore::open(&home)?;
            let client = Client::connect(&home, std::path::Path::new(&checkout.path))?;
            let destination = match store.destination(&checkout.id)? {
                Some(destination @ Destination::Opencode { .. }) => destination,
                Some(Destination::OpencodeNew) => {
                    let session_id = client.create_session(&title)?;
                    let destination = Destination::Opencode { session_id };
                    store.set_destination(&checkout.id, destination.clone())?;
                    destination
                }
                None | Some(Destination::Markdown) => {
                    return Err(muster_core::opencode::DispatchError::Failed(
                        "OpenCode is not the selected review destination.".into(),
                    ));
                }
            };
            let session_id = match &destination {
                Destination::Opencode { session_id } => session_id.clone(),
                Destination::OpencodeNew | Destination::Markdown => {
                    unreachable!("resolved destination")
                }
            };
            // The claim, the prompt and the send are one step in the store, so a second
            // window cannot put these notes on the wire as another round.
            let sent = store.send_round(
                &client,
                RoundRequest {
                    checkout: &checkout,
                    session_id: &session_id,
                    notes: &notes,
                    review_markdown: &markdown,
                },
            );
            // The round is on disk either way, so the window is told what it is even when
            // the answer was lost.
            Ok::<_, muster_core::opencode::DispatchError>((
                destination,
                store.rounds(&checkout.id)?,
                sent.map(|_| ()).err().map(|error| error.to_string()),
            ))
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update(cx, |app, cx| {
                app.review.opencode_busy = false;
                cx.notify();
                if app.open_diff.as_ref().map(|(id, _)| id.as_str()) != Some(checkout_id.as_str()) {
                    if let Some((active, _)) = app.open_diff.clone() {
                        app.restore_opencode_review(&active, cx);
                    }
                    return;
                }
                // This result supersedes any restore started while the request ran.
                app.review.opencode_generation = app.review.opencode_generation.wrapping_add(1);
                app.review.opencode_loading = false;
                match result {
                    Ok((destination, rounds, dispatch_error)) => {
                        app.review.opencode_destination = Some(destination);
                        app.review.opencode_rounds = rounds;
                        app.review.opencode_error = dispatch_error;
                    }
                    Err(error) => app.review.opencode_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn retry_queued_opencode_rounds(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.review.opencode_busy || self.review.opencode_loading {
            return;
        }
        let Some((checkout_id, _)) = self.open_diff.as_ref() else {
            return;
        };
        let checkout_id = checkout_id.clone();
        let Some(checkout) = self.navigation.checkout(&checkout_id).cloned() else {
            return;
        };
        let Some(home) = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute())
        else {
            return;
        };
        self.review.opencode_busy = true;
        let worker = cx.background_executor().spawn(async move {
            let store = muster_core::opencode::RoundStore::open(&home)?;
            let client = muster_core::opencode::Client::connect(
                &home,
                std::path::Path::new(&checkout.path),
            )?;
            let flush = store.flush_rounds(&client, &checkout.id);
            let rounds = store.rounds(&checkout.id)?;
            Ok::<_, muster_core::opencode::DispatchError>((flush, rounds))
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update(cx, |app, cx| {
                app.review.opencode_busy = false;
                cx.notify();
                if app.open_diff.as_ref().map(|(id, _)| id.as_str()) != Some(checkout_id.as_str()) {
                    if let Some((active, _)) = app.open_diff.clone() {
                        app.restore_opencode_review(&active, cx);
                    }
                    return;
                }
                // This result supersedes any restore started while the request ran.
                app.review.opencode_generation = app.review.opencode_generation.wrapping_add(1);
                app.review.opencode_loading = false;
                match result {
                    Ok((flush, rounds)) => {
                        app.review.opencode_rounds = rounds;
                        app.review.opencode_error = flush.err().map(|error| error.to_string());
                    }
                    Err(error) => app.review.opencode_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn reconcile_opencode_round(
        &mut self,
        round_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.review.opencode_busy {
            return;
        }
        let Some((checkout_id, _)) = self.open_diff.as_ref() else {
            return;
        };
        let checkout_id = checkout_id.clone();
        let Some(checkout) = self.navigation.checkout(&checkout_id).cloned() else {
            return;
        };
        let Some(home) = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute())
        else {
            return;
        };
        let round_id = round_id.to_owned();
        self.review.opencode_busy = true;
        cx.notify();
        let worker = cx.background_executor().spawn(async move {
            let store = muster_core::opencode::RoundStore::open(&home)?;
            let client = muster_core::opencode::Client::connect(
                &home,
                std::path::Path::new(&checkout.path),
            )?;
            store.reconcile_round(&client, &checkout.id, &round_id)?;
            store.rounds(&checkout.id)
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update(cx, |app, cx| {
                app.review.opencode_busy = false;
                cx.notify();
                if app.open_diff.as_ref().map(|(id, _)| id.as_str()) != Some(checkout_id.as_str()) {
                    if let Some((active, _)) = app.open_diff.clone() {
                        app.restore_opencode_review(&active, cx);
                    }
                    return;
                }
                // This result supersedes any restore started while the request ran.
                app.review.opencode_generation = app.review.opencode_generation.wrapping_add(1);
                app.review.opencode_loading = false;
                match result {
                    Ok(rounds) => {
                        app.review.opencode_rounds = rounds;
                        app.review.opencode_error = None;
                    }
                    Err(error) => app.review.opencode_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn resend_unknown_opencode_round(
        &mut self,
        round_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.review.opencode_busy {
            return;
        }
        let Some((checkout_id, _)) = self.open_diff.as_ref() else {
            return;
        };
        let checkout_id = checkout_id.clone();
        let Some(checkout) = self.navigation.checkout(&checkout_id).cloned() else {
            return;
        };
        let Some(home) = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute())
        else {
            return;
        };
        let round_id = round_id.to_owned();
        self.review.opencode_busy = true;
        cx.notify();
        let worker = cx.background_executor().spawn(async move {
            let store = muster_core::opencode::RoundStore::open(&home)?;
            store.resend_round(&checkout.id, &round_id)?;
            store.rounds(&checkout.id)
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update(cx, |app, cx| {
                app.review.opencode_busy = false;
                cx.notify();
                if app.open_diff.as_ref().map(|(id, _)| id.as_str()) != Some(checkout_id.as_str()) {
                    if let Some((active, _)) = app.open_diff.clone() {
                        app.restore_opencode_review(&active, cx);
                    }
                    return;
                }
                // This result supersedes any restore started while the request ran.
                app.review.opencode_generation = app.review.opencode_generation.wrapping_add(1);
                app.review.opencode_loading = false;
                match result {
                    Ok(rounds) => {
                        app.review.opencode_rounds = rounds;
                        app.review.opencode_error = None;
                    }
                    Err(error) => app.review.opencode_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn export_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.review.exporting || self.review.opencode_busy || self.review.opencode_loading {
            return;
        }
        let Some((checkout_id, _)) = self.open_diff.as_ref() else {
            return;
        };
        let checkout_id = checkout_id.clone();
        let Some(checkout) = self.navigation.checkout(&checkout_id).cloned() else {
            self.push_error("The checkout for this review is no longer available.", cx);
            return;
        };
        let Some(repo) = self
            .navigation
            .repos
            .iter()
            .find(|repo| repo.id == checkout.repo_id)
            .cloned()
        else {
            self.push_error("The repository for this review is no longer available.", cx);
            return;
        };
        let (date, timestamp) = match review::local_review_timestamp() {
            Ok(time) => time,
            Err(error) => {
                self.push_error(error, cx);
                return;
            }
        };
        let notes = self
            .review
            .notes
            .get(&checkout_id)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter(|note| self.review.include_outdated || !note.outdated)
            .cloned()
            .collect::<Vec<_>>();
        if notes.is_empty() {
            self.push_error("There are no review notes to export.", cx);
            return;
        }
        let branch = checkout.branch.clone();
        let default_branch = repo.default_branch.clone();
        let storage = self.navigation.review_storage;
        self.review.exporting = true;
        cx.notify();
        let worker = cx.background_executor().spawn(async move {
            let home = std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .ok_or_else(|| "HOME is unavailable for review preferences.".to_owned())?;
            let store = muster_core::opencode::RoundStore::open(&home)
                .map_err(|error| error.to_string())?;
            store
                .set_destination(&checkout.id, muster_core::opencode::Destination::Markdown)
                .map_err(|error| error.to_string())?;
            let markdown = review::build_review_markdown(
                &notes,
                &ReviewContext {
                    branch: branch.as_deref(),
                    default_branch: default_branch.as_deref(),
                    date: &date,
                    language: crate::document::syntax_language,
                },
            );
            muster_core::files::export_review_markdown(
                &repo, &checkout, storage, &timestamp, &markdown,
            )
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update(cx, |app, cx| {
                app.review.exporting = false;
                match result {
                    Ok(path) => {
                        if app.open_diff.as_ref().map(|(id, _)| id.as_str())
                            == Some(checkout_id.as_str())
                        {
                            app.review.opencode_destination =
                                Some(muster_core::opencode::Destination::Markdown);
                        }
                        app.push_info(format!("Review exported to {}", path.display()), cx);
                    }
                    Err(error) => app.push_error(error, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composer_start_choices_stay_on_the_selected_side_and_inside_the_range() {
        let lines = BTreeMap::from([
            (
                (ReviewSide::Old, 3),
                muster_core::review::DiffLine {
                    text: "old 3".into(),
                    mark: '-',
                },
            ),
            (
                (ReviewSide::Old, 5),
                muster_core::review::DiffLine {
                    text: "old 5".into(),
                    mark: '-',
                },
            ),
            (
                (ReviewSide::New, 4),
                muster_core::review::DiffLine {
                    text: "new 4".into(),
                    mark: '+',
                },
            ),
            (
                (ReviewSide::New, 7),
                muster_core::review::DiffLine {
                    text: "new 7".into(),
                    mark: '+',
                },
            ),
        ]);

        assert_eq!(review_start_lines(&lines, ReviewSide::Old, 5), vec![5]);
        assert_eq!(review_start_lines(&lines, ReviewSide::New, 4), vec![4]);
    }

    #[test]
    fn unresolved_rounds_block_new_review_dispatch_until_explicit_resolution() {
        use muster_core::opencode::{ReviewRound, RoundStatus};
        let round = |status| ReviewRound {
            id: "round-1".into(),
            checkout_id: "checkout-1".into(),
            session_id: "ses_app".into(),
            status,
            marker: "muster-review:round-1".into(),
            prompt: "review".into(),
            note_ids: Vec::new(),
            claim: String::new(),
            notes: Vec::new(),
            created_at: String::new(),
            updated_at: String::new(),
        };
        assert!(can_start_opencode_round(&[]));
        assert!(!can_start_opencode_round(&[round(
            RoundStatus::Dispatching
        )]));
        assert!(!can_start_opencode_round(&[round(RoundStatus::Queued)]));
        assert!(can_start_opencode_round(&[round(RoundStatus::Unknown)]));
        assert!(can_start_opencode_round(&[round(RoundStatus::Dispatched)]));
        assert!(can_start_opencode_round(&[round(RoundStatus::Acked)]));
    }

    #[test]
    fn anchor_hashes_are_cached_by_diff_revision() {
        let note = ReviewNote {
            id: "note-1".into(),
            checkout_id: "checkout-1".into(),
            path: "file.txt".into(),
            side: ReviewSide::Old,
            line_start: 1,
            line_end: None,
            content: "Review this".into(),
            code: "original".into(),
            code_hash: review_anchor_hash("original"),
            outdated: false,
        };
        let notes = [note];
        let mut state = ReviewState::default();
        let first = cached_anchor_hashes(
            &mut state,
            "checkout-1",
            "file.txt",
            "revision-1",
            "@@ -1 +1 @@\n-original\n+new\n",
            &notes,
        );
        assert_eq!(
            first[&(ReviewSide::Old, 1)],
            Some(review_anchor_hash("original"))
        );

        let same_revision = cached_anchor_hashes(
            &mut state,
            "checkout-1",
            "file.txt",
            "revision-1",
            "@@ -1 +1 @@\n-replaced\n+new\n",
            &notes,
        );
        assert_eq!(same_revision, first);

        let new_revision = cached_anchor_hashes(
            &mut state,
            "checkout-1",
            "file.txt",
            "revision-2",
            "@@ -1 +1 @@\n-replaced\n+new\n",
            &notes,
        );
        assert_eq!(
            new_revision[&(ReviewSide::Old, 1)],
            Some(review_anchor_hash("replaced"))
        );
    }

    fn note(id: &str, content: &str) -> ReviewNote {
        ReviewNote {
            id: id.into(),
            checkout_id: "checkout-1".into(),
            path: "file.txt".into(),
            side: ReviewSide::New,
            line_start: 4,
            line_end: None,
            content: content.into(),
            code: "let a = 1;".into(),
            code_hash: review_anchor_hash("let a = 1;"),
            outdated: false,
        }
    }

    fn sent_round(id: &str, notes: Vec<ReviewNote>) -> muster_core::opencode::ReviewRound {
        muster_core::opencode::ReviewRound {
            id: id.into(),
            checkout_id: "checkout-1".into(),
            session_id: "ses_review".into(),
            status: muster_core::opencode::RoundStatus::Dispatched,
            marker: format!("muster-review:{id}"),
            prompt: "# Code Review".into(),
            note_ids: notes.iter().map(|note| note.id.clone()).collect(),
            claim: "claimed".into(),
            notes,
            created_at: "1".into(),
            updated_at: "1".into(),
        }
    }

    #[test]
    fn reopening_a_checkout_does_not_put_a_sent_review_back_on_the_wire() {
        let sent = note("note-1", "check this");
        let unsent = note("note-2", "and this");
        let mut state = ReviewState::default();
        // What the restore leaves behind: the window's notes plus the round that took one.
        state
            .notes
            .insert("checkout-1".into(), vec![sent.clone(), unsent.clone()]);
        state.opencode_rounds = vec![sent_round("r1", vec![sent.clone()])];

        let offered = opencode_send_notes(&state, "checkout-1");

        assert_eq!(
            offered,
            vec![unsent],
            "only the note no round has taken is offered again"
        );
    }

    #[test]
    fn a_note_edited_after_it_was_sent_is_offered_again() {
        let sent = note("note-1", "check this");
        let mut edited = sent.clone();
        edited.content = "check this too".into();
        let mut state = ReviewState::default();
        state
            .notes
            .insert("checkout-1".into(), vec![edited.clone()]);
        state.opencode_rounds = vec![sent_round("r1", vec![sent])];

        assert_eq!(opencode_send_notes(&state, "checkout-1"), vec![edited]);
    }

    #[test]
    fn a_round_that_may_or_may_not_have_landed_says_so_rather_than_reading_as_a_send() {
        use muster_core::opencode::RoundStatus;

        assert_eq!(opencode_round_state(RoundStatus::Dispatching), "sending");
        let blocked = opencode_round_state(RoundStatus::Unknown);
        assert!(blocked.contains("will not send it again"), "{blocked}");
        assert_ne!(blocked, opencode_round_state(RoundStatus::Dispatched));
        assert_ne!(blocked, "sending");
    }
}
