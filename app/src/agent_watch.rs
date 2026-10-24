//! What the OpenCode service says, read in the background and drawn in the panel.
//!
//! The service belongs to whoever started it, so this app never starts one and never
//! ends one: it reads the registration, and a service that is not there leaves the rows
//! with no state, which is a state to wait in rather than a failure to report.
//!
//! Three things make the reading safe to draw.
//!
//! * **It is scoped.** A session belongs to a directory, and a session in another
//!   worktree is not this checkout's session. Every answer is checked against the
//!   directory that was asked about.
//! * **It is bounded.** Every request has a timeout, the session list is capped, and the
//!   pending lists are read for a fixed number of locations. A service that is wedged
//!   costs a tick, not the window.
//! * **It is guarded.** A read that was asked for one set of checkouts and answered
//!   after the window asked about another is dropped rather than drawn: a stale answer
//!   describes a world that is no longer on screen.
//!
//! The read itself never happens on the window's thread. It runs on the background
//! executor and comes back as a value, so a service that takes its time costs a tick of
//! the panel and nothing else.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui_kit::Context;
use muster_core::opencode::observe::{AgentAgent, AgentSession, Attention};
use muster_core::opencode::{Client, ReadBudget, ReviewRound, RoundStore, OBSERVATION_TIMEOUT};

use crate::app::TerminalWindow;
use crate::terminal::{OpenCodeReport, OpenCodeState};

/// How often the service is asked about again while something is running.
///
/// A turn ends in silence: the service announces no completion, so the only way a
/// finished turn is noticed is by reading again.
pub(crate) const BUSY_POLL: Duration = Duration::from_secs(2);

/// How often the service is asked about again while nothing is running.
///
/// The service belongs to whoever started it, so this app does not start one: it waits
/// here until that person runs OpenCode, and connects on the first read that answers.
pub(crate) const IDLE_POLL: Duration = Duration::from_secs(5);

/// What one read of the service is about.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentRequest {
    /// The generation this request belongs to.
    ///
    /// A read that answers a request from an earlier generation is about a set of
    /// checkouts the window is no longer showing, so it is dropped.
    pub(crate) generation: u64,
    /// The checkouts whose agent catalog is read, with the directory to read it for.
    pub(crate) directories: Vec<(String, PathBuf)>,
    cursor: usize,
}

/// What one read of the service found.
#[derive(Clone, Debug, Default)]
pub(crate) struct AgentSnapshot {
    pub(crate) generation: u64,
    /// The sessions the service lists, wherever they live.
    pub(crate) sessions: Vec<AgentSession>,
    /// The failure, when the service could not be read at all.
    pub(crate) error: Option<String>,
    /// The agent catalog of each checkout that could be read.
    pub(crate) catalogs: HashMap<String, Vec<AgentAgent>>,
    /// Deliveries captured under the store lock before this observation started.
    eligible: Vec<ReviewRound>,
}

impl AgentSnapshot {
    /// A snapshot that says the service could not be reached.
    fn failed(generation: u64, error: String) -> Self {
        Self {
            generation,
            sessions: Vec::new(),
            error: Some(error),
            catalogs: HashMap::new(),
            eligible: Vec::new(),
        }
    }
}

/// The home directory the service registers itself under.
pub(crate) fn home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

/// Reads the service, off the window's thread.
///
/// The whole read is one function on purpose: it is the only place that talks to the
/// service, so there is one place where a timeout, a scope or a bound is decided.
pub(crate) fn read_snapshot(request: &AgentRequest, budget: ReadBudget) -> AgentSnapshot {
    let Some(home) = home_directory() else {
        return AgentSnapshot::failed(
            request.generation,
            "the home directory is unavailable, so the OpenCode service cannot be found".into(),
        );
    };
    read_snapshot_in(&home, request, budget)
}

fn read_snapshot_in(
    home: &std::path::Path,
    request: &AgentRequest,
    budget: ReadBudget,
) -> AgentSnapshot {
    // Capture before HTTP, not when a delayed completion handler happens to run.
    // A broken store permits observation but never acknowledgement.
    let eligible = RoundStore::open(home)
        .and_then(|store| store.eligible_dispatched())
        .unwrap_or_default();
    let client = match Client::connect_service_with_budget(home, budget.clone()) {
        Ok(client) => client,
        Err(error) => return AgentSnapshot::failed(request.generation, error.to_string()),
    };
    let sessions = match client.service_sessions_from(request.cursor) {
        Ok(sessions) => sessions,
        Err(error) => return AgentSnapshot::failed(request.generation, error.to_string()),
    };
    let mut catalogs = HashMap::new();
    for (checkout, directory) in &request.directories {
        // A catalog that cannot be read leaves the row without a name and without a
        // colour, which is a row that says less rather than a row that says something
        // about another checkout's agents.
        let slice = client
            .clone()
            .with_read_budget(budget.slice(Duration::from_millis(150)));
        if let Ok(agents) = slice.agents(directory) {
            catalogs.insert(checkout.clone(), agents);
        }
    }
    AgentSnapshot {
        generation: request.generation,
        sessions,
        error: None,
        catalogs,
        eligible,
    }
}

/// How long a catalog is reused before it is read again.
///
/// A catalog is a palette rather than a turn state: it changes when a person edits
/// their agents, and no row is made untrue by showing the last one for a few seconds.
pub(crate) const CATALOG_TTL: Duration = Duration::from_secs(10);

/// How many checkouts' catalogs are read in one pass.
const MAX_CATALOGS: usize = 8;

/// What the window knows about the OpenCode service.
#[derive(Default)]
pub(crate) struct AgentState {
    /// The sessions the service lists, wherever they live.
    pub(crate) sessions: Vec<AgentSession>,
    /// The agent catalog of each checkout that could be read.
    pub(crate) catalogs: HashMap<String, Vec<AgentAgent>>,
    /// When each catalog was read, so a palette is not read again every tick.
    catalog_read: HashMap<String, Instant>,
    /// The failure, when the service could not be read at all.
    pub(crate) error: Option<String>,
    /// The sessions that were running at the previous read.
    ///
    /// A turn ends in silence, so this is the only record of what the previous read
    /// said and therefore the only way a finished turn is noticed.
    running: HashSet<String>,
    /// Service-wide lists are capped. Omission or a failed read cannot erase the
    /// last unsafe location; only positively known idle for that identity can.
    activity_evidence: HashMap<String, PathBuf>,
    /// The generation of the request that is on its way.
    pub(crate) generation: u64,
    /// The checkouts the last request asked about.
    directories: Vec<(String, PathBuf)>,
    cursor: usize,
}

impl AgentState {
    /// The catalog of one checkout.
    pub(crate) fn catalog(&self, checkout: &str) -> &[AgentAgent] {
        self.catalogs.get(checkout).map_or(&[], Vec::as_slice)
    }

    /// A session by its id.
    pub(crate) fn session(&self, session_id: &str) -> Option<&AgentSession> {
        self.sessions
            .iter()
            .find(|session| session.id == session_id)
    }

    /// Takes a snapshot, and answers which turns were seen to finish.
    ///
    /// A snapshot from an earlier generation is dropped and nothing is settled: the
    /// checkouts it describes are not the ones on screen, and a turn it saw end
    /// happened in a world this window is no longer showing.
    pub(crate) fn apply(&mut self, snapshot: AgentSnapshot) -> Vec<String> {
        if snapshot.generation != self.generation {
            return Vec::new();
        }
        if let Some(error) = snapshot.error {
            // Keep the last evidence and the completion cursor across unreadable polls.
            // Rows are unavailable, but a destructive guard must not forget live work.
            self.error = Some(error);
            return Vec::new();
        }
        let settled =
            muster_core::opencode::observe::settled_turns(&snapshot.sessions, &mut self.running);
        for session in &snapshot.sessions {
            if session.running || session.awaiting_reply != Some(false) {
                if let Some(directory) = &session.directory {
                    self.activity_evidence
                        .insert(session.id.clone(), directory.clone());
                }
            } else {
                self.activity_evidence.remove(&session.id);
            }
        }
        self.sessions = snapshot.sessions;
        // A catalog that was not read this time is kept: the read is about the
        // sessions, and a palette does not change because a tick passed.
        for (checkout, agents) in snapshot.catalogs {
            self.catalog_read.insert(checkout.clone(), Instant::now());
            self.catalogs.insert(checkout, agents);
        }
        self.error = None;
        settled
    }

    /// The next read's generation, and the request that goes with it.
    ///
    /// The generation moves when the set of checkouts moves, because a read about a
    /// different set of checkouts answers a different question. The catalogs are asked
    /// for only when they are stale, so a palette is not read on every tick.
    pub(crate) fn request(&mut self, directories: Vec<(String, PathBuf)>) -> AgentRequest {
        if self.directories != directories {
            self.directories = directories.clone();
            self.generation = self.generation.wrapping_add(1);
        }
        let mut wanted: Vec<(String, PathBuf)> = directories
            .iter()
            .filter(|(checkout, _)| self.catalog_is_stale(checkout))
            .cloned()
            .collect();
        if !wanted.is_empty() {
            let start = self.cursor % wanted.len();
            wanted.rotate_left(start);
            wanted.truncate(MAX_CATALOGS);
        }
        let cursor = self.cursor;
        self.cursor = self.cursor.wrapping_add(1);
        AgentRequest {
            generation: self.generation,
            directories: wanted,
            cursor,
        }
    }

    /// Whether a catalog is missing or old enough to be read again.
    fn catalog_is_stale(&self, checkout: &str) -> bool {
        self.catalog_read
            .get(checkout)
            .is_none_or(|read| read.elapsed() > CATALOG_TTL)
    }

    pub(crate) fn protects_directory(&self, directory: &std::path::Path) -> bool {
        self.activity_evidence
            .values()
            .any(|path| same_directory(path, directory))
            || self.sessions.iter().any(|session| {
                session
                    .directory
                    .as_deref()
                    .is_some_and(|path| same_directory(path, directory))
                    && (session.running || session.awaiting_reply != Some(false))
            })
    }

    /// The catalog of the checkout a directory belongs to.
    ///
    /// The catalog answers for a directory, so the agent a session names is looked up in
    /// the catalog of the place the session is working rather than in the catalog of
    /// the worktree the row happens to be filed under.
    pub(crate) fn catalog_for_directory(&self, directory: &std::path::Path) -> &[AgentAgent] {
        self.directories
            .iter()
            .find(|(_, path)| same_directory(path, directory))
            .map(|(checkout, _)| self.catalog(checkout))
            .unwrap_or(&[])
    }
}

/// Whether two paths are the same directory.
pub(crate) fn same_directory(left: &std::path::Path, right: &std::path::Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// What a fresh service read means for a destructive mutation.
///
/// The three answers, and the middle one is the question: a service that could not be
/// read is a refusal only when this app had already seen it working. A service that is
/// not running at all is not evidence that something is working, and refusing every
/// archive because OpenCode happens to be closed would be a nuisance rather than a
/// safety.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActivityVerdict {
    /// Nothing is working there.
    Clear,
    /// Something is working there.
    Busy,
    /// The service could not be read, and it was working a moment ago.
    Unreadable,
}

/// A fresh directory read distinguishes absent service from unreadable activity.
/// Pending failures are not evidence of idle, even for sessions outside this app.
pub(crate) fn read_activity(
    home: &std::path::Path,
    directory: &std::path::Path,
) -> Result<Option<bool>, String> {
    let budget = ReadBudget::new(OBSERVATION_TIMEOUT);
    let client = match Client::connect_with_budget(home, directory, budget) {
        Ok(client) => client,
        Err(muster_core::opencode::DispatchError::Unavailable(message))
            if message == muster_core::opencode::NOT_RUNNING =>
        {
            return Ok(None)
        }
        Err(error) => return Err(error.to_string()),
    };
    let sessions = client
        .directory_sessions(directory)
        .map_err(|error| error.to_string())?;
    if sessions
        .iter()
        .any(|session| session.running || session.awaiting_reply == Some(true))
    {
        return Ok(Some(true));
    }
    if sessions
        .iter()
        .any(|session| session.awaiting_reply.is_none())
    {
        return Err("OpenCode pending activity could not be read.".into());
    }
    Ok(Some(false))
}

/// What a fresh read of the service means.
pub(crate) fn activity_verdict(
    fresh: Result<Option<bool>, String>,
    previously_active: bool,
) -> ActivityVerdict {
    match fresh {
        Ok(Some(true)) => ActivityVerdict::Busy,
        Ok(Some(false)) => ActivityVerdict::Clear,
        Ok(None) if !previously_active => ActivityVerdict::Clear,
        Ok(None) | Err(_) => ActivityVerdict::Unreadable,
    }
}

/// How many sessions a list needs before it is worth searching.
///
/// Below this a person reads the list; above it they have to be able to narrow it.
pub(crate) const SEARCH_THRESHOLD: usize = 5;

/// The sessions of a checkout, newest first.
///
/// The newest is the one somebody is most likely to mean, and the ties are broken by
/// the session id so two sessions touched in the same millisecond keep the same order
/// every time.
pub(crate) fn sessions_newest_first(sessions: Vec<AgentSession>) -> Vec<AgentSession> {
    let mut sessions = sessions;
    sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    sessions
}

/// The sessions a chooser offers, after the search.
///
/// The search is case-insensitive and matches the title, because that is the only
/// thing a person knows about a session.
pub(crate) fn sessions_matching(sessions: &[AgentSession], query: &str) -> Vec<AgentSession> {
    let query = query.trim().to_lowercase();
    sessions
        .iter()
        .filter(|session| {
            query.is_empty() || session.display_title().to_lowercase().contains(&query)
        })
        .cloned()
        .collect()
}

/// Which session a terminal's title named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TerminalSession {
    /// Exactly one session carries the title.
    Identified(Box<AgentSession>),
    /// More than one does, so none of them may be drawn.
    Ambiguous,
    /// None does.
    Unknown,
    /// The service could not be read, so nothing can be named.
    Unavailable,
}

impl TerminalSession {
    /// The session, when exactly one of them was named.
    pub(crate) fn identified(&self) -> Option<&AgentSession> {
        match self {
            Self::Identified(session) => Some(session),
            Self::Ambiguous | Self::Unknown | Self::Unavailable => None,
        }
    }
}

/// The session a terminal's title names, out of what the service lists.
///
/// The title is the only thing that says which session a terminal has open: the
/// service has no registry of the programs showing its sessions, so the TUI's own
/// title is what a row is matched on. It is matched conservatively: a title that names
/// several sessions names none of them.
pub(crate) fn match_terminal(
    sessions: &[AgentSession],
    title: &str,
    available: bool,
) -> TerminalSession {
    if !available {
        return TerminalSession::Unavailable;
    }
    let Some(title) = muster_core::opencode::terminal_session_title(title) else {
        return TerminalSession::Unknown;
    };
    match muster_core::opencode::match_session_title(sessions, title) {
        muster_core::opencode::MatchedSession::One(session) => {
            TerminalSession::Identified(Box::new(session.clone()))
        }
        muster_core::opencode::MatchedSession::Ambiguous => TerminalSession::Ambiguous,
        muster_core::opencode::MatchedSession::Unknown => TerminalSession::Unknown,
    }
}

/// What a terminal row says about the OpenCode session it is showing.
///
/// The words, the agent's name, the colour the service gave that agent, and the three
/// states a row can be in: working, asking, failed. A row that could not be told which
/// session it is showing says so rather than borrowing another one's state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentRowState {
    /// The words the row says beside its name.
    pub(crate) note: String,
    /// The agent's name, when the session named one.
    pub(crate) label: Option<String>,
    /// The colour the service gave the agent, when it gave one this app can paint.
    pub(crate) tint: Option<String>,
    /// Whether a turn is running, which is the one state that moves.
    pub(crate) working: bool,
    /// Whether the last turn failed.
    pub(crate) failed: bool,
    /// Whether the session is waiting for a question or a permission.
    pub(crate) asking: bool,
    /// Whether the state could not be read at all.
    pub(crate) unavailable: bool,
    /// How long ago the service last touched the session.
    pub(crate) elapsed: Option<u64>,
}

impl Default for AgentRowState {
    fn default() -> Self {
        Self {
            note: "idle".into(),
            label: None,
            tint: None,
            working: false,
            failed: false,
            asking: false,
            unavailable: false,
            elapsed: None,
        }
    }
}

/// What a row says about the session it is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RowState {
    /// The words the row says beside its name.
    note: &'static str,
    /// Whether a turn is running, which is the one state that moves.
    working: bool,
    /// Whether the session is waiting for a question or a permission.
    asking: bool,
    /// Whether the last turn failed.
    failed: bool,
}

/// The state a row wears, given what the terminal reported and what the service said.
///
/// The terminal's own report wins: it is about this terminal, and the service
/// has no way to say which of its sessions a terminal is showing. The service
/// is asked about the rest -- how the last turn ended, which no report names.
fn reported_state(reported: Option<OpenCodeState>, attention: Option<Attention>) -> RowState {
    match reported {
        Some(OpenCodeState::Working) => RowState {
            note: "working",
            working: true,
            asking: false,
            failed: false,
        },
        Some(OpenCodeState::Blocked) => RowState {
            note: "waiting for an answer",
            working: false,
            asking: true,
            failed: false,
        },
        Some(OpenCodeState::Idle) | Some(OpenCodeState::Clear) => {
            let failed = attention == Some(Attention::Failed);
            RowState {
                note: if failed { "last turn failed" } else { "idle" },
                working: false,
                asking: false,
                failed,
            }
        }
        None => match attention {
            Some(Attention::Asking) => RowState {
                note: "waiting for an answer",
                working: false,
                asking: true,
                failed: false,
            },
            Some(Attention::Busy) => RowState {
                note: "working",
                working: true,
                asking: false,
                failed: false,
            },
            Some(Attention::Failed) => RowState {
                note: "last turn failed",
                working: false,
                asking: false,
                failed: true,
            },
            Some(Attention::Idle) => RowState {
                note: "idle",
                working: false,
                asking: false,
                failed: false,
            },
            _ => RowState {
                note: "state unavailable",
                working: false,
                asking: false,
                failed: false,
            },
        },
    }
}

/// The row a terminal wears, given what the service said.
///
/// Nothing is invented here: a session that could not be named produces a row that says
/// so, a pending state that could not be read produces a row that says its state is
/// unavailable, and a session that is doing nothing produces a row that says nothing is
/// happening.
///
/// The state the terminal reported about itself wins over the service's answer,
/// because it is about this terminal: the service cannot say which of its
/// sessions a terminal is showing, and a row that drew another session's state
/// would be claiming a turn nobody is running here.
pub(crate) fn agent_row_state(
    session: &TerminalSession,
    agents: &[AgentAgent],
    now_millis: i64,
    reported: Option<OpenCodeState>,
) -> AgentRowState {
    match session {
        TerminalSession::Unavailable => AgentRowState {
            note: "state unavailable".into(),
            unavailable: true,
            ..AgentRowState::default()
        },
        TerminalSession::Ambiguous => AgentRowState {
            note: "several sessions with this name".into(),
            unavailable: true,
            ..AgentRowState::default()
        },
        TerminalSession::Unknown => {
            let Some(reported) = reported else {
                // Nothing named the session, and that is what the row says.
                return AgentRowState {
                    note: "session not identified".into(),
                    ..AgentRowState::default()
                };
            };
            // The session could not be named, but the terminal said what it is
            // doing, and that report is about this terminal.
            let state = reported_state(Some(reported), None);
            AgentRowState {
                note: state.note.into(),
                working: state.working,
                asking: state.asking,
                ..AgentRowState::default()
            }
        }
        TerminalSession::Identified(session) => {
            let label = muster_core::opencode::agent_label(agents, session.agent.as_deref());
            let tint = muster_core::opencode::agent_color(agents, session.agent.as_deref());
            let attention = session.attention();
            // A pending state that could not be read is not an answer, and the row says
            // its state is unavailable rather than claiming the session is idle. A
            // terminal that reported about itself has already answered, though.
            if reported.is_none() && session.awaiting_reply.is_none() && !session.running {
                return AgentRowState {
                    note: "state unavailable".into(),
                    label,
                    tint,
                    unavailable: true,
                    elapsed: elapsed_of(session, now_millis),
                    ..AgentRowState::default()
                };
            }
            let state = reported_state(reported, Some(attention));
            AgentRowState {
                note: state.note.into(),
                label,
                tint,
                working: state.working,
                failed: state.failed,
                asking: state.asking,
                unavailable: reported.is_none() && session.awaiting_reply.is_none(),
                elapsed: elapsed_of(session, now_millis),
            }
        }
    }
}

/// How long ago the service last touched a session.
fn elapsed_of(session: &AgentSession, now_millis: i64) -> Option<u64> {
    muster_core::opencode::elapsed_seconds(session.updated_at, now_millis)
}

/// How a duration is written in a row.
///
/// A duration under a minute is not worth a number of seconds, and one that long is
/// not worth a number of minutes: the row says what a person would say about it.
pub(crate) fn elapsed_label(elapsed: Option<u64>) -> String {
    let Some(elapsed) = elapsed else {
        return String::new();
    };
    match elapsed {
        0..=59 => format!("{elapsed}s"),
        60..=3599 => format!("{}m", elapsed / 60),
        _ => format!("{}h", elapsed / 3600),
    }
}

/// Cancellation follows the async owner even while blocking HTTP is on another thread.
struct ObservationGuard(ReadBudget);

impl Drop for ObservationGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn maintain_rounds(
    home: &std::path::Path,
    sessions: &[String],
    eligible: &[ReviewRound],
    checkouts: &[(String, PathBuf)],
    cursor: usize,
    budget: ReadBudget,
    retries: Vec<ReviewRound>,
) -> (bool, Vec<ReviewRound>) {
    let mut sessions = sessions.to_vec();
    let mut eligible: Vec<_> = eligible
        .iter()
        .filter(|round| sessions.contains(&round.session_id))
        .cloned()
        .collect();
    // A retry names only its old deliveries, not all deliveries in a later snapshot.
    for round in retries {
        if !sessions.contains(&round.session_id) {
            sessions.push(round.session_id.clone());
        }
        if !eligible.contains(&round) {
            eligible.push(round);
        }
    }
    let mut pending = Vec::new();
    let Ok(store) = RoundStore::open(home) else {
        return (false, eligible);
    };
    let mut changed = false;
    for session in &sessions {
        if budget.request_timeout().is_err() {
            pending.extend(
                eligible
                    .iter()
                    .filter(|round| &round.session_id == session)
                    .cloned(),
            );
            continue;
        }
        match store.ack_dispatched_rounds(session, &eligible) {
            Ok(count) => changed |= count > 0,
            Err(_) => pending.extend(
                eligible
                    .iter()
                    .filter(|round| &round.session_id == session)
                    .cloned(),
            ),
        }
    }
    let Ok(mut queued) = store.queued() else {
        return (changed, pending);
    };
    if !queued.is_empty() {
        let start = cursor % queued.len();
        queued.rotate_left(start);
    }
    let mut visited = HashSet::new();
    for round in queued {
        if !visited.insert(round.checkout_id.clone()) {
            continue;
        }
        let Some((id, directory)) = checkouts.iter().find(|(id, _)| id == &round.checkout_id)
        else {
            continue;
        };
        // The slice has to cover the read that says the session is free and the
        // offer itself: a send that ran out of time would leave the round as a
        // question rather than a delivery.
        let slice = budget.slice(Duration::from_millis(1500));
        let Ok(client) = Client::connect_with_budget(home, directory, slice) else {
            continue;
        };
        // A failure is still a change: the round it was about may have been claimed
        // before the answer was lost, and the panel has to show that.
        changed |= match store.flush_rounds(&client, id) {
            Ok(sent) => sent > 0,
            Err(_) => true,
        };
    }
    (changed, pending)
}

impl TerminalWindow {
    /// Starts the watch that says what an OpenCode session is doing.
    ///
    /// The read happens on the background executor and comes back as a value, so a
    /// service that takes its time costs a tick of the panel and nothing else. The task
    /// belongs to the window: it stops when the window goes, and it stops on its own
    /// when the entity it reads is gone.
    pub(crate) fn start_agent_watch(&mut self, cx: &mut Context<Self>) {
        if self._agent_watch.is_some() {
            return;
        }
        self._agent_watch = Some(cx.spawn(async move |view, cx| {
            // Keep exact delivery snapshots when the store is busy, never a later
            // delivery that was not eligible for the turn we saw finish.
            let mut pending_ack = Vec::new();
            loop {
                let interval = view
                    .read_with(cx, |app, _| app.agent_poll_interval())
                    .unwrap_or(IDLE_POLL);
                cx.background_executor().timer(interval).await;
                let Ok((request, revision)) =
                    view.update(cx, |app, _| (app.agent_request(), app.membership_revision))
                else {
                    return;
                };
                let guard = ObservationGuard(ReadBudget::new(OBSERVATION_TIMEOUT));
                let budget = guard.0.clone();
                let worker = cx
                    .background_executor()
                    .spawn(async move { read_snapshot(&request, budget) });
                let mut snapshot = worker.await;
                let eligible = std::mem::take(&mut snapshot.eligible);
                drop(guard);
                let guard = ObservationGuard(ReadBudget::new(OBSERVATION_TIMEOUT));
                let Ok(maintenance) = view.update(cx, |app, cx| {
                    if app.closing || app.membership_revision != revision {
                        return None;
                    }
                    let settled = app.agent.apply(snapshot);
                    app.sync_opencode_sessions(cx);
                    app.run_follow_pass(cx);
                    cx.notify();
                    app.maintain_opencode_rounds(
                        settled,
                        eligible,
                        std::mem::take(&mut pending_ack),
                        guard.0.clone(),
                        cx,
                    )
                }) else {
                    return;
                };
                // One cycle owns both observation and queue maintenance; no overlapping
                // detached flushes. Dropping the watch cancels the next blocking read.
                if let Some(worker) = maintenance {
                    let (changed, pending) = worker.await;
                    pending_ack = pending;
                    let _ = view.update(cx, |app, cx| {
                        if changed {
                            if let Some(checkout) = app.open_diff.as_ref().map(|(id, _)| id.clone())
                            {
                                app.restore_opencode_review(&checkout, cx);
                            }
                            cx.notify();
                        }
                    });
                }
            }
        }));
    }

    /// How long until the service is asked about again.
    fn agent_poll_interval(&self) -> Duration {
        if self
            .agent
            .sessions
            .iter()
            .any(|session| session.running || session.awaiting_reply == Some(true))
        {
            BUSY_POLL
        } else {
            IDLE_POLL
        }
    }

    /// The checkouts whose agent catalog is worth reading.
    ///
    /// Every checkout that has a terminal, not only the one on screen: a row in a
    /// worktree that is not selected still says what its session is doing.
    fn agent_directories(&self) -> Vec<(String, PathBuf)> {
        let mut directories = Vec::new();
        for repo in &self.navigation.repos {
            for checkout in &repo.checkouts {
                if checkout.is_missing || self.navigation.archived.contains(&checkout.id) {
                    continue;
                }
                if !checkout
                    .sessions
                    .iter()
                    .any(|session| self.terminals.contains_key(&session.id))
                {
                    continue;
                }
                directories.push((checkout.id.clone(), PathBuf::from(&checkout.canonical_path)));
            }
        }
        directories
    }

    /// The read that is on its way.
    fn agent_request(&mut self) -> AgentRequest {
        let directories = self.agent_directories();
        self.agent.request(directories)
    }

    /// Settle only pre-observation deliveries, then service durable queues on every
    /// poll (including the first poll after restart). Flush rechecks fresh idle.
    fn maintain_opencode_rounds(
        &self,
        sessions: Vec<String>,
        eligible: Vec<ReviewRound>,
        pending: Vec<ReviewRound>,
        budget: ReadBudget,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::Task<(bool, Vec<ReviewRound>)>> {
        let home = home_directory()?;
        let checkouts: Vec<_> = self
            .navigation
            .repos
            .iter()
            .flat_map(|repo| &repo.checkouts)
            .filter(|checkout| {
                !checkout.is_missing
                    && !self.navigation.archived.contains(&checkout.id)
                    && !self.removing_checkouts.contains(&checkout.id)
            })
            .map(|checkout| (checkout.id.clone(), PathBuf::from(&checkout.canonical_path)))
            .collect();
        let cursor = self.agent.cursor;
        Some(cx.background_executor().spawn(async move {
            maintain_rounds(
                &home, &sessions, &eligible, &checkouts, cursor, budget, pending,
            )
        }))
    }

    /// What a terminal row says about the OpenCode session it is showing.
    ///
    /// The title is what says which session a terminal has open, and it is
    /// matched conservatively: a title that names several sessions names none
    /// of them, and a service that could not be read names none either.
    ///
    /// What the terminal itself reported comes first: it names the session it
    /// is showing, and it says what that session is doing, which is the one
    /// thing the service cannot say.
    pub(crate) fn opencode_row_state(
        &self,
        checkout_id: &str,
        title: Option<&str>,
        reported: Option<&OpenCodeReport>,
    ) -> AgentRowState {
        let available = self.agent.error.is_none();
        // The session the terminal named, and the title's own match when it
        // named none or named one the service does not know yet.
        let matched = match reported.and_then(|report| report.session.as_deref()) {
            Some(id) => match self.agent.session(id) {
                Some(session) => TerminalSession::Identified(Box::new(session.clone())),
                None => match_terminal(&self.agent.sessions, title.unwrap_or_default(), available),
            },
            None => match_terminal(&self.agent.sessions, title.unwrap_or_default(), available),
        };
        let catalog = match matched
            .identified()
            .and_then(|session| session.directory.as_deref())
        {
            Some(directory) => self.agent.catalog_for_directory(directory),
            None => self.agent.catalog(checkout_id),
        };
        agent_row_state(
            &matched,
            catalog,
            muster_core::opencode::now_millis(),
            reported.map(|report| report.state),
        )
    }

    /// The sessions a review of this checkout can go to, newest first.
    ///
    /// The newest first, because the newest is the one somebody is most likely to mean,
    /// and the ties are broken by the session id so two sessions touched in the same
    /// millisecond keep the same order every time.
    pub(crate) fn opencode_sessions_for(&self, checkout_id: &str) -> Vec<AgentSession> {
        let Some(checkout) = self.navigation.checkout(checkout_id) else {
            return Vec::new();
        };
        let directory = PathBuf::from(&checkout.canonical_path);
        let sessions: Vec<AgentSession> = self
            .agent
            .sessions
            .iter()
            .filter(|session| {
                session
                    .directory
                    .as_ref()
                    .is_some_and(|session| same_directory(session, &directory))
            })
            .cloned()
            .collect();
        sessions_newest_first(sessions)
    }
}

// Reuse the wire-level fixture, not a stubbed Client, in native orchestration tests.
#[cfg(test)]
#[allow(dead_code)]
#[path = "../../core/src/opencode/mock.rs"]
pub(crate) mod mock;

#[cfg(test)]
mod tests {
    use super::*;
    use muster_core::opencode::observe::AgentSession;

    fn session(id: &str, title: &str) -> AgentSession {
        AgentSession {
            id: id.into(),
            title: title.into(),
            ..AgentSession::default()
        }
    }

    #[test]
    fn a_title_that_names_no_session_names_no_state() {
        let sessions = vec![session("ses_a", "other")];
        assert_eq!(
            match_terminal(&sessions, "OC | missing", true),
            TerminalSession::Unknown
        );
    }

    #[test]
    fn a_title_that_names_two_sessions_names_neither() {
        let sessions = vec![session("ses_a", "same"), session("ses_b", "same")];
        assert_eq!(
            match_terminal(&sessions, "OC | same", true),
            TerminalSession::Ambiguous
        );
    }

    #[test]
    fn a_snapshot_from_an_earlier_generation_is_dropped_and_settles_nothing() {
        let mut state = AgentState {
            generation: 4,
            ..AgentState::default()
        };
        let mut working = session("ses_a", "one");
        working.running = true;
        let stale = AgentSnapshot {
            generation: 3,
            sessions: vec![working],
            ..AgentSnapshot::default()
        };
        assert!(state.apply(stale).is_empty());
        assert!(state.sessions.is_empty());
    }

    #[test]
    fn the_generation_moves_only_when_the_checkouts_do() {
        let mut state = AgentState::default();
        let directories = vec![("checkout".to_string(), PathBuf::from("/tmp/one"))];
        let first = state.request(directories.clone());
        assert_eq!(first.generation, 1);
        let second = state.request(directories);
        assert_eq!(
            second.generation, 1,
            "the same checkouts are the same question"
        );
        let third = state.request(vec![("other".to_string(), PathBuf::from("/tmp/two"))]);
        assert_eq!(third.generation, 2);
    }
    fn register(mock: &mock::Mock) -> tempfile::TempDir {
        // Never let a test discover a developer's real service via XDG.
        if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
            assert!(!PathBuf::from(state).join("opencode/service.json").exists());
        }
        let home = tempfile::tempdir().unwrap();
        let state = home.path().join(".local/state/opencode");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(
            state.join("service.json"),
            serde_json::json!({
                "url": format!("http://127.0.0.1:{}", mock.port()),
                "password": "test", "version": "2.0.22"
            })
            .to_string(),
        )
        .unwrap();
        home
    }

    #[test]
    fn failed_unknown_and_omitted_polls_retain_external_worktree_evidence() {
        let mut state = AgentState::default();
        let directory = PathBuf::from("/external/worktree");
        let mut working = session("ses_external", "outside this app");
        working.running = true;
        working.directory = Some(directory.clone());
        state.apply(AgentSnapshot {
            sessions: vec![working.clone()],
            ..Default::default()
        });
        state.apply(AgentSnapshot::failed(0, "timeout".into()));
        assert!(state.protects_directory(&directory));
        assert!(state.running.contains("ses_external"));
        state.apply(AgentSnapshot::default()); // omitted from capped service list
        assert!(state.protects_directory(&directory));
        assert!(state.running.contains("ses_external"));
        working.running = false;
        working.awaiting_reply = None;
        assert!(state
            .apply(AgentSnapshot {
                sessions: vec![working.clone()],
                ..Default::default()
            })
            .is_empty());
        assert!(state.protects_directory(&directory));
        working.awaiting_reply = Some(false);
        assert_eq!(
            state.apply(AgentSnapshot {
                sessions: vec![working],
                ..Default::default()
            }),
            ["ses_external"]
        );
        assert!(!state.protects_directory(&directory));
    }

    #[test]
    fn running_is_still_visible_when_pending_cannot_be_read() {
        let mut working = session("ses_a", "work");
        working.running = true;
        working.awaiting_reply = None;
        let row = agent_row_state(
            &TerminalSession::Identified(Box::new(working)),
            &[],
            0,
            None,
        );
        assert!(row.working);
        assert!(row.unavailable);
        assert_eq!(row.note, "working");
    }

    #[test]
    fn native_http_polls_read_every_location_and_rotate_catalogs_beyond_eight() {
        use serde_json::json;
        let mock = mock::Mock::start(|request| {
            let data = match request.path.as_str() {
                "/api/info" => json!({}),
                "/api/session" => json!((0..10)
                    .map(|i| json!({
                        "id": format!("ses_{i}"), "title": format!("work {i}"),
                        "location": {"directory": format!("/checkout/{i}")}
                    }))
                    .collect::<Vec<_>>()),
                "/api/session/active" => json!({}),
                _ => json!([]),
            };
            let directory = request
                .query_param("directory")
                .unwrap_or("/located-envelope".into());
            mock::reply(
                200,
                json!({"data": data, "location": {"directory": directory}}),
            )
        });
        let home = register(&mock);
        let mut state = AgentState::default();
        let directories: Vec<_> = (0..10)
            .map(|i| (format!("c{i}"), PathBuf::from(format!("/checkout/{i}"))))
            .collect();
        let mut known = HashSet::new();
        for _ in 0..3 {
            let request = state.request(directories.clone());
            let snapshot =
                read_snapshot_in(home.path(), &request, ReadBudget::new(OBSERVATION_TIMEOUT));
            assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
            assert_eq!(
                snapshot
                    .sessions
                    .iter()
                    .filter(|session| session.awaiting_reply.is_none())
                    .count(),
                0,
                "every location is asked about, so a row knows whether its session is idle"
            );
            known.extend(
                snapshot
                    .sessions
                    .iter()
                    .filter(|session| session.awaiting_reply == Some(false))
                    .map(|session| session.id.clone()),
            );
            state.apply(snapshot);
        }
        assert_eq!(known.len(), 10);
        assert_eq!(state.catalogs.len(), 10);
        assert!(mock
            .requests()
            .iter()
            .any(|request| request.path == "/api/agent"
                && request.query_param("directory").as_deref() == Some("/checkout/9")));
    }

    #[test]
    fn dropping_the_native_observation_owner_cancels_between_http_calls() {
        use serde_json::json;
        let guard = ObservationGuard(ReadBudget::new(OBSERVATION_TIMEOUT));
        let cancel = guard.0.clone();
        let mock = mock::Mock::start(move |request| {
            if request.path == "/api/session" {
                cancel.cancel();
            }
            mock::reply(200, json!({"data": []}))
        });
        let home = register(&mock);
        let snapshot = read_snapshot_in(home.path(), &AgentRequest::default(), guard.0.clone());
        assert!(snapshot.error.is_some());
        assert_eq!(
            mock.requests().len(),
            2,
            "discovery and list only, no active/pending/catalog calls after cancellation"
        );
        let budget = guard.0.clone();
        drop(guard);
        assert!(Client::connect_service_with_budget(home.path(), budget).is_err());
        assert_eq!(mock.requests().len(), 2);
    }

    #[test]
    fn unreadable_pending_and_slow_discovery_protect_external_worktrees() {
        use serde_json::json;
        let mock = mock::Mock::start(|request| {
            let directory = request.query_param("directory").unwrap_or_default();
            let (status, data) = match request.path.as_str() {
                "/api/session" => (
                    200,
                    json!([{"id": "ses_external", "location": {"directory": directory}}]),
                ),
                "/api/form" => (500, json!({})),
                _ => (200, json!({})),
            };
            mock::reply(
                status,
                json!({"data": data, "location": {"directory": directory}}),
            )
        });
        let home = register(&mock);
        let read = read_activity(home.path(), std::path::Path::new("/external"));
        assert_eq!(activity_verdict(read, false), ActivityVerdict::Unreadable);
        mock.stop();
        let slow = mock::Mock::start_on(mock.port(), |_| {
            std::thread::sleep(Duration::from_millis(1200));
            mock::reply(200, json!({"data": {}}))
        });
        let started = Instant::now();
        let read = read_activity(home.path(), std::path::Path::new("/external"));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            activity_verdict(read, false),
            ActivityVerdict::Unreadable,
            "a timed-out probe is unknown, not an absent service"
        );
        slow.stop();
        assert_eq!(
            activity_verdict(
                read_activity(home.path(), std::path::Path::new("/external")),
                true
            ),
            ActivityVerdict::Unreadable
        );
    }

    #[test]
    fn native_scheduler_flushes_restart_queues_without_an_edge_and_late_ack_cannot_take_them() {
        use muster_core::opencode::{rounds::RoundRequest, RoundStatus};
        use muster_core::review::{ReviewNote, ReviewSide};
        use serde_json::json;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let checkout = muster_core::navigation::open_directory(&root)
            .unwrap()
            .repo
            .checkouts
            .remove(0);
        let path = root.clone();
        let mock = mock::Mock::start(move |request| {
            let session = json!({"id": "ses_review", "location": {"directory": path}});
            let data = match request.path.as_str() {
                "/api/session" => json!([session]),
                "/api/session/ses_review" => session,
                "/api/form" | "/api/permission/request" | "/api/session/ses_review/message" => {
                    json!([])
                }
                _ => json!({}),
            };
            mock::reply(200, json!({"data": data, "location": {"directory": path}}))
        });
        let home = register(&mock);
        let note = ReviewNote {
            id: "note".into(),
            checkout_id: checkout.id.clone(),
            path: "file.rs".into(),
            side: ReviewSide::New,
            line_start: 1,
            line_end: None,
            content: "fix this".into(),
            code: "code".into(),
            code_hash: "hash".into(),
            outdated: false,
        };
        let notes = [note.clone()];
        let request = || RoundRequest {
            checkout: &checkout,
            session_id: "ses_review",
            notes: &notes,
            review_markdown: "# Review",
        };
        let store = RoundStore::open(home.path()).unwrap();
        let client = Client::connect(home.path(), &root).unwrap();
        let old = store.send_round(&client, request()).unwrap();
        let mut changed_note = note;
        changed_note.content = "a second review".into();
        let new = store
            .queue_round(RoundRequest {
                notes: &[changed_note],
                ..request()
            })
            .unwrap();
        drop(store); // restart: no window has seen this session run
        let snapshot = read_snapshot_in(
            home.path(),
            &AgentRequest::default(),
            ReadBudget::new(OBSERVATION_TIMEOUT),
        );
        assert!(snapshot.error.is_none());
        assert_eq!(snapshot.eligible.len(), 1);
        let checkouts = [(checkout.id.clone(), root)];
        assert!(
            maintain_rounds(
                home.path(),
                &[],
                &[],
                &checkouts,
                0,
                ReadBudget::new(OBSERVATION_TIMEOUT),
                Vec::new(),
            )
            .0
        );
        // A busy store must not lose the completion edge or widen its eligible deliveries.
        let store = RoundStore::open(home.path()).unwrap();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(store.path().with_file_name("opencode-rounds.lock"))
            .unwrap();
        lock.lock().unwrap();
        let (changed, pending) = maintain_rounds(
            home.path(),
            &["ses_review".into()],
            &snapshot.eligible,
            &[],
            0,
            ReadBudget::new(OBSERVATION_TIMEOUT),
            Vec::new(),
        );
        assert!(!changed);
        assert_eq!(pending, snapshot.eligible);
        drop(lock);
        // Another window flushed before this window applied its earlier idle observation.
        assert!(
            maintain_rounds(
                home.path(),
                &[],
                &store.eligible_dispatched().unwrap(),
                &checkouts,
                0,
                ReadBudget::new(OBSERVATION_TIMEOUT),
                pending,
            )
            .0
        );
        let store = RoundStore::open(home.path()).unwrap();
        assert_eq!(
            store.round(&checkout.id, &old.id).unwrap().unwrap().status,
            RoundStatus::Acked
        );
        assert_eq!(
            store.round(&checkout.id, &new.id).unwrap().unwrap().status,
            RoundStatus::Dispatched
        );
        assert_eq!(
            mock.requests()
                .iter()
                .filter(|request| request.method == "POST")
                .count(),
            2
        );
    }
    #[test]
    fn native_snapshot_shares_one_deadline_through_pending_and_catalogs() {
        use serde_json::json;
        let mock = mock::Mock::start(|request| {
            // The unscoped reads are cheap and the per-location ones are not: the session
            // list has to arrive inside the deadline, and the pending lists and the
            // catalogs are the work that deadline has to cut off. A read that has to
            // succeed cannot also be an expensive one -- a loaded runner schedules a
            // thread later than a workstation does, and this test is about the deadline
            // rather than about the machine.
            let unscoped = matches!(
                request.path.as_str(),
                "/api/info" | "/api/session" | "/api/session/active"
            );
            if !unscoped {
                std::thread::sleep(Duration::from_millis(80));
            }
            let data = match request.path.as_str() {
                "/api/session" => json!((0..12)
                    .map(|i| json!({
                        "id": format!("ses_{i}"),
                        "location": {"directory": format!("/checkout/{i}")}
                    }))
                    .collect::<Vec<_>>()),
                "/api/info" | "/api/session/active" => json!({}),
                _ => json!([]),
            };
            let directory = request.query_param("directory").unwrap_or_default();
            mock::reply(
                200,
                json!({"data": data, "location": {"directory": directory}}),
            )
        });
        let home = register(&mock);
        let request = AgentRequest {
            directories: (0..8)
                .map(|i| (format!("c{i}"), PathBuf::from(format!("/checkout/{i}"))))
                .collect(),
            ..Default::default()
        };
        let started = Instant::now();
        let snapshot = read_snapshot_in(
            home.path(),
            &request,
            ReadBudget::new(Duration::from_millis(600)),
        );
        assert!(
            started.elapsed() < Duration::from_millis(900),
            "snapshot resets its deadline between endpoints"
        );
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        assert!(snapshot
            .sessions
            .iter()
            .any(|session| session.awaiting_reply.is_none()));
        // The pending read is given half of what is left, so the catalogs get the other
        // half: a row that knows the name of its agent is worth more than a pending list
        // that would have said the session was idle.
        assert!(
            snapshot.catalogs.len() < 8,
            "the deadline cuts the catalogs off too"
        );
        // The work would be thirty-five reads: three unscoped, twenty-four pending and
        // eight catalogs. The deadline stops it long before that.
        let reads = mock.requests().len();
        assert!(reads <= 12, "the snapshot made {reads} reads");
    }
}
