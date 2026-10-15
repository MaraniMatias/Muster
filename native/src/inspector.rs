//! Lazy checkout Files and Git Changes inspector.
use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::Icon;
use gpui_kit::component::Sizable;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{div, px, AnyElement, Context, IntoElement, ParentElement, Styled};
use muster_core::workspace::{Checkout, Repo, RepoKind};

use crate::{
    app::{ButtonA11y, InspectorTab, TerminalWindow},
    theme::Roles,
};

fn file_marker(status: &str) -> &'static str {
    match status {
        "UU" | "AA" | "DD" | "DU" | "UD" | "AU" | "UA" => "U",
        _ if status.contains('A') => "A",
        _ if status.contains('D') => "D",
        _ if status.contains('M') => "M",
        _ => "",
    }
}

fn inspector_row_style(cx: &gpui_kit::App, selected: bool) -> ButtonCustomVariant {
    let roles = Roles::of(cx);
    ButtonCustomVariant::new(cx)
        .color(roles.row_rest(selected))
        .foreground(roles.text)
        .hover(roles.row_hover(selected))
        .active(roles.pressed)
}

enum ChangeRow {
    Directory(String),
    File(muster_core::navigation::ChangedFile),
}

#[derive(Clone)]
struct FileRow {
    entry: muster_core::files::FileEntry,
    depth: usize,
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

fn flatten(
    directory: &str,
    depth: usize,
    checkout: &str,
    entries: &std::collections::HashMap<(String, String), Vec<muster_core::files::FileEntry>>,
    expanded: &std::collections::HashSet<(String, String)>,
    rows: &mut Vec<FileRow>,
) {
    if let Some(children) = entries.get(&(checkout.to_owned(), directory.to_owned())) {
        for entry in children {
            rows.push(FileRow {
                entry: entry.clone(),
                depth,
            });
            if entry.directory && expanded.contains(&(checkout.to_owned(), entry.path.clone())) {
                flatten(&entry.path, depth + 1, checkout, entries, expanded, rows);
            }
        }
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
                        app.file_entries.insert(key.clone(), entries);
                        app.expanded_files
                            .retain(|expanded| expanded.0 != key.0 || expanded == &key);
                        app.file_errors.remove(&key);
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
                        app.git_status.insert(key.clone(), status);
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
                        app.git_status_errors.insert(key.clone(), error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn request_file_diff(&mut self, id: &str, path: &str, cx: &mut Context<Self>) {
        self.queue_file_diff(id, path, false, cx);
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
        let mut panel = div()
            .w(px(self.inspector_width))
            .flex_none()
            .h_full()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(roles.surface)
            .text_size(px(11.))
            .border_l_1()
            .border_color(roles.border);
        let files_tab = self.inspector_tab == InspectorTab::Files || repo.kind != RepoKind::Git;
        panel = panel.child(
            div()
                .flex()
                .gap_2()
                .px_3()
                .py_2()
                .child(
                    Button::new("inspector-files")
                        .custom(inspector_row_style(cx, files_tab))
                        .small()
                        .rounded(px(0.))
                        .flex_1()
                        .label("Files")
                        .aria_selected(files_tab)
                        .accessibility_label("Files")
                        .on_click(cx.listener(|app, _, _, cx| {
                            app.inspector_tab = InspectorTab::Files;
                            cx.notify();
                        })),
                )
                .when(repo.kind == RepoKind::Git, |tabs| {
                    let id = checkout_id.clone();
                    let count = self.git_status.get(&id).map(|s| s.files.len()).unwrap_or(0);
                    tabs.child(
                        Button::new("inspector-changes")
                            .custom(inspector_row_style(cx, !files_tab))
                            .small()
                            .rounded(px(0.))
                            .flex_1()
                            .label(if count == 0 {
                                "Changes".to_owned()
                            } else {
                                format!("Changes {count}")
                            })
                            .aria_selected(!files_tab)
                            .accessibility_label("Changes")
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.inspector_tab = InspectorTab::Changes;
                                app.request_git_status(&id, cx);
                                cx.notify();
                            })),
                    )
                }),
        );
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
            let mut body = div().flex_1().min_h_0().flex().flex_col().child(
                Button::new("all-changes")
                    .custom(inspector_row_style(cx, all_selected))
                    .small()
                    .rounded(px(0.))
                    .w_full()
                    .justify_start()
                    .aria_selected(all_selected)
                    .label("All changes")
                    .on_click(cx.listener(move |app, _, _, cx| app.open_changes(&id, None, cx))),
            );
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
                        Button::new("retry-status")
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
                    let list =
                        gpui_kit::uniform_list("changes-list", rows.len(), move |range, _, cx| {
                            range
                                .map(|index| match &rows[index] {
                                    ChangeRow::Directory(directory) => div()
                                        .h(px(26.))
                                        .px_2()
                                        .py_1()
                                        .text_color(roles.text_muted)
                                        .child(directory.clone())
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
                                        let status_ink = match file_marker(&file.status) {
                                            "A" => roles.status.added,
                                            "D" => roles.status.removed,
                                            "M" | "U" => roles.status.warning,
                                            _ => roles.text_muted,
                                        };
                                        Button::new(format!("change-{path}"))
                                            .custom(inspector_row_style(cx, selected))
                                            .small()
                                            .rounded(px(0.))
                                            .h(px(26.))
                                            .w_full()
                                            .min_w_0()
                                            .justify_start()
                                            .aria_selected(selected)
                                            .accessibility_label(path.clone())
                                            .tooltip(
                                                file.old_path
                                                    .as_ref()
                                                    .map(|old| format!("{old} → {path}"))
                                                    .unwrap_or_else(|| path.clone()),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap_2()
                                                    .w_full()
                                                    .min_w_0()
                                                    .child(
                                                        div()
                                                            .text_color(status_ink)
                                                            .child(file.status.clone()),
                                                    )
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .min_w_0()
                                                            .truncate()
                                                            .child(name),
                                                    )
                                                    .when_some(
                                                        file.additions.filter(|n| *n != 0),
                                                        |row, n| {
                                                            row.child(
                                                                div()
                                                                    .text_color(roles.status.added)
                                                                    .child(format!("+{n}")),
                                                            )
                                                        },
                                                    )
                                                    .when_some(
                                                        file.deletions.filter(|n| *n != 0),
                                                        |row, n| {
                                                            row.child(
                                                                div()
                                                                    .text_color(
                                                                        roles.status.removed,
                                                                    )
                                                                    .child(format!("−{n}")),
                                                            )
                                                        },
                                                    ),
                                            )
                                            .on_click(move |_, _, cx| {
                                                let _ = weak.update(cx, |app, cx| {
                                                    app.open_changes(&id, Some(path.clone()), cx)
                                                });
                                            })
                                            .into_any_element()
                                    }
                                })
                                .collect::<Vec<_>>()
                        })
                        .flex_1()
                        .min_h_0()
                        .w_full();
                    body = body.child(list);
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
        let statuses = self
            .git_status
            .get(&checkout_id)
            .map(|s| {
                s.files
                    .iter()
                    .map(|f| (f.path.clone(), f.status.clone()))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let expanded = self.expanded_files.clone();
        let selected = self.selected_file.get(&checkout_id).cloned();
        let mut rows = Vec::new();
        flatten(
            "",
            0,
            &checkout_id,
            &self.file_entries,
            &self.expanded_files,
            &mut rows,
        );
        let total = rows.len();
        let weak = cx.weak_entity();
        // One row per failed read, each with the operation that failed: a
        // directory that could not be listed is asked for again as a listing,
        // and a file that could not be read is read again, because listing a
        // file as a directory cannot answer either.
        let mut failures = Vec::new();
        for ((owner, path), error) in &self.file_errors {
            if owner == &checkout_id {
                failures.push((path.clone(), error.clone(), false));
            }
        }
        failures.sort();
        let loading = self.file_loading.clone();
        let repo_for = repo.clone();
        let checkout_for = checkout.clone();
        let list = gpui_kit::uniform_list("files-list", total, move |range, _, cx| {
            range
                .map(|index| {
                    let row = rows[index].clone();
                    let entry = row.entry.clone();
                    let path = entry.path.clone();
                    let directory = entry.directory;
                    let open = expanded.contains(&(checkout_for.id.clone(), path.clone()));
                    let status = statuses.get(&path).map(String::as_str).unwrap_or("");
                    let marker = file_marker(status);
                    let label = if marker.is_empty() {
                        entry.name.clone()
                    } else {
                        format!("{}  {marker}", entry.name)
                    };
                    let icon = if directory {
                        if open {
                            IconName::FolderOpen
                        } else {
                            IconName::Folder
                        }
                    } else {
                        IconName::FileText
                    };
                    let selected = selected.as_deref() == Some(path.as_str());
                    let weak = weak.clone();
                    let repo = repo_for.clone();
                    let checkout = checkout_for.clone();
                    Button::new(format!("file:{path}"))
                        .custom(inspector_row_style(cx, selected))
                        .small()
                        .rounded(px(0.))
                        .h(px(26.))
                        .w_full()
                        .justify_start()
                        .pl(px(8. + row.depth as f32 * 14.))
                        .text_color(if entry.name.starts_with('.') {
                            roles.text_muted
                        } else {
                            roles.text
                        })
                        .icon(Icon::new(icon).size_4().text_color(if directory {
                            roles.text_muted
                        } else {
                            roles.file_icon(&path)
                        }))
                        .aria_selected(selected)
                        .when(directory, |button| button.aria_expanded(open))
                        .tooltip(path.clone())
                        .label(label)
                        .on_click(move |_, window, cx| {
                            let _ = weak.update(cx, |app, cx| {
                                if directory {
                                    let key = (checkout.id.clone(), path.clone());
                                    if app.expanded_files.remove(&key) {
                                        // A collapsed directory keeps nothing
                                        // but its name: the listing is dropped
                                        // with it, so a tree walked wide and
                                        // closed again does not stay resident.
                                        app.file_request.remove(&key);
                                        app.file_loading.remove(&key);
                                        app.file_entries.remove(&key);
                                    } else {
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
                                    }
                                } else {
                                    app.selected_file.insert(checkout.id.clone(), path.clone());
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
                        })
                        .into_any_element()
                })
                .collect::<Vec<_>>()
        });
        let root = (checkout_id.clone(), String::new());
        let mut body = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(list.flex_1().min_h_0().w_full());
        for (path, error, was_read) in &failures {
            let was_read = *was_read;
            let name = if path.is_empty() {
                "root".to_owned()
            } else {
                path.clone()
            };
            body = body.child(
                div()
                    .px_3()
                    .py_1()
                    .text_color(roles.status.warning)
                    .child(format!("{name}/: {error}")),
            );
            let rp = path.clone();
            let rrepo = repo.clone();
            let rcheckout = checkout.clone();
            let verb = if was_read { "reading" } else { "listing" };
            body = body.child(
                Button::new(format!(
                    "retry-{}-{name}",
                    if was_read { "read" } else { "list" }
                ))
                .ghost()
                .small()
                .label(format!("Retry {name}/"))
                .accessibility_label(format!("Retry {verb} {name}/"))
                .on_click(cx.listener(move |app, _, window, cx| {
                    if was_read {
                        app.request_document(
                            rrepo.clone(),
                            rcheckout.clone(),
                            rp.clone(),
                            window,
                            cx,
                        );
                    } else {
                        app.request_directory(
                            rrepo.clone(),
                            rcheckout.clone(),
                            rp.clone(),
                            window,
                            cx,
                        );
                    }
                })),
            );
        }
        if loading.contains(&root) {
            body = body.child(
                div()
                    .px_3()
                    .py_1()
                    .text_color(roles.text_muted)
                    .child("Loading files…"),
            );
        }
        if self.file_entries.get(&root).is_some_and(Vec::is_empty)
            && !loading.contains(&root)
            && !self.file_errors.contains_key(&root)
        {
            body = body.child(div().px_3().child("No files"));
        }
        for (owner, path) in loading {
            if owner == checkout_id && !path.is_empty() {
                body = body.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_color(roles.text_muted)
                        .child(format!("Loading {path}…")),
                );
            }
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
