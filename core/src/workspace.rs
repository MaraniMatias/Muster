//! The shapes the workspace travels in, as the bridge hands them to the window.
//!
//! Nothing here is read back. Every type in this file is built in Rust from a database row or from
//! what Git said, and only the `Serialize` half runs: no command takes one of these as an argument
//! and no column holds one as JSON. That is why the `Option`s below carry
//! `skip_serializing_if` and no `default` — `None` leaves the wire entirely rather than coming back
//! as a field nobody wrote, and a shape this build never produces cannot arrive here to be read.
//! The blob rows that *are* read back (`AppLayoutState`, `CheckoutUiState` in
//! `persistence`, `CheckoutTerminalLayout` in `terminal_layout`) are the ones with no `default`
//! and a refusal in their load path.

use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RepoKind {
    Git,
    Plain,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SessionType {
    Shell,
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Repo {
    pub id: String,
    pub kind: RepoKind,
    pub name: String,
    pub root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
    pub checkouts: Vec<Checkout>,
    pub created_at: String,
    pub last_opened_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Checkout {
    pub id: String,
    pub repo_id: String,
    pub path: String,
    pub canonical_path: String,
    pub is_primary: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead_of_default: Option<u32>,
    pub changed_files: u32,
    pub is_missing: bool,
    pub sessions: Vec<Session>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    #[serde(rename = "type")]
    pub session_type: SessionType,
    pub checkout_id: String,
    pub name: String,
    pub created_at: String,
    pub status: SessionStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Active,
    Inactive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSessionStatus {
    pub state: TerminalProcessState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<u32>,
    pub foreground_process: bool,
    /// The program in front of the shell, by name: `opencode`, `nvim`.
    ///
    /// Absent when the shell itself is in front, which is the resting state and needs no
    /// naming, and when the foreground group is gone or belongs to another user.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreground_app: Option<String>,
    /// Where the shell itself is, as the OS records it.
    ///
    /// The shell's own directory and not the foreground group's: a shell's directory changes when
    /// someone types `cd`, while a program's changes for all sorts of reasons of its own, and only
    /// the first says where this terminal is working. Absent once the session has exited, and when
    /// the OS will not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TerminalProcessState {
    Running,
    Exited,
}

/// A worktree that is off the panel and can be put back on it.
///
/// It is a checkout like any other, so it names itself the way a row on the panel does,
/// and it keeps the repository it belongs to: the repo root is the one row that can
/// restore it, so the panel has to know which root to hang the action under.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ArchivedCheckout {
    pub id: String,
    pub repo_id: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceState {
    pub repos: Vec<Repo>,
    pub home_checkout_id: Option<String>,
    /// The worktrees that are registered but off the panel, so the repo root can offer
    /// them back. Empty on every workspace with nothing archived, which is why it is not
    /// always on the wire.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub archived_worktrees: Vec<ArchivedCheckout>,
    pub active_checkout_id: Option<String>,
    pub active_session_id: Option<String>,
}

/// A folder that has been opened before, as the workdir menu lists it. The rows are raised on
/// every folder that is registered, so reading them back is the whole of "recently opened".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecentPath {
    pub canonical_path: String,
    pub last_opened_at: String,
}

impl Repo {
    pub fn plain(path: &Path, now: impl Into<String>) -> Result<Self, String> {
        let canonical_path = canonical_directory(path)?;
        let path = canonical_path.display().to_string();
        let name = canonical_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| path.clone());
        let id = repo_id_for_path(&path);
        let checkout = Checkout::new(id.clone(), &canonical_path, true)?;
        let now = now.into();

        Ok(Self {
            id,
            kind: RepoKind::Plain,
            name,
            root: path,
            default_branch: None,
            checkouts: vec![checkout],
            created_at: now.clone(),
            last_opened_at: now,
        })
    }
}

impl Checkout {
    pub fn new(repo_id: String, path: &Path, is_primary: bool) -> Result<Self, String> {
        let canonical_path = canonical_directory(path)?;
        let path = canonical_path.display().to_string();

        Ok(Self {
            id: checkout_id_for_path(&path),
            repo_id,
            path: path.clone(),
            canonical_path: path,
            is_primary,
            branch: None,
            head: None,
            ahead_of_default: None,
            changed_files: 0,
            is_missing: false,
            sessions: Vec::new(),
        })
    }
}

pub fn repo_id_for_path(canonical_path: &str) -> String {
    format!("repo:{canonical_path}")
}

pub fn checkout_id_for_path(canonical_path: &str) -> String {
    format!("checkout:{canonical_path}")
}

fn canonical_directory(path: &Path) -> Result<std::path::PathBuf, String> {
    let canonical_path =
        fs::canonicalize(path).map_err(|error| format!("could not open folder: {error}"))?;
    if !canonical_path.is_dir() {
        return Err("selected path is not a folder".into());
    }
    Ok(canonical_path)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{checkout_id_for_path, repo_id_for_path, Checkout, Repo, RepoKind, SessionType};

    #[test]
    fn session_types_serialize_and_reject_removed_variants() {
        assert_eq!(
            serde_json::to_string(&SessionType::Shell).unwrap(),
            "\"shell\""
        );
        assert_eq!(
            serde_json::to_string(&SessionType::Agent).unwrap(),
            "\"agent\""
        );

        for removed in ["nvim", "server", "custom"] {
            assert!(serde_json::from_str::<SessionType>(&format!("\"{removed}\"")).is_err());
        }
    }

    #[test]
    fn plain_repo_has_exactly_one_primary_checkout_with_canonical_ids() {
        let temp = tempdir().expect("temporary directory");
        let path = temp.path().join("plain");
        fs::create_dir(&path).expect("create folder");

        let repo = Repo::plain(&path, "2026-01-01T00:00:00Z").expect("plain repo");
        let canonical_path = path.canonicalize().unwrap().display().to_string();

        assert_eq!(repo.kind, RepoKind::Plain);
        assert_eq!(repo.id, repo_id_for_path(&canonical_path));
        assert_eq!(repo.root, canonical_path);
        assert_eq!(repo.checkouts.len(), 1);
        assert_eq!(repo.checkouts[0].id, checkout_id_for_path(&canonical_path));
        assert!(repo.checkouts[0].is_primary);
        assert!(repo.checkouts[0].sessions.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_alias_has_the_same_stable_repo_and_checkout_identity() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("temporary directory");
        let path = temp.path().join("real");
        let alias = temp.path().join("alias");
        fs::create_dir(&path).expect("create folder");
        symlink(&path, &alias).expect("create symlink");

        let direct = Repo::plain(&path, "now").expect("direct repo");
        let through_alias = Repo::plain(&alias, "now").expect("symlink repo");

        assert_eq!(direct.id, through_alias.id);
        assert_eq!(direct.checkouts[0].id, through_alias.checkouts[0].id);
        assert_eq!(direct.root, through_alias.root);
    }

    #[test]
    fn primary_and_worktree_share_the_same_checkout_model() {
        let temp = tempdir().expect("temporary directory");
        let primary_path = temp.path().join("primary");
        let worktree_path = temp.path().join("worktree");
        fs::create_dir(&primary_path).expect("create primary");
        fs::create_dir(&worktree_path).expect("create worktree");
        let repo_id = repo_id_for_path(&primary_path.canonicalize().unwrap().display().to_string());

        let primary =
            Checkout::new(repo_id.clone(), &primary_path, true).expect("primary checkout");
        let worktree = Checkout::new(repo_id, &worktree_path, false).expect("worktree checkout");

        assert!(primary.is_primary);
        assert!(!worktree.is_primary);
        assert_eq!(primary.changed_files, worktree.changed_files);
        assert_eq!(primary.sessions, worktree.sessions);
    }
}
