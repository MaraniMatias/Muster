//! What the service says about its sessions, its agents, and what they are doing.
//!
//! The service is another program on its own release cadence, so every field here is
//! tolerance of a third party's wire rather than a contract this app keeps with it: a
//! session with no agent, no model and no timestamps yet is still a session, and reading
//! it as one keeps the panel painting instead of turning a half-answered row into an
//! error.
//!
//! What is *not* tolerated is ambiguity. A session that cannot be named exactly one way
//! is not named at all, and a pending state that could not be read is unknown rather than
//! idle: `Some(false)` would clear a row that is in fact waiting for an answer.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::Deserialize;

use super::{ApiModel, ApiSession, Client, DispatchError};

/// How many sessions the service-wide read asks for.
///
/// The route answers newest-first and stops at fifty on its own whatever this says, so
/// the number is a request rather than a promise: a service that honours it would only
/// widen the window.
const SESSION_LIST_LIMIT: &str = "200";

/// How many locations the pending lists are read for in one pass.
///
/// Every location is two more requests. A rotating starting point gives even a location
/// beyond this slice a turn on later polls. Omitted and unreadable locations are unknown.
const MAX_PENDING_DIRECTORIES: usize = 8;

/// One entry of `GET /api/session/active`, keyed by session id.
///
/// `running` is the only value the route writes, and it is the whole point of the
/// route: a session that is absent is one that is not working.
#[derive(Debug, Deserialize)]
struct ApiActiveSession {
    #[serde(default, rename = "type")]
    kind: String,
}

/// A pending form or a pending permission, which names the session it is about.
#[derive(Debug, Deserialize)]
struct ApiPendingRequest {
    #[serde(rename = "sessionID")]
    session_id: String,
}

/// One entry of `GET /api/agent`, which is where the colour an agent is painted with
/// lives.
#[derive(Debug, Deserialize)]
struct ApiAgent {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    hidden: bool,
}

/// One session as the service describes it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentSession {
    /// The session's own id, `ses_…`.
    pub id: String,
    /// The title its TUI writes into the terminal, and what a row is matched on.
    pub title: String,
    /// The agent running it, named by the catalog.
    pub agent: Option<String>,
    /// The session this one is a child of, when it is a subagent.
    pub parent_id: Option<String>,
    /// How the last turn ended: `succeeded`, `failed`, `interrupted`.
    pub outcome: Option<String>,
    /// The model as one string, in the `provider/id#variant` form.
    pub model: Option<String>,
    /// Whether the service is draining a turn for it right now.
    pub running: bool,
    /// Whether it is waiting for a question or a permission.
    ///
    /// `None` when the pending state could not be read, which is a different thing from
    /// `Some(false)`: one is "not waiting" and the other is "nobody could say".
    pub awaiting_reply: Option<bool>,
    /// When the session was created, in epoch milliseconds.
    pub created_at: i64,
    /// When the service last touched it, in epoch milliseconds.
    pub updated_at: i64,
    /// When it last went idle, in epoch milliseconds.
    pub idle_at: Option<i64>,
    /// Where the session lives, when the service said.
    pub directory: Option<PathBuf>,
}

impl AgentSession {
    /// The title a picker shows. A session the service gave no title is still a session,
    /// and a row that says nothing about it names nothing either.
    pub fn display_title(&self) -> &str {
        if self.title.is_empty() {
            "OpenCode session"
        } else {
            &self.title
        }
    }
}

/// One agent the service offers, with the colour it paints that agent with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentAgent {
    pub id: String,
    pub name: String,
    pub mode: String,
    /// The colour the service gave, as a hex string, when it gave one this app can paint.
    pub color: Option<String>,
    pub hidden: bool,
}

/// How loudly a session wants for attention.
///
/// A pending request outranks a running turn: a session waiting for an answer is not
/// working, whatever else the service says about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attention {
    /// Pending requests could not be read; this is not evidence of an idle session.
    Unknown,
    /// Nothing is happening and nothing failed.
    Idle,
    /// A turn is running.
    Busy,
    /// A question or a permission is waiting for the person.
    Asking,
    /// The last turn failed.
    Failed,
}

impl Attention {
    /// Whether this state is one that moves.
    pub fn is_busy(self) -> bool {
        matches!(self, Self::Busy)
    }
}

impl AgentSession {
    /// What this session wants for attention.
    ///
    /// A known running turn stays busy even when pending requests could not be read.
    /// Otherwise an unread pending state is unknown, never idle.
    pub fn attention(&self) -> Attention {
        if self.awaiting_reply == Some(true) {
            return Attention::Asking;
        }
        if self.running {
            return Attention::Busy;
        }
        if self.awaiting_reply.is_none() {
            return Attention::Unknown;
        }
        if self.outcome.as_deref() == Some("failed") {
            return Attention::Failed;
        }
        Attention::Idle
    }
}

/// The name the row shows for a session's agent.
///
/// The catalog's name when it has one, and the agent's own id when it does not: an id is
/// not a nice name, but it is the truth, and a row with no name is a row that cannot be
/// told apart from another one.
pub fn agent_label(agents: &[AgentAgent], agent_id: Option<&str>) -> Option<String> {
    let agent_id = agent_id.filter(|id| !id.is_empty())?;
    let agent = agents.iter().find(|agent| agent.id == agent_id);
    Some(match agent {
        Some(agent) if !agent.name.is_empty() => agent.name.clone(),
        _ => agent_id.to_string(),
    })
}

/// The colour the service paints an agent with, when it painted one this app can paint.
///
/// A colour the service owns is not a colour this app trusts: it is a string on a wire
/// that becomes a tint in a window, so it is checked for being a colour at all before it
/// is handed back. A colour that is not one is `None`, and the row wears the theme's own
/// accent instead.
pub fn agent_color(agents: &[AgentAgent], agent_id: Option<&str>) -> Option<String> {
    let agent_id = agent_id.filter(|id| !id.is_empty())?;
    let color = agents
        .iter()
        .find(|agent| agent.id == agent_id)
        .and_then(|agent| agent.color.clone())?;
    is_hex_color(&color).then_some(color)
}

/// Whether a string is a hex colour this app will paint: `#RGB`, `#RRGGBB` or `#RRGGBBAA`.
pub fn is_hex_color(color: &str) -> bool {
    let Some(digits) = color.strip_prefix('#') else {
        return false;
    };
    matches!(digits.len(), 3 | 6 | 8) && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// How long ago the service last touched a session, in whole seconds.
///
/// `None` when the service never said, or said something this app cannot read as a
/// moment: a duration drawn from a number that is not a duration is a number that lies.
pub fn elapsed_seconds(updated_at: i64, now_millis: i64) -> Option<u64> {
    if updated_at <= 0 {
        return None;
    }
    Some(((now_millis - updated_at).max(0) / 1000) as u64)
}

/// The epoch milliseconds right now, as the service counts time.
pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        .unwrap_or_default()
}

/// The session of a list that a terminal's title names, when exactly one of them does.
///
/// A title that names several sessions names none of them: borrowing another session's
/// state is how a row ends up saying something true about a different conversation.
pub fn match_session_title<'a>(sessions: &'a [AgentSession], title: &str) -> MatchedSession<'a> {
    let exact: Vec<&AgentSession> = sessions
        .iter()
        .filter(|session| session.title == title)
        .collect();
    if !exact.is_empty() {
        return if exact.len() == 1 {
            MatchedSession::One(exact[0])
        } else {
            MatchedSession::Ambiguous
        };
    }
    // A truncated title is one the TUI shortened with an ellipsis. The prefix still
    // identifies a session when only one of them carries it.
    let Some(stem) = title.strip_suffix('…') else {
        return MatchedSession::Unknown;
    };
    let matches: Vec<&AgentSession> = sessions
        .iter()
        .filter(|session| session.title.starts_with(stem))
        .collect();
    match matches.len() {
        1 => MatchedSession::One(matches[0]),
        0 => MatchedSession::Unknown,
        _ => MatchedSession::Ambiguous,
    }
}

/// What a terminal's title said about the session it is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchedSession<'a> {
    /// Exactly one session carries the title.
    One(&'a AgentSession),
    /// More than one does, so none of them may be drawn.
    Ambiguous,
    /// None does.
    Unknown,
}

impl<'a> MatchedSession<'a> {
    /// The session, when exactly one of them was named.
    pub fn one(self) -> Option<&'a AgentSession> {
        match self {
            Self::One(session) => Some(session),
            Self::Ambiguous | Self::Unknown => None,
        }
    }

    /// Whether nothing was named, either because no session carried the title or
    /// because several did.
    pub fn is_unnamed(self) -> bool {
        !matches!(self, Self::One(_))
    }
}

/// The sessions whose turn was seen to finish.
///
/// A turn ends in silence: the service announces no completion, so the only way a
/// finished turn is noticed is by reading again and seeing that a session that was
/// working is not working any more. `was_running` is that record of what the previous
/// read said, and it is the caller's to keep.
///
/// A session that is waiting for a question is not finished: it is still the same turn,
/// stopped at a question. So a session that is running **or** awaiting a reply stays in
/// the record, and only leaving both is a completion.
///
/// The service caps its list. An omitted session is not proof of idle and retains
/// its history until a later positive idle reading.
pub fn settled_turns(sessions: &[AgentSession], was_running: &mut HashSet<String>) -> Vec<String> {
    let mut settled = Vec::new();
    for session in sessions {
        if session.running || session.awaiting_reply == Some(true) {
            was_running.insert(session.id.clone());
        } else if session.awaiting_reply == Some(false) && was_running.remove(&session.id) {
            settled.push(session.id.clone());
        }
    }
    settled
}

/// The title an OpenCode TUI writes into the terminal it runs in.
///
/// The prefix is the TUI's own: a terminal that is not running OpenCode does not write
/// one, and neither does the OpenCode home screen, whose title names the program rather
/// than a session.
pub fn terminal_session_title(title: &str) -> Option<&str> {
    let session = title.strip_prefix("OC | ")?.trim();
    (!session.is_empty() && session != "OpenCode").then_some(session)
}

impl Client {
    /// Every session the service lists, wherever it lives.
    ///
    /// The list is read without a directory, and that is the whole reason this read
    /// exists: a session that moved to another worktree is not listed by the directory it
    /// left, so a scoped read cannot name the session a terminal is showing, and the row
    /// that would say so draws no state at all.
    ///
    /// A location whose pending requests could not be read leaves its sessions
    /// **unknown** rather than idle: a question this app could not see is not a question
    /// that was not asked.
    pub fn service_sessions(&self) -> Result<Vec<AgentSession>, DispatchError> {
        static PENDING_CURSOR: AtomicUsize = AtomicUsize::new(0);
        self.service_sessions_from(PENDING_CURSOR.fetch_add(1, Ordering::Relaxed))
    }

    /// A window-owned rotating slice. Advance `start` by one on every poll so several
    /// windows cannot accidentally lock one another to the same subset of locations.
    pub fn service_sessions_from(&self, start: usize) -> Result<Vec<AgentSession>, DispatchError> {
        // A scoped caller can use this read too, but it must not inherit thirty seconds
        // per endpoint. The same deadline is shared by every request in this pass.
        let budget = self
            .read_budget
            .clone()
            .unwrap_or_else(|| super::ReadBudget::new(super::OBSERVATION_TIMEOUT));
        let client = self.clone().with_read_budget(budget.clone());
        let listed: Vec<ApiSession> =
            client.get_json_unscoped(&format!("/api/session?limit={SESSION_LIST_LIMIT}"))?;
        for raw in &listed {
            super::validate_session_id(&raw.id)?;
        }
        let running = client.running_sessions()?;
        let mut directories: Vec<PathBuf> = Vec::new();
        for raw in &listed {
            let Some(location) = &raw.location else {
                continue;
            };
            let directory = PathBuf::from(&location.directory);
            if !directories.contains(&directory) {
                directories.push(directory);
            }
        }
        // Advance by one, not by the slice length: even when a slow first location
        // spends the deadline, every location eventually becomes the first one read.
        directories.sort();
        if !directories.is_empty() {
            let count = directories.len();
            directories.rotate_left(start % count);
        }
        directories.truncate(MAX_PENDING_DIRECTORIES);
        let mut awaiting: HashMap<PathBuf, HashSet<String>> = HashMap::new();
        for directory in &directories {
            // A slow location cannot spend the whole snapshot before another gets a
            // turn. Both pending routes share this slice and the outer deadline.
            let pending_client = client
                .clone()
                .with_read_budget(budget.slice(std::time::Duration::from_millis(250)));
            if let Ok(sessions) = pending_client.pending_sessions(directory) {
                awaiting.insert(directory.clone(), sessions);
            }
        }
        Ok(listed
            .into_iter()
            .map(|raw| {
                let directory = raw
                    .location
                    .as_ref()
                    .map(|location| PathBuf::from(&location.directory));
                let awaiting_reply = directory
                    .as_ref()
                    .and_then(|directory| awaiting.get(directory))
                    .map(|sessions| sessions.contains(&raw.id));
                let running = running.contains(&raw.id);
                into_session(raw, running, awaiting_reply)
            })
            .collect())
    }

    /// The sessions one directory holds.
    ///
    /// This is the list a review round is about: a session in another worktree is not a
    /// place this checkout's review goes.
    pub fn directory_sessions(&self, directory: &Path) -> Result<Vec<AgentSession>, DispatchError> {
        let listed: Vec<ApiSession> = self.get_json_for("/api/session", directory)?;
        for raw in &listed {
            super::validate_session_id(&raw.id)?;
            super::check_scope(raw.location.as_ref(), directory)?;
        }
        let running = self.running_sessions_for(directory)?;
        let pending = self.pending_sessions(directory);
        Ok(listed
            .into_iter()
            .map(|raw| {
                let awaiting_reply = pending
                    .as_ref()
                    .ok()
                    .map(|sessions| sessions.contains(&raw.id));
                let running = running.contains(&raw.id);
                into_session(raw, running, awaiting_reply)
            })
            .collect())
    }

    /// The agents one directory offers, with the colour the service paints them with.
    ///
    /// The catalog answers for a directory, so one project's agents are never handed to
    /// another project's row.
    pub fn agents(&self, directory: &Path) -> Result<Vec<AgentAgent>, DispatchError> {
        let listed: Vec<ApiAgent> = self.get_json_for("/api/agent", directory)?;
        Ok(listed
            .into_iter()
            .map(|raw| AgentAgent {
                id: raw.id,
                name: raw.name,
                mode: raw.mode,
                color: raw.color,
                hidden: raw.hidden,
            })
            .collect())
    }

    /// Everything the session was told, as one searchable document.
    ///
    /// The route answers JSON and the message shape differs between OpenCode versions,
    /// so the payload is handed back as it arrived rather than modelled: the questions
    /// asked of it are whether a marker is in it and where its completed work happened.
    pub fn session_messages(&self, session_id: &str) -> Result<serde_json::Value, DispatchError> {
        super::validate_session_id(session_id)?;
        if self.scoped {
            self.resolve_session(session_id)?;
        }
        self.get_json(&format!("/api/session/{session_id}/message"))
    }

    /// The sessions the service is draining a turn for.
    ///
    /// The route answers for the whole service rather than for one connection, so one
    /// read says which of them are working anywhere, which is the only way a turn
    /// started in somebody's own terminal is ever seen.
    fn running_sessions(&self) -> Result<HashSet<String>, DispatchError> {
        let active: HashMap<String, ApiActiveSession> =
            self.get_json_unscoped("/api/session/active")?;
        Ok(active
            .into_iter()
            .filter(|(_, session)| session.kind == "running")
            .map(|(session_id, _)| session_id)
            .collect())
    }

    /// The same answer, asked of one location.
    fn running_sessions_for(&self, directory: &Path) -> Result<HashSet<String>, DispatchError> {
        let active: HashMap<String, ApiActiveSession> =
            self.get_json_for("/api/session/active", directory)?;
        Ok(active
            .into_iter()
            .filter(|(_, session)| session.kind == "running")
            .map(|(session_id, _)| session_id)
            .collect())
    }

    /// The sessions waiting for a question or a permission in one location.
    ///
    /// Both lists answer for one location, so a location that cannot be read is a
    /// location whose sessions are unknown rather than idle.
    fn pending_sessions(&self, directory: &Path) -> Result<HashSet<String>, DispatchError> {
        let mut pending = HashSet::new();
        for path in ["/api/form", "/api/permission/request"] {
            let requests: Vec<ApiPendingRequest> = self.get_json_for(path, directory)?;
            pending.extend(requests.into_iter().map(|request| request.session_id));
        }
        Ok(pending)
    }
}

/// One session as the app reads it.
fn into_session(raw: ApiSession, running: bool, awaiting_reply: Option<bool>) -> AgentSession {
    AgentSession {
        id: raw.id,
        title: raw.title,
        agent: raw.agent,
        parent_id: raw.parent_id,
        outcome: raw.outcome,
        model: raw.model.as_ref().map(ApiModel::label),
        running,
        awaiting_reply,
        created_at: raw.time.created,
        updated_at: raw.time.updated,
        idle_at: raw.time.idle,
        directory: raw
            .location
            .as_ref()
            .map(|location| PathBuf::from(&location.directory)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, title: &str) -> AgentSession {
        AgentSession {
            id: id.into(),
            title: title.into(),
            awaiting_reply: Some(false),
            ..AgentSession::default()
        }
    }

    fn agent(id: &str, name: &str, color: Option<&str>) -> AgentAgent {
        AgentAgent {
            id: id.into(),
            name: name.into(),
            mode: "primary".into(),
            color: color.map(str::to_string),
            hidden: false,
        }
    }

    #[test]
    fn a_pending_request_outranks_a_running_turn() {
        let mut busy = session("ses_a", "one");
        busy.running = true;
        assert_eq!(busy.attention(), Attention::Busy);

        let mut asking = busy.clone();
        asking.awaiting_reply = Some(true);
        assert_eq!(asking.attention(), Attention::Asking);
    }

    #[test]
    fn a_failed_turn_is_a_state_of_its_own() {
        let mut failed = session("ses_a", "one");
        failed.outcome = Some("failed".into());
        assert_eq!(failed.attention(), Attention::Failed);

        failed.running = true;
        assert_eq!(failed.attention(), Attention::Busy);
    }

    #[test]
    fn a_pending_state_that_could_not_be_read_is_not_an_answer() {
        let mut unknown = session("ses_a", "one");
        unknown.awaiting_reply = None;
        assert_eq!(unknown.attention(), Attention::Unknown);
        unknown.running = true;
        assert_eq!(unknown.attention(), Attention::Busy);
    }

    #[test]
    fn an_agent_is_named_by_its_catalog_name_or_by_its_own_id() {
        let agents = vec![agent("build", "Build", None), agent("plan", "", None)];
        assert_eq!(
            agent_label(&agents, Some("build")).as_deref(),
            Some("Build")
        );
        assert_eq!(agent_label(&agents, Some("plan")).as_deref(), Some("plan"));
        assert_eq!(
            agent_label(&agents, Some("other")).as_deref(),
            Some("other")
        );
        assert_eq!(agent_label(&agents, None), None);
    }

    #[test]
    fn a_colour_the_service_gave_is_only_used_when_it_is_a_colour() {
        let agents = vec![
            agent("build", "Build", Some("#5c7cfa")),
            agent("plan", "Plan", Some("rebeccapurple")),
            agent("test", "Test", Some("#abc")),
        ];
        assert_eq!(
            agent_color(&agents, Some("build")).as_deref(),
            Some("#5c7cfa")
        );
        assert_eq!(agent_color(&agents, Some("plan")), None);
        assert_eq!(agent_color(&agents, Some("test")).as_deref(), Some("#abc"));
        assert_eq!(agent_color(&agents, Some("other")), None);
    }

    #[test]
    fn exactly_one_session_named_by_a_title_is_the_session_and_two_are_none() {
        let sessions = vec![
            session("ses_a", "fix the parser"),
            session("ses_b", "other"),
        ];
        assert!(matches!(
            match_session_title(&sessions, "fix the parser"),
            MatchedSession::One(_)
        ));
        assert_eq!(
            match_session_title(&sessions, "fix the pars"),
            MatchedSession::Unknown
        );
    }

    #[test]
    fn two_sessions_with_the_same_title_name_none_of_them() {
        let sessions = vec![session("ses_a", "same"), session("ses_b", "same")];
        assert_eq!(
            match_session_title(&sessions, "same"),
            MatchedSession::Ambiguous
        );
    }

    #[test]
    fn a_truncated_title_names_the_one_session_that_carries_its_prefix() {
        let sessions = vec![
            session("ses_a", "a long title"),
            session("ses_b", "another"),
        ];
        assert!(matches!(
            match_session_title(&sessions, "a long ti…"),
            MatchedSession::One(_)
        ));
        assert_eq!(
            match_session_title(&sessions, "nothing…"),
            MatchedSession::Unknown
        );
    }

    #[test]
    fn a_truncated_title_that_several_sessions_carry_names_none_of_them() {
        let sessions = vec![
            session("ses_a", "same long title"),
            session("ses_b", "same long"),
        ];
        assert_eq!(
            match_session_title(&sessions, "same…"),
            MatchedSession::Ambiguous
        );
    }

    #[test]
    fn the_title_a_tui_writes_is_the_one_after_its_own_prefix() {
        assert_eq!(
            terminal_session_title("OC | fix the parser"),
            Some("fix the parser")
        );
        assert_eq!(terminal_session_title("OC | OpenCode"), None);
        assert_eq!(terminal_session_title("OC | "), None);
        assert_eq!(terminal_session_title("fix the parser"), None);
    }

    #[test]
    fn a_duration_is_only_drawn_from_a_number_that_is_one() {
        assert_eq!(elapsed_seconds(1_000, 61_000), Some(60));
        assert_eq!(elapsed_seconds(0, 61_000), None);
        assert_eq!(elapsed_seconds(1_000, 500), Some(0));
    }

    #[test]
    fn a_turn_is_finished_when_it_stops_being_a_turn_and_not_before() {
        let mut running = session("ses_a", "one");
        running.running = true;
        let mut was_running = HashSet::new();
        assert!(settled_turns(&[running.clone()], &mut was_running).is_empty());

        // Still the same turn, stopped at a question.
        let mut asking = running.clone();
        asking.running = false;
        asking.awaiting_reply = Some(true);
        assert!(settled_turns(&[asking], &mut was_running).is_empty());

        // The turn is over.
        let idle = session("ses_a", "one");
        assert_eq!(settled_turns(&[idle], &mut was_running), vec!["ses_a"]);
        assert!(was_running.is_empty());
    }

    #[test]
    fn an_unknown_pending_read_keeps_running_history_until_positive_idle() {
        let mut running = session("ses_a", "one");
        running.running = true;
        let mut was_running = HashSet::new();
        assert!(settled_turns(&[running], &mut was_running).is_empty());
        let mut unknown = session("ses_a", "one");
        unknown.awaiting_reply = None;
        assert!(settled_turns(&[unknown.clone()], &mut was_running).is_empty());
        assert!(was_running.contains("ses_a"));
        assert!(settled_turns(&[unknown], &mut was_running).is_empty());
        assert_eq!(
            settled_turns(&[session("ses_a", "one")], &mut was_running),
            vec!["ses_a"]
        );
    }

    fn home_for(port: u16) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join(".local/state/opencode");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("service.json"),
            serde_json::json!({
                "url": format!("http://127.0.0.1:{port}"),
                "password": "secret",
                "version": "2.0.22",
            })
            .to_string(),
        )
        .unwrap();
        home
    }

    #[test]
    fn unscoped_located_envelopes_and_pending_slices_are_read_on_the_real_wire() {
        use crate::opencode::mock::{self, Mock};
        let mock =
            Mock::start(|request| {
                let (data, location) =
                    match request.path.as_str() {
                        "/api/info" => (serde_json::json!({}), "/service".to_string()),
                        "/api/session" => {
                            assert!(request.header("x-opencode-directory").is_none());
                            assert!(request.query_param("directory").is_none());
                            (serde_json::Value::Array((0..11).map(|index| serde_json::json!({
                        "id": format!("ses_{index}"),
                        "location": {"directory": format!("/work/{index:02}")},
                    })).collect()), "/service".to_string())
                        }
                        "/api/session/active" => {
                            assert!(request.header("x-opencode-directory").is_none());
                            (
                                serde_json::json!({"ses_10": {"type": "running"}}),
                                "/service".to_string(),
                            )
                        }
                        "/api/session/ses_10/message" => {
                            assert!(request.header("x-opencode-directory").is_none());
                            (serde_json::json!([]), "/work/10".to_string())
                        }
                        "/api/form" | "/api/permission/request" => {
                            let directory = request.query_param("directory").unwrap();
                            assert_eq!(
                                request.header("x-opencode-directory"),
                                Some(directory.as_str())
                            );
                            if directory == "/work/03" {
                                return mock::reply(500, serde_json::json!({}));
                            }
                            (serde_json::json!([]), directory)
                        }
                        _ => return mock::reply(404, serde_json::json!({})),
                    };
                mock::reply(
                    200,
                    serde_json::json!({"data": data, "location": {"directory": location}}),
                )
            });
        let home = home_for(mock.port());
        let mut seen_known = HashSet::new();
        // Reconnect each time, as the production watch does. A cursor kept only on the
        // Client would restart at the first eight directories and fail this check.
        for start in 0..12 {
            let client = Client::connect_service(home.path()).unwrap();
            let sessions = client.service_sessions_from(start).unwrap();
            assert_eq!(sessions.len(), 11);
            assert!(
                sessions
                    .iter()
                    .find(|session| session.id == "ses_10")
                    .unwrap()
                    .running
            );
            assert!(
                sessions
                    .iter()
                    .filter(|session| session.awaiting_reply.is_none())
                    .count()
                    >= 3
            );
            assert_eq!(
                sessions
                    .iter()
                    .find(|session| session.id == "ses_3")
                    .unwrap()
                    .awaiting_reply,
                None
            );
            for session in sessions {
                if session.awaiting_reply == Some(false) {
                    seen_known.insert(session.id);
                }
            }
            assert_eq!(
                client.session_messages("ses_10").unwrap(),
                serde_json::json!([])
            );
        }
        assert_eq!(
            seen_known.len(),
            10,
            "every readable location gets a turn, including those beyond eight"
        );
        assert!(!seen_known.contains("ses_3"));
    }

    #[test]
    fn a_scoped_list_rejects_a_foreign_session_inside_a_correct_envelope() {
        use crate::opencode::mock::{self, Mock};
        let mock = Mock::start(|request| {
            let data = if request.path == "/api/info" {
                serde_json::json!({})
            } else {
                serde_json::json!([{"id": "ses_foreign", "location": {"directory": "/foreign"}}])
            };
            mock::reply(
                200,
                serde_json::json!({"data": data, "location": {"directory": "/expected"}}),
            )
        });
        let home = home_for(mock.port());
        let client = Client::connect_service(home.path()).unwrap();
        assert!(matches!(
            client.directory_sessions(Path::new("/expected")),
            Err(DispatchError::Foreign(_))
        ));
        assert_eq!(
            mock.requests().len(),
            2,
            "foreign items fail before activity or pending reads"
        );
    }

    #[test]
    fn cancellation_between_pending_calls_prevents_further_network_requests() {
        use crate::opencode::{
            mock::{self, Mock},
            ReadBudget, OBSERVATION_TIMEOUT,
        };
        let budget = ReadBudget::new(OBSERVATION_TIMEOUT);
        let cancel = budget.clone();
        let mock = Mock::start(move |request| {
            let data = match request.path.as_str() {
                "/api/info" | "/api/session/active" => serde_json::json!({}),
                "/api/session" => {
                    serde_json::json!([{"id": "ses_one", "location": {"directory": "/one"}}])
                }
                "/api/form" => {
                    cancel.cancel();
                    serde_json::json!([])
                }
                _ => panic!("a cancelled read reached {}", request.path),
            };
            mock::reply(200, serde_json::json!({"data": data}))
        });
        let home = home_for(mock.port());
        let client = Client::connect_service_with_budget(home.path(), budget).unwrap();
        let sessions = client.service_sessions().unwrap();
        assert_eq!(sessions[0].awaiting_reply, None);
        assert!(client.agents(Path::new("/one")).is_err());
        assert_eq!(mock.requests().len(), 4);
    }

    #[test]
    fn one_deadline_bounds_the_whole_snapshot_not_each_of_its_requests() {
        use crate::opencode::{
            mock::{self, Mock},
            ReadBudget,
        };
        use std::time::{Duration, Instant};
        let mock = Mock::start(|request| {
            let data = match request.path.as_str() {
                "/api/info" | "/api/session/active" => serde_json::json!({}),
                "/api/session" => {
                    serde_json::json!([{"id": "ses_one", "location": {"directory": "/one"}}])
                }
                _ => serde_json::json!([]),
            };
            std::thread::sleep(Duration::from_millis(60));
            mock::reply(200, serde_json::json!({"data": data}))
        });
        let home = home_for(mock.port());
        let start = Instant::now();
        let budget = ReadBudget::new(Duration::from_millis(210));
        let client = Client::connect_service_with_budget(home.path(), budget).unwrap();
        let sessions = client.service_sessions();
        assert!(sessions.is_err() || sessions.unwrap()[0].awaiting_reply.is_none());
        assert!(client.agents(Path::new("/one")).is_err());
        assert!(
            start.elapsed() < Duration::from_millis(350),
            "the deadline restarted for every request: {:?}",
            start.elapsed()
        );
        assert!(mock.requests().len() <= 4);
    }

    #[test]
    fn an_omitted_session_is_not_a_finished_turn_or_proof_of_idle() {
        let mut running = session("ses_a", "one");
        running.running = true;
        let mut was_running = HashSet::new();
        settled_turns(&[running], &mut was_running);
        assert!(settled_turns(&[], &mut was_running).is_empty());
        assert!(was_running.contains("ses_a"));
    }
}
