use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::domain::review::{ReviewNote, ReviewRound};
use crate::domain::terminal_layout::CheckoutTerminalLayout;
use crate::domain::workspace::{
    ArchivedCheckout, Checkout, RecentPath, Repo, RepoKind, Session, SessionStatus, SessionType,
    WorkspaceState,
};

const SCHEMA_VERSION: i64 = 18;
const REVIEW_NOTE_COLUMNS: &str = "id, checkout_id, path, side, line_start, line_end, content, code, status, code_hash, outdated, round_id, created_at, updated_at";
const REVIEW_ROUND_COLUMNS: &str =
    "id, checkout_id, session_id, status, marker, note_ids, created_at, updated_at";
const ACTIVE_CHECKOUT: &str = "active_checkout_id";
const ACTIVE_SESSION: &str = "active_session_id";
const WORKTREE_LOCATION: &str = "worktree_location";
const WINDOW_GEOMETRY: &str = "window_geometry";
const WINDOW_MAXIMIZED: &str = "window_maximized";
const UI_LAYOUT: &str = "ui_layout_v1";
const SIDEBAR_WIDTH_MIN: u32 = 240;
const SIDEBAR_WIDTH_MAX: u32 = 500;
const INSPECTOR_WIDTH_MIN: u32 = 200;
const INSPECTOR_WIDTH_MAX: u32 = 480;
const PREVIEW_WIDTH_MIN: u32 = 260;
const PREVIEW_WIDTH_MAX: u32 = 900;

/// The layout row is the window's own shape and nothing else: which mode it is in and how wide
/// the panes are. The preferences a person sets once (sizes, ligatures, the scale) live in
/// `~/.muster/config.yml`.
///
/// There is no version in the row. `SCHEMA_VERSION` is the only ladder in this database, and a
/// per-blob ladder inside it only ever bought a second way to keep reading shapes this build
/// cannot write. A row that does not parse into this struct is the default window and a deleted
/// row, which is the whole of what the ladder was buying.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AppLayoutState {
    pub mode: String,
    pub sidebar_width: u32,
    pub inspector_width: u32,
    pub preview_width: u32,
}

impl Default for AppLayoutState {
    fn default() -> Self {
        Self {
            mode: "focus".into(),
            sidebar_width: 240,
            inspector_width: 280,
            preview_width: 360,
        }
    }
}

impl AppLayoutState {
    /// Clamp a reported dimension and discard a mode this build does not name.
    ///
    /// This is validation of what arrives, not a reading of an older row: a row this build cannot
    /// read never reaches here, it is refused whole by [`Database::load_app_layout`].
    fn normalized(mut self) -> Self {
        if self.mode != "focus" && self.mode != "split" {
            self.mode = "focus".into();
        }
        self.sidebar_width = self
            .sidebar_width
            .clamp(SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX);
        self.inspector_width = self
            .inspector_width
            .clamp(INSPECTOR_WIDTH_MIN, INSPECTOR_WIDTH_MAX);
        self.preview_width = self
            .preview_width
            .clamp(PREVIEW_WIDTH_MIN, PREVIEW_WIDTH_MAX);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PersistedDocument {
    pub checkout_id: String,
    pub path: String,
    pub origin: String,
    pub source: String,
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutUiState {
    pub document: Option<PersistedDocument>,
    pub main_view: String,
    /// The whole-change-set diff has no path, so it cannot be a `document`.
    pub diff_all_files: bool,
    pub inspector_tab: String,
    pub selected_file_path: Option<String>,
    pub selected_change_path: Option<String>,
    pub expanded_directories: Vec<String>,
    pub files_scroll_top: u32,
    pub changes_scroll_top: u32,
    pub document_scroll_top: u32,
    pub document_scroll_left: u32,
    pub diff_scroll_top: u32,
}

impl Default for CheckoutUiState {
    fn default() -> Self {
        Self {
            document: None,
            main_view: "terminal".into(),
            diff_all_files: false,
            inspector_tab: "files".into(),
            selected_file_path: None,
            selected_change_path: None,
            expanded_directories: Vec::new(),
            files_scroll_top: 0,
            changes_scroll_top: 0,
            document_scroll_top: 0,
            document_scroll_left: 0,
            diff_scroll_top: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone)]
pub struct Database {
    connection: Arc<Mutex<Connection>>,
    home_checkout_id: Arc<Mutex<Option<String>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitRepoRegistrationSnapshot {
    repo_id: String,
    kind: String,
    name: String,
    root: String,
    default_branch: Option<String>,
    last_opened_at: String,
    checkouts: Vec<GitCheckoutRegistrationSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitCheckoutRegistrationSnapshot {
    id: String,
    path: String,
    canonical_path: String,
    is_primary: bool,
    branch: Option<String>,
    head: Option<String>,
    ahead_of_default: Option<u32>,
    position: i64,
    is_missing: bool,
    is_archived: bool,
}

impl GitRepoRegistrationSnapshot {
    pub(crate) fn checkout_ids(&self) -> Vec<String> {
        self.checkouts
            .iter()
            .map(|checkout| checkout.id.clone())
            .collect()
    }

    pub(crate) fn is_archived_checkout(&self, checkout_id: &str) -> bool {
        self.checkouts
            .iter()
            .any(|checkout| checkout.id == checkout_id && checkout.is_archived)
    }

    fn matches(&self, expected: &Self, writes_last_opened_at: bool) -> bool {
        self.repo_id == expected.repo_id
            && self.kind == expected.kind
            && self.name == expected.name
            && self.root == expected.root
            && self.default_branch == expected.default_branch
            && (!writes_last_opened_at || self.last_opened_at == expected.last_opened_at)
            && self.checkouts == expected.checkouts
    }
}

impl Database {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let connection = Connection::open(path).map_err(db_error)?;
        Self::from_connection(connection)
    }

    fn from_connection(connection: Connection) -> Result<Self, String> {
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(db_error)?;
        create_schema(&connection)?;
        // No PTY outlives the process that spawned it, so every stored session is dead the
        // moment the app reopens. Keeping the rows piled one per launch per checkout up
        // forever, and every one of them drags a dead tab into that checkout's saved layout.
        let transaction = connection.unchecked_transaction().map_err(db_error)?;
        transaction
            .execute("DELETE FROM sessions", [])
            .map_err(db_error)?;
        transaction
            .execute("DELETE FROM checkout_terminal_layouts", [])
            .map_err(db_error)?;
        set_preference(&transaction, ACTIVE_SESSION, None)?;
        transaction.commit().map_err(db_error)?;

        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            home_checkout_id: Arc::new(Mutex::new(None)),
        })
    }

    #[cfg(test)]
    fn open_in_memory() -> Result<Self, String> {
        Self::from_connection(Connection::open_in_memory().map_err(db_error)?)
    }

    pub fn ensure_not_home_checkout(&self, checkout_id: &str) -> Result<(), String> {
        let home_checkout_id = self
            .home_checkout_id
            .lock()
            .map_err(|error| error.to_string())?
            .clone();
        if home_checkout_id.as_deref() == Some(checkout_id) {
            return Err("the Home workdir cannot be removed".into());
        }
        Ok(())
    }

    pub fn set_home_checkout_id(&self, checkout_id: String) -> Result<(), String> {
        *self
            .home_checkout_id
            .lock()
            .map_err(|error| error.to_string())? = Some(checkout_id);
        Ok(())
    }

    /// Ensures the operating-system Home directory is listed first without treating it as a
    /// recently opened folder or changing its last-opened timestamp.
    pub fn register_home_repo(&self, repo: &Repo) -> Result<(), String> {
        let checkout = repo.checkouts.first().ok_or("Home has no checkout")?;
        if repo.kind != RepoKind::Plain
            || repo.id != crate::domain::workspace::repo_id_for_path(&repo.root)
            || repo.checkouts.len() != 1
            || !checkout.is_primary
            || checkout.repo_id != repo.id
            || checkout.id != crate::domain::workspace::checkout_id_for_path(&repo.root)
            || checkout.canonical_path != repo.root
            || Path::new(&repo.root).canonicalize().ok().as_deref() != Some(Path::new(&repo.root))
        {
            return Err("Home directory failed backend identity validation".into());
        }

        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let transaction = connection.unchecked_transaction().map_err(db_error)?;
        let existing = transaction
            .query_row(
                "SELECT r.id, c.id FROM checkouts c JOIN repos r ON r.id = c.repo_id
                 WHERE c.canonical_path = ?1",
                [&repo.root],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(db_error)?;

        let max_position: i64 = transaction
            .query_row("SELECT COALESCE(MAX(position), 0) FROM repos", [], |row| {
                row.get(0)
            })
            .map_err(db_error)?;
        let offset = max_position + 2;
        transaction
            .execute("UPDATE repos SET position = position + ?1", [offset])
            .map_err(db_error)?;
        transaction
            .execute("UPDATE repos SET position = position - ?1 + 1", [offset])
            .map_err(db_error)?;

        if let Some((repo_id, checkout_id)) = existing {
            if checkout_id != checkout.id {
                return Err("Home checkout has an unexpected identity".into());
            }
            transaction
                .execute(
                    "UPDATE repos SET name = 'Home', position = 0 WHERE id = ?1",
                    [&repo_id],
                )
                .map_err(db_error)?;
        } else {
            let repo_id_in_use: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM repos WHERE id = ?1)",
                    [&repo.id],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            if repo_id_in_use {
                return Err("Home repository identity is already in use".into());
            }
            transaction
                .execute(
                    "INSERT INTO repos (id, kind, name, root, default_branch, position, created_at, last_opened_at)
                     VALUES (?1, 'plain', 'Home', ?2, NULL, 0, ?3, ?4)",
                    params![repo.id, repo.root, repo.created_at, repo.last_opened_at],
                )
                .map_err(db_error)?;
            transaction
                .execute(
                    "INSERT INTO checkouts
                     (id, repo_id, path, canonical_path, is_primary, branch, head, ahead_of_default, changed_files, position)
                     VALUES (?1, ?2, ?3, ?4, 1, NULL, NULL, NULL, 0, 0)",
                    params![checkout.id, repo.id, checkout.path, checkout.canonical_path],
                )
                .map_err(db_error)?;
        }
        transaction
            .execute(
                "DELETE FROM recent_paths WHERE canonical_path = ?1",
                [&repo.root],
            )
            .map_err(db_error)?;
        transaction.commit().map_err(db_error)?;
        drop(connection);
        *self
            .home_checkout_id
            .lock()
            .map_err(|error| error.to_string())? = Some(checkout.id.clone());
        Ok(())
    }

    pub fn register_plain_repo(&self, repo: Repo) -> Result<WorkspaceState, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let transaction = connection.unchecked_transaction().map_err(db_error)?;
        let current_checkout_id = get_preference(&transaction, ACTIVE_CHECKOUT)?;
        let existing_repo_id = transaction
            .query_row(
                "SELECT repo_id FROM checkouts WHERE canonical_path = ?1",
                [&repo.root],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(db_error)?;
        let checkout_id = if let Some(repo_id) = existing_repo_id {
            transaction
                .execute(
                    "UPDATE repos SET last_opened_at = ?1 WHERE id = ?2",
                    params![repo.last_opened_at, repo_id],
                )
                .map_err(db_error)?;
            repo.checkouts[0].id.clone()
        } else {
            let repo_position = read_position(&transaction, MAX_POSITION_FROM_REPOS, [])?;
            transaction
                .execute(
                    "INSERT INTO repos (id, kind, name, root, default_branch, position, created_at, last_opened_at)
                     VALUES (?1, 'plain', ?2, ?3, NULL, ?4, ?5, ?6)",
                    params![
                        repo.id,
                        repo.name,
                        repo.root,
                        repo_position,
                        repo.created_at,
                        repo.last_opened_at
                    ],
                )
                .map_err(db_error)?;
            let checkout = &repo.checkouts[0];
            transaction
                .execute(
                    "INSERT INTO checkouts
                     (id, repo_id, path, canonical_path, is_primary, branch, head, ahead_of_default, changed_files, position)
                     VALUES (?1, ?2, ?3, ?4, 1, NULL, NULL, NULL, 0, 0)",
                    params![checkout.id, repo.id, checkout.path, checkout.canonical_path],
                )
                .map_err(db_error)?;
            checkout.id.clone()
        };

        set_preference(&transaction, ACTIVE_CHECKOUT, Some(&checkout_id))?;
        if current_checkout_id.as_deref() != Some(checkout_id.as_str()) {
            set_preference(&transaction, ACTIVE_SESSION, None)?;
        }
        transaction
            .execute(
                "INSERT INTO recent_paths (canonical_path, last_opened_at)
                 VALUES (?1, ?2)
                 ON CONFLICT(canonical_path) DO UPDATE SET last_opened_at = excluded.last_opened_at",
                params![repo.root, repo.last_opened_at],
            )
            .map_err(db_error)?;
        transaction
            .execute(
                "DELETE FROM recent_paths WHERE canonical_path NOT IN
                 (SELECT canonical_path FROM recent_paths ORDER BY last_opened_at DESC LIMIT 10)",
                [],
            )
            .map_err(db_error)?;
        transaction.commit().map_err(db_error)?;
        drop(connection);
        self.load_workspace()
    }

    pub fn register_git_repo(
        &self,
        repo: Repo,
        focus_checkout_id: &str,
    ) -> Result<WorkspaceState, String> {
        self.store_git_repo(&repo, Some(focus_checkout_id))?;
        self.load_workspace()
    }

    /// Register against a full snapshot, or only if absent when the snapshot is `None`.
    pub(crate) fn register_git_repo_if_unchanged(
        &self,
        repo: &Repo,
        focus_checkout_id: &str,
        expected: Option<&GitRepoRegistrationSnapshot>,
    ) -> Result<Option<WorkspaceState>, String> {
        if !self.store_git_repo_replacing(
            repo,
            Some(focus_checkout_id),
            None,
            expected,
            expected.is_none(),
        )? {
            return Ok(None);
        }
        self.load_workspace().map(Some)
    }

    pub fn reconcile_git_repo(&self, repo: &Repo) -> Result<(), String> {
        self.store_git_repo(repo, None)
    }

    /// Capture the persisted fields a Git reconciliation may update, including archived rows.
    pub(crate) fn git_repo_registration_snapshot(
        &self,
        repo_id: &str,
    ) -> Result<Option<GitRepoRegistrationSnapshot>, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let transaction = connection.unchecked_transaction().map_err(db_error)?;
        let snapshot = git_repo_registration_snapshot_in(&transaction, repo_id)?;
        transaction.commit().map_err(db_error)?;
        Ok(snapshot)
    }

    /// Reconcile only if the full persisted registration still matches the snapshot.
    /// Returns false without writing if another operation changed it.
    pub(crate) fn reconcile_git_repo_if_unchanged(
        &self,
        repo: &Repo,
        expected: &GitRepoRegistrationSnapshot,
    ) -> Result<bool, String> {
        self.store_git_repo_replacing(repo, None, None, Some(expected), false)
    }

    fn store_git_repo(&self, repo: &Repo, focus_checkout_id: Option<&str>) -> Result<(), String> {
        self.store_git_repo_replacing(repo, focus_checkout_id, None, None, false)
            .map(|_| ())
    }

    pub(crate) fn locate_git_checkout_if_unchanged(
        &self,
        repo: &Repo,
        missing_checkout_id: &str,
        focus_checkout_id: &str,
        expected: &GitRepoRegistrationSnapshot,
    ) -> Result<bool, String> {
        self.store_git_repo_replacing(
            repo,
            Some(focus_checkout_id),
            Some(missing_checkout_id),
            Some(expected),
            false,
        )
    }

    pub fn relocate_plain_checkout(
        &self,
        old_repo_id: &str,
        old_checkout_id: &str,
        new_repo: &Repo,
    ) -> Result<WorkspaceState, String> {
        if new_repo.kind != RepoKind::Plain
            || new_repo.id != crate::domain::workspace::repo_id_for_path(&new_repo.root)
            || new_repo.checkouts.len() != 1
            || !new_repo.checkouts[0].is_primary
            || new_repo.checkouts[0].repo_id != new_repo.id
            || new_repo.checkouts[0].id
                != crate::domain::workspace::checkout_id_for_path(&new_repo.root)
            || new_repo.checkouts[0].canonical_path != new_repo.root
            || Path::new(&new_repo.root).canonicalize().ok().as_deref()
                != Some(Path::new(&new_repo.root))
        {
            return Err("located plain checkout failed backend identity validation".into());
        }
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let transaction = connection.unchecked_transaction().map_err(db_error)?;
        let (kind, is_primary, stored_path, old_root, created_at, repo_position, is_missing) =
            transaction
                .query_row(
                    "SELECT r.kind, c.is_primary, c.canonical_path, r.root, r.created_at,
                            r.position, c.is_missing
                     FROM checkouts c JOIN repos r ON r.id = c.repo_id
                     WHERE r.id = ?1 AND c.id = ?2",
                    params![old_repo_id, old_checkout_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, bool>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, bool>(6)?,
                        ))
                    },
                )
                .optional()
                .map_err(db_error)?
                .ok_or_else(|| "missing plain checkout is no longer registered".to_string())?;
        if kind != "plain"
            || !is_primary
            || is_missing
            || Path::new(&stored_path).is_dir()
            || new_repo.id == old_repo_id
            || new_repo.checkouts[0].canonical_path != new_repo.root
            || old_repo_id != crate::domain::workspace::repo_id_for_path(&old_root)
            || old_checkout_id != crate::domain::workspace::checkout_id_for_path(&stored_path)
        {
            return Err("located directory does not match a missing plain checkout".into());
        }
        let checkout_count: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM checkouts WHERE repo_id = ?1",
                [old_repo_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        let active_sessions: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE checkout_id = ?1 AND status = 'active')",
                [old_checkout_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if checkout_count != 1 || active_sessions {
            return Err("close active sessions before relocating this plain checkout".into());
        }
        let path_in_use: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM checkouts WHERE canonical_path = ?1)",
                [&new_repo.root],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        let repo_id_in_use: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM repos WHERE id = ?1)",
                [&new_repo.id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if path_in_use || repo_id_in_use {
            return Err("located directory is already registered in Muster".into());
        }
        let next_position: i64 = transaction
            .query_row(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM repos",
                [],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        transaction
            .execute(
                "UPDATE repos SET position = ?1 WHERE id = ?2",
                params![next_position, old_repo_id],
            )
            .map_err(db_error)?;
        transaction
            .execute(
                "INSERT INTO repos (id, kind, name, root, default_branch, position, created_at, last_opened_at)
                 VALUES (?1, 'plain', ?2, ?3, NULL, ?4, ?5, ?6)",
                params![
                    new_repo.id,
                    new_repo.name,
                    new_repo.root,
                    repo_position,
                    created_at,
                    new_repo.last_opened_at,
                ],
            )
            .map_err(db_error)?;
        let checkout = &new_repo.checkouts[0];
        transaction
            .execute(
                "INSERT INTO checkouts
                 (id, repo_id, path, canonical_path, is_primary, branch, head, ahead_of_default, changed_files, position, is_missing)
                 VALUES (?1, ?2, ?3, ?4, 1, NULL, NULL, NULL, 0, 0, 0)",
                params![checkout.id, new_repo.id, checkout.path, checkout.canonical_path],
            )
            .map_err(db_error)?;
        transfer_checkout_metadata(&transaction, old_checkout_id, &checkout.id)?;
        transaction
            .execute("DELETE FROM repos WHERE id = ?1", [old_repo_id])
            .map_err(db_error)?;
        transaction
            .execute(
                "INSERT INTO recent_paths (canonical_path, last_opened_at) VALUES (?1, ?2)
                 ON CONFLICT(canonical_path) DO UPDATE SET last_opened_at = excluded.last_opened_at",
                params![new_repo.root, new_repo.last_opened_at],
            )
            .map_err(db_error)?;
        let workspace = load_workspace(&transaction)?;
        transaction.commit().map_err(db_error)?;
        Ok(workspace)
    }

    fn store_git_repo_replacing(
        &self,
        repo: &Repo,
        focus_checkout_id: Option<&str>,
        replace_missing_checkout_id: Option<&str>,
        expected_snapshot: Option<&GitRepoRegistrationSnapshot>,
        require_absent: bool,
    ) -> Result<bool, String> {
        if repo.kind != RepoKind::Git
            || repo.id != crate::domain::workspace::repo_id_for_path(&repo.root)
            || repo.checkouts.is_empty()
            || repo.checkouts.iter().any(|checkout| {
                checkout.repo_id != repo.id
                    || checkout.id
                        != crate::domain::workspace::checkout_id_for_path(&checkout.canonical_path)
            })
            || !repo
                .checkouts
                .iter()
                .any(|checkout| checkout.is_primary && checkout.canonical_path == repo.root)
        {
            return Err("Git repository membership failed backend validation".into());
        }
        if let Some(checkout_id) = focus_checkout_id {
            if !repo
                .checkouts
                .iter()
                .any(|checkout| checkout.id == checkout_id)
            {
                return Err("requested checkout is not a member of the resolved repository".into());
            }
        }

        let mut connection = self.connection.lock().map_err(|error| error.to_string())?;
        let transaction = if expected_snapshot.is_some() || require_absent {
            connection.transaction_with_behavior(TransactionBehavior::Immediate)
        } else {
            connection.unchecked_transaction()
        }
        .map_err(db_error)?;
        if let Some(expected) = expected_snapshot {
            let current = git_repo_registration_snapshot_in(&transaction, &repo.id)?;
            if expected.repo_id != repo.id
                || expected.root != repo.root
                || !current
                    .is_some_and(|current| current.matches(expected, focus_checkout_id.is_some()))
            {
                return Ok(false);
            }
        } else if require_absent
            && git_repo_registration_snapshot_in(&transaction, &repo.id)?.is_some()
        {
            return Ok(false);
        }
        let now = &repo.last_opened_at;
        let position = read_repo_position(&transaction, &repo.id)?;
        transaction
            .execute(
                "INSERT INTO repos (id, kind, name, root, default_branch, position, created_at, last_opened_at)
                 VALUES (?1, 'git', ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                   kind = 'git', name = excluded.name, root = excluded.root,
                   default_branch = COALESCE(repos.default_branch, excluded.default_branch),
                   last_opened_at = CASE WHEN ?8 THEN excluded.last_opened_at ELSE repos.last_opened_at END",
                params![
                    repo.id,
                    repo.name,
                    repo.root,
                    repo.default_branch,
                    position,
                    repo.created_at,
                    now,
                    focus_checkout_id.is_some(),
                ],
            )
            .map_err(db_error)?;
        // The checkouts Git no longer lists are marked missing and moved past the ones it does,
        // each to its own number, and they are given that number rather than having their own
        // added to it. Adding the offset to the stored position made every sync double them,
        // because the next offset was read back from a column the last one had already grown:
        // sixty-odd syncs later it held a value past `i64::MAX`, SQLite stored it as a REAL, and
        // every later read of a position failed with a type error — one removed worktree stopping
        // any folder from being registered.
        //
        // The number is absolute, so syncing the same repository again writes the same one and
        // the rows do not drift. The live checkouts are written from `repo.checkouts` immediately
        // below and take the places 0..n back whatever they had here, and the ones that are gone
        // keep their order among themselves by ranking on `rowid`, which `UNIQUE (repo_id,
        // position)` would otherwise refuse.
        // The checkouts Git no longer lists keep a number of their own, past everything that is
        // still there, and they are given that number rather than having their own added to it.
        // Adding the offset to the stored position made every sync double them, because the next
        // offset was read back from a column the last one had already grown: sixty-odd syncs later
        // it held a value past `i64::MAX`, SQLite stored it as a REAL, and every later read of a
        // position failed with a type error — one removed worktree stopping any folder from being
        // registered.
        //
        // They are written in reverse order and each takes the place the row above it just left,
        // so no two ever hold the same number while `UNIQUE (repo_id, position)` is watching, and
        // the run always starts above the largest position already in the repo: a repo whose
        // checkouts were all moved out of the way earlier must not be handed the same range back.
        let highest: i64 = read_position(&transaction, HIGHEST_CHECKOUT_POSITION, [&repo.id])?;
        let first_absent = highest.max(repo.checkouts.len() as i64) + 1;
        let existing = transaction
            .prepare("SELECT rowid FROM checkouts WHERE repo_id = ?1 ORDER BY position DESC")
            .and_then(|mut statement| {
                statement
                    .query_map([&repo.id], |row| row.get::<_, i64>(0))?
                    .collect::<Result<Vec<i64>, rusqlite::Error>>()
            })
            .map_err(db_error)?;
        for (offset, rowid) in existing.iter().enumerate() {
            transaction
                .execute(
                    "UPDATE checkouts SET is_missing = 1, position = ?2 WHERE rowid = ?1",
                    params![rowid, first_absent + offset as i64],
                )
                .map_err(db_error)?;
        }
        for (checkout_position, checkout) in repo.checkouts.iter().enumerate() {
            transaction
                .execute(
                    "INSERT INTO checkouts
                     (id, repo_id, path, canonical_path, is_primary, branch, head, ahead_of_default, changed_files, position, is_missing)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                     ON CONFLICT(id) DO UPDATE SET
                       repo_id = excluded.repo_id, path = excluded.path,
                       canonical_path = excluded.canonical_path, is_primary = excluded.is_primary,
                       branch = excluded.branch, head = excluded.head,
                       ahead_of_default = excluded.ahead_of_default, position = excluded.position,
                       is_missing = excluded.is_missing",
                    params![
                        checkout.id,
                        repo.id,
                        checkout.path,
                        checkout.canonical_path,
                        checkout.is_primary,
                        checkout.branch,
                        checkout.head,
                        checkout.ahead_of_default,
                        checkout.changed_files,
                        checkout_position as i64,
                        checkout.is_missing,
                    ],
                )
                .map_err(db_error)?;
        }
        if let Some(missing_checkout_id) = replace_missing_checkout_id {
            let (old_repo_id, is_primary, is_missing, branch, head) = transaction
                .query_row(
                    "SELECT repo_id, is_primary, is_missing, branch, head FROM checkouts WHERE id = ?1",
                    [missing_checkout_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, bool>(1)?,
                            row.get::<_, bool>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, Option<String>>(4)?,
                        ))
                    },
                )
                .optional()
                .map_err(db_error)?
                .ok_or_else(|| "missing checkout is no longer registered".to_string())?;
            let replacement = repo
                .checkouts
                .iter()
                .find(|checkout| Some(checkout.id.as_str()) == focus_checkout_id)
                .ok_or_else(|| {
                    "located checkout is not part of the resolved repository".to_string()
                })?;
            let identity_matches = match (branch.as_deref(), replacement.branch.as_deref()) {
                (Some(previous), Some(located)) => previous == located,
                (None, None) => head
                    .as_deref()
                    .is_some_and(|previous| replacement.head.as_deref() == Some(previous)),
                _ => false,
            };
            if old_repo_id != repo.id || is_primary || !is_missing || !identity_matches {
                return Err(
                    "located directory does not match the missing worktree identity".into(),
                );
            }
            let has_active_sessions: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sessions WHERE checkout_id = ?1 AND status = 'active')",
                    [missing_checkout_id],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            if has_active_sessions {
                return Err(
                    "close active terminal sessions before relocating this worktree".into(),
                );
            }
            transfer_checkout_metadata(&transaction, missing_checkout_id, &replacement.id)?;
            transaction
                .execute("DELETE FROM checkouts WHERE id = ?1", [missing_checkout_id])
                .map_err(db_error)?;
        }
        if let Some(checkout_id) = focus_checkout_id {
            let current_checkout_id = get_preference(&transaction, ACTIVE_CHECKOUT)?;
            set_preference(&transaction, ACTIVE_CHECKOUT, Some(checkout_id))?;
            if current_checkout_id.as_deref() != Some(checkout_id) {
                set_preference(&transaction, ACTIVE_SESSION, None)?;
            }
            transaction
                .execute(
                    "INSERT INTO recent_paths (canonical_path, last_opened_at)
                     VALUES (?1, ?2)
                     ON CONFLICT(canonical_path) DO UPDATE SET last_opened_at = excluded.last_opened_at",
                    params![repo.root, now],
                )
                .map_err(db_error)?;
        }
        transaction.commit().map_err(db_error)?;
        Ok(true)
    }

    pub fn existing_git_checkout(&self, repo_id: &str) -> Result<Option<PathBuf>, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT canonical_path FROM checkouts
                 WHERE repo_id = ?1 ORDER BY is_primary DESC, position",
            )
            .map_err(db_error)?;
        let paths = statement
            .query_map([repo_id], |row| row.get::<_, String>(0))
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        Ok(paths
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.is_dir()))
    }

    pub fn set_default_branch(
        &self,
        repo_id: &str,
        branch: &str,
    ) -> Result<WorkspaceState, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let updated = connection
            .execute(
                "UPDATE repos SET default_branch = ?1 WHERE id = ?2 AND kind = 'git'",
                params![branch, repo_id],
            )
            .map_err(db_error)?;
        if updated == 0 {
            return Err("Git repository does not exist".into());
        }
        drop(connection);
        self.load_workspace()
    }

    pub fn worktree_location(&self) -> Result<Option<String>, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        get_preference(&connection, WORKTREE_LOCATION)
    }

    pub fn set_worktree_location(&self, path: &str) -> Result<(), String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        connection
            .execute(
                "INSERT INTO preferences (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![WORKTREE_LOCATION, path],
            )
            .map_err(db_error)?;
        Ok(())
    }

    pub fn window_geometry(&self) -> Result<Option<WindowGeometry>, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        get_preference(&connection, WINDOW_GEOMETRY)?
            .map(|value| serde_json::from_str(&value).map_err(|error| error.to_string()))
            .transpose()
    }

    pub fn set_window_geometry(&self, geometry: WindowGeometry) -> Result<(), String> {
        if geometry.width < 900
            || geometry.height < 600
            || geometry.width > 16_384
            || geometry.height > 16_384
        {
            return Err("window geometry is outside the supported range".into());
        }
        let serialized = serde_json::to_string(&geometry).map_err(|error| error.to_string())?;
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        connection
            .execute(
                "INSERT INTO preferences (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![WINDOW_GEOMETRY, serialized],
            )
            .map_err(db_error)?;
        Ok(())
    }

    pub fn window_maximized(&self) -> Result<bool, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        Ok(get_preference(&connection, WINDOW_MAXIMIZED)?.as_deref() == Some("true"))
    }

    pub fn set_window_maximized(&self, maximized: bool) -> Result<(), String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        connection
            .execute(
                "INSERT INTO preferences (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![WINDOW_MAXIMIZED, maximized.to_string()],
            )
            .map_err(db_error)?;
        Ok(())
    }

    /// Reads the layout row, and drops it when it does not parse into this build's shape.
    ///
    /// There is no version to compare, so the whole refusal is the parse: what this build cannot
    /// read is the default window, and deleting the row says so out loud instead of leaving one to
    /// be read again on the next launch.
    pub fn load_app_layout(&self) -> Result<AppLayoutState, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let Some(serialized) = get_preference(&connection, UI_LAYOUT)? else {
            return Ok(AppLayoutState::default());
        };
        let layout = serde_json::from_str::<AppLayoutState>(&serialized)
            .ok()
            .map(AppLayoutState::normalized)
            .unwrap_or_default();
        if serde_json::to_string(&layout).map_err(|error| error.to_string())? != serialized {
            connection
                .execute("DELETE FROM preferences WHERE key = ?1", [UI_LAYOUT])
                .map_err(db_error)?;
        }
        Ok(layout)
    }

    pub fn save_app_layout(&self, layout: &AppLayoutState) -> Result<(), String> {
        let layout = layout.clone().normalized();
        let serialized = serde_json::to_string(&layout).map_err(|error| error.to_string())?;
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        connection
            .execute(
                "INSERT INTO preferences (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![UI_LAYOUT, serialized],
            )
            .map_err(db_error)?;
        Ok(())
    }

    pub fn load_checkout_ui_state(&self, checkout_id: &str) -> Result<CheckoutUiState, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM checkouts WHERE id = ?1)",
                [checkout_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if !exists {
            return Err("checkout does not exist".into());
        }
        let serialized = connection
            .query_row(
                "SELECT state_json FROM checkout_ui_states WHERE checkout_id = ?1",
                [checkout_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(db_error)?;
        let Some(serialized) = serialized else {
            return Ok(CheckoutUiState::default());
        };
        let state = serde_json::from_str::<CheckoutUiState>(&serialized)
            .ok()
            .filter(|state| validate_checkout_ui_state(checkout_id, state))
            .unwrap_or_default();
        if serde_json::to_string(&state).map_err(|error| error.to_string())? != serialized {
            connection
                .execute(
                    "DELETE FROM checkout_ui_states WHERE checkout_id = ?1",
                    [checkout_id],
                )
                .map_err(db_error)?;
        }
        Ok(state)
    }

    /// Which program a checkout's reviews are delivered to, or `markdown` when it exports.
    ///
    /// Each installed agent opens its own path to the same place.
    pub fn review_target(&self, checkout_id: &str) -> Result<String, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let key = format!("review_target:{checkout_id}");
        Ok(match get_preference(&connection, &key)?.as_deref() {
            Some(stored @ ("opencode" | "pi")) => stored.into(),
            _ => "markdown".into(),
        })
    }

    pub fn set_review_target(&self, checkout_id: &str, target: &str) -> Result<(), String> {
        if !matches!(target, "markdown" | "opencode" | "pi") {
            return Err("review target is not supported".into());
        }
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let transaction = connection.unchecked_transaction().map_err(db_error)?;
        let key = format!("review_target:{checkout_id}");
        set_preference(&transaction, &key, Some(target))?;
        transaction.commit().map_err(db_error)
    }

    pub fn save_checkout_ui_state(
        &self,
        checkout_id: &str,
        state: &CheckoutUiState,
    ) -> Result<(), String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let checkout_exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM checkouts WHERE id = ?1)",
                [checkout_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if !checkout_exists || !validate_checkout_ui_state(checkout_id, state) {
            return Err("saved checkout UI state failed validation".into());
        }
        let serialized = serde_json::to_string(state).map_err(|error| error.to_string())?;
        connection
            .execute(
                "INSERT INTO checkout_ui_states (checkout_id, state_json) VALUES (?1, ?2)
                 ON CONFLICT(checkout_id) DO UPDATE SET state_json = excluded.state_json",
                params![checkout_id, serialized],
            )
            .map_err(db_error)?;
        Ok(())
    }

    /// Drops the registration of a checkout whose directory is still on disk, and nothing else.
    ///
    /// This is what takes a workdir off the panel while its files stay where they are: the row
    /// and everything that hangs off it go, the directory does not, so opening the folder again
    /// brings the workdir back.
    pub fn close_checkout(&self, checkout_id: &str) -> Result<WorkspaceState, String> {
        self.forget_checkout(checkout_id, false)
    }

    /// Drops an unavailable checkout only when the caller explicitly chooses to close it.
    /// A live one is refused, so it can only be forgotten through [`Database::close_checkout`].
    pub fn close_missing_checkout(&self, checkout_id: &str) -> Result<WorkspaceState, String> {
        self.forget_checkout(checkout_id, true)
    }

    /// The one routine behind both explicit closes. `require_missing` is the whole difference:
    /// the missing-checkout action refuses a live directory.
    fn forget_checkout(
        &self,
        checkout_id: &str,
        require_missing: bool,
    ) -> Result<WorkspaceState, String> {
        self.ensure_not_home_checkout(checkout_id)?;
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let transaction = connection.unchecked_transaction().map_err(db_error)?;
        let (repo_id, is_primary, stored_missing, kind, path) = transaction
            .query_row(
                "SELECT c.repo_id, c.is_primary, c.is_missing, r.kind, c.canonical_path FROM checkouts c
                 JOIN repos r ON r.id = c.repo_id WHERE c.id = ?1",
                [checkout_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| "checkout is no longer registered".to_string())?;
        let is_missing = stored_missing || !Path::new(&path).is_dir();
        if require_missing && !is_missing {
            return Err("only a missing checkout can be closed".into());
        }
        let affected_id = if is_primary || kind == "plain" {
            &repo_id
        } else {
            checkout_id
        };
        let has_active_sessions: bool = transaction
            .query_row(
                if is_primary || kind == "plain" {
                    "SELECT EXISTS(SELECT 1 FROM sessions s JOIN checkouts c ON c.id = s.checkout_id WHERE c.repo_id = ?1 AND s.status = 'active')"
                } else {
                    "SELECT EXISTS(SELECT 1 FROM sessions WHERE checkout_id = ?1 AND status = 'active')"
                },
                [affected_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if has_active_sessions {
            // A terminal still attached to the directory would outlive the row that names it.
            return Err(if is_missing {
                "close active terminal sessions before closing this missing location".into()
            } else {
                "close active terminal sessions before closing this checkout".into()
            });
        }
        let current_checkout_id = get_preference(&transaction, ACTIVE_CHECKOUT)?;
        if is_primary || kind == "plain" {
            transaction
                .execute("DELETE FROM repos WHERE id = ?1", [&repo_id])
                .map_err(db_error)?;
            if current_checkout_id.as_deref().is_some_and(|active_id| {
                transaction
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM checkouts WHERE id = ?1)",
                        [active_id],
                        |row| row.get::<_, bool>(0),
                    )
                    .unwrap_or(false)
            }) {
                // An active checkout in a different repo remains selected.
            } else {
                set_preference(&transaction, ACTIVE_CHECKOUT, None)?;
                set_preference(&transaction, ACTIVE_SESSION, None)?;
            }
        } else {
            transaction
                .execute(
                    "DELETE FROM checkouts WHERE id = ?1 AND repo_id = ?2",
                    params![checkout_id, repo_id],
                )
                .map_err(db_error)?;
            if current_checkout_id.as_deref() == Some(checkout_id) {
                let primary_id: String = transaction
                    .query_row(
                        "SELECT id FROM checkouts WHERE repo_id = ?1 AND is_primary = 1",
                        [&repo_id],
                        |row| row.get(0),
                    )
                    .map_err(db_error)?;
                set_preference(&transaction, ACTIVE_CHECKOUT, Some(&primary_id))?;
                set_preference(&transaction, ACTIVE_SESSION, None)?;
            }
        }
        let workspace = load_workspace(&transaction)?;
        transaction.commit().map_err(db_error)?;
        Ok(workspace)
    }

    /// Takes a worktree off the panel without forgetting it, and leaves the disk exactly
    /// as it was: the branch, the commits and the files stay, and
    /// [`Database::restore_archived_worktrees`] puts the row back.
    ///
    /// Only a live worktree of a Git repository can be archived. A repo root is the head
    /// of the list its worktrees hang off, so archiving it would archive the list; a
    /// plain folder is not a worktree at all; and a worktree whose directory is gone has
    /// nothing to bring back, which is what closing it is for. A terminal still running
    /// in it is refused for the same reason a close is: the process would outlive the
    /// row that names it.
    pub fn archive_checkout(&self, checkout_id: &str) -> Result<WorkspaceState, String> {
        self.ensure_not_home_checkout(checkout_id)?;
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let transaction = connection.unchecked_transaction().map_err(db_error)?;
        let (repo_id, is_primary, stored_missing, kind, path) = transaction
            .query_row(
                "SELECT c.repo_id, c.is_primary, c.is_missing, r.kind, c.canonical_path FROM checkouts c
                 JOIN repos r ON r.id = c.repo_id WHERE c.id = ?1",
                [checkout_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| "checkout is no longer registered".to_string())?;
        if is_primary || kind == "plain" {
            return Err("only a worktree can be archived".into());
        }
        if stored_missing || !Path::new(&path).is_dir() {
            return Err(
                "a worktree whose directory is gone cannot be archived; close it instead".into(),
            );
        }
        let has_active_sessions: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE checkout_id = ?1 AND status = 'active')",
                [checkout_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if has_active_sessions {
            return Err("close active terminal sessions before archiving this worktree".into());
        }
        transaction
            .execute(
                "UPDATE checkouts SET is_archived = 1 WHERE id = ?1",
                [checkout_id],
            )
            .map_err(db_error)?;
        // The row leaves the panel, so the selection cannot stay on it: the workdir the
        // user is looking at after archiving is the repo root, the same place a close
        // hands the selection over to.
        if get_preference(&transaction, ACTIVE_CHECKOUT)?.as_deref() == Some(checkout_id) {
            let primary_id: String = transaction
                .query_row(
                    "SELECT id FROM checkouts WHERE repo_id = ?1 AND is_primary = 1",
                    [&repo_id],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            set_preference(&transaction, ACTIVE_CHECKOUT, Some(&primary_id))?;
            set_preference(&transaction, ACTIVE_SESSION, None)?;
        }
        let workspace = load_workspace(&transaction)?;
        transaction.commit().map_err(db_error)?;
        Ok(workspace)
    }

    /// Puts every archived worktree of one repository back on the panel, and leaves the
    /// disk alone: nothing here creates, removes or checks out anything.
    pub fn restore_archived_worktrees(&self, repo_id: &str) -> Result<WorkspaceState, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let transaction = connection.unchecked_transaction().map_err(db_error)?;
        let exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM repos WHERE id = ?1)",
                [repo_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if !exists {
            return Err("repository is no longer registered".into());
        }
        transaction
            .execute(
                "UPDATE checkouts SET is_archived = 0 WHERE repo_id = ?1 AND is_archived = 1",
                [repo_id],
            )
            .map_err(db_error)?;
        let workspace = load_workspace(&transaction)?;
        transaction.commit().map_err(db_error)?;
        Ok(workspace)
    }

    pub fn terminal_session_checkout(&self, session_id: &str) -> Result<Option<String>, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        connection
            .query_row(
                "SELECT checkout_id FROM sessions WHERE id = ?1",
                [session_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error)
    }

    pub fn remove_checkout(
        &self,
        repo_id: &str,
        checkout_id: &str,
    ) -> Result<WorkspaceState, String> {
        self.ensure_not_home_checkout(checkout_id)?;
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let transaction = connection.unchecked_transaction().map_err(db_error)?;
        let checkout = transaction
            .query_row(
                "SELECT is_primary FROM checkouts WHERE id = ?1 AND repo_id = ?2",
                params![checkout_id, repo_id],
                |row| row.get::<_, bool>(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| "checkout is not registered under this repository".to_string())?;
        if checkout {
            return Err("the primary checkout cannot be removed".into());
        }
        transaction
            .execute(
                "DELETE FROM checkouts WHERE id = ?1 AND repo_id = ?2",
                params![checkout_id, repo_id],
            )
            .map_err(db_error)?;
        if get_preference(&transaction, ACTIVE_CHECKOUT)?.as_deref() == Some(checkout_id) {
            let primary_id: String = transaction
                .query_row(
                    "SELECT id FROM checkouts WHERE repo_id = ?1 AND is_primary = 1",
                    [repo_id],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            set_preference(&transaction, ACTIVE_CHECKOUT, Some(&primary_id))?;
            set_preference(&transaction, ACTIVE_SESSION, None)?;
        } else if let Some(active_session) = get_preference(&transaction, ACTIVE_SESSION)? {
            let still_exists: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
                    [&active_session],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            if !still_exists {
                set_preference(&transaction, ACTIVE_SESSION, None)?;
            }
        }
        let workspace = load_workspace(&transaction)?;
        transaction.commit().map_err(db_error)?;
        Ok(workspace)
    }

    pub fn select_checkout(&self, checkout_id: Option<String>) -> Result<WorkspaceState, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let transaction = connection.unchecked_transaction().map_err(db_error)?;
        if let Some(checkout_id) = checkout_id.as_deref() {
            let exists: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM checkouts WHERE id = ?1)",
                    [checkout_id],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            if !exists {
                return Err("checkout does not exist".into());
            }
            let current = get_preference(&transaction, ACTIVE_CHECKOUT)?;
            set_preference(&transaction, ACTIVE_CHECKOUT, Some(checkout_id))?;
            if current.as_deref() != Some(checkout_id) {
                set_preference(&transaction, ACTIVE_SESSION, None)?;
            }
        } else {
            set_preference(&transaction, ACTIVE_CHECKOUT, None)?;
            set_preference(&transaction, ACTIVE_SESSION, None)?;
        }
        transaction.commit().map_err(db_error)?;
        drop(connection);
        self.load_workspace()
    }

    pub fn select_session(&self, session_id: Option<String>) -> Result<WorkspaceState, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let transaction = connection.unchecked_transaction().map_err(db_error)?;
        if let Some(session_id) = session_id.as_deref() {
            let checkout_id = transaction
                .query_row(
                    "SELECT checkout_id FROM sessions WHERE id = ?1",
                    [session_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(db_error)?
                .ok_or_else(|| "session does not exist".to_string())?;
            set_preference(&transaction, ACTIVE_CHECKOUT, Some(&checkout_id))?;
            set_preference(&transaction, ACTIVE_SESSION, Some(session_id))?;
        } else {
            set_preference(&transaction, ACTIVE_SESSION, None)?;
        }
        transaction.commit().map_err(db_error)?;
        drop(connection);
        self.load_workspace()
    }

    /// Every checkout still on the panel, with the directory it lives in.
    ///
    /// A missing or archived checkout is left out because a session cannot be handed to one:
    /// there is no row to put it under and the directory behind it is not there to work in.
    /// The stored path is returned as it is, because `canonical_path` is already canonical and
    /// resolving it again per call is work a directory comparison would do anyway.
    pub fn checkout_directories(&self) -> Result<Vec<(String, PathBuf)>, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT id, canonical_path FROM checkouts WHERE is_missing = 0 AND is_archived = 0",
            )
            .map_err(db_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    PathBuf::from(row.get::<_, String>(1)?),
                ))
            })
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        Ok(rows)
    }

    pub fn terminal_checkout_path(&self, checkout_id: &str) -> Result<PathBuf, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let stored_path = connection
            .query_row(
                "SELECT canonical_path FROM checkouts WHERE id = ?1 AND is_missing = 0 AND is_archived = 0",
                [checkout_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| "checkout does not exist or is missing".to_string())?;
        let path = PathBuf::from(&stored_path)
            .canonicalize()
            .map_err(|error| format!("checkout directory is unavailable: {error}"))?;
        if !path.is_dir() || path != Path::new(&stored_path) {
            return Err("checkout path is unavailable or no longer canonical".into());
        }
        Ok(path)
    }

    pub fn add_terminal_session(&self, session: &Session) -> Result<WorkspaceState, String> {
        self.add_active_session(session)
    }

    pub fn add_active_session(&self, session: &Session) -> Result<WorkspaceState, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let transaction = connection.unchecked_transaction().map_err(db_error)?;
        let checkout_exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM checkouts WHERE id = ?1 AND is_missing = 0)",
                [&session.checkout_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if !checkout_exists {
            return Err("checkout does not exist or is missing".into());
        }
        transaction
            .execute(
                "INSERT INTO sessions (id, checkout_id, session_type, name, created_at, status)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'active')",
                params![
                    session.id,
                    session.checkout_id,
                    session_type_name(&session.session_type),
                    session.name,
                    session.created_at
                ],
            )
            .map_err(db_error)?;
        set_preference(&transaction, ACTIVE_CHECKOUT, Some(&session.checkout_id))?;
        set_preference(&transaction, ACTIVE_SESSION, Some(&session.id))?;
        transaction.commit().map_err(db_error)?;
        drop(connection);
        self.load_workspace()
    }

    pub fn remove_terminal_session(&self, session_id: &str) -> Result<WorkspaceState, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let transaction = connection.unchecked_transaction().map_err(db_error)?;
        let removed = transaction
            .execute("DELETE FROM sessions WHERE id = ?1", [session_id])
            .map_err(db_error)?;
        if removed == 0 {
            return Err("session does not exist".into());
        }
        if get_preference(&transaction, ACTIVE_SESSION)?.as_deref() == Some(session_id) {
            set_preference(&transaction, ACTIVE_SESSION, None)?;
        }
        let workspace = load_workspace(&transaction)?;
        transaction.commit().map_err(db_error)?;
        Ok(workspace)
    }

    /// Renames a session in place.
    ///
    /// The row is already there and the name is a plain label, so this is one `UPDATE`: no new
    /// column and no new table, which matters because the schema is written once and never
    /// migrated. The workspace comes back so the caller paints the name the database now holds
    /// rather than the one it asked for.
    pub fn rename_terminal_session(
        &self,
        session_id: &str,
        name: &str,
    ) -> Result<WorkspaceState, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let renamed = connection
            .execute(
                "UPDATE sessions SET name = ?2 WHERE id = ?1",
                rusqlite::params![session_id, name],
            )
            .map_err(db_error)?;
        if renamed == 0 {
            return Err("session does not exist".into());
        }
        drop(connection);
        self.load_workspace()
    }

    /// Moves a terminal to another registered, non-missing checkout.
    ///
    /// The process is not touched: a session is a row, and moving it changes only the checkout that
    /// owns it. Both layouts are reconciled in the same transaction as the row, because a layout
    /// still naming a session its checkout no longer holds is a layout the next read prunes and the
    /// next save refuses.
    ///
    /// `select_target` says whether the window should follow. A person dragging a row to another
    /// worktree wants to be there; a session that moved on its own does not get to decide what the
    /// window is looking at, so the caller that noticed the move passes false and leaves the
    /// selection alone.
    pub fn move_terminal_session(
        &self,
        session_id: &str,
        target_checkout_id: &str,
        select_target: bool,
    ) -> Result<WorkspaceState, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let transaction = connection.unchecked_transaction().map_err(db_error)?;
        let source_checkout_id: String = transaction
            .query_row(
                "SELECT checkout_id FROM sessions WHERE id = ?1",
                [session_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| "session does not exist".to_string())?;
        if source_checkout_id == target_checkout_id {
            return Err("the terminal session is already in that worktree".into());
        }
        let _target_repo_id: String = transaction
            .query_row(
                "SELECT repo_id FROM checkouts
                 WHERE id = ?1 AND is_missing = 0 AND is_archived = 0",
                [target_checkout_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| "checkout does not exist or is missing".to_string())?;
        transaction
            .execute(
                "UPDATE sessions SET checkout_id = ?1 WHERE id = ?2",
                params![target_checkout_id, session_id],
            )
            .map_err(db_error)?;
        reconcile_stored_layout(&transaction, &source_checkout_id)?;
        reconcile_stored_layout(&transaction, target_checkout_id)?;
        if select_target {
            // The worktree the session now belongs to is the one whose files, changes and agent the
            // window shows, so the move selects it and the session inside it.
            set_preference(&transaction, ACTIVE_CHECKOUT, Some(target_checkout_id))?;
            set_preference(&transaction, ACTIVE_SESSION, Some(session_id))?;
        }
        transaction.commit().map_err(db_error)?;
        drop(connection);
        self.load_workspace()
    }

    /// Load one registered checkout without hydrating unrelated repositories or preferences.
    /// Load one registered checkout and its repository without hydrating unrelated repositories.
    pub fn load_registered_checkout(
        &self,
        checkout_id: &str,
    ) -> Result<Option<(Repo, Checkout)>, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        // Same two halves as `load_workspace`, and for the same reason: the folder check is a
        // `stat` on the user's disk, and it does not belong inside the global critical section.
        let mut workspace = load_workspace_for_checkout(&connection, checkout_id)?;
        drop(connection);
        mark_missing_checkouts(&mut workspace);
        let Some(repo) = workspace.repos.into_iter().next() else {
            return Ok(None);
        };
        let Some(checkout) = repo
            .checkouts
            .iter()
            .find(|checkout| checkout.id == checkout_id)
            .cloned()
        else {
            return Ok(None);
        };
        Ok(Some((repo, checkout)))
    }

    pub fn load_workspace(&self) -> Result<WorkspaceState, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        // The rows are the database's to answer and the lock is released before the folders are
        // asked about. `is_dir` is a `stat`, and a `stat` on a network mount, a spinning disk or a
        // volume that is on its way out can take as long as the kernel feels like — none of which
        // is the connection's business, and every unrelated read, write and bookkeeping call in
        // the app is queued behind this one mutex. Checking the folders first would mean one slow
        // path stalls the whole workspace; checking them second also means the answer is fresher.
        let mut state = load_workspace(&connection)?;
        drop(connection);
        mark_missing_checkouts(&mut state);
        state.home_checkout_id = self
            .home_checkout_id
            .lock()
            .map_err(|error| error.to_string())?
            .clone();
        Ok(state)
    }

    /// The folders opened before, newest first, inside the ten the writes keep. The stamps are
    /// ISO-8601, so ordering them as text is ordering them in time.
    pub fn list_recent_paths(&self) -> Result<Vec<RecentPath>, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT canonical_path, last_opened_at FROM recent_paths
                 ORDER BY last_opened_at DESC LIMIT 10",
            )
            .map_err(db_error)?;
        let paths = statement
            .query_map([], |row| {
                Ok(RecentPath {
                    canonical_path: row.get(0)?,
                    last_opened_at: row.get(1)?,
                })
            })
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        Ok(paths)
    }

    pub fn load_terminal_layout(
        &self,
        checkout_id: &str,
    ) -> Result<Option<CheckoutTerminalLayout>, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let transaction = connection.unchecked_transaction().map_err(db_error)?;
        let session_ids = terminal_layout_session_ids(&transaction, checkout_id)?;
        let serialized = transaction
            .query_row(
                "SELECT layout_json FROM checkout_terminal_layouts WHERE checkout_id = ?1",
                [checkout_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(db_error)?;
        let Some(serialized) = serialized else {
            transaction.commit().map_err(db_error)?;
            return Ok(None);
        };
        let mut layout: CheckoutTerminalLayout = serde_json::from_str(&serialized)
            .map_err(|error| format!("saved terminal layout is invalid: {error}"))?;
        layout.reconcile_sessions(&session_ids);
        validate_terminal_layout(&transaction, checkout_id, &layout)?;
        let normalized = serde_json::to_string(&layout).map_err(|error| error.to_string())?;
        if normalized != serialized {
            transaction
                .execute(
                    "UPDATE checkout_terminal_layouts SET layout_json = ?1 WHERE checkout_id = ?2",
                    params![normalized, checkout_id],
                )
                .map_err(db_error)?;
        }
        transaction.commit().map_err(db_error)?;
        Ok(Some(layout))
    }

    pub fn save_terminal_layout(
        &self,
        checkout_id: &str,
        layout: &CheckoutTerminalLayout,
    ) -> Result<(), String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let transaction = connection.unchecked_transaction().map_err(db_error)?;
        let session_ids = terminal_layout_session_ids(&transaction, checkout_id)?;
        let mut normalized = layout.clone();
        normalized.reconcile_sessions(&session_ids);
        validate_terminal_layout(&transaction, checkout_id, &normalized)?;
        let serialized = serde_json::to_string(&normalized).map_err(|error| error.to_string())?;
        transaction
            .execute(
                "INSERT INTO checkout_terminal_layouts (checkout_id, layout_json)
                 VALUES (?1, ?2)
                 ON CONFLICT(checkout_id) DO UPDATE SET layout_json = excluded.layout_json",
                params![checkout_id, serialized],
            )
            .map_err(db_error)?;
        transaction.commit().map_err(db_error)
    }

    pub fn viewed_files(&self, checkout_id: &str) -> Result<Vec<String>, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let mut statement = connection
            .prepare("SELECT path FROM viewed_files WHERE checkout_id = ?1 ORDER BY path")
            .map_err(db_error)?;
        let files = statement
            .query_map([checkout_id], |row| row.get::<_, String>(0))
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        Ok(files)
    }

    pub fn mark_file_viewed(&self, checkout_id: &str, path: &str) -> Result<(), String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        connection
            .execute(
                "INSERT INTO viewed_files (checkout_id, path, viewed_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(checkout_id, path) DO UPDATE SET viewed_at = excluded.viewed_at",
                params![checkout_id, path, timestamp()],
            )
            .map_err(db_error)?;
        Ok(())
    }

    pub fn review_notes(&self, checkout_id: &str) -> Result<Vec<ReviewNote>, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let mut statement = connection
            .prepare(&format!(
                "SELECT {REVIEW_NOTE_COLUMNS} FROM review_notes
                 WHERE checkout_id = ?1 ORDER BY path, line_start, created_at"
            ))
            .map_err(db_error)?;
        let notes = statement
            .query_map([checkout_id], read_review_note)
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        Ok(notes)
    }

    pub fn add_review_note(&self, note: &ReviewNote) -> Result<ReviewNote, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        connection
            .execute(
                "INSERT INTO review_notes (id, checkout_id, path, side, line_start, line_end, content, code, status, code_hash, outdated, round_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    note.id,
                    note.checkout_id,
                    note.path,
                    note.side,
                    note.line_start,
                    note.line_end,
                    note.content,
                    note.code,
                    note.status,
                    note.code_hash,
                    note.outdated,
                    note.round_id,
                    note.created_at,
                    note.updated_at,
                ],
            )
            .map_err(db_error)?;
        Ok(note.clone())
    }

    /// Updates a note only when it belongs to `checkout_id`; a foreign ID is rejected.
    pub fn update_review_note(
        &self,
        id: &str,
        checkout_id: &str,
        content: &str,
    ) -> Result<ReviewNote, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let updated_at = timestamp();
        let changed = connection
            .execute(
                "UPDATE review_notes SET content = ?1, status = 'draft', round_id = NULL, updated_at = ?2
                 WHERE id = ?3 AND checkout_id = ?4
                   AND (round_id IS NULL OR EXISTS (
                       SELECT 1 FROM review_rounds
                       WHERE id = review_notes.round_id
                         AND status NOT IN ('queued', 'dispatching', 'dispatched')
                   ))",
                params![content, updated_at, id, checkout_id],
            )
            .map_err(db_error)?;
        if changed == 0 {
            let owned: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM review_notes WHERE id = ?1 AND checkout_id = ?2)",
                    params![id, checkout_id],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            return Err(if owned {
                "review note belongs to an unfinished review round".into()
            } else {
                "review note does not belong to the requested checkout".into()
            });
        }
        self.read_review_note_by_id(&connection, id)
    }

    pub fn delete_review_note(&self, id: &str, checkout_id: &str) -> Result<(), String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let deleted = connection
            .execute(
                "DELETE FROM review_notes WHERE id = ?1 AND checkout_id = ?2
                 AND (round_id IS NULL OR EXISTS (
                     SELECT 1 FROM review_rounds
                     WHERE id = review_notes.round_id
                       AND status NOT IN ('queued', 'dispatching', 'dispatched')
                 ))",
                params![id, checkout_id],
            )
            .map_err(db_error)?;
        if deleted == 0 {
            let owned: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM review_notes WHERE id = ?1 AND checkout_id = ?2)",
                    params![id, checkout_id],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            return Err(if owned {
                "review note belongs to an unfinished review round".into()
            } else {
                "review note does not belong to the requested checkout".into()
            });
        }
        Ok(())
    }

    pub fn mark_review_notes_sent(
        &self,
        checkout_id: &str,
        ids: &[String],
    ) -> Result<usize, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let transaction = connection.unchecked_transaction().map_err(db_error)?;
        let updated_at = timestamp();
        let mut marked = 0;
        for id in ids {
            marked += transaction
                .execute(
                    "UPDATE review_notes SET status = 'sent', updated_at = ?1
                     WHERE id = ?2 AND checkout_id = ?3",
                    params![updated_at, id, checkout_id],
                )
                .map_err(db_error)?;
        }
        if marked != ids.len() {
            let _ = transaction.rollback();
            return Err("review note does not belong to the requested checkout".into());
        }
        transaction.commit().map_err(db_error)?;
        Ok(marked)
    }

    /// Stamps a single note as outdated. Never clears the mark: only the user may do that.
    pub fn mark_review_note_outdated(
        &self,
        id: &str,
        checkout_id: &str,
    ) -> Result<ReviewNote, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let changed = connection
            .execute(
                "UPDATE review_notes SET outdated = 1, updated_at = ?1
                 WHERE id = ?2 AND checkout_id = ?3",
                params![timestamp(), id, checkout_id],
            )
            .map_err(db_error)?;
        if changed == 0 {
            return Err("review note does not belong to the requested checkout".into());
        }
        self.read_review_note_by_id(&connection, id)
    }

    /// Clears the outdated mark, but only for a note that is actually marked.
    pub fn clear_review_note_outdated(
        &self,
        id: &str,
        checkout_id: &str,
    ) -> Result<ReviewNote, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let changed = connection
            .execute(
                "UPDATE review_notes SET outdated = 0, updated_at = ?1
                 WHERE id = ?2 AND checkout_id = ?3 AND outdated = 1",
                params![timestamp(), id, checkout_id],
            )
            .map_err(db_error)?;
        if changed == 0 {
            return Err("review note does not belong to the requested checkout".into());
        }
        self.read_review_note_by_id(&connection, id)
    }

    /// Marks a note resolved, and only for a note still in flight.
    pub fn resolve_review_note(&self, id: &str, checkout_id: &str) -> Result<ReviewNote, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let changed = connection
            .execute(
                "UPDATE review_notes SET status = 'resolved', updated_at = ?1
                 WHERE id = ?2 AND checkout_id = ?3 AND status = 'sent'",
                params![timestamp(), id, checkout_id],
            )
            .map_err(db_error)?;
        if changed == 0 {
            return Err("review note is not an outstanding note of this checkout".into());
        }
        self.read_review_note_by_id(&connection, id)
    }

    /// Records a round, and links its notes to it, in one transaction.
    ///
    /// `round.status` is written before the agent is called, so a crash between here and
    /// the send leaves a `dispatching` round that reconciliation can resolve.
    /// Stores a round with the exact message it will be sent with, if it has one yet.
    pub fn add_review_round(
        &self,
        round: &ReviewRound,
        note_ids: &[String],
        prompt: Option<&str>,
    ) -> Result<ReviewRound, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let transaction = connection.unchecked_transaction().map_err(db_error)?;
        let encoded = serde_json::to_string(note_ids)
            .map_err(|error| format!("could not encode notes: {error}"))?;
        transaction
            .execute(
                "INSERT INTO review_rounds (id, checkout_id, session_id, status, marker, note_ids, prompt, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    round.id,
                    round.checkout_id,
                    round.session_id,
                    round.status,
                    round.marker,
                    encoded,
                    prompt,
                    round.created_at,
                    round.updated_at,
                ],
            )
            .map_err(db_error)?;
        for id in note_ids {
            let linked = transaction
                .execute(
                    "UPDATE review_notes
                     SET status = 'sent', round_id = ?1, updated_at = ?2
                     WHERE id = ?3 AND checkout_id = ?4 AND status != 'resolved'
                       AND (round_id IS NULL OR EXISTS (
                           SELECT 1 FROM review_rounds
                           WHERE id = review_notes.round_id
                             AND status NOT IN ('queued', 'dispatching', 'dispatched')
                       ))",
                    params![round.id, round.updated_at, id, round.checkout_id],
                )
                .map_err(db_error)?;
            if linked == 0 {
                let status = transaction
                    .query_row(
                        "SELECT status FROM review_notes WHERE id = ?1 AND checkout_id = ?2",
                        params![id, round.checkout_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(db_error)?;
                let error = match status.as_deref() {
                    None => "review note does not belong to the requested checkout",
                    Some("resolved") => "review note is resolved",
                    Some(_) => "review note is claimed by an unfinished review round or has an invalid round link",
                };
                let _ = transaction.rollback();
                return Err(error.into());
            }
        }
        transaction.commit().map_err(db_error)?;
        Ok(round.clone())
    }

    /// The message a queued round is waiting to be sent with.
    ///
    /// Kept off [`ReviewRound`] on purpose: the prompt can be half a megabyte and the UI
    /// never asks for it, only the flush that is about to send it does.
    pub fn review_round_prompt(
        &self,
        id: &str,
        checkout_id: &str,
    ) -> Result<Option<String>, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        // A round of another checkout must read as "no message", not as an error: the caller
        // is asking what to send, and "not ours" is a plain answer to that.
        connection
            .query_row(
                "SELECT prompt FROM review_rounds WHERE id = ?1 AND checkout_id = ?2",
                params![id, checkout_id],
                |row| row.get::<_, Option<String>>(0),
            )
            // `.optional()` turns "no such round" into `None`; the closure's own `Option`
            // is there for a round whose message column is NULL. The two must not stack.
            .optional()
            .map_err(db_error)
            .map(|stored| stored.flatten())
    }

    pub fn review_rounds(&self, checkout_id: &str) -> Result<Vec<ReviewRound>, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let rounds = {
            let mut statement = connection
                .prepare(&format!(
                    "SELECT {REVIEW_ROUND_COLUMNS} FROM review_rounds
                     WHERE checkout_id = ?1 ORDER BY created_at DESC, rowid DESC"
                ))
                .map_err(db_error)?;
            let collected = statement
                .query_map([checkout_id], read_review_round)
                .map_err(db_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(db_error)?;
            collected
        };
        Ok(rounds)
    }

    /// Advances a round's status, rejecting a move out of the state it is actually in.
    pub fn set_review_round_status(
        &self,
        id: &str,
        checkout_id: &str,
        from: &str,
        to: &str,
    ) -> Result<ReviewRound, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let changed = connection
            .execute(
                "UPDATE review_rounds SET status = ?1, updated_at = ?2
                 WHERE id = ?3 AND checkout_id = ?4 AND status = ?5",
                params![to, timestamp(), id, checkout_id, from],
            )
            .map_err(db_error)?;
        if changed == 0 {
            return Err("review round is not in the expected state".into());
        }
        connection
            .query_row(
                &format!("SELECT {REVIEW_ROUND_COLUMNS} FROM review_rounds WHERE id = ?1"),
                [id],
                read_review_round,
            )
            .map_err(db_error)
    }

    /// Puts every interrupted round of the checkout back to `queued`.
    pub fn requeue_review_rounds(&self, checkout_id: &str) -> Result<usize, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        connection
            .execute(
                "UPDATE review_rounds SET status = 'queued', updated_at = ?1
                 WHERE checkout_id = ?2 AND status = 'dispatching'",
                params![timestamp(), checkout_id],
            )
            .map_err(db_error)
    }

    fn read_review_note_by_id(
        &self,
        connection: &Connection,
        id: &str,
    ) -> Result<ReviewNote, String> {
        connection
            .query_row(
                &format!("SELECT {REVIEW_NOTE_COLUMNS} FROM review_notes WHERE id = ?1"),
                [id],
                read_review_note,
            )
            .map_err(db_error)
    }

    /// Anchor hashes of the requested notes, so the caller can compare and stamp drift.
    pub fn review_note_hashes(
        &self,
        checkout_id: &str,
        path: &str,
        ids: &[String],
    ) -> Result<Vec<(String, String)>, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT id, code_hash FROM review_notes
                 WHERE checkout_id = ?1 AND path = ?2 AND id = ?3",
            )
            .map_err(db_error)?;
        let mut found = Vec::with_capacity(ids.len());
        for id in ids {
            let row = statement
                .query_row(params![checkout_id, path, id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .optional()
                .map_err(db_error)?;
            match row {
                Some((id, code_hash)) => found.push((id, code_hash)),
                None => return Err("review note does not belong to the requested checkout".into()),
            }
        }
        Ok(found)
    }
}

fn read_review_note(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReviewNote> {
    Ok(ReviewNote {
        id: row.get(0)?,
        checkout_id: row.get(1)?,
        path: row.get(2)?,
        side: row.get(3)?,
        line_start: row.get(4)?,
        line_end: row.get(5)?,
        content: row.get(6)?,
        code: row.get(7)?,
        status: row.get(8)?,
        code_hash: row.get(9)?,
        outdated: row.get(10)?,
        round_id: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}

fn read_review_round(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReviewRound> {
    let note_ids: String = row.get(5)?;
    Ok(ReviewRound {
        id: row.get(0)?,
        checkout_id: row.get(1)?,
        session_id: row.get(2)?,
        status: row.get(3)?,
        marker: row.get(4)?,
        note_ids: serde_json::from_str(&note_ids).unwrap_or_default(),
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn terminal_layout_session_ids(
    connection: &Connection,
    checkout_id: &str,
) -> Result<Vec<String>, String> {
    let checkout_exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM checkouts WHERE id = ?1)",
            [checkout_id],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    if !checkout_exists {
        return Err("checkout does not exist".into());
    }
    let mut statement = connection
        .prepare("SELECT id FROM sessions WHERE checkout_id = ?1 ORDER BY rowid")
        .map_err(db_error)?;
    let session_ids = statement
        .query_map([checkout_id], |row| row.get::<_, String>(0))
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    Ok(session_ids)
}

fn transfer_checkout_metadata(
    transaction: &Transaction<'_>,
    previous_checkout_id: &str,
    next_checkout_id: &str,
) -> Result<(), String> {
    let conflicting_reference: bool = transaction
        .query_row(
            "SELECT EXISTS (
                 SELECT 1 FROM review_notes note
                 LEFT JOIN review_rounds round ON round.id = note.round_id
                 WHERE note.checkout_id = ?1 AND note.round_id IS NOT NULL
                   AND (round.id IS NULL OR round.checkout_id != ?1)
             ) OR EXISTS (
                 SELECT 1 FROM review_notes note
                 JOIN review_rounds round ON round.id = note.round_id
                 WHERE note.checkout_id = ?2 AND round.checkout_id = ?1
             ) OR EXISTS (
                 SELECT 1 FROM review_rounds moved_round
                 CROSS JOIN json_each(moved_round.note_ids) listed_note
                 JOIN review_notes existing_note ON existing_note.id = listed_note.value
                 WHERE moved_round.checkout_id = ?1 AND existing_note.checkout_id = ?2
             ) OR EXISTS (
                 SELECT 1 FROM review_rounds existing_round
                 CROSS JOIN json_each(existing_round.note_ids) listed_note
                 JOIN review_notes moved_note ON moved_note.id = listed_note.value
                 WHERE existing_round.checkout_id = ?2 AND moved_note.checkout_id = ?1
             )",
            params![previous_checkout_id, next_checkout_id],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    if conflicting_reference {
        return Err("review notes and rounds have conflicting checkout ownership".into());
    }
    transaction
        .execute(
            "UPDATE sessions SET checkout_id = ?1 WHERE checkout_id = ?2",
            params![next_checkout_id, previous_checkout_id],
        )
        .map_err(db_error)?;
    transaction
        .execute(
            "UPDATE checkout_terminal_layouts SET checkout_id = ?1 WHERE checkout_id = ?2",
            params![next_checkout_id, previous_checkout_id],
        )
        .map_err(db_error)?;
    transaction
        .execute(
            "UPDATE viewed_files SET checkout_id = ?1 WHERE checkout_id = ?2",
            params![next_checkout_id, previous_checkout_id],
        )
        .map_err(db_error)?;
    transaction
        .execute(
            "UPDATE review_rounds
             SET checkout_id = ?1,
                 status = CASE WHEN status IN ('queued', 'dispatching') THEN 'relocated' ELSE status END,
                 updated_at = CASE WHEN status IN ('queued', 'dispatching') THEN ?3 ELSE updated_at END
             WHERE checkout_id = ?2",
            params![next_checkout_id, previous_checkout_id, timestamp()],
        )
        .map_err(db_error)?;
    transaction
        .execute(
            "UPDATE review_notes SET checkout_id = ?1 WHERE checkout_id = ?2",
            params![next_checkout_id, previous_checkout_id],
        )
        .map_err(db_error)?;
    let checkout_ui_state = transaction
        .query_row(
            "SELECT state_json FROM checkout_ui_states WHERE checkout_id = ?1",
            [previous_checkout_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(db_error)?;
    if let Some(serialized) = checkout_ui_state {
        let mut state = serde_json::from_str::<CheckoutUiState>(&serialized).ok();
        if let Some(state) = state
            .as_mut()
            .filter(|state| validate_checkout_ui_state(previous_checkout_id, state))
        {
            if let Some(document) = state.document.as_mut() {
                document.checkout_id = next_checkout_id.to_string();
            }
            let serialized = serde_json::to_string(state).map_err(|error| error.to_string())?;
            transaction
                .execute(
                    "UPDATE checkout_ui_states SET checkout_id = ?1, state_json = ?2 WHERE checkout_id = ?3",
                    params![next_checkout_id, serialized, previous_checkout_id],
                )
                .map_err(db_error)?;
        } else {
            transaction
                .execute(
                    "DELETE FROM checkout_ui_states WHERE checkout_id = ?1",
                    [previous_checkout_id],
                )
                .map_err(db_error)?;
        }
    }
    if get_preference(transaction, ACTIVE_CHECKOUT)?.as_deref() == Some(previous_checkout_id) {
        set_preference(transaction, ACTIVE_CHECKOUT, Some(next_checkout_id))?;
    }
    Ok(())
}

fn validate_terminal_layout(
    connection: &Connection,
    checkout_id: &str,
    layout: &CheckoutTerminalLayout,
) -> Result<(), String> {
    let session_ids = terminal_layout_session_ids(connection, checkout_id)?;
    let session_ids = session_ids.into_iter().collect();
    layout.validate(&session_ids)
}

/// Brings one checkout's stored layout back in line with the sessions it holds now.
///
/// A checkout with no stored layout needs nothing here: the next read builds one from its
/// sessions, which is the same shape this would have written.
fn reconcile_stored_layout(transaction: &Transaction<'_>, checkout_id: &str) -> Result<(), String> {
    let Some(serialized) = transaction
        .query_row(
            "SELECT layout_json FROM checkout_terminal_layouts WHERE checkout_id = ?1",
            [checkout_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(db_error)?
    else {
        return Ok(());
    };
    let mut layout: CheckoutTerminalLayout = serde_json::from_str(&serialized)
        .map_err(|error| format!("saved terminal layout is invalid: {error}"))?;
    layout.reconcile_sessions(&terminal_layout_session_ids(transaction, checkout_id)?);
    let reconciled = serde_json::to_string(&layout).map_err(|error| error.to_string())?;
    if reconciled == serialized {
        return Ok(());
    }
    transaction
        .execute(
            "UPDATE checkout_terminal_layouts SET layout_json = ?1 WHERE checkout_id = ?2",
            params![reconciled, checkout_id],
        )
        .map_err(db_error)?;
    Ok(())
}

fn safe_checkout_relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn safe_review_relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && path.ends_with(".md")
        && !path.contains('\\')
        && !path.contains('\0')
        && Path::new(path)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

/// What a saved state has to hold for this build to hand it back.
///
/// Every field is required, so a row written before one of them existed does not parse and is
/// discarded whole rather than half honoured. There is no version here to compare: a field that
/// arrives is a field this build knows what to mean.
fn validate_checkout_ui_state(checkout_id: &str, state: &CheckoutUiState) -> bool {
    matches!(state.main_view.as_str(), "terminal" | "document")
        && matches!(state.inspector_tab.as_str(), "files" | "changes")
        && state.document.as_ref().is_none_or(|document| {
            document.checkout_id == checkout_id
                && matches!(
                    document.origin.as_str(),
                    "checkout" | "review-default" | "review-workdir"
                )
                && if document.origin == "checkout" {
                    safe_checkout_relative_path(&document.path)
                } else {
                    safe_review_relative_path(&document.path)
                }
                && matches!(document.source.as_str(), "file" | "change")
                && matches!(document.mode.as_str(), "diff" | "view" | "code")
                && (document.source != "file" || document.mode != "diff")
                && (document.origin == "checkout"
                    || (document.source == "file" && document.mode != "diff"))
        })
        && (state.main_view != "document" || state.document.is_some())
        // The whole-change-set diff names no file, so a document alongside it is contradictory.
        && (!state.diff_all_files || (state.main_view == "terminal" && state.document.is_none()))
        && state
            .selected_file_path
            .as_ref()
            .is_none_or(|path| safe_checkout_relative_path(path))
        && state
            .selected_change_path
            .as_ref()
            .is_none_or(|path| safe_checkout_relative_path(path))
        && state.expanded_directories.len() <= 2000
        && state
            .expanded_directories
            .iter()
            .all(|path| safe_checkout_relative_path(path))
        && state.files_scroll_top <= 10_000_000
        && state.changes_scroll_top <= 10_000_000
        && state.document_scroll_top <= 10_000_000
        && state.document_scroll_left <= 10_000_000
        && state.diff_scroll_top <= 10_000_000
}

/// Writes the whole schema into a file that has none and stamps it with `SCHEMA_VERSION`.
///
/// There is no ladder: a file stamped with this build's version is left untouched, a file
/// with no stamp is written from scratch, and a file stamped with anything else is refused
/// rather than upgraded. Refusing is the point. The alternative was to reshape a database
/// this build did not write, and losing the repos someone registered is worse than asking
/// them to delete a file they were told to delete.
fn create_schema(connection: &Connection) -> Result<(), String> {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(db_error)?;
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    if version != 0 {
        return Err(format!(
            "the workspace database is schema version {version} and this build speaks version \
             {SCHEMA_VERSION}: update by deleting {} and opening the app again, which starts an \
             empty workspace",
            connection.path().unwrap_or("the workspace database"),
        ));
    }

    let transaction = connection.unchecked_transaction().map_err(db_error)?;
    transaction
        .execute_batch(
            "CREATE TABLE repos (
                id TEXT PRIMARY KEY NOT NULL,
                kind TEXT NOT NULL CHECK (kind IN ('git', 'plain')),
                name TEXT NOT NULL,
                root TEXT NOT NULL,
                default_branch TEXT,
                position INTEGER NOT NULL UNIQUE,
                created_at TEXT NOT NULL,
                last_opened_at TEXT NOT NULL
            );
            CREATE TABLE checkouts (
                id TEXT PRIMARY KEY NOT NULL,
                repo_id TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
                path TEXT NOT NULL,
                canonical_path TEXT NOT NULL UNIQUE,
                is_primary INTEGER NOT NULL CHECK (is_primary IN (0, 1)),
                branch TEXT,
                head TEXT,
                ahead_of_default INTEGER,
                changed_files INTEGER NOT NULL DEFAULT 0,
                is_missing INTEGER NOT NULL DEFAULT 0,
                is_archived INTEGER NOT NULL DEFAULT 0 CHECK (is_archived IN (0, 1)),
                position INTEGER NOT NULL,
                UNIQUE (repo_id, position)
            );
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY NOT NULL,
                checkout_id TEXT NOT NULL REFERENCES checkouts(id) ON DELETE CASCADE,
                session_type TEXT NOT NULL CHECK (session_type IN ('shell', 'agent')),
                name TEXT NOT NULL,
                created_at TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'inactive' CHECK (status IN ('active', 'inactive'))
            );
            CREATE TABLE recent_paths (
                canonical_path TEXT PRIMARY KEY NOT NULL,
                last_opened_at TEXT NOT NULL
            );
            CREATE TABLE preferences (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );
            CREATE TABLE checkout_terminal_layouts (
                checkout_id TEXT PRIMARY KEY NOT NULL REFERENCES checkouts(id) ON DELETE CASCADE,
                layout_json TEXT NOT NULL
            );
            CREATE TABLE viewed_files (
                checkout_id TEXT NOT NULL REFERENCES checkouts(id) ON DELETE CASCADE,
                path TEXT NOT NULL,
                viewed_at TEXT NOT NULL,
                PRIMARY KEY (checkout_id, path)
            );
            CREATE TABLE checkout_ui_states (
                checkout_id TEXT PRIMARY KEY NOT NULL REFERENCES checkouts(id) ON DELETE CASCADE,
                state_json TEXT NOT NULL
            );
            CREATE TABLE review_notes (
                id TEXT PRIMARY KEY NOT NULL,
                checkout_id TEXT NOT NULL REFERENCES checkouts(id) ON DELETE CASCADE,
                path TEXT NOT NULL,
                side TEXT NOT NULL CHECK (side IN ('old', 'new')),
                line_start INTEGER NOT NULL,
                line_end INTEGER,
                content TEXT NOT NULL,
                code TEXT NOT NULL,
                status TEXT NOT NULL CHECK (status IN ('draft', 'sent', 'resolved')),
                code_hash TEXT NOT NULL,
                outdated INTEGER NOT NULL DEFAULT 0 CHECK (outdated IN (0, 1)),
                round_id TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX review_notes_checkout_path ON review_notes (checkout_id, path);
            CREATE INDEX review_notes_round ON review_notes (round_id);
            CREATE TABLE review_rounds (
                id TEXT PRIMARY KEY NOT NULL,
                checkout_id TEXT NOT NULL REFERENCES checkouts(id) ON DELETE CASCADE,
                session_id TEXT NOT NULL,
                status TEXT NOT NULL CHECK (
                    status IN ('queued', 'dispatching', 'dispatched', 'acked', 'relocated')
                ),
                marker TEXT NOT NULL,
                note_ids TEXT NOT NULL,
                prompt TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX review_rounds_checkout ON review_rounds (checkout_id, created_at);",
        )
        .map_err(db_error)?;
    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(db_error)?;
    transaction.commit().map_err(db_error)
}

fn load_workspace(connection: &Connection) -> Result<WorkspaceState, String> {
    load_workspace_filtered(connection, None)
}

fn load_workspace_for_checkout(
    connection: &Connection,
    checkout_id: &str,
) -> Result<WorkspaceState, String> {
    load_workspace_filtered(connection, Some(checkout_id))
}

// Full loads use four set queries; a scoped lookup uses the same three without preferences.
// Rows come back carrying the stored `is_missing` only: whether a folder is still on disk is
// the filesystem's answer, and it is `mark_missing_checkouts`' to give once the caller has
// dropped the connection.
fn load_workspace_filtered(
    connection: &Connection,
    checkout_id: Option<&str>,
) -> Result<WorkspaceState, String> {
    let mut repos_statement = connection
        .prepare(
            "SELECT id, kind, name, root, default_branch, created_at, last_opened_at
             FROM repos
             WHERE ?1 IS NULL OR id = (
                 SELECT repo_id FROM checkouts WHERE id = ?1 AND is_archived = 0
             )
             ORDER BY position",
        )
        .map_err(db_error)?;
    let repo_rows = repos_statement
        .query_map([checkout_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(db_error)?;
    let repo_rows = repo_rows.collect::<Result<Vec<_>, _>>().map_err(db_error)?;
    drop(repos_statement);

    let mut checkouts_statement = connection
        .prepare(
            "SELECT repo_id, id, path, canonical_path, is_primary, branch, head, ahead_of_default,
                    changed_files, is_missing, is_archived
             FROM checkouts
             WHERE ?1 IS NULL OR repo_id = (
                 SELECT repo_id FROM checkouts WHERE id = ?1 AND is_archived = 0
             )
             ORDER BY repo_id, position",
        )
        .map_err(db_error)?;
    let checkout_rows = checkouts_statement
        .query_map([checkout_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, bool>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<u32>>(7)?,
                    row.get::<_, u32>(8)?,
                    row.get::<_, bool>(9)?,
                    row.get::<_, bool>(10)?,
                ),
            ))
        })
        .map_err(db_error)?;
    let mut checkouts_by_repo = HashMap::new();
    for row in checkout_rows {
        let (repo_id, checkout) = row.map_err(db_error)?;
        checkouts_by_repo
            .entry(repo_id)
            .or_insert_with(Vec::new)
            .push(checkout);
    }
    drop(checkouts_statement);

    let mut sessions_statement = connection
        .prepare(
            "SELECT s.checkout_id, s.id, s.session_type, s.name, s.created_at, s.status
             FROM sessions s JOIN checkouts c ON c.id = s.checkout_id
             WHERE c.is_archived = 0 AND (?1 IS NULL OR c.repo_id = (
                 SELECT repo_id FROM checkouts WHERE id = ?1 AND is_archived = 0
             ))
             ORDER BY c.repo_id, c.position, s.rowid",
        )
        .map_err(db_error)?;
    let session_rows = sessions_statement
        .query_map([checkout_id], session_from_row)
        .map_err(db_error)?;
    let mut sessions_by_checkout = HashMap::new();
    for row in session_rows {
        let (checkout_id, session) = row.map_err(db_error)?;
        sessions_by_checkout
            .entry(checkout_id)
            .or_insert_with(Vec::new)
            .push(session);
    }
    drop(sessions_statement);

    let mut repos = Vec::with_capacity(repo_rows.len());
    let mut archived_worktrees = Vec::new();
    for (id, kind, name, root, default_branch, created_at, last_opened_at) in repo_rows {
        let checkout_rows = checkouts_by_repo.remove(&id).unwrap_or_default();
        let mut checkouts = Vec::with_capacity(checkout_rows.len());
        for (
            checkout_id,
            path,
            canonical_path,
            is_primary,
            branch,
            head,
            ahead_of_default,
            changed_files,
            is_missing,
            is_archived,
        ) in checkout_rows
        {
            // An archived worktree is registered and alive; it is off the panel, not gone.
            // The two lists are one pass over one set of rows, so the panel and the list
            // of what could come back to it can never disagree.
            if is_archived {
                archived_worktrees.push(ArchivedCheckout {
                    id: checkout_id,
                    repo_id: id.clone(),
                    path,
                    branch,
                });
                continue;
            }
            let sessions = sessions_by_checkout
                .remove(&checkout_id)
                .unwrap_or_default();
            checkouts.push(Checkout {
                id: checkout_id,
                repo_id: id.clone(),
                is_missing,
                path,
                canonical_path,
                is_primary,
                branch,
                head,
                ahead_of_default,
                changed_files,
                sessions,
            });
        }

        repos.push(Repo {
            id,
            kind: parse_repo_kind(&kind)?,
            name,
            root,
            default_branch,
            checkouts,
            created_at,
            last_opened_at,
        });
    }

    let (active_checkout_id, active_session_id) = if checkout_id.is_some() {
        (None, None)
    } else {
        let mut statement = connection
            .prepare("SELECT key, value FROM preferences WHERE key IN (?1, ?2)")
            .map_err(db_error)?;
        let rows = statement
            .query_map([ACTIVE_CHECKOUT, ACTIVE_SESSION], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(db_error)?;
        let (mut active_checkout_id, mut active_session_id) = (None, None);
        for row in rows {
            let (key, value) = row.map_err(db_error)?;
            match key.as_str() {
                ACTIVE_CHECKOUT => active_checkout_id = Some(value),
                ACTIVE_SESSION => active_session_id = Some(value),
                _ => unreachable!(),
            }
        }
        (active_checkout_id, active_session_id)
    };

    Ok(WorkspaceState {
        repos,
        home_checkout_id: None,
        archived_worktrees,
        active_checkout_id,
        active_session_id,
    })
}

/// Fold the filesystem into a workspace that has already been read, marking as missing every
/// checkout whose folder is gone. A row stored as missing stays missing even if its folder is
/// back: that flag is the record of a worktree the user was told about, and a checkout that was
/// missing once is still their decision to make, not a silent guess. Archived worktrees never
/// reach here — they are not on the panel and are not statted while the panel is being built.
fn mark_missing_checkouts(state: &mut WorkspaceState) {
    for repo in &mut state.repos {
        for checkout in &mut repo.checkouts {
            checkout.is_missing =
                checkout.is_missing || !PathBuf::from(&checkout.canonical_path).is_dir();
        }
    }
}

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, Session)> {
    let checkout_id: String = row.get(0)?;
    let session_type: String = row.get(2)?;
    let status: String = row.get(5)?;
    Ok((
        checkout_id.clone(),
        Session {
            id: row.get(1)?,
            session_type: parse_session_type(&session_type).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::other(error)),
                )
            })?,
            checkout_id,
            name: row.get(3)?,
            created_at: row.get(4)?,
            status: match status.as_str() {
                "active" => SessionStatus::Active,
                "inactive" => SessionStatus::Inactive,
                _ => return Err(rusqlite::Error::InvalidQuery),
            },
        },
    ))
}

fn parse_repo_kind(kind: &str) -> Result<RepoKind, String> {
    match kind {
        "git" => Ok(RepoKind::Git),
        "plain" => Ok(RepoKind::Plain),
        _ => Err(format!("unknown repository kind: {kind}")),
    }
}

fn parse_session_type(session_type: &str) -> Result<SessionType, String> {
    match session_type {
        "shell" => Ok(SessionType::Shell),
        "agent" => Ok(SessionType::Agent),
        _ => Err(format!("unknown session type: {session_type}")),
    }
}

fn session_type_name(session_type: &SessionType) -> &'static str {
    match session_type {
        SessionType::Shell => "shell",
        SessionType::Agent => "agent",
    }
}

fn get_preference(connection: &Connection, key: &str) -> Result<Option<String>, String> {
    connection
        .query_row(
            "SELECT value FROM preferences WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)
}

fn set_preference(
    transaction: &Transaction<'_>,
    key: &str,
    value: Option<&str>,
) -> Result<(), String> {
    if let Some(value) = value {
        transaction
            .execute(
                "INSERT INTO preferences (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map_err(db_error)?;
    } else {
        transaction
            .execute("DELETE FROM preferences WHERE key = ?1", [key])
            .map_err(db_error)?;
    }
    Ok(())
}

pub fn timestamp() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    millis.to_string()
}

fn git_repo_registration_snapshot_in(
    connection: &Connection,
    repo_id: &str,
) -> Result<Option<GitRepoRegistrationSnapshot>, String> {
    let Some((repo_id, kind, name, root, default_branch, last_opened_at)) = connection
        .query_row(
            "SELECT id, kind, name, root, default_branch, last_opened_at
             FROM repos WHERE id = ?1",
            [repo_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()
        .map_err(db_error)?
    else {
        return Ok(None);
    };
    if kind != "git" {
        return Ok(None);
    }
    let mut statement = connection
        .prepare(
            "SELECT id, path, canonical_path, is_primary, branch, head,
                    ahead_of_default, position, is_missing, is_archived
             FROM checkouts WHERE repo_id = ?1 ORDER BY id",
        )
        .map_err(db_error)?;
    let checkouts = statement
        .query_map([&repo_id], |row| {
            Ok(GitCheckoutRegistrationSnapshot {
                id: row.get(0)?,
                path: row.get(1)?,
                canonical_path: row.get(2)?,
                is_primary: row.get(3)?,
                branch: row.get(4)?,
                head: row.get(5)?,
                ahead_of_default: row.get(6)?,
                position: row.get(7)?,
                is_missing: row.get(8)?,
                is_archived: row.get(9)?,
            })
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    Ok(Some(GitRepoRegistrationSnapshot {
        repo_id,
        kind,
        name,
        root,
        default_branch,
        last_opened_at,
        checkouts,
    }))
}

fn db_error(error: rusqlite::Error) -> String {
    format!("SQLite operation failed: {error}")
}

/// Where a repo that is already on the panel keeps its place among the others, or the end of the
/// line when it is new.
const POSITION_OF_REGISTERED_REPO: &str =
    "SELECT COALESCE((SELECT position FROM repos WHERE id = ?1), MAX(position) + 1, 0) FROM repos";
/// The end of the line for a repo this registration does not name.
const MAX_POSITION_FROM_REPOS: &str = "SELECT COALESCE(MAX(position) + 1, 0) FROM repos";
/// The largest place a checkout of one repo holds, or -1 when it holds none.
const HIGHEST_CHECKOUT_POSITION: &str =
    "SELECT COALESCE(MAX(position), -1) FROM checkouts WHERE repo_id = ?1";

/// Reads where the repo `repo_id` stands among the others, or the end of the line when it is new.
fn read_repo_position(
    transaction: &rusqlite::Transaction<'_>,
    repo_id: &str,
) -> Result<i64, String> {
    read_position(transaction, POSITION_OF_REGISTERED_REPO, params![repo_id])
}

/// Reads a position the way SQLite is able to store it.
///
/// `INTEGER` is a type of affinity, not a promise about what a row holds: a value that no 64-bit
/// integer can represent is written as a REAL, and reading it back as `i64` is an error rather
/// than a number. A column with one such row in it then answers `MAX()` as a REAL, and the failure
/// names a type, not the row that caused it, several queries away from the worktree that was
/// removed. Ordering is all this value is for, so a position that is not one is the end of the
/// line: nothing is lost, and the next write puts the row on a number that fits.
fn read_position<P: rusqlite::Params>(
    transaction: &rusqlite::Transaction<'_>,
    query: &str,
    parameters: P,
) -> Result<i64, String> {
    let value: rusqlite::types::Value = transaction
        .query_row(query, parameters, |row| row.get(0))
        .map_err(db_error)?;
    // A whole number that SQLite happened to store as a REAL is still a position, and reading it
    // is what keeps an older file working. A fractional one is not, and the end of the line is
    // where a value nothing can order correctly belongs.
    let position = match value {
        rusqlite::types::Value::Integer(position) => position,
        rusqlite::types::Value::Real(position) if position.fract() == 0.0 => position as i64,
        _ => i64::MAX,
    };
    Ok(position)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use rusqlite::{params, Connection};
    use tempfile::tempdir;

    use crate::domain::{
        review::review_anchor_hash,
        terminal_layout::{CheckoutTerminalLayout, TerminalLayoutNode, TerminalLayoutTab},
        workspace::{
            checkout_id_for_path, repo_id_for_path, Checkout, Repo, RepoKind, Session,
            SessionStatus, SessionType,
        },
    };

    use super::{
        load_workspace, mark_missing_checkouts, AppLayoutState, CheckoutUiState, Database,
        PersistedDocument, ReviewNote, ReviewRound, SCHEMA_VERSION, UI_LAYOUT,
    };

    fn plain_repo(path: &Path, now: &str) -> Repo {
        Repo::plain(path, now).expect("plain repo")
    }

    fn add_round(database: &Database, checkout_id: &str, id: &str, status: &str) -> ReviewRound {
        let note_id = format!("note:{id}");
        let code = "let answer = 42;";
        database
            .add_review_note(&ReviewNote {
                id: note_id.clone(),
                checkout_id: checkout_id.into(),
                path: "src/lib.rs".into(),
                side: "new".into(),
                line_start: 7,
                line_end: None,
                content: format!("note for {id}"),
                code: code.into(),
                status: "draft".into(),
                code_hash: review_anchor_hash(code),
                outdated: false,
                round_id: None,
                created_at: "now".into(),
                updated_at: "now".into(),
            })
            .unwrap();
        let round = ReviewRound {
            id: id.into(),
            checkout_id: checkout_id.into(),
            session_id: "session:historic".into(),
            status: status.into(),
            marker: format!("muster-review:{id}"),
            note_ids: vec![note_id],
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        database
            .add_review_round(&round, &round.note_ids, Some("saved review prompt"))
            .unwrap()
    }

    fn assert_moved_round(
        database: &Database,
        checkout_id: &str,
        id: &str,
        status: &str,
    ) -> ReviewRound {
        let round = database
            .review_rounds(checkout_id)
            .unwrap()
            .into_iter()
            .find(|round| round.id == id)
            .expect("round remains addressable in the destination checkout");
        let note_id = format!("note:{id}");
        assert_eq!(round.checkout_id, checkout_id);
        assert_eq!(round.status, status);
        assert_eq!(round.session_id, "session:historic");
        assert_eq!(round.note_ids.as_slice(), std::slice::from_ref(&note_id));
        assert_eq!(round.marker, format!("muster-review:{id}"));
        assert_eq!(
            database
                .review_round_prompt(id, checkout_id)
                .unwrap()
                .as_deref(),
            Some("saved review prompt")
        );
        let note = database
            .review_notes(checkout_id)
            .unwrap()
            .into_iter()
            .find(|note| note.id == note_id)
            .expect("round note remains addressable in the destination checkout");
        assert_eq!(note.checkout_id, checkout_id);
        assert_eq!(note.round_id.as_deref(), Some(id));
        assert_eq!(note.status, "sent");
        assert_eq!(note.content, format!("note for {id}"));
        assert_eq!(note.code, "let answer = 42;");
        round
    }

    #[test]
    fn persisted_session_parser_rejects_removed_session_types() {
        for session_type in ["nvim", "server", "custom"] {
            assert_eq!(
                super::parse_session_type(session_type).unwrap_err(),
                format!("unknown session type: {session_type}")
            );
        }
    }

    #[test]
    fn home_is_pinned_first_without_recent_activity_and_cannot_be_closed() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let other = temp.path().join("other");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&other).unwrap();
        let database = Database::open_in_memory().unwrap();
        let home_repo = plain_repo(&home, "home-opened");
        let home_checkout_id = home_repo.checkouts[0].id.clone();

        database
            .register_plain_repo(plain_repo(&other, "other-opened"))
            .unwrap();
        database.register_plain_repo(home_repo.clone()).unwrap();
        database.register_home_repo(&home_repo).unwrap();
        let first = database.load_workspace().unwrap();
        assert_eq!(first.repos[0].name, "Home");
        assert_eq!(first.repos[0].checkouts[0].id, home_checkout_id);
        assert_eq!(
            first.home_checkout_id.as_deref(),
            Some(home_checkout_id.as_str())
        );
        assert!(!database
            .list_recent_paths()
            .unwrap()
            .iter()
            .any(|recent| recent.canonical_path == home_repo.root));

        database
            .register_home_repo(&plain_repo(&home, "later"))
            .unwrap();
        let repeated = database.load_workspace().unwrap();
        assert_eq!(repeated.repos[0].last_opened_at, "home-opened");
        assert_eq!(
            repeated
                .repos
                .iter()
                .filter(|repo| repo.name == "Home")
                .count(),
            1
        );
        assert!(database
            .close_checkout(&home_checkout_id)
            .unwrap_err()
            .contains("Home"));
        assert!(database
            .close_missing_checkout(&home_checkout_id)
            .unwrap_err()
            .contains("Home"));
        assert!(database
            .archive_checkout(&home_checkout_id)
            .unwrap_err()
            .contains("Home"));
        assert!(database
            .remove_checkout(&home_repo.id, &home_checkout_id)
            .unwrap_err()
            .contains("Home"));
    }

    /// A file with no stamp gets the whole schema, a file already stamped with this build's
    /// version is left alone, and a file from any other version is refused by name.
    #[test]
    fn a_fresh_file_gets_schema_18_and_a_foreign_one_is_refused() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("workspace.sqlite3");
        let database = Database::open(&path).expect("fresh database");
        assert_eq!(SCHEMA_VERSION, 18);
        let checkout_path = temp.path().join("checkout");
        fs::create_dir(&checkout_path).unwrap();
        let repo = plain_repo(&checkout_path, "now");
        let checkout_id = repo.checkouts[0].id.clone();
        database.register_plain_repo(repo).unwrap();
        {
            let connection = database.connection.lock().unwrap();
            assert_eq!(
                connection
                    .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                    .unwrap(),
                SCHEMA_VERSION
            );
            let tables: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN
                     ('repos', 'checkouts', 'sessions', 'recent_paths', 'preferences',
                      'checkout_terminal_layouts', 'viewed_files', 'checkout_ui_states',
                      'review_notes', 'review_rounds')",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(tables, 10);
            for session_type in ["nvim", "server", "custom"] {
                let error = connection
                    .execute(
                        "INSERT INTO sessions (id, checkout_id, session_type, name, created_at)
                         VALUES (?1, ?2, ?3, 'legacy', 'now')",
                        params![format!("session:{session_type}"), checkout_id, session_type],
                    )
                    .unwrap_err();
                assert!(
                    error.to_string().contains("CHECK constraint failed"),
                    "{error}"
                );
            }
            // Reopening what this build already wrote has nothing left to do.
            super::create_schema(&connection).expect("a stamped file is left alone");
        }

        let foreign_path = temp.path().join("foreign.sqlite3");
        {
            let connection = Connection::open(&foreign_path).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE preserved (value TEXT NOT NULL);
                     INSERT INTO preserved VALUES ('keep');
                     PRAGMA user_version = 15;",
                )
                .unwrap();
        }
        let error = Database::open(&foreign_path)
            .err()
            .expect("a file from another schema version is refused");
        assert!(error.contains("schema version 15"), "{error}");
        assert!(error.contains("this build speaks version 18"), "{error}");
        assert!(error.contains("deleting"), "{error}");
        assert!(error.contains("empty workspace"), "{error}");

        let connection = Connection::open(&foreign_path).unwrap();
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            15
        );
        assert_eq!(
            connection
                .query_row("SELECT value FROM preserved", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "keep"
        );
    }

    /// A session outlives nothing: its PTY died with the process, so reopening drops the rows
    /// instead of letting one dead session per launch pile up per checkout.
    #[test]
    fn reopening_drops_the_terminal_sessions_the_previous_process_owned() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("workspace.sqlite3");
        let folder = temp.path().join("checkout");
        fs::create_dir(&folder).unwrap();
        let repo = plain_repo(&folder, "now");
        let checkout_id = repo.checkouts[0].id.clone();
        let session = Session {
            id: "session:dead".into(),
            session_type: SessionType::Shell,
            checkout_id: checkout_id.clone(),
            name: "zsh".into(),
            created_at: "now".into(),
            status: SessionStatus::Active,
        };
        {
            let database = Database::open(&path).unwrap();
            database.register_plain_repo(repo).unwrap();
            database.add_terminal_session(&session).unwrap();
            database
                .save_terminal_layout(
                    &checkout_id,
                    &CheckoutTerminalLayout {
                        active_tab_id: Some("tab:dead".into()),
                        tabs: vec![TerminalLayoutTab {
                            id: "tab:dead".into(),
                            root: TerminalLayoutNode::Session {
                                session_id: session.id.clone(),
                            },
                        }],
                        session_order: vec![session.id.clone()],
                    },
                )
                .unwrap();
            assert_eq!(
                database.load_workspace().unwrap().repos[0].checkouts[0]
                    .sessions
                    .len(),
                1
            );
        }

        let database = Database::open(&path).unwrap();
        let restored = database.load_workspace().unwrap();
        assert!(restored.repos[0].checkouts[0].sessions.is_empty());
        assert_eq!(restored.active_session_id, None);
        assert_eq!(database.load_terminal_layout(&checkout_id).unwrap(), None);
    }

    /// D2-04: three line comments across two files, still drafts after a restart.
    ///
    /// A restart reopens the file, so anything held only in memory would be lost here; the
    /// drafts are asserted from the reopened handle rather than the one that wrote them.
    #[test]
    fn review_drafts_across_two_files_survive_a_database_restart() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("workspace.sqlite3");
        let folder = temp.path().join("checkout");
        fs::create_dir(&folder).unwrap();

        let checkout_id = {
            let database = Database::open(&db_path).unwrap();
            let repo = plain_repo(&folder, "now");
            let checkout_id = repo.checkouts[0].id.clone();
            database.register_plain_repo(repo).unwrap();
            let now = "2026-09-26T00:00:00Z";
            // Two notes on one file and one on another: the batch spans files, not just lines.
            let notes = [("src/main.rs", 4), ("src/main.rs", 12), ("src/lib.rs", 7)];
            for (index, (path, line)) in notes.into_iter().enumerate() {
                let code = format!("let value{index} = {index};");
                database
                    .add_review_note(&ReviewNote {
                        id: format!("note:{index}"),
                        checkout_id: checkout_id.clone(),
                        path: path.into(),
                        side: "new".into(),
                        line_start: line,
                        line_end: None,
                        content: format!("draft {index}"),
                        code: code.clone(),
                        status: "draft".into(),
                        code_hash: review_anchor_hash(&code),
                        outdated: false,
                        round_id: None,
                        created_at: now.into(),
                        updated_at: now.into(),
                    })
                    .unwrap();
            }
            checkout_id
        };

        let reopened = Database::open(&db_path).expect("reopened database");
        let drafts = reopened
            .review_notes(&checkout_id)
            .expect("drafts after restart");
        assert_eq!(drafts.len(), 3, "{drafts:?}");
        assert!(drafts.iter().all(|note| note.status == "draft"));
        assert!(drafts.iter().all(|note| !note.outdated));
        assert_eq!(
            drafts
                .iter()
                .filter(|note| note.path == "src/main.rs")
                .count(),
            2
        );
        assert_eq!(
            drafts
                .iter()
                .filter(|note| note.path == "src/lib.rs")
                .count(),
            1
        );
        // The unbounded line is what still has to point at the right code after the restart.
        assert!(drafts
            .iter()
            .any(|note| note.line_start == 12 && note.line_end.is_none()));
        assert!(drafts
            .iter()
            .all(|note| !note.code_hash.is_empty() && !note.code.is_empty()));
    }

    #[test]
    fn ui_state_round_trips_and_discards_invalid_saved_data() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("ui-state.sqlite3");
        let folder = temp.path().join("checkout");
        fs::create_dir(&folder).unwrap();
        let repo = plain_repo(&folder, "now");
        let checkout_id = repo.checkouts[0].id.clone();
        let database = Database::open(&db_path).unwrap();
        database.register_plain_repo(repo).unwrap();

        let layout = AppLayoutState {
            mode: "split".into(),
            sidebar_width: 340,
            inspector_width: 420,
            preview_width: 720,
        };
        database.save_app_layout(&layout).unwrap();
        let state = CheckoutUiState {
            document: Some(PersistedDocument {
                checkout_id: checkout_id.clone(),
                path: "docs/guide.md".into(),
                origin: "checkout".into(),
                source: "file".into(),
                mode: "view".into(),
            }),
            main_view: "document".into(),
            inspector_tab: "changes".into(),
            selected_file_path: Some("docs/guide.md".into()),
            selected_change_path: Some("src/main.rs".into()),
            expanded_directories: vec!["docs".into(), "src".into()],
            files_scroll_top: 640,
            changes_scroll_top: 480,
            document_scroll_top: 320,
            document_scroll_left: 4,
            diff_scroll_top: 900,
            ..CheckoutUiState::default()
        };
        database
            .save_checkout_ui_state(&checkout_id, &state)
            .unwrap();
        assert_eq!(database.load_app_layout().unwrap(), layout);
        assert_eq!(
            database.load_checkout_ui_state(&checkout_id).unwrap(),
            state
        );

        {
            let connection = database.connection.lock().unwrap();
            connection
                .execute(
                    "UPDATE checkout_ui_states SET state_json = ?1 WHERE checkout_id = ?2",
                    params![r#"{"version":1,"document":{"checkoutId":"checkout:wrong","path":"../secret","source":"file","mode":"view"},"mainView":"document","inspectorTab":"files","selectedFilePath":null,"selectedChangePath":null,"expandedDirectories":[],"filesScrollTop":0,"documentScrollTop":0,"documentScrollLeft":0}"#, checkout_id],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE preferences SET value = ?1 WHERE key = ?2",
                    params![
                        r#"{"sidebarWidth":300,"inspectorWidth":320,"previewWidth":500}"#,
                        UI_LAYOUT
                    ],
                )
                .unwrap();
        }
        assert_eq!(
            database.load_checkout_ui_state(&checkout_id).unwrap(),
            CheckoutUiState::default()
        );
        assert_eq!(
            database.load_app_layout().unwrap(),
            AppLayoutState::default()
        );
        let connection = database.connection.lock().unwrap();
        let invalid_checkout_state_remains: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM checkout_ui_states WHERE checkout_id = ?1)",
                [&checkout_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!invalid_checkout_state_remains);
        let invalid_global_state_remains: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM preferences WHERE key = 'ui_layout_v1')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!invalid_global_state_remains);
    }

    #[test]
    fn a_review_document_survives_a_restart_with_its_origin() {
        let temp = tempdir().unwrap();
        let folder = temp.path().join("checkout");
        fs::create_dir(&folder).unwrap();
        let checkout_id = plain_repo(&folder, "now").checkouts[0].id.clone();
        let db_path = temp.path().join("ui-state.sqlite3");
        {
            let database = Database::open(&db_path).unwrap();
            database
                .register_plain_repo(plain_repo(&folder, "now"))
                .unwrap();
            database
                .save_checkout_ui_state(
                    &checkout_id,
                    &CheckoutUiState {
                        document: Some(PersistedDocument {
                            checkout_id: checkout_id.clone(),
                            path: "2026-03-14-1532.md".into(),
                            origin: "review-workdir".into(),
                            source: "file".into(),
                            mode: "view".into(),
                        }),
                        main_view: "document".into(),
                        ..CheckoutUiState::default()
                    },
                )
                .unwrap();
        }

        let reopened = Database::open(&db_path).unwrap();
        assert_eq!(
            reopened
                .load_checkout_ui_state(&checkout_id)
                .unwrap()
                .document
                .unwrap()
                .origin,
            "review-workdir"
        );
    }

    #[test]
    fn a_document_saved_without_an_origin_is_dropped_not_guessed() {
        let temp = tempdir().unwrap();
        let folder = temp.path().join("checkout");
        fs::create_dir(&folder).unwrap();
        let repo = plain_repo(&folder, "now");
        let checkout_id = repo.checkouts[0].id.clone();
        let database = Database::open(temp.path().join("ui-state.sqlite3")).unwrap();
        database.register_plain_repo(repo).unwrap();
        let old_shape = serde_json::to_string(&serde_json::json!({
            "version": 1,
            "document": {
                "checkoutId": checkout_id.clone(),
                "path": "README.md",
                "source": "file",
                "mode": "view"
            },
            "mainView": "document",
            "diffAllFiles": false,
            "inspectorTab": "files",
            "selectedFilePath": null,
            "selectedChangePath": null,
            "expandedDirectories": [],
            "filesScrollTop": 0,
            "changesScrollTop": 0,
            "documentScrollTop": 0,
            "documentScrollLeft": 0,
            "diffScrollTop": 0
        }))
        .unwrap();
        database
            .connection
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO checkout_ui_states (checkout_id, state_json) VALUES (?1, ?2)",
                params![checkout_id, old_shape],
            )
            .unwrap();

        assert_eq!(
            database.load_checkout_ui_state(&checkout_id).unwrap(),
            CheckoutUiState::default()
        );
    }

    #[test]
    fn review_target_defaults_to_markdown_and_is_stored_per_checkout() {
        let database = Database::open_in_memory().unwrap();
        assert_eq!(database.review_target("checkout:one").unwrap(), "markdown");
        database
            .set_review_target("checkout:one", "opencode")
            .unwrap();
        assert_eq!(database.review_target("checkout:one").unwrap(), "opencode");
        // Pi remains an independent review destination even though it has no agent session list.
        database.set_review_target("checkout:one", "pi").unwrap();
        assert_eq!(database.review_target("checkout:one").unwrap(), "pi");
        database
            .set_review_target("checkout:one", "markdown")
            .unwrap();
        assert_eq!(database.review_target("checkout:one").unwrap(), "markdown");
        assert_eq!(database.review_target("checkout:two").unwrap(), "markdown");
        assert!(database.set_review_target("checkout:one", "codex").is_err());
        database
            .connection
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO preferences (key, value) VALUES ('review_target:checkout:three', 'codex')",
                [],
            )
            .unwrap();
        assert_eq!(
            database.review_target("checkout:three").unwrap(),
            "markdown"
        );
    }

    #[test]
    fn a_review_round_and_its_prompt_survive_a_restart() {
        let temp = tempdir().unwrap();
        let folder = temp.path().join("checkout");
        fs::create_dir(&folder).unwrap();
        let path = temp.path().join("workspace.sqlite3");
        let repo = plain_repo(&folder, "now");
        let checkout_id = repo.checkouts[0].id.clone();
        let database = Database::open(&path).unwrap();
        database.register_plain_repo(repo).unwrap();
        let expected = add_round(&database, &checkout_id, "round:one", "dispatched");
        drop(database);

        let reopened = Database::open(&path).unwrap();
        assert_eq!(
            reopened.review_rounds(&checkout_id).unwrap(),
            vec![expected]
        );
        assert_eq!(
            reopened
                .review_round_prompt("round:one", &checkout_id)
                .unwrap()
                .as_deref(),
            Some("saved review prompt")
        );
    }

    #[test]
    fn app_layout_normalizes_dimensions_and_unknown_modes() {
        let layout = serde_json::from_value::<AppLayoutState>(serde_json::json!({
            "mode": "unknown",
            "sidebarWidth": 260,
            "inspectorWidth": 320,
            "previewWidth": 1200
        }))
        .unwrap()
        .normalized();
        assert_eq!(
            layout,
            AppLayoutState {
                mode: "focus".into(),
                sidebar_width: 260,
                inspector_width: 320,
                preview_width: 900,
            }
        );

        let out_of_range = serde_json::from_value::<AppLayoutState>(serde_json::json!({
            "mode": "split",
            "sidebarWidth": 220,
            "inspectorWidth": 560,
            "previewWidth": 200
        }))
        .unwrap()
        .normalized();
        assert_eq!(
            out_of_range,
            AppLayoutState {
                mode: "split".into(),
                sidebar_width: 240,
                inspector_width: 480,
                preview_width: 260,
            }
        );
    }

    #[test]
    fn a_layout_row_this_build_cannot_read_falls_to_the_default_and_is_deleted() {
        // Widths are the only thing this row holds now, so a row written when it also held the
        // preferences is a shape this build cannot read. There is no version left to compare it
        // against, so what refuses it is the shape itself: no parse, the default window, and the
        // row deleted rather than left to be read again on the next launch.
        let database = Database::open_in_memory().unwrap();
        database
            .connection
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO preferences (key, value) VALUES (?1, ?2)",
                params![UI_LAYOUT, r#"{"mode":"split","sidebarWidth":"wide"}"#],
            )
            .unwrap();

        assert_eq!(
            database.load_app_layout().unwrap(),
            AppLayoutState::default()
        );
        let connection = database.connection.lock().unwrap();
        let remains: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM preferences WHERE key = ?1)",
                [UI_LAYOUT],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!remains);
    }

    #[test]
    fn a_layout_row_carrying_fields_this_build_dropped_is_read_and_rewritten() {
        // The opposite case: the row still parses, so the widths in it are honoured, and the
        // fields this build does not have are dropped on the way out, which is what deletes the
        // row. Nothing is guessed and nothing is refused that this build can still read.
        let database = Database::open_in_memory().unwrap();
        database
            .connection
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO preferences (key, value) VALUES (?1, ?2)",
                params![
                    UI_LAYOUT,
                    r#"{"version":4,"mode":"split","sidebarWidth":340,"inspectorWidth":420,"previewWidth":720,"terminalScrollbar":"auto","zoom":1.2}"#
                ],
            )
            .unwrap();

        assert_eq!(
            database.load_app_layout().unwrap(),
            AppLayoutState {
                mode: "split".into(),
                sidebar_width: 340,
                inspector_width: 420,
                preview_width: 720,
            }
        );
        let connection = database.connection.lock().unwrap();
        let remains: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM preferences WHERE key = ?1)",
                [UI_LAYOUT],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!remains);
    }

    #[test]
    fn checkout_ui_state_rejects_old_saved_state_shape() {
        // A row written before `diffAllFiles` and the scroll fields existed has no version left to
        // be refused by, so the missing fields are what refuses it: no parse, the default state.
        let old_state = serde_json::json!({
            "document": null,
            "mainView": "terminal",
            "inspectorTab": "files",
            "selectedFilePath": null,
            "selectedChangePath": null,
            "expandedDirectories": [],
            "filesScrollTop": 64,
            "documentScrollTop": 32,
            "documentScrollLeft": 4
        });
        assert!(serde_json::from_value::<CheckoutUiState>(old_state).is_err());

        // A row carrying fields this build no longer has still reads: it is what this build can
        // make sense of that is kept, and nothing about it is refused on a version it does not
        // write.
        let carried = serde_json::json!({
            "version": 1,
            "document": null,
            "mainView": "terminal",
            "diffAllFiles": false,
            "inspectorTab": "files",
            "selectedFilePath": null,
            "selectedChangePath": null,
            "expandedDirectories": [],
            "filesScrollTop": 64,
            "changesScrollTop": 0,
            "documentScrollTop": 32,
            "documentScrollLeft": 4,
            "diffScrollTop": 0
        });
        assert_eq!(
            serde_json::from_value::<CheckoutUiState>(carried).unwrap(),
            CheckoutUiState {
                files_scroll_top: 64,
                document_scroll_top: 32,
                document_scroll_left: 4,
                ..CheckoutUiState::default()
            }
        );

        let invalid = CheckoutUiState {
            changes_scroll_top: 10_000_001,
            ..CheckoutUiState::default()
        };
        assert!(!super::validate_checkout_ui_state("checkout:one", &invalid));
    }

    #[test]
    fn the_whole_change_set_view_survives_a_save_and_load() {
        let temp = tempdir().unwrap();
        let folder = temp.path().join("checkout");
        fs::create_dir(&folder).unwrap();
        let repo = plain_repo(&folder, "now");
        let checkout_id = repo.checkouts[0].id.clone();
        let database = Database::open(temp.path().join("ui-state.sqlite3")).unwrap();
        database.register_plain_repo(repo).unwrap();

        let all_changes = CheckoutUiState {
            diff_all_files: true,
            ..CheckoutUiState::default()
        };
        assert!(super::validate_checkout_ui_state(
            &checkout_id,
            &all_changes
        ));
        database
            .save_checkout_ui_state(&checkout_id, &all_changes)
            .unwrap();
        assert_eq!(
            database.load_checkout_ui_state(&checkout_id).unwrap(),
            all_changes
        );

        // It names no file, so a state that also carries one is contradictory, not a guess.
        let contradictory = CheckoutUiState {
            diff_all_files: true,
            main_view: "document".into(),
            document: Some(PersistedDocument {
                checkout_id: checkout_id.clone(),
                path: "src/main.rs".into(),
                origin: "checkout".into(),
                source: "change".into(),
                mode: "diff".into(),
            }),
            ..CheckoutUiState::default()
        };
        assert!(!super::validate_checkout_ui_state(
            &checkout_id,
            &contradictory
        ));
    }

    #[test]
    fn checkout_ui_state_moves_with_relocated_plain_checkout_and_cascades_on_delete() {
        let temp = tempdir().unwrap();
        let old_path = temp.path().join("old");
        let new_path = temp.path().join("new");
        fs::create_dir(&old_path).unwrap();
        fs::create_dir(&new_path).unwrap();
        let old_repo = plain_repo(&old_path, "1");
        let new_repo = plain_repo(&new_path, "2");
        let old_checkout_id = old_repo.checkouts[0].id.clone();
        let new_checkout_id = new_repo.checkouts[0].id.clone();
        let database = Database::open_in_memory().unwrap();
        database.register_plain_repo(old_repo.clone()).unwrap();
        let ui_state = CheckoutUiState {
            document: Some(PersistedDocument {
                checkout_id: old_checkout_id.clone(),
                path: "README.md".into(),
                origin: "checkout".into(),
                source: "file".into(),
                mode: "view".into(),
            }),
            main_view: "document".into(),
            ..CheckoutUiState::default()
        };
        database
            .save_checkout_ui_state(&old_checkout_id, &ui_state)
            .unwrap();
        fs::remove_dir(&old_path).unwrap();

        database
            .relocate_plain_checkout(&old_repo.id, &old_checkout_id, &new_repo)
            .unwrap();
        let mut moved_state = ui_state;
        moved_state.document.as_mut().unwrap().checkout_id = new_checkout_id.clone();
        assert_eq!(
            database.load_checkout_ui_state(&new_checkout_id).unwrap(),
            moved_state
        );
        assert_eq!(
            database
                .load_checkout_ui_state(&old_checkout_id)
                .unwrap_err(),
            "checkout does not exist"
        );

        database
            .connection
            .lock()
            .unwrap()
            .execute("DELETE FROM repos WHERE id = ?1", [&new_repo.id])
            .unwrap();
        let connection = database.connection.lock().unwrap();
        let state_remains: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM checkout_ui_states WHERE checkout_id = ?1)",
                [&new_checkout_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!state_remains);
    }

    #[test]
    fn relocating_plain_checkout_preserves_review_history_and_strands_pending_rounds() {
        let temp = tempdir().unwrap();
        let old_path = temp.path().join("old");
        let new_path = temp.path().join("new");
        let other_path = temp.path().join("other");
        for path in [&old_path, &new_path, &other_path] {
            fs::create_dir(path).unwrap();
        }
        let old_repo = plain_repo(&old_path, "1");
        let new_repo = plain_repo(&new_path, "2");
        let other_repo = plain_repo(&other_path, "3");
        let old_id = old_repo.checkouts[0].id.clone();
        let new_id = new_repo.checkouts[0].id.clone();
        let other_id = other_repo.checkouts[0].id.clone();
        let database = Database::open_in_memory().unwrap();
        database.register_plain_repo(old_repo.clone()).unwrap();
        database.register_plain_repo(other_repo).unwrap();
        add_round(&database, &old_id, "round:acked", "acked");
        add_round(&database, &old_id, "round:dispatched", "dispatched");
        add_round(&database, &old_id, "round:queued", "queued");
        add_round(&database, &old_id, "round:dispatching", "dispatching");
        add_round(&database, &other_id, "round:other", "acked");
        fs::remove_dir(&old_path).unwrap();

        database
            .relocate_plain_checkout(&old_repo.id, &old_id, &new_repo)
            .unwrap();

        assert!(database
            .load_registered_checkout(&old_id)
            .unwrap()
            .is_none());
        assert!(database.review_notes(&old_id).unwrap().is_empty());
        assert!(database.review_rounds(&old_id).unwrap().is_empty());
        assert_moved_round(&database, &new_id, "round:acked", "acked");
        assert_moved_round(&database, &new_id, "round:dispatched", "dispatched");
        for id in ["round:queued", "round:dispatching"] {
            let relocated = assert_moved_round(&database, &new_id, id, "relocated");
            assert!(!relocated.is_pending());
        }
        assert_eq!(database.requeue_review_rounds(&new_id).unwrap(), 0);
        assert_moved_round(&database, &other_id, "round:other", "acked");

        database
            .connection
            .lock()
            .unwrap()
            .execute("DELETE FROM repos WHERE id = ?1", [&new_repo.id])
            .unwrap();
        assert!(database.review_notes(&new_id).unwrap().is_empty());
        assert!(database.review_rounds(&new_id).unwrap().is_empty());
    }

    #[test]
    fn locating_git_checkout_preserves_review_round_links_and_other_ownership() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        let old_path = temp.path().join("old-worktree");
        let new_path = temp.path().join("new-worktree");
        let other_root = temp.path().join("other-repo");
        let other_worktree = temp.path().join("other-worktree");
        for path in [&root, &old_path, &other_root, &other_worktree] {
            fs::create_dir_all(path).unwrap();
        }
        let (old_repo, old_id) = git_repo_with_worktree(&root, &old_path);
        let (other_repo, other_id) = git_repo_with_worktree(&other_root, &other_worktree);
        let database = Database::open_in_memory().unwrap();
        database
            .register_git_repo(old_repo.clone(), &old_id)
            .unwrap();
        database.register_git_repo(other_repo, &other_id).unwrap();
        add_round(&database, &old_id, "round:git-acked", "acked");
        add_round(&database, &other_id, "round:other-git", "acked");
        fs::remove_dir_all(&old_path).unwrap();
        fs::create_dir(&new_path).unwrap();

        let (mut located_repo, new_id) = git_repo_with_worktree(&root, &new_path);
        located_repo
            .checkouts
            .iter_mut()
            .find(|checkout| checkout.id == new_id)
            .unwrap()
            .branch = Some("feature".into());
        database
            .connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE checkouts SET is_missing = 1, branch = 'feature' WHERE id = ?1",
                [&old_id],
            )
            .unwrap();
        let snapshot = database
            .git_repo_registration_snapshot(&old_repo.id)
            .unwrap()
            .unwrap();

        assert!(database
            .locate_git_checkout_if_unchanged(&located_repo, &old_id, &new_id, &snapshot)
            .unwrap());

        assert!(database
            .load_registered_checkout(&old_id)
            .unwrap()
            .is_none());
        assert!(database.review_notes(&old_id).unwrap().is_empty());
        assert!(database.review_rounds(&old_id).unwrap().is_empty());
        assert_moved_round(&database, &new_id, "round:git-acked", "acked");
        assert_moved_round(&database, &other_id, "round:other-git", "acked");
    }

    #[test]
    fn relocation_rejects_cross_checkout_round_references_and_rolls_back() {
        let temp = tempdir().unwrap();
        let source_path = temp.path().join("source");
        let target_path = temp.path().join("target");
        fs::create_dir(&source_path).unwrap();
        fs::create_dir(&target_path).unwrap();
        let source = plain_repo(&source_path, "1");
        let target = plain_repo(&target_path, "2");
        let source_id = source.checkouts[0].id.clone();
        let target_id = target.checkouts[0].id.clone();
        let database = Database::open_in_memory().unwrap();
        database.register_plain_repo(source).unwrap();
        database.register_plain_repo(target).unwrap();
        add_round(&database, &source_id, "round:source", "acked");
        database
            .add_review_note(&ReviewNote {
                id: "note:target-collision".into(),
                checkout_id: target_id.clone(),
                path: "src/main.rs".into(),
                side: "new".into(),
                line_start: 1,
                line_end: None,
                content: "must stay with target".into(),
                code: "fn main() {}".into(),
                status: "sent".into(),
                code_hash: review_anchor_hash("fn main() {}"),
                outdated: false,
                round_id: Some("round:source".into()),
                created_at: "now".into(),
                updated_at: "now".into(),
            })
            .unwrap();

        {
            let connection = database.connection.lock().unwrap();
            let transaction = connection.unchecked_transaction().unwrap();
            transaction
                .execute(
                    "INSERT INTO preferences (key, value) VALUES ('rollback_probe', 'written')",
                    [],
                )
                .unwrap();
            let error = super::transfer_checkout_metadata(&transaction, &source_id, &target_id)
                .unwrap_err();
            assert!(error.contains("conflicting checkout ownership"), "{error}");
        }

        let connection = database.connection.lock().unwrap();
        let probe_written: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM preferences WHERE key = 'rollback_probe')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!probe_written);
        drop(connection);
        assert_moved_round(&database, &source_id, "round:source", "acked");
        let target_note = database.review_notes(&target_id).unwrap().pop().unwrap();
        assert_eq!(target_note.checkout_id, target_id);
        assert_eq!(target_note.round_id.as_deref(), Some("round:source"));
    }

    #[test]
    fn viewed_file_progress_is_checkout_scoped_and_survives_database_restart() {
        let temp = tempdir().expect("temporary directory");
        let db_path = temp.path().join("workspace.sqlite3");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();

        let (first_id, second_id) = {
            let database = Database::open(&db_path).expect("database");
            let first_state = database
                .register_plain_repo(plain_repo(&first, "1"))
                .expect("first checkout");
            let second_state = database
                .register_plain_repo(plain_repo(&second, "2"))
                .expect("second checkout");
            let first_id = first_state.repos[0].checkouts[0].id.clone();
            let second_id = second_state.repos[1].checkouts[0].id.clone();
            database
                .mark_file_viewed(&first_id, "src/main.rs")
                .expect("mark viewed");
            (first_id, second_id)
        };

        let reopened = Database::open(&db_path).expect("reopened database");
        assert_eq!(reopened.viewed_files(&first_id).unwrap(), ["src/main.rs"]);
        assert!(reopened.viewed_files(&second_id).unwrap().is_empty());
    }

    /// The workdir menu lists what this machine has had open, so the read has to come back in
    /// the order the menu shows — newest first — and inside the ten rows the writes keep.
    #[test]
    fn recent_paths_come_back_newest_first_and_capped_at_ten() {
        let database = Database::open_in_memory().expect("database");
        let mut newest_first = Vec::new();
        for index in 1..=12 {
            let temp = tempdir().expect("temporary directory");
            let folder = temp.path().join(format!("folder {index}"));
            fs::create_dir(&folder).unwrap();
            // The stamps are ISO-8601, so their text order is their time order.
            database
                .register_plain_repo(plain_repo(
                    &folder,
                    &format!("2026-01-{index:02}T00:00:00Z"),
                ))
                .expect("register folder");
        }
        for (index, recent) in database
            .list_recent_paths()
            .expect("recent paths")
            .iter()
            .enumerate()
        {
            let expected = 12 - index;
            assert!(
                recent
                    .canonical_path
                    .ends_with(&format!("folder {expected}")),
                "{recent:?}"
            );
            newest_first.push(recent.canonical_path.clone());
        }
        assert_eq!(newest_first.len(), 10);
    }

    #[test]
    fn plain_repo_order_selection_and_identity_survive_restart_without_duplicates() {
        let temp = tempdir().expect("temporary directory");
        let db_path = temp.path().join("workspace.sqlite3");
        let first = temp.path().join("first plain");
        let second = temp.path().join("second plain");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();

        let first_repo = plain_repo(&first, "1");
        let second_repo = plain_repo(&second, "2");
        {
            let database = Database::open(&db_path).expect("database");
            database
                .register_plain_repo(first_repo.clone())
                .expect("register first");
            database
                .register_plain_repo(second_repo)
                .expect("register second");
            let state = database
                .register_plain_repo(plain_repo(&first, "3"))
                .expect("reopen first");
            assert_eq!(state.repos.len(), 2);
            assert_eq!(state.repos[0].id, first_repo.id);
            assert_eq!(state.repos[1].name, "second plain");
            assert_eq!(
                state.active_checkout_id,
                Some(first_repo.checkouts[0].id.clone())
            );
        }

        let state = Database::open(&db_path)
            .expect("restart database")
            .load_workspace()
            .expect("restore workspace");
        assert_eq!(state.repos.len(), 2);
        assert_eq!(state.repos[0].id, first_repo.id);
        assert_eq!(
            state.active_checkout_id,
            Some(first_repo.checkouts[0].id.clone())
        );
        assert_eq!(
            state.repos[0].kind,
            crate::domain::workspace::RepoKind::Plain
        );
        assert_eq!(state.repos[0].checkouts.len(), 1);
    }

    #[test]
    fn window_geometry_and_maximized_preference_restore_from_sqlite() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("workspace.sqlite3");
        let geometry = super::WindowGeometry {
            x: -1200,
            y: 80,
            width: 1440,
            height: 920,
        };
        {
            let database = Database::open(&path).unwrap();
            database.set_window_geometry(geometry).unwrap();
            database.set_window_maximized(true).unwrap();
            assert!(database
                .set_window_geometry(super::WindowGeometry {
                    width: 800,
                    ..geometry
                })
                .is_err());
        }

        let restored = Database::open(path).unwrap();
        assert_eq!(restored.window_geometry().unwrap(), Some(geometry));
        assert!(restored.window_maximized().unwrap());
    }

    #[test]
    fn sec_06_session_lookup_only_returns_ids_registered_in_sqlite() {
        let temp = tempdir().unwrap();
        let folder = temp.path().join("checkout");
        fs::create_dir(&folder).unwrap();
        let repo = plain_repo(&folder, "now");
        let database = Database::open_in_memory().unwrap();
        database.register_plain_repo(repo.clone()).unwrap();
        let session = Session {
            id: "session:known".into(),
            session_type: SessionType::Shell,
            checkout_id: repo.checkouts[0].id.clone(),
            name: "shell".into(),
            created_at: "now".into(),
            status: SessionStatus::Active,
        };
        database.add_terminal_session(&session).unwrap();

        assert_eq!(
            database.terminal_session_checkout(&session.id).unwrap(),
            Some(repo.checkouts[0].id.clone())
        );
        assert_eq!(
            database
                .terminal_session_checkout("session:unknown")
                .unwrap(),
            None
        );
    }

    /// Syncing the same repository again has to leave its checkouts where they were. Each sync
    /// used to push every position up by the current maximum, so the numbers doubled every time:
    /// past `i64::MAX` SQLite stored the column as a REAL, and the next read of any position in it
    /// failed with a type error that named no row. Sixty-odd syncs is a few months of opening the
    /// app, and the first thing to break was registering a folder at all.
    #[test]
    fn syncing_the_same_repository_again_does_not_move_its_checkouts() {
        let temp = tempdir().expect("temporary directory");
        let root = temp.path().join("repo");
        let worktree = temp.path().join("feature");
        for folder in [&root, &worktree] {
            fs::create_dir(folder).unwrap();
        }
        let (repo, worktree_id) = git_repo_with_worktree(&root, &worktree);
        let focus = repo.checkouts[0].id.clone();
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        database.register_git_repo(repo.clone(), &focus).unwrap();

        let positions = |database: &Database| {
            let connection = database.connection.lock().unwrap();
            let rows = connection
                .prepare("SELECT id, position, typeof(position) FROM checkouts ORDER BY id")
                .unwrap()
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            rows
        };
        let first = positions(&database);
        assert!(
            first.iter().all(|(_, _, kind)| kind == "integer"),
            "a position was not stored as an integer: {first:?}"
        );

        for _ in 0..80 {
            database.register_git_repo(repo.clone(), &focus).unwrap();
        }
        assert_eq!(positions(&database), first, "syncing moved the checkouts");
        // A worktree Git stopped listing is the row that used to keep growing: it keeps a number
        // that fits, and it sorts past the ones that are still there.
        fs::remove_dir_all(&worktree).unwrap();
        let shrunk = Repo {
            checkouts: vec![repo.checkouts[0].clone()],
            ..repo.clone()
        };
        database.register_git_repo(shrunk, &focus).unwrap();
        let after = positions(&database);
        assert!(
            after.iter().all(|(_, _, kind)| kind == "integer"),
            "a missing checkout stored a position no integer can hold: {after:?}"
        );
        let live = after
            .iter()
            .find(|(id, _, _)| id == &focus)
            .expect("the primary checkout is still listed");
        let gone = after
            .iter()
            .find(|(id, _, _)| id == &worktree_id)
            .expect("the removed worktree is still listed");
        assert!(
            gone.1 > live.1,
            "the removed worktree sorts before the one that is left"
        );
    }

    /// The column is typed by affinity, not by promise: a value too large for a 64-bit integer is
    /// written as a REAL, and reading it back as `i64` is an error. One such row is enough to fail
    /// every read of every position in the table, so a file carrying one has to still register the
    /// folders it did not have, rather than refusing all of them with a type name.
    #[test]
    fn a_position_too_large_for_an_integer_does_not_stop_a_folder_from_being_registered() {
        let temp = tempdir().expect("temporary directory");
        let root = temp.path().join("repo");
        let worktree = temp.path().join("feature");
        let other_root = temp.path().join("other");
        for folder in [&root, &worktree, &other_root] {
            fs::create_dir(folder).unwrap();
        }
        let (repo, _) = git_repo_with_worktree(&root, &worktree);
        let focus = repo.checkouts[0].id.clone();
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        database.register_git_repo(repo.clone(), &focus).unwrap();
        {
            // What a database written by the doubling above holds: past the largest integer.
            let connection = database.connection.lock().unwrap();
            connection
                .execute(
                    "UPDATE repos SET position = 1.0376293541461623e+19 WHERE id = ?1",
                    [&repo.id],
                )
                .unwrap();
        }

        let other = plain_repo(&other_root, "other");
        let registered = database
            .register_plain_repo(other.clone())
            .expect("a position that is not an integer must not stop a registration");
        assert!(registered
            .repos
            .iter()
            .any(|candidate| candidate.id == other.id));
    }
    #[test]
    fn checkout_order_is_restored_from_its_persisted_position() {
        let temp = tempdir().expect("temporary directory");
        let db_path = temp.path().join("workspace.sqlite3");
        let root = temp.path().join("repo");
        let second_checkout = temp.path().join("feature");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&second_checkout).unwrap();
        let repo = plain_repo(&root, "1");
        let database = Database::open(&db_path).expect("database");
        database
            .register_plain_repo(repo.clone())
            .expect("register repo");
        {
            let connection = database.connection.lock().unwrap();
            connection
                .execute("UPDATE repos SET kind = 'git' WHERE id = ?1", [&repo.id])
                .unwrap();
            connection
                .execute(
                    "UPDATE checkouts SET position = 1 WHERE id = ?1",
                    [&repo.checkouts[0].id],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO checkouts
                     (id, repo_id, path, canonical_path, is_primary, branch, head, ahead_of_default, changed_files, position)
                     VALUES ('checkout:feature', ?1, ?2, ?2, 0, 'feature', NULL, NULL, 0, 0)",
                    params![repo.id, second_checkout.canonicalize().unwrap().display().to_string()],
                )
                .unwrap();
        }

        let state = database.load_workspace().expect("restore workspace");
        assert_eq!(state.repos[0].kind, crate::domain::workspace::RepoKind::Git);
        assert_eq!(
            state.repos[0]
                .checkouts
                .iter()
                .map(|checkout| checkout.id.as_str())
                .collect::<Vec<_>>(),
            ["checkout:feature", repo.checkouts[0].id.as_str()]
        );
    }

    #[test]
    fn batched_workspace_load_preserves_checkout_order_sessions_archives_and_missing_state() {
        let temp = tempdir().expect("temporary directory");
        let root = temp.path().join("repo");
        let feature = temp.path().join("feature");
        let archived = temp.path().join("archived");
        let other_root = temp.path().join("other");
        for path in [&root, &feature, &archived, &other_root] {
            fs::create_dir(path).unwrap();
        }
        let repo = plain_repo(&root, "1");
        let other_repo = plain_repo(&other_root, "2");
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        database.register_plain_repo(repo.clone()).unwrap();
        database.register_plain_repo(other_repo.clone()).unwrap();
        let feature_id = "checkout:feature";
        let archived_id = "checkout:archived";
        {
            let connection = database.connection.lock().unwrap();
            connection
                .execute("UPDATE repos SET kind = 'git' WHERE id = ?1", [&repo.id])
                .unwrap();
            connection
                .execute(
                    "UPDATE checkouts SET position = 2, is_missing = 1 WHERE id = ?1",
                    [&repo.checkouts[0].id],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO checkouts
                     (id, repo_id, path, canonical_path, is_primary, branch, changed_files, position)
                     VALUES (?1, ?2, ?3, ?4, 0, 'feature', 7, 0)",
                    params![
                        feature_id,
                        repo.id,
                        feature.display().to_string(),
                        feature.canonicalize().unwrap().display().to_string(),
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO checkouts
                     (id, repo_id, path, canonical_path, is_primary, branch, is_archived, position)
                     VALUES (?1, ?2, ?3, ?4, 0, 'archived', 1, 1)",
                    params![
                        archived_id,
                        repo.id,
                        archived.display().to_string(),
                        archived.canonicalize().unwrap().display().to_string(),
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO sessions (id, checkout_id, session_type, name, created_at, status)
                     VALUES ('session:feature', ?1, 'shell', 'feature shell', '3', 'active'),
                            ('session:archived', ?2, 'shell', 'archived shell', '4', 'active')",
                    params![feature_id, archived_id],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO preferences (key, value) VALUES ('active_checkout_id', ?1)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    [feature_id],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO preferences (key, value) VALUES ('active_session_id', 'session:feature')
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    [],
                )
                .unwrap();
        }

        let state = database.load_workspace().expect("load workspace");
        assert_eq!(
            state
                .repos
                .iter()
                .map(|repo| repo.id.as_str())
                .collect::<Vec<_>>(),
            [repo.id.as_str(), other_repo.id.as_str()]
        );
        assert_eq!(
            state.repos[0]
                .checkouts
                .iter()
                .map(|checkout| checkout.id.as_str())
                .collect::<Vec<_>>(),
            [feature_id, repo.checkouts[0].id.as_str()]
        );
        assert_eq!(state.repos[0].checkouts[0].changed_files, 7);
        assert!(state.repos[0].checkouts[1].is_missing);
        assert_eq!(
            state.repos[0].checkouts[0].sessions[0].id,
            "session:feature"
        );
        assert_eq!(
            state
                .archived_worktrees
                .iter()
                .map(|checkout| checkout.id.as_str())
                .collect::<Vec<_>>(),
            [archived_id]
        );
        assert_eq!(state.active_checkout_id.as_deref(), Some(feature_id));
        assert_eq!(state.active_session_id.as_deref(), Some("session:feature"));

        let (lookup_repo, lookup_checkout) = database
            .load_registered_checkout(feature_id)
            .unwrap()
            .expect("registered checkout");
        assert_eq!(lookup_repo.id, repo.id);
        assert_eq!(lookup_repo.checkouts[0].id, feature_id);
        assert_eq!(lookup_checkout.sessions[0].id, "session:feature");
        assert!(database
            .load_registered_checkout(archived_id)
            .unwrap()
            .is_none());
        assert!(database
            .load_registered_checkout("checkout:unregistered")
            .unwrap()
            .is_none());
        let (other_lookup_repo, _) = database
            .load_registered_checkout(&other_repo.checkouts[0].id)
            .unwrap()
            .expect("checkout from second repository");
        assert_eq!(other_lookup_repo.id, other_repo.id);
    }

    #[test]
    fn terminal_checkout_path_comes_from_persisted_non_missing_checkouts() {
        let temp = tempdir().expect("temporary directory");
        let folder = temp.path().join("checkout");
        fs::create_dir(&folder).unwrap();
        let repo = plain_repo(&folder, "now");
        let database = Database::open_in_memory().expect("database");
        database
            .register_plain_repo(repo.clone())
            .expect("register checkout");

        assert_eq!(
            database
                .terminal_checkout_path(&repo.checkouts[0].id)
                .unwrap(),
            folder.canonicalize().unwrap()
        );
        assert!(database
            .terminal_checkout_path("checkout:unregistered")
            .is_err());

        database
            .connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE checkouts SET is_missing = 1 WHERE id = ?1",
                [&repo.checkouts[0].id],
            )
            .unwrap();
        assert!(database
            .terminal_checkout_path(&repo.checkouts[0].id)
            .is_err());
        database
            .connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE checkouts SET is_missing = 0 WHERE id = ?1",
                [&repo.checkouts[0].id],
            )
            .unwrap();
        fs::remove_dir(&folder).unwrap();
        assert!(database
            .terminal_checkout_path(&repo.checkouts[0].id)
            .is_err());
    }

    /// A missing or archived worktree is not a place a session can be sent, so it is not
    /// offered as one; the path is what makes a session's directory nameable at all.
    #[test]
    fn checkout_directories_lists_only_the_worktrees_a_session_can_be_handed_to() {
        let temp = tempdir().expect("temporary directory");
        let root = temp.path().join("root");
        let worktree = temp.path().join("worktree");
        let gone = temp.path().join("gone");
        for folder in [&root, &worktree, &gone] {
            fs::create_dir(folder).unwrap();
        }
        let (repo, worktree_id) = git_repo_with_worktree(&root, &worktree);
        let root_id = repo.checkouts[0].id.clone();
        let database = Database::open_in_memory().expect("database");
        database
            .register_git_repo(repo, &worktree_id)
            .expect("register worktrees");

        let listed = database.checkout_directories().expect("list checkouts");
        let mut listed_ids = listed.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>();
        listed_ids.sort_unstable();
        let mut expected = vec![root_id.as_str(), worktree_id.as_str()];
        expected.sort_unstable();
        assert_eq!(listed_ids, expected);
        assert!(listed.iter().all(|(_, path)| path.is_absolute()));

        // Archiving takes a worktree off the panel, so a session may no longer be handed to it.
        database
            .archive_checkout(&worktree_id)
            .expect("archive worktree");
        assert!(database
            .checkout_directories()
            .expect("list checkouts")
            .iter()
            .all(|(id, _)| id != &worktree_id));
    }

    #[test]
    fn checkout_layouts_persist_and_heal_sessions_owned_by_another_checkout() {
        let temp = tempdir().expect("temporary directory");
        let first_path = temp.path().join("first");
        let second_path = temp.path().join("second");
        fs::create_dir(&first_path).unwrap();
        fs::create_dir(&second_path).unwrap();
        let first_repo = plain_repo(&first_path, "1");
        let second_repo = plain_repo(&second_path, "2");
        let database_path = temp.path().join("workspace.sqlite3");
        let database = Database::open(&database_path).expect("database");
        database
            .register_plain_repo(first_repo.clone())
            .expect("register first checkout");
        database
            .register_plain_repo(second_repo.clone())
            .expect("register second checkout");
        let first_session = Session {
            id: "session:first".into(),
            session_type: SessionType::Shell,
            checkout_id: first_repo.checkouts[0].id.clone(),
            name: "zsh".into(),
            created_at: "now".into(),
            status: SessionStatus::Active,
        };
        let second_session = Session {
            id: "session:second".into(),
            checkout_id: second_repo.checkouts[0].id.clone(),
            ..first_session.clone()
        };
        let first_split_session = Session {
            id: "session:first-split".into(),
            ..first_session.clone()
        };
        database
            .add_terminal_session(&first_session)
            .expect("add first session");
        database
            .add_terminal_session(&second_session)
            .expect("add second session");
        database
            .add_terminal_session(&first_split_session)
            .expect("add split session");
        let layout = CheckoutTerminalLayout {
            active_tab_id: Some("tab:first".into()),
            tabs: vec![TerminalLayoutTab {
                id: "tab:first".into(),
                root: TerminalLayoutNode::Split {
                    id: "split:first".into(),
                    direction: crate::domain::terminal_layout::SplitDirection::Horizontal,
                    ratio: 0.5,
                    first: Box::new(TerminalLayoutNode::Session {
                        session_id: first_session.id.clone(),
                    }),
                    second: Box::new(TerminalLayoutNode::Session {
                        session_id: first_split_session.id.clone(),
                    }),
                },
            }],
            session_order: vec![first_session.id.clone(), first_split_session.id.clone()],
        };

        database
            .save_terminal_layout(&first_repo.checkouts[0].id, &layout)
            .expect("save checkout layout");
        assert_eq!(
            database
                .load_terminal_layout(&first_repo.checkouts[0].id)
                .expect("load layout"),
            Some(layout.clone())
        );
        let foreign_layout = CheckoutTerminalLayout {
            active_tab_id: Some("tab:second".into()),
            tabs: vec![TerminalLayoutTab {
                id: "tab:second".into(),
                root: TerminalLayoutNode::Session {
                    session_id: second_session.id.clone(),
                },
            }],
            session_order: vec![second_session.id],
        };
        database
            .save_terminal_layout(&first_repo.checkouts[0].id, &foreign_layout)
            .expect("save checkout layout heals stale session membership");
        let healed = database
            .load_terminal_layout(&first_repo.checkouts[0].id)
            .expect("load healed layout")
            .expect("healed layout");
        assert_eq!(
            healed.session_order,
            vec![first_session.id.clone(), first_split_session.id.clone()]
        );
        database
            .save_terminal_layout(&first_repo.checkouts[0].id, &layout)
            .expect("restore layout for persistence checks");
        assert!(database.load_terminal_layout("checkout:unknown").is_err());
        assert_eq!(
            database
                .load_terminal_layout(&first_repo.checkouts[0].id)
                .expect("failed save leaves old layout intact"),
            Some(layout.clone())
        );
        database
            .connection
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM sessions WHERE id = ?1",
                [&first_split_session.id],
            )
            .unwrap();
        let pruned = database
            .load_terminal_layout(&first_repo.checkouts[0].id)
            .expect("prune a removed historical session");
        assert_eq!(
            pruned.unwrap().tabs[0].root,
            TerminalLayoutNode::Session {
                session_id: first_session.id
            }
        );
        drop(database);

        // A restart is not a session: what the saved layout pointed at is gone, so the layout
        // goes with it instead of healing into a row of dead tabs.
        let reopened = Database::open(&database_path).expect("reopen database");
        assert_eq!(
            reopened
                .load_terminal_layout(&first_repo.checkouts[0].id)
                .expect("load layout from SQLite"),
            None
        );
    }

    #[test]
    fn moving_a_session_hands_the_row_to_a_sibling_worktree_and_takes_it_off_the_panels() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        let worktree = temp.path().join("worktree");
        let other = temp.path().join("other");
        let other_worktree = temp.path().join("other-worktree");
        for folder in [&root, &worktree, &other, &other_worktree] {
            fs::create_dir_all(folder).unwrap();
        }
        let (repo, worktree_id) = git_repo_with_worktree(&root, &worktree);
        let (other_repo, other_worktree_id) = git_repo_with_worktree(&other, &other_worktree);
        let root_id = repo.checkouts[0].id.clone();
        let other_root_id = other_repo.checkouts[0].id.clone();
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        database.register_git_repo(repo, &worktree_id).unwrap();
        database
            .register_git_repo(other_repo, &other_worktree_id)
            .unwrap();
        let session = Session {
            id: "session:moved".into(),
            session_type: SessionType::Shell,
            checkout_id: root_id.clone(),
            name: "zsh".into(),
            created_at: "now".into(),
            status: SessionStatus::Active,
        };
        database.add_terminal_session(&session).unwrap();
        database
            .save_terminal_layout(
                &root_id,
                &CheckoutTerminalLayout {
                    active_tab_id: Some("tab:moved".into()),
                    tabs: vec![TerminalLayoutTab {
                        id: "tab:moved".into(),
                        root: TerminalLayoutNode::Session {
                            session_id: session.id.clone(),
                        },
                    }],
                    session_order: vec![session.id.clone()],
                },
            )
            .unwrap();

        // A registered checkout in another Git repository is a valid destination.
        let moved_elsewhere = database
            .move_terminal_session(&session.id, &other_root_id, false)
            .unwrap();
        assert_eq!(
            database.terminal_session_checkout(&session.id).unwrap(),
            Some(other_root_id.clone())
        );
        let foreign_session = moved_elsewhere
            .repos
            .iter()
            .flat_map(|repo| &repo.checkouts)
            .find(|checkout| checkout.id == other_root_id)
            .unwrap()
            .sessions
            .first()
            .unwrap();
        assert_eq!(foreign_session.name, session.name);
        assert_eq!(foreign_session.created_at, session.created_at);
        database
            .move_terminal_session(&session.id, &root_id, false)
            .unwrap();
        assert_eq!(
            database.terminal_session_checkout(&session.id).unwrap(),
            Some(root_id.clone())
        );
        assert!(database
            .move_terminal_session(&session.id, &root_id, true)
            .is_err());
        assert!(database
            .move_terminal_session("session:unknown", &worktree_id, true)
            .is_err());

        let moved = database
            .move_terminal_session(&session.id, &worktree_id, true)
            .unwrap();

        let moved_repo = moved
            .repos
            .iter()
            .find(|item| item.checkouts.iter().any(|checkout| checkout.id == root_id))
            .unwrap();
        let source = moved_repo
            .checkouts
            .iter()
            .find(|item| item.id == root_id)
            .unwrap();
        let target = moved_repo
            .checkouts
            .iter()
            .find(|item| item.id == worktree_id)
            .unwrap();
        assert!(source.sessions.is_empty());
        assert_eq!(target.sessions.len(), 1);
        assert_eq!(target.sessions[0].id, session.id);
        // The worktree the terminal now belongs to is the one the window shows, so the files and
        // the changes on screen are that worktree's.
        assert_eq!(
            moved.active_checkout_id.as_deref(),
            Some(worktree_id.as_str())
        );
        assert_eq!(
            moved.active_session_id.as_deref(),
            Some(session.id.as_str())
        );
        // Both layouts follow the row: the source loses the pane, the destination gains one.
        assert_eq!(
            database.load_terminal_layout(&root_id).unwrap(),
            Some(CheckoutTerminalLayout {
                active_tab_id: None,
                tabs: vec![],
                session_order: vec![],
            })
        );
        // The destination had no stored layout, so it has none to repair: the pane its sessions
        // name is built on the next read, and the session it gained is in it.
        assert_eq!(database.load_terminal_layout(&worktree_id).unwrap(), None);
    }

    #[test]
    fn registered_checkouts_can_take_terminal_sessions_from_any_source() {
        for select_target in [false, true] {
            let temp = tempdir().unwrap();
            let home = temp.path().join("home");
            let plain = temp.path().join("plain");
            let root = temp.path().join("repo");
            let worktree = temp.path().join("test");
            let other = temp.path().join("other");
            let other_worktree = temp.path().join("other-test");
            for path in [&home, &plain, &root, &worktree, &other, &other_worktree] {
                fs::create_dir_all(path).unwrap();
            }
            let database = Database::open_in_memory().unwrap();
            let home_repo = plain_repo(&home, "now");
            let home_id = home_repo.checkouts[0].id.clone();
            database.register_plain_repo(home_repo.clone()).unwrap();
            database.register_home_repo(&home_repo).unwrap();
            let plain_repo = plain_repo(&plain, "now");
            let plain_id = plain_repo.checkouts[0].id.clone();
            database.register_plain_repo(plain_repo).unwrap();
            let (repo, worktree_id) = git_repo_with_worktree(&root, &worktree);
            let root_id = repo.checkouts[0].id.clone();
            database.register_git_repo(repo, &worktree_id).unwrap();
            let (other_repo, other_id) = git_repo_with_worktree(&other, &other_worktree);
            database.register_git_repo(other_repo, &other_id).unwrap();
            let session = Session {
                id: "session:home-agent".into(),
                session_type: SessionType::Shell,
                checkout_id: home_id.clone(),
                name: "OpenCode".into(),
                created_at: "now".into(),
                status: SessionStatus::Active,
            };
            database.add_terminal_session(&session).unwrap();
            database
                .move_terminal_session(&session.id, &plain_id, false)
                .unwrap();
            assert_eq!(
                database.terminal_session_checkout(&session.id).unwrap(),
                Some(plain_id.clone())
            );
            database
                .move_terminal_session(&session.id, &home_id, false)
                .unwrap();
            database
                .save_terminal_layout(
                    &home_id,
                    &CheckoutTerminalLayout {
                        active_tab_id: Some("tab:home".into()),
                        tabs: vec![TerminalLayoutTab {
                            id: "tab:home".into(),
                            root: TerminalLayoutNode::Session {
                                session_id: session.id.clone(),
                            },
                        }],
                        session_order: vec![session.id.clone()],
                    },
                )
                .unwrap();
            let before = database.load_workspace().unwrap();
            assert!(database
                .move_terminal_session(&session.id, "checkout:unknown", select_target)
                .is_err());
            // Missing and archived destinations remain refused by the same public move contract.
            for column in ["is_missing", "is_archived"] {
                let sql = format!("UPDATE checkouts SET {column} = ?1 WHERE id = ?2");
                database
                    .connection
                    .lock()
                    .unwrap()
                    .execute(&sql, params![1, &worktree_id])
                    .unwrap();
                assert!(database
                    .move_terminal_session(&session.id, &worktree_id, select_target)
                    .is_err());
                database
                    .connection
                    .lock()
                    .unwrap()
                    .execute(&sql, params![0, &worktree_id])
                    .unwrap();
            }
            assert_eq!(
                database.terminal_session_checkout(&session.id).unwrap(),
                Some(home_id.clone())
            );
            database
                .move_terminal_session(&session.id, &root_id, select_target)
                .unwrap();
            assert_eq!(
                database.terminal_session_checkout(&session.id).unwrap(),
                Some(root_id.clone())
            );
            let moved = database
                .move_terminal_session(&session.id, &worktree_id, select_target)
                .unwrap();
            assert_eq!(
                database.terminal_session_checkout(&session.id).unwrap(),
                Some(worktree_id.clone())
            );
            let destination = moved
                .repos
                .iter()
                .flat_map(|repo| &repo.checkouts)
                .find(|checkout| checkout.id == worktree_id)
                .unwrap();
            assert_eq!(destination.sessions[0].id, session.id);
            assert_eq!(destination.sessions[0].name, session.name);
            assert_eq!(destination.sessions[0].created_at, session.created_at);
            assert!(database
                .load_terminal_layout(&home_id)
                .unwrap()
                .unwrap()
                .tabs
                .is_empty());
            if select_target {
                assert_eq!(
                    moved.active_checkout_id.as_deref(),
                    Some(worktree_id.as_str())
                );
                assert_eq!(
                    moved.active_session_id.as_deref(),
                    Some(session.id.as_str())
                );
            } else {
                assert_eq!(moved.active_checkout_id, before.active_checkout_id);
                assert_eq!(moved.active_session_id, before.active_session_id);
            }
            // The same session can move from Git B back to Home, then onward to another plain
            // checkout and back into Git A. Each move changes the row's checkout owner only.
            for destination in [&other_id, &home_id, &plain_id, &root_id] {
                database
                    .move_terminal_session(&session.id, destination, select_target)
                    .unwrap();
                assert_eq!(
                    database.terminal_session_checkout(&session.id).unwrap(),
                    Some(destination.clone())
                );
            }
        }
    }

    /// A move that happened on its own moves the row and nothing else: the window stays on the
    /// worktree the person was looking at, because a session changing directory is not a reason to
    /// take over the screen.
    #[test]
    fn a_move_that_was_not_asked_for_leaves_the_window_where_it_was() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        let worktree = temp.path().join("worktree");
        for folder in [&root, &worktree] {
            fs::create_dir_all(folder).unwrap();
        }
        let (repo, worktree_id) = git_repo_with_worktree(&root, &worktree);
        let root_id = repo.checkouts[0].id.clone();
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        database.register_git_repo(repo, &worktree_id).unwrap();
        let session = Session {
            id: "session:walked".into(),
            session_type: SessionType::Shell,
            checkout_id: root_id.clone(),
            name: "zsh".into(),
            created_at: "now".into(),
            status: SessionStatus::Active,
        };
        database.add_terminal_session(&session).unwrap();

        let moved = database
            .move_terminal_session(&session.id, &worktree_id, false)
            .unwrap();

        // The row moved...
        assert_eq!(
            database.terminal_session_checkout(&session.id).unwrap(),
            Some(worktree_id.clone())
        );
        // ...and the window did not.
        assert_eq!(moved.active_checkout_id.as_deref(), Some(root_id.as_str()));
        assert_ne!(
            moved.active_checkout_id.as_deref(),
            Some(worktree_id.as_str())
        );
    }

    #[test]
    fn missing_directory_is_reported_but_kept_in_persisted_organization() {
        let temp = tempdir().expect("temporary directory");
        let db_path = temp.path().join("workspace.sqlite3");
        let folder = temp.path().join("removed later");
        fs::create_dir(&folder).unwrap();
        let repo = plain_repo(&folder, "1");
        Database::open(&db_path)
            .expect("database")
            .register_plain_repo(repo.clone())
            .expect("register repo");
        fs::remove_dir(&folder).unwrap();

        // Reading the organization never prunes: the row stays, and reports itself missing.
        // Only the startup prune, in `services::workspace::restore`, drops it.
        let state = Database::open(&db_path)
            .expect("restart database")
            .load_workspace()
            .expect("restore workspace");
        assert_eq!(state.repos.len(), 1);
        assert_eq!(state.repos[0].id, repo.id);
        assert!(state.repos[0].checkouts[0].is_missing);
    }

    /// The workspace read is two halves: the rows, under the lock, and the folders, after it.
    /// The split cannot be observed by timing, so it is observed by what the locked half alone
    /// answers: a checkout whose folder is gone still comes back as stored, which is only
    /// possible when nothing in that half has stat'ed anything.
    #[test]
    fn folder_checks_are_folded_into_the_state_after_the_lock_is_released() {
        let temp = tempdir().expect("temporary directory");
        let live = temp.path().join("live");
        let stored_missing = temp.path().join("stored missing");
        for folder in [&live, &stored_missing] {
            fs::create_dir(folder).unwrap();
        }
        let repo = plain_repo(&live, "1");
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        database.register_plain_repo(repo.clone()).unwrap();
        let live_id = repo.checkouts[0].id.clone();
        // Two of these four folders are never created, so only the filesystem can report them
        // gone, and only the stored flags can report the rest.
        let rows = [
            ("checkout:gone", temp.path().join("gone"), 2, false, false),
            (
                "checkout:stored-missing",
                stored_missing.canonicalize().unwrap(),
                3,
                false,
                true,
            ),
            (
                "checkout:archived",
                temp.path().join("archived"),
                4,
                true,
                false,
            ),
        ];
        {
            let connection = database.connection.lock().unwrap();
            for (id, path, position, is_archived, is_missing) in &rows {
                connection
                    .execute(
                        "INSERT INTO checkouts
                         (id, repo_id, path, canonical_path, is_primary, is_archived, is_missing, position)
                         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7)",
                        params![
                            id,
                            repo.id,
                            path.display().to_string(),
                            path.display().to_string(),
                            is_archived,
                            is_missing,
                            position
                        ],
                    )
                    .unwrap();
            }
        }

        // The locked half reads rows and nothing else, so it reports what is stored even where
        // the disk disagrees, and an archived worktree is still off the panel and not gone.
        let rows_only = {
            let connection = database.connection.lock().unwrap();
            load_workspace(&connection).expect("read rows")
        };
        let flagged = |state: &crate::domain::workspace::WorkspaceState, id: &str| {
            state
                .repos
                .iter()
                .flat_map(|repo| &repo.checkouts)
                .find(|checkout| checkout.id == id)
                .expect("checkout on the panel")
                .is_missing
        };
        assert!(!flagged(&rows_only, &live_id));
        assert!(!flagged(&rows_only, "checkout:gone"));
        assert!(flagged(&rows_only, "checkout:stored-missing"));
        assert_eq!(rows_only.repos[0].checkouts.len(), 3);
        assert!(rows_only
            .archived_worktrees
            .iter()
            .any(|archived| archived.id == "checkout:archived"));

        // The unlocked half asks the disk, and only ever adds: gone becomes missing, a checkout
        // stored as missing keeps the flag with its folder right there, and a live one stays.
        let mut state = rows_only;
        mark_missing_checkouts(&mut state);
        assert!(!flagged(&state, &live_id));
        assert!(flagged(&state, "checkout:gone"));
        assert!(flagged(&state, "checkout:stored-missing"));
        assert_eq!(state.repos[0].checkouts.len(), 3);

        // The same state, through both entry points, which is where the halves are joined.
        let loaded = database.load_workspace().expect("load workspace");
        assert!(!flagged(&loaded, &live_id));
        assert!(flagged(&loaded, "checkout:gone"));
        assert!(flagged(&loaded, "checkout:stored-missing"));
        let (_, gone) = database
            .load_registered_checkout("checkout:gone")
            .unwrap()
            .expect("registered checkout");
        assert!(gone.is_missing);
        let (_, stored) = database
            .load_registered_checkout("checkout:stored-missing")
            .unwrap()
            .expect("registered checkout");
        assert!(stored.is_missing);
    }

    /// A Git repository with its primary and every given worktree on disk, without running
    /// Git: the persistence layer only stores paths. The returned ids are the worktrees, in
    /// the order they were given.
    fn git_repo_with_worktrees(root: &Path, worktrees: &[&Path]) -> (Repo, Vec<String>) {
        let root = fs::canonicalize(root).expect("root");
        let repo_id = repo_id_for_path(&root.display().to_string());
        let mut checkouts = vec![Checkout::new(repo_id.clone(), &root, true).expect("primary")];
        let ids = worktrees
            .iter()
            .map(|worktree| {
                let worktree = fs::canonicalize(worktree).expect("worktree");
                let id = checkout_id_for_path(&worktree.display().to_string());
                checkouts.push(Checkout::new(repo_id.clone(), &worktree, false).expect("worktree"));
                id
            })
            .collect();
        let repo = Repo {
            id: repo_id,
            kind: RepoKind::Git,
            name: "repo".into(),
            root: root.display().to_string(),
            default_branch: None,
            checkouts,
            created_at: "1".into(),
            last_opened_at: "1".into(),
        };
        (repo, ids)
    }

    /// A Git repository whose primary is on disk and whose worktree is not, without running
    /// Git: the persistence layer only stores paths.
    fn git_repo_with_worktree(root: &Path, worktree: &Path) -> (Repo, String) {
        let root = fs::canonicalize(root).expect("root");
        let root = fs::canonicalize(root).expect("root");
        let root_path = root.display().to_string();
        let repo_id = repo_id_for_path(&root_path);
        let worktree_id = checkout_id_for_path(
            &fs::canonicalize(worktree)
                .expect("worktree")
                .display()
                .to_string(),
        );
        let repo = Repo {
            id: repo_id.clone(),
            kind: RepoKind::Git,
            name: "repo".into(),
            root: root_path,
            default_branch: None,
            checkouts: vec![
                Checkout::new(repo_id.clone(), &root, true).expect("primary"),
                Checkout::new(repo_id, worktree, false).expect("worktree"),
            ],
            created_at: "1".into(),
            last_opened_at: "1".into(),
        };
        (repo, worktree_id)
    }

    /// Archiving is the reversible half of leaving a workdir: the row leaves the panel and
    /// the worktree stays registered, so a repo root can put it back and the disk is
    /// untouched either way. Closing is the half that forgets.
    #[test]
    fn archiving_a_worktree_takes_the_row_off_the_panel_and_keeps_it_restorable() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        let worktree = temp.path().join("worktree");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&worktree).unwrap();
        fs::write(root.join("tracked.txt"), "kept\n").unwrap();
        let (repo, worktree_id) = git_repo_with_worktree(&root, &worktree);
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        database.register_git_repo(repo, &worktree_id).unwrap();

        let archived = database.archive_checkout(&worktree_id).unwrap();

        // Off the panel, and named on the shelf the repo root reads, with the branch it is
        // known by rather than a bare path.
        assert_eq!(archived.repos[0].checkouts.len(), 1);
        assert!(archived.repos[0].checkouts[0].is_primary);
        assert_eq!(archived.archived_worktrees.len(), 1);
        assert_eq!(archived.archived_worktrees[0].id, worktree_id);
        assert_eq!(
            archived.archived_worktrees[0].repo_id,
            repo_id_for_path(&root.canonicalize().unwrap().display().to_string())
        );
        // The worktree was the selected one, so the selection follows it off the panel.
        assert_ne!(
            archived.active_checkout_id.as_deref(),
            Some(worktree_id.as_str())
        );
        assert_eq!(archived.active_session_id, None);
        // Nothing on disk moved: the directory and its file are exactly where they were.
        assert!(worktree.is_dir());
        assert!(root.join("tracked.txt").exists());
    }

    #[test]
    fn restoring_puts_every_archived_worktree_of_a_repository_back_on_the_panel() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        let first = temp.path().join("one");
        let second = temp.path().join("two");
        for folder in [&root, &first, &second] {
            fs::create_dir_all(folder).unwrap();
        }
        let (repo, ids) = git_repo_with_worktrees(&root, &[&first, &second]);
        let repo_id = repo.id.clone();
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        database.register_git_repo(repo, &ids[0]).unwrap();
        for id in &ids {
            database.archive_checkout(id).unwrap();
        }
        let shelved = database.load_workspace().unwrap();
        assert_eq!(shelved.repos[0].checkouts.len(), 1);
        assert_eq!(shelved.archived_worktrees.len(), 2);

        let restored = database.restore_archived_worktrees(&repo_id).unwrap();

        // One action brings the whole set back, which is the point of counting them.
        assert_eq!(restored.repos[0].checkouts.len(), 3);
        assert!(restored.archived_worktrees.is_empty());
        assert!(first.is_dir() && second.is_dir());
    }

    #[test]
    fn restoring_leaves_the_archived_worktrees_of_another_repository_alone() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        let other = temp.path().join("other");
        let worktree = temp.path().join("worktree");
        let other_worktree = temp.path().join("other-worktree");
        for folder in [&root, &other, &worktree, &other_worktree] {
            fs::create_dir_all(folder).unwrap();
        }
        let (first_repo, first_worktree_id) = git_repo_with_worktree(&root, &worktree);
        let (second_repo, second_worktree_id) = git_repo_with_worktree(&other, &other_worktree);
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        database
            .register_git_repo(first_repo, &first_worktree_id)
            .unwrap();
        database
            .register_git_repo(second_repo, &second_worktree_id)
            .unwrap();
        database.archive_checkout(&first_worktree_id).unwrap();
        database.archive_checkout(&second_worktree_id).unwrap();

        let restored = database
            .restore_archived_worktrees(&repo_id_for_path(
                &root.canonicalize().unwrap().display().to_string(),
            ))
            .unwrap();

        // The other repository's worktree is still on its own shelf: archiving is per repo,
        // so restoring one repo cannot quietly put back another repo's work.
        assert_eq!(restored.archived_worktrees.len(), 1);
        assert_eq!(restored.archived_worktrees[0].id, second_worktree_id);
    }

    #[test]
    fn archiving_is_refused_where_it_would_mean_more_than_taking_a_row_off_the_panel() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        let worktree = temp.path().join("worktree");
        let plain = temp.path().join("plain");
        for folder in [&root, &worktree, &plain] {
            fs::create_dir_all(folder).unwrap();
        }
        let (repo, worktree_id) = git_repo_with_worktree(&root, &worktree);
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        database.register_git_repo(repo, &worktree_id).unwrap();
        let primary_id = checkout_id_for_path(&root.canonicalize().unwrap().display().to_string());
        let plain_repo = plain_repo(&plain, "1");
        let plain_id = plain_repo.checkouts[0].id.clone();
        database.register_plain_repo(plain_repo).unwrap();

        // A repo root is the head of the list its worktrees hang off: archiving it would
        // archive the list, and closing is what the row's cross is for.
        let root_error = database.archive_checkout(&primary_id).unwrap_err();
        assert!(
            root_error.contains("only a worktree can be archived"),
            "{root_error}"
        );

        // A plain folder is not a worktree at all.
        let plain_error = database.archive_checkout(&plain_id).unwrap_err();
        assert!(
            plain_error.contains("only a worktree can be archived"),
            "{plain_error}"
        );

        // A worktree whose directory is gone has nothing to bring back.
        fs::remove_dir(&worktree).unwrap();
        let missing_error = database.archive_checkout(&worktree_id).unwrap_err();
        assert!(
            missing_error.contains("cannot be archived"),
            "{missing_error}"
        );

        // Nothing was archived by any of the refusals.
        assert!(database
            .load_workspace()
            .unwrap()
            .archived_worktrees
            .is_empty());
    }

    #[test]
    fn archiving_a_worktree_is_refused_while_one_of_its_sessions_runs() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        let worktree = temp.path().join("worktree");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&worktree).unwrap();
        let (repo, worktree_id) = git_repo_with_worktree(&root, &worktree);
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        database.register_git_repo(repo, &worktree_id).unwrap();
        database
            .add_terminal_session(&Session {
                id: "session:live".into(),
                session_type: SessionType::Shell,
                checkout_id: worktree_id.clone(),
                name: "shell".into(),
                created_at: "now".into(),
                status: SessionStatus::Active,
            })
            .unwrap();

        let error = database.archive_checkout(&worktree_id).unwrap_err();

        // The process would outlive the row that names it, which is the same reason a close
        // refuses: a terminal attached to a directory the panel no longer lists is unowned.
        assert!(error.contains("close active terminal sessions"), "{error}");
        assert_eq!(
            database.load_workspace().unwrap().repos[0].checkouts.len(),
            2
        );
        assert!(worktree.is_dir());
    }

    #[test]
    fn an_archived_worktree_survives_a_restart_and_is_still_there_to_restore() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("workspace.sqlite3");
        let root = temp.path().join("repo");
        let worktree = temp.path().join("worktree");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&worktree).unwrap();
        let (repo, worktree_id) = git_repo_with_worktree(&root, &worktree);
        {
            let database = Database::open(&db_path).unwrap();
            database.register_git_repo(repo, &worktree_id).unwrap();
            database.archive_checkout(&worktree_id).unwrap();
        }

        // Reopened from the file, not from the handle that wrote it: archiving is a stored
        // state, so it has to outlive the process that set it.
        let restored = Database::open(&db_path).unwrap().load_workspace().unwrap();
        assert_eq!(restored.repos[0].checkouts.len(), 1);
        assert_eq!(restored.archived_worktrees.len(), 1);
        assert_eq!(restored.archived_worktrees[0].id, worktree_id);
    }

    #[test]
    fn closing_a_missing_git_worktree_removes_only_its_registration() {
        let temp = tempdir().expect("temporary directory");
        let root = temp.path().join("repo");
        let worktree = temp.path().join("worktree");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&worktree).unwrap();
        fs::write(root.join("tracked.txt"), "kept\n").unwrap();
        let (repo, worktree_id) = git_repo_with_worktree(&root, &worktree);
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        let registered = database.register_git_repo(repo, &worktree_id).unwrap();
        assert_eq!(
            registered.active_checkout_id.as_deref(),
            Some(worktree_id.as_str())
        );
        // The worktree is deleted outside Muster, so the stored row is only marked by its path.
        fs::remove_dir(&worktree).unwrap();

        let closed = database.close_missing_checkout(&worktree_id).unwrap();

        // The entry goes and the selection falls back to the surviving primary; the
        // repository and its files stay exactly where they were.
        assert_eq!(closed.repos.len(), 1);
        assert_eq!(closed.repos[0].checkouts.len(), 1);
        let primary = &closed.repos[0].checkouts[0];
        assert!(primary.is_primary);
        assert_eq!(
            closed.active_checkout_id.as_deref(),
            Some(primary.id.as_str())
        );
        assert_eq!(closed.active_session_id, None);
        assert!(root.join("tracked.txt").exists());
    }

    #[test]
    fn closing_a_checkout_that_still_exists_is_refused() {
        let temp = tempdir().expect("temporary directory");
        let folder = temp.path().join("plain");
        fs::create_dir(&folder).unwrap();
        let repo = plain_repo(&folder, "1");
        let checkout_id = repo.checkouts[0].id.clone();
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        database.register_plain_repo(repo).unwrap();

        let error = database.close_missing_checkout(&checkout_id).unwrap_err();

        assert!(error.contains("only a missing checkout can be closed"));
        assert_eq!(database.load_workspace().unwrap().repos.len(), 1);
    }

    #[test]
    fn closing_a_live_worktree_drops_its_registration_and_leaves_its_directory() {
        let temp = tempdir().expect("temporary directory");
        let root = temp.path().join("repo");
        let worktree = temp.path().join("worktree");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&worktree).unwrap();
        fs::write(root.join("tracked.txt"), "kept\n").unwrap();
        let (repo, worktree_id) = git_repo_with_worktree(&root, &worktree);
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        let registered = database.register_git_repo(repo, &worktree_id).unwrap();

        let closed = database.close_checkout(&worktree_id).unwrap();

        // Only the row goes: the directory and everything Git wrote in it stay, which is what
        // makes this the reverse of opening the folder again.
        assert_eq!(closed.repos[0].checkouts.len(), 1);
        assert_eq!(
            closed.active_checkout_id.as_deref(),
            Some(registered.repos[0].checkouts[0].id.as_str())
        );
        assert_eq!(closed.active_session_id, None);
        assert!(worktree.is_dir());
        assert!(root.join("tracked.txt").exists());
    }

    #[test]
    fn closing_a_live_checkout_is_refused_while_one_of_its_sessions_runs() {
        let temp = tempdir().expect("temporary directory");
        let folder = temp.path().join("plain");
        fs::create_dir(&folder).unwrap();
        let repo = plain_repo(&folder, "1");
        let checkout_id = repo.checkouts[0].id.clone();
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        database.register_plain_repo(repo).unwrap();
        database
            .add_terminal_session(&Session {
                id: "session:live".into(),
                session_type: SessionType::Shell,
                checkout_id: checkout_id.clone(),
                name: "shell".into(),
                created_at: "now".into(),
                status: SessionStatus::Active,
            })
            .unwrap();

        let error = database.close_checkout(&checkout_id).unwrap_err();

        assert!(error.contains("close active terminal sessions"));
        assert_eq!(database.load_workspace().unwrap().repos.len(), 1);
        assert!(folder.is_dir());
    }

    fn print_load_benchmark(name: &str, samples: &[std::time::Duration], error_count: usize) {
        assert!(!samples.is_empty());
        let mut ordered = samples.to_vec();
        ordered.sort_unstable();
        let percentile = |percent: usize| ordered[(ordered.len() * percent - 1) / 100];
        println!(
            "{name} samples={} p50_ns={} p95_ns={} error_count={error_count}",
            samples.len(),
            percentile(50).as_nanos(),
            percentile(95).as_nanos(),
        );
    }

    #[test]
    #[ignore = "manual bounded local load benchmark; run scripts/benchmark-persistence-load.mjs"]
    fn bounded_database_load_metrics() {
        use std::time::Instant;

        const REPO_COUNT: usize = 12;
        const REPETITIONS: usize = 25;

        let temp = tempdir().expect("temporary benchmark directory");
        let database = Database::open(temp.path().join("workspace.sqlite3"))
            .expect("temporary benchmark database");
        let mut checkout_ids = Vec::with_capacity(REPO_COUNT);
        for index in 0..REPO_COUNT {
            let path = temp.path().join(format!("repo-{index:02}"));
            fs::create_dir(&path).expect("temporary checkout directory");
            let repo = plain_repo(&path, &format!("fixture-{index:02}"));
            checkout_ids.push(repo.checkouts[0].id.clone());
            database
                .register_plain_repo(repo)
                .expect("register fixture repo");
        }

        let target_checkout_id = checkout_ids.last().expect("fixture checkout").clone();
        println!("fixtures repos={REPO_COUNT} checkouts={REPO_COUNT} repetitions={REPETITIONS}");
        let mut workspace_samples = Vec::with_capacity(REPETITIONS);
        let mut workspace_errors = 0;
        for _ in 0..REPETITIONS {
            let started = Instant::now();
            let result = database.load_workspace();
            workspace_samples.push(started.elapsed());
            match result {
                Ok(state) => assert_benchmark_workspace(&state, REPO_COUNT),
                Err(_) => workspace_errors += 1,
            }
        }
        print_load_benchmark(
            "Database::load_workspace",
            &workspace_samples,
            workspace_errors,
        );
        assert_eq!(workspace_errors, 0, "load_workspace returned errors");

        let mut checkout_samples = Vec::with_capacity(REPETITIONS);
        let mut checkout_errors = 0;
        for _ in 0..REPETITIONS {
            let started = Instant::now();
            let result = database.load_registered_checkout(&target_checkout_id);
            checkout_samples.push(started.elapsed());
            match result {
                Ok(Some((repo, checkout))) => {
                    assert_eq!(checkout.id, target_checkout_id);
                    assert_eq!(repo.id, checkout.repo_id);
                    assert_eq!(repo.checkouts.len(), 1);
                    assert!(!checkout.is_missing);
                }
                Ok(None) => panic!("registered fixture checkout was not loaded"),
                Err(_) => checkout_errors += 1,
            }
        }
        print_load_benchmark(
            "Database::load_registered_checkout",
            &checkout_samples,
            checkout_errors,
        );
        assert_eq!(
            checkout_errors, 0,
            "load_registered_checkout returned errors"
        );

        // Verify that the real path check is observed and missing/unknown lookups stay explicit.
        let missing_checkout_id = &checkout_ids[0];
        fs::remove_dir_all(temp.path().join("repo-00")).expect("remove fixture checkout");
        let workspace = database
            .load_workspace()
            .expect("load workspace with missing path");
        assert!(workspace
            .repos
            .iter()
            .flat_map(|repo| &repo.checkouts)
            .any(|checkout| checkout.id == *missing_checkout_id && checkout.is_missing));
        let (_, checkout) = database
            .load_registered_checkout(missing_checkout_id)
            .expect("load missing-path checkout")
            .expect("registered checkout remains addressable");
        assert!(checkout.is_missing);
        assert!(database
            .load_registered_checkout("checkout:not-registered")
            .expect("unknown checkout lookup")
            .is_none());
    }

    fn assert_benchmark_workspace(
        state: &crate::domain::workspace::WorkspaceState,
        expected_repos: usize,
    ) {
        assert_eq!(state.repos.len(), expected_repos);
        assert!(state
            .repos
            .iter()
            .all(|repo| { repo.checkouts.len() == 1 && !repo.checkouts[0].is_missing }));
    }
}
