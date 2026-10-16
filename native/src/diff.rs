//! Retained, virtualized unified code review surface.
use crate::{
    app::{InspectorTab, TerminalWindow, SIDEBAR_FONT_FAMILY},
    theme::Roles,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::diff::{Diff, DiffAnnotation, DiffEvent, DiffFile, DiffState};
use gpui_kit::component::Sizable;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    div, px, AnyElement, AppContext, Context, Entity, Focusable, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled, Subscription, TestSupportExt,
};
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

    pub(crate) fn open_changes(
        &mut self,
        checkout: &str,
        path: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.clear_document();
        self.open_diff = Some((checkout.to_owned(), path.clone()));
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
        // Sync initial/programmatic expansion without generating user events.
        state.update(cx, |state, cx| {
            for path in &paths {
                state.set_file_collapsed(
                    path,
                    !view.ready.contains(path)
                        || (selected.is_none()
                            && !self
                                .expanded_diffs
                                .contains(&(checkout.clone(), path.clone()))),
                    cx,
                );
            }
        });
        let weak = cx.weak_entity();
        // Each header row and each notice row carries its own click target, so
        // the window is reached through one clone each.
        let header_weak = weak.clone();
        let annotations = notices
            .keys()
            .map(|path| DiffAnnotation::file(format!("diff-notice-{path}"), path.clone()))
            .collect::<Vec<_>>();
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
            .font_family(SIDEBAR_FONT_FAMILY)
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
                    .cursor_pointer()
                    .on_click(move |_, _, cx| toggle.apply(cx))
                    .min_w_0()
                    .gap(px(6.))
                    .when(!marker.is_empty(), |row| {
                        row.child(
                            div()
                                .id(format!("diff-status-{}", file.path()))
                                .test_support()
                                .flex_shrink_0()
                                .text_size(px(10.))
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
            .render_annotation(move |annotation, _, _| {
                let path = annotation.path().to_string();
                let (message, retry) = notices.get(&path).cloned().unwrap_or_default();
                let mut row = div().px_3().py_2().child(
                    div()
                        .id(format!("diff-notice-text-{path}"))
                        .test_support()
                        // The notice is prose, but it is read where the change
                        // is, and the diff refines its own root down to the
                        // chrome size: a notice naming no size of its own is the
                        // smallest letter in the window.
                        .text_size(px(f32::from(crate::terminal::FONT_SIZE)))
                        .text_color(roles.text_muted)
                        .child(message),
                );
                if retry {
                    let weak = weak.clone();
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
                row
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
                        .flex()
                        .items_center()
                        .gap_3()
                        .px_3()
                        .h(px(40.))
                        .flex_none()
                        .border_b_1()
                        .border_color(roles.border)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(DIFF_CHROME_PX))
                                .text_color(roles.text_muted)
                                .child(selected.clone().unwrap_or_else(|| "All changes".into())),
                        )
                        .when_some(source, |header, (repo, source_checkout, path)| {
                            let weak = weak.clone();
                            header.child(
                                Button::new(format!("diff-open-source-{path}"))
                                    .text()
                                    .small()
                                    .compact()
                                    .rounded(px(0.))
                                    .label("Source")
                                    .accessibility_label(format!("Show the source of {path}"))
                                    .tooltip("Show this file's source")
                                    // The same one transition the other way round
                                    // is: this pane is a document's changes, and the
                                    // source is a document of its own.
                                    .on_click(move |_, window, cx| {
                                        let (repo, checkout, path) =
                                            (repo.clone(), source_checkout.clone(), path.clone());
                                        let _ = weak.update(cx, |app, cx| {
                                            app.request_document(repo, checkout, path, window, cx)
                                        });
                                    }),
                            )
                        })
                        .child(
                            Button::new("close-diff")
                                .custom(crate::app::sidebar_action_style(cx))
                                .small()
                                .compact()
                                .rounded(px(0.))
                                .icon(IconName::X)
                                .accessibility_label("Close preview")
                                .tooltip("Close preview")
                                .on_click(move |_, window, cx| {
                                    let _ = weak.update(cx, |app, cx| {
                                        app.open_diff = None;
                                        app.diff_view = None;
                                        app.prune_diff_cache();
                                        app.focus_terminal(window, cx);
                                        cx.notify();
                                    });
                                }),
                        ),
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
