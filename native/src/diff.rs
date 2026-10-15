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
    ParentElement, Styled, Subscription, TestSupportExt,
};

pub(crate) const MAX_RETAINED_PATCH_BYTES: usize = 32 * 1024 * 1024;

/// The type a diff's own chrome is drawn at: Legacy's file header, one size for
/// the status letter, the path and the counts alike.
///
/// The toolkit draws that header at `text_sm()`, and the code below refines
/// over it, so this is the one place the size and the face are named. The code
/// rows and the `@@` band keep the sizes they set for themselves.
const DIFF_CHROME_PX: f32 = 11.;

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

fn placeholder(path: &str) -> DiffFile {
    let old = serde_json::to_string(&format!("a/{path}")).expect("a filename is JSON text");
    let new = serde_json::to_string(&format!("b/{path}")).expect("a filename is JSON text");
    DiffFile::parse(&format!("diff --git {old} {new}\n"))
        .expect("quoted file header")
        .remove(0)
}

impl TerminalWindow {
    /// Open the worktree a sidebar row names.
    ///
    /// One action for the row and for the button drawn over its label, because
    /// they are the same click on different pixels: a row is one target with a
    /// focus ring, a tooltip and a count in it, not two targets that happen to
    /// agree. A row carrying `+N -N` is advertising a diff, so a dirty worktree
    /// opens its whole change set. A clean one currently shows its terminal;
    /// per-checkout preview restoration remains a separate parity requirement.
    /// Clicking a terminal always shows that terminal.
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
        let dirty = self
            .diff_totals
            .get(checkout)
            .is_some_and(|totals| totals.additions > 0 || totals.deletions > 0);
        if dirty {
            self.inspector_tab = InspectorTab::Changes;
            self.open_changes(checkout, None, cx);
        } else {
            self.focus_terminal(window, cx);
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
                    Some("Binary file; text diff is unavailable.".into())
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
                        DiffEvent::FileExpanded(path) => {
                            app.expanded_diffs.insert((id.clone(), path.to_string()));
                            app.request_file_diff(&id, path, cx);
                        }
                        DiffEvent::FileCollapsed(path) => {
                            let key = (id.clone(), path.to_string());
                            app.expanded_diffs.remove(&key);
                            app.diff_queue.forget(&key);
                            app.git_diff_request.remove(&key);
                            app.git_diff.remove(&key);
                            app.git_diff_errors.remove(&key);
                        }
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
        let annotations = notices
            .keys()
            .map(|path| DiffAnnotation::file(format!("diff-notice-{path}"), path.clone()))
            .collect::<Vec<_>>();
        let id = checkout.clone();
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
            .font_family(SIDEBAR_FONT_FAMILY)
            .text_size(px(DIFF_CHROME_PX))
            .render_header(move |file, _, _| {
                let (status, added, removed) =
                    stats.get(file.path().as_str()).cloned().unwrap_or_default();
                // Legacy's column order: the letter that says what Git did,
                // then the path, then the counts.
                let marker = crate::inspector::change_marker(&status);
                div()
                    .flex()
                    .items_center()
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
                let mut row = div()
                    .px_3()
                    .py_2()
                    .text_color(roles.text_muted)
                    .child(message);
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
                        .py(px(6.))
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
