//! Retained, virtualized unified code review surface.
use crate::{
    app::{InspectorTab, TerminalWindow, ViewKind, ViewPin, UI_FONT_FAMILY},
    theme::Roles,
    tooltip::RestingTooltip,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::diff::{
    Diff, DiffAnnotation, DiffEvent, DiffFile, DiffLinePosition, DiffSide, DiffState,
};
use gpui_kit::component::select::Select;
use gpui_kit::component::Disableable;
use gpui_kit::component::Sizable;
use gpui_kit::component::{
    input::Textarea,
    menu::{DropdownMenu, PopupMenuItem},
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    div, px, Anchor, AnyElement, AppContext, Context, Entity, Focusable, InteractiveElement,
    IntoElement, ParentElement, StatefulInteractiveElement, Styled, Subscription, TestSupportExt,
};
use muster_core::review::{self, AnchorOutcome, ReviewSide};
use muster_core::workspace::RepoKind;

pub(crate) const MAX_RETAINED_PATCH_BYTES: usize = 32 * 1024 * 1024;

/// The type a diff's own chrome is drawn at: Legacy's file header, one size for
/// the status letter, the path and the counts alike.
///
/// The toolkit draws that header at `text_sm()`, and the code below refines
/// over it, so this is the one place the size and the face are named. The code
/// rows keep the size they set for themselves, and so does a notice, which is
/// prose about the change and reads at the code's own size rather than at the
/// chrome's. The `@@` band is the one thing here this crate cannot reach: the
/// toolkit sets it at `text_xs()` with no prop and no theme field behind it.
pub(crate) const DIFF_CHROME_PX: f32 = 11.;

/// One parsed viewer: individual files and All changes share the same scroll,
/// source-coordinate, selection, folding and highlighting implementation.
pub(crate) struct DiffView {
    owner: (String, Option<String>),
    revisions: Vec<(String, String)>,
    pub(crate) state: Entity<DiffState>,
    pub(crate) parse_errors: std::collections::HashMap<String, String>,
    pub(crate) ready: std::collections::HashSet<String>,
    _events: Subscription,
}

/// What a change with no text to show says about itself.
///
/// "Binary file" is a fact about the encoding and nothing about the change: a
/// picture that was added, a picture that went away and a picture that was
/// replaced are three different things to somebody reviewing a branch. The format
/// comes from the bytes the diff refused to print and the direction from the
/// letter Git already put on the row, so the sentence names the change rather
/// than the encoding. A file that is not an image keeps saying what it is.
fn binary_notice(image: Option<muster_core::navigation::ImageFormat>, status: &str) -> String {
    let Some(format) = image else {
        return "Binary file; text diff is unavailable.".into();
    };
    match status.chars().next() {
        Some('?') | Some('A') => format!("Image added · {}.", format.name()),
        Some('D') => format!("Image removed · {}.", format.name()),
        _ => format!("Image changed · {}.", format.name()),
    }
}

/// Whether a changed file is still on disk to be read.
///
/// Git's own letter is the answer, and it is the letter already drawn on the row:
/// a change that removed the file puts `D` in one of its columns, and there is
/// nothing behind the path to open. Every other letter — added, modified,
/// renamed, untracked, conflicted — has bytes there. A path Git has said nothing
/// about is not known to be gone either, so it is offered and the read is what
/// says otherwise.
fn on_disk(status: &str) -> bool {
    !status.contains('D')
}

/// What the window knows about a checkout's change set, as far as a row's click
/// is concerned.
///
/// Line counts are not part of this. A checkout whose only change is one binary
/// file counts `+0 −0` and still has changes to read, so a figure of zero is not
/// an answer and an absent figure is not an answer either. The change list Git
/// was actually asked for is the answer, and it is one only once a read has
/// returned it.
enum ChangePresence {
    Present,
    Absent,
    /// No read has answered for this checkout.
    Unknown,
    /// A read answered that it could not tell.
    Failed,
}

/// A row click waiting for Git to say whether its checkout has a change set.
#[derive(Clone)]
pub(crate) struct RowChoice {
    checkout: String,
    /// The [`TerminalWindow::view_intent`] this click established.
    intent: u64,
}

impl TerminalWindow {
    /// The Git status letter for one file of one checkout, or an empty string.
    ///
    /// The letter is already on the row and in the change list, so a notice that
    /// names a change reads the same one rather than asking Git again.
    fn git_status_status(&self, checkout: &str, path: &str) -> String {
        self.git_status
            .get(checkout)
            .and_then(|status| status.files.iter().find(|file| file.path == path))
            .map(|file| file.status.clone())
            .unwrap_or_default()
    }
}

fn placeholder(path: &str) -> DiffFile {
    let old = serde_json::to_string(&format!("a/{path}")).expect("a filename is JSON text");
    let new = serde_json::to_string(&format!("b/{path}")).expect("a filename is JSON text");
    DiffFile::parse(&format!("diff --git {old} {new}\n"))
        .expect("quoted file header")
        .remove(0)
}

/// The gesture a file's header row carries: open the file's diff, or close it.
///
/// The toolkit's `Diff` answers a click on its chevron with
/// `toggle_file_collapsed`, which is private to the crate and emits the event
/// the window listens for. A row of the window's own therefore folds or unfolds
/// the same state itself and then says so, so both answers are one transition
/// rather than two.
struct ToggleDiffFile {
    state: Entity<DiffState>,
    weak: gpui_kit::WeakEntity<TerminalWindow>,
    checkout: String,
    path: String,
}

impl ToggleDiffFile {
    fn apply(&self, cx: &mut gpui_kit::App) {
        // The chevron's own gesture: whatever the file is now, the click makes
        // it the other thing.
        let opening = self.state.read(cx).is_file_collapsed(&self.path);
        self.state.update(cx, |state, cx| {
            state.set_file_collapsed(&self.path, !opening, cx)
        });
        let _ = self.weak.update(cx, |app, cx| {
            if opening {
                app.expand_diff_file(&self.checkout, &self.path, cx);
            } else {
                app.collapse_diff_file(&self.checkout, &self.path);
            }
            cx.notify();
        });
    }
}

impl TerminalWindow {
    /// Whether Git answers for this checkout at all.
    ///
    /// A plain directory has no change set to have, which the panel already says
    /// in as many words. That is a known answer rather than a missing one, so it
    /// is settled here instead of waiting for a read nobody will make.
    fn is_git_checkout(&self, checkout: &str) -> bool {
        self.navigation.repos.iter().any(|repo| {
            repo.kind == RepoKind::Git && repo.checkouts.iter().any(|item| item.id == checkout)
        })
    }

    /// What Git's own change list says about a checkout, and how sure the window is.
    ///
    /// A failed read is the newest answer the window holds about a checkout, so
    /// it outranks the list it is still holding: a read that failed after a clean
    /// one never said the checkout is clean, and one that failed after a dirty
    /// one never said the change set is still there. The list is kept for what
    /// is drawn — the panel shows it under the failure — and is not an answer to
    /// a question asked after the failure.
    fn change_presence(&self, checkout: &str) -> ChangePresence {
        if !self.is_git_checkout(checkout) {
            return ChangePresence::Absent;
        }
        if self.git_status_errors.contains_key(checkout) {
            return ChangePresence::Failed;
        }
        match self.git_status.get(checkout) {
            Some(status) if status.files.is_empty() => ChangePresence::Absent,
            Some(_) => ChangePresence::Present,
            None => ChangePresence::Unknown,
        }
    }

    /// Apply a row click that had to wait for Git, if it is still what the person
    /// last asked for.
    ///
    /// The read answers after the click, and the window does not stand still while
    /// it runs: another row, a terminal, or a file in the tree has already said
    /// what should be on screen. An answer that arrives after any of those is
    /// dropped rather than allowed to put a change set, or the keyboard, over it.
    pub(crate) fn settle_row_choice(&mut self, cx: &mut Context<Self>) {
        let Some(choice) = self.pending_row_choice.clone() else {
            return;
        };
        if choice.intent != self.view_intent {
            self.pending_row_choice = None;
            return;
        }
        match self.change_presence(&choice.checkout) {
            ChangePresence::Present => {
                self.pending_row_choice = None;
                self.inspector_tab = InspectorTab::Changes;
                self.open_changes(&choice.checkout, None, cx);
            }
            ChangePresence::Absent => {
                self.pending_row_choice = None;
                // Nothing central is open, so the next frame hands the keyboard to
                // the terminal rather than this background answer taking it.
                self.focus_pending = true;
            }
            ChangePresence::Failed => {
                self.pending_row_choice = None;
                let error = self
                    .git_status_errors
                    .get(&choice.checkout)
                    .cloned()
                    .unwrap_or_default();
                self.push_error(
                    format!("Could not read the changes of this worktree: {error}"),
                    cx,
                );
            }
            // Another checkout's read answered, or this one is still on its way.
            ChangePresence::Unknown => {}
        }
    }
}

impl TerminalWindow {
    /// Open the worktree a sidebar row names.
    ///
    /// One action for the row and for the button drawn over its label, because
    /// they are the same click on different pixels: a row is one target with a
    /// focus ring, a tooltip and a count in it, not two targets that happen to
    /// agree. Whether the checkout has changes is Git's answer and not the row's
    /// `+N -N`, which a binary change writes as `+0 -0`; a checkout with a change
    /// set opens it, a known-clean one shows its terminal, and clicking a terminal
    /// always shows that terminal. When Git has not answered yet, neither answer
    /// is available, and the click waits for that read rather than claiming a
    /// checkout is clean because nothing has been counted.
    pub(crate) fn select_checkout_row(
        &mut self,
        checkout: &str,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        let checkout = checkout.to_owned();
        self.request_document_transition(
            Box::new(move |app, window, cx| app.select_checkout_row_now(&checkout, window, cx)),
            window,
            cx,
        );
    }

    fn select_checkout_row_now(
        &mut self,
        checkout: &str,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        if !self.navigation.select_checkout(checkout) {
            return;
        }
        self.clear_document();
        self.open_diff = None;
        self.persist();
        self.refresh_diff_totals(cx);
        match self.change_presence(checkout) {
            ChangePresence::Present => {
                self.inspector_tab = InspectorTab::Changes;
                self.open_changes(checkout, None, cx);
            }
            ChangePresence::Absent => self.focus_terminal(window, cx),
            // Asking for the status the row decision needs is the coalesced,
            // background read the Changes tab already asks for, so a click on a
            // checkout nobody has read yet costs Git a read and not a process per
            // row.
            ChangePresence::Unknown | ChangePresence::Failed => {
                self.pending_row_choice = Some(RowChoice {
                    checkout: checkout.to_owned(),
                    intent: self.view_intent,
                });
                self.request_git_status(checkout, cx);
            }
        }
        cx.notify();
    }

    pub(crate) fn request_open_changes(
        &mut self,
        checkout: &str,
        path: Option<String>,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        let checkout = checkout.to_owned();
        // The search over the sessions a review can go to is opened with the change
        // set: it is part of the panel that is about to be on screen.
        self.open_opencode_search(window, cx);
        self.request_document_transition(
            Box::new(move |app, _, cx| app.open_changes(&checkout, path, cx)),
            window,
            cx,
        );
    }

    pub(crate) fn open_changes(
        &mut self,
        checkout: &str,
        path: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.clear_document();
        self.open_diff = Some((checkout.to_owned(), path.clone()));
        // The pane is on a change from here on, and the row that says so is
        // named after it.
        self.view_pin = Some(ViewPin {
            checkout_id: checkout.to_owned(),
            kind: ViewKind::Diff,
            path: path.clone(),
        });
        // A choice raised about one change set is not a choice about another.
        self.review.opencode_choice = None;
        self.restore_opencode_review(checkout, cx);
        self.sync_opencode_sessions(cx);
        self.prune_diff_cache();
        self.focus_pending = false;
        self.open_crumb = None;
        if let Some(path) = path {
            self.request_file_diff(checkout, &path, cx);
        } else {
            self.request_git_status(checkout, cx);
        }
        cx.notify();
    }

    /// A file in the change set is opened: ask Git for its diff and remember
    /// that it is open.
    ///
    /// The diff's own chevron and a click anywhere on the file's header row are
    /// the same gesture on different pixels, so they share this rather than
    /// keeping one state transition each. A single file's diff is already read
    /// before it is drawn, so this is the change set's path.
    pub(crate) fn expand_diff_file(&mut self, checkout: &str, path: &str, cx: &mut Context<Self>) {
        self.expanded_diffs
            .insert((checkout.to_owned(), path.to_owned()));
        self.request_file_diff(checkout, path, cx);
    }

    /// A file in the change set is closed again, which forgets its diff and the
    /// queue slot that was reading it.
    pub(crate) fn collapse_diff_file(&mut self, checkout: &str, path: &str) {
        let key = (checkout.to_owned(), path.to_owned());
        self.expanded_diffs.remove(&key);
        self.diff_queue.forget(&key);
        self.git_diff_request.remove(&key);
        self.git_diff.remove(&key);
        self.git_diff_errors.remove(&key);
    }

    pub(crate) fn prune_diff_cache(&mut self) {
        let owner = self.open_diff.as_ref();
        let expanded = &self.expanded_diffs;
        let wanted = |key: &(String, String)| {
            owner.is_some_and(|(checkout, path)| {
                &key.0 == checkout
                    && match path {
                        Some(path) => &key.1 == path,
                        None => expanded.contains(key),
                    }
            })
        };
        self.git_diff.retain(|key, _| wanted(key));
        self.git_diff_request.retain(|key, _| wanted(key));
        self.git_diff_errors.retain(|key, _| wanted(key));
    }

    pub(crate) fn diff_element(
        &mut self,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let Some(owner) = self.open_diff.clone() else {
            self.diff_view = None;
            self.prune_diff_cache();
            return None;
        };
        let (checkout, selected) = &owner;
        if self.navigation.active_checkout.as_ref() != Some(checkout) {
            return None;
        }
        let roles = Roles::of(cx);
        // The source of the file whose change is on screen, when there is one to
        // read. A whole change set names no file, so it names no source either.
        let source = selected.as_deref().and_then(|path| {
            if !on_disk(&self.git_status_status(checkout, path)) {
                return None;
            }
            let (repo, checkout) = self.document_source(checkout)?;
            Some((repo, checkout, path.to_owned()))
        });
        let paths = if let Some(path) = selected {
            vec![path.clone()]
        } else if let Some(status) = self.git_status.get(checkout) {
            status.files.iter().map(|file| file.path.clone()).collect()
        } else {
            Vec::new()
        };
        let revisions = paths
            .iter()
            .map(|path| {
                let key = (checkout.clone(), path.clone());
                let revision = if let Some(diff) = self.git_diff.get(&key) {
                    diff.revision.clone()
                } else if let Some(error) = self.git_diff_errors.get(&key) {
                    format!("error:{error}")
                } else if self.git_diff_request.contains_key(&key) {
                    "loading".into()
                } else {
                    "unloaded".into()
                };
                (path.clone(), revision)
            })
            .collect::<Vec<_>>();
        let mut notices = std::collections::HashMap::new();
        for path in &paths {
            let key = (checkout.clone(), path.clone());
            if selected.is_none() && !self.expanded_diffs.contains(&key) {
                continue;
            }
            let (message, retry) = if let Some(error) = self.git_diff_errors.get(&key) {
                (Some(error.clone()), true)
            } else if let Some(diff) = self.git_diff.get(&key) {
                let message = if diff.too_large {
                    Some("Diff exceeds safe preview limits (16 MiB, 100,000 rows, 10,000 hunks or 64 KiB per line).".into())
                } else if let Some(target) = &diff.symlink_target {
                    Some(format!("Symlink target: {target}"))
                } else if diff.is_binary {
                    Some(binary_notice(
                        diff.image,
                        &self.git_status_status(checkout, path),
                    ))
                } else if diff.patch.is_empty() {
                    Some("No net text changes against the default branch.".into())
                } else {
                    None
                };
                (message, false)
            } else if self.git_diff_request.contains_key(&key) {
                (Some("Loading diff…".into()), false)
            } else {
                (
                    Some("The diff queue is full. Close an expanded file, then try again.".into()),
                    true,
                )
            };
            if let Some(message) = message {
                notices.insert(path.clone(), (message, retry));
            }
        }
        let rebuild = self
            .diff_view
            .as_ref()
            .is_none_or(|view| view.owner != owner || view.revisions != revisions);
        if rebuild {
            let mut files = Vec::with_capacity(paths.len());
            let mut parse_errors = std::collections::HashMap::new();
            let mut ready = std::collections::HashSet::new();
            for path in &paths {
                let parsed = self
                    .git_diff
                    .get(&(checkout.clone(), path.clone()))
                    .filter(|diff| !diff.patch.is_empty() && !diff.too_large && !diff.is_binary)
                    .map(|diff| DiffFile::parse(&diff.patch));
                let file = match parsed {
                    Some(Ok(mut parsed)) if !parsed.is_empty() => {
                        ready.insert(path.clone());
                        parsed.remove(0)
                    }
                    Some(Err(error)) => {
                        parse_errors.insert(path.clone(), format!("Cannot parse diff: {error}"));
                        placeholder(path)
                    }
                    Some(Ok(_)) => {
                        parse_errors.insert(
                            path.clone(),
                            "Cannot parse diff: unrecognized patch format.".into(),
                        );
                        placeholder(path)
                    }
                    None => placeholder(path),
                };
                files.push(file.with_language(crate::document::syntax_language(path)));
            }
            if let Some(view) = self.diff_view.as_mut().filter(|view| view.owner == owner) {
                view.state.update(cx, |state, cx| {
                    let selection = state.selected_lines().filter(|range| {
                        view.revisions
                            .iter()
                            .find(|(path, _)| path == range.path().as_str())
                            == revisions
                                .iter()
                                .find(|(path, _)| path == range.path().as_str())
                    });
                    state.set_files(files, cx);
                    if let Some(range) = selection {
                        let position = gpui_kit::component::diff::DiffLinePosition::new(
                            range.path().clone(),
                            range.side(),
                            range.start(),
                        );
                        state.set_selected_lines(Some(range), cx);
                        state.scroll_to_line(position, cx);
                    }
                });
                view.revisions = revisions;
                view.parse_errors = parse_errors;
                view.ready = ready;
            } else {
                let state = cx.new(|cx| DiffState::new(files, cx));
                state.read(cx).focus_handle(cx).focus(window, cx);
                let id = checkout.clone();
                let all = selected.is_none();
                let events = cx.subscribe(&state, move |app, _, event, cx| {
                    if !all {
                        return;
                    }
                    match event {
                        DiffEvent::FileExpanded(path) => app.expand_diff_file(&id, path, cx),
                        DiffEvent::FileCollapsed(path) => app.collapse_diff_file(&id, path),
                        _ => {}
                    }
                    cx.notify();
                });
                self.diff_view = Some(DiffView {
                    owner: owner.clone(),
                    revisions,
                    state,
                    parse_errors,
                    ready,
                    _events: events,
                });
                // A fresh diff state has nothing selected in it, so a note that
                // is still being written says which lines it covers again.
                self.sync_review_note_selection(cx);
            }
        }
        let view = self.diff_view.as_ref()?;
        notices.extend(
            view.parse_errors
                .iter()
                .filter(|(path, _)| {
                    selected.is_some()
                        || self
                            .expanded_diffs
                            .contains(&(checkout.clone(), (*path).clone()))
                })
                .map(|(path, error)| (path.clone(), (error.clone(), false))),
        );
        let state = view.state.clone();
        let ready_paths = view.ready.clone();
        // Sync initial/programmatic expansion without generating user events.
        state.update(cx, |state, cx| {
            for path in &paths {
                state.set_file_collapsed(
                    path,
                    !ready_paths.contains(path)
                        || (selected.is_none()
                            && !self
                                .expanded_diffs
                                .contains(&(checkout.clone(), path.clone()))),
                    cx,
                );
            }
        });
        let weak = cx.weak_entity();
        // Each rendered row owns the weak handle for the action it offers.
        let header_weak = weak.clone();
        let add_note_weak = weak.clone();
        let annotation_weak = weak.clone();
        let notes_for_checkout = self
            .review
            .notes
            .get(checkout)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|note| paths.contains(&note.path))
            .collect::<Vec<_>>();
        self.review
            .anchor_hashes
            .retain(|(cached_checkout, path), _| {
                cached_checkout == checkout && paths.contains(path)
            });
        let mut note_lines = std::collections::HashMap::new();
        let mut complete_diffs = std::collections::HashMap::new();
        for note in &notes_for_checkout {
            if note_lines.contains_key(&note.path) {
                continue;
            }
            let key = (checkout.clone(), note.path.clone());
            if !ready_paths.contains(&note.path) {
                self.review.anchor_hashes.remove(&key);
                complete_diffs.insert(note.path.clone(), false);
                continue;
            }
            let Some(diff) = self.git_diff.get(&key) else {
                self.review.anchor_hashes.remove(&key);
                complete_diffs.insert(note.path.clone(), false);
                continue;
            };
            let complete = !diff.too_large && !diff.is_binary && diff.symlink_target.is_none();
            complete_diffs.insert(note.path.clone(), complete);
            note_lines.insert(
                note.path.clone(),
                crate::review::cached_anchor_hashes(
                    &mut self.review,
                    checkout,
                    &note.path,
                    &diff.revision,
                    &diff.patch,
                    &notes_for_checkout,
                ),
            );
        }
        if let Some(notes) = self.review.notes.get_mut(checkout) {
            for note in notes.iter_mut().filter(|note| paths.contains(&note.path)) {
                if let Some(current_hash) = note_lines
                    .get(&note.path)
                    .and_then(|hashes| hashes.get(&(note.side, note.line_start)))
                    .and_then(Option::as_ref)
                {
                    if current_hash != &note.code_hash {
                        note.outdated = true;
                    }
                }
            }
        }
        let notes_for_checkout = self
            .review
            .notes
            .get(checkout)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|note| paths.contains(&note.path))
            .collect::<Vec<_>>();
        let mut note_views = std::collections::HashMap::new();
        let mut annotations = notices
            .keys()
            .map(|path| DiffAnnotation::file(format!("diff-notice-{path}"), path.clone()))
            .collect::<Vec<_>>();
        for note in notes_for_checkout {
            let position = DiffLinePosition::new(
                note.path.clone(),
                match note.side {
                    ReviewSide::Old => DiffSide::Original,
                    ReviewSide::New => DiffSide::Modified,
                },
                note.line_end.unwrap_or(note.line_start),
            );
            let outcome = review::anchor_outcome_hash(
                &note,
                note_lines
                    .get(&note.path)
                    .and_then(|hashes| hashes.get(&(note.side, note.line_start)))
                    .and_then(Option::as_deref),
                complete_diffs.get(&note.path).copied().unwrap_or(false),
            );
            let annotation_id = format!("review-note-{}", note.id);
            if matches!(outcome, AnchorOutcome::Missing | AnchorOutcome::Unknown) {
                // A line annotation has no row to attach to when an anchor has disappeared.
                // Keep its controls reachable at file scope instead of silently losing the note.
                annotations.push(DiffAnnotation::file(
                    annotation_id.clone(),
                    note.path.clone(),
                ));
            } else {
                annotations.push(DiffAnnotation::line(annotation_id.clone(), position));
            }
            note_views.insert(format!("review-note-{}", note.id), (note, outcome));
        }
        let composer =
            self.review.composer.as_ref().filter(|composer| {
                composer.checkout_id == *checkout && paths.contains(&composer.path)
            });
        let composer_input = composer.map(|composer| composer.input.clone());
        let composer_picker = composer.map(|composer| composer.start_picker.clone());
        let composer_can_save =
            composer.is_some_and(|composer| !composer.input.read(cx).value().trim().is_empty());
        let composer_start = composer.map(|composer| composer.line_start).unwrap_or(1);
        let composer_side = composer
            .map(|composer| composer.side.as_str())
            .unwrap_or("new");
        if let Some(composer) = composer {
            // The card hangs on the line the `+` was clicked on rather than the one
            // the range starts on, so choosing another start moves the wash over
            // the lines instead of dragging the box up the diff with it.
            annotations.push(DiffAnnotation::line(
                "review-composer",
                DiffLinePosition::new(
                    composer.path.clone(),
                    match composer.anchor.0 {
                        ReviewSide::Old => DiffSide::Original,
                        ReviewSide::New => DiffSide::Modified,
                    },
                    composer.anchor.1,
                ),
            ));
        }
        let checkout_id = checkout.clone();
        // Every note in the round, and not only the ones on screen: a save sends
        // the review, not the file that happens to be open in it.
        let note_count = self.review.notes_for(checkout, None);
        let exporting_review = self.review.exporting;
        // The file the last save wrote, when it holds this checkout's notes. The
        // toolbar names it rather than a toast saying it once: the row is where
        // the Save button is, so it is where somebody looks for the answer.
        let exported = self
            .review
            .exported
            .as_ref()
            .filter(|export| export.checkout_id == *checkout)
            .map(|export| export.path.display().to_string());
        let has_pending_opencode =
            !crate::review::can_start_opencode_round(&self.review.opencode_rounds);
        let include_outdated = self.review.include_outdated;
        let destination_busy = self.review.destination_busy;
        let opencode_loading = self.review.opencode_loading;
        let opencode_selected = matches!(
            self.review.destination,
            Some(
                muster_core::opencode::Destination::Opencode { .. }
                    | muster_core::opencode::Destination::OpencodeNew
            )
        );
        // Pi is a destination too, and it is not a round: it opens a terminal
        // rather than delivering a message, so nothing about it is recorded.
        let pi_selected = self.review.destination == Some(muster_core::opencode::Destination::Pi);
        let agent_selected = opencode_selected || pi_selected;
        let destination_error = agent_selected
            .then(|| self.review.destination_error.clone())
            .flatten();
        let opencode_rounds = self.review.opencode_rounds.clone();
        let opencode_choices = self.opencode_session_choices();
        let opencode_total = self.review.opencode_sessions.len();
        let opencode_target_label = match &self.review.destination {
            Some(muster_core::opencode::Destination::Opencode { session_id }) => self
                .review
                .opencode_sessions
                .iter()
                .find(|session| &session.id == session_id)
                .map(|session| session.display_title().to_string())
                .unwrap_or_else(|| "Selected session".into()),
            Some(muster_core::opencode::Destination::OpencodeNew) => "New session".into(),
            _ => String::new(),
        };
        let opencode_search = self.review.opencode_search.clone();
        let opencode_status = opencode_selected
            .then(|| opencode_rounds.last())
            .flatten()
            .map(|round| {
                format!(
                    "Review · {}",
                    crate::review::opencode_round_state(round.status)
                )
            });
        let opencode_session = match (&self.review.destination, opencode_selected) {
            (Some(muster_core::opencode::Destination::Opencode { session_id }), true) => {
                Some(session_id.clone())
            }
            _ => None,
        };
        let has_outdated = self
            .review
            .notes
            .get(checkout)
            .is_some_and(|notes| notes.iter().any(|note| note.outdated));
        let id = checkout.clone();
        // The header row answers for the diff's own chevron, so it needs the
        // checkout the change set belongs to as well as the file.
        let header_id = checkout.clone();
        let stats = self
            .git_status
            .get(checkout)
            .map(|status| {
                status
                    .files
                    .iter()
                    .map(|file| {
                        (
                            file.path.clone(),
                            (file.status.clone(), file.additions, file.deletions),
                        )
                    })
                    .collect::<std::collections::HashMap<_, _>>()
            })
            .unwrap_or_default();
        let viewer = Diff::new(&state)
            .header_visible(selected.is_none())
            // The pane is as wide as the pane, and no wider: a long line wraps
            // rather than dragging a sideways scroller under the code, so the
            // header row stays where it is — the letter, the path and the counts
            // answer what changed without scrolling after them.
            .soft_wrap(true)
            .font_family(UI_FONT_FAMILY)
            .text_size(px(DIFF_CHROME_PX))
            .render_header(move |file, _, _| {
                let (status, added, removed) =
                    stats.get(file.path().as_str()).cloned().unwrap_or_default();
                // Legacy's column order: the letter that says what Git did,
                // then the path, then the counts.
                let marker = crate::inspector::change_marker(&status);
                let path = file.path().to_string();
                let toggle = ToggleDiffFile {
                    state: state.clone(),
                    weak: header_weak.clone(),
                    checkout: header_id.clone(),
                    path: path.clone(),
                };
                div()
                    .id(format!("diff-file-{path}"))
                    .test_support()
                    // The row is the file's collapse control, the way the
                    // chevron beside it is: one gesture and one meaning, on
                    // every pixel of the row rather than on sixteen of them.
                    .w_full()
                    .flex_1()
                    .flex()
                    .items_center()
                    .on_click(move |_, _, cx| toggle.apply(cx))
                    .min_w_0()
                    .gap(px(6.))
                    .when(!marker.is_empty(), |row| {
                        row.child(
                            div()
                                .id(format!("diff-status-{}", file.path()))
                                .test_support()
                                .flex_shrink_0()
                                .text_size(px(11.))
                                .text_color(match marker.as_str() {
                                    "A" => roles.status.success,
                                    "D" => roles.status.danger_text,
                                    _ => roles.text_muted,
                                })
                                .child(marker),
                        )
                    })
                    .child(
                        div()
                            .id(format!("diff-path-{}", file.path()))
                            .test_support()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.))
                            .child(file.path().clone()),
                    )
                    .when_some(added.filter(|n| *n != 0), |row, n| {
                        row.child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(11.))
                                .text_color(roles.status.success)
                                .child(format!("+{n}")),
                        )
                    })
                    .when_some(removed.filter(|n| *n != 0), |row, n| {
                        row.child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(11.))
                                .text_color(roles.status.danger_text)
                                .child(format!("−{n}")),
                        )
                    })
            })
            .annotations(annotations)
            .on_add_annotation(move |range, window, cx| {
                let _ = add_note_weak.update(cx, |app, cx| {
                    app.begin_review_note(range, window, cx);
                });
            })
            .render_annotation(move |annotation, _window, _cx| {
                let annotation_id = annotation.id().to_string();
                if annotation_id == "review-composer" {
                    let (Some(input), Some(picker)) =
                        (composer_input.as_ref(), composer_picker.as_ref())
                    else {
                        return div().into_any_element();
                    };
                    let save = annotation_weak.clone();
                    let cancel = annotation_weak.clone();
                    let start = composer_start;
                    let side = composer_side;
                    let cancel_escape = annotation_weak.clone();
                    return div()
                        .id("review-composer-card")
                        .test_support()
                        .font_family(UI_FONT_FAMILY)
                        .text_size(px(12.))
                        // The toolkit draws every annotation inside a padded box.
                        // The composer is the one annotation that is a row of the
                        // diff rather than a card laid over it, so it takes that
                        // padding back — twelve pixels is the wrapper's own
                        // `px_3 py_3` — and sits flush between the lines above and
                        // below it.
                        .m_neg_3()
                        .border_1()
                        .border_color(roles.border)
                        .bg(roles.surface)
                        // What is inside the box is Legacy's: the field has room
                        // to write in and the buttons are not glued to it.
                        .p_2()
                        .on_key_down(move |event, _, cx| {
                            if event.keystroke.key.eq_ignore_ascii_case("escape") {
                                let _ =
                                    cancel_escape.update(cx, |app, cx| app.cancel_review_note(cx));
                                cx.stop_propagation();
                            }
                        })
                        .child(
                            // Keep the range label and its start picker together.
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(px(11.))
                                .text_color(roles.text_muted)
                                .child(
                                    div()
                                        .id("review-composer-range")
                                        .test_support()
                                        .min_w_0()
                                        .flex_shrink_0()
                                        .child(format!("{side} line {start}")),
                                )
                                .child(
                                    // The select fills whatever the wrapper is,
                                    // so the row's own width is what makes it
                                    // compact rather than a whole-field control.
                                    div().w(px(56.)).flex_shrink_0().child(
                                        Select::new(picker)
                                            .id("review-range-start")
                                            .xsmall()
                                            .bg(roles.surface_base)
                                            .menu_width(px(96.))
                                            .accessibility_label("First line of the range"),
                                    ),
                                ),
                        )
                        .child(
                            // Legacy's field: a field of its own that turns the
                            // accent blue while it has focus, which is what
                            // `.review-note-input:focus` did there. The state
                            // grows it from one row to six as the note is written.
                            Textarea::new(input)
                                .w_full()
                                .mt_2()
                                .bg(roles.surface_base)
                                .px_2()
                                .py_1()
                                .font_family(UI_FONT_FAMILY)
                                .text_size(px(12.))
                                .aria_label("Review note")
                                .into_any_element(),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .mt_2()
                                .flex_row_reverse()
                                .child(
                                    Button::new("save-review-note")
                                        .primary()
                                        .xsmall()
                                        .label("Save")
                                        .disabled(!composer_can_save)
                                        .on_click(move |_, _, cx| {
                                            let _ =
                                                save.update(cx, |app, cx| app.save_review_note(cx));
                                        }),
                                )
                                .child(
                                    Button::new("cancel-review-note")
                                        .ghost()
                                        .xsmall()
                                        .label("Cancel")
                                        .on_click(move |_, _, cx| {
                                            let _ = cancel
                                                .update(cx, |app, cx| app.cancel_review_note(cx));
                                        }),
                                ),
                        )
                        .into_any_element();
                }
                if let Some((note, outcome)) = note_views.get(&annotation_id) {
                    let note_id = note.id.clone();
                    let checkout_id = checkout_id.clone();
                    let edit = annotation_weak.clone();
                    let accept = annotation_weak.clone();
                    let delete = annotation_weak.clone();
                    // What a saved note can be asked to do goes where every other row
                    // of this app keeps its actions: at the right-hand end, on the
                    // note's own header row. They are icons rather than labelled
                    // buttons, so the note reads as the card's text and not as its
                    // footer, and each one names itself for the tooltip and for a
                    // screen reader that cannot see the drawing.
                    let mut card = div()
                        .id(format!("review-card-{}", note.id))
                        .test_support()
                        .px_3()
                        .py_2()
                        .bg(roles.surface)
                        .text_size(px(11.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(roles.text_muted)
                                        .child(format!(
                                            "{}:{}{}",
                                            note.side.as_str(),
                                            note.line_start,
                                            note.line_end
                                                .filter(|end| *end != note.line_start)
                                                .map(|end| format!("-{end}"))
                                                .unwrap_or_default()
                                        )),
                                )
                                .child({
                                    let id = note_id.clone();
                                    Button::new(format!("edit-review-{}", note.id))
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Pencil)
                                        .accessibility_label("Edit this note")
                                        .resting_tooltip("Edit this note")
                                        .on_click(move |_, window, cx| {
                                            let _ = edit.update(cx, |app, cx| {
                                                app.edit_review_note(&id, window, cx);
                                            });
                                        })
                                })
                                .child({
                                    let (checkout, id) = (checkout_id.clone(), note_id.clone());
                                    Button::new(format!("delete-review-{}", note.id))
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::X)
                                        .accessibility_label("Delete this note")
                                        .resting_tooltip("Delete this note")
                                        .on_click(move |_, _, cx| {
                                            let _ = delete.update(cx, |app, cx| {
                                                app.request_delete_review_note(&checkout, &id, cx);
                                            });
                                        })
                                }),
                        )
                        .child(div().py_1().child(note.content.clone()));
                    if note.outdated {
                        card = card.child(div().text_color(roles.status.warning).child(
                            "This anchor changed; it is excluded from export unless included.",
                        ));
                    } else if let Some(message) = match outcome {
                        AnchorOutcome::Changed => Some("This anchor changed."),
                        AnchorOutcome::Missing => Some("This line is no longer in the diff."),
                        AnchorOutcome::Unknown => Some("This part of the diff is not loaded."),
                        AnchorOutcome::Unchanged => None,
                    } {
                        card = card.child(div().text_color(roles.text_muted).child(message));
                    }
                    // Accepting a drifted anchor is a decision about the note rather
                    // than a thing to do to it, so it stays a labelled action below
                    // the note and is drawn only when there is an anchor to accept.
                    if note.outdated {
                        let id = note_id.clone();
                        let checkout = checkout_id.clone();
                        card = card.child(
                            div().pt_1().child(
                                Button::new(format!("accept-review-{}", note.id))
                                    .ghost()
                                    .small()
                                    .label("Accept")
                                    .resting_tooltip("Keep this note on the lines it names")
                                    .on_click(move |_, _, cx| {
                                        let _ = accept.update(cx, |app, cx| {
                                            app.accept_outdated_note(&checkout, &id, cx);
                                        });
                                    }),
                            ),
                        );
                    }
                    return card.into_any_element();
                }
                let path = annotation.path().to_string();
                let (message, retry) = notices.get(&path).cloned().unwrap_or_default();
                let mut row = div().px_3().py_2().child(
                    div()
                        .id(format!("diff-notice-text-{path}"))
                        .test_support()
                        .text_size(px(f32::from(crate::terminal::FONT_SIZE)))
                        .text_color(roles.text_muted)
                        .child(message),
                );
                if retry {
                    let weak = annotation_weak.clone();
                    let id = id.clone();
                    row = row.child(
                        Button::new(format!("retry-diff-{path}"))
                            .small()
                            .label("Retry")
                            .on_click(move |_, _, cx| {
                                let _ = weak
                                    .update(cx, |app, cx| app.request_file_diff(&id, &path, cx));
                            }),
                    );
                }
                row.into_any_element()
            })
            .size_full()
            .bg(roles.content)
            .text_color(roles.content_text);
        let body = if paths.is_empty() {
            let message = self
                .git_status_errors
                .get(checkout)
                .cloned()
                .unwrap_or_else(|| {
                    if self.git_status.contains_key(checkout) {
                        "No changed files.".into()
                    } else {
                        "Loading changes…".into()
                    }
                });
            let mut empty = div().p_3().child(message);
            if self.git_status_errors.contains_key(checkout) {
                let weak = cx.weak_entity();
                let checkout = checkout.clone();
                empty = empty.child(
                    Button::new("retry-change-set")
                        .small()
                        .label("Retry")
                        .on_click(move |_, _, cx| {
                            let _ =
                                weak.update(cx, |app, cx| app.request_git_status(&checkout, cx));
                        }),
                );
            }
            empty.into_any_element()
        } else {
            viewer.into_any_element()
        };
        let weak = cx.weak_entity();
        let close = weak.clone();
        Some(
            div()
                .size_full()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .bg(roles.surface_base)
                .text_color(roles.text)
                .child(
                    div()
                        .id("review-toolbar")
                        .test_support()
                        .flex().flex_col().gap_1().px_3().py_1()
                        .flex_none().min_w_0()
                        .text_size(px(12.))
                        .border_b_1().border_color(roles.border)
                        .child(
                            div().id("review-title-row").test_support()
                                .flex().items_center().gap_2().min_w_0()
                                .child(
                                    div().flex_1().min_w_0().flex().items_center().gap_2()
                                        .child(
                                            div().min_w_0().truncate()
                                                .text_size(px(DIFF_CHROME_PX))
                                                .text_color(roles.text_muted)
                                                .child(selected.clone().unwrap_or_else(|| "All changes".into())),
                                        )
                                        // Legacy's document toolbar: the path, and the
                                        // icon that puts it on the clipboard beside it.
                                        // The copy is of the very path the title names,
                                        // and a whole change set names no file, so it
                                        // has nothing to copy.
                                        .when_some(selected.clone(), |row, path| {
                                            let copy = path.clone();
                                            row.child(
                                                Button::new("copy-diff-path")
                                                    .text().cursor_default().small().compact().h(px(20.)).flex_none()
                                                    .icon(IconName::Copy)
                                                    .accessibility_label(format!("Copy the path of {path}"))
                                                    .resting_tooltip(format!("Copy the path of {path}"))
                                                    .on_click(move |_, _, cx| {
                                                        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(copy.clone()));
                                                    }),
                                            )
                                        }),
                                )
                                // The action and the destination chooser are one
                                // control: two halves of the same button, joined edge
                                // to edge and rounded only at the outer corners, so
                                // the chooser reads as part of the action rather than
                                // as a second thing to press beside it. The inner
                                // corners are named here because a split has to say
                                // which corners it keeps, and the outer one is the
                                // theme's own control radius.
                                .child({
                                    let send_action = weak.clone();
                                    let pi_action = weak.clone();
                                    let export_action = weak.clone();
                                    let choose = weak.clone();
                                    let busy = exporting_review || destination_busy || opencode_loading;
                                    let label = if pi_selected {
                                        "Send to Pi"
                                    } else if opencode_selected {
                                        "Send to OpenCode"
                                    } else {
                                        "Save"
                                    };
                                    div().id("review-export").test_support().flex().flex_none()
                                        .child(
                                            Button::new("review-primary-action")
                                                .primary()
                                                .small().compact()
                                                .rounded_l(px(4.)).rounded_r_none()
                                                // Markdown is the default destination, so the
                                                // action says what it does and the chooser
                                                // beside it says where it goes. Naming the
                                                // format here would make the default read as a
                                                // special case of itself.
                                                .label(label)
                                                .accessibility_label(label)
                                                .disabled(note_count == 0 || busy || (opencode_selected && has_pending_opencode))
                                                .when(opencode_selected, |button| button.on_click(move |_, window, cx| { let _ = send_action.update(cx, |app, cx| app.send_review_to_opencode(window, cx)); }))
                                                .when(pi_selected, |button| button.on_click(move |_, window, cx| { let _ = pi_action.update(cx, |app, cx| app.send_review_to_pi(window, cx)); }))
                                                .when(!agent_selected, |button| button.on_click(move |_, window, cx| { let _ = export_action.update(cx, |app, cx| app.export_review(window, cx)); }))
                                        )
                                        .child(
                                            Button::new("review-destination")
                                                .primary()
                                                .small().compact()
                                                .rounded_r(px(4.)).rounded_l_none()
                                                .dropdown_caret(true)
                                                .accessibility_label("Review destination")
                                                .disabled(busy)
                                                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                                                    let markdown = choose.clone();
                                                    let opencode = choose.clone();
                                                    let pi = choose.clone();
                                                    menu.item(PopupMenuItem::new("Markdown").on_click(move |_, _, cx| {
                                                        let _ = markdown.update(cx, |app, cx| app.set_destination(muster_core::opencode::Destination::Markdown, cx));
                                                    })).item(PopupMenuItem::new("OpenCode").on_click(move |_, _, cx| {
                                                        let _ = opencode.update(cx, |app, cx| {
                                                            app.choose_opencode_destination(cx);
                                                        });
                                                    })).item(PopupMenuItem::new("Pi").on_click(move |_, _, cx| {
                                                        let _ = pi.update(cx, |app, cx| app.set_destination(muster_core::opencode::Destination::Pi, cx));
                                                    }))
                                                })
                                        )
                                })
                                .when_some(source, |header, (repo, source_checkout, path)| {
                                    let weak = weak.clone();
                                    header.child(Button::new(format!("diff-open-source-{path}"))
                                        .text().cursor_default().small().compact().h(px(24.))
                                        .label("Source").accessibility_label(format!("Show the source of {path}"))
                                        .resting_tooltip("Show this file's source")
                                        .on_click(move |_, window, cx| {
                                            let (repo, checkout, path) = (repo.clone(), source_checkout.clone(), path.clone());
                                            let _ = weak.update(cx, |app, cx| app.request_document(repo, checkout, path, window, cx));
                                        }))
                                })
                                .child(Button::new("close-diff")
                                    .ghost()
                                    .small().compact()
                                    .icon(IconName::X).accessibility_label("Close preview").resting_tooltip("Close preview")
                                    .on_click(move |_, window, cx| {
                                        let _ = close.update(cx, |app, cx| {
                                            app.open_diff = None;
                                            app.diff_view = None;
                                            app.prune_diff_cache();
                                            // A close is a close: the row that
                                            // named this view goes with it.
                                            app.view_pin = None;
                                            app.focus_terminal(window, cx);
                                            cx.notify();
                                        });
                                    })),
                        )
                        .child(div().id("review-secondary-row").test_support()
                            .flex().items_center().gap_2().min_w_0().text_size(px(11.))
                            .when(note_count > 0, |row| row.child(div().id("review-note-count").test_support()
                                .text_color(roles.text_muted).child(format!("{note_count} {}", if note_count == 1 { "note" } else { "notes" }))))
                            .when(has_outdated, |row| {
                                let toggle = weak.clone();
                                row.child(Checkbox::new("review-outdated")
                                    .checked(include_outdated).label("Include outdated notes")
                                    .on_change(move |_, _, cx| { let _ = toggle.update(cx, |app, cx| app.toggle_include_outdated(cx)); }))
                            })
                            .when_some(exported, |row, path| {
                                let copy = path.clone();
                                row.child(div().flex().items_center().gap_1().min_w_0()
                                    .child(div().id("review-exported-path").test_support()
                                        // Bounded, and shrinkable below that bound: a home
                                        // directory is longer than this row is wide, and a
                                        // path that pushes the rest of the row off the pane
                                        // is a path nobody could read. The icon beside it
                                        // never shrinks, so the path is what gives way.
                                        .max_w(px(320.)).min_w_0().truncate()
                                        .child(path.clone()))
                                    // Legacy's document toolbar: the path, and the icon that
                                    // puts it on the clipboard beside it.
                                    .child(Button::new("copy-review-export-path")
                                        // A text button draws no box of its own: the
                                        // toolkit's size ladder reaches the button
                                        // variants that fill one, and not this one.
                                        // The height is the target this control
                                        // offers, so it is named here.
                                        .text().cursor_default().small().compact().h(px(20.))
                                        .icon(IconName::Copy)
                                        .accessibility_label("Copy the saved review's path")
                                        .resting_tooltip(path.clone())
                                        .on_click(move |_, _, cx| {
                                            cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(copy.clone()));
                                        })))
                            })
                            .when(opencode_selected, |row| {
                                let choose = weak.clone();
                                let choices = opencode_choices.clone();
                                // The search comes first: it is what narrows the
                                // list the chooser below offers. It is only here
                                // when the list is long enough to need one.
                                let searched = row
                                    .when(opencode_total > crate::agent_watch::SEARCH_THRESHOLD, |row| {
                                        row.when_some(opencode_search.clone(), |row, search| {
                                            row.child(
                                                gpui_kit::component::input::Input::new(&search)
                                                    .id("review-opencode-search")
                                                    .small()
                                                    .bg(roles.surface_base)
                                                    .aria_label("Search OpenCode sessions")
                                                    .w(px(150.)),
                                            )
                                        })
                                    });
                                searched.child(
                                    Button::new("review-opencode-session")
                                        .xsmall()
                                        .label(opencode_target_label.clone())
                                        .accessibility_label("OpenCode session")
                                        .resting_tooltip(opencode_target_label.clone())
                                        .dropdown_caret(true)
                                        .dropdown_menu_with_anchor(
                                            Anchor::TopRight,
                                            move |menu, _, _| {
                                                let mut menu = menu.min_w(px(260.));
                                                for session in &choices {
                                                    let id = session.id.clone();
                                                    let title =
                                                        session.display_title().to_string();
                                                    let weak = choose.clone();
                                                    menu = menu.item(
                                                        PopupMenuItem::new(title).on_click(
                                                            move |_, _, cx| {
                                                                let _ = weak.update(
                                                                    cx,
                                                                    |app, cx| {
                                                                        app.choose_opencode_session(
                                                                            Some(id.clone()),
                                                                            cx,
                                                                        );
                                                                    },
                                                                );
                                                            },
                                                        ),
                                                    );
                                                }
                                                let weak = choose.clone();
                                                menu.item(
                                                    PopupMenuItem::new("New session").on_click(
                                                        move |_, _, cx| {
                                                            let _ = weak.update(cx, |app, cx| {
                                                                app.choose_opencode_session(
                                                                    None, cx,
                                                                );
                                                            });
                                                        },
                                                    ),
                                                )
                                            },
                                        ),
                                )
                            })
                            .when_some(opencode_session, |row, session| row.child(Button::new("copy-opencode-session-id")
                                .text().cursor_default().small().compact().h(px(20.)).label("Copy session ID").resting_tooltip(session.clone())
                                .accessibility_label("Copy the app-created OpenCode session id")
                                .on_click(move |_, _, cx| cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(session.clone())))))
                            .when_some(opencode_status, |row, status| row.child(div().id("opencode-review-status").test_support()
                                .text_size(px(11.)).text_color(roles.text_muted).child(status)))
                            .when_some(destination_error, |row, error| row.child(div().id("opencode-review-error").test_support()
                                .text_size(px(11.)).text_color(roles.status.danger_text).child(error)))
                            .when_some(opencode_rounds.iter().find(|round| opencode_selected && (round.status.is_pending()
                                || round.status == muster_core::opencode::RoundStatus::Unknown))
                                .map(|round| (round.id.clone(), round.status)), |row, (round_id, status)| {
                                    let reconcile = weak.clone();
                                    let reconcile_id = round_id.clone();
                                    let weak = weak.clone();
                                    let button = Button::new("resolve-opencode-round").small().compact();
                                    let button = match status {
                                        muster_core::opencode::RoundStatus::Queued => button.label("Retry queued review").on_click(move |_, window, cx| {
                                            let _ = weak.update(cx, |app, cx| app.retry_queued_opencode_rounds(window, cx));
                                        }),
                                        muster_core::opencode::RoundStatus::Unknown => button.label("Allow retry (may duplicate)")
                                            .resting_tooltip("This review may already have been delivered. Sending it again may deliver it twice.")
                                            .on_click(move |_, window, cx| {
                                                let round_id = round_id.clone();
                                                let _ = weak.update(cx, |app, cx| app.resend_unknown_opencode_round(&round_id, window, cx));
                                            }),
                                        _ => {
                                            let round_id = round_id.clone();
                                            button.label("Reconcile uncertain send").on_click(move |_, window, cx| {
                                                let _ = weak.update(cx, |app, cx| app.reconcile_opencode_round(&round_id, window, cx));
                                            })
                                        }
                                    };
                                    row.child(button.disabled(destination_busy || opencode_loading))
                                        .when(status == muster_core::opencode::RoundStatus::Unknown, |row| row.child(
                                            Button::new("recheck-unknown-opencode-round").small().compact().label("Check delivery again")
                                                .disabled(destination_busy || opencode_loading)
                                                .on_click(move |_, window, cx| {
                                                    let _ = reconcile.update(cx, |app, cx| app.reconcile_opencode_round(&reconcile_id, window, cx));
                                                })))
                                })),
                        )
                .child(div().flex_1().min_h_0().child(body))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{binary_notice, on_disk};
    use muster_core::navigation::ImageFormat;

    /// A change that removed the file is the one change with nothing behind its
    /// path, and the letter saying so is the letter the row already wears.
    #[test]
    fn a_removed_file_has_no_source_to_offer() {
        for status in ["D", " D", "MD", "AD", "DD"] {
            assert!(!on_disk(status), "{status}");
        }
        for status in [
            "", "M", " M", "A", "AM", "??", "R", "R ", "C", "T", "MM", "UU",
        ] {
            assert!(on_disk(status), "{status}");
        }
    }

    /// A change with no text names the change, and the letter Git already put on
    /// the row is what says which change it is.
    #[test]
    fn a_change_with_no_text_says_which_one_it_is() {
        assert_eq!(
            binary_notice(Some(ImageFormat::Png), "??"),
            "Image added · png."
        );
        assert_eq!(
            binary_notice(Some(ImageFormat::Png), "A"),
            "Image added · png."
        );
        assert_eq!(
            binary_notice(Some(ImageFormat::Jpeg), "D"),
            "Image removed · jpeg."
        );
        assert_eq!(
            binary_notice(Some(ImageFormat::WebP), "M"),
            "Image changed · webp."
        );
        // Not a picture: the notice keeps saying what it is, because that is the
        // only thing it knows.
        assert_eq!(
            binary_notice(None, "M"),
            "Binary file; text diff is unavailable."
        );
    }

    /// A file whose bytes named no picture is not named by its extension, so the
    /// wording that adds, removes and changes an image is reachable only for an
    /// image, and a non-image says the same thing whichever letter Git gave it.
    #[test]
    fn a_file_with_no_picture_says_the_same_thing_whatever_changed() {
        for status in ["??", "A", "D", "M", "R", "MM"] {
            assert_eq!(
                binary_notice(None, status),
                "Binary file; text diff is unavailable.",
                "{status}"
            );
        }
    }
}
