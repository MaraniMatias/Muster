use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{
    domain::{
        ipc::{IpcError, IpcErrorCode},
        workspace::{RecentPath, Repo, RepoKind},
    },
    git,
    persistence::{timestamp, Database},
    services::{folder, git as git_service, worktree},
};

#[derive(Clone)]
pub struct HomeDirectory(pub PathBuf);

pub fn register_folder(
    database: &Database,
    path: &Path,
) -> Result<crate::domain::workspace::WorkspaceState, IpcError> {
    let opened = folder::open_folder(path)?;
    let registration_snapshots = database
        .load_workspace()
        .map_err(operation_error)?
        .repos
        .into_iter()
        .filter(|repo| repo.kind == RepoKind::Git)
        .map(|repo| {
            database
                .git_repo_registration_snapshot(&repo.id)
                .map_err(operation_error)?
                .map(|snapshot| (repo.id, snapshot))
                .ok_or_else(|| {
                    IpcError::new(
                        IpcErrorCode::InvalidCheckout,
                        "workspace registrations changed; refresh and retry opening this folder",
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let now = timestamp();
    if let Some((repo, focus_checkout_id)) = git::resolve_repository(Path::new(&opened.path), &now)?
    {
        let expected = registration_snapshots
            .iter()
            .find(|(repo_id, _)| repo_id == &repo.id)
            .map(|(_, snapshot)| snapshot);
        if expected.is_some_and(|snapshot| snapshot.is_archived_checkout(&focus_checkout_id)) {
            return Err(IpcError::new(
                IpcErrorCode::InvalidCheckout,
                "this worktree is archived; restore it from Archived Worktrees before opening it",
            ));
        }
        database
            .register_git_repo_if_unchanged(&repo, &focus_checkout_id, expected)
            .map_err(operation_error)?
            .ok_or_else(|| {
                IpcError::new(
                    IpcErrorCode::InvalidCheckout,
                    "workspace registrations changed while opening this folder; refresh and retry",
                )
            })
    } else {
        let repo = Repo::plain(Path::new(&opened.path), now)
            .map_err(|error| IpcError::new(IpcErrorCode::InvalidPath, error))?;
        database
            .register_plain_repo(repo)
            .map_err(|error| IpcError::new(IpcErrorCode::OperationFailed, error))
    }
}

pub fn list_recent_paths(database: &Database) -> Result<Vec<RecentPath>, IpcError> {
    database.list_recent_paths().map_err(operation_error)
}

pub fn locate_missing_checkout(
    database: &Database,
    agents: &crate::services::agent::AgentService,
    checkout_id: &str,
    selected_path: &Path,
) -> Result<crate::domain::workspace::WorkspaceState, IpcError> {
    let selected_path = fs::canonicalize(selected_path).map_err(|error| {
        IpcError::new(
            IpcErrorCode::FolderMissing,
            format!("located directory is unavailable: {error}"),
        )
    })?;
    if !selected_path.is_dir() {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "located path is not a directory",
        ));
    }
    let initial = database.load_workspace().map_err(operation_error)?;
    let (repo, _) = crate::services::checkout::registered_checkout(
        &initial.repos,
        checkout_id,
        "checkout ID is not registered",
    )?;
    let repo_id = repo.id.clone();
    let checkout_ids = repo_agent_checkout_ids(&initial, &repo_id);
    let target_id =
        crate::domain::workspace::checkout_id_for_path(&selected_path.display().to_string());
    let mut operation_ids = checkout_ids.clone();
    operation_ids.push(target_id);
    operation_ids.sort();
    operation_ids.dedup();

    let Some((snapshot_repo, snapshot_checkout, registration_snapshot, generations)) =
        with_agent_checkout_operations(agents, &checkout_ids, || {
            let current = database.load_workspace().map_err(operation_error)?;
            let (repo, checkout) = crate::services::checkout::registered_checkout(
                &current.repos,
                checkout_id,
                "checkout ID is not registered",
            )?;
            if repo.id != repo_id || repo_agent_checkout_ids(&current, &repo_id) != checkout_ids {
                return Ok(None);
            }
            let registration_snapshot = if repo.kind == RepoKind::Git {
                let Some(snapshot) = database
                    .git_repo_registration_snapshot(&repo_id)
                    .map_err(operation_error)?
                else {
                    return Ok(None);
                };
                if snapshot.checkout_ids() != checkout_ids {
                    return Ok(None);
                }
                Some(snapshot)
            } else {
                None
            };
            let generations = checkout_ids
                .iter()
                .map(|id| {
                    agents
                        .checkout_generation(id)
                        .map(|generation| (id.clone(), generation))
                        .map_err(crate::services::agent::map_error)
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Some((
                repo.clone(),
                checkout.clone(),
                registration_snapshot,
                generations,
            )))
        })?
    else {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "checkout registrations changed; refresh and retry locating this directory",
        ));
    };
    let retargets_id = snapshot_checkout.is_missing
        && selected_path != Path::new(&snapshot_checkout.canonical_path)
        && !(snapshot_repo.kind == RepoKind::Git && snapshot_checkout.is_primary);
    let mut removal = if retargets_id {
        Some(reserve_idle_agent_checkout(agents, checkout_id)?)
    } else {
        None
    };

    with_agent_checkout_operations(agents, &operation_ids, || {
        let current = database.load_workspace().map_err(operation_error)?;
        let (repo, checkout) = crate::services::checkout::registered_checkout(
            &current.repos,
            checkout_id,
            "checkout ID is not registered",
        )?;
        if repo.root != snapshot_repo.root
            || repo.kind != snapshot_repo.kind
            || checkout.canonical_path != snapshot_checkout.canonical_path
            || checkout.is_missing != snapshot_checkout.is_missing
            || repo_agent_checkout_ids(&current, &repo_id) != checkout_ids
        {
            return Err(IpcError::new(
                IpcErrorCode::InvalidCheckout,
                "checkout registrations changed; refresh and retry locating this directory",
            ));
        }
        for (id, generation) in &generations {
            if agents
                .checkout_epoch(id)
                .map_err(crate::services::agent::map_error)?
                != *generation
            {
                return Err(IpcError::new(
                    IpcErrorCode::InvalidCheckout,
                    "checkout changed; refresh and retry locating this directory",
                ));
            }
        }
        locate_missing_checkout_locked(
            database,
            agents,
            repo,
            checkout,
            &selected_path,
            registration_snapshot.as_ref(),
            &mut removal,
        )
    })
}

/// Tells Git about a worktree that moved, so the path it records is the one on disk.
///
/// A mutation, and so the one Git in this file that runs with no deadline, for the reason
/// `git::run_git` gives: a repair ended part way through is administrative state nobody asked to be
/// left in. What it does take from the shared runner is the locale, so the diagnostic below is the
/// same sentence on every machine, and the error it fails with is bounded like every other one --
/// `git_error` summarizes what Git said instead of handing a helper's whole stderr to a message a
/// user reads.
fn repair_located_worktree(management_root: &Path, selected_path: &Path) -> Result<(), IpcError> {
    let repair = Command::new("git")
        .args(["worktree", "repair"])
        .arg(selected_path)
        .current_dir(management_root)
        .env("LC_ALL", "C")
        .env_remove("LANGUAGE")
        .output()
        .map_err(|error| {
            IpcError::new(
                IpcErrorCode::GitFailed,
                format!("could not repair the located worktree: {error}"),
            )
        })?;
    if !repair.status.success() {
        return Err(git_service::git_error(
            "could not repair the located worktree",
            &repair,
        ));
    }
    Ok(())
}

fn locate_missing_checkout_locked(
    database: &Database,
    agents: &crate::services::agent::AgentService,
    repo: &Repo,
    checkout: &crate::domain::workspace::Checkout,
    selected_path: &Path,
    registration_snapshot: Option<&crate::persistence::GitRepoRegistrationSnapshot>,
    removal: &mut Option<crate::services::agent::WorktreeRemovalGuard>,
) -> Result<crate::domain::workspace::WorkspaceState, IpcError> {
    if !selected_path.is_dir() {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "located path is not a directory",
        ));
    }
    if !checkout.is_missing {
        if selected_path != Path::new(&checkout.canonical_path) {
            return Err(IpcError::new(
                IpcErrorCode::InvalidCheckout,
                "only a missing checkout can be located",
            ));
        }
        return register_folder(database, selected_path);
    }
    if checkout
        .sessions
        .iter()
        .any(|session| session.status == crate::domain::workspace::SessionStatus::Active)
    {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "close active terminal sessions before relocating this checkout",
        ));
    }

    if repo.kind == RepoKind::Plain {
        if selected_path == Path::new(&checkout.canonical_path) {
            return register_folder(database, selected_path);
        }
        if git::resolve_repository(selected_path, &timestamp())?.is_some() {
            return Err(IpcError::new(
                IpcErrorCode::InvalidCheckout,
                "located directory is a Git checkout, not the missing plain directory",
            ));
        }
        let relocated = Repo::plain(selected_path, timestamp())
            .map_err(|error| IpcError::new(IpcErrorCode::InvalidPath, error))?;
        let Some(removal) = removal.take() else {
            return Err(IpcError::new(
                IpcErrorCode::InvalidCheckout,
                "checkout relocation reservation is missing; retry locating this directory",
            ));
        };
        let state = database
            .relocate_plain_checkout(&repo.id, &checkout.id, &relocated)
            .map_err(operation_error)?;
        agents.stop_for_worktree_removal(&checkout.id);
        removal.commit();
        return Ok(state);
    }

    if selected_path == Path::new(&checkout.canonical_path) {
        let (resolved, focus_id) = git::resolve_repository(selected_path, &timestamp())?
            .ok_or_else(|| {
                IpcError::new(
                    IpcErrorCode::NotRepository,
                    "located directory is not a Git checkout",
                )
            })?;
        if resolved.id != repo.id || focus_id != checkout.id {
            return Err(IpcError::new(
                IpcErrorCode::InvalidCheckout,
                "located directory does not match the missing checkout",
            ));
        }
        let snapshot = registration_snapshot.ok_or_else(|| {
            IpcError::new(
                IpcErrorCode::InvalidCheckout,
                "Git checkout registration changed; refresh and retry locating it",
            )
        })?;
        return database
            .register_git_repo_if_unchanged(&resolved, &focus_id, Some(snapshot))
            .map_err(operation_error)?
            .ok_or_else(|| {
                IpcError::new(
                    IpcErrorCode::InvalidCheckout,
                    "Git checkout registration changed; refresh and retry locating it",
                )
            });
    }

    if checkout.is_primary {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "plain directories and primary checkouts can only be located at their original path; open a moved directory as a new location",
        ));
    }
    let management_root = database
        .existing_git_checkout(&repo.id)
        .map_err(operation_error)?
        .ok_or_else(|| {
            IpcError::new(
                IpcErrorCode::FolderMissing,
                "no available checkout exists to verify this worktree",
            )
        })?;
    if git_common_dir(&management_root)? != git_common_dir(selected_path)? {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "located directory belongs to a different Git repository",
        ));
    }
    repair_located_worktree(&management_root, selected_path)?;
    let (resolved, focus_id) =
        git::resolve_repository(selected_path, &timestamp())?.ok_or_else(|| {
            IpcError::new(
                IpcErrorCode::NotRepository,
                "located directory is not a Git checkout",
            )
        })?;
    if resolved.id != repo.id || !resolved.checkouts.iter().any(|item| item.id == focus_id) {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "located directory does not resolve to the repository containing this missing worktree",
        ));
    }
    let Some(removal) = removal.take() else {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "checkout relocation reservation is missing; retry locating this directory",
        ));
    };
    let snapshot = registration_snapshot.ok_or_else(|| {
        IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "Git checkout registration changed; refresh and retry locating it",
        )
    })?;
    finish_git_checkout_location(
        database,
        agents,
        &checkout.id,
        &resolved,
        &focus_id,
        snapshot,
        removal,
    )
}

pub(crate) fn finish_git_checkout_location(
    database: &Database,
    agents: &crate::services::agent::AgentService,
    checkout_id: &str,
    resolved: &Repo,
    focus_checkout_id: &str,
    snapshot: &crate::persistence::GitRepoRegistrationSnapshot,
    removal: crate::services::agent::WorktreeRemovalGuard,
) -> Result<crate::domain::workspace::WorkspaceState, IpcError> {
    if !database
        .locate_git_checkout_if_unchanged(resolved, checkout_id, focus_checkout_id, snapshot)
        .map_err(operation_error)?
    {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "Git checkout registration changed; refresh and retry locating it",
        ));
    }
    agents.stop_for_worktree_removal(checkout_id);
    removal.commit();
    database.load_workspace().map_err(operation_error)
}

fn reserve_idle_agent_checkout(
    agents: &crate::services::agent::AgentService,
    checkout_id: &str,
) -> Result<crate::services::agent::WorktreeRemovalGuard, IpcError> {
    let removal = agents.with_checkout_operation(checkout_id, || {
        agents
            .reserve_worktree_removal(checkout_id)
            .map_err(crate::services::agent::map_error)
    })?;
    let active = agents
        .active_worktree_agent_sessions(checkout_id)
        .map_err(crate::services::agent::map_error)?;
    if !active.is_empty() {
        let names = active
            .iter()
            .map(|session| session.title.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            format!("stop active agent session(s) before relocating this checkout: {names}"),
        ));
    }
    Ok(removal)
}

fn git_common_dir(path: &Path) -> Result<std::path::PathBuf, IpcError> {
    let output = crate::services::git::run_git_read(
        path,
        &["rev-parse", "--git-common-dir"],
        crate::services::git::GIT_READ_TIMEOUT,
    )?;
    if !output.status.success() {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "located directory does not contain Git worktree metadata",
        ));
    }
    let common_dir_text = String::from_utf8_lossy(&output.stdout);
    let common_dir = Path::new(common_dir_text.trim());
    let common_dir = if common_dir.is_absolute() {
        common_dir.to_path_buf()
    } else {
        path.join(common_dir)
    };
    common_dir.canonicalize().map_err(|error| {
        IpcError::new(
            IpcErrorCode::InvalidCheckout,
            format!("could not resolve Git worktree ownership: {error}"),
        )
    })
}

/// Takes a workdir off the panel, and leaves the disk exactly as it was.
///
/// A workdir that is still there loses its registration and nothing else: the branch, the
/// commits and the files are untouched, so opening the folder again brings it back. A workdir
/// whose directory is unavailable can still be registered; it is forgotten only when the user
/// explicitly closes it through [`close_missing_checkout`].
pub fn close_checkout(
    database: &Database,
    backend: &crate::terminal::TerminalBackend,
    agents: &crate::services::agent::AgentService,
    checkout_id: &str,
) -> Result<crate::domain::workspace::WorkspaceState, IpcError> {
    let state = database.load_workspace().map_err(operation_error)?;
    let (repo, checkout) = crate::services::checkout::registered_checkout(
        &state.repos,
        checkout_id,
        "checkout ID is not registered",
    )?;
    if checkout.is_missing {
        return close_missing_checkout(database, backend, agents, checkout_id);
    }
    let checkout_ids = checkout_agent_ids(&state, repo, checkout);
    with_agent_checkout_guards(
        agents,
        &checkout_ids,
        || {
            let current = database.load_workspace().map_err(operation_error)?;
            let (repo, checkout) = crate::services::checkout::registered_checkout(
                &current.repos,
                checkout_id,
                "checkout ID is not registered",
            )?;
            if checkout_agent_ids(&current, repo, checkout) != checkout_ids {
                return Err(IpcError::new(
                    IpcErrorCode::InvalidCheckout,
                    "checkout registrations changed; refresh and retry closing this location",
                ));
            }
            Ok(())
        },
        || {
            database
                .close_checkout(checkout_id)
                .map_err(operation_error)
        },
    )
}

/// Takes a worktree off the panel and keeps it, so the repo root can offer it back.
///
/// Archiving is the reversible half of leaving a workdir: the row stays registered and the
/// disk is untouched, and [`restore_archived_worktrees`] is the way back. Closing is the
/// other half and forgets the checkout entirely.
pub fn archive_checkout(
    database: &Database,
    agents: &crate::services::agent::AgentService,
    checkout_id: &str,
) -> Result<crate::domain::workspace::WorkspaceState, IpcError> {
    let state = database.load_workspace().map_err(operation_error)?;
    let (repo, checkout) = crate::services::checkout::registered_checkout(
        &state.repos,
        checkout_id,
        "checkout ID is not registered",
    )?;
    let checkout_ids = checkout_agent_ids(&state, repo, checkout);
    with_agent_checkout_guards(
        agents,
        &checkout_ids,
        || {
            let current = database.load_workspace().map_err(operation_error)?;
            let (repo, checkout) = crate::services::checkout::registered_checkout(
                &current.repos,
                checkout_id,
                "checkout ID is not registered",
            )?;
            if checkout_agent_ids(&current, repo, checkout) != checkout_ids {
                return Err(IpcError::new(
                    IpcErrorCode::InvalidCheckout,
                    "checkout registrations changed; refresh and retry archiving",
                ));
            }
            Ok(())
        },
        || {
            database
                .archive_checkout(checkout_id)
                .map_err(operation_error)
        },
    )
}

/// Puts back every worktree this repository archived, so one action brings the whole set
/// to the panel rather than making the user walk them one at a time.
pub fn restore_archived_worktrees(
    database: &Database,
    agents: &crate::services::agent::AgentService,
    repo_id: &str,
) -> Result<crate::domain::workspace::WorkspaceState, IpcError> {
    let state = database.load_workspace().map_err(operation_error)?;
    if !state.repos.iter().any(|repo| repo.id == repo_id) {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "repository ID is not registered",
        ));
    }
    let checkout_ids = archived_checkout_ids(&state, repo_id);
    if checkout_ids.is_empty() {
        return database
            .restore_archived_worktrees(repo_id)
            .map_err(operation_error);
    }
    with_agent_checkout_guards(
        agents,
        &checkout_ids,
        || {
            let current = database.load_workspace().map_err(operation_error)?;
            if archived_checkout_ids(&current, repo_id) != checkout_ids {
                return Err(IpcError::new(
                    IpcErrorCode::InvalidCheckout,
                    "archived worktrees changed; refresh and retry restoring them",
                ));
            }
            Ok(())
        },
        || {
            database
                .restore_archived_worktrees(repo_id)
                .map_err(operation_error)
        },
    )
}

pub fn close_missing_checkout(
    database: &Database,
    backend: &crate::terminal::TerminalBackend,
    agents: &crate::services::agent::AgentService,
    checkout_id: &str,
) -> Result<crate::domain::workspace::WorkspaceState, IpcError> {
    let state = database.load_workspace().map_err(operation_error)?;
    let (repo, checkout) = crate::services::checkout::registered_checkout(
        &state.repos,
        checkout_id,
        "checkout ID is not registered",
    )?;
    if !checkout.is_missing {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "only a missing checkout can be closed",
        ));
    }
    if repo.kind == RepoKind::Git && !checkout.is_primary {
        if checkout
            .sessions
            .iter()
            .any(|session| session.status == crate::domain::workspace::SessionStatus::Active)
        {
            return Err(IpcError::new(
                IpcErrorCode::InvalidCheckout,
                "close active terminal sessions before closing this missing worktree",
            ));
        }
        let info = worktree::removal_info(database, agents, checkout_id)?;
        let confirmation = worktree::WorktreeRemovalConfirmation {
            confirm_dirty: false,
            confirmed_dirty_files: info.dirty_files,
            confirmed_session_ids: Vec::new(),
            expected_branch: info.branch,
            expected_unmerged_commits: info.unmerged_commits,
            delete_branch: false,
        };
        return worktree::remove(database, backend, agents, checkout_id, &confirmation)
            .map(|removed| removed.workspace);
    }
    if repo.kind == RepoKind::Git && checkout.is_primary {
        let checkout_ids = repo_agent_checkout_ids(&state, &repo.id);
        return close_missing_primary_git_repo(database, agents, checkout_id, &checkout_ids);
    }
    let checkout_ids = checkout_agent_ids(&state, repo, checkout);
    with_agent_checkout_guards(
        agents,
        &checkout_ids,
        || {
            let current = database.load_workspace().map_err(operation_error)?;
            let (current_repo, current_checkout) = crate::services::checkout::registered_checkout(
                &current.repos,
                checkout_id,
                "checkout ID is not registered",
            )?;
            if !current_checkout.is_missing
                || checkout_agent_ids(&current, current_repo, current_checkout) != checkout_ids
            {
                return Err(IpcError::new(
                    IpcErrorCode::InvalidCheckout,
                    "checkout registrations changed; refresh and retry closing this location",
                ));
            }
            Ok(())
        },
        || {
            database
                .close_missing_checkout(checkout_id)
                .map_err(operation_error)
        },
    )
}

fn close_missing_primary_git_repo(
    database: &Database,
    agents: &crate::services::agent::AgentService,
    checkout_id: &str,
    checkout_ids: &[String],
) -> Result<crate::domain::workspace::WorkspaceState, IpcError> {
    with_agent_checkout_guards(
        agents,
        checkout_ids,
        || {
            database
                .ensure_not_home_checkout(checkout_id)
                .map_err(operation_error)?;
            let state = database.load_workspace().map_err(operation_error)?;
            let (repo, checkout) = crate::services::checkout::registered_checkout(
                &state.repos,
                checkout_id,
                "checkout ID is not registered",
            )?;
            if repo.kind != RepoKind::Git || !checkout.is_primary || !checkout.is_missing {
                return Err(IpcError::new(
                    IpcErrorCode::InvalidCheckout,
                    "repository changed; refresh and retry closing the missing checkout",
                ));
            }
            if repo_agent_checkout_ids(&state, &repo.id) != checkout_ids {
                return Err(IpcError::new(
                    IpcErrorCode::InvalidCheckout,
                    "repository checkouts changed; refresh and retry closing the missing checkout",
                ));
            }
            if repo.checkouts.iter().any(|checkout| {
                checkout.sessions.iter().any(|session| {
                    session.status == crate::domain::workspace::SessionStatus::Active
                })
            }) {
                return Err(IpcError::new(
                    IpcErrorCode::InvalidCheckout,
                    "close active terminal sessions before closing this missing location",
                ));
            }
            Ok(())
        },
        || {
            database
                .close_missing_checkout(checkout_id)
                .map_err(operation_error)
        },
    )
}

fn checkout_agent_ids(
    state: &crate::domain::workspace::WorkspaceState,
    repo: &crate::domain::workspace::Repo,
    checkout: &crate::domain::workspace::Checkout,
) -> Vec<String> {
    if repo.kind == RepoKind::Plain || checkout.is_primary {
        repo_agent_checkout_ids(state, &repo.id)
    } else {
        vec![checkout.id.clone()]
    }
}

fn archived_checkout_ids(
    state: &crate::domain::workspace::WorkspaceState,
    repo_id: &str,
) -> Vec<String> {
    let mut ids: Vec<_> = state
        .archived_worktrees
        .iter()
        .filter(|checkout| checkout.repo_id == repo_id)
        .map(|checkout| checkout.id.clone())
        .collect();
    ids.sort();
    ids
}

fn repo_agent_checkout_ids(
    state: &crate::domain::workspace::WorkspaceState,
    repo_id: &str,
) -> Vec<String> {
    let mut ids: Vec<_> = state
        .repos
        .iter()
        .find(|repo| repo.id == repo_id)
        .into_iter()
        .flat_map(|repo| repo.checkouts.iter().map(|checkout| checkout.id.clone()))
        .chain(
            state
                .archived_worktrees
                .iter()
                .filter(|checkout| checkout.repo_id == repo_id)
                .map(|checkout| checkout.id.clone()),
        )
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

fn with_agent_checkout_operations<T>(
    agents: &crate::services::agent::AgentService,
    checkout_ids: &[String],
    operation: impl FnOnce() -> T,
) -> T {
    match checkout_ids.split_first() {
        Some((checkout_id, remaining)) => agents.with_checkout_operation(checkout_id, || {
            with_agent_checkout_operations(agents, remaining, operation)
        }),
        None => operation(),
    }
}

fn with_agent_checkout_guards<T>(
    agents: &crate::services::agent::AgentService,
    checkout_ids: &[String],
    validate: impl FnOnce() -> Result<(), IpcError>,
    operation: impl FnOnce() -> Result<T, IpcError>,
) -> Result<T, IpcError> {
    let mut checkout_ids = checkout_ids.to_vec();
    checkout_ids.sort();
    checkout_ids.dedup();
    with_agent_checkout_operations(agents, &checkout_ids, || {
        let removals = checkout_ids
            .iter()
            .map(|id| {
                agents
                    .reserve_worktree_removal(id)
                    .map_err(crate::services::agent::map_error)
            })
            .collect::<Result<Vec<_>, _>>()?;
        validate()?;

        let mut active = Vec::new();
        for id in &checkout_ids {
            active.extend(
                agents
                    .active_worktree_agent_sessions(id)
                    .map_err(crate::services::agent::map_error)?,
            );
        }
        if !active.is_empty() {
            let names = active
                .iter()
                .map(|session| session.title.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(IpcError::new(
                IpcErrorCode::InvalidCheckout,
                format!("stop active agent session(s) before changing this checkout: {names}"),
            ));
        }
        let result = operation()?;
        for id in &checkout_ids {
            agents.stop_for_worktree_removal(id);
        }
        for removal in removals {
            removal.commit();
        }
        Ok(result)
    })
}

fn operation_error(error: String) -> IpcError {
    IpcError::new(IpcErrorCode::OperationFailed, error)
}

pub fn restore(
    database: &Database,
    home: &HomeDirectory,
) -> Result<crate::domain::workspace::WorkspaceState, IpcError> {
    let state = database
        .load_workspace()
        .map_err(|error| IpcError::new(IpcErrorCode::OperationFailed, error))?;
    for repo in state.repos.iter().filter(|repo| repo.kind == RepoKind::Git) {
        let Some(path) = database
            .existing_git_checkout(&repo.id)
            .map_err(|error| IpcError::new(IpcErrorCode::OperationFailed, error))?
        else {
            continue;
        };
        if let Ok(Some((resolved, _))) = git::resolve_repository(&path, &timestamp()) {
            if resolved.id == repo.id {
                database
                    .reconcile_git_repo(&resolved)
                    .map_err(|error| IpcError::new(IpcErrorCode::OperationFailed, error))?;
            }
        }
    }
    // Keep registered checkouts whose directories are temporarily unavailable; load_workspace
    // marks them missing and reconciliation clears that state when their paths return.
    let mut home_repo = Repo::plain(&home.0, timestamp())
        .map_err(|error| IpcError::new(IpcErrorCode::InvalidPath, error))?;
    home_repo.name = "Home".into();
    let home_checkout_id = home_repo.checkouts[0].id.clone();
    database
        .register_home_repo(&home_repo)
        .map_err(operation_error)?;
    database
        .select_checkout(Some(home_checkout_id))
        .map_err(|error| IpcError::new(IpcErrorCode::OperationFailed, error))
}

/// Reads one repository's worktrees again and registers the ones Git now lists.
///
/// More than one hand adds a worktree: Muster's own dialog, an agent running `git worktree add`
/// in a terminal, a script. The disk is what they all wrote to and what the panel has to agree
/// with, so this asks Git for one repository's list and reconciles the answer: the same reading
/// [`restore`] takes for every repository, asked for one because a change said this one moved.
///
/// The archived stay archived: reconciliation writes what Git lists and leaves every other
/// column of a checkout it already knows alone, so a worktree the user put on the shelf is not
/// brought back by another hand adding a sibling.
pub fn sync_repo(
    database: &Database,
    agents: &crate::services::agent::AgentService,
    repo_id: &str,
) -> Result<Option<crate::domain::workspace::WorkspaceState>, IpcError> {
    let Some(snapshot) = git_repo_sync_snapshot(database, agents, repo_id)? else {
        return Ok(None);
    };
    let Some(path) = snapshot
        .repo
        .checkouts
        .iter()
        .map(|checkout| PathBuf::from(&checkout.canonical_path))
        .chain(
            snapshot
                .state
                .archived_worktrees
                .iter()
                .filter(|checkout| checkout.repo_id == repo_id)
                .map(|checkout| PathBuf::from(&checkout.path)),
        )
        .find(|path| path.is_dir())
    else {
        return Ok(None);
    };

    // Git lists all live and missing worktrees; the workspace splits those same rows between
    // the panel and the archive shelf. Compare just their paths first: most filesystem events
    // near `.git/worktrees` are not a membership change, and one `worktree list` is cheaper than
    // resolving every checkout, updating the database and returning the workspace again.
    let registered: BTreeSet<PathBuf> = snapshot
        .repo
        .checkouts
        .iter()
        .map(|checkout| PathBuf::from(&checkout.canonical_path))
        .chain(
            snapshot
                .state
                .archived_worktrees
                .iter()
                .filter(|checkout| checkout.repo_id == repo_id)
                .map(|checkout| PathBuf::from(&checkout.path)),
        )
        .collect();
    if worktree::registered_paths(&path)? == registered {
        return Ok(None);
    }

    // A membership change is a reason to reconcile, not a reason to ignore Git errors. Unlike
    // startup's best-effort scan, this is a live request with a panel waiting for its answer.
    let Some(resolved) = resolve_repo(database, &snapshot.repo.id)? else {
        return Ok(None);
    };
    reconcile_git_repo_if_current(database, agents, &snapshot, &resolved)
}

struct GitRepoSyncSnapshot {
    state: crate::domain::workspace::WorkspaceState,
    repo: Repo,
    checkout_ids: Vec<String>,
    registration_snapshot: crate::persistence::GitRepoRegistrationSnapshot,
    generations: Vec<(String, u64)>,
}

fn git_repo_sync_snapshot(
    database: &Database,
    agents: &crate::services::agent::AgentService,
    repo_id: &str,
) -> Result<Option<GitRepoSyncSnapshot>, IpcError> {
    let initial = database.load_workspace().map_err(operation_error)?;
    if !initial
        .repos
        .iter()
        .any(|repo| repo.id == repo_id && repo.kind == RepoKind::Git)
    {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "Git repository is not registered",
        ));
    }
    let checkout_ids = repo_agent_checkout_ids(&initial, repo_id);
    with_agent_checkout_operations(agents, &checkout_ids, || {
        let state = database.load_workspace().map_err(operation_error)?;
        let Some(repo) = state
            .repos
            .iter()
            .find(|repo| repo.id == repo_id && repo.kind == RepoKind::Git)
        else {
            return Ok(None);
        };
        if repo_agent_checkout_ids(&state, repo_id) != checkout_ids {
            return Ok(None);
        }
        let repo = repo.clone();
        let Some(registration_snapshot) = database
            .git_repo_registration_snapshot(repo_id)
            .map_err(operation_error)?
        else {
            return Ok(None);
        };
        if registration_snapshot.checkout_ids() != checkout_ids {
            return Ok(None);
        }
        let generations = checkout_ids
            .iter()
            .map(|checkout_id| {
                agents
                    .checkout_generation(checkout_id)
                    .map(|generation| (checkout_id.clone(), generation))
                    .map_err(crate::services::agent::map_error)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(GitRepoSyncSnapshot {
            state,
            repo,
            checkout_ids: checkout_ids.clone(),
            registration_snapshot,
            generations,
        }))
    })
}

fn reconcile_git_repo_if_current(
    database: &Database,
    agents: &crate::services::agent::AgentService,
    snapshot: &GitRepoSyncSnapshot,
    resolved: &Repo,
) -> Result<Option<crate::domain::workspace::WorkspaceState>, IpcError> {
    with_agent_checkout_operations(agents, &snapshot.checkout_ids, || {
        let state = database.load_workspace().map_err(operation_error)?;
        let Some(repo) = state
            .repos
            .iter()
            .find(|repo| repo.id == snapshot.repo.id && repo.kind == RepoKind::Git)
        else {
            return Ok(None);
        };
        if repo.root != snapshot.repo.root
            || repo_agent_checkout_ids(&state, &snapshot.repo.id) != snapshot.checkout_ids
        {
            return Ok(None);
        }
        for (checkout_id, generation) in &snapshot.generations {
            if agents
                .checkout_generation(checkout_id)
                .map_err(crate::services::agent::map_error)?
                != *generation
            {
                return Ok(None);
            }
        }
        if !database
            .reconcile_git_repo_if_unchanged(resolved, &snapshot.registration_snapshot)
            .map_err(operation_error)?
        {
            return Ok(None);
        }
        database.load_workspace().map(Some).map_err(operation_error)
    })
}

/// Re-resolves one registered repository from any checkout that still exists.
///
/// A repository whose checkouts are all gone has nothing to reconcile, and one Git no longer
/// recognizes is left as it is rather than deleted: the directory may be coming back, and the row
/// that says so is the one that offers to look for it.
fn resolve_repo(
    database: &Database,
    repo_id: &str,
) -> Result<Option<crate::domain::workspace::Repo>, IpcError> {
    let Some(path) = database
        .existing_git_checkout(repo_id)
        .map_err(|error| IpcError::new(IpcErrorCode::OperationFailed, error))?
    else {
        return Ok(None);
    };
    let Some((resolved, _)) = git::resolve_repository(&path, &timestamp())? else {
        return Ok(None);
    };
    if resolved.id != repo_id {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "checkout no longer belongs to the registered Git repository",
        ));
    }
    Ok(Some(resolved))
}

pub fn set_default_branch(
    database: &Database,
    repo_id: &str,
    branch: &str,
) -> Result<crate::domain::workspace::WorkspaceState, IpcError> {
    let branch = branch
        .trim()
        .strip_prefix("origin/")
        .unwrap_or(branch.trim());
    if branch.is_empty() {
        return Err(IpcError::new(
            IpcErrorCode::DefaultBranchUnknown,
            "default branch cannot be empty",
        ));
    }
    let root = database
        .existing_git_checkout(repo_id)
        .map_err(|error| IpcError::new(IpcErrorCode::OperationFailed, error))?
        .ok_or_else(|| {
            IpcError::new(
                IpcErrorCode::InvalidCheckout,
                "no available checkout exists for this repository",
            )
        })?;
    let ref_name = format!("refs/heads/{branch}");
    let valid_output = crate::services::git::run_git_read(
        &root,
        &["check-ref-format", &ref_name],
        crate::services::git::GIT_READ_TIMEOUT,
    )?;
    let valid = if valid_output.status.success() {
        true
    } else if valid_output.status.code() == Some(1)
        && valid_output.stdout.is_empty()
        && valid_output.stderr.is_empty()
    {
        false
    } else {
        return Err(crate::services::git::git_error(
            "could not validate Git branch",
            &valid_output,
        ));
    };
    let remote_ref = format!("refs/remotes/origin/{branch}");
    let mut exists = false;
    for reference in [&ref_name, &remote_ref] {
        let output = crate::services::git::run_git_read(
            &root,
            &["show-ref", "--verify", "--quiet", reference],
            crate::services::git::GIT_READ_TIMEOUT,
        )?;
        if output.status.success() {
            exists = true;
            break;
        }
        if output.status.code() != Some(1) || !output.stdout.is_empty() || !output.stderr.is_empty()
        {
            return Err(crate::services::git::git_error(
                "could not check Git branch",
                &output,
            ));
        }
    }
    if !valid || !exists {
        return Err(IpcError::new(
            IpcErrorCode::DefaultBranchUnknown,
            format!("Git branch '{branch}' does not exist or is not a valid branch name"),
        ));
    }
    database
        .set_default_branch(repo_id, branch)
        .map_err(|error| IpcError::new(IpcErrorCode::OperationFailed, error))
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, process::Command};

    use tempfile::tempdir;

    use crate::{
        domain::{
            ipc::IpcErrorCode,
            review::{ReviewNote, ReviewRound},
            terminal_layout::{CheckoutTerminalLayout, TerminalLayoutNode, TerminalLayoutTab},
            workspace::{RepoKind, Session, SessionStatus, SessionType},
        },
        persistence::Database,
        services::git::{run_git_read, GIT_READ_TIMEOUT},
    };

    use super::{
        archive_checkout, close_checkout, close_missing_checkout, locate_missing_checkout,
        register_folder, repair_located_worktree, restore, set_default_branch, sync_repo,
        HomeDirectory,
    };

    fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("Git is installed");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_read(cwd: &Path, args: &[&str]) -> String {
        let output = run_git_read(cwd, args, GIT_READ_TIMEOUT).expect("Git read completes");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn init_repo(path: &Path) {
        fs::create_dir_all(path).unwrap();
        git(path, &["init", "-b", "trunk"]);
        git(path, &["config", "user.name", "Muster test"]);
        git(path, &["config", "user.email", "muster@example.invalid"]);
        fs::write(path.join("tracked.txt"), "initial\n").unwrap();
        git(path, &["add", "tracked.txt"]);
        git(path, &["commit", "-m", "initial"]);
    }

    fn database(path: &Path) -> Database {
        Database::open(path.join("workspace.sqlite3")).expect("database")
    }

    /// A repair that fails has to say so in bounded words.
    ///
    /// What Git prints on this path is whatever a helper it started printed, and a message a user
    /// reads is not the place for all of it: before this went through `git_error`, the raw stderr
    /// went into the error whole, so a helper that printed a wall of text put a wall of text into
    /// the app's error. Git prints this diagnostic on stderr with nothing on stdout, which is the
    /// case that has to be covered -- so the fixture makes Git print a lot of it, and the assertion
    /// is that what came back is summarized, prefixed with the action and the exit code.
    #[test]
    fn a_failed_worktree_repair_reports_a_bounded_diagnostic() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        init_repo(&primary);
        // A path Git names in full and does not accept: its own words are long, and every one of
        // them used to travel to the user.
        let wall = "w".repeat(64);
        let not_a_worktree = temp
            .path()
            .join(&wall)
            .join(&wall)
            .join(&wall)
            .join(&wall)
            .join(&wall)
            .join(&wall);

        let error = repair_located_worktree(&primary, &not_a_worktree)
            .expect_err("repairing a path that is not a worktree cannot succeed");

        assert_eq!(error.code, IpcErrorCode::GitFailed);
        assert!(
            error
                .message
                .starts_with("could not repair the located worktree (exit code "),
            "{error:?}"
        );
        // The diagnostic is on stderr and stdout is empty, so this is Git's own words summarized rather
        // than an empty answer dressed up as one: at most three lines of at most 120 characters, with
        // the rest of the path dropped and marked as dropped -- the same bound every other Git
        // diagnostic in this app is held to.
        let said = error.message.split_once(": ").unwrap().1;
        assert!(said.contains("fatal: Invalid path"), "{error:?}");
        assert!(
            said.chars().count() <= 121 * 3 + " / ".len() * 2,
            "{error:?}"
        );
        assert!(
            said.chars().count() < not_a_worktree.to_string_lossy().chars().count(),
            "the whole path travelled to the user: {error:?}"
        );
        assert!(said.ends_with('…'), "{error:?}");
    }

    fn home_directory(parent: &Path) -> HomeDirectory {
        let path = parent.join("home");
        fs::create_dir_all(&path).unwrap();
        HomeDirectory(path)
    }

    #[test]
    fn opening_a_worktree_registers_primary_first_and_focuses_that_worktree() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo with spaces");
        let linked = temp.path().join("feature checkout");
        init_repo(&primary);
        git(
            &primary,
            &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
        );

        let db = database(temp.path());
        let state = register_folder(&db, &linked).unwrap();

        assert_eq!(state.repos.len(), 1);
        let repo = &state.repos[0];
        assert_eq!(repo.kind, RepoKind::Git);
        assert_eq!(repo.checkouts.len(), 2);
        assert!(repo.checkouts[0].is_primary);
        assert_eq!(
            repo.checkouts[0].canonical_path,
            primary.canonicalize().unwrap().display().to_string()
        );
        assert_eq!(repo.checkouts[1].branch.as_deref(), Some("feature"));
        assert_eq!(
            state.active_checkout_id.as_deref(),
            Some(repo.checkouts[1].id.as_str())
        );
        assert_eq!(
            repo.default_branch, None,
            "no origin/HEAD must not assume trunk"
        );
        let chosen = set_default_branch(&db, &repo.id, "trunk").unwrap();
        assert_eq!(chosen.repos[0].default_branch.as_deref(), Some("trunk"));
        let reopened = register_folder(&db, &linked).unwrap();
        assert_eq!(reopened.repos[0].default_branch.as_deref(), Some("trunk"));
    }

    #[test]
    fn origin_head_is_used_and_a_manual_default_branch_is_persisted() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        let remote = temp.path().join("origin.git");
        init_repo(&primary);
        fs::create_dir(&remote).unwrap();
        git(&remote, &["init", "--bare"]);
        git(
            &primary,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&primary, &["push", "-u", "origin", "trunk"]);
        git(
            &primary,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/trunk",
            ],
        );
        let db = database(temp.path());

        let detected = register_folder(&db, &primary).unwrap();

        assert_eq!(detected.repos[0].default_branch.as_deref(), Some("trunk"));
        let picked = set_default_branch(&db, &detected.repos[0].id, "trunk").unwrap();
        assert_eq!(picked.repos[0].default_branch.as_deref(), Some("trunk"));
        assert!(set_default_branch(&db, &detected.repos[0].id, "not-a-branch").is_err());
    }

    #[test]
    fn opening_subdirectories_and_symlinks_reuses_repo_and_focuses_containing_checkout() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        let linked = temp.path().join("linked");
        init_repo(&primary);
        git(
            &primary,
            &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
        );
        let nested = linked.join("nested");
        fs::create_dir(&nested).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&nested, temp.path().join("nested alias")).unwrap();

        let db = database(temp.path());
        let first = register_folder(&db, &primary).unwrap();
        let second = register_folder(&db, &nested).unwrap();
        #[cfg(unix)]
        let third = register_folder(&db, &temp.path().join("nested alias")).unwrap();

        assert_eq!(second.repos.len(), 1);
        assert_eq!(second.repos[0].checkouts.len(), 2);
        assert_eq!(
            second.active_checkout_id.as_deref(),
            Some(first.repos[0].checkouts[1].id.as_str())
        );
        #[cfg(unix)]
        assert_eq!(third.repos.len(), 1);
        #[cfg(unix)]
        assert_eq!(third.active_checkout_id, second.active_checkout_id);
    }

    #[test]
    fn reopening_a_repo_does_not_duplicate_repo_or_checkout_rows() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        let db = database(temp.path());

        register_folder(&db, &root).unwrap();
        let reopened = register_folder(&db, &root).unwrap();

        assert_eq!(reopened.repos.len(), 1);
        assert_eq!(reopened.repos[0].checkouts.len(), 1);
    }

    #[test]
    fn restart_restores_checkout_order_and_focus_but_no_terminal_sessions() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        let linked = temp.path().join("feature checkout");
        let database_path = temp.path().join("workspace.sqlite3");
        init_repo(&primary);
        git(
            &primary,
            &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
        );

        let session = Session {
            id: "session:feature-shell".into(),
            session_type: SessionType::Shell,
            checkout_id: crate::domain::workspace::checkout_id_for_path(
                &linked.canonicalize().unwrap().display().to_string(),
            ),
            name: "zsh · feature".into(),
            created_at: "saved-at".into(),
            status: SessionStatus::Inactive,
        };
        {
            let database = Database::open(&database_path).unwrap();
            let opened = register_folder(&database, &primary).unwrap();
            let focused = register_folder(&database, &linked).unwrap();
            assert_eq!(
                focused.repos[0].checkouts[0].id,
                opened.repos[0].checkouts[0].id
            );
            assert_eq!(
                focused.active_checkout_id.as_deref(),
                Some(session.checkout_id.as_str())
            );
            database.add_terminal_session(&session).unwrap();

            let primary_id = focused.repos[0].checkouts[0].id.clone();
            let switched = database.select_checkout(Some(primary_id)).unwrap();
            assert_eq!(switched.active_session_id, None);
            let selected = database.select_session(Some(session.id.clone())).unwrap();
            assert_eq!(
                selected.active_checkout_id.as_deref(),
                Some(session.checkout_id.as_str())
            );
            assert_eq!(
                selected.active_session_id.as_deref(),
                Some(session.id.as_str())
            );
        }

        let database = Database::open(&database_path).unwrap();
        let home = temp.path().join("home");
        fs::create_dir(&home).unwrap();
        let restored = restore(&database, &super::HomeDirectory(home.clone())).unwrap();
        let home_checkout_id = crate::domain::workspace::checkout_id_for_path(
            &home.canonicalize().unwrap().display().to_string(),
        );
        assert_eq!(restored.repos[0].name, "Home");
        assert_eq!(
            restored.home_checkout_id.as_deref(),
            Some(home_checkout_id.as_str())
        );
        assert_eq!(
            restored.active_checkout_id.as_deref(),
            Some(home_checkout_id.as_str())
        );
        let repo = restored
            .repos
            .iter()
            .find(|repo| {
                repo.id
                    == crate::domain::workspace::repo_id_for_path(
                        &primary.canonicalize().unwrap().display().to_string(),
                    )
            })
            .unwrap();
        assert_eq!(repo.checkouts.len(), 2);
        assert!(repo.checkouts[0].is_primary);
        assert_eq!(repo.checkouts[1].branch.as_deref(), Some("feature"));
        // The session the previous process selected is gone with its PTY, so nothing points at it.
        assert_eq!(restored.active_session_id, None);
        assert!(repo.checkouts[1].sessions.is_empty());
    }

    #[test]
    fn restore_keeps_a_worktree_missing_while_the_app_was_closed() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        let linked = temp.path().join("gone");
        init_repo(&primary);
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-b",
                "temporary",
                linked.to_str().unwrap(),
            ],
        );
        let linked_canonical = linked.canonicalize().unwrap().display().to_string();
        let db = database(temp.path());
        register_folder(&db, &primary).unwrap();
        let focused = register_folder(&db, &linked).unwrap();
        let removed_id = focused
            .active_checkout_id
            .clone()
            .expect("opening the worktree selects it");
        fs::remove_dir_all(&linked).unwrap();

        let state = restore(&db, &home_directory(temp.path())).unwrap();

        // Git still lists the worktree, but its unavailable directory is not proof that it
        // was intentionally removed.
        let repo = state
            .repos
            .iter()
            .find(|repo| repo.kind == RepoKind::Git)
            .unwrap();
        let missing = repo
            .checkouts
            .iter()
            .find(|checkout| checkout.canonical_path == linked_canonical)
            .expect("temporarily unavailable worktree remains registered");
        assert!(missing.is_missing);
        assert_eq!(repo.checkouts.len(), 2);
        assert!(repo.checkouts[0].is_primary);
        assert_eq!(state.active_checkout_id, state.home_checkout_id);
        assert_ne!(
            state.active_checkout_id.as_deref(),
            Some(removed_id.as_str())
        );
    }

    #[test]
    fn restore_keeps_a_repository_whose_root_is_temporarily_gone() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        init_repo(&primary);
        let db = database(temp.path());
        let repo_id = register_folder(&db, &primary).unwrap().repos[0].id.clone();
        fs::remove_dir_all(&primary).unwrap();

        let state = restore(&db, &home_directory(temp.path())).unwrap();

        assert_eq!(state.repos.len(), 2);
        let repo = state.repos.iter().find(|repo| repo.id == repo_id).unwrap();
        assert!(repo.checkouts[0].is_missing);
        assert_eq!(state.active_checkout_id, state.home_checkout_id);
    }

    #[test]
    fn restore_preserves_review_history_while_a_worktree_is_unavailable_and_recovers_it() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        let linked = temp.path().join("worktree");
        let unavailable = temp.path().join("unavailable");
        init_repo(&primary);
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-b",
                "temporary",
                linked.to_str().unwrap(),
            ],
        );
        let db = database(temp.path());
        register_folder(&db, &primary).unwrap();
        let registered = register_folder(&db, &linked).unwrap();
        let checkout_id = registered.active_checkout_id.unwrap();
        let note_id = "review-note:temporary".to_owned();
        let note = ReviewNote {
            id: note_id.clone(),
            checkout_id: checkout_id.clone(),
            path: "tracked.txt".into(),
            side: "new".into(),
            line_start: 1,
            line_end: None,
            content: "Keep this review note".into(),
            code: "initial".into(),
            status: "draft".into(),
            code_hash: "test-hash".into(),
            outdated: false,
            round_id: None,
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        db.add_review_note(&note).unwrap();
        let round = ReviewRound {
            id: "review-round:temporary".into(),
            checkout_id: checkout_id.clone(),
            session_id: None,
            status: "queued".into(),
            marker: "muster-review:review-round:temporary".into(),
            note_ids: vec![note_id.clone()],
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        db.add_review_round(&round, &[note_id], None).unwrap();
        let saved_note = db.review_notes(&checkout_id).unwrap().remove(0);
        let saved_round = db.review_rounds(&checkout_id).unwrap().remove(0);

        fs::rename(&linked, &unavailable).unwrap();
        let missing = restore(&db, &home_directory(temp.path())).unwrap();
        let repo = missing
            .repos
            .iter()
            .find(|repo| repo.kind == RepoKind::Git)
            .unwrap();
        assert!(repo
            .checkouts
            .iter()
            .any(|checkout| checkout.id == checkout_id && checkout.is_missing));
        assert_eq!(
            db.review_notes(&checkout_id).unwrap(),
            vec![saved_note.clone()]
        );
        assert_eq!(
            db.review_rounds(&checkout_id).unwrap(),
            vec![saved_round.clone()]
        );

        fs::rename(&unavailable, &linked).unwrap();
        let returned = restore(&db, &home_directory(temp.path())).unwrap();
        let repo = returned
            .repos
            .iter()
            .find(|repo| repo.kind == RepoKind::Git)
            .unwrap();
        assert!(repo
            .checkouts
            .iter()
            .any(|checkout| checkout.id == checkout_id && !checkout.is_missing));
        assert_eq!(db.review_notes(&checkout_id).unwrap(), vec![saved_note]);
        assert_eq!(db.review_rounds(&checkout_id).unwrap(), vec![saved_round]);
    }

    #[test]
    fn restore_discovers_worktrees_created_outside_the_app() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        let linked = temp.path().join("external worktree");
        init_repo(&primary);
        let db = database(temp.path());
        register_folder(&db, &primary).unwrap();
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-b",
                "external",
                linked.to_str().unwrap(),
            ],
        );

        let restored = restore(&db, &home_directory(temp.path())).unwrap();

        let repo = restored
            .repos
            .iter()
            .find(|repo| repo.kind == RepoKind::Git)
            .unwrap();
        assert_eq!(repo.checkouts.len(), 2);
        assert_eq!(repo.checkouts[1].branch.as_deref(), Some("external"));
        assert_eq!(restored.active_checkout_id, restored.home_checkout_id);
    }

    /// A worktree is created by more than one hand: an agent running `git worktree add` in a
    /// terminal, a script, this app's own dialog. Whichever one it was, the panel has to show it
    /// while the app is open rather than at the next launch, and the registration Muster holds is
    /// the only place that reconciliation can happen.
    #[test]
    fn sync_repo_lists_a_worktree_created_outside_the_app_while_it_is_open() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        let linked = temp.path().join("external worktree");
        init_repo(&primary);
        let db = database(temp.path());
        let opened = register_folder(&db, &primary).unwrap();
        let repo_id = opened.repos[0].id.clone();
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-b",
                "external",
                linked.to_str().unwrap(),
            ],
        );

        let synced = sync_repo(
            &db,
            &crate::services::agent::AgentService::without_service(),
            &repo_id,
        )
        .unwrap()
        .expect("a worktree joined");

        assert_eq!(synced.repos.len(), 1);
        assert_eq!(synced.repos[0].checkouts.len(), 2);
        assert_eq!(
            synced.repos[0].checkouts[1].branch.as_deref(),
            Some("external")
        );
        // The panel grows rather than the selection moving: an agent adding a worktree is not
        // something the user asked the app to open.
        assert_eq!(synced.active_checkout_id, opened.active_checkout_id);
    }

    #[test]
    fn sync_repo_does_not_reload_the_workspace_when_the_worktree_list_is_unchanged() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        init_repo(&primary);
        let db = database(temp.path());
        let opened = register_folder(&db, &primary).unwrap();

        let synced = sync_repo(
            &db,
            &crate::services::agent::AgentService::without_service(),
            &opened.repos[0].id,
        )
        .unwrap();

        assert!(synced.is_none());
    }

    #[test]
    fn conditional_sync_keeps_a_checkout_registered_after_its_snapshot() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        let linked = temp.path().join("late checkout");
        init_repo(&primary);
        let db = database(temp.path());
        let opened = register_folder(&db, &primary).unwrap();
        let repo_id = opened.repos[0].id.clone();
        let expected = db
            .git_repo_registration_snapshot(&repo_id)
            .unwrap()
            .expect("the Git repository registration snapshot");
        let stale = super::resolve_repo(&db, &repo_id)
            .unwrap()
            .expect("the repository resolves before another checkout is added");
        assert_eq!(stale.checkouts.len(), 1);

        git(
            &primary,
            &["worktree", "add", "-b", "late", linked.to_str().unwrap()],
        );
        register_folder(&db, &linked).unwrap();
        let late_id = crate::domain::workspace::checkout_id_for_path(
            &linked.canonicalize().unwrap().display().to_string(),
        );
        assert!(db.load_workspace().unwrap().repos[0]
            .checkouts
            .iter()
            .any(|checkout| checkout.id == late_id && !checkout.is_missing));

        assert!(!db
            .reconcile_git_repo_if_unchanged(&stale, &expected)
            .unwrap());
        let current = db.load_workspace().unwrap();
        let late_checkout = current.repos[0]
            .checkouts
            .iter()
            .find(|checkout| checkout.id == late_id)
            .unwrap();
        assert!(!late_checkout.is_missing);
    }

    #[test]
    fn conditional_locate_does_not_mark_a_late_worktree_missing() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        let old_path = temp.path().join("old worktree");
        let moved_path = temp.path().join("moved worktree");
        let late_path = temp.path().join("late worktree");
        init_repo(&primary);
        git(
            &primary,
            &["worktree", "add", "-b", "moved", old_path.to_str().unwrap()],
        );
        let db = database(temp.path());
        let opened = register_folder(&db, &primary).unwrap();
        let repo_id = opened.repos[0].id.clone();
        let missing_id = crate::domain::workspace::checkout_id_for_path(
            &old_path.canonicalize().unwrap().display().to_string(),
        );
        let expected = db
            .git_repo_registration_snapshot(&repo_id)
            .unwrap()
            .expect("the Git repository registration snapshot");

        fs::rename(&old_path, &moved_path).unwrap();
        git(
            &primary,
            &["worktree", "repair", moved_path.to_str().unwrap()],
        );
        let (resolved, focus_id) =
            crate::git::resolve_repository(&moved_path, &crate::persistence::timestamp())
                .unwrap()
                .expect("the relocated worktree resolves");
        git(
            &primary,
            &["worktree", "add", "-b", "late", late_path.to_str().unwrap()],
        );
        register_folder(&db, &late_path).unwrap();
        let late_id = crate::domain::workspace::checkout_id_for_path(
            &late_path.canonicalize().unwrap().display().to_string(),
        );

        assert!(db.load_workspace().unwrap().repos[0]
            .checkouts
            .iter()
            .any(|checkout| checkout.id == late_id && !checkout.is_missing));
        assert!(!db
            .locate_git_checkout_if_unchanged(&resolved, &missing_id, &focus_id, &expected)
            .unwrap());
        let current = db.load_workspace().unwrap();
        assert!(current.repos[0]
            .checkouts
            .iter()
            .any(|checkout| checkout.id == late_id && !checkout.is_missing));
        assert!(current.repos[0]
            .checkouts
            .iter()
            .any(|checkout| checkout.id == missing_id));
    }

    #[test]
    fn conditional_sync_rejects_concurrent_archive_membership_changes() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        let linked = temp.path().join("sibling");
        init_repo(&primary);
        git(
            &primary,
            &["worktree", "add", "-b", "sibling", linked.to_str().unwrap()],
        );
        let db = database(temp.path());
        let opened = register_folder(&db, &primary).unwrap();
        let repo_id = opened.repos[0].id.clone();
        let sibling_id = opened.repos[0]
            .checkouts
            .iter()
            .find(|checkout| !checkout.is_primary)
            .unwrap()
            .id
            .clone();
        let agents = crate::services::agent::AgentService::without_service();
        let snapshot = super::git_repo_sync_snapshot(&db, &agents, &repo_id)
            .unwrap()
            .expect("the Git repository registration snapshot");
        let stale = super::resolve_repo(&db, &repo_id)
            .unwrap()
            .expect("the stale Git snapshot");
        archive_checkout(&db, &agents, &sibling_id).unwrap();

        assert!(
            super::reconcile_git_repo_if_current(&db, &agents, &snapshot, &stale)
                .unwrap()
                .is_none()
        );
        let current = db.load_workspace().unwrap();
        assert_eq!(current.repos[0].checkouts.len(), 1);
        assert_eq!(current.archived_worktrees.len(), 1);
        assert_eq!(current.archived_worktrees[0].id, sibling_id);
    }

    #[test]
    fn conditional_sync_preserves_concurrent_metadata_for_same_checkout_path() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        init_repo(&primary);
        let db = database(temp.path());
        let opened = register_folder(&db, &primary).unwrap();
        let repo_id = opened.repos[0].id.clone();
        let checkout_id = opened.repos[0].checkouts[0].id.clone();
        let agents = crate::services::agent::AgentService::without_service();
        let snapshot = super::git_repo_sync_snapshot(&db, &agents, &repo_id)
            .unwrap()
            .expect("the Git repository registration snapshot");
        let stale = super::resolve_repo(&db, &repo_id)
            .unwrap()
            .expect("the stale Git snapshot");

        let mut concurrent = opened.repos[0].clone();
        let checkout = &mut concurrent.checkouts[0];
        checkout.branch = Some("concurrent-branch".into());
        checkout.head = Some("concurrent-head".into());
        checkout.is_missing = true;
        db.register_git_repo(concurrent, &checkout_id).unwrap();

        assert!(
            super::reconcile_git_repo_if_current(&db, &agents, &snapshot, &stale)
                .unwrap()
                .is_none()
        );
        let current = db.load_workspace().unwrap();
        let checkout = &current.repos[0].checkouts[0];
        assert_eq!(checkout.id, checkout_id);
        assert_eq!(
            checkout.canonical_path,
            opened.repos[0].checkouts[0].canonical_path
        );
        assert_eq!(checkout.branch.as_deref(), Some("concurrent-branch"));
        assert_eq!(checkout.head.as_deref(), Some("concurrent-head"));
        assert!(checkout.is_missing);
    }

    #[test]
    fn sync_repo_reports_a_failed_git_worktree_list() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        init_repo(&primary);
        let db = database(temp.path());
        let opened = register_folder(&db, &primary).unwrap();
        fs::rename(primary.join(".git"), primary.join(".git unavailable")).unwrap();

        let result = sync_repo(
            &db,
            &crate::services::agent::AgentService::without_service(),
            &opened.repos[0].id,
        );

        assert!(result.is_err());
    }

    /// A worktree that is gone from disk stops being one of the rows, the same way a restart
    /// leaves it: as a row that says its directory is missing and can be closed, rather than a row
    /// describing changes nobody can read any more.
    #[test]
    fn sync_repo_marks_a_worktree_removed_outside_the_app_as_missing() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        let linked = temp.path().join("external worktree");
        init_repo(&primary);
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-b",
                "external",
                linked.to_str().unwrap(),
            ],
        );
        let db = database(temp.path());
        let opened = register_folder(&db, &primary).unwrap();
        let repo_id = opened.repos[0].id.clone();
        let worktree_id = opened.repos[0].checkouts[1].id.clone();
        git(&primary, &["worktree", "remove", linked.to_str().unwrap()]);

        let synced = sync_repo(
            &db,
            &crate::services::agent::AgentService::without_service(),
            &repo_id,
        )
        .unwrap()
        .expect("a worktree left");

        let checkout = synced.repos[0]
            .checkouts
            .iter()
            .find(|checkout| checkout.id == worktree_id)
            .expect("the row is still there to close");
        assert!(checkout.is_missing);
    }

    /// An archived worktree is registered and alive but off the panel, so reading the disk again
    /// has to leave it there. Otherwise the shelf emptied itself the moment an agent added a
    /// sibling, and the row the user had put away came back on its own.
    #[test]
    fn sync_repo_keeps_an_archived_worktree_archived() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        let archived = temp.path().join("archived worktree");
        let added = temp.path().join("added worktree");
        init_repo(&primary);
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-b",
                "archived",
                archived.to_str().unwrap(),
            ],
        );
        let db = database(temp.path());
        let opened = register_folder(&db, &archived).unwrap();
        let repo_id = opened.repos[0].id.clone();
        let worktree_id = opened.repos[0].checkouts[1].id.clone();
        archive_checkout(
            &db,
            &crate::services::agent::AgentService::without_service(),
            &worktree_id,
        )
        .unwrap();
        git(
            &primary,
            &["worktree", "add", "-b", "added", added.to_str().unwrap()],
        );

        let synced = sync_repo(
            &db,
            &crate::services::agent::AgentService::without_service(),
            &repo_id,
        )
        .unwrap()
        .expect("a worktree joined");

        assert_eq!(synced.repos[0].checkouts.len(), 2);
        assert!(synced.repos[0]
            .checkouts
            .iter()
            .all(|checkout| checkout.branch.as_deref() != Some("archived")));
        assert!(synced.repos[0]
            .checkouts
            .iter()
            .any(|checkout| checkout.branch.as_deref() == Some("added")));
        assert_eq!(synced.archived_worktrees.len(), 1);
        assert_eq!(synced.archived_worktrees[0].id, worktree_id);
    }

    /// Launching the app re-reads Git's own worktree list and writes it back over what is
    /// registered, so an archived worktree is re-inserted on every start. It has to come
    /// back archived: otherwise archiving would last until the next launch, and the user
    /// would find their panel rearranged by a restart they did nothing to cause.
    #[test]
    fn stale_sync_snapshot_cannot_resurrect_a_checkout_after_close_and_reopen() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        let stale_worktree = temp.path().join("stale worktree");
        init_repo(&primary);
        let db = database(temp.path());
        let opened = register_folder(&db, &primary).unwrap();
        let repo_id = opened.repos[0].id.clone();
        let primary_id = opened.repos[0].checkouts[0].id.clone();
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-b",
                "stale",
                stale_worktree.to_str().unwrap(),
            ],
        );
        let agents = crate::services::agent::AgentService::without_service();
        let snapshot = super::git_repo_sync_snapshot(&db, &agents, &repo_id)
            .unwrap()
            .expect("the repository is still registered");
        let resolved = super::resolve_repo(&db, &repo_id)
            .unwrap()
            .expect("Git still resolves the stale worktree");
        assert_eq!(snapshot.state.repos[0].checkouts.len(), 1);
        assert_eq!(resolved.checkouts.len(), 2);

        git(
            &primary,
            &[
                "worktree",
                "remove",
                "--force",
                stale_worktree.to_str().unwrap(),
            ],
        );
        close_checkout(
            &db,
            &crate::terminal::TerminalBackend::default(),
            &agents,
            &primary_id,
        )
        .unwrap();
        let reopened = register_folder(&db, &primary).unwrap();
        assert_eq!(reopened.repos[0].checkouts.len(), 1);

        let stale_result =
            super::reconcile_git_repo_if_current(&db, &agents, &snapshot, &resolved).unwrap();

        assert!(stale_result.is_none());
        let current = db.load_workspace().unwrap();
        assert_eq!(current.repos[0].checkouts.len(), 1);
        assert_eq!(current.repos[0].checkouts[0].id, primary_id);
    }

    #[test]
    fn a_restart_keeps_an_archived_worktree_archived() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        let linked = temp.path().join("feature worktree");
        init_repo(&primary);
        git(
            &primary,
            &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
        );
        let db = database(temp.path());
        let opened = register_folder(&db, &linked).unwrap();
        let worktree_id = opened
            .repos
            .iter()
            .flat_map(|repo| repo.checkouts.iter())
            .find(|checkout| checkout.branch.as_deref() == Some("feature"))
            .expect("the feature worktree")
            .id
            .clone();

        archive_checkout(
            &db,
            &crate::services::agent::AgentService::without_service(),
            &worktree_id,
        )
        .unwrap();
        let restored = restore(&db, &home_directory(temp.path())).unwrap();

        let repo = restored
            .repos
            .iter()
            .find(|repo| repo.kind == RepoKind::Git)
            .unwrap();
        assert_eq!(repo.checkouts.len(), 1);
        assert!(repo.checkouts[0].is_primary);
        assert_eq!(restored.active_checkout_id, restored.home_checkout_id);
        assert_eq!(restored.archived_worktrees.len(), 1);
        assert_eq!(restored.archived_worktrees[0].id, worktree_id);
    }

    #[test]
    fn plain_directory_stays_a_single_plain_checkout() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("plain");
        fs::create_dir(&path).unwrap();

        let state = register_folder(&database(temp.path()), &path).unwrap();

        assert_eq!(state.repos.len(), 1);
        assert_eq!(state.repos[0].kind, RepoKind::Plain);
        assert_eq!(state.repos[0].checkouts.len(), 1);
        assert!(state.repos[0].checkouts[0].is_primary);
    }

    #[test]
    fn a_corrupt_git_checkout_is_not_registered_as_a_plain_folder() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("repo");
        init_repo(&path);
        fs::write(path.join(".git/config"), "[core\n").unwrap();
        let db = database(temp.path());

        let error = register_folder(&db, &path).expect_err("a corrupt Git checkout must fail");

        assert_eq!(error.code, crate::domain::ipc::IpcErrorCode::GitFailed);
        assert!(db.load_workspace().unwrap().repos.is_empty());
    }

    #[test]
    fn explicitly_opened_submodule_is_standalone_and_bare_repository_is_rejected() {
        let temp = tempdir().unwrap();
        let child = temp.path().join("child");
        let parent = temp.path().join("parent");
        let bare = temp.path().join("bare.git");
        init_repo(&child);
        init_repo(&parent);
        git(
            &parent,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                child.to_str().unwrap(),
                "vendor/child",
            ],
        );
        git(&parent, &["commit", "-am", "add submodule"]);
        fs::create_dir(&bare).unwrap();
        git(&bare, &["init", "--bare"]);

        let state = register_folder(&database(temp.path()), &parent.join("vendor/child")).unwrap();

        assert_eq!(state.repos.len(), 1);
        assert_eq!(
            state.repos[0].root,
            parent
                .join("vendor/child")
                .canonicalize()
                .unwrap()
                .display()
                .to_string()
        );
        assert_eq!(state.repos[0].checkouts.len(), 1);
        let error = register_folder(&database(temp.path()), &bare).unwrap_err();
        assert!(error
            .message
            .contains("Bare repositories are not supported"));
    }

    #[test]
    fn locating_a_moved_worktree_repairs_git_and_migrates_its_checkout_metadata() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        let moved_from = temp.path().join("worktree-old");
        let moved_to = temp.path().join("worktree-new");
        init_repo(&primary);
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                moved_from.to_str().unwrap(),
            ],
        );
        let database = database(temp.path());
        let registered = register_folder(&database, &primary).unwrap();
        let old_id = register_folder(&database, &moved_from)
            .unwrap()
            .active_checkout_id
            .unwrap();
        for (round_id, status) in [
            ("review-round:acked", "acked"),
            ("review-round:dispatched", "dispatched"),
            ("review-round:queued", "queued"),
            ("review-round:dispatching", "dispatching"),
        ] {
            let note_id = format!("review-note:{round_id}");
            database
                .add_review_note(&ReviewNote {
                    id: note_id.clone(),
                    checkout_id: old_id.clone(),
                    path: "tracked.txt".into(),
                    side: "new".into(),
                    line_start: 1,
                    line_end: None,
                    content: format!("Review note for {status}"),
                    code: "initial".into(),
                    status: "draft".into(),
                    code_hash: "test-hash".into(),
                    outdated: false,
                    round_id: None,
                    created_at: "now".into(),
                    updated_at: "now".into(),
                })
                .unwrap();
            let round = ReviewRound {
                id: round_id.into(),
                checkout_id: old_id.clone(),
                session_id: Some("session:historical-agent".into()),
                status: status.into(),
                marker: format!("muster-review:{round_id}"),
                note_ids: vec![note_id],
                created_at: "now".into(),
                updated_at: "now".into(),
            };
            database
                .add_review_round(&round, &round.note_ids, Some("saved prompt"))
                .unwrap();
        }
        let session = Session {
            id: "session:feature-shell".into(),
            session_type: SessionType::Shell,
            checkout_id: old_id.clone(),
            name: "zsh".into(),
            created_at: "now".into(),
            status: SessionStatus::Active,
        };
        database.add_terminal_session(&session).unwrap();
        let layout = CheckoutTerminalLayout {
            active_tab_id: Some("tab:feature".into()),
            tabs: vec![TerminalLayoutTab {
                id: "tab:feature".into(),
                root: TerminalLayoutNode::Session {
                    session_id: session.id.clone(),
                },
            }],
            session_order: vec![session.id.clone()],
        };
        database.save_terminal_layout(&old_id, &layout).unwrap();
        database.mark_file_viewed(&old_id, "README.md").unwrap();
        drop(database);
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        fs::rename(&moved_from, &moved_to).unwrap();

        let located = locate_missing_checkout(
            &database,
            &crate::services::agent::AgentService::without_service(),
            &old_id,
            &moved_to,
        )
        .unwrap();
        let repo = &located.repos[0];
        let new_id = crate::domain::workspace::checkout_id_for_path(
            &moved_to.canonicalize().unwrap().display().to_string(),
        );
        assert_eq!(repo.checkouts.len(), 2);
        assert!(repo.checkouts.iter().all(|checkout| checkout.id != old_id));
        let checkout = repo
            .checkouts
            .iter()
            .find(|checkout| checkout.id == new_id)
            .unwrap();
        assert_eq!(checkout.branch.as_deref(), Some("feature"));
        assert!(!checkout.is_missing);
        // The restart that precedes the relocation already took the session and its layout with
        // it, so what still has to move is the history that outlives a terminal.
        assert!(checkout.sessions.is_empty());
        assert_eq!(located.active_checkout_id.as_deref(), Some(new_id.as_str()));
        assert_eq!(located.active_session_id, None);
        assert_eq!(database.load_terminal_layout(&new_id).unwrap(), None);
        assert_eq!(database.viewed_files(&new_id).unwrap(), ["README.md"]);
        assert_eq!(repo.id, registered.repos[0].id);
        assert!(database.review_notes(&old_id).unwrap().is_empty());
        assert!(database.review_rounds(&old_id).unwrap().is_empty());
        let moved_rounds = database.review_rounds(&new_id).unwrap();
        let moved_notes = database.review_notes(&new_id).unwrap();
        assert_eq!(moved_rounds.len(), 4);
        assert_eq!(moved_notes.len(), 4);
        for (round_id, original_status) in [
            ("review-round:acked", "acked"),
            ("review-round:dispatched", "dispatched"),
            ("review-round:queued", "queued"),
            ("review-round:dispatching", "dispatching"),
        ] {
            let round = moved_rounds
                .iter()
                .find(|round| round.id == round_id)
                .unwrap();
            let note_id = format!("review-note:{round_id}");
            let expected_status = match original_status {
                "queued" | "dispatching" => "relocated",
                other => other,
            };
            assert_eq!(round.checkout_id, new_id);
            assert_eq!(round.status, expected_status);
            assert_eq!(
                round.session_id.as_deref(),
                Some("session:historical-agent")
            );
            assert_eq!(round.marker, format!("muster-review:{round_id}"));
            assert_eq!(round.note_ids, [note_id.as_str()]);
            assert_eq!(
                database
                    .review_round_prompt(round_id, &new_id)
                    .unwrap()
                    .as_deref(),
                Some("saved prompt")
            );
            let note = moved_notes.iter().find(|note| note.id == note_id).unwrap();
            assert_eq!(note.checkout_id, new_id);
            assert_eq!(note.round_id.as_deref(), Some(round_id));
            assert_eq!(note.status, "sent");
            assert_eq!(note.content, format!("Review note for {original_status}"));
        }
    }

    #[test]
    fn missing_plain_checkout_can_only_be_located_at_its_saved_path() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("plain");
        fs::create_dir(&path).unwrap();
        let database = database(temp.path());
        let state = register_folder(&database, &path).unwrap();
        let checkout_id = state.repos[0].checkouts[0].id.clone();
        fs::remove_dir(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let other = temp.path().join("other");
        fs::create_dir(&other).unwrap();

        assert!(locate_missing_checkout(
            &database,
            &crate::services::agent::AgentService::without_service(),
            &checkout_id,
            &other
        )
        .is_err());
        let restored = locate_missing_checkout(
            &database,
            &crate::services::agent::AgentService::without_service(),
            &checkout_id,
            &path,
        )
        .unwrap();
        assert!(!restored.repos[0].checkouts[0].is_missing);
    }

    #[test]
    fn locating_a_moved_plain_directory_rekeys_the_workspace_and_preserves_history() {
        let temp = tempdir().unwrap();
        let old_path = temp.path().join("plain-old");
        let new_path = temp.path().join("plain-new");
        fs::create_dir(&old_path).unwrap();
        let database = database(temp.path());
        let state = register_folder(&database, &old_path).unwrap();
        let old_id = state.repos[0].checkouts[0].id.clone();
        let session = Session {
            id: "session:plain-shell".into(),
            session_type: SessionType::Shell,
            checkout_id: old_id.clone(),
            name: "zsh".into(),
            created_at: "now".into(),
            status: SessionStatus::Active,
        };
        database.add_terminal_session(&session).unwrap();
        database.mark_file_viewed(&old_id, "notes.md").unwrap();
        drop(database);
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        fs::rename(&old_path, &new_path).unwrap();

        let relocated = locate_missing_checkout(
            &database,
            &crate::services::agent::AgentService::without_service(),
            &old_id,
            &new_path,
        )
        .unwrap();
        let repo = &relocated.repos[0];
        let new_id = crate::domain::workspace::checkout_id_for_path(
            &new_path.canonicalize().unwrap().display().to_string(),
        );
        assert_eq!(
            repo.id,
            crate::domain::workspace::repo_id_for_path(&repo.root)
        );
        assert_eq!(
            repo.root,
            new_path.canonicalize().unwrap().display().to_string()
        );
        assert!(repo.checkouts.iter().all(|checkout| checkout.id != old_id));
        assert_eq!(repo.checkouts[0].id, new_id);
        assert!(repo.checkouts[0].sessions.is_empty());
        assert_eq!(database.viewed_files(&new_id).unwrap(), ["notes.md"]);
        assert_eq!(
            relocated.active_checkout_id.as_deref(),
            Some(new_id.as_str())
        );
        assert_eq!(relocated.active_session_id, None);
    }

    #[test]
    fn closing_a_missing_location_removes_only_its_workspace_registration() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("plain");
        fs::create_dir(&path).unwrap();
        let database = database(temp.path());
        let state = register_folder(&database, &path).unwrap();
        let checkout_id = state.repos[0].checkouts[0].id.clone();
        fs::remove_dir(&path).unwrap();

        let closed = close_missing_checkout(
            &database,
            &crate::terminal::TerminalBackend::default(),
            &crate::services::agent::AgentService::without_service(),
            &checkout_id,
        )
        .unwrap();
        assert!(closed.repos.is_empty());
    }

    #[test]
    fn closing_a_missing_git_worktree_drops_its_entry_and_leaves_the_disk_alone() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        let linked = temp.path().join("gone");
        init_repo(&primary);
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-b",
                "temporary",
                linked.to_str().unwrap(),
            ],
        );
        let database = database(temp.path());
        let registered = register_folder(&database, &primary).unwrap();
        let primary_id = registered.repos[0].checkouts[0].id.clone();
        let linked_id = register_folder(&database, &linked)
            .unwrap()
            .active_checkout_id
            .expect("opening the worktree selects it");
        database
            .add_active_session(&Session {
                id: "session:gone".into(),
                session_type: SessionType::Shell,
                checkout_id: linked_id.clone(),
                name: "shell".into(),
                created_at: "now".into(),
                status: SessionStatus::Active,
            })
            .unwrap();
        fs::remove_dir_all(&linked).unwrap();
        let backend = crate::terminal::TerminalBackend::default();

        // A live session still blocks the close, exactly as it does for a worktree on disk.
        let refused = close_missing_checkout(
            &database,
            &backend,
            &crate::services::agent::AgentService::without_service(),
            &linked_id,
        )
        .unwrap_err();
        assert!(refused.message.contains("active terminal sessions"));
        assert_eq!(
            database.load_workspace().unwrap().repos[0].checkouts.len(),
            2
        );

        database.remove_terminal_session("session:gone").unwrap();
        let closed = close_missing_checkout(
            &database,
            &backend,
            &crate::services::agent::AgentService::without_service(),
            &linked_id,
        )
        .unwrap();

        // Only the entry leaves the list: the repository, its file and its branch stay.
        assert_eq!(closed.repos[0].checkouts.len(), 1);
        assert_eq!(closed.repos[0].checkouts[0].id, primary_id);
        assert_eq!(
            closed.active_checkout_id.as_deref(),
            Some(closed.repos[0].checkouts[0].id.as_str())
        );
        assert!(primary.join("tracked.txt").exists());
        git(
            &primary,
            &["show-ref", "--verify", "--quiet", "refs/heads/temporary"],
        );
    }

    #[test]
    fn closing_a_location_that_still_exists_is_refused() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("plain");
        fs::create_dir(&path).unwrap();
        let database = database(temp.path());
        let state = register_folder(&database, &path).unwrap();
        let checkout_id = state.repos[0].checkouts[0].id.clone();

        let error = close_missing_checkout(
            &database,
            &crate::terminal::TerminalBackend::default(),
            &crate::services::agent::AgentService::without_service(),
            &checkout_id,
        )
        .unwrap_err();

        assert!(error
            .message
            .contains("only a missing checkout can be closed"));
        assert_eq!(database.load_workspace().unwrap().repos.len(), 1);
    }

    #[test]
    fn closing_a_live_worktree_takes_the_entry_off_the_list_and_nothing_else() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        let linked = temp.path().join("feature");
        init_repo(&primary);
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-b",
                "temporary",
                linked.to_str().unwrap(),
            ],
        );
        fs::write(linked.join("work-in-progress.txt"), "uncommitted\n").unwrap();
        let database = database(temp.path());
        let registered = register_folder(&database, &primary).unwrap();
        let primary_id = registered.repos[0].checkouts[0].id.clone();
        let linked_id = register_folder(&database, &linked)
            .unwrap()
            .active_checkout_id
            .expect("opening the worktree selects it");
        let backend = crate::terminal::TerminalBackend::default();

        let closed = close_checkout(
            &database,
            &backend,
            &crate::services::agent::AgentService::without_service(),
            &linked_id,
        )
        .unwrap();

        // The row is gone and everything it pointed at is still there, uncommitted work
        // included: opening the folder again is what brings the workdir back.
        assert_eq!(closed.repos[0].checkouts.len(), 1);
        assert_eq!(closed.repos[0].checkouts[0].id, primary_id);
        assert_eq!(
            closed.active_checkout_id.as_deref(),
            Some(closed.repos[0].checkouts[0].id.as_str())
        );
        assert!(linked.join("work-in-progress.txt").exists());
        // Git still knows the worktree, because Muster only forgot it: `git worktree list` goes
        // on naming the directory, and so does the branch.
        let listed = git_read(&primary, &["worktree", "list", "--porcelain"]);
        assert!(listed.contains(linked.to_str().unwrap()));
        git_read(
            &primary,
            &["show-ref", "--verify", "--quiet", "refs/heads/temporary"],
        );
    }

    #[test]
    fn closing_a_live_worktree_is_refused_while_its_session_is_active() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        let linked = temp.path().join("feature");
        init_repo(&primary);
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-b",
                "temporary",
                linked.to_str().unwrap(),
            ],
        );
        let database = database(temp.path());
        register_folder(&database, &primary).unwrap();
        let linked_id = register_folder(&database, &linked)
            .unwrap()
            .active_checkout_id
            .expect("opening the worktree selects it");
        database
            .add_active_session(&Session {
                id: "session:live".into(),
                session_type: SessionType::Shell,
                checkout_id: linked_id.clone(),
                name: "shell".into(),
                created_at: "now".into(),
                status: SessionStatus::Active,
            })
            .unwrap();

        let refused = close_checkout(
            &database,
            &crate::terminal::TerminalBackend::default(),
            &crate::services::agent::AgentService::without_service(),
            &linked_id,
        )
        .unwrap_err();

        // A terminal still attached to the directory would outlive the row that names it.
        assert!(refused.message.contains("active terminal sessions"));
        assert_eq!(
            database.load_workspace().unwrap().repos[0].checkouts.len(),
            2
        );
    }

    #[test]
    fn closing_a_location_that_is_gone_still_prunes_the_worktree_pointing_at_it() {
        let temp = tempdir().unwrap();
        let primary = temp.path().join("repo");
        let linked = temp.path().join("gone");
        init_repo(&primary);
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-b",
                "temporary",
                linked.to_str().unwrap(),
            ],
        );
        let database = database(temp.path());
        register_folder(&database, &primary).unwrap();
        let linked_id = register_folder(&database, &linked)
            .unwrap()
            .active_checkout_id
            .expect("opening the worktree selects it");
        fs::remove_dir_all(&linked).unwrap();

        // The same row closes both ways, so the one command cannot skip the prune a directory
        // that is gone would otherwise leave behind in the repository's worktree list.
        let closed = close_checkout(
            &database,
            &crate::terminal::TerminalBackend::default(),
            &crate::services::agent::AgentService::without_service(),
            &linked_id,
        )
        .unwrap();

        assert_eq!(closed.repos[0].checkouts.len(), 1);
        let listed = git_read(&primary, &["worktree", "list", "--porcelain"]);
        assert!(!listed.contains(linked.to_str().unwrap()));
    }

    #[test]
    fn closing_a_missing_checkout_is_refused_while_its_session_is_active() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("plain");
        fs::create_dir(&path).unwrap();
        let database = database(temp.path());
        let state = register_folder(&database, &path).unwrap();
        let checkout_id = state.repos[0].checkouts[0].id.clone();
        database
            .add_terminal_session(&Session {
                id: "session:missing".into(),
                session_type: SessionType::Shell,
                checkout_id: checkout_id.clone(),
                name: "shell".into(),
                created_at: "now".into(),
                status: SessionStatus::Active,
            })
            .unwrap();
        fs::remove_dir(&path).unwrap();

        let error = close_missing_checkout(
            &database,
            &crate::terminal::TerminalBackend::default(),
            &crate::services::agent::AgentService::without_service(),
            &checkout_id,
        )
        .unwrap_err();
        assert!(error.message.contains("active terminal sessions"));
        assert_eq!(database.load_workspace().unwrap().repos.len(), 1);
    }
}
