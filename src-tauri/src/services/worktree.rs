use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
};

use serde::{Deserialize, Serialize};

use crate::{
    domain::{
        ipc::{IpcError, IpcErrorCode},
        workspace::{Checkout, Repo, Session, SessionStatus, SessionType, WorkspaceState},
    },
    persistence::{timestamp, Database},
    services::{checkout::resolve_checkout_path, folder, git},
    terminal::TerminalBackend,
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeDefaults {
    pub location: String,
    pub default_branch: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedWorktree {
    pub workspace: WorkspaceState,
    pub checkout_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveWorktreeSession {
    pub id: String,
    #[serde(rename = "type")]
    pub session_type: SessionType,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeRemovalInfo {
    pub checkout_id: String,
    pub is_primary: bool,
    pub is_missing: bool,
    pub branch: Option<String>,
    pub dirty_files: Vec<String>,
    pub unmerged_commits: u32,
    pub active_sessions: Vec<ActiveWorktreeSession>,
    pub active_agent_sessions: Vec<ActiveWorktreeSession>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeRemovalConfirmation {
    pub confirm_dirty: bool,
    pub confirmed_dirty_files: Vec<String>,
    pub confirmed_session_ids: Vec<String>,
    pub expected_branch: Option<String>,
    pub expected_unmerged_commits: u32,
    pub delete_branch: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovedWorktree {
    pub workspace: WorkspaceState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Clone)]
struct Context {
    repo: Repo,
    checkout: Checkout,
    root: PathBuf,
}

pub fn defaults(database: &Database, checkout_id: &str) -> Result<WorktreeDefaults, IpcError> {
    let context = context(database, checkout_id)?;
    ensure_available_source(&context)?;
    require_main_branch(&context.root)?;
    Ok(WorktreeDefaults {
        location: Path::new(&context.repo.root)
            .join(".worktrees")
            .display()
            .to_string(),
        default_branch: "main".into(),
    })
}

pub fn create(
    database: &Database,
    checkout_id: &str,
    task_name: &str,
    branch: &str,
    location: &Path,
) -> Result<CreatedWorktree, IpcError> {
    let context = context(database, checkout_id)?;
    ensure_available_source(&context)?;
    let task_name = sanitize_component(task_name)?;
    let branch = branch.trim();
    if branch.is_empty() || !valid_branch(&context.root, branch)? {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "enter a valid Git branch name",
        ));
    }

    let default = require_main_branch(&context.root)?;

    let entries = worktrees(&context.root)?;
    if let Some(holder) = entries
        .iter()
        .find(|entry| entry.branch.as_deref() == Some(branch))
    {
        return Err(branch_in_use(branch, holder, &context.repo));
    }

    let destination_parent = prepare_location(location, Path::new(&context.repo.root))?;
    let repo_worktrees = Path::new(&context.repo.root).join(".worktrees");
    if location == repo_worktrees && destination_parent != repo_worktrees {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "the repository's .worktrees location cannot be a symbolic link",
        ));
    }
    if destination_parent != repo_worktrees {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "worktrees must be created in the repository's .worktrees directory",
        ));
    }
    let requested_destination = destination_parent.join(&task_name);
    let requested_checkout_id = crate::domain::workspace::checkout_id_for_path(
        &requested_destination.display().to_string(),
    );
    let registration_snapshot = database
        .git_repo_registration_snapshot(&context.repo.id)
        .map_err(operation_error)?
        .ok_or_else(|| {
            IpcError::new(
                IpcErrorCode::InvalidCheckout,
                "repository registration changed; refresh and retry creating this worktree",
            )
        })?;
    if registration_snapshot.is_archived_checkout(&requested_checkout_id) {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "an archived worktree already uses this location; restore or close it before creating another worktree here",
        ));
    }
    ensure_worktrees_ignored(Path::new(&context.repo.root))?;
    fs::create_dir_all(&destination_parent).map_err(|error| {
        IpcError::new(
            IpcErrorCode::PermissionDenied,
            format!("could not create the worktree location: {error}"),
        )
    })?;
    let destination_parent = destination_parent.canonicalize().map_err(|error| {
        IpcError::new(
            IpcErrorCode::InvalidPath,
            format!("could not resolve the worktree location: {error}"),
        )
    })?;
    if destination_parent != repo_worktrees {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "worktrees must be created in the repository's .worktrees directory",
        ));
    }
    let destination = destination_parent.join(task_name);
    if destination.exists() {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            format!(
                "worktree destination already exists: {}",
                destination.display()
            ),
        ));
    }

    let result = git_write_output(
        &context.root,
        vec![
            "worktree".into(),
            "add".into(),
            "-b".into(),
            branch.into(),
            destination.clone().into_os_string(),
            default.reference.clone().into(),
        ],
    )?;
    if !result.status.success() {
        if let Some(holder) = worktrees(&context.root)?
            .iter()
            .find(|entry| entry.branch.as_deref() == Some(branch))
        {
            return Err(branch_in_use(branch, holder, &context.repo));
        }
        return Err(git_error("could not create worktree", &result));
    }

    let registered = folder::open_folder(&destination)
        .and_then(|opened| {
        let (repo, focus_checkout_id) =
            crate::git::resolve_repository(Path::new(&opened.path), &timestamp())?.ok_or_else(
                || {
                    IpcError::new(
                        IpcErrorCode::NotRepository,
                        "created worktree is not a Git checkout",
                    )
                },
            )?;
        register_created_worktree(
            database,
            &repo,
            &focus_checkout_id,
            &registration_snapshot,
        )
    })
        .map_err(|error| {
            IpcError::new(
                error.code,
                format!(
                    "Git created the worktree at {} but workspace registration failed: {}. The worktree and branch were left untouched.",
                    destination.display(),
                    error.message
                ),
            )
        })?;
    let created = registered
        .repos
        .iter()
        .flat_map(|repo| repo.checkouts.iter())
        .find(|checkout| checkout.canonical_path == destination.display().to_string())
        .ok_or_else(|| {
            IpcError::new(
                IpcErrorCode::OperationFailed,
                "Git created the worktree, but Muster could not register it",
            )
        })?;
    Ok(CreatedWorktree {
        checkout_id: created.id.clone(),
        workspace: registered,
    })
}

fn register_created_worktree(
    database: &Database,
    repo: &Repo,
    focus_checkout_id: &str,
    snapshot: &crate::persistence::GitRepoRegistrationSnapshot,
) -> Result<WorkspaceState, IpcError> {
    database
        .register_git_repo_if_unchanged(repo, focus_checkout_id, Some(snapshot))
        .map_err(operation_error)?
        .ok_or_else(|| {
            IpcError::new(
                IpcErrorCode::InvalidCheckout,
                "workspace registrations changed during worktree creation; refresh and retry",
            )
        })
}

pub fn removal_info(
    database: &Database,
    agents: &crate::services::agent::AgentService,
    checkout_id: &str,
) -> Result<WorktreeRemovalInfo, IpcError> {
    agents.with_checkout_operation(checkout_id, || {
        removal_info_locked(database, agents, checkout_id)
    })
}

fn removal_info_locked(
    database: &Database,
    agents: &crate::services::agent::AgentService,
    checkout_id: &str,
) -> Result<WorktreeRemovalInfo, IpcError> {
    let context = context(database, checkout_id)?;
    let (branch, is_missing) = if context.checkout.is_missing {
        (context.checkout.branch.clone(), true)
    } else {
        let entry = worktrees(&context.root)?
            .into_iter()
            .find(|entry| paths_equal(entry.path.as_deref(), &context.checkout.canonical_path))
            .ok_or_else(|| {
                IpcError::new(
                    IpcErrorCode::InvalidCheckout,
                    "Git no longer lists this checkout; refresh the workspace before removing it",
                )
            })?;
        (entry.branch.or(context.checkout.branch.clone()), false)
    };
    let branch = match branch {
        Some(branch) if local_branch_exists(&context.root, &branch)? => Some(branch),
        _ => None,
    };

    let dirty_files = if is_missing {
        Vec::new()
    } else {
        let status = checked_git(
            &context.root,
            ["status", "--porcelain=v2", "-z", "--untracked-files=all"],
            "could not inspect worktree changes",
        )?;
        git::parse_porcelain_v2(&status.stdout)?
            .into_values()
            .map(|file| file.path)
            .collect()
    };

    let unmerged_commits = if let Some(branch) = branch.as_deref() {
        let default = git::resolve_default_ref(
            &context.root,
            context.repo.default_branch.as_deref(),
            Some(Path::new(&context.repo.root)),
        )?;
        let ref_name = format!("refs/heads/{branch}");
        let output = checked_git(
            &context.root,
            [
                "rev-list",
                "--count",
                &format!("{}..{ref_name}", default.reference),
            ],
            "could not count commits not merged into the default branch",
        )?;
        output_text(&output).parse::<u32>().map_err(|error| {
            IpcError::new(
                IpcErrorCode::GitFailed,
                format!("Git returned an invalid unmerged commit count: {error}"),
            )
        })?
    } else {
        0
    };

    let active_sessions = context
        .checkout
        .sessions
        .iter()
        .filter(|session| {
            session.status == SessionStatus::Active && session.session_type == SessionType::Shell
        })
        .map(ActiveWorktreeSession::from)
        .collect();
    let active_agent_sessions = agents
        .active_worktree_agent_sessions(&context.checkout.id)
        .map_err(crate::services::agent::map_error)?
        .into_iter()
        .map(|session| ActiveWorktreeSession {
            id: session.id,
            session_type: SessionType::Agent,
            name: session.title,
        })
        .collect();
    Ok(WorktreeRemovalInfo {
        checkout_id: context.checkout.id,
        is_primary: context.checkout.is_primary,
        is_missing,
        branch,
        dirty_files,
        unmerged_commits,
        active_sessions,
        active_agent_sessions,
    })
}

pub fn remove(
    database: &Database,
    backend: &TerminalBackend,
    agents: &crate::services::agent::AgentService,
    checkout_id: &str,
    confirmation: &WorktreeRemovalConfirmation,
) -> Result<RemovedWorktree, IpcError> {
    agents.with_checkout_operation(checkout_id, || {
        remove_locked(database, backend, agents, checkout_id, confirmation)
    })
}

fn remove_locked(
    database: &Database,
    backend: &TerminalBackend,
    agents: &crate::services::agent::AgentService,
    checkout_id: &str,
    confirmation: &WorktreeRemovalConfirmation,
) -> Result<RemovedWorktree, IpcError> {
    database
        .ensure_not_home_checkout(checkout_id)
        .map_err(operation_error)?;
    let _removal = agents
        .reserve_worktree_removal(checkout_id)
        .map_err(crate::services::agent::map_error)?;
    let info = removal_info_locked(database, agents, checkout_id)?;
    if info.is_primary {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "the primary checkout cannot be removed",
        ));
    }
    if !info.active_agent_sessions.is_empty() {
        let agents = info
            .active_agent_sessions
            .iter()
            .map(|session| session.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            format!("stop the active agent session(s) before removing this worktree: {agents}"),
        ));
    }
    if info.branch != confirmation.expected_branch
        || info.unmerged_commits != confirmation.expected_unmerged_commits
    {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "the worktree branch or unmerged commit count changed; review the removal safeguards and retry",
        ));
    }
    if info.dirty_files != confirmation.confirmed_dirty_files {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "the worktree changes changed; review the dirty files and confirm again",
        ));
    }
    if !info.dirty_files.is_empty() && !confirmation.confirm_dirty {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "worktree has uncommitted changes; commit or stash them in a shell, or explicitly confirm removal",
        ));
    }
    let mut actual_ids: Vec<_> = info
        .active_sessions
        .iter()
        .map(|session| session.id.clone())
        .collect();
    let mut confirmed_ids = confirmation.confirmed_session_ids.clone();
    actual_ids.sort();
    confirmed_ids.sort();
    if actual_ids != confirmed_ids {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "the active session list changed; review and confirm the current sessions before removing this worktree",
        ));
    }

    let context = context(database, checkout_id)?;
    let management_root = database
        .existing_git_checkout(&context.repo.id)
        .map_err(operation_error)?
        .and_then(|path| path.canonicalize().ok())
        .filter(|path| path != Path::new(&context.checkout.canonical_path))
        .unwrap_or_else(|| context.root.clone());
    for session_id in &actual_ids {
        if !backend.close(session_id).map_err(operation_error)? {
            return Err(IpcError::new(
                IpcErrorCode::InvalidCheckout,
                "an active terminal session is no longer available; refresh and retry",
            ));
        }
        database
            .remove_terminal_session(session_id)
            .map_err(operation_error)?;
        agents.pi().forget_terminal(session_id);
    }

    agents.stop_for_worktree_removal(checkout_id);

    if info.is_missing {
        let pruned = git_write_output(
            &management_root,
            vec![
                "worktree".into(),
                "prune".into(),
                "--expire".into(),
                "now".into(),
            ],
        )?;
        if !pruned.status.success() {
            return Err(git_error("could not prune the missing worktree", &pruned));
        }
    } else {
        let mut args: Vec<OsString> = vec!["worktree".into(), "remove".into()];
        if !info.dirty_files.is_empty() {
            args.push("--force".into());
        }
        args.push(PathBuf::from(&context.checkout.canonical_path).into_os_string());
        let output = git_write_output(&management_root, args)?;
        if !output.status.success() {
            return Err(git_error("could not remove worktree", &output));
        }
    }

    let mut branch_error = None;
    if confirmation.delete_branch {
        if let Some(branch) = info.branch.as_deref() {
            let output = git_write_output(
                &management_root,
                vec!["branch".into(), "-D".into(), "--".into(), branch.into()],
            )?;
            if !output.status.success() {
                branch_error = Some(git_error(
                    "worktree was removed, but branch deletion failed",
                    &output,
                ));
            }
        }
    }
    let workspace = database
        .remove_checkout(&context.repo.id, checkout_id)
        .map_err(operation_error)?;
    _removal.commit();
    Ok(RemovedWorktree {
        workspace,
        warning: branch_error.map(|error| error.message),
    })
}

impl From<&Session> for ActiveWorktreeSession {
    fn from(session: &Session) -> Self {
        Self {
            id: session.id.clone(),
            session_type: session.session_type.clone(),
            name: session.name.clone(),
        }
    }
}

fn context(database: &Database, checkout_id: &str) -> Result<Context, IpcError> {
    let (repo, checkout) = database
        .load_registered_checkout(checkout_id)
        .map_err(operation_error)?
        .ok_or_else(|| {
            IpcError::new(
                IpcErrorCode::InvalidCheckout,
                "checkout ID is not registered",
            )
        })?;
    if repo.kind != crate::domain::workspace::RepoKind::Git {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "worktrees are only available for Git repositories",
        ));
    }
    let root = if checkout.is_missing {
        let path = database
            .existing_git_checkout(&repo.id)
            .map_err(operation_error)?
            .ok_or_else(|| {
                IpcError::new(
                    IpcErrorCode::FolderMissing,
                    "no available checkout exists to manage this repository's worktrees",
                )
            })?;
        path.canonicalize().map_err(|error| {
            IpcError::new(
                IpcErrorCode::FolderMissing,
                format!("repository checkout is unavailable: {error}"),
            )
        })?
    } else {
        resolve_checkout_path(&repo, checkout_id, Path::new("."))?
    };
    Ok(Context {
        repo: repo.clone(),
        checkout: checkout.clone(),
        root,
    })
}

fn ensure_available_source(context: &Context) -> Result<(), IpcError> {
    if context.checkout.is_missing {
        return Err(IpcError::new(
            IpcErrorCode::FolderMissing,
            "cannot create a worktree from a missing checkout",
        ));
    }
    Ok(())
}

#[derive(Default)]
struct WorktreeEntry {
    path: Option<PathBuf>,
    branch: Option<String>,
}

pub(crate) fn registered_paths(root: &Path) -> Result<BTreeSet<PathBuf>, IpcError> {
    Ok(worktrees(root)?
        .into_iter()
        .filter_map(|entry| entry.path)
        .map(|path| path.canonicalize().unwrap_or(path))
        .collect())
}

fn worktrees(root: &Path) -> Result<Vec<WorktreeEntry>, IpcError> {
    let output = checked_git(
        root,
        ["worktree", "list", "--porcelain", "-z"],
        "could not list Git worktrees",
    )?;
    let mut result = Vec::new();
    let mut current = WorktreeEntry::default();
    for line in output.stdout.split(|byte| *byte == 0) {
        if line.is_empty() {
            if current.path.is_some() {
                result.push(current);
            }
            current = WorktreeEntry::default();
            continue;
        }
        let line = String::from_utf8_lossy(line);
        if let Some(value) = line.strip_prefix("worktree ") {
            current.path = Some(PathBuf::from(value));
        } else if let Some(value) = line.strip_prefix("branch refs/heads/") {
            current.branch = Some(value.to_owned());
        }
    }
    if current.path.is_some() {
        result.push(current);
    }
    Ok(result)
}

fn paths_equal(path: Option<&Path>, stored: &str) -> bool {
    path.is_some_and(|path| {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        path == Path::new(stored)
    })
}

fn branch_in_use(branch: &str, holder: &WorktreeEntry, repo: &Repo) -> IpcError {
    let path = holder
        .path
        .as_deref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "an unknown checkout".into());
    let label = repo
        .checkouts
        .iter()
        .find(|checkout| paths_equal(holder.path.as_deref(), &checkout.canonical_path))
        .map(|checkout| {
            format!(
                "{} checkout ({})",
                if checkout.is_primary {
                    "primary"
                } else {
                    "worktree"
                },
                checkout.canonical_path
            )
        })
        .unwrap_or_else(|| format!("checkout at {path}"));
    IpcError::new(
        IpcErrorCode::GitFailed,
        format!("branch '{branch}' is already checked out in {label}"),
    )
}

fn valid_branch(root: &Path, branch: &str) -> Result<bool, IpcError> {
    let reference = format!("refs/heads/{branch}");
    let output = git::run_git_read(
        root,
        &["check-ref-format", &reference],
        git::GIT_READ_TIMEOUT,
    )?;
    if output.status.success() {
        Ok(true)
    } else if output.status.code() == Some(1)
        && output.stdout.is_empty()
        && output.stderr.is_empty()
    {
        Ok(false)
    } else {
        Err(git::git_error("could not validate Git branch", &output))
    }
}

fn require_main_branch(root: &Path) -> Result<git::DefaultRef, IpcError> {
    if local_branch_exists(root, "main")? {
        return Ok(git::DefaultRef {
            reference: "refs/heads/main".into(),
            branch: "main".into(),
        });
    }
    let reference = "refs/remotes/origin/main";
    let output = git::run_git_read(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "refs/remotes/origin/main^{commit}",
        ],
        git::GIT_READ_TIMEOUT,
    )?;
    if output.status.success() {
        return Ok(git::DefaultRef {
            reference: reference.into(),
            branch: "main".into(),
        });
    }
    if output.status.code() == Some(1) && output.stdout.is_empty() && output.stderr.is_empty() {
        return Err(IpcError::new(
            IpcErrorCode::DefaultBranchUnknown,
            "Git branch 'main' does not exist; create or fetch it before creating a worktree",
        ));
    }
    Err(git::git_error(
        "could not resolve Git branch 'main'",
        &output,
    ))
}

fn local_branch_exists(root: &Path, branch: &str) -> Result<bool, IpcError> {
    if !valid_branch(root, branch)? {
        return Ok(false);
    }
    let reference = format!("refs/heads/{branch}^{{commit}}");
    let output = git::run_git_read(
        root,
        &["rev-parse", "--verify", "--quiet", &reference],
        git::GIT_READ_TIMEOUT,
    )?;
    if output.status.success() {
        Ok(true)
    } else if output.status.code() == Some(1)
        && output.stdout.is_empty()
        && output.stderr.is_empty()
    {
        Ok(false)
    } else {
        Err(git::git_error("could not read local Git branch", &output))
    }
}

fn sanitize_component(value: &str) -> Result<String, IpcError> {
    let mut sanitized = String::new();
    for character in value.trim().chars() {
        if character.is_alphanumeric() || matches!(character, '-' | '_' | '.' | ' ') {
            sanitized.push(character);
        } else if !sanitized.ends_with('-') {
            sanitized.push('-');
        }
    }
    let sanitized = sanitized.trim_matches([' ', '.']).trim_start_matches('.');
    let mut sanitized = sanitized.chars().take(80).collect::<String>();
    while sanitized.len() > 180 {
        sanitized.pop();
    }
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "worktree name must contain a letter or number",
        ));
    }
    Ok(sanitized)
}

fn prepare_location(location: &Path, repo_root: &Path) -> Result<PathBuf, IpcError> {
    if !location.is_absolute()
        || location
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "worktree location must be an absolute path without '..'",
        ));
    }
    let mut existing = location;
    let mut missing = Vec::new();
    while !existing.exists() {
        if fs::symlink_metadata(existing).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(IpcError::new(
                IpcErrorCode::InvalidPath,
                "worktree location contains a broken symbolic link",
            ));
        }
        let name = existing.file_name().ok_or_else(|| {
            IpcError::new(
                IpcErrorCode::InvalidPath,
                "worktree location is unavailable",
            )
        })?;
        missing.push(name.to_os_string());
        existing = existing.parent().ok_or_else(|| {
            IpcError::new(
                IpcErrorCode::InvalidPath,
                "worktree location is unavailable",
            )
        })?;
    }
    let mut resolved = existing.canonicalize().map_err(|error| {
        IpcError::new(
            IpcErrorCode::InvalidPath,
            format!("could not resolve worktree location: {error}"),
        )
    })?;
    if !resolved.is_dir() {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "worktree location must be a directory",
        ));
    }
    for component in missing.iter().rev() {
        resolved.push(component);
    }
    if resolved.starts_with(repo_root) && resolved != repo_root.join(".worktrees") {
        return Err(IpcError::new(
            IpcErrorCode::PathOutsideCheckout,
            "worktrees inside the repository must use its .worktrees directory",
        ));
    }
    Ok(resolved)
}

fn ensure_worktrees_ignored(root: &Path) -> Result<(), IpcError> {
    let output = checked_git(
        root,
        ["rev-parse", "--git-path", "info/exclude"],
        "could not locate Git exclude file",
    )?;
    let raw_path = PathBuf::from(output_text(&output));
    let path = if raw_path.is_absolute() {
        raw_path
    } else {
        root.join(raw_path)
    };
    let git_dir = checked_git(
        root,
        ["rev-parse", "--absolute-git-dir"],
        "could not locate Git metadata",
    )?;
    let git_dir = PathBuf::from(output_text(&git_dir))
        .canonicalize()
        .map_err(|error| {
            IpcError::new(
                IpcErrorCode::InvalidPath,
                format!("could not resolve Git metadata directory: {error}"),
            )
        })?;
    let git_common_dir = checked_git(
        root,
        ["rev-parse", "--git-common-dir"],
        "could not locate shared Git metadata",
    )?;
    let git_common_dir = PathBuf::from(output_text(&git_common_dir));
    let git_common_dir = if git_common_dir.is_absolute() {
        git_common_dir
    } else {
        root.join(git_common_dir)
    }
    .canonicalize()
    .map_err(|error| {
        IpcError::new(
            IpcErrorCode::InvalidPath,
            format!("could not resolve shared Git metadata directory: {error}"),
        )
    })?;
    let exclude_parent = path.parent().ok_or_else(|| {
        IpcError::new(
            IpcErrorCode::InvalidPath,
            "Git exclude file has no parent directory",
        )
    })?;
    let exclude_parent = exclude_parent.canonicalize().map_err(|error| {
        IpcError::new(
            IpcErrorCode::InvalidPath,
            format!("could not resolve Git info directory: {error}"),
        )
    })?;
    if !exclude_parent.starts_with(&git_dir) && !exclude_parent.starts_with(&git_common_dir) {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "Git's local exclude file is outside the repository metadata directory",
        ));
    }
    let contents = match fs::read(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            return Err(IpcError::new(
                IpcErrorCode::PermissionDenied,
                format!("could not read Git's local exclude file: {error}"),
            ));
        }
    };
    let already_ignored = contents
        .split(|byte| *byte == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .any(|line| {
            matches!(
                line,
                b"/.worktrees/" | b"/.worktrees" | b".worktrees/" | b".worktrees"
            )
        });
    if already_ignored {
        return Ok(());
    }
    if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "Git's local exclude file is a symbolic link; refusing to modify a file outside the repository metadata",
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            IpcError::new(
                IpcErrorCode::PermissionDenied,
                format!("could not create Git's local info directory: {error}"),
            )
        })?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| {
            IpcError::new(
                IpcErrorCode::PermissionDenied,
                format!("could not update Git's local exclude file: {error}"),
            )
        })?;
    if !contents.is_empty() && !contents.ends_with(b"\n") {
        file.write_all(b"\n").map_err(|error| {
            IpcError::new(
                IpcErrorCode::PermissionDenied,
                format!("could not update Git's local exclude file: {error}"),
            )
        })?;
    }
    file.write_all(b"/.worktrees/\n").map_err(|error| {
        IpcError::new(
            IpcErrorCode::PermissionDenied,
            format!("could not update Git's local exclude file: {error}"),
        )
    })
}

fn git_write_output(root: &Path, args: Vec<OsString>) -> Result<Output, IpcError> {
    Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|error| {
            IpcError::new(
                IpcErrorCode::GitFailed,
                format!("could not start Git: {error}"),
            )
        })
}

fn checked_git<const N: usize>(
    root: &Path,
    args: [&str; N],
    action: &str,
) -> Result<Output, IpcError> {
    let output = git::run_git_read(root, &args, git::GIT_READ_TIMEOUT)?;
    if !output.status.success() {
        return Err(git_error(action, &output));
    }
    Ok(output)
}

fn git_error(action: &str, output: &Output) -> IpcError {
    git::git_error(action, output)
}

fn output_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout)
        .trim_end_matches(['\r', '\n'])
        .to_owned()
}

fn operation_error(error: String) -> IpcError {
    IpcError::new(IpcErrorCode::OperationFailed, error)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{BufRead, BufReader, Read, Write},
        net::{TcpListener, TcpStream},
        os::unix::process::ExitStatusExt,
        path::{Path, PathBuf},
        process::{Command, ExitStatus, Output},
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc, Arc,
        },
        thread,
        time::Duration,
    };

    use rusqlite::Connection;
    use tempfile::tempdir;

    use crate::{
        domain::{
            agent::AgentSession,
            workspace::{Session, SessionStatus, SessionType},
        },
        persistence::Database,
        services::{
            agent::AgentService,
            git::{run_git_read, GIT_READ_TIMEOUT},
            workspace,
        },
        terminal::{SpawnOptions, TerminalBackend},
    };

    use super::{
        create, defaults, git_error, register_created_worktree, removal_info, remove,
        WorktreeRemovalConfirmation, WorktreeRemovalInfo,
    };

    #[test]
    fn worktree_git_error_keeps_stderr_when_stdout_is_empty() {
        let output = Output {
            status: ExitStatus::from_raw(1 << 8),
            stdout: Vec::new(),
            stderr: b"fatal: worktree operation failed".to_vec(),
        };

        let error = git_error("could not create worktree", &output);

        assert_eq!(
            error.message,
            "could not create worktree (exit code 1): fatal: worktree operation failed"
        );
    }

    /// Git writes used to prepare fixtures; no deadline is applied to mutations.
    fn git(cwd: &Path, args: &[&str]) -> String {
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
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
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

    fn fixture(base: &Path) -> (Database, PathBuf, String) {
        let root = base.join("repo with spaces");
        fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-b", "main"]);
        git(&root, &["config", "user.name", "Muster test"]);
        git(&root, &["config", "user.email", "muster@example.invalid"]);
        fs::write(root.join("base.txt"), "base\n").unwrap();
        git(&root, &["add", "base.txt"]);
        git(&root, &["commit", "-m", "base"]);
        let database = Database::open(base.join("workspace.sqlite3")).unwrap();
        let state = workspace::register_folder(&database, &root).unwrap();
        let checkout_id = state.repos[0].checkouts[0].id.clone();
        (database, root, checkout_id)
    }

    fn plain_fixture(base: &Path) -> (Database, PathBuf, String) {
        let root = base.join("plain checkout");
        fs::create_dir_all(&root).unwrap();
        let database = Database::open(base.join("workspace.sqlite3")).unwrap();
        let state = workspace::register_folder(&database, &root).unwrap();
        let checkout_id = state.repos[0].checkouts[0].id.clone();
        (database, root, checkout_id)
    }

    fn add_worktree(database: &Database, root: &Path, base: &Path, branch: &str) -> String {
        let path = base.join(format!("checkout-{branch}"));
        git(
            root,
            &["worktree", "add", "-b", branch, path.to_str().unwrap()],
        );
        let state = workspace::register_folder(database, &path).unwrap();
        state.repos[0]
            .checkouts
            .iter()
            .find(|checkout| {
                checkout.canonical_path == path.canonicalize().unwrap().display().to_string()
            })
            .unwrap()
            .id
            .clone()
    }

    fn repo_with_sibling(base: &Path) -> (Database, PathBuf, String, String, PathBuf) {
        let (database, root, primary_id) = fixture(base);
        let sibling_id = add_worktree(&database, &root, base, "agent-sibling");
        let sibling_directory = base.join("checkout-agent-sibling");
        (database, root, primary_id, sibling_id, sibling_directory)
    }

    fn branch_exists(root: &Path, branch: &str) -> bool {
        run_git_read(
            root,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
            GIT_READ_TIMEOUT,
        )
        .expect("Git ref read completes")
        .status
        .success()
    }

    fn confirmation(
        info: &WorktreeRemovalInfo,
        confirm_dirty: bool,
        confirmed_dirty_files: Vec<String>,
        confirmed_session_ids: Vec<String>,
        delete_branch: bool,
    ) -> WorktreeRemovalConfirmation {
        WorktreeRemovalConfirmation {
            confirm_dirty,
            confirmed_dirty_files,
            confirmed_session_ids,
            expected_branch: info.branch.clone(),
            expected_unmerged_commits: info.unmerged_commits,
            delete_branch,
        }
    }

    struct MockAgentApi {
        port: u16,
        prompt_started: mpsc::Receiver<()>,
        release_prompt: Option<mpsc::Sender<()>>,
        session_lists: Arc<AtomicUsize>,
        stopped: Arc<AtomicBool>,
        server: Option<thread::JoinHandle<()>>,
    }

    impl MockAgentApi {
        fn release_prompt(&mut self) {
            if let Some(release) = self.release_prompt.take() {
                let _ = release.send(());
            }
        }
    }

    impl Drop for MockAgentApi {
        fn drop(&mut self) {
            self.release_prompt();
            self.stopped.store(true, Ordering::SeqCst);
            if let Some(server) = self.server.take() {
                let _ = server.join();
            }
        }
    }

    fn mock_agent_api(directory: String) -> MockAgentApi {
        mock_agent_api_with_session_list_status(directory, 200)
    }

    fn mock_agent_api_with_session_list_status(
        directory: String,
        session_list_status: u16,
    ) -> MockAgentApi {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let server_stopped = Arc::clone(&stopped);
        let session_lists = Arc::new(AtomicUsize::new(0));
        let server_session_lists = Arc::clone(&session_lists);
        let (prompt_started_tx, prompt_started) = mpsc::channel();
        let (release_prompt, prompt_released) = mpsc::channel();
        let server = thread::spawn(move || {
            let session = serde_json::json!({
                "id": "ses_integrated",
                "title": "coding agent",
                "time": { "created": 1, "updated": 2, "idle": null },
                "location": { "directory": directory },
            });
            while !server_stopped.load(Ordering::SeqCst) {
                let (stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => break,
                };
                stream.set_nonblocking(false).unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                let mut parts = request.split_whitespace();
                let method = parts.next().unwrap_or_default();
                // The query is stripped: every request now carries `?directory=…`, and these
                // mocks match on the route.
                let path = parts
                    .next()
                    .unwrap_or_default()
                    .split('?')
                    .next()
                    .unwrap_or_default()
                    .to_string();
                let mut content_length = 0;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header == "\r\n" || header.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            content_length = value.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut body = vec![0; content_length];
                reader.read_exact(&mut body).unwrap();
                let data = match (method, path.as_str()) {
                    ("POST", "/api/session") => session.clone(),
                    ("GET", "/api/session") => {
                        server_session_lists.fetch_add(1, Ordering::SeqCst);
                        serde_json::json!([session.clone()])
                    }
                    // The service's own running answer. Empty here, so the session reads as idle
                    // unless a test makes it otherwise, which is what an idle mock should say.
                    ("GET", "/api/session/active") => serde_json::json!({}),
                    ("GET", "/api/form" | "/api/permission/request") => serde_json::json!([]),
                    ("GET", "/api/session/ses_integrated") => session.clone(),
                    ("POST", "/api/session/ses_integrated/prompt") => {
                        let _ = prompt_started_tx.send(());
                        let _ = prompt_released.recv();
                        serde_json::json!({})
                    }
                    _ => serde_json::json!({}),
                };
                let payload = serde_json::json!({ "data": data }).to_string();
                let status = if method == "GET" && path == "/api/session" {
                    session_list_status
                } else {
                    200
                };
                let response = format!(
                    "HTTP/1.1 {status} Test Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                let _ = reader.get_mut().write_all(response.as_bytes());
            }
        });
        MockAgentApi {
            port,
            prompt_started,
            release_prompt: Some(release_prompt),
            session_lists,
            stopped,
            server: Some(server),
        }
    }

    /// Runs a request to the mock agent server with SIGCHLD blocked on this thread.
    ///
    /// This suite forks thousands of children, and every one of them delivers a SIGCHLD that the
    /// kernel can land on whichever thread happens to be blocked in a socket read at that moment,
    /// which ends the syscall and comes back as `EINTR`. Nothing the server did causes that, and
    /// repeating the request is not free either: a repeated prompt reaches it twice. Blocking the
    /// signal for the length of one request keeps the interruption from happening at all, so the
    /// request is sent exactly once and these tests keep asserting on the real thing.
    ///
    /// The mask belongs to this thread alone and is restored on the way out, so a signal that
    /// arrives is left for another thread to take. The window holds nothing but the request, which
    /// is why the mask cannot leak into a fork that happens on this thread.
    fn asking_the_mock_agent<T>(request: impl FnOnce() -> T) -> T {
        let mut blocked: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut blocked);
            libc::sigaddset(&mut blocked, libc::SIGCHLD);
        }
        let mut previous: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut previous) };
        let answer = request();
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut()) };
        answer
    }

    /// Creates a session on the mock agent server, once.
    fn create_session_from_the_mock_agent(
        agents: &AgentService,
        checkout_id: &str,
        directory: &Path,
        title: &str,
    ) -> AgentSession {
        asking_the_mock_agent(|| agents.create_session(checkout_id, directory, title))
            .unwrap_or_else(|error| {
                panic!("the mock agent server could not be asked for a session: {error:?}")
            })
    }

    fn prompt_agent(
        agents: Arc<AgentService>,
        checkout_id: String,
        directory: PathBuf,
        session_id: String,
        server: &mut MockAgentApi,
    ) {
        let prompt = thread::spawn(move || {
            asking_the_mock_agent(|| {
                agents.prompt(&checkout_id, &directory, &session_id, "start work")
            })
        });
        server
            .prompt_started
            .recv_timeout(Duration::from_secs(2))
            .expect("the real AgentService prompt reached the mock OpenCode route");
        server.release_prompt();
        prompt
            .join()
            .unwrap()
            .expect("the prompt should be accepted");
    }

    #[test]
    fn integrated_agent_activity_guards_removal_and_serializes_prompt_races() {
        let temp = tempdir().unwrap();
        let (database, root, _) = fixture(temp.path());
        let checkout_id = add_worktree(&database, &root, temp.path(), "integrated-agent");
        let state = database.load_workspace().unwrap();
        let checkout = state.repos[0]
            .checkouts
            .iter()
            .find(|checkout| checkout.id == checkout_id)
            .unwrap();
        let directory = PathBuf::from(&checkout.canonical_path);
        let mut server = mock_agent_api(directory.display().to_string());
        let agents = Arc::new(AgentService::with_test_server(server.port));
        let created =
            create_session_from_the_mock_agent(&agents, &checkout_id, &directory, "coding agent");

        // A fresh conversation has no turn-start event and must not look busy just because
        // OpenCode also reports no idle timestamp for it.
        let idle_info = removal_info(&database, &agents, &checkout_id).unwrap();
        assert!(idle_info.active_agent_sessions.is_empty());
        let idle_confirmation = confirmation(&idle_info, false, vec![], vec![], false);

        let prompt_agents = Arc::clone(&agents);
        let prompt_checkout = checkout_id.clone();
        let prompt_directory = directory.clone();
        let prompt = thread::spawn(move || {
            asking_the_mock_agent(|| {
                prompt_agents.prompt(
                    &prompt_checkout,
                    &prompt_directory,
                    &created.id,
                    "start work",
                )
            })
        });
        server
            .prompt_started
            .recv_timeout(Duration::from_secs(2))
            .expect("the real AgentService prompt reached the mock OpenCode route");

        let remove_agents = Arc::clone(&agents);
        let remove_database = database.clone();
        let remove_checkout = checkout_id.clone();
        let (attempted_tx, attempted_rx) = mpsc::channel();
        let removal = thread::spawn(move || {
            let _ = attempted_tx.send(());
            remove(
                &remove_database,
                &TerminalBackend::default(),
                &remove_agents,
                &remove_checkout,
                &idle_confirmation,
            )
        });
        attempted_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            !removal.is_finished(),
            "removal raced past an in-flight prompt"
        );
        server.release_prompt();
        prompt
            .join()
            .unwrap()
            .expect("the prompt should be accepted");
        let blocked = removal.join().unwrap().unwrap_err();
        assert!(
            blocked.message.contains("active agent"),
            "{}",
            blocked.message
        );
        assert!(
            directory.is_dir(),
            "the active agent's checkout was removed"
        );

        let active = removal_info(&database, &agents, &checkout_id).unwrap();
        assert_eq!(active.active_agent_sessions[0].name, "coding agent");
        agents.stop(&checkout_id);
        let idle = removal_info(&database, &agents, &checkout_id).unwrap();
        assert!(idle.active_agent_sessions.is_empty());
        remove(
            &database,
            &TerminalBackend::default(),
            &agents,
            &checkout_id,
            &confirmation(&idle, false, vec![], vec![], false),
        )
        .unwrap();
        assert!(!directory.exists());
    }

    #[test]
    fn removal_stops_an_idle_integrated_agent_bridge() {
        let temp = tempdir().unwrap();
        let (database, root, _) = fixture(temp.path());
        let checkout_id = add_worktree(&database, &root, temp.path(), "idle-agent");
        let state = database.load_workspace().unwrap();
        let checkout = state.repos[0]
            .checkouts
            .iter()
            .find(|checkout| checkout.id == checkout_id)
            .unwrap();
        let directory = PathBuf::from(&checkout.canonical_path);
        let server = mock_agent_api(directory.display().to_string());
        let agents = Arc::new(AgentService::with_test_server(server.port));
        create_session_from_the_mock_agent(&agents, &checkout_id, &directory, "coding agent");

        let info = removal_info(&database, &agents, &checkout_id).unwrap();
        assert!(info.active_agent_sessions.is_empty());
        let confirmation = confirmation(&info, false, vec![], vec![], false);
        let lists_before_remove = server.session_lists.load(Ordering::SeqCst);

        remove(
            &database,
            &TerminalBackend::default(),
            &agents,
            &checkout_id,
            &confirmation,
        )
        .unwrap();

        let lists_after_remove = server.session_lists.load(Ordering::SeqCst);
        assert_eq!(lists_after_remove, lists_before_remove + 1);
        assert!(agents
            .active_worktree_agent_sessions(&checkout_id)
            .unwrap()
            .is_empty());
        assert_eq!(
            server.session_lists.load(Ordering::SeqCst),
            lists_after_remove,
            "removal should stop the idle bridge before deleting its checkout"
        );
        assert!(!directory.exists());
    }

    #[test]
    fn closing_missing_primary_waits_for_prompt_and_refuses_busy_agent() {
        let temp = tempdir().unwrap();
        let (database, root, primary_id, _, _) = repo_with_sibling(temp.path());
        let mut server = mock_agent_api(root.display().to_string());
        let agents = Arc::new(AgentService::with_test_server(server.port));
        let created =
            create_session_from_the_mock_agent(&agents, &primary_id, &root, "primary agent");
        fs::remove_dir_all(&root).unwrap();

        let prompt_agents = Arc::clone(&agents);
        let prompt_checkout = primary_id.clone();
        let prompt_directory = root.clone();
        let prompt_session = created.id;
        let prompt = thread::spawn(move || {
            asking_the_mock_agent(|| {
                prompt_agents.prompt(
                    &prompt_checkout,
                    &prompt_directory,
                    &prompt_session,
                    "start work",
                )
            })
        });
        server
            .prompt_started
            .recv_timeout(Duration::from_secs(2))
            .expect("the prompt should reach the mock OpenCode route");

        let close_agents = Arc::clone(&agents);
        let close_database = database.clone();
        let close_checkout = primary_id.clone();
        let (attempted_tx, attempted_rx) = mpsc::channel();
        let close = thread::spawn(move || {
            let _ = attempted_tx.send(());
            workspace::close_missing_checkout(
                &close_database,
                &TerminalBackend::default(),
                &close_agents,
                &close_checkout,
            )
        });
        attempted_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(!close.is_finished(), "close raced past an in-flight prompt");

        server.release_prompt();
        prompt
            .join()
            .unwrap()
            .expect("the prompt should be accepted");
        let error = close.join().unwrap().unwrap_err();
        assert!(error.message.contains("active agent"), "{}", error.message);
        assert_eq!(
            database.load_workspace().unwrap().repos[0].checkouts.len(),
            2
        );
    }

    #[test]
    fn closing_missing_primary_stops_idle_agent_before_cascading_repo_delete() {
        let temp = tempdir().unwrap();
        let (database, root, primary_id, _, _) = repo_with_sibling(temp.path());
        let server = mock_agent_api(root.display().to_string());
        let agents = Arc::new(AgentService::with_test_server(server.port));
        let stale_generation = agents.checkout_generation(&primary_id).unwrap();
        create_session_from_the_mock_agent(&agents, &primary_id, &root, "idle primary agent");
        let stale_directory = root.clone();
        fs::remove_dir_all(&root).unwrap();

        let closed = workspace::close_missing_checkout(
            &database,
            &TerminalBackend::default(),
            &agents,
            &primary_id,
        )
        .unwrap();

        assert!(closed.repos.is_empty());
        let list_count = server.session_lists.load(Ordering::SeqCst);
        assert_eq!(list_count, 1);
        assert!(agents
            .active_worktree_agent_sessions(&primary_id)
            .unwrap()
            .is_empty());
        assert_eq!(server.session_lists.load(Ordering::SeqCst), list_count);
        let error = agents
            .create_session_at_generation(
                &primary_id,
                &stale_directory,
                "stale path",
                stale_generation,
                || database.terminal_checkout_path(&primary_id).is_ok(),
            )
            .unwrap_err();
        assert!(format!("{error:?}").contains("changed"));
        assert_eq!(server.session_lists.load(Ordering::SeqCst), list_count);
    }

    #[test]
    fn workspace_read_failure_rolls_back_close_and_preserves_the_bridge() {
        let temp = tempdir().unwrap();
        let (database, _, _, checkout_id, directory) = repo_with_sibling(temp.path());
        let server = mock_agent_api(directory.display().to_string());
        let agents = Arc::new(AgentService::with_test_server(server.port));
        let generation = agents.checkout_generation(&checkout_id).unwrap();
        create_session_from_the_mock_agent(&agents, &checkout_id, &directory, "idle agent");
        let bridge = agents.bridge(&checkout_id, &directory).unwrap();

        Connection::open(temp.path().join("workspace.sqlite3"))
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fail_workspace_read_after_close
                 AFTER DELETE ON checkouts WHEN OLD.is_primary = 0
                 BEGIN
                     UPDATE checkouts SET ahead_of_default = -1
                     WHERE repo_id = OLD.repo_id AND is_primary = 1;
                 END;",
            )
            .unwrap();

        assert!(workspace::close_checkout(
            &database,
            &TerminalBackend::default(),
            &agents,
            &checkout_id,
        )
        .is_err());
        assert_eq!(
            agents.checkout_generation(&checkout_id).unwrap(),
            generation
        );
        let current_bridge = agents.bridge(&checkout_id, &directory).unwrap();
        assert!(Arc::ptr_eq(&bridge, &current_bridge));
        assert!(database.load_workspace().unwrap().repos[0]
            .checkouts
            .iter()
            .any(|checkout| checkout.id == checkout_id));
    }

    #[test]
    fn stale_agent_request_cannot_cross_close_and_reopen_but_new_generation_can() {
        let temp = tempdir().unwrap();
        let (database, root, primary_id) = fixture(temp.path());
        let mut server = mock_agent_api(root.display().to_string());
        let agents = Arc::new(AgentService::with_test_server(server.port));
        let stale_directory = root.clone();
        let stale_generation = agents.checkout_generation(&primary_id).unwrap();
        let session =
            create_session_from_the_mock_agent(&agents, &primary_id, &root, "pre-close session");
        fs::remove_dir_all(&root).unwrap();

        workspace::close_missing_checkout(
            &database,
            &TerminalBackend::default(),
            &agents,
            &primary_id,
        )
        .unwrap();
        assert_ne!(
            agents.checkout_generation(&primary_id).unwrap(),
            stale_generation
        );

        fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-b", "main"]);
        git(&root, &["config", "user.name", "Muster test"]);
        git(&root, &["config", "user.email", "muster@example.invalid"]);
        fs::write(root.join("base.txt"), "base\n").unwrap();
        git(&root, &["add", "base.txt"]);
        git(&root, &["commit", "-m", "base"]);
        let reopened = workspace::register_folder(&database, &root).unwrap();
        assert_eq!(reopened.repos[0].checkouts[0].id, primary_id);

        server.release_prompt();
        assert!(asking_the_mock_agent(|| agents.prompt_at_generation(
            &primary_id,
            &stale_directory,
            &session.id,
            "stale IPC prompt",
            stale_generation,
            || database.terminal_checkout_path(&primary_id).is_ok(),
        ))
        .is_err());
        assert!(server.prompt_started.try_recv().is_err());

        let current_generation = agents.checkout_generation(&primary_id).unwrap();
        asking_the_mock_agent(|| {
            agents
                .prompt_at_generation(
                    &primary_id,
                    &root,
                    &session.id,
                    "new IPC prompt",
                    current_generation,
                    || database.terminal_checkout_path(&primary_id).is_ok(),
                )
                .expect("a new request for the registered checkout is accepted");
        });
        server
            .prompt_started
            .recv_timeout(Duration::from_secs(1))
            .expect("the current generation reaches the server");
    }

    #[test]
    fn failed_worktree_retarget_keeps_the_registered_bridge_alive() {
        let temp = tempdir().unwrap();
        let (database, root, _, checkout_id, old_directory) = repo_with_sibling(temp.path());
        let server = mock_agent_api(old_directory.display().to_string());
        let agents = Arc::new(AgentService::with_test_server(server.port));
        let generation = agents.checkout_generation(&checkout_id).unwrap();
        create_session_from_the_mock_agent(&agents, &checkout_id, &old_directory, "idle agent");
        let bridge = agents.bridge(&checkout_id, &old_directory).unwrap();
        let moved_directory = temp.path().join("unrepaired worktree");
        fs::rename(&old_directory, &moved_directory).unwrap();
        drop(database);
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();

        let error = workspace::locate_missing_checkout(&database, &agents, &checkout_id, &root)
            .unwrap_err();

        assert!(error.message.contains("identity"), "{}", error.message);
        assert_eq!(
            agents.checkout_generation(&checkout_id).unwrap(),
            generation
        );
        let current_bridge = agents.bridge(&checkout_id, &old_directory).unwrap();
        assert!(Arc::ptr_eq(&bridge, &current_bridge));
        assert!(database.load_workspace().unwrap().repos[0]
            .checkouts
            .iter()
            .any(|checkout| checkout.id == checkout_id));
    }

    #[test]
    fn concurrent_registration_during_git_locate_keeps_the_old_bridge_alive() {
        let temp = tempdir().unwrap();
        let (database, root, _, checkout_id, directory) = repo_with_sibling(temp.path());
        let repo_id = database.load_workspace().unwrap().repos[0].id.clone();
        let snapshot = database
            .git_repo_registration_snapshot(&repo_id)
            .unwrap()
            .expect("the Git repository registration snapshot");
        let (resolved, focus_id) =
            crate::git::resolve_repository(&root, &crate::persistence::timestamp())
                .unwrap()
                .expect("the repository resolves before a late worktree is registered");
        let server = mock_agent_api(directory.display().to_string());
        let agents = Arc::new(AgentService::with_test_server(server.port));
        let generation = agents.checkout_generation(&checkout_id).unwrap();
        create_session_from_the_mock_agent(&agents, &checkout_id, &directory, "idle agent");
        let bridge = agents.bridge(&checkout_id, &directory).unwrap();
        let removal = agents.reserve_worktree_removal(&checkout_id).unwrap();
        add_worktree(&database, &root, temp.path(), "late-locate");

        let error = workspace::finish_git_checkout_location(
            &database,
            &agents,
            &checkout_id,
            &resolved,
            &focus_id,
            &snapshot,
            removal,
        )
        .unwrap_err();

        assert!(error.message.contains("registration changed"));
        assert_eq!(
            agents.checkout_generation(&checkout_id).unwrap(),
            generation
        );
        let current_bridge = agents.bridge(&checkout_id, &directory).unwrap();
        assert!(Arc::ptr_eq(&bridge, &current_bridge));
    }

    #[test]
    fn locating_a_moved_worktree_invalidates_old_agent_path_and_keeps_checkout_history() {
        let temp = tempdir().unwrap();
        let (database, _root, _, checkout_id, old_directory) = repo_with_sibling(temp.path());
        database
            .mark_file_viewed(&checkout_id, "README.md")
            .unwrap();
        drop(database);
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        let moved_directory = temp.path().join("moved worktree");
        let _server = mock_agent_api(old_directory.display().to_string());
        let agents = Arc::new(AgentService::with_test_server(_server.port));
        let stale_generation = agents.checkout_generation(&checkout_id).unwrap();
        let session = create_session_from_the_mock_agent(
            &agents,
            &checkout_id,
            &old_directory,
            "old checkout agent",
        );
        let old_bridge = agents.bridge(&checkout_id, &old_directory).unwrap();
        fs::rename(&old_directory, &moved_directory).unwrap();

        let located =
            workspace::locate_missing_checkout(&database, &agents, &checkout_id, &moved_directory)
                .unwrap();
        let new_id = crate::domain::workspace::checkout_id_for_path(
            &moved_directory
                .canonicalize()
                .unwrap()
                .display()
                .to_string(),
        );
        let moved = located.repos[0]
            .checkouts
            .iter()
            .find(|checkout| checkout.id == new_id)
            .expect("the moved checkout is registered under its new path");
        assert_eq!(database.viewed_files(&new_id).unwrap(), ["README.md"]);
        assert!(located.repos[0]
            .checkouts
            .iter()
            .all(|checkout| checkout.id != checkout_id));

        let stale = asking_the_mock_agent(|| {
            agents
                .prompt_at_generation(
                    &checkout_id,
                    &old_directory,
                    &session.id,
                    "stale path prompt",
                    stale_generation,
                    || database.terminal_checkout_path(&checkout_id).is_ok(),
                )
                .unwrap_err()
        });
        assert!(format!("{stale:?}").contains("changed"));
        assert_eq!(
            moved.canonical_path,
            moved_directory
                .canonicalize()
                .unwrap()
                .display()
                .to_string()
        );

        fs::create_dir(&old_directory).unwrap();
        let reopened = workspace::register_folder(&database, &old_directory).unwrap();
        assert!(reopened.repos.iter().any(|repo| repo
            .checkouts
            .iter()
            .any(|checkout| checkout.id == checkout_id)));
        let current_generation = agents.checkout_generation(&checkout_id).unwrap();
        agents
            .create_session_at_generation(
                &checkout_id,
                &old_directory,
                "reopened checkout agent",
                current_generation,
                || database.terminal_checkout_path(&checkout_id).is_ok(),
            )
            .unwrap();
        let new_bridge = agents.bridge(&checkout_id, &old_directory).unwrap();
        assert!(!Arc::ptr_eq(&old_bridge, &new_bridge));
        assert_eq!(new_bridge.directory(), old_directory);
    }

    #[test]
    fn closing_missing_primary_checks_archived_checkout_agents_before_cascading() {
        let temp = tempdir().unwrap();
        let (database, root, primary_id, sibling_id, sibling_directory) =
            repo_with_sibling(temp.path());
        let mut server = mock_agent_api(sibling_directory.display().to_string());
        let agents = Arc::new(AgentService::with_test_server(server.port));
        let created = create_session_from_the_mock_agent(
            &agents,
            &sibling_id,
            &sibling_directory,
            "sibling agent",
        );
        prompt_agent(
            Arc::clone(&agents),
            sibling_id.clone(),
            sibling_directory,
            created.id,
            &mut server,
        );
        database.archive_checkout(&sibling_id).unwrap();
        fs::remove_dir_all(&root).unwrap();

        let error = workspace::close_missing_checkout(
            &database,
            &TerminalBackend::default(),
            &agents,
            &primary_id,
        )
        .unwrap_err();

        assert!(error.message.contains("active agent"), "{}", error.message);
        let retained = database.load_workspace().unwrap();
        assert_eq!(retained.repos[0].checkouts.len(), 1);
        assert!(retained
            .archived_worktrees
            .iter()
            .any(|checkout| checkout.id == sibling_id));
    }

    #[test]
    fn closing_missing_primary_fails_closed_when_agent_activity_cannot_be_queried() {
        let temp = tempdir().unwrap();
        let (database, root, primary_id, _, _) = repo_with_sibling(temp.path());
        let server = mock_agent_api_with_session_list_status(root.display().to_string(), 503);
        let agents = Arc::new(AgentService::with_test_server(server.port));
        create_session_from_the_mock_agent(
            &agents,
            &primary_id,
            &root,
            "agent with unavailable status",
        );
        fs::remove_dir_all(&root).unwrap();

        let error = workspace::close_missing_checkout(
            &database,
            &TerminalBackend::default(),
            &agents,
            &primary_id,
        )
        .unwrap_err();

        assert!(error.message.contains("503"), "{}", error.message);
        assert_eq!(database.load_workspace().unwrap().repos.len(), 1);
        assert!(agents.active_worktree_agent_sessions(&primary_id).is_err());
        assert_eq!(server.session_lists.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn closing_missing_plain_checkout_refuses_busy_agent_activity() {
        let temp = tempdir().unwrap();
        let (database, root, checkout_id) = plain_fixture(temp.path());
        let mut server = mock_agent_api(root.display().to_string());
        let agents = Arc::new(AgentService::with_test_server(server.port));
        let session =
            create_session_from_the_mock_agent(&agents, &checkout_id, &root, "plain agent");
        prompt_agent(
            Arc::clone(&agents),
            checkout_id.clone(),
            root.clone(),
            session.id,
            &mut server,
        );
        fs::remove_dir_all(&root).unwrap();

        let error = workspace::close_missing_checkout(
            &database,
            &TerminalBackend::default(),
            &agents,
            &checkout_id,
        )
        .unwrap_err();

        assert!(error.message.contains("active agent"), "{}", error.message);
        assert_eq!(database.load_workspace().unwrap().repos.len(), 1);
    }

    #[test]
    fn closing_missing_plain_checkout_fails_closed_when_activity_query_fails() {
        let temp = tempdir().unwrap();
        let (database, root, checkout_id) = plain_fixture(temp.path());
        let server = mock_agent_api_with_session_list_status(root.display().to_string(), 503);
        let agents = Arc::new(AgentService::with_test_server(server.port));
        create_session_from_the_mock_agent(&agents, &checkout_id, &root, "plain agent");
        fs::remove_dir_all(&root).unwrap();

        let error = workspace::close_missing_checkout(
            &database,
            &TerminalBackend::default(),
            &agents,
            &checkout_id,
        )
        .unwrap_err();

        assert!(error.message.contains("503"), "{}", error.message);
        assert_eq!(database.load_workspace().unwrap().repos.len(), 1);
    }

    #[test]
    fn closing_and_reopening_missing_plain_checkout_invalidates_stale_agent_generation() {
        let temp = tempdir().unwrap();
        let (database, root, checkout_id) = plain_fixture(temp.path());
        let server = mock_agent_api(root.display().to_string());
        let agents = Arc::new(AgentService::with_test_server(server.port));
        let stale_generation = agents.checkout_generation(&checkout_id).unwrap();
        let session =
            create_session_from_the_mock_agent(&agents, &checkout_id, &root, "old plain agent");
        let old_bridge = agents.bridge(&checkout_id, &root).unwrap();
        fs::remove_dir_all(&root).unwrap();

        workspace::close_missing_checkout(
            &database,
            &TerminalBackend::default(),
            &agents,
            &checkout_id,
        )
        .unwrap();
        fs::create_dir_all(&root).unwrap();
        let reopened = workspace::register_folder(&database, &root).unwrap();
        assert_eq!(reopened.repos[0].checkouts[0].id, checkout_id);

        let stale = asking_the_mock_agent(|| {
            agents
                .prompt_at_generation(
                    &checkout_id,
                    &root,
                    &session.id,
                    "stale plain prompt",
                    stale_generation,
                    || database.terminal_checkout_path(&checkout_id).is_ok(),
                )
                .unwrap_err()
        });
        assert!(format!("{stale:?}").contains("changed"));

        let current_generation = agents.checkout_generation(&checkout_id).unwrap();
        agents
            .create_session_at_generation(
                &checkout_id,
                &root,
                "new plain agent",
                current_generation,
                || database.terminal_checkout_path(&checkout_id).is_ok(),
            )
            .expect("the reopened plain checkout starts a fresh bridge");
        let new_bridge = agents.bridge(&checkout_id, &root).unwrap();
        assert!(!Arc::ptr_eq(&old_bridge, &new_bridge));
        assert_eq!(new_bridge.directory(), root);
    }

    #[test]
    fn sec_07_creates_from_any_checkout_and_sanitizes_worktree_task_names() {
        let temp = tempdir().unwrap();
        let (database, root, primary_id) = fixture(temp.path());
        let source_id = add_worktree(&database, &root, temp.path(), "current-feature");
        assert_ne!(source_id, primary_id);
        fs::write(root.join("main-only.txt"), "main head\n").unwrap();
        git(&root, &["add", "main-only.txt"]);
        git(&root, &["commit", "-m", "main-only"]);
        let location = root.canonicalize().unwrap().join(".worktrees");
        let excludes_path = root.join(".git/info/exclude");
        fs::write(&excludes_path, b"# existing user rule\n*.local\n").unwrap();
        let excludes_before = fs::read(&excludes_path).unwrap();

        let settings = defaults(&database, &source_id).unwrap();
        assert_eq!(settings.default_branch, "main");
        assert_eq!(settings.location, location.display().to_string());
        let created = create(
            &database,
            &source_id,
            "../../$(touch pwned); *",
            "feature/safe-name",
            &location,
        )
        .unwrap();

        let checkout = created.workspace.repos[0]
            .checkouts
            .iter()
            .find(|checkout| checkout.id == created.checkout_id)
            .unwrap();
        assert_eq!(checkout.branch.as_deref(), Some("feature/safe-name"));
        assert_eq!(
            Path::new(&checkout.canonical_path).parent(),
            Some(location.canonicalize().unwrap().as_path())
        );
        assert_eq!(
            git(
                Path::new(&checkout.canonical_path),
                &["log", "-1", "--format=%s"]
            ),
            "main-only"
        );
        assert_eq!(
            created.workspace.active_checkout_id.as_deref(),
            Some(created.checkout_id.as_str())
        );
        assert!(!temp.path().join("pwned").exists());
        let excludes_after = fs::read(excludes_path).unwrap();
        assert!(excludes_after.starts_with(&excludes_before));
        assert!(String::from_utf8_lossy(&excludes_after).contains("/.worktrees/"));
        assert_eq!(
            git_read(&root, &["status", "--porcelain", "--untracked-files=all"]),
            ""
        );
        assert_eq!(
            defaults(&database, &primary_id).unwrap().location,
            location.display().to_string()
        );
    }

    #[test]
    fn stale_creation_snapshot_keeps_a_concurrently_registered_sibling_and_created_work() {
        let temp = tempdir().unwrap();
        let (database, root, _) = fixture(temp.path());
        let repo_id = database.load_workspace().unwrap().repos[0].id.clone();
        let snapshot = database
            .git_repo_registration_snapshot(&repo_id)
            .unwrap()
            .unwrap();
        let location = root.canonicalize().unwrap().join(".worktrees");
        fs::create_dir_all(&location).unwrap();
        let created_path = location.join("created-before-sibling");
        git(
            &root,
            &[
                "worktree",
                "add",
                "-b",
                "feature/created-before-sibling",
                created_path.to_str().unwrap(),
                "main",
            ],
        );
        let (resolved, focus_checkout_id) =
            crate::git::resolve_repository(&created_path, &crate::persistence::timestamp())
                .unwrap()
                .unwrap();
        fs::write(created_path.join("user-work.txt"), "keep this work\n").unwrap();

        let sibling_id = add_worktree(&database, &root, temp.path(), "registered-after-resolve");
        let error = register_created_worktree(&database, &resolved, &focus_checkout_id, &snapshot)
            .unwrap_err();

        assert!(error.message.contains("registrations changed"));
        let current = database.load_workspace().unwrap();
        let sibling = current.repos[0]
            .checkouts
            .iter()
            .find(|checkout| checkout.id == sibling_id)
            .unwrap();
        assert!(!sibling.is_missing);
        assert_eq!(
            fs::read_to_string(created_path.join("user-work.txt")).unwrap(),
            "keep this work\n"
        );
        assert!(branch_exists(&root, "feature/created-before-sibling"));
    }

    #[test]
    fn create_refuses_to_reuse_a_pruned_archived_worktree_identity() {
        let temp = tempdir().unwrap();
        let (database, root, primary_id) = fixture(temp.path());
        let location = root.canonicalize().unwrap().join(".worktrees");
        let archived = create(
            &database,
            &primary_id,
            "archived-task",
            "feature/archived-task",
            &location,
        )
        .unwrap();
        let archived_path = location.join("archived-task");
        database.archive_checkout(&archived.checkout_id).unwrap();
        fs::remove_dir_all(&archived_path).unwrap();
        git(&root, &["worktree", "prune", "--expire", "now"]);
        let (resolved, _) = crate::git::resolve_repository(&root, &crate::persistence::timestamp())
            .unwrap()
            .unwrap();
        database.reconcile_git_repo(&resolved).unwrap();

        let error = create(
            &database,
            &primary_id,
            "archived-task",
            "feature/recreated-task",
            &location,
        )
        .unwrap_err();

        assert!(error.message.contains("archived worktree"));
        assert!(error.message.contains("restore or close"));
        assert!(!archived_path.exists());
        assert!(!branch_exists(&root, "feature/recreated-task"));
        assert!(branch_exists(&root, "feature/archived-task"));
        let current = database.load_workspace().unwrap();
        assert_eq!(current.archived_worktrees.len(), 1);
        assert_eq!(current.archived_worktrees[0].id, archived.checkout_id);

        git(
            &root,
            &[
                "worktree",
                "add",
                archived_path.to_str().unwrap(),
                "feature/archived-task",
            ],
        );
        let open_error = workspace::register_folder(&database, &archived_path).unwrap_err();
        assert!(open_error.message.contains("worktree is archived"));
        let current = database.load_workspace().unwrap();
        assert_eq!(current.repos[0].checkouts.len(), 1);
        assert_eq!(current.archived_worktrees.len(), 1);
    }

    #[test]
    fn creation_keeps_git_worktree_when_workspace_load_fails_after_commit() {
        let temp = tempdir().unwrap();
        let (database, root, checkout_id) = fixture(temp.path());
        let location = root.canonicalize().unwrap().join(".worktrees");
        let destination = location.join("failed-workspace-read");
        let database_path = temp.path().join("workspace.sqlite3");
        let escaped_destination = destination.to_string_lossy().replace('\'', "''");
        Connection::open(&database_path)
            .unwrap()
            .execute_batch(&format!(
                "CREATE TRIGGER fail_created_workspace_read
                 AFTER INSERT ON checkouts
                 WHEN NEW.canonical_path = '{escaped_destination}'
                 BEGIN
                   UPDATE checkouts SET is_missing = 'invalid' WHERE id = NEW.id;
                 END;"
            ))
            .unwrap();

        let error = create(
            &database,
            &checkout_id,
            "failed-workspace-read",
            "feature/failed-workspace-read",
            &location,
        )
        .unwrap_err();

        assert!(error.message.contains("left untouched"));
        assert!(destination.is_dir());
        assert!(branch_exists(&root, "feature/failed-workspace-read"));
        let registered_id: String = Connection::open(database_path)
            .unwrap()
            .query_row(
                "SELECT id FROM checkouts WHERE canonical_path = ?1",
                [destination.display().to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            registered_id,
            crate::domain::workspace::checkout_id_for_path(&destination.display().to_string())
        );
    }

    #[test]
    fn worktree_creation_requires_main_without_falling_back_to_the_current_branch() {
        let temp = tempdir().unwrap();
        let (database, root, checkout_id) = fixture(temp.path());
        git(&root, &["branch", "-m", "trunk"]);

        let error = defaults(&database, &checkout_id).unwrap_err();

        assert!(error.message.contains("Git branch 'main' does not exist"));
        assert!(create(
            &database,
            &checkout_id,
            "work",
            "feature/work",
            &root.join(".worktrees"),
        )
        .unwrap_err()
        .message
        .contains("Git branch 'main' does not exist"));
    }

    #[test]
    fn worktree_creation_is_fixed_to_the_repository_worktrees_directory() {
        let temp = tempdir().unwrap();
        let (database, root, checkout_id) = fixture(temp.path());
        let external = temp.path().join("external-worktrees");

        let error = create(&database, &checkout_id, "work", "feature/work", &external).unwrap_err();

        assert!(error.message.contains("repository's .worktrees directory"));
        assert!(!external.exists());
        assert!(!root.join(".worktrees").exists());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_append_to_an_exclude_symlink_outside_git_metadata() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let (database, root, checkout_id) = fixture(temp.path());
        let exclude = root.join(".git/info/exclude");
        let outside = temp.path().join("user-exclude");
        fs::write(&outside, b"user data\n").unwrap();
        fs::remove_file(&exclude).unwrap();
        symlink(&outside, &exclude).unwrap();

        let error = create(
            &database,
            &checkout_id,
            "work",
            "feature/work",
            &root.join(".worktrees"),
        )
        .unwrap_err();

        assert!(error.message.contains("symbolic link"));
        assert_eq!(fs::read(outside).unwrap(), b"user data\n");
    }

    #[test]
    fn sec_07_rejects_branch_metacharacters_and_names_the_checkout_holding_an_existing_branch() {
        let temp = tempdir().unwrap();
        let (database, root, primary_id) = fixture(temp.path());
        let holder_id = add_worktree(&database, &root, temp.path(), "occupied");
        let location = temp.path().join("outside");

        let in_use = create(&database, &primary_id, "other", "occupied", &location).unwrap_err();
        assert!(in_use.message.contains("already checked out"));
        assert!(in_use.message.contains(&holder_id.replace("checkout:", "")));

        let invalid = create(
            &database,
            &primary_id,
            "other",
            "$(touch pwned);bad",
            &location,
        )
        .unwrap_err();
        assert!(invalid.message.contains("valid Git branch"));
        assert!(!temp.path().join("pwned").exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_configured_location_symlink_that_traverses_into_the_repository() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let (database, root, checkout_id) = fixture(temp.path());
        let alias = temp.path().join("external-alias");
        symlink(&root, &alias).unwrap();

        let error = create(&database, &checkout_id, "work", "feature/symlink", &alias).unwrap_err();

        assert!(error.message.contains("inside the repository"));
        assert!(!root.join("repo with spaces/work").exists());
    }

    #[test]
    fn dirty_removal_requires_confirmation_and_keeps_the_branch_by_default() {
        let temp = tempdir().unwrap();
        let (database, root, _) = fixture(temp.path());
        let checkout_id = add_worktree(&database, &root, temp.path(), "dirty-feature");
        let state = database.load_workspace().unwrap();
        let checkout = state.repos[0]
            .checkouts
            .iter()
            .find(|checkout| checkout.id == checkout_id)
            .unwrap();
        fs::write(
            Path::new(&checkout.canonical_path).join("untracked.txt"),
            "dirty",
        )
        .unwrap();
        let info = removal_info(&database, &AgentService::without_service(), &checkout_id).unwrap();
        assert_eq!(info.dirty_files, ["untracked.txt"]);

        let backend = TerminalBackend::default();
        let unconfirmed = confirmation(&info, false, info.dirty_files.clone(), vec![], false);
        assert!(remove(
            &database,
            &backend,
            &AgentService::without_service(),
            &checkout_id,
            &unconfirmed
        )
        .is_err());
        fs::write(
            Path::new(&checkout.canonical_path).join("another-change.txt"),
            "changed after confirmation",
        )
        .unwrap();
        let stale_confirmation = confirmation(&info, true, info.dirty_files.clone(), vec![], false);
        let stale = remove(
            &database,
            &backend,
            &AgentService::without_service(),
            &checkout_id,
            &stale_confirmation,
        )
        .unwrap_err();
        assert!(stale.message.contains("changes changed"));
        let info = removal_info(&database, &AgentService::without_service(), &checkout_id).unwrap();
        let confirmed = confirmation(&info, true, info.dirty_files.clone(), vec![], false);
        let result = remove(
            &database,
            &backend,
            &AgentService::without_service(),
            &checkout_id,
            &confirmed,
        )
        .unwrap();

        assert!(branch_exists(&root, "dirty-feature"));
        assert!(!result.workspace.repos[0]
            .checkouts
            .iter()
            .any(|item| item.id == checkout_id));
        assert!(!Path::new(&checkout.canonical_path).exists());
    }

    #[test]
    fn reports_unmerged_commit_count_and_only_deletes_branch_when_chosen() {
        let temp = tempdir().unwrap();
        let (database, root, _) = fixture(temp.path());
        let checkout_id = add_worktree(&database, &root, temp.path(), "ahead-feature");
        let state = database.load_workspace().unwrap();
        let checkout = state.repos[0]
            .checkouts
            .iter()
            .find(|checkout| checkout.id == checkout_id)
            .unwrap();
        let path = Path::new(&checkout.canonical_path);
        fs::write(path.join("ahead.txt"), "ahead\n").unwrap();
        git(path, &["add", "ahead.txt"]);
        git(path, &["commit", "-m", "ahead"]);
        let info = removal_info(&database, &AgentService::without_service(), &checkout_id).unwrap();
        assert_eq!(info.unmerged_commits, 1);

        let confirmed = confirmation(&info, false, vec![], vec![], true);
        remove(
            &database,
            &TerminalBackend::default(),
            &AgentService::without_service(),
            &checkout_id,
            &confirmed,
        )
        .unwrap();

        assert!(!branch_exists(&root, "ahead-feature"));
    }

    #[test]
    fn primary_is_denied() {
        let temp = tempdir().unwrap();
        let (database, _, primary_id) = fixture(temp.path());
        let backend = TerminalBackend::default();
        let info = removal_info(&database, &AgentService::without_service(), &primary_id).unwrap();
        let confirmation = confirmation(&info, true, vec![], vec![], false);

        assert!(remove(
            &database,
            &backend,
            &AgentService::without_service(),
            &primary_id,
            &confirmation
        )
        .unwrap_err()
        .message
        .contains("primary"));
    }

    #[test]
    fn live_and_disconnected_pi_sessions_block_worktree_removal() {
        let temp = tempdir().unwrap();
        let (database, root, _) = fixture(temp.path());
        let checkout_id = add_worktree(&database, &root, temp.path(), "pi-active");
        let agents = AgentService::without_service();
        let credential = agents
            .pi()
            .issue_credential("terminal:pi", &checkout_id)
            .unwrap();
        let token = credential.rsplit(':').next().unwrap();
        let mut stream = TcpStream::connect(agents.pi().address().unwrap()).unwrap();
        let hello = serde_json::json!({
            "v": crate::services::pi::PROTOCOL_VERSION,
            "type": "hello",
            "token": token,
            "terminalId": "terminal:pi",
            "checkoutId": checkout_id,
            "sessionId": "pi-session",
            "title": "Pi session",
            "work": {
                "generation": "test-generation",
                "sequence": 0,
                "targets": [],
                "ambiguous": false,
            },
        });
        stream.write_all(format!("{hello}\n").as_bytes()).unwrap();

        for _ in 0..100 {
            if !agents
                .pi()
                .active_worktree_sessions(&checkout_id)
                .is_empty()
            {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let active = removal_info(&database, &agents, &checkout_id).unwrap();
        assert_eq!(active.active_agent_sessions.len(), 1);
        assert!(active.active_agent_sessions[0].name.contains("Pi session"));

        drop(stream);
        for _ in 0..100 {
            if agents.pi().active_worktree_sessions(&checkout_id)[0]
                .title
                .contains("status unknown")
            {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let unknown = removal_info(&database, &agents, &checkout_id).unwrap();
        assert!(unknown.active_agent_sessions[0]
            .name
            .contains("status unknown"));
        let confirmed = confirmation(&unknown, false, vec![], vec![], false);
        let error = remove(
            &database,
            &TerminalBackend::default(),
            &agents,
            &checkout_id,
            &confirmed,
        )
        .unwrap_err();
        assert!(error.message.contains("stop the active agent session"));
    }

    #[test]
    fn active_terminal_sessions_are_listed_require_exact_confirmation_and_are_stopped() {
        let temp = tempdir().unwrap();
        let (database, root, _) = fixture(temp.path());
        let checkout_id = add_worktree(&database, &root, temp.path(), "session-feature");
        let state = database.load_workspace().unwrap();
        let checkout = state.repos[0]
            .checkouts
            .iter()
            .find(|checkout| checkout.id == checkout_id)
            .unwrap();
        let session_id = "session:active-worktree";
        let backend = TerminalBackend::default();
        backend
            .spawn(
                session_id.into(),
                SpawnOptions {
                    program: PathBuf::from("/bin/sh"),
                    args: vec!["-c".into(), "exec sleep 30".into()],
                    cwd: PathBuf::from(&checkout.canonical_path),
                    cols: 80,
                    rows: 24,
                    env: vec![],
                },
                Box::new(|_| Ok(())),
            )
            .unwrap();
        database
            .add_active_session(&Session {
                id: session_id.into(),
                session_type: SessionType::Shell,
                checkout_id: checkout_id.clone(),
                name: "shell · session-feature".into(),
                created_at: "now".into(),
                status: SessionStatus::Active,
            })
            .unwrap();

        let info = removal_info(&database, &AgentService::without_service(), &checkout_id).unwrap();
        assert_eq!(info.active_sessions[0].id, session_id);
        let no_session_confirmation = confirmation(&info, false, vec![], vec![], false);
        assert!(remove(
            &database,
            &backend,
            &AgentService::without_service(),
            &checkout_id,
            &no_session_confirmation
        )
        .is_err());
        let confirmed = confirmation(&info, false, vec![], vec![session_id.into()], false);
        let result = remove(
            &database,
            &backend,
            &AgentService::without_service(),
            &checkout_id,
            &confirmed,
        )
        .unwrap();

        assert!(!result.workspace.repos[0]
            .checkouts
            .iter()
            .any(|checkout| checkout.id == checkout_id));
        assert!(backend.status(session_id).is_err());
    }

    #[test]
    fn missing_worktree_removal_prunes_git_metadata_and_clears_registration() {
        let temp = tempdir().unwrap();
        let (database, root, _) = fixture(temp.path());
        let checkout_id = add_worktree(&database, &root, temp.path(), "missing-feature");
        let state = database.load_workspace().unwrap();
        let path = state.repos[0]
            .checkouts
            .iter()
            .find(|checkout| checkout.id == checkout_id)
            .unwrap()
            .canonical_path
            .clone();
        fs::remove_dir_all(&path).unwrap();
        git(&root, &["worktree", "prune", "--expire", "now"]);
        git(&root, &["branch", "-D", "--", "missing-feature"]);

        let info = removal_info(&database, &AgentService::without_service(), &checkout_id).unwrap();
        assert!(info.is_missing);
        assert_eq!(info.branch, None);
        let confirmed = confirmation(&info, false, vec![], vec![], false);
        let result = remove(
            &database,
            &TerminalBackend::default(),
            &AgentService::without_service(),
            &checkout_id,
            &confirmed,
        )
        .unwrap();

        assert!(!result.workspace.repos[0]
            .checkouts
            .iter()
            .any(|checkout| checkout.id == checkout_id));
        let listed = git_read(&root, &["worktree", "list", "--porcelain"]);
        assert!(!listed.contains("missing-feature"));
    }
}
