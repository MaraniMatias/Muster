use serde::Serialize;

/// One OpenCode session Muster knows about, scoped to the checkout that owns it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentSession {
    /// OpenCode's own `ses_…` id, only ever valid inside its checkout's server.
    pub id: String,
    pub checkout_id: String,
    pub title: String,
    /// When the server last saw this session go idle, if ever.
    ///
    /// Absent means "no turn has finished yet", which covers both a session that never ran
    /// and one that is working right now. `/api/session/active` reports the latter separately;
    /// this remains the timestamp fact rather than a derived verdict.
    pub idle_at: Option<i64>,
    /// A pending reply; None means a candidate location could not be read. Scoped reads fail instead.
    pub awaiting_reply: Option<bool>,
    /// The agent running this session, and therefore the mode it is in: OpenCode spells
    /// `build`/`plan` as agents, so one name answers both questions.
    pub agent: Option<String>,
    /// The model behind it, as `provider/id#variant`.
    pub model: Option<String>,
    /// The session this one was spawned from, which is how a subagent is tied to its parent.
    pub parent_id: Option<String>,
    /// The service reports this session as draining a turn right now.
    ///
    /// This is the server's own answer from `/api/session/active`, so it covers a turn a person
    /// started in their own TUI, which no event this client watched would ever announce.
    pub running: bool,
    /// How the last turn ended: `succeeded`, `failed` or `interrupted`.
    pub outcome: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One session that is working in a different worktree than it was the last time it was read.
///
/// OpenCode reports the directory every session is in and changes it when an agent is sent to
/// work somewhere else, which is what the TUI shows at its footer. Two checkouts of this app's
/// workspace on either side is what makes that a relocation rather than a session that happens
/// to live in another project.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentRelocation {
    pub session_id: String,
    pub from_checkout_id: String,
    pub to_checkout_id: String,
    /// When this relocation was first observed, in Unix milliseconds. Replays keep the same value.
    pub observed_at: i64,
}

/// One agent a checkout's server offers, as OpenCode describes it.
///
/// The color is OpenCode's own, so a row painted with it matches what the user sees in the
/// TUI for the same agent.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentAgent {
    pub id: String,
    pub name: String,
    /// `primary` is a mode a person picks, `subagent` is one a parent spawns, `all` is both.
    pub mode: String,
    /// The hex OpenCode paints this agent with, absent when it has no color of its own.
    pub color: Option<String>,
    /// Internal agents: real, and not something to offer as a choice.
    pub hidden: bool,
}

/// A normalized event from the checkout's server, with the server's own payload
/// kept intact so the UI can render tool names and permission resources.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentEvent {
    pub checkout_id: String,
    pub session_id: Option<String>,
    /// Normalized kind the UI branches on; see `agent_event_kind`.
    pub kind: AgentEventKind,
    /// The original SSE `type`, kept for diagnostics and unknown-event passthrough.
    pub raw_type: String,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AgentEventKind {
    /// Anything Muster does not model yet. The UI ignores it safely.
    Unknown,
    /// A turn started: `session.execution.started` or a new `session.step.started`.
    TurnStarted,
    /// A turn ended successfully.
    TurnFinished,
    /// A turn ended in an error. `data` carries the server's `error` object.
    TurnFailed,
    /// A tool was invoked. `data` carries `name` and `input`.
    ToolCalled,
    /// A permission was requested.
    PermissionAsked,
    /// A question was asked.
    QuestionAsked,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub checkout_id: String,
    pub running: bool,
    pub sessions: Vec<AgentSession>,
    /// Why the bridge is not running, when it is not.
    pub error: String,
}

impl AgentStatus {
    pub fn stopped(checkout_id: &str, error: &str) -> Self {
        Self {
            checkout_id: checkout_id.to_string(),
            running: false,
            sessions: Vec::new(),
            error: error.to_string(),
        }
    }
}

/// Maps a raw SSE type to the kind the UI branches on.
///
/// v2.0.18 has no `session.execution.completed`, only the `*.failed` variants, and no
/// `session.idle` event at all: idleness is `time.idle` on the session object. So a turn
/// is "finished" only when the server says so, and Muster must not infer it from silence.
pub fn agent_event_kind(raw_type: &str) -> AgentEventKind {
    match raw_type {
        "session.execution.started" | "session.step.started" => AgentEventKind::TurnStarted,
        "session.execution.completed" | "session.step.completed" | "session.idle" => {
            AgentEventKind::TurnFinished
        }
        "session.execution.failed" | "session.step.failed" => AgentEventKind::TurnFailed,
        "session.tool.called" => AgentEventKind::ToolCalled,
        "permission.asked" => AgentEventKind::PermissionAsked,
        "question.asked" => AgentEventKind::QuestionAsked,
        _ => AgentEventKind::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::{agent_event_kind, AgentEventKind};

    #[test]
    fn maps_the_observed_turn_lifecycle() {
        assert_eq!(
            agent_event_kind("session.execution.started"),
            AgentEventKind::TurnStarted
        );
        assert_eq!(
            agent_event_kind("session.step.started"),
            AgentEventKind::TurnStarted
        );
        assert_eq!(
            agent_event_kind("session.tool.called"),
            AgentEventKind::ToolCalled
        );
        assert_eq!(
            agent_event_kind("permission.asked"),
            AgentEventKind::PermissionAsked
        );
        assert_eq!(
            agent_event_kind("question.asked"),
            AgentEventKind::QuestionAsked
        );
        assert_eq!(
            agent_event_kind("session.execution.failed"),
            AgentEventKind::TurnFailed
        );
        assert_eq!(
            agent_event_kind("session.step.failed"),
            AgentEventKind::TurnFailed
        );
    }

    #[test]
    fn ignores_types_muster_does_not_model() {
        for raw in [
            "server.connected",
            "mcp.status.changed",
            "session.inbox.enqueued",
            "session.text.delta",
            "session.instructions.updated",
            "project.updated",
        ] {
            assert_eq!(agent_event_kind(raw), AgentEventKind::Unknown, "{raw}");
        }
    }
}
