//! Creating a worktree, and taking one away.
//!
//! Two dialogs over the same subject: a worktree that does not exist yet, and
//! one that does. They share their shape because they share their hazard — Git
//! cannot be rolled back once it has been asked — so both refuse to close while
//! their mutation is running, and both say what is about to happen before they
//! do it.
//!
//! A draft is the dialog's own state: the fields it is asking about, what Git
//! has answered so far, and the failure that keeps it open. It is not a
//! worktree and not a checkout; it is the conversation, and it is discarded
//! whole when the conversation ends.

use std::path::PathBuf;

use gpui_kit::component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    dialog::{Dialog, DialogContent, DialogFooter},
    input::{Input, InputState},
    Disableable,
};
use gpui_kit::{
    div, prelude::FluentBuilder, px, AnyElement, Context, Entity, IntoElement, ParentElement,
    Styled, Subscription,
};
use muster_core::navigation::WorktreeRemovalInfo;
use muster_core::workspace::RepoKind;

use crate::app::TerminalWindow;
use crate::theme::Roles;

/// The one worktree this window is removing, if it is removing one.
pub(crate) struct WorktreeRemovalDraft {
    pub(crate) request_id: u64,
    pub(crate) repo_id: String,
    pub(crate) checkout_id: String,
    pub(crate) info: Option<WorktreeRemovalInfo>,
    pub(crate) loading: bool,
    pub(crate) busy: bool,
    pub(crate) dirty_confirmed: bool,
    pub(crate) unmerged_confirmed: bool,
    pub(crate) delete_branch: bool,
    pub(crate) error: Option<String>,
}

/// The one worktree this window is creating, if it is creating one.
///
/// Its own struct rather than a field soup because the dialog is a modal
/// conversation: the three inputs, whether the branch is still the suggestion,
/// what Git answered about the starting point, and the failure that keeps the
/// dialog open. The mutation runs on a worker and cannot be rolled back, so
/// `busy` also refuses the dialog's own Cancel.
pub(crate) struct WorktreeDraft {
    pub(crate) request_id: u64,
    pub(crate) repo_id: String,
    pub(crate) task: Entity<InputState>,
    pub(crate) branch: Entity<InputState>,
    /// The branch this worktree starts from, prefilled when Git named one.
    ///
    /// Not optional because a repository whose default branch Git will not
    /// guess is ordinary enough to meet on the first run, and the answer to
    /// that is a field to fill rather than a dead dialog. It is also the only
    /// answer creation uses: a field the person cleared is a starting point
    /// they refused, and a second, remembered answer would quietly override it.
    pub(crate) base: Entity<InputState>,
    /// Where worktrees of this repository go, once Git has answered.
    pub(crate) location: Option<PathBuf>,
    pub(crate) branch_edited: bool,
    /// Set while the task name is rewriting the branch, so a programmatic
    /// value is not read back as the person having taken the branch over.
    pub(crate) syncing_branch: bool,
    pub(crate) loading_defaults: bool,
    pub(crate) busy: bool,
    pub(crate) error: Option<String>,
    pub(crate) _subscriptions: Vec<Subscription>,
}

pub(crate) fn escape_git_path(path: &str) -> String {
    path.chars()
        .map(|character| {
            if character.is_control() {
                character.escape_default().to_string()
            } else {
                character.to_string()
            }
        })
        .collect()
}

/// Close a worktree dialog only when its non-cancellable Git mutation is idle.
pub(crate) fn close_worktree_dialog(app: &mut TerminalWindow, cx: &mut Context<TerminalWindow>) {
    if let Some(draft) = app.worktree.as_mut() {
        if draft.busy {
            draft.error =
                Some("This worktree is still being created. Wait for it to finish.".into());
        } else {
            app.worktree = None;
        }
    } else if let Some(draft) = app.worktree_removal.as_mut() {
        if draft.busy {
            draft.error = Some("Worktree deletion is in progress. Wait for Git to finish.".into());
        } else {
            app.worktree_removal = None;
        }
    }
    cx.notify();
}

/// The draft this window is creating, drawn as the conversation it is.
impl TerminalWindow {
    pub(crate) fn worktree_dialog(
        &self,
        draft: &WorktreeDraft,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let roles = Roles::of(cx);
        let ready = draft.location.is_some()
            && !draft.busy
            && !draft.loading_defaults
            && !draft.task.read(cx).value().trim().is_empty()
            && !draft.branch.read(cx).value().trim().is_empty()
            && !draft.base.read(cx).value().trim().is_empty();
        // The content builder is called again on every repaint, so it holds the
        // three input entities and builds the fields itself rather than closing
        // over elements it would have to hand out twice.
        let task = draft.task.clone();
        let branch = draft.branch.clone();
        let base = draft.base.clone();
        let origin = draft
            .location
            .as_ref()
            .map(|location| format!("Worktrees are created under {}", location.display()));
        let error = draft.error.clone();
        let loading_defaults = draft.loading_defaults;
        let context = self
            .navigation
            .repos
            .iter()
            .find(|repo| repo.id == draft.repo_id)
            .map(|repo| {
                let root = repo.checkouts.iter().find(|checkout| checkout.is_primary);
                let context = root
                    .map(|checkout| checkout.branch.as_deref().unwrap_or(&checkout.path))
                    .unwrap_or(&repo.root);
                format!("{context} · {}", repo.name)
            });
        let cancel_button = cx.weak_entity();
        let cancel_dialog = cx.weak_entity();
        let submit_button = cx.weak_entity();
        Dialog::new(cx)
            .bg(roles.surface_raised)
            .border_color(roles.border_strong)
            .width(px(440.))
            .title("Create worktree")
            .content(move |content: DialogContent, _, _| {
                let content = content.when_some(context.clone(), |content, context| {
                    content.child(
                        div()
                            .text_size(px(12.))
                            .text_color(roles.text_muted)
                            .child(context),
                    )
                });
                if loading_defaults {
                    return content.child(
                        div()
                            .py_4()
                            .text_size(px(12.))
                            .child("Checking Git worktree…"),
                    );
                }
                let mut content = content
                    .child(div().text_size(px(12.)).child("Task name"))
                    .child(
                        Input::new(&task)
                            .bg(roles.surface_base)
                            .aria_label("Task name")
                            .w_full(),
                    )
                    // Legacy says this under the field, and the native dialog
                    // had dropped it: the task name is the directory on disk,
                    // and a person who does not know that will not predict the
                    // path their worktree lands in.
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(roles.text_muted)
                            .child("Used as directory name; unsafe characters are cleaned by Git."),
                    )
                    .child(div().mt_3().text_size(px(12.)).child("New branch"))
                    .child(
                        Input::new(&branch)
                            .bg(roles.surface_base)
                            .aria_label("New branch")
                            .w_full(),
                    )
                    .child(
                        div()
                            .mt_3()
                            .text_size(px(12.))
                            .child("Starting from branch"),
                    )
                    .child(
                        Input::new(&base)
                            .bg(roles.surface_base)
                            .aria_label("Starting from branch")
                            .w_full(),
                    );
                if let Some(origin) = origin.clone() {
                    content = content.child(
                        div()
                            .mt_2()
                            .text_size(px(11.))
                            .text_color(roles.text_muted)
                            .child(origin),
                    );
                }
                if let Some(error) = error.clone() {
                    content = content.child(
                        div()
                            .mt_2()
                            .text_size(px(12.))
                            .text_color(roles.status.danger_text)
                            .child(error),
                    );
                }
                content
            })
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new("worktree-cancel")
                            .custom(crate::app::sidebar_action_style(cx).foreground(roles.text))
                            .label("Cancel")
                            .on_click(move |_, _window, cx| {
                                let _ = cancel_button.update(cx, |app, cx| {
                                    close_worktree_dialog(app, cx);
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        Button::new("worktree-create")
                            .primary()
                            .label(if draft.busy {
                                "Creating…"
                            } else {
                                "Create and open shell"
                            })
                            .disabled(!ready)
                            .when(!ready, |button| {
                                button
                                    .bg(roles.disabled_surface)
                                    .text_color(roles.text_disabled)
                                    .border_color(gpui_kit::transparent_black())
                            })
                            .on_click(move |_, window, cx| {
                                let _ = submit_button
                                    .update(cx, |app, cx| app.submit_worktree(window, cx));
                            }),
                    ),
            )
            .on_cancel(move |_, _, cx| {
                let _ = cancel_dialog.update(cx, |app, cx| {
                    close_worktree_dialog(app, cx);
                    cx.notify();
                });
                false
            })
            .into_any_element()
    }

    pub(crate) fn worktree_removal_dialog(
        &self,
        draft: &WorktreeRemovalDraft,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let roles = Roles::of(cx);
        let id = draft.checkout_id.clone();
        let request_id = draft.request_id;
        let info = draft.info.clone();
        let loading = draft.loading;
        let busy = draft.busy;
        let dirty_confirmed = draft.dirty_confirmed;
        let unmerged_confirmed = draft.unmerged_confirmed;
        let delete_branch = draft.delete_branch;
        let error = draft.error.clone();
        let checkout = self.navigation.checkout(&id);
        let repo = self
            .navigation
            .repos
            .iter()
            .find(|repo| repo.id == draft.repo_id);
        let branch = info
            .as_ref()
            .and_then(|info| info.branch.as_deref())
            .or_else(|| checkout.and_then(|checkout| checkout.branch.as_deref()))
            .unwrap_or("detached HEAD");
        let label = format!(
            "{} · {}",
            branch,
            repo.map_or("Git repository", |repo| repo.name.as_str())
        );
        let path = info
            .as_ref()
            .map(|info| info.path.clone())
            .or_else(|| checkout.map(|checkout| checkout.canonical_path.clone()))
            .unwrap_or_else(|| id.clone());
        let valid_checkout = repo.is_some_and(|repo| repo.kind == RepoKind::Git)
            && checkout.is_some_and(|checkout| {
                !checkout.is_primary
                    && !checkout.is_missing
                    && self.navigation.home.as_deref() != Some(&id)
            });
        let session_names = checkout
            .map(|checkout| {
                checkout
                    .sessions
                    .iter()
                    .map(|session| session.name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let retained_terminals = self
            .terminals
            .iter()
            .filter(|(session_id, terminal)| {
                terminal.checkout_id == id && self.navigation.session(session_id).is_none()
            })
            .map(|(session_id, _)| format!("Terminal {session_id}"))
            .collect::<Vec<_>>();
        let pending_ids = self
            .navigation
            .pending
            .iter()
            .filter(|(_, pending)| *pending == &id)
            .map(|(request, _)| request.clone())
            .collect::<Vec<_>>();
        let has_terminal = self.checkout_has_terminal_activity(&id);
        let has_pending = !pending_ids.is_empty();
        let can_archive = !busy && valid_checkout && !has_terminal;
        let can_delete = !loading
            && !busy
            && valid_checkout
            && !has_terminal
            && info.as_ref().is_some_and(|info| {
                (info.dirty_files.is_empty() || dirty_confirmed)
                    && (info.branch.is_some() || info.unmerged_commits == 0 || unmerged_confirmed)
            });
        let shell_checkout_id = id.clone();
        let open_shell = cx.weak_entity();
        let toggle_dirty = cx.weak_entity();
        let toggle_unmerged = cx.weak_entity();
        let toggle_branch = cx.weak_entity();
        let archive = cx.weak_entity();
        let delete = cx.weak_entity();
        let close_for_cancel = cx.weak_entity();
        let close_for_escape = cx.weak_entity();
        Dialog::new(cx)
            .bg(roles.surface_raised)
            .border_color(roles.border_strong)
            .width(px(520.))
            .title("Archive or delete worktree")
            .content(move |mut content: DialogContent, _, _| {
                content = content
                    .child(
                        div()
                            .text_size(px(12.))
                            .child("Archive hides this worktree, keeps every file and branch, and does not run Git. Delete removes this selected worktree directory and its Git registration."),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_size(px(11.))
                            .text_color(roles.text_muted)
                            .child(format!("{label} · {}", escape_git_path(&path))),
                    );
                if loading {
                    content = content.child(
                        div()
                            .mt_3()
                            .text_size(px(12.))
                            .text_color(roles.text_muted)
                            .child("Checking Git worktree…"),
                    );
                }
                if let Some(info) = info.clone() {
                    if info.dirty_files.is_empty() {
                        content = content.child(
                            div()
                                .mt_3()
                                .text_size(px(12.))
                                .text_color(roles.text_muted)
                                .child("No uncommitted files."),
                        );
                    } else {
                        let toggle_dirty = toggle_dirty.clone();
                        let mut preview = div()
                            .mt_3()
                            .p_2()
                            .bg(roles.surface_inset)
                            .text_size(px(12.))
                            .child(format!(
                                "{} uncommitted file(s) may be lost if you delete this worktree:",
                                info.dirty_files.len()
                            ));
                        for file in info.dirty_files.iter().take(8) {
                            preview = preview.child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(roles.text_muted)
                                    .child(escape_git_path(file)),
                            );
                        }
                        if info.dirty_files.len() > 8 {
                            preview = preview.child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(roles.text_muted)
                                    .child(format!("and {} more…", info.dirty_files.len() - 8)),
                            );
                        }
                        let open_shell = open_shell.clone();
                        let shell_checkout_id = shell_checkout_id.clone();
                        content = content
                            .child(preview)
                            .child(Button::new("worktree-commit-shell")
                                .text()
                                .label("Open a shell to commit or stash first")
                                .disabled(busy)
                                .when(busy, |button| {
                                    button.bg(roles.disabled_surface)
                                        .text_color(roles.text_disabled)
                                        .border_color(gpui_kit::transparent_black())
                                })
                                .on_click(move |_, window, cx| {
                                    let _ = open_shell.update(cx, |app, cx| {
                                        close_worktree_dialog(app, cx);
                                        app.request_terminal(&shell_checkout_id, window, cx);
                                    });
                                }))
                            .child(
                                Checkbox::new("worktree-confirm-dirty")
                                    .checked(dirty_confirmed)
                                    .label("I understand uncommitted files may be lost; delete anyway.")
                                    .on_change(move |checked, _, cx| {
                                        let _ = toggle_dirty.update(cx, |app, cx| {
                                            if let Some(draft) = app
                                                .worktree_removal
                                                .as_mut()
                                                .filter(|draft| draft.request_id == request_id)
                                            {
                                                draft.dirty_confirmed = *checked;
                                                draft.error = None;
                                            }
                                            cx.notify();
                                        });
                                    }),
                            );
                    }
                    if info.unmerged_commits > 0 {
                        let warning = if info.branch.is_some() {
                            format!(
                                "Branch {} has {} unmerged commit(s) versus {}. The branch is kept unless you choose to delete it.",
                                info.branch.as_deref().unwrap_or_default(),
                                info.unmerged_commits,
                                info.default_branch.as_deref().unwrap_or("the default branch")
                            )
                        } else {
                            format!(
                                "Detached HEAD has {} commit(s) unreachable from branches, remotes, or tags. Deleting this worktree may lose them.",
                                info.unmerged_commits
                            )
                        };
                        content = content.child(
                            div()
                                .mt_2()
                                .p_2()
                                .bg(roles.surface_inset)
                                .text_size(px(12.))
                                .child(warning),
                        );
                        if info.branch.is_none() {
                            let toggle_unmerged = toggle_unmerged.clone();
                            content = content.child(
                                Checkbox::new("worktree-confirm-unmerged")
                                    .checked(unmerged_confirmed)
                                    .label("I understand these detached commits may be lost; delete anyway.")
                                    .on_change(move |checked, _, cx| {
                                        let _ = toggle_unmerged.update(cx, |app, cx| {
                                            if let Some(draft) = app
                                                .worktree_removal
                                                .as_mut()
                                                .filter(|draft| draft.request_id == request_id)
                                            {
                                                draft.unmerged_confirmed = *checked;
                                                draft.error = None;
                                            }
                                            cx.notify();
                                        });
                                    }),
                            );
                        }
                    }
                    if let Some(branch) = info.branch.as_deref() {
                        let toggle_branch = toggle_branch.clone();
                        content = content.child(
                            Checkbox::new("worktree-delete-branch")
                                .checked(delete_branch)
                                .label(format!(
                                    "Delete local branch {}{}",
                                    branch,
                                    if info.unmerged_commits > 0 {
                                        format!(" and its {} unmerged commit(s)", info.unmerged_commits)
                                    } else {
                                        String::new()
                                    }
                                ))
                                .on_change(move |checked, _, cx| {
                                    let _ = toggle_branch.update(cx, |app, cx| {
                                        if let Some(draft) = app
                                            .worktree_removal
                                            .as_mut()
                                            .filter(|draft| draft.request_id == request_id)
                                        {
                                            draft.delete_branch = *checked;
                                            draft.error = None;
                                        }
                                        cx.notify();
                                    });
                                }),
                        );
                    }
                }
                if has_terminal {
                    let mut sessions = div()
                        .mt_2()
                        .text_size(px(12.))
                        .text_color(roles.status.warning)
                        .child("Close these terminal sessions before archiving or deleting:");
                    for name in &session_names {
                        sessions = sessions.child(div().text_size(px(11.)).child(name.clone()));
                    }
                    for name in &retained_terminals {
                        sessions = sessions.child(div().text_size(px(11.)).child(name.clone()));
                    }
                    content = content.child(sessions);
                }
                if has_pending {
                    let mut pending = div()
                        .text_size(px(12.))
                        .text_color(roles.status.warning)
                        .child("Wait for terminal creation to finish before continuing:");
                    for request in &pending_ids {
                        pending = pending.child(div().text_size(px(11.)).child(request.clone()));
                    }
                    content = content.child(pending);
                }
                if let Some(error) = error.clone() {
                    content = content.child(
                        div()
                            .mt_2()
                            .text_size(px(12.))
                            .text_color(roles.status.danger_text)
                            .child(error),
                    );
                }
                content
            })
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new("worktree-removal-cancel")
                            .custom(crate::app::sidebar_action_style(cx).foreground(roles.text))
                            .label("Cancel")
                            .disabled(busy)
                            .when(busy, |button| {
                                button.bg(roles.disabled_surface)
                                    .text_color(roles.text_disabled)
                                    .border_color(gpui_kit::transparent_black())
                            })
                            .on_click(move |_, _, cx| {
                                let _ = close_for_cancel.update(cx, |app, cx| {
                                    close_worktree_dialog(app, cx);
                                });
                            }),
                    )
                    .child(
                        Button::new("worktree-archive")
                            .custom(crate::app::sidebar_action_style(cx).foreground(roles.text))
                            .label(if busy { "Archiving…" } else { "Archive" })
                            .disabled(!can_archive)
                            .when(!can_archive, |button| {
                                button.bg(roles.disabled_surface)
                                    .text_color(roles.text_disabled)
                                    .border_color(gpui_kit::transparent_black())
                            })
                            .on_click(move |_, _, cx| {
                                let _ = archive.update(cx, |app, cx| app.submit_worktree_archive(cx));
                            }),
                    )
                    .child(
                        Button::new("worktree-delete")
                            .danger()
                            .border_1()
                            .border_color(roles.status.danger)
                            .label(if busy { "Deleting…" } else { "Delete worktree" })
                            .disabled(!can_delete)
                            .when(!can_delete, |button| {
                                button.bg(roles.disabled_surface)
                                    .text_color(roles.text_disabled)
                                    .border_color(gpui_kit::transparent_black())
                            })
                            .on_click(move |_, window, cx| {
                                let _ = delete.update(cx, |app, cx| {
                                    app.submit_worktree_delete(window, cx)
                                });
                            }),
                    ),
            )
            .on_cancel(move |_, _, cx| {
                let _ = close_for_escape.update(cx, close_worktree_dialog);
                false
            })
            .into_any_element()
    }
}
