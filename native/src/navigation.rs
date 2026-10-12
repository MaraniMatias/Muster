//! Native navigation state. Live terminal entities belong to the window, not this model.
//! Discovery merges membership without replacing newer selection or live session order.

use std::collections::{HashMap, HashSet};

use muster_core::navigation::{NavigationState, OpenedDirectory};
use muster_core::workspace::{Checkout, Repo, RepoKind, Session};

#[derive(Default)]
pub(crate) struct Navigation {
    pub repos: Vec<Repo>,
    pub active_checkout: Option<String>,
    pub active_session: Option<String>,
    pub home: Option<String>,
    pub archived: HashSet<String>,
    pub folded_repos: HashSet<String>,
    pub folded_checkouts: HashSet<String>,
    pub selected_by_checkout: HashMap<String, String>,
    pub pending: HashMap<String, String>,
    pub revision: u64,
    pub sidebar_width: f32,
    pub panels_visible: bool,
}

impl Navigation {
    pub fn new() -> Self {
        Self {
            sidebar_width: 240.,
            panels_visible: true,
            ..Self::default()
        }
    }

    pub fn checkout(&self, id: &str) -> Option<&Checkout> {
        self.repos
            .iter()
            .flat_map(|repo| &repo.checkouts)
            .find(|item| item.id == id)
    }

    pub fn session(&self, id: &str) -> Option<&Session> {
        self.repos
            .iter()
            .flat_map(|repo| &repo.checkouts)
            .flat_map(|checkout| &checkout.sessions)
            .find(|item| item.id == id)
    }

    /// Background refresh only changes membership, never navigation. Keep a vanished
    /// checkout with its live sessions, reporting it missing instead of losing ownership.
    pub fn merge(&mut self, mut opened: OpenedDirectory, select: bool) {
        if let Some(previous) = self.repos.iter_mut().find(|repo| repo.id == opened.repo.id) {
            opened.repo.created_at.clone_from(&previous.created_at);
            for checkout in &mut opened.repo.checkouts {
                if let Some(old) = previous.checkouts.iter().find(|old| old.id == checkout.id) {
                    checkout.sessions.clone_from(&old.sessions);
                }
            }
            for old in &previous.checkouts {
                if !opened.repo.checkouts.iter().any(|item| item.id == old.id) {
                    let mut missing = old.clone();
                    missing.is_missing = true;
                    opened.repo.checkouts.push(missing);
                }
            }
            *previous = opened.repo;
        } else {
            self.repos.push(opened.repo);
        }
        if select && !self.archived.contains(&opened.checkout_id) {
            self.select_checkout(&opened.checkout_id);
        }
    }

    pub fn select_checkout(&mut self, id: &str) -> bool {
        let Some(checkout) = self.checkout(id) else {
            return false;
        };
        if checkout.is_missing || self.archived.contains(id) {
            return false;
        }
        let session = self
            .selected_by_checkout
            .get(id)
            .filter(|selected| {
                checkout
                    .sessions
                    .iter()
                    .any(|session| &session.id == *selected)
            })
            .cloned()
            .or_else(|| checkout.sessions.last().map(|session| session.id.clone()));
        self.active_checkout = Some(id.into());
        self.active_session = session;
        self.revision += 1;
        true
    }

    pub fn select_session(&mut self, id: &str) -> bool {
        let Some(session) = self.session(id) else {
            return false;
        };
        let checkout = session.checkout_id.clone();
        if !self.select_checkout(&checkout) {
            return false;
        }
        self.active_session = Some(id.into());
        self.selected_by_checkout.insert(checkout, id.into());
        true
    }

    pub fn add_session(&mut self, session: Session, requested_revision: Option<u64>) -> bool {
        let Some(checkout) = self
            .repos
            .iter_mut()
            .flat_map(|repo| &mut repo.checkouts)
            .find(|checkout| checkout.id == session.checkout_id && !checkout.is_missing)
        else {
            return false;
        };
        if self.archived.contains(&checkout.id) {
            return false;
        }
        let id = session.id.clone();
        checkout.sessions.push(session);
        if Some(self.revision) == requested_revision {
            self.select_session(&id);
        }
        true
    }

    pub fn remove_session(&mut self, id: &str) {
        for checkout in self.repos.iter_mut().flat_map(|repo| &mut repo.checkouts) {
            checkout.sessions.retain(|session| session.id != id);
        }
        self.selected_by_checkout
            .retain(|_, selected| selected != id);
        if self.active_session.as_deref() == Some(id) {
            self.active_session = self
                .active_checkout
                .as_deref()
                .and_then(|id| self.checkout(id))
                .and_then(|checkout| checkout.sessions.last())
                .map(|session| session.id.clone());
        }
    }

    pub fn rename(&mut self, id: &str, name: &str) -> Result<(), String> {
        let has_control = name.chars().any(char::is_control);
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 60 || has_control {
            return Err(
                "Terminal names must contain 1–60 characters, without control characters.".into(),
            );
        }
        let session = self
            .repos
            .iter_mut()
            .flat_map(|repo| &mut repo.checkouts)
            .flat_map(|checkout| &mut checkout.sessions)
            .find(|session| session.id == id)
            .ok_or("This terminal has already closed.")?;
        session.name = name.into();
        Ok(())
    }

    /// A row move does not imply a shell `cd`. The host must never feed directory
    /// changes to a foreground TUI, and reports the unchanged cwd separately.
    pub fn move_session(&mut self, id: &str, target: &str, index: usize) -> Result<(), String> {
        if !self
            .checkout(target)
            .is_some_and(|checkout| !checkout.is_missing)
            || self.archived.contains(target)
        {
            return Err("The destination directory is unavailable.".into());
        }
        let mut session = self
            .session(id)
            .cloned()
            .ok_or("This terminal has already closed.")?;
        self.remove_session(id);
        session.checkout_id = target.into();
        let destination = self
            .repos
            .iter_mut()
            .flat_map(|repo| &mut repo.checkouts)
            .find(|checkout| checkout.id == target)
            .expect("validated destination");
        destination
            .sessions
            .insert(index.min(destination.sessions.len()), session);
        self.select_session(id);
        Ok(())
    }

    pub fn archive(&mut self, id: &str) -> Result<(), String> {
        let checkout = self.checkout(id).ok_or("This worktree no longer exists.")?;
        if checkout.is_primary
            || checkout.is_missing
            || self.home.as_deref() == Some(id)
            || !checkout.sessions.is_empty()
            || self.pending.values().any(|pending| pending == id)
            || !self.repos.iter().any(|repo| {
                repo.kind == RepoKind::Git && repo.checkouts.iter().any(|item| item.id == id)
            })
        {
            return Err(
                "Only available worktrees without terminal sessions can be archived.".into(),
            );
        }
        self.archived.insert(id.into());
        if self.active_checkout.as_deref() == Some(id) {
            self.active_checkout = None;
            self.active_session = None;
        }
        self.revision += 1;
        Ok(())
    }

    pub fn persistent_state(&self) -> NavigationState {
        let mut archived_checkout_ids: Vec<_> = self.archived.iter().cloned().collect();
        archived_checkout_ids.sort();
        NavigationState {
            roots: self.repos.iter().map(|repo| repo.root.clone()).collect(),
            archived_checkout_ids,
            active_checkout_id: self.active_checkout.clone(),
            sidebar_width: self.sidebar_width,
            ..NavigationState::default()
        }
    }
}

pub(crate) fn checkout_title(repo: &Repo, checkout: &Checkout) -> String {
    checkout.branch.clone().unwrap_or_else(|| {
        if repo.kind == RepoKind::Plain {
            repo.name.clone()
        } else if checkout.is_primary {
            "Base".into()
        } else {
            checkout.path.clone()
        }
    })
}

pub(crate) fn session_title(name: &str, app: Option<&str>, title: Option<&str>) -> String {
    if app == Some("opencode") {
        if let Some(title) = title
            .and_then(|title| title.strip_prefix("OC | "))
            .map(str::trim)
            .filter(|title| !title.is_empty() && *title != "OpenCode")
        {
            return title.into();
        }
    }
    app.map(str::to_owned).unwrap_or_else(|| name.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use muster_core::workspace::{SessionStatus, SessionType};
    use tempfile::tempdir;

    fn fixture() -> (tempfile::TempDir, Navigation, String) {
        let dir = tempdir().unwrap();
        let repo = Repo::plain(dir.path(), "now").unwrap();
        let id = repo.checkouts[0].id.clone();
        let mut nav = Navigation::new();
        nav.merge(
            OpenedDirectory {
                repo,
                checkout_id: id.clone(),
            },
            true,
        );
        (dir, nav, id)
    }
    fn session(id: &str, checkout_id: &str) -> Session {
        Session {
            id: id.into(),
            checkout_id: checkout_id.into(),
            name: "sh".into(),
            created_at: "now".into(),
            session_type: SessionType::Shell,
            status: SessionStatus::Active,
        }
    }
    #[test]
    fn checkout_selection_does_not_create_terminals() {
        let (_dir, mut nav, id) = fixture();
        nav.select_checkout(&id);
        assert!(nav.checkout(&id).unwrap().sessions.is_empty());
        assert_eq!(nav.active_session, None);
    }
    #[test]
    fn stale_creation_does_not_steal_newer_navigation() {
        let (_dir, mut nav, id) = fixture();
        let revision = nav.revision;
        nav.select_checkout(&id);
        assert!(nav.add_session(session("one", &id), Some(revision)));
        assert_eq!(nav.active_session, None);
        assert_eq!(nav.checkout(&id).unwrap().sessions.len(), 1);
    }
    #[test]
    fn refresh_preserves_order_session_and_selection() {
        let (_dir, mut nav, id) = fixture();
        nav.add_session(session("one", &id), Some(nav.revision));
        nav.add_session(session("two", &id), Some(nav.revision));
        let mut repo = nav.repos[0].clone();
        repo.checkouts[0].sessions.clear();
        let revision = nav.revision;
        nav.merge(
            OpenedDirectory {
                repo,
                checkout_id: id.clone(),
            },
            false,
        );
        assert_eq!(nav.active_session.as_deref(), Some("two"));
        assert_eq!(nav.revision, revision);
        assert_eq!(
            nav.checkout(&id)
                .unwrap()
                .sessions
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
    }
    #[test]
    fn rename_is_validated_and_targets_identity() {
        let (_dir, mut nav, id) = fixture();
        nav.add_session(session("one", &id), Some(nav.revision));
        nav.rename("one", "  named  ").unwrap();
        assert_eq!(nav.session("one").unwrap().name, "named");
        for invalid in ["", "\ncontrol", &"x".repeat(61)] {
            assert!(nav.rename("one", invalid).is_err());
        }
        assert_eq!(nav.session("one").unwrap().name, "named");
    }
    #[test]
    fn reorder_and_close_use_drawn_order() {
        let (_dir, mut nav, id) = fixture();
        nav.add_session(session("one", &id), Some(nav.revision));
        nav.add_session(session("two", &id), Some(nav.revision));
        nav.move_session("two", &id, 0).unwrap();
        assert_eq!(nav.checkout(&id).unwrap().sessions[0].id, "two");
        nav.remove_session("two");
        assert_eq!(nav.active_session.as_deref(), Some("one"));
    }
    #[test]
    fn process_identity_gates_agent_title() {
        assert_eq!(session_title("renamed", None, Some("OC | old")), "renamed");
        assert_eq!(session_title("sh", Some("nvim"), Some("OC | old")), "nvim");
        assert_eq!(
            session_title("sh", Some("opencode"), Some("OC | OC | nested")),
            "OC | nested"
        );
        assert_eq!(
            session_title("sh", Some("opencode"), Some("OC | OpenCode")),
            "opencode"
        );
    }
    #[test]
    fn folds_and_hidden_panels_do_not_change_navigation_or_sessions() {
        let (_dir, mut nav, id) = fixture();
        nav.add_session(session("one", &id), Some(nav.revision));
        nav.folded_checkouts.insert(id.clone());
        nav.folded_repos.insert(nav.repos[0].id.clone());
        nav.panels_visible = false;
        assert_eq!(nav.active_session.as_deref(), Some("one"));
        assert_eq!(nav.checkout(&id).unwrap().sessions.len(), 1);
        assert_eq!(nav.persistent_state().sidebar_width, 240.);
    }
}
