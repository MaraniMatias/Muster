//! The window's line-anchored review drafts and their Markdown export.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::app::TerminalWindow;
use crate::theme::Roles;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{Dialog, DialogContent, DialogFooter};
use gpui_kit::component::diff::{DiffLineRange, DiffSide};
use gpui_kit::component::input::{InputState, TextareaState};
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

/// The range a note covers, in the coordinates the diff's own selection speaks.
///
/// A note names a side and the lines on it, and the diff washes a range in
/// those same terms, so the two are one thing said twice.
fn review_note_range(
    path: &str,
    side: ReviewSide,
    line_start: usize,
    line_end: usize,
) -> DiffLineRange {
    DiffLineRange::new(
        path.to_owned(),
        match side {
            ReviewSide::Old => DiffSide::Original,
            ReviewSide::New => DiffSide::Modified,
        },
        line_start,
        line_end,
    )
}

/// Candidate first lines for a note range: every line from the top of its own hunk down to the
/// line it ends on, on either side.
///
/// A change that removed the lines above an added one is part of the same range, so a note that
/// ends on a `+` may still start on a `−`. An untouched line is one line of both sides and is
/// named on the side the note is anchored to, which is what keeps the same text from being offered
/// twice under two numbers.
pub(crate) fn review_start_lines(
    lines: &BTreeMap<(ReviewSide, usize), muster_core::review::DiffLine>,
    anchor: (ReviewSide, usize),
) -> Vec<(ReviewSide, usize)> {
    if anchor.1 == 0 || !lines.contains_key(&anchor) {
        return Vec::new();
    }
    let drawn = drawn_line(lines, anchor);
    let Some(run) = review_drawn_lines(lines)
        .into_iter()
        .find(|run| run.contains(&drawn))
    else {
        return Vec::new();
    };
    let Some(index) = run.iter().position(|line| *line == drawn) else {
        return Vec::new();
    };
    let mut starts = Vec::new();
    let mut bytes = 0;
    for (at, line) in run[..=index].iter().enumerate().rev() {
        let side = match lines[line].mark {
            '-' => ReviewSide::Old,
            '+' => ReviewSide::New,
            _ => anchor.0,
        };
        bytes += lines[line].text.len() + usize::from(at != index);
        if bytes > MAX_NOTE_CODE_BYTES {
            break;
        }
        starts.push((side, line.1));
    }
    starts.reverse();
    starts
}

/// The line of the drawing a line of the diff belongs to: an untouched line is one line of both
/// sides, so it is drawn once, on the new side's numbering.
fn drawn_line(
    lines: &BTreeMap<(ReviewSide, usize), muster_core::review::DiffLine>,
    line: (ReviewSide, usize),
) -> (ReviewSide, usize) {
    match lines[&line].mark {
        ' ' => (ReviewSide::New, line.1),
        _ => line,
    }
}

/// The lines a diff draws, in order, one run per hunk.
///
/// The old and the new side are two numbered copies of the same hunk, so the drawing is their
/// merge: a removed line is only in the old one, an added line only in the new one, and an
/// untouched line is in both. A removal is written before the addition that replaced it, and a gap
/// in either side's numbering is where one hunk ends and the next begins.
fn review_drawn_lines(
    lines: &BTreeMap<(ReviewSide, usize), muster_core::review::DiffLine>,
) -> Vec<Vec<(ReviewSide, usize)>> {
    let next_of = |side: ReviewSide, after: usize| {
        lines
            .range((side, after + 1)..)
            .next()
            .map(|((_, line), _)| *line)
    };
    let mut runs = Vec::new();
    let (mut old, mut new) = (0usize, 0usize);
    while let (Some(next_old), Some(next_new)) =
        (next_of(ReviewSide::Old, old), next_of(ReviewSide::New, new))
    {
        old = next_old - 1;
        new = next_new - 1;
        let mut run = Vec::new();
        loop {
            let old_mark = lines.get(&(ReviewSide::Old, old + 1)).map(|line| line.mark);
            let new_mark = lines.get(&(ReviewSide::New, new + 1)).map(|line| line.mark);
            if old_mark == Some('-') {
                old += 1;
                run.push((ReviewSide::Old, old));
            } else if new_mark == Some('+') {
                new += 1;
                run.push((ReviewSide::New, new));
            } else if old_mark == Some(' ') && new_mark == Some(' ') {
                old += 1;
                new += 1;
                run.push((ReviewSide::New, new));
            } else if old_mark.is_some() {
                old += 1;
                run.push((ReviewSide::Old, old));
            } else if new_mark.is_some() {
                new += 1;
                run.push((ReviewSide::New, new));
            } else {
                break;
            }
        }
        runs.push(run);
    }
    runs
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
    /// Exact snapshots finished in this window. Pending round history must not
    /// restore them when a diff reopens; the queue itself remains unchanged.
    completed_notes: HashMap<String, HashSet<String>>,
    pub(crate) anchor_hashes: AnchorHashCache,
    pub(crate) composer: Option<ReviewComposer>,
    /// The note whose lines are washed on the diff.
    ///
    /// The mark belongs to a note rather than to a draft: a note that has been
    /// applied says which lines it is about, and that stays on the diff until
    /// another note takes the mark over or that note is deleted. A draft borrows
    /// it while it is being written.
    pub(crate) marked: Option<String>,
    pub(crate) include_outdated: bool,
    /// The note a person asked to delete, with the checkout it belongs to, until
    /// they say whether they meant to.
    pub(crate) delete_confirmation: Option<(String, String)>,
    pub(crate) clear_confirmation: bool,
    pub(crate) clearing: bool,
    pub(crate) forget_confirmation: bool,
    pub(crate) forgetting: bool,
    pub(crate) exporting: bool,
    /// Where the notes were last saved, so the toolbar can keep naming the file
    /// instead of a toast saying it once and losing it.
    pub(crate) exported: Option<ExportedReview>,
    pub(crate) destination_busy: bool,
    pub(crate) opencode_loading: bool,
    pub(crate) destination: Option<muster_core::opencode::Destination>,
    pub(crate) opencode_rounds: Vec<muster_core::opencode::ReviewRound>,
    /// The sessions a review of this checkout can go to, newest first.
    ///
    /// A read of what the service lists, not a list kept in step with it: the service
    /// is the only thing that knows which sessions exist.
    pub(crate) opencode_sessions: Vec<muster_core::opencode::AgentSession>,
    /// What the search over those sessions said.
    pub(crate) opencode_query: String,
    /// The search field, when there are enough sessions to need one.
    pub(crate) opencode_search: Option<Entity<InputState>>,
    /// The subscription that carries what the search said.
    pub(crate) opencode_search_subscription: Option<gpui_kit::Subscription>,
    /// The review that is waiting for an answer about a busy session.
    pub(crate) opencode_choice: Option<OpenCodeReview>,
    pub(crate) destination_error: Option<String>,
    opencode_checkout: Option<String>,
    opencode_generation: u64,
}

/// The file a save wrote, with the checkout whose notes went into it.
///
/// The checkout is part of it because the file outlives the diff: a review saved
/// for one checkout is still on disk after another checkout's change set is opened,
/// and only the first of the two is what that file holds.
pub(crate) struct ExportedReview {
    pub(crate) checkout_id: String,
    pub(crate) path: PathBuf,
}

/// A review that is ready to go, and the choice a busy target raises.
///
/// The review is prepared before the choice is asked, so "Send now" and "Queue" are
/// the same message either way and neither has to rebuild it.
pub(crate) struct OpenCodeReview {
    /// The checkout the review is about.
    pub(crate) checkout: muster_core::workspace::Checkout,
    /// The session it would go to, when one was chosen.
    pub(crate) session_id: Option<String>,
    /// The notes it carries.
    pub(crate) notes: Vec<ReviewNote>,
    /// The review as it would be sent.
    pub(crate) markdown: String,
    /// The title a session created for this review carries.
    pub(crate) title: String,
}

/// What a person chose about a review whose target is busy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpenCodeDispatch {
    /// Read fresh activity and ask before interrupting a running turn.
    IfReady,
    /// Send it into the turn that is running.
    Now,
    /// Wait for the turn that is running to finish.
    Queue,
}

pub(crate) fn can_start_opencode_round(rounds: &[muster_core::opencode::ReviewRound]) -> bool {
    !rounds.iter().any(|round| round.status.is_pending())
}

impl ReviewState {
    /// Finish only the snapshots that actually left the window, not edits made
    /// while the export or send was in flight. Returns whether their mark went too.
    fn complete_notes(&mut self, checkout: &str, completed: &HashSet<String>) -> bool {
        if completed.is_empty() {
            return false;
        }
        self.completed_notes
            .entry(checkout.to_owned())
            .or_default()
            .extend(completed.iter().cloned());
        if self.opencode_loading && self.opencode_checkout.as_deref() == Some(checkout) {
            // A read started before this completion cannot describe pending work.
            self.opencode_generation = self.opencode_generation.wrapping_add(1);
            self.opencode_loading = false;
        }
        let mut removed = HashSet::new();
        if let Some(notes) = self.notes.get_mut(checkout) {
            notes.retain(|note| {
                if !completed.contains(&review::review_note_key(note)) {
                    return true;
                }
                removed.insert(note.id.clone());
                self.anchor_hashes
                    .remove(&(checkout.to_owned(), note.path.clone()));
                false
            });
        }
        // An editor opened before delivery still holds somebody's writing. Its
        // next Save creates a new pending note rather than editing a removed one.
        if let Some(composer) = self.composer.as_mut().filter(|composer| {
            composer.checkout_id == checkout
                && composer
                    .editing_id
                    .as_ref()
                    .is_some_and(|id| removed.contains(id))
        }) {
            composer.editing_id = None;
        }
        let clear_mark = self.marked.as_ref().is_some_and(|id| removed.contains(id));
        if clear_mark {
            self.marked = None;
        }
        clear_mark
    }

    /// Only unresolved rounds return to the working review. Delivered snapshots
    /// stay in their immutable round history, never in the diff's pending notes.
    fn restore_pending_notes(
        &mut self,
        checkout: &str,
        rounds: &[muster_core::opencode::ReviewRound],
    ) {
        let delivered: HashSet<_> = rounds
            .iter()
            .filter(|round| round.status.is_settled())
            .flat_map(|round| round.claimed_keys())
            .collect();
        let completed = self.completed_notes.get(checkout);
        let notes = self.notes.entry(checkout.to_owned()).or_default();
        for note in rounds
            .iter()
            .rev()
            .filter(|round| !round.status.is_settled())
            .flat_map(|round| &round.notes)
        {
            let key = review::review_note_key(note);
            if !delivered.contains(&key)
                && !completed.is_some_and(|completed| completed.contains(&key))
                && !notes.iter().any(|existing| existing.id == note.id)
            {
                notes.push(note.clone());
            }
        }
    }

    /// Finish only the read that still owns the loading state. An older callback
    /// must neither restore notes nor clear a newer read's loading flag.
    fn finish_opencode_restore(&mut self, checkout: &str, generation: u64) -> bool {
        if self.opencode_generation != generation
            || self.opencode_checkout.as_deref() != Some(checkout)
        {
            return false;
        }
        self.opencode_loading = false;
        true
    }

    /// The notes `checkout` holds, and of `path` within it when one is named,
    /// counted the way the diff counts them: an outdated note is one the person
    /// has said is not part of this review.
    ///
    /// A whole change set asks for every note in its checkout, because that is
    /// what it draws and what a save sends.
    pub(crate) fn notes_for(&self, checkout: &str, path: Option<&str>) -> usize {
        self.notes.get(checkout).map_or(0, |notes| {
            notes
                .iter()
                .filter(|note| {
                    (self.include_outdated || !note.outdated)
                        && path.is_none_or(|path| note.path == path)
                })
                .count()
        })
    }

    /// The paths in `checkout` that hold a note, with how many of them count,
    /// in the order the paths were first written on.
    ///
    /// The panel's rows are this and nothing else, so the list cannot claim a
    /// file it has no note on, and cannot forget one it does.
    pub(crate) fn noted_paths(&self, checkout: &str) -> Vec<(&str, usize)> {
        let mut paths: Vec<(&str, usize)> = Vec::new();
        for note in self
            .notes
            .get(checkout)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter(|note| self.include_outdated || !note.outdated)
        {
            match paths
                .iter_mut()
                .find(|(path, _)| *path == note.path.as_str())
            {
                Some(counted) => counted.1 += 1,
                None => paths.push((note.path.as_str(), 1)),
            }
        }
        paths
    }
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
    /// The line the note was opened on, which is where the composer card hangs
    /// and where the picker counts back from. Choosing a start on the other side
    /// of the change moves the range, never the card.
    pub(crate) anchor: (ReviewSide, usize),
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

/// Picker labels keep the old side negative and mark added lines on the new side.
fn review_start_label(line: usize, side: ReviewSide, mark: char) -> SharedString {
    match side {
        ReviewSide::Old => format!("-{line}").into(),
        ReviewSide::New if mark == '+' => format!("+{line}").into(),
        ReviewSide::New => line.to_string().into(),
    }
}

/// The side and line a picker item stands for, without its change marker.
fn review_line_label(label: &str) -> Option<(ReviewSide, usize)> {
    if let Some(line) = label.strip_prefix('-') {
        return Some((ReviewSide::Old, line.parse().ok()?));
    }
    Some((
        ReviewSide::New,
        label.strip_prefix('+').unwrap_or(label).parse().ok()?,
    ))
}

/// The lines a note range could start on, and the one this note starts at.
///
/// The delegate is the toolkit's own `Vec<SharedString>`, so a candidate is a
/// line number with its change marker, without a code preview.
fn review_start_choices(
    lines: &BTreeMap<(ReviewSide, usize), muster_core::review::DiffLine>,
    start: (ReviewSide, usize),
    anchor: (ReviewSide, usize),
) -> (Vec<SharedString>, Option<IndexPath>) {
    let starts = review_start_lines(lines, anchor);
    (
        starts
            .iter()
            .map(|(side, line)| review_start_label(*line, *side, lines[&(*side, *line)].mark))
            .collect(),
        starts
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
    start: (ReviewSide, usize),
    anchor: (ReviewSide, usize),
    window: &mut Window,
    cx: &mut Context<TerminalWindow>,
) -> (
    Entity<SelectState<Vec<SharedString>>>,
    gpui_kit::Subscription,
) {
    let (items, selected) = review_start_choices(lines, start, anchor);
    let picker = cx.new(|cx| SelectState::new(items, selected, window, cx));
    let events = cx.subscribe_in(&picker, window, |app, _, event, window, cx| {
        let SelectEvent::Confirm(Some(line)) = event else {
            return;
        };
        if let Some((side, line)) = review_line_label(line) {
            app.set_review_note_start(side, line, window, cx);
        }
    });
    (picker, events)
}

/// The keystroke that finishes a note, registered for as long as the composer is open.
///
/// An interceptor rather than a key handler on the card because it runs before the
/// input's own Return: stopping the platform-modifier+Enter here is what keeps it
/// from also inserting a newline. Enter on its own still writes the newline.
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
    fn complete_review_notes(
        &mut self,
        checkout: &str,
        completed: &HashSet<String>,
        cx: &mut Context<Self>,
    ) {
        let was_loading = self.review.opencode_loading
            && self.review.opencode_checkout.as_deref() == Some(checkout);
        if self.review.complete_notes(checkout, completed)
            && self
                .open_diff
                .as_ref()
                .is_some_and(|(owner, _)| owner == checkout)
        {
            self.sync_review_note_selection(cx);
        }
        if was_loading
            && !self.review.opencode_loading
            && self
                .open_diff
                .as_ref()
                .is_some_and(|(owner, _)| owner == checkout)
        {
            self.restore_opencode_review(checkout, cx);
        }
    }

    pub(crate) fn complete_delivered_review_notes(
        &mut self,
        checkout: &str,
        rounds: &[muster_core::opencode::ReviewRound],
        cx: &mut Context<Self>,
    ) {
        let completed = rounds
            .iter()
            .filter(|round| round.checkout_id == checkout && round.status.is_settled())
            .flat_map(|round| round.claimed_keys())
            .collect();
        self.complete_review_notes(checkout, &completed, cx);
    }

    /// Insert the note's captured range as editable comment text. This is not
    /// clipboard selection and never changes the captured source or export format.
    pub(crate) fn suggest_review_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(composer) = self.review.composer.as_ref().filter(|composer| {
            self.open_diff.as_ref().is_some_and(|(checkout, path)| {
                *checkout == composer.checkout_id
                    && path.as_ref().is_none_or(|path| *path == composer.path)
            })
        }) else {
            return;
        };
        if !composer.input.read(cx).value().is_empty() {
            return;
        }
        let input = composer.input.clone();
        let code = composer.code.clone();
        input.update(cx, |input, cx| input.set_value(code, window, cx));
        input.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

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
                && composer.anchor.0 == side
                && start < composer.anchor.1
        }) {
            self.set_review_note_start(side, start, window, cx);
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
            review_start_picker(&lines, (side, start), (side, end), window, cx);
        let save_keys = review_save_keys(&input, cx);
        let subscription = cx.observe(&input, |_, _, cx| cx.notify());
        self.review.composer = Some(ReviewComposer {
            checkout_id: checkout_id.clone(),
            path,
            anchor: (side, end),
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
        let (start_picker, picker_events) = review_start_picker(
            &lines,
            (note.side, note.line_start),
            (note.side, line_end),
            window,
            cx,
        );
        let save_keys = review_save_keys(&input, cx);
        let subscription = cx.observe(&input, |_, _, cx| cx.notify());
        self.review.composer = Some(ReviewComposer {
            checkout_id: note.checkout_id,
            path: note.path,
            anchor: (note.side, line_end),
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

    /// The lines a note covers, washed in the diff's own selection.
    ///
    /// A note is about a range, and a range is invisible while the box that writes
    /// it floats over the diff: the start moves, the end does not, and nothing on
    /// screen says which lines the comment now covers. The toolkit's selection
    /// already paints that state, so the composer drives it rather than washing
    /// the lines a second time in a colour of its own. Called from where a draft
    /// is opened, moved or dropped, and from a diff that was rebuilt underneath
    /// one: a fresh state starts with nothing selected.
    pub(crate) fn sync_review_note_selection(&mut self, cx: &mut Context<Self>) {
        let range = self.review_selection_range();
        self.apply_diff_selection(range, cx);
    }

    /// The lines the diff's own selection wears: the draft being written, or
    /// the note that was applied last.
    ///
    /// A draft borrows the mark while it is written, because the range it
    /// covers is the one the composer is changing. With no draft, the note that
    /// was applied last owns it, so the lines a comment is about stay washed on
    /// the diff.
    fn review_selection_range(&self) -> Option<DiffLineRange> {
        if let Some(composer) = self.review.composer.as_ref() {
            return Some(review_note_range(
                &composer.path,
                composer.side,
                composer.line_start,
                composer.line_end,
            ));
        }
        let id = self.review.marked.as_ref()?;
        let note = self
            .review
            .notes
            .values()
            .flatten()
            .find(|note| &note.id == id)?;
        Some(review_note_range(
            &note.path,
            note.side,
            note.line_start,
            note.line_end.unwrap_or(note.line_start),
        ))
    }

    /// The range the diff's own selection wears, applied to the diff on screen.
    ///
    /// The selection is the diff's own, so a range that is not on screen is
    /// simply not drawn: the toolkit washes the lines it can find and leaves
    /// the rest alone.
    fn apply_diff_selection(&mut self, range: Option<DiffLineRange>, cx: &mut Context<Self>) {
        if let Some(view) = self.diff_view.as_ref() {
            view.state
                .update(cx, |state, cx| state.set_selected_lines(range, cx));
        }
    }

    pub(crate) fn set_review_note_start(
        &mut self,
        start_side: ReviewSide,
        line_start: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(composer) = self.review.composer.as_ref() else {
            return;
        };
        if line_start == 0 {
            return;
        }
        let (checkout_id, path, anchor) = (
            composer.checkout_id.clone(),
            composer.path.clone(),
            composer.anchor,
        );
        let Some(diff) = self.git_diff.get(&(checkout_id.clone(), path.clone())) else {
            return;
        };
        let lines = review::read_diff_lines(&diff.patch);
        // The range is read on the side its start is on, so a start the change
        // removed turns the note to the removed side and ends it on the last line
        // of that side the range covers. The anchor is where it was clicked, so
        // the card stays where the comment is being written.
        let Some(line_end) = review_start_lines(&lines, anchor)
            .into_iter()
            .rev()
            .find(|(side, _)| *side == start_side)
            .map(|(_, line)| line)
        else {
            return;
        };
        let Some(code) = review::review_range_code(&lines, start_side, line_start, line_end) else {
            return;
        };
        if code.len() > MAX_NOTE_CODE_BYTES {
            return;
        }
        // The picker is how this line is chosen, so it is told what the draft now
        // says rather than left to agree with it by itself.
        let chosen = review_start_label(
            line_start,
            start_side,
            lines[&(start_side, line_start)].mark,
        );
        let composer = self.review.composer.as_mut().expect("composer was read");
        composer.side = start_side;
        composer.line_start = line_start;
        composer.line_end = line_end;
        composer.code = code;
        composer.start_picker.update(cx, |picker, cx| {
            picker.set_selected_value(&chosen, window, cx)
        });
        self.review.anchor_hashes.remove(&(checkout_id, path));
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
        // The lines the note covers, kept for the diff: a note that is applied
        // leaves its range washed on the diff, because the mark is what says
        // which lines the comment is about.
        let range = review_note_range(
            &composer.path,
            composer.side,
            composer.line_start,
            composer.line_end,
        );
        let composer = self.review.composer.take().expect("composer was checked");
        let mut applied = composer.editing_id.clone();
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
                let reanchored = note.side != composer.side
                    || note.line_start != composer.line_start
                    || note.line_end != line_end
                    || note.code != composer.code;
                note.content = content;
                if reanchored {
                    note.side = composer.side;
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
                id: id.clone(),
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
            applied = Some(id);
        }
        // The note that was applied owns the mark: its lines stay washed on the
        // diff until another note takes the mark over or that note is deleted.
        self.review.marked = applied;
        self.apply_diff_selection(Some(range), cx);
        cx.notify();
    }

    pub(crate) fn cancel_review_note(&mut self, cx: &mut Context<Self>) {
        self.review.composer = None;
        self.sync_review_note_selection(cx);
        cx.notify();
    }

    /// A note is somebody's own writing and there is no undo behind it, so the cross
    /// asks rather than removes. The answer lives in the window rather than in the
    /// card, because the dialog is what is on screen while the question stands.
    pub(crate) fn request_delete_review_note(
        &mut self,
        checkout_id: &str,
        id: &str,
        cx: &mut Context<Self>,
    ) {
        self.review.delete_confirmation = Some((checkout_id.to_owned(), id.to_owned()));
        cx.notify();
    }

    pub(crate) fn cancel_delete_review_note(&mut self, cx: &mut Context<Self>) {
        self.review.delete_confirmation = None;
        cx.notify();
    }

    pub(crate) fn confirm_delete_review_note(&mut self, cx: &mut Context<Self>) {
        let Some((checkout_id, id)) = self.review.delete_confirmation.take() else {
            return;
        };
        self.delete_review_note(&checkout_id, &id, cx);
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
        // A note that is gone takes its lines with it: the mark belongs to the
        // note, so a note that is not there owns nothing.
        if self.review.marked.as_deref() == Some(id) {
            self.review.marked = None;
            self.apply_diff_selection(None, cx);
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
        // The default cache is one folder under the home directory that every checkout
        // writes to, so which checkout is being cleared is not what this action acts
        // on: it needs a checkout to resolve, and the panel is opened from the title
        // bar, where no diff is open.
        let checkout_id = self
            .open_diff
            .as_ref()
            .map(|(id, _)| id.clone())
            .or_else(|| self.navigation.active_checkout.clone());
        let Some(checkout_id) = checkout_id else {
            cx.notify();
            return;
        };
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
                    Ok(count) => app.push_info(
                        format!(
                            "Cleared {count} review export{}.",
                            if count == 1 { "" } else { "s" }
                        ),
                        cx,
                    ),
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
                            "This deletes only timestamped Markdown exports with Muster's ownership marker in ~/.muster/tmp/reviews. Every checkout writes there, so this clears the cache for all of them. Exports inside a checkout are never cleared here.",
                        ),
                    )
                })
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("cancel-clear-review-exports")
                                .label("Cancel")
                                .on_click(move |_, _, cx| {
                                    let _ = cancel.update(cx, |app, cx| {
                                        app.cancel_review_clear(cx);
                                    });
                                }),
                        )
                        .child(
                            Button::new("confirm-clear-review-exports")
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

    /// What deleting a note costs, said before it is done: it leaves the review, so a
    /// later export or a sent review does not carry it, and it cannot be undone.
    pub(crate) fn review_delete_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.review.delete_confirmation.as_ref()?;
        let roles = Roles::of(cx);
        let cancel = cx.weak_entity();
        let cancel_for_escape = cx.weak_entity();
        let delete = cx.weak_entity();
        Some(
            Dialog::new(cx)
                .bg(roles.surface_raised)
                .border_color(roles.border_strong)
                .width(px(440.))
                .title("Delete this note?")
                .content(|content: DialogContent, _, _| {
                    content.child(
                        div().text_size(px(12.)).child(
                            "The note is removed from this review: a later export or a sent review does not carry it, and it cannot be undone.",
                        ),
                    )
                })
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("cancel-delete-review-note")
                                .label("Cancel")
                                .on_click(move |_, _, cx| {
                                    let _ = cancel.update(cx, |app, cx| {
                                        app.cancel_delete_review_note(cx);
                                    });
                                }),
                        )
                        .child(
                            Button::new("confirm-delete-review-note")
                                .danger()
                                .label("Delete note")
                                .on_click(move |_, _, cx| {
                                    let _ = delete.update(cx, |app, cx| {
                                        app.confirm_delete_review_note(cx);
                                    });
                                }),
                        ),
                )
                .on_cancel(move |_, _, cx| {
                    let _ = cancel_for_escape.update(cx, |app, cx| {
                        app.cancel_delete_review_note(cx);
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

    pub(crate) fn set_destination(
        &mut self,
        destination: muster_core::opencode::Destination,
        cx: &mut Context<Self>,
    ) {
        if self.review.destination_busy || self.review.opencode_loading || self.review.exporting {
            return;
        }
        let Some((checkout_id, _)) = self.open_diff.as_ref() else {
            return;
        };
        let checkout_id = checkout_id.clone();
        self.review.destination_busy = true;
        self.review.destination_error = None;
        cx.notify();
        let Some(home) = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .filter(|path| path.is_absolute())
        else {
            self.review.destination_busy = false;
            self.review.destination_error =
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
                app.review.destination_busy = false;
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
                        app.review.destination = Some(destination);
                        app.review.destination_error = None;
                    }
                    Err(error) => app.review.destination_error = Some(error.to_string()),
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
            self.review.destination_error =
                Some("HOME is unavailable for OpenCode review storage.".into());
            return;
        };
        let checkout_id = checkout.to_owned();
        if self.review.opencode_checkout.as_deref() != Some(checkout) {
            self.review.opencode_generation = self.review.opencode_generation.wrapping_add(1);
            self.review.opencode_checkout = Some(checkout_id.clone());
            self.review.opencode_rounds.clear();
            self.review.opencode_loading = false;
            self.review.destination = None;
            self.review.destination_error = None;
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
                if !app.review.finish_opencode_restore(&checkout_id, generation) {
                    return;
                }
                if app.open_diff.as_ref().map(|(id, _)| id.as_str()) != Some(checkout_id.as_str()) {
                    return;
                }
                match result {
                    Ok((destination, rounds)) => {
                        app.review.destination = destination;
                        app.complete_delivered_review_notes(&checkout_id, &rounds, cx);
                        app.review.restore_pending_notes(&checkout_id, &rounds);
                        app.review.opencode_rounds = rounds;
                    }
                    Err(error) => app.review.destination_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn send_review_to_opencode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.review.destination_busy
            || self.review.opencode_loading
            || self.review.exporting
            || !can_start_opencode_round(&self.review.opencode_rounds)
        {
            return;
        }
        let Some(review) = self.prepare_opencode_review(cx) else {
            return;
        };
        // The poll only paints the panel. The locked send path reads activity again
        // before offering anything and returns the review for a choice if now busy.
        self.dispatch_opencode_review(review, OpenCodeDispatch::IfReady, window, cx);
    }

    /// Gathers what a review to OpenCode would carry, without sending anything.
    ///
    /// The notes are what no round has already taken, unchanged: a review that was
    /// delivered is not delivered again because the window was reopened, and a note
    /// somebody edited since is a new comment and is kept.
    fn prepare_opencode_review(&mut self, cx: &mut Context<Self>) -> Option<OpenCodeReview> {
        let (checkout_id, _) = self.open_diff.as_ref()?;
        let checkout_id = checkout_id.clone();
        let Some(checkout) = self.navigation.checkout(&checkout_id).cloned() else {
            self.review.destination_error =
                Some("The checkout for this review is no longer available.".into());
            cx.notify();
            return None;
        };
        let Some(repo) = self
            .navigation
            .repos
            .iter()
            .find(|repo| repo.id == checkout.repo_id)
            .cloned()
        else {
            self.review.destination_error =
                Some("The repository for this review is no longer available.".into());
            cx.notify();
            return None;
        };
        let notes = opencode_send_notes(&self.review, &checkout_id)
            .into_iter()
            .filter(|note| self.review.include_outdated || !note.outdated)
            .collect::<Vec<_>>();
        if notes.is_empty() {
            self.review.destination_error =
                Some("There are no review notes to send: every note here was already sent.".into());
            cx.notify();
            return None;
        }
        let (date, _) = match review::local_review_timestamp() {
            Ok(value) => value,
            Err(error) => {
                self.review.destination_error = Some(error);
                cx.notify();
                return None;
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
        // The session the review would go to. A destination that names none is not a
        // destination: the review would go nowhere.
        let session_id = match self.review.destination.clone() {
            Some(muster_core::opencode::Destination::Opencode { session_id }) => Some(session_id),
            Some(muster_core::opencode::Destination::OpencodeNew) => None,
            _ => {
                self.review.destination_error =
                    Some("OpenCode is not the selected review destination.".into());
                cx.notify();
                return None;
            }
        };
        Some(OpenCodeReview {
            checkout,
            session_id,
            notes,
            markdown,
            title,
        })
    }

    /// Sends or queues a prepared review.
    ///
    /// The round is recorded before it is offered either way: a queued review is
    /// written down with the exact message it will be sent with, so a round that waits
    /// is a round that provably was never taken, and a round that is sent is claimed
    /// before the request goes out.
    fn dispatch_opencode_review(
        &mut self,
        review: OpenCodeReview,
        mode: OpenCodeDispatch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(home) = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute())
        else {
            self.review.destination_error =
                Some("HOME is unavailable for OpenCode review storage.".into());
            cx.notify();
            return;
        };
        let checkout_id = review.checkout.id.clone();
        self.review.opencode_choice = None;
        self.review.destination_busy = true;
        self.review.destination_error = None;
        cx.notify();
        let worker = cx.background_executor().spawn(async move {
            use muster_core::opencode::{
                rounds::RoundRequest, Client, Destination, ReadBudget, RoundStore,
            };
            let store = RoundStore::open(&home)?;
            let mut review = review;
            // Queueing an existing destination is a durable local action, even when
            // the service disappeared after the choice was shown.
            let client = if mode != OpenCodeDispatch::Queue || review.session_id.is_none() {
                Some(Client::connect_with_budget(
                    &home,
                    std::path::Path::new(&review.checkout.canonical_path),
                    // A send waits for the service on purpose: the deadline is the
                    // whole action, and it is longer than an observation's.
                    ReadBudget::patient(std::time::Duration::from_secs(30)),
                )?)
            } else {
                None
            };
            let session_id = match review.session_id.clone() {
                Some(session_id) => session_id,
                None => {
                    // A session created for this review is Muster's own, so the
                    // destination remembers it and the next review goes to the same
                    // conversation.
                    let client = client.as_ref().ok_or_else(|| {
                        muster_core::opencode::DispatchError::Unavailable(
                            "OpenCode is unavailable to create a destination".into(),
                        )
                    })?;
                    let session_id = client.create_session(&review.title)?;
                    store.set_destination(
                        &review.checkout.id,
                        Destination::Opencode {
                            session_id: session_id.clone(),
                        },
                    )?;
                    session_id
                }
            };
            review.session_id = Some(session_id.clone());
            let request = RoundRequest {
                checkout: &review.checkout,
                session_id: &session_id,
                notes: &review.notes,
                review_markdown: &review.markdown,
            };
            let outcome = match (mode, client.as_ref()) {
                (OpenCodeDispatch::IfReady, Some(client)) => {
                    store.send_round_if_ready(client, request)
                }
                (OpenCodeDispatch::Now, Some(client)) => {
                    store.send_round(client, request).map(Some)
                }
                (OpenCodeDispatch::Queue, _) => store.queue_round(request).map(Some),
                _ => Err(muster_core::opencode::DispatchError::Unavailable(
                    "OpenCode is unavailable; no review was sent".into(),
                )),
            };
            let needs_choice = matches!(outcome, Ok(None));
            // The round is on disk on any uncertain send, so refresh even if the answer
            // was lost. A busy preflight has claimed nothing and keeps the same review.
            let rounds = store.rounds(&review.checkout.id)?;
            Ok::<_, muster_core::opencode::DispatchError>((
                Destination::Opencode { session_id },
                rounds,
                outcome.err().map(|error| error.to_string()),
                needs_choice.then_some(review),
            ))
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update(cx, |app, cx| {
                app.review.destination_busy = false;
                if let Ok((_, rounds, _, _)) = &result {
                    app.complete_delivered_review_notes(&checkout_id, rounds, cx);
                }
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
                    Ok((destination, rounds, dispatch_error, choice)) => {
                        app.review.destination = Some(destination);
                        app.review.opencode_rounds = rounds;
                        app.review.destination_error = dispatch_error;
                        app.review.opencode_choice = choice;
                    }
                    Err(error) => app.review.destination_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The choice a busy session raises, asked before anything is sent.
    pub(crate) fn opencode_choice_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let review = self.review.opencode_choice.as_ref()?;
        let roles = Roles::of(cx);
        let cancel = cx.weak_entity();
        let cancel_for_escape = cx.weak_entity();
        let send_now = cx.weak_entity();
        let queue = cx.weak_entity();
        let session = review
            .session_id
            .as_deref()
            .and_then(|session_id| self.agent.session(session_id))
            .map(|session| session.display_title().to_string())
            .unwrap_or_else(|| "the selected session".into());
        let message = format!(
            "{session} is working right now. Send the review into the turn that is running, queue it for the turn that follows, or do not send it."
        );
        Some(
            Dialog::new(cx)
                .bg(roles.surface_raised)
                .border_color(roles.border_strong)
                .width(px(440.))
                .title("Choose when to send")
                .content(move |content: DialogContent, _, _| {
                    content.child(div().text_size(px(12.)).child(message.clone()))
                })
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("cancel-opencode-send")
                                .label("Not now")
                                .on_click(move |_, _, cx| {
                                    let _ = cancel.update(cx, |app, cx| {
                                        app.cancel_opencode_choice(cx);
                                    });
                                }),
                        )
                        .child(Button::new("queue-opencode-send").label("Queue").on_click(
                            move |_, window, cx| {
                                let _ = queue.update(cx, |app, cx| {
                                    app.choose_opencode_dispatch(
                                        OpenCodeDispatch::Queue,
                                        window,
                                        cx,
                                    );
                                });
                            },
                        ))
                        .child(
                            Button::new("send-opencode-send")
                                .primary()
                                .label("Send now")
                                .on_click(move |_, window, cx| {
                                    let _ = send_now.update(cx, |app, cx| {
                                        app.choose_opencode_dispatch(
                                            OpenCodeDispatch::Now,
                                            window,
                                            cx,
                                        );
                                    });
                                }),
                        ),
                )
                .on_cancel(move |_, _, cx| {
                    let _ = cancel_for_escape.update(cx, |app, cx| {
                        app.cancel_opencode_choice(cx);
                    });
                    false
                })
                .into_any_element(),
        )
    }

    /// The choice a busy session raised is answered.
    pub(crate) fn choose_opencode_dispatch(
        &mut self,
        mode: OpenCodeDispatch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(review) = self.review.opencode_choice.take() else {
            return;
        };
        self.dispatch_opencode_review(review, mode, window, cx);
    }

    /// Refreshes the sessions a review of the open diff can go to.
    ///
    /// A read of what the service lists rather than a list kept in step with it: the
    /// service is the only thing that knows which sessions exist.
    pub(crate) fn sync_opencode_sessions(&mut self, cx: &mut Context<Self>) {
        let Some((checkout_id, _)) = self.open_diff.as_ref() else {
            self.review.opencode_sessions.clear();
            return;
        };
        let sessions = self.opencode_sessions_for(checkout_id);
        self.review.opencode_sessions = sessions;
        cx.notify();
    }

    /// The sessions the chooser offers, after the search.
    ///
    /// The search is case-insensitive and matches the title, because that is the only
    /// thing a person knows about a session.
    pub(crate) fn opencode_session_choices(&self) -> Vec<muster_core::opencode::AgentSession> {
        crate::agent_watch::sessions_matching(
            &self.review.opencode_sessions,
            &self.review.opencode_query,
        )
    }

    /// Chooses where OpenCode sends this checkout's reviews.
    ///
    /// The newest session is the one somebody is most likely to mean, and a session
    /// created for this review is offered in the chooser for the times the newest is
    /// not it.
    pub(crate) fn choose_opencode_destination(&mut self, cx: &mut Context<Self>) {
        // An explicit choice is kept while it still exists: choosing "OpenCode" again
        // is not a request for a different session.
        let destination = match self.review.destination.clone() {
            Some(destination @ muster_core::opencode::Destination::Opencode { .. }) => destination,
            _ => match self.review.opencode_sessions.first() {
                Some(session) => muster_core::opencode::Destination::Opencode {
                    session_id: session.id.clone(),
                },
                None => muster_core::opencode::Destination::OpencodeNew,
            },
        };
        self.set_destination(destination, cx);
    }

    /// Chooses one session, or a session created for this review.
    pub(crate) fn choose_opencode_session(
        &mut self,
        session_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let destination = match session_id {
            Some(session_id) => muster_core::opencode::Destination::Opencode { session_id },
            None => muster_core::opencode::Destination::OpencodeNew,
        };
        self.set_destination(destination, cx);
    }

    /// What the search over the sessions said.
    pub(crate) fn set_opencode_query(&mut self, query: String, cx: &mut Context<Self>) {
        self.review.opencode_query = query;
        cx.notify();
    }

    /// Opens the search over the sessions a review can go to.
    ///
    /// The field is opened once and kept: it is part of the panel that says where a
    /// review goes, and a search that disappears between two openings is a search that
    /// loses what was typed in it.
    pub(crate) fn open_opencode_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.review.opencode_search.is_some() {
            return;
        }
        let query = self.review.opencode_query.clone();
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_value(query, window, cx);
            input
        });
        let subscription = cx.subscribe_in(&input, window, |app, _, event, _, cx| {
            if !matches!(event, gpui_kit::component::input::InputEvent::Change) {
                return;
            }
            let value = app
                .review
                .opencode_search
                .as_ref()
                .map(|input| input.read(cx).value().to_string())
                .unwrap_or_default();
            app.set_opencode_query(value, cx);
        });
        self.review.opencode_search = Some(input);
        self.review.opencode_search_subscription = Some(subscription);
        cx.notify();
    }

    /// The choice is not answered, and the review does not go.
    pub(crate) fn cancel_opencode_choice(&mut self, cx: &mut Context<Self>) {
        self.review.opencode_choice = None;
        cx.notify();
    }

    pub(crate) fn retry_queued_opencode_rounds(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.review.destination_busy || self.review.opencode_loading {
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
        self.review.destination_busy = true;
        let worker = cx.background_executor().spawn(async move {
            let store = muster_core::opencode::RoundStore::open(&home)?;
            let client = muster_core::opencode::Client::connect_with_budget(
                &home,
                std::path::Path::new(&checkout.path),
                muster_core::opencode::ReadBudget::patient(std::time::Duration::from_secs(30)),
            )?;
            let flush = store.flush_rounds(&client, &checkout.id);
            let rounds = store.rounds(&checkout.id)?;
            Ok::<_, muster_core::opencode::DispatchError>((flush, rounds))
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update(cx, |app, cx| {
                app.review.destination_busy = false;
                if let Ok((_, rounds)) = &result {
                    app.complete_delivered_review_notes(&checkout_id, rounds, cx);
                }
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
                        app.review.destination_error = flush.err().map(|error| error.to_string());
                    }
                    Err(error) => app.review.destination_error = Some(error.to_string()),
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
        if self.review.destination_busy {
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
        self.review.destination_busy = true;
        cx.notify();
        let worker = cx.background_executor().spawn(async move {
            let store = muster_core::opencode::RoundStore::open(&home)?;
            let client = muster_core::opencode::Client::connect_with_budget(
                &home,
                std::path::Path::new(&checkout.path),
                muster_core::opencode::ReadBudget::patient(std::time::Duration::from_secs(30)),
            )?;
            store.reconcile_round(&client, &checkout.id, &round_id)?;
            store.rounds(&checkout.id)
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update(cx, |app, cx| {
                app.review.destination_busy = false;
                if let Ok(rounds) = &result {
                    app.complete_delivered_review_notes(&checkout_id, rounds, cx);
                }
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
                        app.review.destination_error = None;
                    }
                    Err(error) => app.review.destination_error = Some(error.to_string()),
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
        if self.review.destination_busy {
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
        self.review.destination_busy = true;
        cx.notify();
        let worker = cx.background_executor().spawn(async move {
            let store = muster_core::opencode::RoundStore::open(&home)?;
            store.resend_round(&checkout.id, &round_id)?;
            store.rounds(&checkout.id)
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update(cx, |app, cx| {
                app.review.destination_busy = false;
                if let Ok(rounds) = &result {
                    app.complete_delivered_review_notes(&checkout_id, rounds, cx);
                }
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
                        app.review.destination_error = None;
                    }
                    Err(error) => app.review.destination_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens the confirmation for giving up the reviews Muster has already delivered.
    ///
    /// The store refuses to grow past 8 MiB and gives up a delivered review's prompt on
    /// its own; this is the deliberate half, for a store whose notes are the weight.
    ///
    /// What it gives up is every checkout's, not one diff's, and it has to be reachable
    /// from anywhere: the moment a person needs room is usually when no diff is open,
    /// which is when a route that needed one would leave the store full for good.
    pub(crate) fn open_opencode_forget_confirmation(&mut self, cx: &mut Context<Self>) {
        if self.review.forgetting {
            return;
        }
        self.review.forget_confirmation = true;
        cx.notify();
    }

    pub(crate) fn cancel_opencode_forget(&mut self, cx: &mut Context<Self>) {
        self.review.forget_confirmation = false;
        cx.notify();
    }

    /// Forgets the reviews the service has already answered for, and says how many.
    ///
    /// Nothing unresolved is touched and nothing already sent is sent again: what each
    /// forgotten review took stays in its claim as a key rather than as the review, so
    /// the notes a person still has on screen are still not offered as a new review.
    pub(crate) fn forget_delivered_opencode_reviews(
        &mut self,
        reset_duplicate_scope: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.review.forgetting {
            return;
        }
        self.review.forget_confirmation = false;
        // The store is one file for the whole workspace, so this is not about a diff;
        // an open one is only re-read afterwards, to show what is left of it.
        let checkout_id = self.open_diff.as_ref().map(|(id, _)| id.clone());
        let Some(home) = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute())
        else {
            self.review.destination_error =
                Some("HOME is unavailable for OpenCode review storage.".into());
            cx.notify();
            return;
        };
        let worker_checkout_id = checkout_id.clone();
        self.review.forgetting = true;
        self.review.destination_error = None;
        cx.notify();
        let worker = cx.background_executor().spawn(async move {
            let store = muster_core::opencode::RoundStore::open(&home)?;
            let forgotten = if reset_duplicate_scope {
                store.reset_settled_duplicate_scope()?
            } else {
                store.forget_settled_history()?
            };
            // Re-read what the open diff shows, if it was showing anything to begin with.
            let rounds = match &worker_checkout_id {
                Some(id) => Some(store.rounds(id)?),
                None => None,
            };
            Ok::<_, muster_core::opencode::DispatchError>((forgotten, rounds))
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let _ = view.update(cx, |app, cx| {
                app.review.forgetting = false;
                let still_open = app.open_diff.as_ref().map(|(id, _)| id.as_str());
                let ours = checkout_id.is_some() && still_open == checkout_id.as_deref();
                match result {
                    Ok((forgotten, rounds)) => {
                        if ours {
                            // This result supersedes any restore started while the
                            // store was read.
                            app.review.opencode_generation =
                                app.review.opencode_generation.wrapping_add(1);
                            app.review.opencode_loading = false;
                            if let Some(rounds) = rounds {
                                app.review.opencode_rounds = rounds;
                            }
                            app.review.destination_error = None;
                        } else if let Some((active, _)) = app.open_diff.clone() {
                            // The diff this was asked about is closed or is another one,
                            // so what was forgotten is not what it is showing.
                            app.restore_opencode_review(&active, cx);
                        }
                        app.push_info(
                            if reset_duplicate_scope {
                                format!(
                                    "Forgot {forgotten} delivered review{} and reset duplicate protection.",
                                    if forgotten == 1 { "" } else { "s" }
                                )
                            } else {
                                format!(
                                    "Forgot {forgotten} delivered review{}; duplicate protection is retained.",
                                    if forgotten == 1 { "" } else { "s" }
                                )
                            },
                            cx,
                        );
                    }
                    Err(error) => app.review.destination_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// What forgetting costs, said before it is done: what goes, what stays, and what
    /// cannot be undone.
    pub(crate) fn opencode_forget_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.review.forget_confirmation {
            return None;
        }
        let roles = Roles::of(cx);
        let cancel = cx.weak_entity();
        let cancel_for_escape = cx.weak_entity();
        let forget = cx.weak_entity();
        let reset = cx.weak_entity();
        Some(
            Dialog::new(cx)
                .bg(roles.surface_raised)
                .border_color(roles.border_strong)
                .width(px(440.))
                .title("Forget delivered reviews?")
                .content(|content: DialogContent, _, _| {
                    content.child(
                        div().text_size(px(12.)).child(
                            "Forget delivered reviews to remove their notes while keeping duplicate protection, or reset duplicate protection to remove settled records entirely. Resetting means unchanged delivered notes can be sent again. Queued, dispatching, and unresolved reviews are kept. This cannot be undone.",
                        ),
                    )
                })
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("cancel-forget-opencode-reviews")
                                .label("Cancel")
                                .on_click(move |_, _, cx| {
                                    let _ = cancel.update(cx, |app, cx| {
                                        app.cancel_opencode_forget(cx);
                                    });
                                }),
                        )
                        .child(
                            Button::new("confirm-forget-opencode-reviews")
                                .label("Forget notes")
                                .on_click(move |_, window, cx| {
                                    let _ = forget.update(cx, |app, cx| {
                                        app.forget_delivered_opencode_reviews(false, window, cx);
                                    });
                                }),
                        )
                        .child(
                            Button::new("confirm-reset-opencode-reviews")
                                .danger()
                                .label("Forget and reset")
                                .on_click(move |_, window, cx| {
                                    let _ = reset.update(cx, |app, cx| {
                                        app.forget_delivered_opencode_reviews(true, window, cx);
                                    });
                                }),
                        ),
                )
                .on_cancel(move |_, _, cx| {
                    let _ = cancel_for_escape.update(cx, |app, cx| {
                        app.cancel_opencode_forget(cx);
                    });
                    false
                })
                .into_any_element(),
        )
    }

    /// Apply an export's answer to the snapshots it captured, even if the user
    /// switched checkout or kept editing while the file was being written.
    pub(crate) fn finish_review_export(
        &mut self,
        checkout_id: &str,
        completed: &HashSet<String>,
        result: Result<PathBuf, String>,
        cx: &mut Context<Self>,
    ) {
        self.review.exporting = false;
        match result {
            Ok(path) => {
                self.complete_review_notes(checkout_id, completed, cx);
                if self.open_diff.as_ref().map(|(id, _)| id.as_str()) == Some(checkout_id) {
                    self.review.destination = Some(muster_core::opencode::Destination::Markdown);
                }
                // Keep the receipt visible after the pending notes are gone.
                self.review.exported = Some(ExportedReview {
                    checkout_id: checkout_id.to_owned(),
                    path,
                });
            }
            Err(error) => self.push_error(error, cx),
        }
        cx.notify();
    }

    pub(crate) fn export_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.review.exporting || self.review.destination_busy || self.review.opencode_loading {
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
        let completed = notes.iter().map(review::review_note_key).collect();
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
                app.finish_review_export(&checkout_id, &completed, result, cx);
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_may_start_on_a_line_the_change_removed_above_the_line_it_ends_on() {
        // Two untouched lines, two the change removed, one it added: the note ends
        // on the added one and may start on any line the diff draws above it.
        let lines = review::read_diff_lines("@@ -1,4 +1,3 @@\n one\n two\n-three\n-four\n+THREE");
        let starts = review_start_lines(&lines, (ReviewSide::New, 3));
        assert_eq!(
            starts,
            vec![
                (ReviewSide::New, 1),
                (ReviewSide::New, 2),
                (ReviewSide::Old, 3),
                (ReviewSide::Old, 4),
                (ReviewSide::New, 3),
            ]
        );
        assert_eq!(
            review_start_choices(&lines, (ReviewSide::New, 2), (ReviewSide::New, 3))
                .0
                .iter()
                .map(|label| label.as_ref())
                .collect::<Vec<_>>(),
            ["1", "2", "-3", "-4", "+3"]
        );
        // A note that ends on a removed line is offered the untouched lines above
        // it on that side, because they are the numbers a range on it is written
        // in.
        assert_eq!(
            review_start_lines(&lines, (ReviewSide::Old, 4)),
            vec![
                (ReviewSide::Old, 1),
                (ReviewSide::Old, 2),
                (ReviewSide::Old, 3),
                (ReviewSide::Old, 4),
            ]
        );
    }

    #[test]
    fn a_range_never_reaches_over_the_gap_into_the_next_hunk() {
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

        assert_eq!(
            review_start_lines(&lines, (ReviewSide::Old, 5)),
            vec![(ReviewSide::Old, 5)]
        );
        assert_eq!(
            review_start_lines(&lines, (ReviewSide::New, 4)),
            vec![(ReviewSide::Old, 3), (ReviewSide::New, 4)]
        );
        assert_eq!(
            review_start_lines(&lines, (ReviewSide::New, 7)),
            vec![(ReviewSide::Old, 5), (ReviewSide::New, 7)]
        );
    }

    #[test]
    fn picker_labels_keep_old_lines_negative_and_mark_added_new_lines() {
        for (side, mark, label) in [
            (ReviewSide::Old, ' ', "-22"),
            (ReviewSide::New, '+', "+22"),
            (ReviewSide::New, ' ', "22"),
        ] {
            assert_eq!(review_start_label(22, side, mark).as_ref(), label);
            // The label is the whole of what the picker reports, so the side the
            // note ends up on has to come back out of it too.
            assert_eq!(review_line_label(label), Some((side, 22)));
        }
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
    fn a_completed_review_removes_only_the_snapshots_it_saved() {
        let saved = note("saved", "done");
        let before_edit = note("edited", "before");
        let edited = note("edited", "after");
        let new = note("new", "still pending");
        let mut excluded = note("outdated", "not exported");
        excluded.outdated = true;
        let mut state = ReviewState::default();
        state.notes.insert(
            "checkout-1".into(),
            vec![saved.clone(), edited.clone(), new.clone(), excluded.clone()],
        );
        state.notes.insert("checkout-2".into(), vec![saved.clone()]);
        state.marked = Some(saved.id.clone());
        let completed = [saved, before_edit]
            .iter()
            .map(review::review_note_key)
            .collect();

        assert!(state.complete_notes("checkout-1", &completed));
        assert_eq!(state.notes["checkout-1"], vec![edited, new, excluded]);
        assert_eq!(state.notes["checkout-2"].len(), 1);
        assert!(state.marked.is_none());
    }

    #[test]
    fn exporting_a_queued_note_does_not_restore_it_when_the_diff_reopens() {
        use muster_core::opencode::RoundStatus;
        // Completing Markdown is local to this window, not a cancellation or
        // modification of any durable OpenCode payload.
        for status in [
            RoundStatus::Queued,
            RoundStatus::Dispatching,
            RoundStatus::Unknown,
        ] {
            let original = note("exported", "saved as Markdown");
            let mut round = sent_round("pending", vec![original.clone()]);
            round.status = status;
            let history = round.clone();
            let completed = HashSet::from([review::review_note_key(&original)]);
            let mut state = ReviewState::default();
            state.notes.insert("checkout-1".into(), vec![original]);
            state.complete_notes("checkout-1", &completed);
            state.opencode_rounds.clear();
            state.opencode_checkout = Some("another-checkout".into());
            state.restore_pending_notes("checkout-1", std::slice::from_ref(&round));
            assert!(state.notes["checkout-1"].is_empty(), "{status:?}");
            assert_eq!(round, history, "the pending send remains intact");

            // A changed snapshot is pending work, even with the same note id.
            let mut edited = round.clone();
            edited.notes[0].content = "new writing".into();
            state.restore_pending_notes("checkout-1", std::slice::from_ref(&edited));
            assert_eq!(state.notes["checkout-1"], edited.notes);
        }
    }

    #[test]
    fn a_delivery_invalidates_an_older_restore_without_clearing_the_new_read() {
        let original = note("delivered", "sent to OpenCode");
        let mut queued = sent_round("pending", vec![original.clone()]);
        queued.status = muster_core::opencode::RoundStatus::Queued;
        let mut state = ReviewState::default();
        state
            .notes
            .insert("checkout-1".into(), vec![original.clone()]);
        state.opencode_checkout = Some("checkout-1".into());
        state.opencode_loading = true;
        let old_generation = state.opencode_generation;

        // The watcher confirms delivery after the read captured Queued.
        state.complete_notes(
            "checkout-1",
            &HashSet::from([review::review_note_key(&original)]),
        );
        assert!(
            !state.opencode_loading,
            "the cancelled read cannot block refresh"
        );
        let fresh_generation = state.opencode_generation;
        state.opencode_loading = true;
        if state.finish_opencode_restore("checkout-1", old_generation) {
            state.restore_pending_notes("checkout-1", &[queued]);
        }
        assert!(state.notes["checkout-1"].is_empty());
        assert!(
            state.opencode_loading,
            "a stale callback cannot finish the fresh read"
        );
        assert!(state.finish_opencode_restore("checkout-1", fresh_generation));
        assert!(!state.opencode_loading);
    }

    #[test]
    fn delivered_rounds_never_return_as_pending_notes() {
        use muster_core::opencode::RoundStatus;
        let mut rounds = Vec::new();
        for (id, status) in [
            ("queued", RoundStatus::Queued),
            ("sending", RoundStatus::Dispatching),
            ("unknown", RoundStatus::Unknown),
            ("delivered", RoundStatus::Dispatched),
            ("finished", RoundStatus::Acked),
        ] {
            let mut round = sent_round(id, vec![note(id, id)]);
            round.status = status;
            rounds.push(round);
        }
        let edited = note("delivered", "edited during send");
        let mut state = ReviewState::default();
        state.notes.insert(
            "checkout-1".into(),
            vec![edited.clone(), note("finished", "finished")],
        );
        let completed = rounds
            .iter()
            .filter(|round| round.status.is_settled())
            .flat_map(|round| round.claimed_keys())
            .collect();
        state.complete_notes("checkout-1", &completed);
        state.restore_pending_notes("checkout-1", &rounds);
        state.restore_pending_notes("checkout-1", &rounds);
        assert_eq!(state.notes["checkout-1"].len(), 4);
        assert!(state.notes["checkout-1"].contains(&edited));
        for id in ["queued", "sending", "unknown"] {
            assert!(state.notes["checkout-1"].iter().any(|note| note.id == id));
        }
        assert!(!state.notes["checkout-1"]
            .iter()
            .any(|note| note.id == "finished"));
        assert_eq!(
            rounds[3].notes,
            vec![note("delivered", "delivered")],
            "delivery history remains immutable"
        );
    }

    #[test]
    fn a_compacted_delivery_claim_cannot_restore_an_older_queued_snapshot() {
        let original = note("note-1", "delivered");
        let mut queued = sent_round("queued", vec![original.clone()]);
        queued.status = muster_core::opencode::RoundStatus::Queued;
        let mut delivered = sent_round("delivered", vec![original.clone()]);
        delivered.claim = review::review_note_key(&original);
        delivered.notes.clear();
        let mut state = ReviewState::default();
        state.restore_pending_notes("checkout-1", &[queued, delivered]);
        assert!(state.notes["checkout-1"].is_empty());
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
