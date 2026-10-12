//! A minimal OpenCode bridge: a client of the service the person running OpenCode started.
//!
//! Everything Muster needs from the agent is scoped to the checkout that owns it, so every
//! request names that checkout's directory and every response is checked against the expected
//! path before it is trusted. A `ses_…` id from one checkout is meaningless in another's
//! directory, and `agent_session` is what enforces that.
//!
//! The service is the user's, not this app's. Nothing here starts one and nothing here ends one:
//! `services::opencode` finds the service that is already running, and the absence of one leaves
//! the app disconnected rather than leaving a server behind.
//!
//! # What the service cannot tell us
//!
//! JSON requests carry the checkout's directory twice, because the service reads the scope two
//! ways: the `x-opencode-directory` header decides which location answers, and the `directory`
//! query parameter filters session lists. Sending the header alone leaves `/api/session`
//! answering for every location the service knows, and sending the query alone leaves `/api/agent`
//! answering for the service's own working directory. The SSE request carries the header too;
//! each event is additionally filtered by its reported location when one is present.
//!
//! OpenCode 2.0.22 has no client registry. There is no route that reports which TUI process is
//! attached to which session, the `x-opencode-client` header is only ever sent *out* to model
//! providers, and `/api/session/active` answers for the whole service rather than for one
//! connection. So a session cannot be attributed to the terminal that is showing it. What *is*
//! available per terminal is the directory it runs in, which is what `terminal_agent_rows` uses:
//! it reports each terminal's own directory scope instead of guessing which session that
//! terminal has selected. Guessing from `time.viewed` or from recency would misattribute the
//! moment two TUIs share a checkout, which is the case this has to be right for.

use std::{
    collections::{HashMap, HashSet},
    io::{self, BufRead, BufReader, Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::sleep,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::Deserialize;

mod relocation;

use crate::{
    domain::{
        agent::{agent_event_kind, AgentAgent, AgentProvider, AgentRelocation, AgentSession},
        ipc::{IpcError, IpcErrorCode},
    },
    services::opencode::{self, ServiceEndpoint},
};

pub use crate::domain::agent::AgentEvent;

/// The HTTP requests a bridge makes, which a service that is busy answering a turn can delay.
const JSON_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How many sessions one relocation read asks for.
///
/// The service answers with the newest fifty and, as measured against 2.0.23, ignores both this
/// and `cursor`: fifty is a ceiling this side cannot raise. Sent anyway, because it costs nothing
/// and a server that honours it widens the window for free.
const SESSION_LIST_LIMIT: &str = "500";
/// How long a session's last known worktree is kept once the reads stop naming it.
///
/// A session that is not in the page this read asked for, or whose directory is one this panel has
/// no checkout for, keeps its last known position for this long rather than forever: the position
/// is what turns a worktree registered later into a move, and a stale one is worse than a missing
/// one.
const SESSION_LOCATION_TTL: Duration = Duration::from_secs(300);
/// How long a reported move is handed over again before it is given up on.
///
/// A move is a change in where a session works, and a terminal left in the worktree it left is
/// reading the wrong tree, so the move is offered for longer than one poll is likely to take. It
/// is not offered forever: a move nobody applied after this long is not going to be applied at
/// all, and repeating it would keep trying to move a terminal that is no longer there.
const PENDING_MOVES_TTL: Duration = Duration::from_secs(30);
/// How long a freshly found service is given to answer for this checkout before the app says it
/// is not there yet.
const SERVER_READY_TIMEOUT: Duration = Duration::from_secs(5);
// `stopAgent` is awaited by the UI.
const STARTUP_STOP_WAIT: Duration = Duration::from_secs(3);
// A waiter has to cover the launcher's wall time: its discovery budget plus the 3s reader cleanup.
// It is only an inert caller-wait bound: expiry does not cancel a healthy connect, which does not
// hold the slot open.
const STARTUP_WAIT_TIMEOUT: Duration = Duration::from_secs(9);
const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(30);
/// The service is a loopback process guarded by its own password, so basic auth is the whole
/// trust boundary. See `sec_09`.
const MAX_PROMPT_BYTES: usize = 512 * 1024;
// Bound both each response header line and the aggregate (status + headers).
const MAX_EVENT_HEADERS_BYTES: usize = 32 * 1024;
const MAX_EVENT_HEADER_LINE_BYTES: usize = 8 * 1024;
// SSE event payloads can include long transcript text; bound all wire bytes per event.
const MAX_SSE_FRAME_BYTES: usize = 1024 * 1024;
const MAX_SSE_FRAME_LINES: usize = 16 * 1024;
/// The header the service reads to decide which location answers a request.
///
/// It scopes the answer; the `directory` query parameter filters session lists. JSON requests
/// carry both; the SSE request carries this header and filters event locations locally. The one
/// read that carries neither is `get_unscoped_json`, and only because a session list filtered by
/// directory cannot name the session a terminal has open when that session lives in another
/// worktree — the row it exists for would then draw no state at all.
const DIRECTORY_HEADER: &str = "x-opencode-directory";

/// How long the service-wide session list is reused before it is read again.
///
/// Every checkout asks in the same tick and the answer is the same for all of them, so the window
/// only has to outlive one tick's requests. Longer than that would keep describing a turn that has
/// already ended.
const CANDIDATE_CACHE_TTL: Duration = Duration::from_secs(1);

/// How many sessions that read asks for.
///
/// The route answers newest-first — measured: `time.updated` descending — and stops at fifty on its
/// own, which is fewer than a busy service accumulates in a week, while a terminal left open on an
/// older session is still a row in the panel. Measured against the real service, two hundred
/// sessions are 109 KB, once per window.
const CANDIDATE_SESSION_LIMIT: usize = 200;

/// How long one directory's agent catalog is reused before it is read again.
///
/// Longer than the session list's window because a catalog is a palette rather than a turn state:
/// it changes when a person edits their agents, and no row is made untrue by showing the last one
/// for a few seconds.
const AGENT_CATALOG_TTL: Duration = Duration::from_secs(10);

/// The service a bridge talks to, and the directory it is scoped to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ServerCredentials {
    endpoint: ServiceEndpoint,
    directory: PathBuf,
}

#[derive(Debug, Deserialize)]
struct ApiEnvelope<T> {
    data: T,
    #[serde(default)]
    location: Option<ApiLocation>,
}

#[derive(Debug, Deserialize)]
struct ApiLocation {
    directory: String,
}

/// One session as `GET /api/session` and `GET /api/session/{id}` answer it.
///
/// **The `#[serde(default)]`s here are tolerance of a third party's wire, not back-compat with an
/// older Muster.** Every one of these `Api*` types parses a response written by the OpenCode
/// service, which is a separate program on its own release cadence: we do not ship it, we do not
/// bump it and `SUPPORTED_MAJOR` only refuses a different major. A field that is absent from one
/// response is a session with no agent, no model, no parent or no timestamps yet, and reading it as
/// absent keeps the panel painting instead of turning a half-answered row into a bridge error. The
/// fields that *identify* a session (`id`, `ApiModel::id`, `ApiAgent::id`) carry no default for
/// the same reason in the other direction: without them there is nothing to attribute the answer
/// to, so it is refused.
#[derive(Debug, Deserialize)]
struct ApiSession {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    time: ApiSessionTime,
    /// Present inside the session object, unlike the envelope-level `location`.
    #[serde(default)]
    location: Option<ApiLocation>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default, rename = "parentID")]
    parent_id: Option<String>,
    #[serde(default)]
    outcome: Option<String>,
    #[serde(default)]
    model: Option<ApiModel>,
}

#[derive(Debug, Deserialize)]
struct ApiModel {
    #[serde(default)]
    provider_id: String,
    id: String,
    #[serde(default)]
    variant: Option<String>,
}

impl ApiModel {
    /// The model as one string, in the `provider/id#variant` form OpenCode writes in config.
    fn label(&self) -> String {
        let base = if self.provider_id.is_empty() {
            self.id.clone()
        } else {
            format!("{}/{}", self.provider_id, self.id)
        };
        match &self.variant {
            Some(variant) => format!("{base}#{variant}"),
            None => base,
        }
    }
}

/// One entry of `GET /api/session/active`, keyed by session id.
///
/// `running` is the only value the route writes, and it is the whole point of the route: a session
/// that is absent is one that is not working. Nothing else about the turn is described here.
#[derive(Debug, Deserialize)]
struct ApiActiveSession {
    #[serde(default, rename = "type")]
    kind: String,
}

/// Pending forms (questions) and permissions carry the same session reference.
#[derive(Debug, Deserialize)]
struct ApiPendingRequest {
    #[serde(rename = "sessionID")]
    session_id: String,
}

/// One entry of `GET /api/agent`, which is where the color an agent is painted with lives.
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

#[derive(Debug, Default, Deserialize)]
struct ApiSessionTime {
    #[serde(default)]
    created: i64,
    #[serde(default)]
    updated: i64,
    #[serde(default)]
    idle: Option<i64>,
}

impl ApiSession {
    /// Refuses a session that belongs to another directory than the checkout we asked about.
    fn check_scope(&self, expected: &Path) -> Result<(), BridgeError> {
        match &self.location {
            Some(location) if !same_directory(Path::new(&location.directory), expected) => Err(
                BridgeError::Foreign("the agent server answered for a different directory".into()),
            ),
            _ => Ok(()),
        }
    }
}

impl<T> ApiEnvelope<T> {
    /// Refuses a response that names a different directory than the checkout we asked about.
    fn into_scoped(self, expected: &Path) -> Result<T, BridgeError> {
        if let Some(location) = self.location {
            if !same_directory(Path::new(&location.directory), expected) {
                return Err(BridgeError::Foreign(
                    "the agent server answered for a different directory".into(),
                ));
            }
        }
        Ok(self.data)
    }

    /// The payload of a request that asked for no directory in the first place.
    ///
    /// There is no scope to check such an answer against, so there is nothing to refuse and the
    /// payload is handed back as it arrived. The only caller is [`AgentBridge::session_locations`],
    /// which asks across every location precisely because the scope is what it cannot use.
    fn into_unscoped_data(self) -> T {
        self.data
    }
}

fn same_directory(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

#[derive(Debug, Clone)]
pub enum BridgeError {
    Unavailable(String),
    Foreign(String),
    Failed(String),
    /// The endpoint this bridge holds is not the one the service answers on: either nothing
    /// answered there, or the service refused the credentials this bridge carries. It is a variant
    /// of its own because it is the only failure that says the address has to be found again, and
    /// because a status the service chose for itself — a rejected validation, a route this version
    /// does not have — says nothing about the endpoint and must not cost the app its bridge.
    Stale(String),
}

impl BridgeError {
    fn into_ipc(self, fallback: IpcErrorCode) -> IpcError {
        let (code, message) = match self {
            BridgeError::Unavailable(message) | BridgeError::Stale(message) => {
                (IpcErrorCode::AgentUnavailable, message)
            }
            BridgeError::Foreign(message) => (IpcErrorCode::AgentOwnershipMismatch, message),
            BridgeError::Failed(message) => (fallback, message),
        };
        IpcError::new(code, message)
    }
}

pub struct AgentBridge {
    checkout_id: String,
    credentials: ServerCredentials,
    /// The HTTP client every JSON request of this bridge goes through.
    ///
    /// One agent owns one connection pool, so an agent per request is a pool per request: every
    /// read would open its own socket and pay for its handshake against a service one loopback hop
    /// away. Sharing it across threads is what ureq is for. It carries no timeout of its own —
    /// those travel with each request, because the budgets are not interchangeable (see
    /// `with_timeout`).
    client: ureq::Agent,
    /// A clone of the event socket, used to interrupt a blocking read during stop.
    event_socket: Mutex<Option<TcpStream>>,
    /// Set once the service is reached, so the event reader can be told to stop.
    reader: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// The reader loop reconnects forever, so it needs its own exit condition.
    stopped: std::sync::atomic::AtomicBool,
}

/// Where normalized events go. Injected so the service does not depend on Tauri.
pub type EventSink = Arc<dyn Fn(AgentEvent) + Send + Sync>;

impl AgentBridge {
    /// Reaches the service the user started, if there is one.
    ///
    /// Discovery is the whole of startup: there is no port to pick and no process to wait for, so
    /// this either has a service to talk to or reports that there is not one yet.
    fn connect(home: &Path, checkout_id: &str, directory: &Path) -> Result<Self, BridgeError> {
        let endpoint = opencode::discover(home).map_err(BridgeError::Unavailable)?;
        let credentials = ServerCredentials {
            endpoint,
            directory: directory.to_path_buf(),
        };
        let credentials = ready(credentials, Instant::now() + SERVER_READY_TIMEOUT)?;
        Ok(Self {
            checkout_id: checkout_id.to_string(),
            credentials,
            client: ureq::Agent::new_with_defaults(),
            event_socket: Mutex::new(None),
            reader: Mutex::new(None),
            stopped: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn base_url(&self) -> String {
        self.credentials.endpoint.url.clone()
    }

    fn auth_header(&self) -> String {
        opencode::authorization(&self.credentials.endpoint.password)
    }

    fn directory_header(&self) -> io::Result<String> {
        let directory = self.directory().to_string_lossy().into_owned();
        if directory.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the checkout path cannot be represented in an HTTP header",
            ));
        }
        Ok(directory)
    }

    /// Starts the event reader for this checkout's server, once.
    fn start_reader(self: &Arc<Self>, sink: &EventSink) {
        let mut slot = match self.reader.lock() {
            Ok(slot) => slot,
            Err(_) => return,
        };
        if slot.is_some() {
            return;
        }
        let bridge = Arc::clone(self);
        let sink = Arc::clone(sink);
        *slot = Some(std::thread::spawn(move || {
            read_events(bridge, sink);
        }));
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.credentials.directory
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    fn install_event_socket(&self, socket: TcpStream) -> bool {
        let Ok(mut slot) = self.event_socket.lock() else {
            let _ = socket.shutdown(Shutdown::Both);
            return false;
        };
        *slot = Some(socket);
        if self.is_stopped() {
            if let Some(socket) = slot.take() {
                let _ = socket.shutdown(Shutdown::Both);
            }
            false
        } else {
            true
        }
    }

    fn clear_event_socket(&self) {
        if let Ok(mut slot) = self.event_socket.lock() {
            if let Some(socket) = slot.take() {
                let _ = socket.shutdown(Shutdown::Both);
            }
        }
    }

    /// Fetches `path` and returns the `data` payload, refusing a foreign directory.
    fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, BridgeError> {
        self.get_json_with_timeout(path, JSON_REQUEST_TIMEOUT)
    }

    fn get_json_with_timeout<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        timeout: Duration,
    ) -> Result<T, BridgeError> {
        self.get_json_in_directory(path, self.directory(), timeout)
    }

    fn get_json_in_directory<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        directory: &Path,
        timeout: Duration,
    ) -> Result<T, BridgeError> {
        let url = format!("{}{path}", self.base_url());
        let envelope: ApiEnvelope<T> = send_json(retry_interrupted(|| {
            with_timeout(self.client.get(&url), timeout)
                .query("directory", directory.to_string_lossy().as_ref())
                .header("authorization", self.auth_header())
                .header(DIRECTORY_HEADER, directory.to_string_lossy().as_ref())
                .header("accept", "application/json")
                .call()
        }))?;
        envelope.into_scoped(directory)
    }

    /// Fetches a scoped read within a shared absolute deadline, including interrupted retries.
    fn get_json_in_directory_until<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        directory: &Path,
        deadline: Instant,
    ) -> Result<T, BridgeError> {
        let url = format!("{}{path}", self.base_url());
        let response = retry_interrupted_until(deadline, |timeout| {
            with_timeout(self.client.get(&url), timeout)
                .query("directory", directory.to_string_lossy().as_ref())
                .header("authorization", self.auth_header())
                .header(DIRECTORY_HEADER, directory.to_string_lossy().as_ref())
                .header("accept", "application/json")
                .call()
        })?;
        let envelope: ApiEnvelope<T> = send_json(response)?;
        envelope.into_scoped(directory)
    }

    /// Fetches `path` as the service answers it for every directory it knows.
    ///
    /// No directory in the query and none in the header, which is the only way `/api/session` will
    /// describe a session outside the checkout that happens to be asking: measured, the same route
    /// answers one session for a worktree, fifty for its repository and two hundred and more for
    /// the service. A read that carries the directory cannot answer "which session does this
    /// terminal have open" for a terminal whose session was started somewhere else, and that
    /// question is the whole reason a candidate list exists.
    fn get_unscoped_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        timeout: Duration,
    ) -> Result<T, BridgeError> {
        let url = format!("{}{path}", self.base_url());
        let response = retry_interrupted_until(Instant::now() + timeout, |remaining| {
            with_timeout(self.client.get(&url), remaining)
                .header("authorization", self.auth_header())
                .header("accept", "application/json")
                .call()
        })?;
        let envelope: ApiEnvelope<T> = send_json(response)?;
        Ok(envelope.into_unscoped_data())
    }

    /// Posts to `path` and returns the `data` payload, refusing a foreign directory.
    fn post_json<B: serde::Serialize + ?Sized, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, BridgeError> {
        let envelope: ApiEnvelope<T> = send_json(
            with_timeout(
                self.client.post(&format!("{}{path}", self.base_url())),
                JSON_REQUEST_TIMEOUT,
            )
            .query("directory", self.directory().to_string_lossy().as_ref())
            .header("authorization", self.auth_header())
            .header(
                DIRECTORY_HEADER,
                self.directory().to_string_lossy().as_ref(),
            )
            .send_json(body),
        )?;
        envelope.into_scoped(self.directory())
    }

    /// Every session the service lists, with the directory it is in, across every location.
    ///
    /// This is the one read that is deliberately not scoped, and it exists because a session
    /// that moves cannot be seen from where it was: `?directory=` filters the list by where a
    /// session is now, so a session that left stops answering for the directory it left and
    /// nothing reports that it went anywhere. Read without the scope, the same session is still
    /// there under its new directory, which is the only thing that makes a move observable.
    ///
    /// Nothing here is addressed by the answer: the caller matches each directory against the
    /// checkouts it already knows and drops the rest, so no directory this app has no checkout
    /// for crosses into the UI. Child sessions are excluded (`parentID=null`): their tool work
    /// is keyed separately, and a parent's subagent prompt/result does not attest a target path.
    ///
    /// `limit` is sent, and 2.0.23 answers with fifty sessions whatever it is set to: measured against
    /// the running service, `limit=5`, `limit=50` and `limit=500` all return the newest fifty, and
    /// `cursor.next` returns that same page again. So fifty is a ceiling this read cannot raise
    /// from here, and the parameter is kept because it costs nothing and a server that honours it
    /// would only widen the window. What makes the ceiling tolerable is that a move is a recent
    /// event: a session that just changed worktree is among the newest fifty. A session that moved
    /// and then went quiet long enough to fall out of the page is dropped by
    /// [`SESSION_LOCATION_TTL`] and is only picked up again as new.
    fn session_locations_until(&self, deadline: Instant) -> Result<Vec<ApiSession>, BridgeError> {
        let url = format!("{}/api/session", self.base_url());
        let response = retry_interrupted_until(deadline, |timeout| {
            with_timeout(self.client.get(&url), timeout)
                .query("parentID", "null")
                .query("limit", SESSION_LIST_LIMIT)
                .header("authorization", self.auth_header())
                .header("accept", "application/json")
                .call()
        })?;
        let envelope: ApiEnvelope<Vec<ApiSession>> = send_json(response)?;
        Ok(envelope
            .into_unscoped_data()
            .into_iter()
            .filter(|raw| raw.parent_id.is_none() && raw.location.is_some())
            .collect())
    }

    /// The session ids the service is draining a turn for, as its own answer rather than an
    /// inference from idle times.
    ///
    /// This is the only signal in the API that a turn is running, and it is the only one that
    /// covers a turn a person started in their own TUI. It answers for the whole service, so the
    /// caller is what narrows it to one directory.
    fn running_sessions(&self) -> Result<HashSet<String>, BridgeError> {
        self.running_sessions_until(Instant::now() + JSON_REQUEST_TIMEOUT)
    }

    fn running_sessions_until(&self, deadline: Instant) -> Result<HashSet<String>, BridgeError> {
        let active: HashMap<String, ApiActiveSession> = self.get_json_with_timeout(
            "/api/session/active",
            deadline.saturating_duration_since(Instant::now()),
        )?;
        Ok(active
            .into_iter()
            .filter(|(_, session)| session.kind == "running")
            .map(|(session_id, _)| session_id)
            .collect())
    }

    /// OpenCode v2.0.25 exposes questions as forms. Both lists answer for one location,
    /// not the whole service, even when the directory header is omitted.
    fn awaiting_sessions(
        &self,
        directory: &Path,
        timeout: Duration,
    ) -> Result<HashSet<String>, BridgeError> {
        let mut pending = HashSet::new();
        for path in ["/api/form", "/api/permission/request"] {
            let requests: Vec<ApiPendingRequest> =
                self.get_json_in_directory(path, directory, timeout)?;
            pending.extend(requests.into_iter().map(|request| request.session_id));
        }
        Ok(pending)
    }

    fn awaiting_sessions_until(
        &self,
        directory: &Path,
        deadline: Instant,
    ) -> Result<HashSet<String>, BridgeError> {
        let mut pending = HashSet::new();
        for path in ["/api/form", "/api/permission/request"] {
            let requests: Vec<ApiPendingRequest> =
                self.get_json_in_directory_until(path, directory, deadline)?;
            pending.extend(requests.into_iter().map(|request| request.session_id));
        }
        Ok(pending)
    }

    /// Resolves a session id inside this bridge only, then hands back the raw session.
    fn session(&self, session_id: &str) -> Result<ApiSession, BridgeError> {
        validate_session_id(session_id)?;
        let session: ApiSession = self.get_json(&format!("/api/session/{session_id}"))?;
        session.check_scope(self.directory())?;
        Ok(session)
    }

    /// Everything the session has been told, as one searchable blob.
    ///
    /// The route answers JSON, and the message shape differs between OpenCode versions, so
    /// the payload is re-serialized and searched rather than modelled: the only question
    /// asked is whether a given marker is present.
    fn session_transcript(&self, session_id: &str) -> Result<String, BridgeError> {
        validate_session_id(session_id)?;
        // Resolve first, so a foreign id is refused before anything is read.
        self.session(session_id)?;
        let messages: serde_json::Value =
            self.get_json(&format!("/api/session/{session_id}/message"))?;
        serde_json::to_string(&messages)
            .map_err(|error| BridgeError::Failed(format!("could not read the transcript: {error}")))
    }

    fn to_agent_session(
        &self,
        raw: &ApiSession,
        running: bool,
        awaiting_reply: bool,
    ) -> AgentSession {
        AgentSession {
            id: raw.id.clone(),
            checkout_id: self.checkout_id.clone(),
            provider: AgentProvider::OpenCode,
            title: if raw.title.is_empty() {
                "OpenCode session".to_string()
            } else {
                raw.title.clone()
            },
            idle_at: raw.time.idle,
            awaiting_reply: Some(awaiting_reply),
            agent: raw.agent.clone(),
            model: raw.model.as_ref().map(ApiModel::label),
            parent_id: raw.parent_id.clone(),
            running,
            outcome: raw.outcome.clone(),
            created_at: raw.time.created,
            updated_at: raw.time.updated,
        }
    }

    fn stop(&self) {
        self.stop_with_timeout(STARTUP_STOP_WAIT);
    }

    fn stop_with_timeout(&self, timeout: Duration) {
        // The reader reconnects forever unless told to stop, so set the flag before
        // killing the server: otherwise joining it would never return.
        self.signal_stop();
        let deadline = Instant::now() + timeout;
        self.clear_event_socket();
        if let Some(handle) = self.reader.lock().ok().and_then(|mut slot| slot.take()) {
            join_reader(handle, deadline);
        }
    }

    /// Stops this bridge without waiting: the reader is told to end and the socket it is blocked
    /// on is closed, which is all that ending this app's half of the connection takes. The service
    /// on the other side is the user's and keeps running.
    fn signal_stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.clear_event_socket();
    }
}

/// A single event frame from the stream.
///
/// The defaults are the third-party tolerance described on `ApiSession`: the service's SSE frames
/// carry `location` only when the event is scoped and `data` only when the event has a payload.
#[derive(Debug, Deserialize)]
struct SseFrame {
    #[serde(rename = "type")]
    raw_type: String,
    #[serde(default)]
    location: Option<ApiLocation>,
    #[serde(default)]
    data: serde_json::Value,
}

/// Reads at most limit wire bytes; LF/CRLF terminators count, while an EOF-terminated line
/// may use the full limit because EOF adds no byte.
fn read_bounded_line(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    limit: usize,
) -> io::Result<usize> {
    line.clear();
    loop {
        if line.len() == limit {
            if reader.fill_buf()?.is_empty() {
                return Ok(line.len());
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "line exceeds configured limit",
            ));
        }
        let (read, complete, over_limit) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                return Ok(line.len());
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let read = newline.map_or(available.len(), |index| index + 1);
            let remaining = limit - line.len();
            if read > remaining {
                line.extend_from_slice(&available[..remaining]);
                (remaining, false, true)
            } else {
                line.extend_from_slice(&available[..read]);
                (read, newline.is_some(), false)
            }
        };
        reader.consume(read);
        if over_limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "line exceeds configured limit",
            ));
        }
        if complete {
            return Ok(line.len());
        }
    }
}

fn trim_line_ending(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn read_http_header_line(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    remaining: &mut usize,
) -> io::Result<()> {
    let length = read_bounded_line(reader, line, (*remaining).min(MAX_EVENT_HEADER_LINE_BYTES))?;
    if length == 0 || line.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "incomplete SSE response headers",
        ));
    }
    *remaining -= length;
    Ok(())
}

fn read_sse_frame(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut data = Vec::new();
    let mut line = Vec::new();
    let mut frame_bytes = 0;
    let mut frame_lines = 0;
    let mut data_seen = false;
    loop {
        let length = read_bounded_line(
            reader,
            &mut line,
            MAX_SSE_FRAME_BYTES.saturating_sub(frame_bytes),
        )?;
        if length == 0 {
            if data_seen {
                data.pop();
                return Ok(Some(data));
            }
            return Ok(None);
        }
        frame_bytes += length;
        frame_lines += 1;
        if frame_lines > MAX_SSE_FRAME_LINES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SSE frame has too many lines",
            ));
        }
        let text = std::str::from_utf8(trim_line_ending(&line)).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid UTF-8 in SSE stream")
        })?;
        if text.is_empty() {
            if data_seen {
                data.pop();
                return Ok(Some(data));
            }
            frame_bytes = 0;
            frame_lines = 0;
            continue;
        }
        if text.starts_with(':') {
            continue;
        }
        let (field, value) = text.split_once(':').unwrap_or((text, ""));
        if field == "data" {
            let value = value.strip_prefix(' ').unwrap_or(value);
            data.extend_from_slice(value.as_bytes());
            data.push(b'\n');
            data_seen = true;
        }
    }
}

fn event_from_payload(payload: &[u8], bridge: &AgentBridge) -> io::Result<Option<AgentEvent>> {
    let frame = serde_json::from_slice::<SseFrame>(payload)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid SSE event"))?;
    if let Some(location) = &frame.location {
        if !same_directory(Path::new(&location.directory), bridge.directory()) {
            return Ok(None);
        }
    }
    let data = frame.data;
    let session_id = data
        .get("sessionID")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let raw_type = frame.raw_type;
    Ok(Some(AgentEvent {
        checkout_id: bridge.checkout_id.clone(),
        session_id,
        kind: agent_event_kind(&raw_type),
        raw_type,
        data,
    }))
}

/// A small HTTP body reader for `/api/event`. ureq's body timeout is a total-body timeout, which
/// would periodically tear down a healthy event stream. The bridge keeps a clone of this socket
/// so stop can interrupt a silent read without changing the stream's lifetime.
struct SseReader {
    reader: BufReader<TcpStream>,
    chunked: bool,
    remaining_chunk: usize,
    finished: bool,
}

impl Read for SseReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.finished || buffer.is_empty() {
            return Ok(0);
        }
        if self.chunked && self.remaining_chunk == 0 {
            let mut size_line = Vec::new();
            let length = read_bounded_line(
                &mut self.reader,
                &mut size_line,
                MAX_EVENT_HEADER_LINE_BYTES,
            )?;
            if length == 0 || size_line.last() != Some(&b'\n') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "incomplete SSE chunk header",
                ));
            }
            let size_text = std::str::from_utf8(trim_line_ending(&size_line))
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid SSE chunk"))?;
            let size = size_text
                .split(';')
                .next()
                .and_then(|value| usize::from_str_radix(value.trim(), 16).ok())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid SSE chunk"))?;
            if size == 0 {
                let mut trailer_budget = MAX_EVENT_HEADERS_BYTES;
                let mut trailer = Vec::new();
                loop {
                    let length = read_bounded_line(
                        &mut self.reader,
                        &mut trailer,
                        trailer_budget.min(MAX_EVENT_HEADER_LINE_BYTES),
                    )?;
                    if length == 0 {
                        self.finished = true;
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "incomplete SSE chunk trailers",
                        ));
                    }
                    trailer_budget -= length;
                    if trailer == b"\r\n" || trailer == b"\n" {
                        break;
                    }
                }
                self.finished = true;
                return Ok(0);
            }
            self.remaining_chunk = size;
        }

        let limit = if self.chunked {
            buffer.len().min(self.remaining_chunk)
        } else {
            buffer.len()
        };
        let read = self.reader.read(&mut buffer[..limit])?;
        if read == 0 {
            self.finished = true;
            return if self.chunked {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated SSE chunk",
                ))
            } else {
                Ok(0)
            };
        }
        if self.chunked {
            self.remaining_chunk -= read;
            if self.remaining_chunk == 0 {
                let mut crlf = [0; 2];
                self.reader.read_exact(&mut crlf)?;
                if crlf != *b"\r\n" {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid SSE chunk terminator",
                    ));
                }
            }
        }
        Ok(read)
    }
}

fn event_stream(bridge: &AgentBridge) -> io::Result<SseReader> {
    let port = bridge.credentials.endpoint.port;
    let directory = bridge.directory_header()?;
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(10))?;
    let interrupt_socket = stream.try_clone()?;
    stream.set_nodelay(true)?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    write!(
        stream,
        "GET /api/event HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: {}\r\n{}: {directory}\r\nAccept: text/event-stream\r\nConnection: keep-alive\r\n\r\n",
        bridge.auth_header(),
        DIRECTORY_HEADER
    )?;
    if !bridge.install_event_socket(interrupt_socket) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "the event stream was stopped",
        ));
    }

    let mut reader = BufReader::new(stream);
    let mut header_budget = MAX_EVENT_HEADERS_BYTES;
    let mut line = Vec::new();
    read_http_header_line(&mut reader, &mut line, &mut header_budget)?;
    let status_line = std::str::from_utf8(trim_line_ending(&line))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid SSE response"))?;
    let status = status_line
        .split_ascii_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid SSE response"))?;
    if status != 200 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the agent event stream was rejected",
        ));
    }
    let mut chunked = false;
    loop {
        read_http_header_line(&mut reader, &mut line, &mut header_budget)?;
        let header = std::str::from_utf8(trim_line_ending(&line))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid SSE response"))?;
        if header.is_empty() {
            break;
        }
        let (name, value) = header
            .split_once(':')
            .filter(|(name, _)| !name.is_empty())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid SSE response"))?;
        if name.eq_ignore_ascii_case("transfer-encoding")
            && value
                .split(',')
                .any(|value| value.trim().eq_ignore_ascii_case("chunked"))
        {
            chunked = true;
        }
    }
    Ok(SseReader {
        reader,
        chunked,
        remaining_chunk: 0,
        finished: false,
    })
}

/// Why the event reader is opening the stream again.
///
/// These are the cases a reader cannot otherwise tell apart, and only the cause is ever kept: an
/// event frame carries session text, so no description of a failed frame may quote one.
#[derive(Debug)]
enum StreamLoss {
    /// The service closed a stream that had been healthy, which is what a restart looks like.
    Ended,
    /// The stream never opened: nothing answered, or the service refused this bridge.
    Refused(io::Error),
    /// A stream that had opened failed on the wire.
    Broken(io::Error),
    /// A frame arrived that is not an event this reader can dispatch.
    Malformed,
}

impl StreamLoss {
    fn detail(&self) -> String {
        match self {
            StreamLoss::Ended => "the service closed the event stream".to_string(),
            StreamLoss::Refused(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                format!("the service refused the event stream: {error}")
            }
            StreamLoss::Refused(error) => format!("could not open the event stream: {error}"),
            StreamLoss::Broken(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                format!("the event stream ended in the middle of a frame: {error}")
            }
            StreamLoss::Broken(error) => format!("the event stream failed: {error}"),
            StreamLoss::Malformed => {
                "the event stream carried a frame that is not an event".to_string()
            }
        }
    }
}

/// Says why the stream was lost, once per backoff tier.
///
/// Two reasons it is not one line per attempt. The attempt is not news: a service that is down is
/// retried forever, with the backoff already at thirty seconds, and a line each time would bury
/// everything else. And the frame is not news either: the line names what failed and how long the
/// wait was, never what the frame said, because an event payload carries session text.
fn report_stream_loss(
    bridge: &AgentBridge,
    reported: &mut Duration,
    backoff: Duration,
    loss: &StreamLoss,
) {
    if backoff <= *reported {
        return;
    }
    *reported = backoff;
    match loss {
        // A service that restarts closes the stream cleanly, so this is expected rather than a fault.
        StreamLoss::Ended => log::debug!(
            "the agent event stream for {} ended; reconnecting in {backoff:?}",
            bridge.checkout_id
        ),
        _ => log::warn!(
            "the agent event stream for {} was lost: {}; reconnecting in {backoff:?}",
            bridge.checkout_id,
            loss.detail()
        ),
    }
}

/// Turns frames into events until the stream cannot give any more, and says why it stopped.
fn consume_events(bridge: &AgentBridge, mut reader: impl BufRead, sink: &EventSink) -> StreamLoss {
    while !bridge.is_stopped() {
        let payload = match read_sse_frame(&mut reader) {
            Ok(Some(payload)) => payload,
            Ok(None) => return StreamLoss::Ended,
            // A frame this reader rejects on its own terms: the bounds and the encoding, never the
            // connection.
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                return StreamLoss::Malformed
            }
            Err(error) => return StreamLoss::Broken(error),
        };
        match event_from_payload(&payload, bridge) {
            Ok(Some(event)) => sink(event),
            // A frame for another location is not a failure: the stream is shared by every checkout
            // the service knows, and this one is answered by its own bridge.
            Ok(None) => continue,
            Err(_) => return StreamLoss::Malformed,
        }
    }
    StreamLoss::Ended
}

/// Consumes `/api/event` until the server dies, normalizing and forwarding each frame.
///
/// Reconnects with a bounded backoff: a dropped stream is normal (laptop sleep, server
/// restart) and a fresh stream replays no history, so the caller must re-read state after
/// a reconnect rather than assume nothing was missed.
fn read_events(bridge: Arc<AgentBridge>, sink: EventSink) {
    let mut backoff = Duration::from_millis(250);
    let mut reported = Duration::ZERO;
    while !bridge.is_stopped() {
        let loss = match event_stream(&bridge) {
            Ok(reader) => {
                // A stream that came up resets the wait: the service is answering again.
                backoff = Duration::from_millis(250);
                reported = Duration::ZERO;
                let loss = consume_events(&bridge, BufReader::new(reader), &sink);
                bridge.clear_event_socket();
                loss
            }
            Err(error) => {
                bridge.clear_event_socket();
                StreamLoss::Refused(error)
            }
        };
        // A stop is not a disconnect, and reporting it would make every checkout that closes write
        // the same line on the way out.
        if bridge.is_stopped() {
            return;
        }
        report_stream_loss(&bridge, &mut reported, backoff, &loss);
        if !sleep_unless_stopped(&bridge, backoff) {
            return;
        }
        backoff = (backoff * 2).min(MAX_RECONNECT_BACKOFF);
    }
}

/// Sleeps in small slices so a stop request is honoured promptly. Returns false if the
/// bridge was stopped while waiting.
fn sleep_unless_stopped(bridge: &AgentBridge, total: Duration) -> bool {
    let slice = Duration::from_millis(50);
    let mut waited = Duration::ZERO;
    while waited < total {
        if bridge.is_stopped() {
            return false;
        }
        sleep(slice.min(total - waited));
        waited += slice;
    }
    !bridge.is_stopped()
}

/// Reader I/O is interruptible, so the normal path finishes before the deadline. If an
/// unexpected reader does not finish, dropping its handle detaches it rather than blocking the
/// caller. Joining the current event thread would panic; it is already on the return path.
fn join_reader(handle: std::thread::JoinHandle<()>, deadline: Instant) {
    if handle.thread().id() == std::thread::current().id() {
        return;
    }
    while !handle.is_finished() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        sleep(Duration::from_millis(10).min(remaining));
    }
    if handle.is_finished() {
        let _ = handle.join();
    }
}

/// Gives one request the budget its own call asked for, from the bridge's pooled client.
///
/// The budgets are per call and not interchangeable: `ready` probes with whatever is left of its
/// five second deadline, an ordinary read gets the whole request timeout. Fixing one timeout on the
/// client would make one of the two wrong, so the pooled agent carries the connections and each
/// request carries its own deadline.
fn with_timeout<T>(request: ureq::RequestBuilder<T>, timeout: Duration) -> ureq::RequestBuilder<T> {
    request
        .config()
        .timeout_global(Some(timeout))
        .timeout_connect(Some(timeout))
        .build()
}

impl Drop for AgentBridge {
    fn drop(&mut self) {
        self.stop();
    }
}

/// An OpenCode session id is `ses_` plus an opaque suffix. Rejecting anything else keeps a
/// WebView-supplied value from reaching a path we would otherwise interpolate.
fn validate_session_id(session_id: &str) -> Result<(), BridgeError> {
    let valid = session_id.len() > 4
        && session_id.starts_with("ses_")
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(BridgeError::Foreign(
            "that is not a valid OpenCode session id".into(),
        ))
    }
}

/// Waits until the discovered service answers for this checkout, before the bridge is handed out.
///
/// Ready means one thing: `/api/agent` answered inside the deadline. What the catalog holds is not
/// part of it. OpenCode always offers its built-in agents, and the list is empty for the first
/// moments after the service starts while it is still loading its configuration; refusing a bridge
/// over that list would leave someone who just started their service with no bridge at all, and a
/// row with no color of its own already falls back rather than failing.
///
/// The boot window is why the loop exists, and the deadline is the only clock it reads. It reads it
/// once per round, because a reply landing a moment before the deadline and a reply landing after it
/// are the same situation and must not be told apart. For the same reason a round that went
/// unanswered is not a verdict: only the deadline ends the wait, unless the service answered about
/// this bridge rather than about the timing, which is reported as it comes.
fn ready(
    credentials: ServerCredentials,
    deadline: Instant,
) -> Result<ServerCredentials, BridgeError> {
    let probe = AgentBridge {
        checkout_id: String::new(),
        credentials: credentials.clone(),
        client: ureq::Agent::new_with_defaults(),
        event_socket: Mutex::new(None),
        reader: Mutex::new(None),
        stopped: std::sync::atomic::AtomicBool::new(false),
    };
    let mut answered = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            // The one place the outcome is decided: did anything land before the clock ran out.
            return if answered {
                Ok(credentials)
            } else {
                Err(BridgeError::Unavailable(
                    "the OpenCode service did not become ready in time".into(),
                ))
            };
        }
        // /api/agent is cheap and confirms reachability, directory scoping and readiness at
        // once, which is why this route and not another.
        match probe.get_json_with_timeout::<Vec<serde_json::Value>>("/api/agent", remaining) {
            Ok(_) => answered = true,
            // Nothing landed this round, which is what a service that is still booting looks like.
            Err(BridgeError::Unavailable(_)) => {}
            // The service answered and what it said was about this bridge: a refused password, an
            // endpoint that moved, or a reply that is not the envelope. Asking again would only be
            // told the same thing.
            Err(error) => return Err(error),
        }
        sleep(Duration::from_millis(50).min(remaining));
    }
}

/// How many times a `GET` may be repeated after a signal interrupts it. The kernel hands back
/// `EINTR` without consuming anything, and the request has to be made again; more than a couple
/// of attempts would only hide a socket that is genuinely gone.
const INTERRUPTED_READ_ATTEMPTS: u32 = 3;

/// Repeats a read a signal cut short.
///
/// Only `GET` comes through here. An interrupted read leaves the response unread and the
/// request unanswered, so making it again asks for the same thing a second time and changes
/// nothing. A `POST` must not be repeated: the server may already have taken the body, and
/// sending it again would deliver a second prompt.
fn retry_interrupted<T>(
    mut read: impl FnMut() -> Result<T, ureq::Error>,
) -> Result<T, ureq::Error> {
    let mut attempts = INTERRUPTED_READ_ATTEMPTS;
    loop {
        match read() {
            Err(error) if is_interrupted(&error) && attempts > 1 => attempts -= 1,
            result => return result,
        }
    }
}

/// Repeats an interrupted read only while its shared request deadline still has budget.
fn retry_interrupted_until<T>(
    deadline: Instant,
    mut read: impl FnMut(Duration) -> Result<T, ureq::Error>,
) -> Result<Result<T, ureq::Error>, BridgeError> {
    let mut attempts = INTERRUPTED_READ_ATTEMPTS;
    loop {
        let timeout = deadline
            .saturating_duration_since(Instant::now())
            .min(JSON_REQUEST_TIMEOUT);
        if timeout.is_zero() {
            return Err(BridgeError::Unavailable(
                "the agent server request timed out".into(),
            ));
        }
        match read(timeout) {
            Err(error) if is_interrupted(&error) && attempts > 1 => attempts -= 1,
            result => return Ok(result),
        }
    }
}

/// Whether a transport failure is a syscall a signal ended, which is the one error the system
/// expects its caller to try again rather than report.
fn is_interrupted(error: &ureq::Error) -> bool {
    matches!(error, ureq::Error::Io(error) if error.kind() == io::ErrorKind::Interrupted)
}

/// How an interrupted request is reported.
///
/// A signal can end the syscall a request is sitting in, and nothing is lost: no reply arrived and
/// the connection is still the one that was opened. Saying so is more honest than claiming the
/// server cannot be reached, and it is the one error a caller may be tempted to repeat, so the
/// wording is worth keeping in one place.
const INTERRUPTED_REQUEST: &str = "an agent request was interrupted by a signal";

/// Parses the `{data: …}` envelope every route replies with.
fn send_json<T: serde::de::DeserializeOwned>(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<ApiEnvelope<T>, BridgeError> {
    match response {
        Ok(mut response) => {
            let raw = response
                .body_mut()
                .read_to_vec()
                .map_err(|error| match error {
                    ureq::Error::Timeout(_) => {
                        BridgeError::Unavailable("the agent server request timed out".into())
                    }
                    error => BridgeError::Failed(format!("could not read the reply: {error}")),
                })?;
            // The body is deliberately not echoed: it can carry session content.
            serde_json::from_slice(&raw).map_err(|error| {
                BridgeError::Failed(format!("unexpected agent server reply: {error}"))
            })
        }
        // Status errors are reduced to safe, actionable details; their bodies are not echoed. The
        // two that refuse this password are the one status that is about the endpoint rather than
        // about the request: the service answered, and what it refused was what this bridge is.
        Err(ureq::Error::StatusCode(status)) if status == 401 || status == 403 => {
            Err(BridgeError::Stale(status_detail(status)))
        }
        Err(ureq::Error::StatusCode(status)) => Err(BridgeError::Failed(status_detail(status))),
        Err(ureq::Error::Timeout(_)) => Err(BridgeError::Unavailable(
            "the agent server request timed out".into(),
        )),
        // A signal ended the syscall. Nothing was answered, so this says so rather than claiming
        // the server is gone, and keeps the cause for whoever reads it.
        Err(error) if is_interrupted(&error) => Err(BridgeError::Unavailable(format!(
            "{INTERRUPTED_REQUEST}: {error}"
        ))),
        // Nothing answered on the address this bridge holds, which is what a service restarted on
        // another port or another password looks like from here. A timeout is deliberately not this
        // one: a service busy answering a turn still answers, it just answers late.
        Err(error) => Err(BridgeError::Stale(format!(
            "the agent server is not reachable: {error}"
        ))),
    }
}

fn status_detail(status: u16) -> String {
    match status {
        404 => "the agent server does not support that request in this version".to_string(),
        401 | 403 => "the agent server rejected Muster's credentials".to_string(),
        other => format!("the agent server answered {other}"),
    }
}

type BridgeLauncher = dyn Fn(&str, &Path) -> Result<Arc<AgentBridge>, BridgeError> + Send + Sync;
/// Repeats discovery, which is the only way to find a service that has been restarted on another
/// port or with another password while this app was holding a bridge to the previous one.
type EndpointFinder = dyn Fn() -> Result<ServiceEndpoint, String> + Send + Sync;
type ReaderStarter = dyn Fn(&Arc<AgentBridge>, &EventSink) + Send + Sync;
type BridgeStopper = dyn Fn(&Arc<AgentBridge>, Duration) + Send + Sync;
type SlotStopper = dyn Fn(Arc<BridgeSlot>, Duration) + Send + Sync;

#[derive(Clone, Copy, PartialEq, Eq)]
enum TrackedTurn {
    Pending,
    Started,
}

#[derive(Default)]
struct RemovalState {
    active: HashSet<String>,
    generations: HashMap<String, u64>,
}

/// Keep delivery serialized with epoch changes so an old event cannot cross a reopen.
fn generation_scoped_sink(
    state: Arc<Mutex<RemovalState>>,
    checkout_id: String,
    generation: u64,
    sink: EventSink,
) -> EventSink {
    Arc::new(move |event| {
        if let Ok(state) = state.lock() {
            let current = !state.active.contains(&checkout_id)
                && state
                    .generations
                    .get(&checkout_id)
                    .copied()
                    .unwrap_or_default()
                    == generation;
            if current {
                sink(event);
            }
        }
    })
}

pub(crate) struct WorktreeRemovalGuard {
    state: Arc<Mutex<RemovalState>>,
    checkout_id: String,
    committed: bool,
}

impl WorktreeRemovalGuard {
    /// Invalidate queued requests atomically before releasing the removal reservation.
    pub(crate) fn commit(mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.active.remove(&self.checkout_id);
            let generation = state
                .generations
                .entry(self.checkout_id.clone())
                .or_default();
            *generation = generation.wrapping_add(1);
        }
        self.committed = true;
    }
}

impl Drop for WorktreeRemovalGuard {
    fn drop(&mut self) {
        if !self.committed {
            if let Ok(mut state) = self.state.lock() {
                state.active.remove(&self.checkout_id);
            }
        }
    }
}

enum BridgeState {
    Starting { stop_requested: bool },
    Ready(Arc<AgentBridge>),
    Failed(BridgeError),
    Stopping,
    Stopped,
}

struct BridgeSlot {
    state: Mutex<BridgeState>,
    changed: Condvar,
}

fn remove_slot_if_current(
    bridges: &mut HashMap<String, Arc<BridgeSlot>>,
    checkout_id: &str,
    slot: &Arc<BridgeSlot>,
) {
    if bridges
        .get(checkout_id)
        .is_some_and(|current| Arc::ptr_eq(current, slot))
    {
        bridges.remove(checkout_id);
    }
}

impl BridgeSlot {
    fn starting() -> Self {
        Self {
            state: Mutex::new(BridgeState::Starting {
                stop_requested: false,
            }),
            changed: Condvar::new(),
        }
    }

    fn wait_with_timeout(&self, timeout: Duration) -> Result<Arc<AgentBridge>, BridgeError> {
        let deadline = Instant::now() + timeout;
        let mut state = self
            .state
            .lock()
            .map_err(|_| BridgeError::Failed("the agent bridge state is poisoned".into()))?;
        loop {
            match &*state {
                BridgeState::Ready(bridge) => return Ok(Arc::clone(bridge)),
                BridgeState::Failed(error) => return Err(error.clone()),
                BridgeState::Stopped => {
                    return Err(BridgeError::Failed(
                        "the agent bridge was stopped while starting".into(),
                    ))
                }
                BridgeState::Starting { .. } | BridgeState::Stopping => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(BridgeError::Unavailable(
                            "timed out waiting for the agent bridge to start".into(),
                        ));
                    }
                    let (next, timed_out) =
                        self.changed.wait_timeout(state, remaining).map_err(|_| {
                            BridgeError::Failed("the agent bridge state is poisoned".into())
                        })?;
                    state = next;
                    if timed_out.timed_out()
                        && matches!(
                            &*state,
                            BridgeState::Starting { .. } | BridgeState::Stopping
                        )
                    {
                        return Err(BridgeError::Unavailable(
                            "timed out waiting for the agent bridge to start".into(),
                        ));
                    }
                }
            }
        }
    }

    /// Signals a published child immediately, or records cancellation for an in-flight start.
    fn signal(&self) {
        let bridge = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            match &mut *state {
                BridgeState::Starting { stop_requested } => {
                    *stop_requested = true;
                    self.changed.notify_all();
                    return;
                }
                BridgeState::Ready(bridge) => Some(Arc::clone(bridge)),
                BridgeState::Failed(_) | BridgeState::Stopping | BridgeState::Stopped => None,
            }
        };
        if let Some(bridge) = bridge {
            bridge.signal_stop();
        }
    }

    fn stop(&self, stopper: &BridgeStopper, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        self.signal();
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        loop {
            match &mut *state {
                BridgeState::Starting { stop_requested } => {
                    *stop_requested = true;
                    self.changed.notify_all();
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        *state = BridgeState::Stopped;
                        self.changed.notify_all();
                        return;
                    }
                    let (next, timed_out) = match self.changed.wait_timeout(state, remaining) {
                        Ok(wait) => wait,
                        Err(_) => return,
                    };
                    state = next;
                    if timed_out.timed_out() && matches!(*state, BridgeState::Starting { .. }) {
                        *state = BridgeState::Stopped;
                        self.changed.notify_all();
                        return;
                    }
                }
                BridgeState::Stopping => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        *state = BridgeState::Stopped;
                        self.changed.notify_all();
                        return;
                    }
                    let (next, timed_out) = match self.changed.wait_timeout(state, remaining) {
                        Ok(wait) => wait,
                        Err(_) => return,
                    };
                    state = next;
                    if timed_out.timed_out() && matches!(*state, BridgeState::Stopping) {
                        *state = BridgeState::Stopped;
                        self.changed.notify_all();
                        return;
                    }
                }
                BridgeState::Ready(bridge) => {
                    let bridge = Arc::clone(bridge);
                    *state = BridgeState::Stopping;
                    drop(state);
                    stopper(&bridge, deadline.saturating_duration_since(Instant::now()));
                    if let Ok(mut state) = self.state.lock() {
                        *state = BridgeState::Stopped;
                        self.changed.notify_all();
                    }
                    return;
                }
                BridgeState::Failed(_) | BridgeState::Stopped => return,
            }
        }
    }
}

/// One read, and when it was taken, so a question asked twice in one tick costs one request.
#[derive(Debug, Clone)]
struct Cached<T> {
    read_at: Instant,
    value: T,
}

impl<T> Cached<T> {
    fn taken(read_at: Instant, value: T) -> Self {
        Self { read_at, value }
    }

    fn within(&self, ttl: Duration) -> bool {
        self.read_at.elapsed() < ttl
    }
}

/// The service's session list, with its running and pending-reply membership.
///
/// The snapshot covers every candidate location, which makes one read of it
/// the answer for every checkout: `running` in particular is a property of a turn, not of a
/// worktree. The arcs are what let the read be shared without being copied per checkout.
#[derive(Debug, Clone)]
struct CandidateRead {
    sessions: Arc<Vec<ApiSession>>,
    running: Arc<HashSet<String>>,
    awaiting: Arc<HashSet<String>>,
    unknown_pending: Arc<HashSet<PathBuf>>,
    pending_failures: Arc<Vec<PendingReadFailure>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PendingReadFailure {
    Timeout,
    Http,
    Scope,
    Decode,
    Transport,
}

impl PendingReadFailure {
    fn classify(error: &BridgeError) -> Self {
        match error {
            BridgeError::Unavailable(message) if message.contains("timed out") => Self::Timeout,
            BridgeError::Unavailable(_) => Self::Transport,
            BridgeError::Stale(message) if message.contains("rejected Muster's credentials") => {
                Self::Http
            }
            BridgeError::Stale(_) => Self::Transport,
            BridgeError::Foreign(_) => Self::Scope,
            BridgeError::Failed(message)
                if message.starts_with("unexpected agent server reply:") =>
            {
                Self::Decode
            }
            BridgeError::Failed(message) if message.starts_with("could not read the reply:") => {
                Self::Transport
            }
            BridgeError::Failed(_) => Self::Http,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Http => "http",
            Self::Scope => "scope",
            Self::Decode => "decode",
            Self::Transport => "transport",
        }
    }
}

fn report_pending_read_failures(failures: &[PendingReadFailure]) {
    let summary = [
        PendingReadFailure::Timeout,
        PendingReadFailure::Http,
        PendingReadFailure::Scope,
        PendingReadFailure::Decode,
        PendingReadFailure::Transport,
    ]
    .into_iter()
    .filter_map(|kind| {
        let count = failures.iter().filter(|failure| **failure == kind).count();
        (count > 0).then(|| format!("{}={count}", kind.label()))
    })
    .collect::<Vec<_>>()
    .join(", ");
    // Report only safe categories and counts; never the path, request, or server text.
    log::warn!("candidate pending reads failed ({summary}); affected sessions remain unknown");
}

/// The service-wide list, and the read that is answering for it right now.
///
/// `reading` is what keeps one poll for nine checkouts from turning into nine identical reads: the
/// first caller publishes itself, releases the lock and goes to the network, and the rest find that
/// read and wait for its answer. Nothing else happens under the lock: network requests must not
/// block every other reader of the cache, least of all the event reader, which can reach it from its
/// own thread.
struct CandidateSlot {
    cached: Option<Cached<CandidateRead>>,
    reading: Option<Arc<CandidateFetch>>,
    last_pending_failures: Option<Vec<PendingReadFailure>>,
}

impl CandidateSlot {
    fn new() -> Self {
        Self {
            cached: None,
            reading: None,
            last_pending_failures: None,
        }
    }

    fn pending_failures_to_report(
        &mut self,
        failures: &[PendingReadFailure],
    ) -> Option<Vec<PendingReadFailure>> {
        if failures.is_empty() {
            self.last_pending_failures = None;
            None
        } else if self.last_pending_failures.as_deref() == Some(failures) {
            None
        } else {
            self.last_pending_failures = Some(failures.to_vec());
            Some(failures.to_vec())
        }
    }
}

/// One read in flight and the answer every caller that arrived while it ran is waiting for.
struct CandidateFetch {
    outcome: Mutex<Option<Result<CandidateRead, BridgeError>>>,
    published: Condvar,
}

impl CandidateFetch {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            outcome: Mutex::new(None),
            published: Condvar::new(),
        })
    }

    /// Hands the answer to the waiters. Only the first publication is the answer: this read is over
    /// once, whoever is still waiting is waiting for what it decided.
    fn publish(&self, outcome: Result<CandidateRead, BridgeError>) {
        if let Ok(mut published) = self.outcome.lock() {
            if published.is_none() {
                *published = Some(outcome);
            }
        }
        self.published.notify_all();
    }

    /// The answer of the read that is already running, shared rather than repeated.
    ///
    /// Bounded so a coalesced caller cannot wait forever. Expiry releases only that caller; it does
    /// not cancel the read that is still answering for the cache.
    fn wait(&self) -> Result<CandidateRead, BridgeError> {
        let deadline = Instant::now() + CANDIDATE_READ_WAIT;
        let mut published = self
            .outcome
            .lock()
            .map_err(|_| BridgeError::Failed("the agent candidate read is poisoned".into()))?;
        loop {
            if let Some(outcome) = published.as_ref() {
                return outcome.clone();
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(BridgeError::Unavailable(
                    "timed out waiting for the shared agent session list".into(),
                ));
            }
            let (next, _) = self
                .published
                .wait_timeout(published, remaining)
                .map_err(|_| BridgeError::Failed("the agent candidate read is poisoned".into()))?;
            published = next;
        }
    }
}

/// How long a coalesced caller waits for the service-wide candidate read.
///
/// This bounds the caller, not the owner: expiry does not cancel pending location lookups or prevent
/// the completed snapshot from being published.
const CANDIDATE_READ_WAIT: Duration =
    Duration::from_secs(JSON_REQUEST_TIMEOUT.as_secs() * 2 * INTERRUPTED_READ_ATTEMPTS as u64);

/// How many candidate locations are asked for their pending requests at once.
///
/// Bounded rather than one thread per location: every read here is an open request against the
/// person's service, and a candidate list can name every worktree they have. A handful at a time is
/// enough for no healthy location to wait on a slow one, which is the whole point.
const PENDING_READ_CONCURRENCY: usize = 4;

/// The total deadline for a candidate snapshot, including its service-wide list, activity and pending reads.
/// Kept under `CANDIDATE_READ_WAIT` so the owner finishes before a coalesced caller gives up.
const PENDING_READ_BUDGET: Duration = JSON_REQUEST_TIMEOUT;

/// Every candidate location's pending sessions, locations that could not say, and safe failure kinds.
///
/// Concurrent and bounded, so one location a service is slow to answer no longer holds these reads:
/// the healthy locations behind it are read at the same time rather than after it. Every pending
/// endpoint shares one absolute deadline; a location that cannot answer in time remains `unknown`
/// rather than idle — `awaiting_reply: None` draws a row with no state, where `Some(false)` could
/// clear the sidebar's rows. Titles are never touched, because ambiguity matching needs every
/// candidate whether or not it answered.
fn read_pending(
    bridge: &AgentBridge,
    directories: &HashSet<PathBuf>,
    deadline: Instant,
) -> (HashSet<String>, HashSet<PathBuf>, Vec<PendingReadFailure>) {
    let queue: Vec<&Path> = directories.iter().map(PathBuf::as_path).collect();
    // Shared rather than partitioned: a worker stuck on a slow location must not also hold the
    // healthy ones it would otherwise have picked up after it.
    let cursor = AtomicUsize::new(0);
    let (answered, awaiting, mut failures) = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..PENDING_READ_CONCURRENCY.min(queue.len()))
            .map(|_| {
                let (queue, cursor) = (&queue, &cursor);
                scope.spawn(move || {
                    let mut answered = Vec::new();
                    let mut awaiting = HashSet::new();
                    let mut failures = Vec::new();
                    while let Some(directory) = queue.get(cursor.fetch_add(1, Ordering::Relaxed)) {
                        // Out of budget: leaving the rest on the queue is how they are marked
                        // unknown, and asking with nothing left is not a read of anything.
                        if deadline <= Instant::now() {
                            break;
                        }
                        match bridge.awaiting_sessions_until(directory, deadline) {
                            Ok(pending) => {
                                awaiting.extend(pending);
                                answered.push(directory.to_path_buf());
                            }
                            Err(error) => failures.push(PendingReadFailure::classify(&error)),
                        }
                    }
                    (answered, awaiting, failures)
                })
            })
            .collect();
        let mut answered = HashSet::new();
        let mut awaiting = HashSet::new();
        let mut failures = Vec::new();
        for worker in workers {
            // A worker that panicked reported nothing, so its location stays unanswered and unknown.
            let Ok((found, pending, failed)) = worker.join() else {
                continue;
            };
            answered.extend(found);
            awaiting.extend(pending);
            failures.extend(failed);
        }
        (answered, awaiting, failures)
    });
    failures.sort_unstable();
    let unknown = directories.difference(&answered).cloned().collect();
    (awaiting, unknown, failures)
}

/// The service-wide candidate list, running state and pending requests at its candidate locations.
fn read_candidates(bridge: &AgentBridge, budget: Duration) -> Result<CandidateRead, BridgeError> {
    let deadline = Instant::now() + budget;
    let sessions: Vec<ApiSession> = bridge.get_unscoped_json(
        &format!("/api/session?limit={CANDIDATE_SESSION_LIMIT}"),
        deadline.saturating_duration_since(Instant::now()),
    )?;
    let running = bridge.running_sessions_until(deadline)?;
    let directories: HashSet<PathBuf> = sessions
        .iter()
        .map(|session| {
            session
                .location
                .as_ref()
                .map(|location| PathBuf::from(location.directory.as_str()))
                .unwrap_or_else(|| bridge.directory().to_path_buf())
        })
        .collect();
    let (awaiting, unknown_pending, pending_failures) =
        read_pending(bridge, &directories, deadline);
    Ok(CandidateRead {
        sessions: Arc::new(sessions),
        running: Arc::new(running),
        awaiting: Arc::new(awaiting),
        unknown_pending: Arc::new(unknown_pending),
        pending_failures: Arc::new(pending_failures),
    })
}

/// Tool work can move between worktrees without moving the OpenCode session itself.
/// Keep that effective location until newer attributable work or an explicit session move.
#[derive(Clone)]
struct SessionWork {
    origin: PathBuf,
    effective: Option<PathBuf>,
    targets: Vec<PathBuf>,
    completed: i64,
    updated: i64,
    running: bool,
    seen: Instant,
}

/// Keep history reads moving through the bounded newest-session page across polling ticks.
fn rotate_sessions_after(sessions: &mut [ApiSession], cursor: Option<&str>) {
    if let Some(index) = cursor.and_then(|cursor| sessions.iter().position(|s| s.id == cursor)) {
        sessions.rotate_left(index + 1);
    }
}

use relocation::{latest_work, CheckoutIndex};

#[cfg(test)]
fn work_directory(checkouts: &[(String, PathBuf)], targets: &[PathBuf]) -> Option<PathBuf> {
    CheckoutIndex::new(checkouts).work_directory(targets)
}

/// The last worktree this panel could name for a session, and when it was last read.
///
/// `checkout_id` is kept even while the session's directory is one this panel has no checkout for.
/// That is the whole point: a session that walks into a worktree Muster has not registered yet is
/// the commonest case there is, and forgetting where it was makes that worktree look like a
/// session appearing for the first time, which is never a move.
#[derive(Debug, Clone)]
struct SessionLocation {
    /// The last checkout the session was seen in, held while its directory is not one of them.
    checkout_id: Option<String>,
    seen: Instant,
}

/// A move that has been reported but might not have arrived.
///
/// Delivery is not something the reader can be trusted with: a caller that throws its answer away,
/// or a window that goes away mid-poll, would lose the move for good if the record were deleted
/// the moment it was handed over. So a reported move stays here and is handed over again, and only
/// drops when the session moves on or when [`PENDING_MOVES_TTL`] says nobody collected it.
#[derive(Debug, Clone)]
struct PendingMove {
    relocation: AgentRelocation,
    since: Instant,
}

/// Owns one server per checkout for the app's lifetime.
pub struct AgentService {
    bridges: Mutex<HashMap<String, Arc<BridgeSlot>>>,
    checkout_operations: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// The service-wide session list, read once and answered from for every checkout's rows.
    candidates: Mutex<CandidateSlot>,
    /// The agent catalogs, per checkout: `/api/agent` answers for one directory, so a cache wider
    /// than that would hand one project's agents to another project's row. Measured against the
    /// real service: Muster offers eight, a sibling repository eleven, three of them its own.
    catalogs: Mutex<HashMap<String, Cached<Arc<Vec<AgentAgent>>>>>,
    removal_state: Arc<Mutex<RemovalState>>,
    busy_turns: Arc<Mutex<HashMap<String, HashMap<String, TrackedTurn>>>>,
    /// Where each session was at the last relocation read, keyed by session id. See
    /// [`AgentService::relocations`], which is the only thing that reads or writes it.
    session_locations: Mutex<HashMap<String, SessionLocation>>,
    session_work: Mutex<HashMap<String, SessionWork>>,
    relocation_snapshot: Mutex<()>,
    /// Last session whose history was read, for fair reads across the newest-page limit.
    relocation_history_cursor: Mutex<Option<String>>,
    /// Moves that have been reported and might not have arrived. See [`PendingMove`].
    pending_moves: Mutex<HashMap<String, PendingMove>>,
    sink: Mutex<Option<EventSink>>,
    launcher: Arc<BridgeLauncher>,
    finder: Arc<EndpointFinder>,
    reader_starter: Arc<ReaderStarter>,
    stopper: Arc<BridgeStopper>,
    slot_stopper: Arc<SlotStopper>,
    startup_stop_wait: Duration,
    startup_wait: Duration,
    /// The shared deadline for per-location pending form/permission reads only. The service-wide
    /// session and activity reads happen before it starts; this is not a whole-snapshot cap.
    /// A field so a test can spend less of it than the shipped request timeout.
    pending_read_budget: Duration,
}

impl AgentService {
    /// Finds the service the user runs. `home` is where that service registers itself, and it is
    /// kept for the life of the app so every later connect looks in the same place.
    pub fn new(home: PathBuf) -> Self {
        let finder_home = home.clone();
        let mut service = Self::with_lifecycle(
            Arc::new(move |checkout_id, directory| {
                AgentBridge::connect(&home, checkout_id, directory).map(Arc::new)
            }),
            Arc::new(|bridge, sink| bridge.start_reader(sink)),
            Arc::new(|bridge, timeout| bridge.stop_with_timeout(timeout)),
            // The same registration `connect` reads: an endpoint that stops answering has to be
            // looked up again, because the file it came from is rewritten by every start.
            Arc::new(move || opencode::discover(&finder_home)),
            STARTUP_STOP_WAIT,
        );
        service.startup_wait = STARTUP_WAIT_TIMEOUT;
        service
    }

    fn with_lifecycle(
        launcher: Arc<BridgeLauncher>,
        reader_starter: Arc<ReaderStarter>,
        stopper: Arc<BridgeStopper>,
        finder: Arc<EndpointFinder>,
        startup_stop_wait: Duration,
    ) -> Self {
        let slot_stopper_callback = Arc::clone(&stopper);
        Self::with_lifecycle_and_slot_stopper(
            launcher,
            reader_starter,
            stopper,
            finder,
            startup_stop_wait,
            Arc::new(move |slot, timeout| {
                slot.stop(slot_stopper_callback.as_ref(), timeout);
            }),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_test_server(port: u16) -> Self {
        let endpoint = ServiceEndpoint {
            url: format!("http://127.0.0.1:{port}"),
            port,
            password: "test".into(),
        };
        let found = endpoint.clone();
        Self::with_lifecycle(
            Arc::new(move |checkout_id, directory| {
                Ok(Arc::new(AgentBridge {
                    checkout_id: checkout_id.to_string(),
                    credentials: ServerCredentials {
                        endpoint: endpoint.clone(),
                        directory: directory.to_path_buf(),
                    },
                    client: ureq::Agent::new_with_defaults(),
                    event_socket: Mutex::new(None),
                    reader: Mutex::new(None),
                    stopped: std::sync::atomic::AtomicBool::new(false),
                }))
            }),
            Arc::new(|_, _| {}),
            Arc::new(|bridge, timeout| bridge.stop_with_timeout(timeout)),
            // A fixture that does not move: discovery finds the port it already serves, so a
            // refused request retires nothing.
            Arc::new(move || Ok(found.clone())),
            Duration::from_millis(50),
        )
    }

    fn with_lifecycle_and_slot_stopper(
        launcher: Arc<BridgeLauncher>,
        reader_starter: Arc<ReaderStarter>,
        stopper: Arc<BridgeStopper>,
        finder: Arc<EndpointFinder>,
        startup_stop_wait: Duration,
        slot_stopper: Arc<SlotStopper>,
    ) -> Self {
        Self {
            bridges: Mutex::new(HashMap::new()),
            checkout_operations: Mutex::new(HashMap::new()),
            candidates: Mutex::new(CandidateSlot::new()),
            catalogs: Mutex::new(HashMap::new()),
            removal_state: Arc::new(Mutex::new(RemovalState::default())),
            busy_turns: Arc::new(Mutex::new(HashMap::new())),
            session_locations: Mutex::new(HashMap::new()),
            session_work: Mutex::new(HashMap::new()),
            relocation_snapshot: Mutex::new(()),
            relocation_history_cursor: Mutex::new(None),
            pending_moves: Mutex::new(HashMap::new()),
            sink: Mutex::new(None),
            launcher,
            finder,
            reader_starter,
            stopper,
            slot_stopper,
            startup_stop_wait,
            startup_wait: startup_stop_wait,
            pending_read_budget: PENDING_READ_BUDGET,
        }
    }

    /// Sets where normalized events are delivered. Without one the bridges still work,
    /// they just do not report live turn state.
    pub fn set_event_sink(&self, sink: EventSink) {
        let tracked: EventSink = {
            let forward = Arc::clone(&sink);
            let busy_turns = Arc::clone(&self.busy_turns);
            Arc::new(move |event| {
                if let Some(session_id) = event.session_id.as_deref() {
                    if let Ok(mut turns) = busy_turns.lock() {
                        let checkout = turns.entry(event.checkout_id.clone()).or_default();
                        match event.kind {
                            crate::domain::agent::AgentEventKind::TurnStarted
                            | crate::domain::agent::AgentEventKind::TurnFinished
                            | crate::domain::agent::AgentEventKind::PermissionAsked
                            | crate::domain::agent::AgentEventKind::QuestionAsked => {
                                checkout.insert(session_id.to_string(), TrackedTurn::Started);
                            }
                            crate::domain::agent::AgentEventKind::TurnFailed => {
                                checkout.remove(session_id);
                            }
                            _ => {}
                        }
                        if checkout.is_empty() {
                            turns.remove(&event.checkout_id);
                        }
                    }
                }
                forward(event);
            })
        };
        if let Ok(mut slot) = self.sink.lock() {
            *slot = Some(tracked);
        }
    }

    /// A service that finds no OpenCode, for the tests that need a holder for generations and
    /// removal state without talking to anything.
    #[cfg(test)]
    pub(crate) fn without_service() -> Self {
        Self::new(PathBuf::from("/muster-no-opencode-service"))
    }

    fn checkout_operation_lock(&self, checkout_id: &str) -> Arc<Mutex<()>> {
        let mut operations = self
            .checkout_operations
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Arc::clone(
            operations
                .entry(checkout_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    pub(crate) fn with_checkout_operation<T>(
        &self,
        checkout_id: &str,
        operation: impl FnOnce() -> T,
    ) -> T {
        let lock = self.checkout_operation_lock(checkout_id);
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        operation()
    }

    /// Serializes an operation that changes ownership between checkouts. Sorting makes opposite
    /// moves acquire the same locks in the same order, while a removal's single-checkout lock still
    /// excludes either side of the move.
    pub(crate) fn with_checkout_operations<T>(
        &self,
        checkout_ids: &[&str],
        operation: impl FnOnce() -> T,
    ) -> T {
        let mut checkout_ids = checkout_ids.to_vec();
        checkout_ids.sort_unstable();
        checkout_ids.dedup();
        let locks = checkout_ids
            .iter()
            .map(|checkout_id| self.checkout_operation_lock(checkout_id))
            .collect::<Vec<_>>();
        let _guards = locks
            .iter()
            .map(|lock| lock.lock().unwrap_or_else(|error| error.into_inner()))
            .collect::<Vec<_>>();
        operation()
    }

    fn ensure_checkout_not_removing(&self, checkout_id: &str) -> Result<(), BridgeError> {
        let state = self
            .removal_state
            .lock()
            .map_err(|_| BridgeError::Failed("the agent removal state is poisoned".into()))?;
        if state.active.contains(checkout_id) {
            return Err(BridgeError::Unavailable(
                "this checkout is being removed".into(),
            ));
        }
        Ok(())
    }

    /// Snapshot before command path lookup / async dispatch; revalidate after taking its lock.
    pub(crate) fn checkout_generation(&self, checkout_id: &str) -> Result<u64, BridgeError> {
        let state = self
            .removal_state
            .lock()
            .map_err(|_| BridgeError::Failed("the agent removal state is poisoned".into()))?;
        if state.active.contains(checkout_id) {
            return Err(BridgeError::Unavailable(
                "this checkout is being removed".into(),
            ));
        }
        Ok(state
            .generations
            .get(checkout_id)
            .copied()
            .unwrap_or_default())
    }

    /// Read the current epoch even while a removal reservation is held by its owner.
    pub(crate) fn checkout_epoch(&self, checkout_id: &str) -> Result<u64, BridgeError> {
        let state = self
            .removal_state
            .lock()
            .map_err(|_| BridgeError::Failed("the agent removal state is poisoned".into()))?;
        Ok(state
            .generations
            .get(checkout_id)
            .copied()
            .unwrap_or_default())
    }

    /// Revalidate the request epoch and current DB registration under the checkout operation lock.
    pub(crate) fn with_checkout_generation<T>(
        &self,
        checkout_id: &str,
        generation: u64,
        is_registered: impl Fn() -> bool,
        operation: impl FnOnce() -> Result<T, BridgeError>,
    ) -> Result<T, BridgeError> {
        self.with_checkout_operation(checkout_id, || {
            if self.checkout_generation(checkout_id)? != generation {
                return Err(BridgeError::Unavailable(
                    "the checkout changed while the agent request was queued; retry it".into(),
                ));
            }
            if !is_registered() {
                return Err(BridgeError::Unavailable(
                    "this checkout is no longer registered".into(),
                ));
            }
            operation()
        })
    }

    pub(crate) fn reserve_worktree_removal(
        &self,
        checkout_id: &str,
    ) -> Result<WorktreeRemovalGuard, BridgeError> {
        {
            let mut state = self
                .removal_state
                .lock()
                .map_err(|_| BridgeError::Failed("the agent removal state is poisoned".into()))?;
            if !state.active.insert(checkout_id.to_string()) {
                return Err(BridgeError::Unavailable(
                    "this checkout is already being removed".into(),
                ));
            }
        }
        let removal = WorktreeRemovalGuard {
            state: Arc::clone(&self.removal_state),
            checkout_id: checkout_id.to_string(),
            committed: false,
        };
        let slot = self
            .bridges
            .lock()
            .map_err(|_| BridgeError::Failed("the agent service is poisoned".into()))?
            .get(checkout_id)
            .cloned();
        if let Some(slot) = slot {
            let state = slot
                .state
                .lock()
                .map_err(|_| BridgeError::Failed("the agent bridge state is poisoned".into()))?;
            if matches!(
                &*state,
                BridgeState::Starting { .. } | BridgeState::Stopping | BridgeState::Stopped
            ) {
                return Err(BridgeError::Unavailable(
                    "the agent bridge is starting or stopping; stop it and retry worktree removal"
                        .into(),
                ));
            }
        }
        Ok(removal)
    }

    /// Busy sessions observed by the real checkout bridge; does not start a bridge.
    pub(crate) fn active_worktree_agent_sessions(
        &self,
        checkout_id: &str,
    ) -> Result<Vec<AgentSession>, BridgeError> {
        let slot = self
            .bridges
            .lock()
            .map_err(|_| BridgeError::Failed("the agent service is poisoned".into()))?
            .get(checkout_id)
            .cloned();
        let Some(slot) = slot else {
            return Ok(Vec::new());
        };
        let bridge = {
            let state = slot
                .state
                .lock()
                .map_err(|_| BridgeError::Failed("the agent bridge state is poisoned".into()))?;
            match &*state {
                BridgeState::Ready(bridge) => Arc::clone(bridge),
                BridgeState::Failed(_) => return Ok(Vec::new()),
                BridgeState::Starting { .. } | BridgeState::Stopping | BridgeState::Stopped => {
                    return Err(BridgeError::Unavailable(
                        "the agent bridge is starting or stopping; retry worktree removal".into(),
                    ));
                }
            }
        };
        let (listed, awaiting, running): (
            Vec<ApiSession>,
            HashSet<String>,
            HashMap<String, ApiActiveSession>,
        ) = self.read_and_refresh(checkout_id, &bridge, || {
            let listed = bridge.get_json_with_timeout("/api/session", Duration::from_secs(5))?;
            let awaiting = bridge.awaiting_sessions(bridge.directory(), Duration::from_secs(5))?;
            let running: HashMap<String, ApiActiveSession> =
                bridge.get_json_with_timeout("/api/session/active", Duration::from_secs(5))?;
            Ok((listed, awaiting, running))
        })?;
        let turns = self
            .busy_turns
            .lock()
            .map_err(|_| BridgeError::Failed("the agent activity state is poisoned".into()))?;
        let mut tracked = turns.get(checkout_id).cloned().unwrap_or_default();
        drop(turns);
        let mut seen = HashSet::new();
        let mut idle = HashSet::new();
        let mut active = Vec::new();
        for raw in listed {
            if raw.check_scope(bridge.directory()).is_err() {
                continue;
            }
            seen.insert(raw.id.clone());
            let is_running = running
                .get(&raw.id)
                .is_some_and(|session| session.kind == "running");
            if awaiting.contains(&raw.id) || is_running {
                active.push(bridge.to_agent_session(&raw, is_running, awaiting.contains(&raw.id)));
                continue;
            }
            match tracked.get(&raw.id) {
                Some(TrackedTurn::Pending) => {
                    active.push(bridge.to_agent_session(&raw, true, false))
                }
                Some(TrackedTurn::Started) if raw.time.idle.is_none() => {
                    active.push(bridge.to_agent_session(&raw, true, false));
                }
                Some(TrackedTurn::Started) => {
                    idle.insert(raw.id.clone());
                }
                None => {}
            }
        }
        for (session_id, state) in &tracked {
            if *state == TrackedTurn::Pending && !seen.contains(session_id) {
                active.push(AgentSession {
                    id: session_id.clone(),
                    checkout_id: checkout_id.to_string(),
                    provider: AgentProvider::OpenCode,
                    title: "OpenCode prompt status is unknown".into(),
                    idle_at: None,
                    awaiting_reply: Some(false),
                    agent: None,
                    model: None,
                    parent_id: None,
                    running: true,
                    outcome: None,
                    created_at: 0,
                    updated_at: 0,
                });
            }
        }
        tracked.retain(|session_id, state| match state {
            TrackedTurn::Pending => true,
            TrackedTurn::Started => seen.contains(session_id) && !idle.contains(session_id),
        });
        if let Ok(mut turns) = self.busy_turns.lock() {
            if tracked.is_empty() {
                turns.remove(checkout_id);
            } else {
                turns.insert(checkout_id.to_string(), tracked);
            }
        }
        Ok(active)
    }

    pub(crate) fn stop_for_worktree_removal(&self, checkout_id: &str) {
        self.stop_locked(checkout_id);
    }

    /// Returns the checkout's bridge, starting its server on first use.
    pub fn bridge(
        &self,
        checkout_id: &str,
        directory: &Path,
    ) -> Result<Arc<AgentBridge>, BridgeError> {
        // Reserve this checkout under the global lock, then boot and create its reader only
        // after releasing it. Stops likewise remove slots before killing or joining anything.
        let removal_state = self
            .removal_state
            .lock()
            .map_err(|_| BridgeError::Failed("the agent removal state is poisoned".into()))?;
        if removal_state.active.contains(checkout_id) {
            return Err(BridgeError::Unavailable(
                "this checkout is being removed".into(),
            ));
        }
        let generation = removal_state
            .generations
            .get(checkout_id)
            .copied()
            .unwrap_or_default();
        let (slot, is_starter) = {
            let mut bridges = self
                .bridges
                .lock()
                .map_err(|_| BridgeError::Failed("the agent service is poisoned".into()))?;
            match bridges.get(checkout_id) {
                Some(slot) => (Arc::clone(slot), false),
                None => {
                    let slot = Arc::new(BridgeSlot::starting());
                    bridges.insert(checkout_id.to_string(), Arc::clone(&slot));
                    (slot, true)
                }
            }
        };
        drop(removal_state);

        if !is_starter {
            return slot.wait_with_timeout(self.startup_wait);
        }

        let sink = self
            .sink
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(Arc::clone))
            .map(|sink| {
                generation_scoped_sink(
                    Arc::clone(&self.removal_state),
                    checkout_id.to_string(),
                    generation,
                    sink,
                )
            });
        let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (self.launcher)(checkout_id, directory)
        }))
        .unwrap_or_else(|_| {
            Err(BridgeError::Failed(
                "the agent bridge launcher panicked".into(),
            ))
        });
        match started {
            Ok(bridge) => {
                let mut state = slot.state.lock().map_err(|_| {
                    BridgeError::Failed("the agent bridge state is poisoned".into())
                })?;
                if !matches!(
                    &*state,
                    BridgeState::Starting {
                        stop_requested: false
                    }
                ) {
                    // Stop already removed the slot, so a late successful start must only clean
                    // up its child and must never publish over a later checkout generation.
                    *state = BridgeState::Stopping;
                    drop(state);
                    (self.stopper)(&bridge, self.startup_stop_wait);
                    if let Ok(mut state) = slot.state.lock() {
                        *state = BridgeState::Stopped;
                        slot.changed.notify_all();
                    }
                    return Err(BridgeError::Failed(
                        "the agent bridge was stopped while starting".into(),
                    ));
                }
                if let Some(sink) = sink {
                    // Start the reader before publishing Ready, so a concurrent stop cannot
                    // finish and then have a reader started for the stopped bridge.
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        (self.reader_starter)(&bridge, &sink)
                    }))
                    .is_err()
                    {
                        let error =
                            BridgeError::Failed("the agent event reader starter panicked".into());
                        *state = BridgeState::Failed(error.clone());
                        slot.changed.notify_all();
                        drop(state);
                        if let Ok(mut bridges) = self.bridges.lock() {
                            remove_slot_if_current(&mut bridges, checkout_id, &slot);
                        }
                        (self.stopper)(&bridge, self.startup_stop_wait);
                        return Err(error);
                    }
                }
                *state = BridgeState::Ready(Arc::clone(&bridge));
                slot.changed.notify_all();
                Ok(bridge)
            }
            Err(error) => {
                let mut state = slot.state.lock().map_err(|_| {
                    BridgeError::Failed("the agent bridge state is poisoned".into())
                })?;
                if matches!(&*state, BridgeState::Stopped)
                    || matches!(
                        &*state,
                        BridgeState::Starting {
                            stop_requested: true
                        }
                    )
                {
                    *state = BridgeState::Stopped;
                } else {
                    *state = BridgeState::Failed(error.clone());
                }
                slot.changed.notify_all();
                drop(state);
                if let Ok(mut bridges) = self.bridges.lock() {
                    remove_slot_if_current(&mut bridges, checkout_id, &slot);
                }
                Err(error)
            }
        }
    }

    /// Runs one exchange with the checkout's bridge and, when that exchange says the bridge's
    /// endpoint is the service's no more, retires the bridge afterwards.
    ///
    /// The refresh is deliberately not a retry. A `POST` may already have reached the service even
    /// though its answer was lost, and asking a second time would deliver a second prompt, so the
    /// caller gets the error its own request produced and the replacement bridge is installed for
    /// the next call. Every call site runs this *after* the exchange, which is also what keeps
    /// discovery out of any lock the exchange held.
    fn read_and_refresh<T>(
        &self,
        checkout_id: &str,
        bridge: &Arc<AgentBridge>,
        exchange: impl FnOnce() -> Result<T, BridgeError>,
    ) -> Result<T, BridgeError> {
        match exchange() {
            Ok(answer) => Ok(answer),
            Err(error) => {
                if matches!(error, BridgeError::Stale(_)) {
                    self.refresh_stale_endpoint(checkout_id, bridge);
                }
                Err(error)
            }
        }
    }

    /// Finds the service again and retires `bridge` when it is no longer where the service is.
    ///
    /// The service is the person's and can be restarted on another port or with another password,
    /// and a bridge that keeps the address it was given then answers nothing while the app reports
    /// that the agent is not running. Discovery runs with no lock held: it is one registration read
    /// and one probe, and a probe against a busy service can take its whole timeout, which the
    /// shared bridges map must not be held across. A discovery that finds the same address leaves
    /// the bridge alone — a service that is merely unreachable right now is not a moved one.
    fn refresh_stale_endpoint(&self, checkout_id: &str, bridge: &Arc<AgentBridge>) {
        if let Ok(endpoint) = (self.finder)() {
            if endpoint != bridge.credentials.endpoint {
                self.retire_stale_bridge(checkout_id, bridge);
            }
        }
    }

    /// Removes the checkout's slot while it still publishes this bridge, and stops what it held.
    ///
    /// The removal is conditional on the slot *and* on the bridge it publishes, so a replacement
    /// that arrived in the meantime is left alone. The generation moves the way a stop moves it:
    /// the retired bridge's reader is stopped, and anything still holding events from it finds
    /// itself a generation behind, so the next start publishes against the new one.
    fn retire_stale_bridge(&self, checkout_id: &str, bridge: &Arc<AgentBridge>) {
        let retired = {
            let Ok(mut bridges) = self.bridges.lock() else {
                return;
            };
            let Some(slot) = bridges.get(checkout_id).cloned() else {
                return;
            };
            let publishes_bridge = slot.state.lock().is_ok_and(|state| {
                matches!(&*state, BridgeState::Ready(current) if Arc::ptr_eq(current, bridge))
            });
            if !publishes_bridge {
                return;
            }
            remove_slot_if_current(&mut bridges, checkout_id, &slot);
            slot
        };
        if let Ok(mut state) = self.removal_state.lock() {
            let generation = state
                .generations
                .entry(checkout_id.to_string())
                .or_default();
            *generation = generation.wrapping_add(1);
        }
        retired.stop(self.stopper.as_ref(), self.startup_stop_wait);
        // Both caches are answers read through the address that no longer serves: the palette and
        // the shared session list both have to be read again from the service that is there now.
        if let Ok(mut catalogs) = self.catalogs.lock() {
            catalogs.remove(checkout_id);
        }
        if let Ok(mut candidates) = self.candidates.lock() {
            candidates.cached = None;
            // A read in flight is answering through the address that no longer serves. Letting the
            // next caller start its own is what makes the list be read again from the service that
            // is there now, rather than having it wait for an answer that will not be cached.
            candidates.reading = None;
        }
    }

    /// Sessions that are working in a worktree of `checkouts` other than the one they were in.
    /// The API location remains authoritative for explicit moves; executed tool targets also
    /// identify work outside that location without changing the running TUI or its shell.
    ///
    /// `checkouts` is every checkout the panel holds with the directory it lives in, and it is
    /// the whole of the world this answer is allowed to talk about: a session in a directory
    /// that is not one of these is somebody else's session and is dropped rather than reported.
    ///
    /// The previous read is what a move is a change from, so it is kept here rather than in the
    /// caller, and it is keyed by session id. A session seen for the first time seeds that record
    /// instead of counting as a move: nothing has moved yet, and a session that has always been in
    /// its worktree would otherwise move on the first read after an app launch.
    ///
    /// What a read does *not* see does not erase what the last one saw. A session that walks into
    /// a worktree this panel has no checkout for yet keeps the worktree it came from, so the move
    /// is still reported when that worktree is registered, which is the commonest case there is:
    /// the agent creates the worktree and walks into it. An entry that goes unread for
    /// [`SESSION_LOCATION_TTL`] is dropped, because a position that old says nothing about where a
    /// session is now.
    ///
    /// A move is offered again on every read for [`PENDING_MOVES_TTL`]. Handing one over is the
    /// only thing this side can do about delivery: the caller is a poll that may throw its answer
    /// away, and nothing here can tell whether it did, so the record outlives the first delivery.
    /// Repeating a move is safe, because the terminal is looked up in the worktree it left and a
    /// move already applied leaves nothing there to move.
    pub fn relocations(
        &self,
        checkouts: &[(String, PathBuf)],
    ) -> Result<Vec<AgentRelocation>, BridgeError> {
        self.relocations_with_baseline(checkouts, false)
    }

    pub fn relocations_with_baseline(
        &self,
        checkouts: &[(String, PathBuf)],
        baseline: bool,
    ) -> Result<Vec<AgentRelocation>, BridgeError> {
        self.relocations_with_budget(checkouts, baseline, Duration::from_secs(2))
    }

    fn relocations_with_budget(
        &self,
        checkouts: &[(String, PathBuf)],
        baseline: bool,
        budget: Duration,
    ) -> Result<Vec<AgentRelocation>, BridgeError> {
        let deadline = Instant::now() + budget;
        // Serialize snapshots without holding state locks across transport I/O.
        let _snapshot = match self.relocation_snapshot.try_lock() {
            Ok(snapshot) => snapshot,
            Err(std::sync::TryLockError::WouldBlock) if baseline => {
                return Err(BridgeError::Unavailable(
                    "could not establish the agent relocation baseline".into(),
                ))
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                return self.offer_pending_moves(Instant::now())
            }
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(BridgeError::Failed(
                    "the agent relocation snapshot is poisoned".into(),
                ))
            }
        };
        let checkout_index = CheckoutIndex::new(checkouts);
        // Any OpenCode bridge reads every location, so the read is asked of one that is already
        // connected rather than of one this call would connect. A bridge is opened for a
        // checkout the panel is already talking to, which is also the only kind of checkout an
        // OpenCode session can move out of, so there is nothing to be gained by opening another one.
        let anchor = {
            let bridges = self
                .bridges
                .lock()
                .map_err(|_| BridgeError::Failed("the agent service is poisoned".into()))?;
            checkouts
                .iter()
                .find(|(checkout_id, _)| bridges.contains_key(checkout_id))
                .cloned()
        };
        let Some((anchor_id, anchor_directory)) = anchor else {
            if baseline {
                self.session_work
                    .lock()
                    .map_err(|_| BridgeError::Failed("the agent service is poisoned".into()))?
                    .clear();
                self.session_locations
                    .lock()
                    .map_err(|_| BridgeError::Failed("the agent service is poisoned".into()))?
                    .clear();
                return self.record_relocations_with_baseline(
                    checkouts,
                    Vec::new(),
                    Instant::now(),
                    true,
                );
            }
            return if checkouts.is_empty() {
                Ok(Vec::new())
            } else {
                self.offer_pending_moves(Instant::now())
            };
        };
        let bridge = match self.bridge(&anchor_id, &anchor_directory) {
            Ok(bridge) => bridge,
            Err(error) if baseline => return Err(error),
            Err(_) => {
                return self.offer_pending_moves(Instant::now());
            }
        };
        let mut sessions = match bridge.session_locations_until(deadline) {
            Ok(sessions) => sessions,
            Err(error) if baseline => return Err(error),
            Err(_) => {
                return self.offer_pending_moves(Instant::now());
            }
        };
        let now = Instant::now();
        let observed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let mut work = self
            .session_work
            .lock()
            .map_err(|_| BridgeError::Failed("the agent service is poisoned".into()))?
            .clone();
        work.retain(|_, entry| now.duration_since(entry.seen) <= SESSION_LOCATION_TTL);
        // One deadline covers initial lists, activity and all session histories.
        if deadline <= Instant::now() {
            return if baseline {
                Err(BridgeError::Unavailable(
                    "could not establish the agent relocation baseline".into(),
                ))
            } else {
                self.offer_pending_moves(Instant::now())
            };
        }
        let running: HashMap<String, ApiActiveSession> = if sessions
            .iter()
            .any(|session| session.time.updated != 0 && work.contains_key(&session.id))
        {
            match bridge.get_unscoped_json(
                "/api/session/active",
                deadline.saturating_duration_since(Instant::now()),
            ) {
                Ok(running) => running,
                Err(error) if baseline => return Err(error),
                Err(_) => {
                    return self.offer_pending_moves(Instant::now());
                }
            }
        } else {
            HashMap::new()
        };
        let mut history_cursor = self
            .relocation_history_cursor
            .lock()
            .map_err(|_| BridgeError::Failed("the agent service is poisoned".into()))?
            .clone();
        rotate_sessions_after(&mut sessions, history_cursor.as_deref());
        let mut located = Vec::new();
        let mut clear_pending = Vec::new();
        for session in sessions {
            let origin = PathBuf::from(session.location.unwrap().directory);
            let entry = work
                .entry(session.id.clone())
                .or_insert_with(|| SessionWork {
                    origin: origin.clone(),
                    effective: Some(origin.clone()),
                    targets: Vec::new(),
                    completed: observed,
                    updated: session.time.updated,
                    running: false,
                    seen: now,
                });
            entry.seen = now;
            if !same_directory(&entry.origin, &origin) {
                entry.origin = origin.clone();
                entry.effective = Some(origin);
                entry.targets.clear();
                entry.completed = observed;
            }
            let changed = entry.updated != session.time.updated;
            let active = running
                .get(&session.id)
                .is_some_and(|active| active.kind == "running");
            let finishing = entry.running;
            entry.running |= active;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if (baseline || changed || active || finishing)
                && checkouts
                    .iter()
                    .any(|(_, directory)| same_directory(directory, &entry.origin))
            {
                if remaining.is_zero() {
                    if baseline {
                        return Err(BridgeError::Unavailable(
                            "could not establish the agent relocation baseline".into(),
                        ));
                    }
                } else {
                    validate_session_id(&session.id)?;
                    let messages: Result<Vec<serde_json::Value>, _> = bridge.get_unscoped_json(
                        &format!(
                            "/api/session/{}/message?type=assistant&order=desc&limit=10",
                            session.id
                        ),
                        remaining,
                    );
                    history_cursor = Some(session.id.clone());
                    match messages {
                        Ok(messages) => {
                            entry.updated = session.time.updated;
                            entry.running = active;
                            if let Some((completed, targets)) =
                                latest_work(&messages, entry.completed, &entry.origin)
                            {
                                entry.completed = completed;
                                entry.targets = targets;
                                // An ambiguous target cannot keep offering an older undelivered move.
                                if checkout_index.work_directory(&entry.targets).is_none() {
                                    clear_pending.push(session.id.clone());
                                }
                            }
                        }
                        Err(error) if baseline => return Err(error),
                        // Keep this session's prior verified cursor/targets and continue other sessions.
                        Err(_) => {}
                    }
                }
            }
            // Resolve again even without new tools: the agent may have created a worktree before
            // Muster registered it. The original target is retained, not its parent checkout.
            if let Some(directory) = checkout_index.work_directory(&entry.targets) {
                entry.effective = Some(directory);
            } else if baseline && !entry.targets.is_empty() {
                // An unresolved target has no checkout to seed; retaining an old effective dir
                // would turn its later registration into a relocation from disabled-time work.
                entry.effective = None;
            }
            located.push((session.id, entry.effective.clone()));
        }
        if deadline <= Instant::now() && baseline {
            return Err(BridgeError::Unavailable(
                "could not establish the agent relocation baseline".into(),
            ));
        }
        if !clear_pending.is_empty() {
            let mut pending = self
                .pending_moves
                .lock()
                .map_err(|_| BridgeError::Failed("the agent service is poisoned".into()))?;
            for session_id in clear_pending {
                pending.remove(&session_id);
            }
        }
        *self
            .session_work
            .lock()
            .map_err(|_| BridgeError::Failed("the agent service is poisoned".into()))? = work;
        *self
            .relocation_history_cursor
            .lock()
            .map_err(|_| BridgeError::Failed("the agent service is poisoned".into()))? =
            history_cursor;
        self.record_relocations_with_baseline(checkouts, located, now, baseline)
    }

    fn offer_pending_moves(&self, now: Instant) -> Result<Vec<AgentRelocation>, BridgeError> {
        let mut pending = self
            .pending_moves
            .lock()
            .map_err(|_| BridgeError::Failed("the agent service is poisoned".into()))?;
        pending.retain(|_, entry| now.duration_since(entry.since) <= PENDING_MOVES_TTL);
        Ok(pending
            .values()
            .map(|entry| entry.relocation.clone())
            .collect())
    }

    /// What changed between the last read and this one, together with what is still owed.
    ///
    /// Split out from the read so that time can be given rather than waited for: both windows
    /// here are what stops a stale record from outliving its usefulness, and neither can be
    /// checked by sleeping in a test.
    #[cfg(test)]
    fn record_relocations(
        &self,
        checkouts: &[(String, PathBuf)],
        located: Vec<(String, PathBuf)>,
        now: Instant,
    ) -> Result<Vec<AgentRelocation>, BridgeError> {
        self.record_relocations_with_baseline(
            checkouts,
            located
                .into_iter()
                .map(|(session_id, directory)| (session_id, Some(directory)))
                .collect(),
            now,
            false,
        )
    }

    fn record_relocations_with_baseline(
        &self,
        checkouts: &[(String, PathBuf)],
        located: Vec<(String, Option<PathBuf>)>,
        now: Instant,
        baseline: bool,
    ) -> Result<Vec<AgentRelocation>, BridgeError> {
        let mut known = self
            .session_locations
            .lock()
            .map_err(|_| BridgeError::Failed("the agent service is poisoned".into()))?;
        known.retain(|_, entry| now.duration_since(entry.seen) <= SESSION_LOCATION_TTL);
        let mut pending = self
            .pending_moves
            .lock()
            .map_err(|_| BridgeError::Failed("the agent service is poisoned".into()))?;
        pending.retain(|_, entry| now.duration_since(entry.since) <= PENDING_MOVES_TTL);
        if baseline {
            // The baseline snapshot supersedes every move and location observed in the prior epoch.
            known.clear();
            pending.clear();
        }
        for (session_id, directory) in located {
            // A session whose directory is not a checkout of this workspace is not news, and
            // naming it would be reporting on a project this app has no worktree for. It is not
            // forgotten either: its last known worktree is what the move is read from when the
            // worktree it walked into is registered.
            let named = directory.as_ref().and_then(|directory| {
                checkouts
                    .iter()
                    .find(|(_, candidate)| same_directory(candidate, directory))
                    .map(|(checkout_id, _)| checkout_id.clone())
            });
            match known.get_mut(&session_id) {
                Some(entry) => {
                    entry.seen = now;
                    if let (Some(was), Some(checkout_id)) = (&entry.checkout_id, &named) {
                        if was != checkout_id {
                            pending.insert(
                                session_id.clone(),
                                PendingMove {
                                    relocation: AgentRelocation {
                                        session_id,
                                        from_checkout_id: was.clone(),
                                        to_checkout_id: checkout_id.clone(),
                                        observed_at: SystemTime::now()
                                            .duration_since(UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_millis()
                                            as i64,
                                    },
                                    since: now,
                                },
                            );
                        }
                    }
                    if named.is_some() {
                        entry.checkout_id = named;
                    }
                }
                None => {
                    known.insert(
                        session_id,
                        SessionLocation {
                            checkout_id: named,
                            seen: now,
                        },
                    );
                }
            }
        }
        // Read after the loop, so what goes out is what this read settled: a move recorded above
        // replaced the one it supersedes, so one answer never asks for the same session to travel
        // twice — and one move per session is also what keeps a terminal from being asked for two
        // hops it cannot take. A move already reported goes out again because handing one over is
        // the only thing this process can do about delivery, and a caller that dropped the first
        // copy has no other way to know.
        Ok(pending
            .values()
            .map(|entry| entry.relocation.clone())
            .collect())
    }

    /// Every session the checkout's server knows about, with the service's own running answer.
    pub fn sessions(
        &self,
        checkout_id: &str,
        directory: &Path,
    ) -> Result<Vec<AgentSession>, BridgeError> {
        let bridge = self.bridge(checkout_id, directory)?;
        self.read_and_refresh(checkout_id, &bridge, || {
            let listed: Vec<ApiSession> = bridge.get_json("/api/session")?;
            // Asked once per list rather than per session: the route answers for the whole service,
            // so membership is what scopes it to this directory.
            let running = bridge.running_sessions()?;
            let awaiting = bridge.awaiting_sessions(directory, JSON_REQUEST_TIMEOUT)?;
            let mut sessions = Vec::new();
            for raw in listed {
                // The list spans every directory the server knows, so foreign sessions are
                // filtered out here. Addressing one by id is what rejects them, below.
                if raw.check_scope(directory).is_ok() {
                    sessions.push(bridge.to_agent_session(
                        &raw,
                        running.contains(&raw.id),
                        awaiting.contains(&raw.id),
                    ));
                }
            }
            Ok(sessions)
        })
    }

    /// Every session the service answers for, whatever directory each one is located in.
    ///
    /// `sessions` answers "what belongs to this worktree", which is what a review round needs: a
    /// prompt sent to a session in another worktree would put the round somewhere the reader did not
    /// ask about. That is not the question a sidebar row asks. A row asks WHICH SESSION THIS
    /// TERMINAL HAS OPEN, and the only thing that can answer it is the title the terminal's own TUI
    /// wrote — and a terminal's session is not necessarily located in the worktree the terminal is
    /// filed under. Measured against the real service: a terminal grouped under the repository had a
    /// session open that lives in `.worktrees/agent-follow-worktree`, so the scoped list could never
    /// name it, and the row fell back to "session not identified" — no state, no colour — while the
    /// agent was plainly working. That list is why this read carries no directory at all: measured,
    /// `/api/session` answers one session for a worktree, fifty for its repository and two hundred
    /// for the service, and the two hundred are the ones that can belong to a row.
    ///
    /// **The list is as wide as the service, not as wide as the repository.** One service is not
    /// guaranteed to be one repository's, and nothing here filters by repository either — pretending
    /// to, by name or by comment, is what made the earlier claim about this false.
    ///
    /// It is the service's list for every checkout, so one read answers for all of them and a poll
    /// that arrives once per checkout costs one request rather than one each. Nothing is trusted
    /// here: a candidate is only ever a candidate, it is refused unless the title matches exactly
    /// one of them, and a wrong candidate therefore draws no state rather than a wrong one.
    pub fn candidate_sessions(
        &self,
        checkout_id: &str,
        directory: &Path,
    ) -> Result<Vec<AgentSession>, BridgeError> {
        let bridge = self.bridge(checkout_id, directory)?;
        // Wrapped from the outside, so a read that turns out to be talking to a service which has
        // moved discovers that one after the shared cache lock is released rather than under it.
        let read = self.read_and_refresh(checkout_id, &bridge, || self.candidate_read(&bridge))?;
        Ok(read
            .sessions
            .iter()
            .map(|raw| {
                let mut session = bridge.to_agent_session(
                    raw,
                    read.running.contains(&raw.id),
                    read.awaiting.contains(&raw.id),
                );
                let directory = raw
                    .location
                    .as_ref()
                    .map(|location| PathBuf::from(&location.directory))
                    .unwrap_or_else(|| bridge.directory().to_path_buf());
                if read.unknown_pending.contains(&directory) {
                    session.awaiting_reply = None;
                }
                session
            })
            .collect())
    }

    /// The service-wide list, read once for every checkout that asks in the same tick.
    ///
    /// A poll arrives once per checkout, so a read every caller could start would be nine identical
    /// requests a tick. The first one to arrive publishes itself as the read in flight and releases
    /// the lock before it goes to the network; the rest find that read and wait for its answer
    /// rather than ask again. The lock therefore only ever covers the state of the slot, never the
    /// its network requests, and a caller that has to react to a failure — the endpoint no longer
    /// serving — still reacts once this has given the lock back.
    fn candidate_read(&self, bridge: &AgentBridge) -> Result<CandidateRead, BridgeError> {
        let fetch = {
            let mut slot = self
                .candidates
                .lock()
                .map_err(|_| BridgeError::Failed("the agent candidate cache is poisoned".into()))?;
            if let Some(read) = slot.cached.as_ref() {
                if read.within(CANDIDATE_CACHE_TTL) {
                    return Ok(read.value.clone());
                }
            }
            match slot.reading.clone() {
                Some(reading) => {
                    drop(slot);
                    return reading.wait();
                }
                None => {
                    let fetch = CandidateFetch::new();
                    slot.reading = Some(Arc::clone(&fetch));
                    fetch
                }
            }
        };
        let outcome = read_candidates(bridge, self.pending_read_budget);
        let mut report_failures = None;
        if let Ok(mut slot) = self.candidates.lock() {
            // Only the read that is still in flight publishes. One a moved service retired
            // mid-flight answered through an address that no longer serves, so it caches nothing:
            // the next caller has to read from the service that is there now.
            if slot
                .reading
                .as_ref()
                .is_some_and(|reading| Arc::ptr_eq(reading, &fetch))
            {
                slot.reading = None;
                if let Ok(read) = &outcome {
                    slot.cached = Some(Cached::taken(Instant::now(), read.clone()));
                    report_failures = slot.pending_failures_to_report(&read.pending_failures);
                }
            }
        }
        if let Some(failures) = report_failures {
            report_pending_read_failures(&failures);
        }
        fetch.publish(outcome.clone());
        outcome
    }

    /// Every agent the checkout's service offers, with the color OpenCode paints it with.
    ///
    /// The list is the same for every session in the checkout, so this is one call behind a cache
    /// rather than one per session. It is keyed by checkout rather than shared, because the route
    /// answers for a directory: two repositories offer different agents and a wider cache would
    /// paint one project's row with the other's palette.
    pub fn agents(
        &self,
        checkout_id: &str,
        directory: &Path,
    ) -> Result<Vec<AgentAgent>, BridgeError> {
        let bridge = self.bridge(checkout_id, directory)?;
        if let Ok(catalogs) = self.catalogs.lock() {
            if let Some(cached) = catalogs.get(checkout_id) {
                if cached.within(AGENT_CATALOG_TTL) {
                    return Ok(cached.value.as_ref().clone());
                }
            }
        }
        let listed: Vec<ApiAgent> =
            self.read_and_refresh(checkout_id, &bridge, || bridge.get_json("/api/agent"))?;
        let catalog: Arc<Vec<AgentAgent>> = Arc::new(
            listed
                .into_iter()
                .map(|raw| AgentAgent {
                    id: raw.id,
                    name: raw.name,
                    mode: raw.mode,
                    color: raw.color,
                    hidden: raw.hidden,
                })
                .collect(),
        );
        if let Ok(mut catalogs) = self.catalogs.lock() {
            catalogs.insert(
                checkout_id.to_string(),
                Cached::taken(Instant::now(), Arc::clone(&catalog)),
            );
        }
        Ok(catalog.as_ref().clone())
    }

    /// Resolves a session inside `checkout_id` only. A session belonging to another
    /// checkout cannot be addressed here: it does not exist in this server.
    pub fn owned_session(
        &self,
        checkout_id: &str,
        directory: &Path,
        session_id: &str,
    ) -> Result<AgentSession, BridgeError> {
        let bridge = self.bridge(checkout_id, directory)?;
        self.read_and_refresh(checkout_id, &bridge, || {
            let raw = bridge.session(session_id)?;
            let running = bridge.running_sessions()?.contains(session_id);
            let awaiting = bridge
                .awaiting_sessions(directory, JSON_REQUEST_TIMEOUT)?
                .contains(session_id);
            Ok(bridge.to_agent_session(&raw, running, awaiting))
        })
    }

    pub fn create_session(
        &self,
        checkout_id: &str,
        directory: &Path,
        title: &str,
    ) -> Result<AgentSession, BridgeError> {
        self.with_checkout_operation(checkout_id, || {
            self.create_session_locked(checkout_id, directory, title)
        })
    }

    pub(crate) fn create_session_at_generation(
        &self,
        checkout_id: &str,
        directory: &Path,
        title: &str,
        generation: u64,
        is_registered: impl Fn() -> bool,
    ) -> Result<AgentSession, BridgeError> {
        self.with_checkout_generation(checkout_id, generation, is_registered, || {
            self.create_session_locked(checkout_id, directory, title)
        })
    }

    fn create_session_locked(
        &self,
        checkout_id: &str,
        directory: &Path,
        title: &str,
    ) -> Result<AgentSession, BridgeError> {
        self.ensure_checkout_not_removing(checkout_id)?;
        let bridge = self.bridge(checkout_id, directory)?;
        if bridge.is_stopped() {
            return Err(BridgeError::Unavailable(
                "the agent bridge was stopped".into(),
            ));
        }
        self.read_and_refresh(checkout_id, &bridge, || {
            let created: ApiSession =
                bridge.post_json("/api/session", &serde_json::json!({ "title": title }))?;
            created.check_scope(directory)?;
            let running = bridge.running_sessions()?.contains(&created.id);
            let awaiting = bridge
                .awaiting_sessions(directory, JSON_REQUEST_TIMEOUT)?
                .contains(&created.id);
            Ok(bridge.to_agent_session(&created, running, awaiting))
        })
    }

    /// Sends the review as one message. v2.0.18 wants {"text": …} on this route.
    pub fn prompt(
        &self,
        checkout_id: &str,
        directory: &Path,
        session_id: &str,
        text: &str,
    ) -> Result<AgentSession, BridgeError> {
        let text = Self::validate_prompt(text)?;
        self.with_checkout_operation(checkout_id, || {
            self.prompt_locked(checkout_id, directory, session_id, text)
        })
    }

    pub(crate) fn prompt_at_generation(
        &self,
        checkout_id: &str,
        directory: &Path,
        session_id: &str,
        text: &str,
        generation: u64,
        is_registered: impl Fn() -> bool,
    ) -> Result<AgentSession, BridgeError> {
        let text = Self::validate_prompt(text)?;
        self.with_checkout_generation(checkout_id, generation, is_registered, || {
            self.prompt_locked(checkout_id, directory, session_id, text)
        })
    }

    fn validate_prompt(text: &str) -> Result<&str, BridgeError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(BridgeError::Failed("the review is empty".into()));
        }
        if text.len() > MAX_PROMPT_BYTES {
            return Err(BridgeError::Failed(
                "the review is too large to send to the agent in one message".into(),
            ));
        }
        Ok(text)
    }

    fn prompt_locked(
        &self,
        checkout_id: &str,
        directory: &Path,
        session_id: &str,
        text: &str,
    ) -> Result<AgentSession, BridgeError> {
        self.ensure_checkout_not_removing(checkout_id)?;
        let bridge = self.bridge(checkout_id, directory)?;
        if bridge.is_stopped() {
            return Err(BridgeError::Unavailable(
                "the agent bridge was stopped".into(),
            ));
        }
        // Resolve first so a foreign id fails before anything is sent.
        self.read_and_refresh(checkout_id, &bridge, || {
            bridge.session(session_id).map(|_| ())
        })?;
        self.track_prompt_pending(checkout_id, session_id);
        // A transport error can arrive after OpenCode accepted the prompt, so retain the
        // pending activity marker until a turn ends or the bridge is explicitly stopped.
        // A refused send costs this send and nothing more: `read_and_refresh` retires a stale
        // bridge but never repeats the request, so a prompt the service may already have taken is
        // not delivered twice. The round marker in the transcript is what decides whether a lost
        // send was in fact delivered, and only a caller that has established its absence re-sends.
        self.read_and_refresh(checkout_id, &bridge, || {
            let _: serde_json::Value = bridge.post_json(
                &format!("/api/session/{session_id}/prompt"),
                &serde_json::json!({ "text": text }),
            )?;
            let raw = bridge.session(session_id)?;
            let running = bridge.running_sessions()?.contains(session_id);
            let awaiting = bridge
                .awaiting_sessions(directory, JSON_REQUEST_TIMEOUT)?
                .contains(session_id);
            Ok(bridge.to_agent_session(&raw, running, awaiting))
        })
    }

    /// Whether the session's transcript mentions `marker`.
    ///
    /// This is what makes a reconnect safe: a round that was recorded as `dispatching` but
    /// never confirmed is only retried when its marker is genuinely absent.
    pub fn session_mentions(
        &self,
        checkout_id: &str,
        directory: &Path,
        session_id: &str,
        marker: &str,
    ) -> Result<bool, BridgeError> {
        if marker.is_empty() {
            return Ok(false);
        }
        let bridge = self.bridge(checkout_id, directory)?;
        self.read_and_refresh(checkout_id, &bridge, || {
            Ok(bridge.session_transcript(session_id)?.contains(marker))
        })
    }

    pub(crate) fn session_mentions_at_generation(
        &self,
        checkout_id: &str,
        directory: &Path,
        session_id: &str,
        marker: &str,
        generation: u64,
        is_registered: impl Fn() -> bool,
    ) -> Result<bool, BridgeError> {
        self.with_checkout_generation(checkout_id, generation, is_registered, || {
            self.session_mentions(checkout_id, directory, session_id, marker)
        })
    }

    /// Stops the checkout's server. Called when the app shuts down.
    pub fn stop(&self, checkout_id: &str) {
        self.with_checkout_operation(checkout_id, || self.stop_locked(checkout_id));
    }

    fn stop_locked(&self, checkout_id: &str) {
        if let Ok(mut state) = self.removal_state.lock() {
            let generation = state
                .generations
                .entry(checkout_id.to_string())
                .or_default();
            *generation = generation.wrapping_add(1);
        }
        let slot = self
            .bridges
            .lock()
            .ok()
            .and_then(|mut bridges| bridges.remove(checkout_id));
        if let Some(slot) = slot {
            slot.stop(self.stopper.as_ref(), self.startup_stop_wait);
        }
        if let Ok(mut turns) = self.busy_turns.lock() {
            turns.remove(checkout_id);
        }
        if let Ok(mut catalogs) = self.catalogs.lock() {
            catalogs.remove(checkout_id);
        }
    }

    pub(crate) fn stop_at_generation(
        &self,
        checkout_id: &str,
        generation: u64,
        is_registered: impl Fn() -> bool,
    ) -> Result<(), BridgeError> {
        self.with_checkout_generation(checkout_id, generation, is_registered, || {
            self.stop_locked(checkout_id);
            Ok(())
        })
    }

    fn track_prompt_pending(&self, checkout_id: &str, session_id: &str) {
        if let Ok(mut turns) = self.busy_turns.lock() {
            turns
                .entry(checkout_id.to_string())
                .or_default()
                .insert(session_id.to_string(), TrackedTurn::Pending);
        }
    }

    pub fn stop_all(&self) {
        let slots = self
            .bridges
            .lock()
            .map(|mut bridges| bridges.drain().map(|(_, slot)| slot).collect::<Vec<_>>())
            .unwrap_or_default();
        // Signal every currently published child before any stopper waits for wait()/join().
        for slot in &slots {
            slot.signal();
        }
        let deadline = Instant::now() + self.startup_stop_wait;
        for slot in slots {
            (self.slot_stopper)(slot, deadline.saturating_duration_since(Instant::now()));
        }
    }
}

impl Drop for AgentService {
    fn drop(&mut self) {
        self.stop_all();
    }
}

pub fn map_error(error: BridgeError) -> IpcError {
    error.into_ipc(IpcErrorCode::OperationFailed)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        io::{self, BufRead, BufReader, Cursor, Read, Write},
        net::{Shutdown, TcpListener, TcpStream},
        path::{Path, PathBuf},
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc, Arc, Mutex,
        },
        thread::sleep,
        time::{Duration, Instant},
    };

    use crate::{
        domain::review::{review_anchor_hash, review_round_marker},
        services::review_round::build_round_prompt,
    };

    use super::{
        event_from_payload, event_stream, generation_scoped_sink, is_interrupted, join_reader,
        read_bounded_line, read_candidates, read_events, read_sse_frame, ready,
        remove_slot_if_current, report_stream_loss, retry_interrupted, retry_interrupted_until,
        same_directory, send_json, status_detail, validate_session_id, AgentBridge, AgentEvent,
        AgentService, ApiAgent, ApiModel, ApiSession, BridgeError, BridgeState, BridgeStopper,
        CandidateSlot, EventSink, PendingMove, PendingReadFailure, RemovalState, ServerCredentials,
        SessionLocation, SessionWork, StreamLoss, CANDIDATE_SESSION_LIMIT, DIRECTORY_HEADER,
        INTERRUPTED_READ_ATTEMPTS, INTERRUPTED_REQUEST, JSON_REQUEST_TIMEOUT,
        MAX_EVENT_HEADERS_BYTES, MAX_EVENT_HEADER_LINE_BYTES, MAX_PROMPT_BYTES,
        MAX_SSE_FRAME_BYTES, MAX_SSE_FRAME_LINES, PENDING_MOVES_TTL, PENDING_READ_CONCURRENCY,
        SESSION_LIST_LIMIT,
    };
    use crate::domain::agent::AgentRelocation;
    use crate::services::opencode::ServiceEndpoint;

    fn assert_relocation(
        actual: &[AgentRelocation],
        session_id: &str,
        from_checkout_id: &str,
        to_checkout_id: &str,
    ) {
        assert_eq!(actual.len(), 1);
        assert_eq!(actual[0].session_id, session_id);
        assert_eq!(actual[0].from_checkout_id, from_checkout_id);
        assert_eq!(actual[0].to_checkout_id, to_checkout_id);
        assert!(actual[0].observed_at > 0);
    }

    fn test_event_bridge(directory: &Path, port: u16) -> AgentBridge {
        AgentBridge {
            checkout_id: "test-checkout".into(),
            credentials: ServerCredentials {
                endpoint: ServiceEndpoint {
                    url: format!("http://127.0.0.1:{port}"),
                    port,
                    password: "test-secret".into(),
                },
                directory: directory.to_path_buf(),
            },
            client: ureq::Agent::new_with_defaults(),
            event_socket: Mutex::new(None),
            reader: Mutex::new(None),
            stopped: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn mock_event_response(
        directory: &Path,
        response: Vec<u8>,
    ) -> (AgentBridge, std::thread::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock server should bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return Vec::new();
            };
            let mut request = Vec::new();
            let mut buffer = [0; 512];
            while request.len() < 2048 && !request.windows(4).any(|part| part == b"\r\n\r\n") {
                let Ok(read) = stream.read(&mut buffer) else {
                    break;
                };
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            let _ = stream.write_all(&response);
            request
        });
        (test_event_bridge(directory, port), server)
    }

    /// A service that answers every request with an empty agent catalog.
    ///
    /// The thread is left running rather than joined: it ends when the test binary does, and a
    /// listener waiting for a request nobody makes is not something to wait on. Its port is
    /// returned because that is all a caller needs to point a bridge at it.
    fn serving_empty_catalog(directory: &Path) -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock service should bind");
        let port = listener.local_addr().unwrap().port();
        let body = serde_json::to_vec(&serde_json::json!({
            "data": [],
            "location": { "directory": directory.to_string_lossy() },
        }))
        .expect("the catalog envelope should encode");
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                }
                if write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .is_err()
                {
                    return;
                }
                if stream.write_all(&body).is_err() {
                    return;
                }
            }
        });
        port
    }

    /// A service that accepts connections and never answers any of them.
    ///
    /// The accepted streams are held rather than dropped, because closing one is an answer of its
    /// own and would end the caller's wait on the wrong grounds. Left running rather than joined,
    /// like `serving_empty_catalog`: it ends when the test binary does, and its port is returned
    /// because that is all a caller needs to point a bridge at it.
    fn serving_silence() -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock service should bind");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                held.push(stream);
            }
        });
        port
    }

    /// What one request looked like on the wire: its request line, its Basic auth header, its
    /// body, and every header it carried so a scope can be asserted on.
    type CapturedJsonRequest = (String, String, Vec<u8>, Vec<(String, String)>);

    /// A service that answers each request from the directory its header names, so location reads
    /// running at the same time can be told apart by something other than the order they arrive in.
    /// A directory named in `slow` is a service busy answering a turn: its connection is held open
    /// for `SLOW_HOLD` and nothing is answered, which is what the pending read's budget has to
    /// survive and what proves a healthy location was read beside it rather than after it.
    ///
    /// Left running rather than joined, like `serving_empty_catalog`: the threads holding a slow
    /// connection open outlive the assertions that stopped waiting for them.
    fn serving_scoped_pending(
        sessions: serde_json::Value,
        pending: &HashMap<PathBuf, String>,
        slow: &[PathBuf],
        delayed_form: Option<(PathBuf, Duration)>,
        stalled_form_body: Option<(PathBuf, Duration)>,
    ) -> u16 {
        /// Long enough that a read waiting behind a slow location takes visibly longer than the
        /// budget these tests spend, short enough not to outlive the test binary by much.
        const SLOW_HOLD: Duration = Duration::from_secs(5);
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock service should bind");
        let port = listener.local_addr().unwrap().port();
        let listed = sessions.to_string();
        let pending: HashMap<String, String> = pending
            .iter()
            .map(|(directory, session_id)| {
                (directory.to_string_lossy().into_owned(), session_id.clone())
            })
            .collect();
        let slow: HashSet<String> = slow
            .iter()
            .map(|directory| directory.to_string_lossy().into_owned())
            .collect();
        let delayed_form = delayed_form
            .map(|(directory, delay)| (directory.to_string_lossy().into_owned(), delay));
        let stalled_form_body = stalled_form_body
            .map(|(directory, delay)| (directory.to_string_lossy().into_owned(), delay));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let (listed, pending, slow, delayed_form, stalled_form_body) = (
                    listed.clone(),
                    pending.clone(),
                    slow.clone(),
                    delayed_form.clone(),
                    stalled_form_body.clone(),
                );
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                    let mut request_line = String::new();
                    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                        return;
                    }
                    let target = request_line.split_whitespace().nth(1).unwrap_or_default();
                    let target = target.to_string();
                    let mut directory = String::new();
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            break;
                        }
                        if line == "\r\n" || line.is_empty() {
                            break;
                        }
                        if let Some((name, value)) = line.trim_end().split_once(':') {
                            if name.eq_ignore_ascii_case(DIRECTORY_HEADER) {
                                directory = value.trim().to_string();
                            }
                        }
                    }
                    let delay_this_form = target.starts_with("/api/form")
                        && delayed_form
                            .as_ref()
                            .is_some_and(|(path, _)| path == &directory);
                    let stall_form_body = target.starts_with("/api/form")
                        && stalled_form_body
                            .as_ref()
                            .is_some_and(|(path, _)| path == &directory);
                    if delay_this_form {
                        if let Some((_, delay)) = &delayed_form {
                            sleep(*delay);
                        }
                    }
                    let body = if target.starts_with("/api/session/active") {
                        serde_json::json!({"data": {}}).to_string()
                    } else if target.starts_with("/api/session?") {
                        listed
                    } else if slow.contains(&directory) && !delay_this_form {
                        sleep(SLOW_HOLD);
                        return;
                    } else {
                        let requests: Vec<serde_json::Value> = pending
                            .get(&directory)
                            .map(|session_id| {
                                vec![serde_json::json!({
                                    "id": "prm_pending", "sessionID": session_id
                                })]
                            })
                            .unwrap_or_default();
                        serde_json::json!({
                            "location": {"directory": directory},
                            "data": requests
                        })
                        .to_string()
                    };
                    if write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .is_err()
                    {
                        return;
                    }
                    if stall_form_body {
                        let split = (body.len() / 2).max(1);
                        if stream.write_all(&body.as_bytes()[..split]).is_ok() {
                            if let Some((_, delay)) = &stalled_form_body {
                                sleep(*delay);
                            }
                            let _ = stream.write_all(&body.as_bytes()[split..]);
                        }
                    } else {
                        let _ = stream.write_all(body.as_bytes());
                    }
                });
            }
        });
        port
    }

    fn mock_json_responses(
        responses: Vec<serde_json::Value>,
    ) -> (u16, std::thread::JoinHandle<Vec<CapturedJsonRequest>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock server should bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            responses
                .into_iter()
                .map(|response| {
                    let (mut stream, _) = listener.accept().expect("request should connect");
                    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                    let mut request_line = String::new();
                    reader
                        .read_line(&mut request_line)
                        .expect("request line should be readable");
                    let mut authorization = String::new();
                    let mut content_length = 0;
                    let mut headers = Vec::new();
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).expect("headers should be readable");
                        if line == "\r\n" || line.is_empty() {
                            break;
                        }
                        if let Some((name, value)) = line.trim_end().split_once(':') {
                            headers.push((name.trim().to_string(), value.trim().to_string()));
                            if name.eq_ignore_ascii_case("authorization") {
                                authorization = value.trim().to_string();
                            } else if name.eq_ignore_ascii_case("content-length") {
                                content_length = value.trim().parse().expect("valid body length");
                            }
                        }
                    }
                    let mut body = vec![0; content_length];
                    reader.read_exact(&mut body).expect("request body should be readable");
                    let response = serde_json::to_vec(&response).expect("response should encode");
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        response.len()
                    )
                    .expect("response headers should be writable");
                    stream
                        .write_all(&response)
                        .expect("response body should be writable");
                    (request_line.trim().to_string(), authorization, body, headers)
                })
                .collect()
        });
        (port, server)
    }

    /// Every request line the mock was asked, in order. Used to count sends.
    type RecordedRequests = Arc<Mutex<Vec<String>>>;

    /// A service that answers one status line per request method, recording what it was asked.
    ///
    /// Answering a GET while refusing a POST is the state a repeated prompt would show up in, so
    /// the request lines are kept: a test can assert the send happened exactly once rather than
    /// only that the call failed.
    fn serving_status_per_method(
        get: &'static str,
        post: &'static str,
        get_body: String,
    ) -> (u16, RecordedRequests) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock service should bind");
        let port = listener.local_addr().unwrap().port();
        let recorded: RecordedRequests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&recorded);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                    continue;
                }
                let mut content_length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.trim_end().split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            content_length = value.trim().parse().unwrap_or(0);
                        }
                    }
                }
                // The body is read and dropped, never logged: it can carry a prompt.
                let mut body = vec![0; content_length];
                let _ = reader.read_exact(&mut body);
                seen.lock().unwrap().push(request_line.trim().to_string());
                let (status, payload) = if request_line.starts_with("POST") {
                    (post, "{}")
                } else {
                    (get, get_body.as_str())
                };
                if write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                )
                .is_err()
                {
                    return;
                }
                if stream.write_all(payload.as_bytes()).is_err() {
                    return;
                }
            }
        });
        (port, recorded)
    }

    /// How much the mock candidate service was asked, and over how many sockets.
    #[derive(Default)]
    struct MockCounts {
        requests: AtomicUsize,
        connections: AtomicUsize,
    }

    /// A service that answers a candidate snapshot and its pending lookups.
    ///
    /// Both counts are the point: `requests` says how many reads were made, and `connections` says
    /// whether the client that made them pooled them. Connections are deliberately left open, which
    /// is the only way a second request can arrive on the same socket. `delay` is served before the
    /// session list is answered, which is what makes a read long enough for another caller to
    /// arrive while it is running, and the returned channel says when that read reached the wire so
    /// a test never has to guess.
    fn serving_candidate_reads(
        sessions: Vec<serde_json::Value>,
        running: &[&str],
        delay: Duration,
    ) -> (u16, Arc<MockCounts>, mpsc::Receiver<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock service should bind");
        let port = listener.local_addr().unwrap().port();
        let counts = Arc::new(MockCounts::default());
        let sessions = Arc::new(
            serde_json::to_vec(&serde_json::json!({ "data": sessions }))
                .expect("the session list should encode"),
        );
        let active = Arc::new(
            serde_json::to_vec(&serde_json::json!({
                "data": running
                    .iter()
                    .map(|id| (*id, serde_json::json!({ "type": "running" })))
                    .collect::<HashMap<_, _>>()
            }))
            .expect("the active map should encode"),
        );
        let pending = Arc::new(
            serde_json::to_vec(&serde_json::json!({ "data": [] }))
                .expect("the pending lists should encode"),
        );
        let (arrived_tx, arrived) = mpsc::channel();
        let served = Arc::clone(&counts);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                served.connections.fetch_add(1, Ordering::SeqCst);
                let connection = Connection {
                    counts: Arc::clone(&served),
                    sessions: Arc::clone(&sessions),
                    active: Arc::clone(&active),
                    pending: Arc::clone(&pending),
                    arrived: arrived_tx.clone(),
                    delay,
                };
                std::thread::spawn(move || connection.serve(stream));
            }
        });
        (port, counts, arrived)
    }

    /// One accepted connection of `serving_candidate_reads`.
    struct Connection {
        counts: Arc<MockCounts>,
        sessions: Arc<Vec<u8>>,
        active: Arc<Vec<u8>>,
        pending: Arc<Vec<u8>>,
        arrived: mpsc::Sender<()>,
        delay: Duration,
    }

    impl Connection {
        fn serve(self, mut stream: TcpStream) {
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
            loop {
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                    return;
                }
                let mut content_length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.trim_end().split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            content_length = value.trim().parse().unwrap_or(0);
                        }
                    }
                }
                // The body is read and dropped, never logged: it can carry a prompt.
                let mut body = vec![0; content_length];
                let _ = reader.read_exact(&mut body);
                let payload = if request_line.starts_with("GET /api/session/active") {
                    Arc::clone(&self.active)
                } else if request_line.starts_with("GET /api/form")
                    || request_line.starts_with("GET /api/permission/request")
                {
                    Arc::clone(&self.pending)
                } else if request_line.starts_with("GET /api/session") {
                    let _ = self.arrived.send(());
                    sleep(self.delay);
                    Arc::clone(&self.sessions)
                } else {
                    return;
                };
                self.counts.requests.fetch_add(1, Ordering::SeqCst);
                if write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    payload.len()
                )
                .is_err()
                {
                    return;
                }
                if stream.write_all(&payload).is_err() {
                    return;
                }
            }
        }
    }

    /// The lines the code under test wrote to the log.
    ///
    /// `log` allows one logger per process and this binary installs none, so this captures into a
    /// buffer a test can read: the point of these tests is what was reported, not where it went.
    /// Capture is off until a test asks for it and the mutex serializes the tests that read it, so
    /// two of them at once cannot read each other's lines — and other tests keep running untouched,
    /// which is why every assertion below is about one checkout's own line.
    static CAPTURED_LOGS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static CAPTURING_LOGS: AtomicBool = AtomicBool::new(false);
    static CAPTURE: Mutex<()> = Mutex::new(());
    static CAPTURING: CapturingLogs = CapturingLogs;

    struct CapturingLogs;

    impl log::Log for CapturingLogs {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            CAPTURING_LOGS.load(Ordering::SeqCst)
        }

        fn log(&self, record: &log::Record<'_>) {
            if !self.enabled(record.metadata()) {
                return;
            }
            CAPTURED_LOGS
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(record.args().to_string());
        }

        fn flush(&self) {}
    }

    /// Takes the process-wide log for the duration of a test. Hold the guard while capturing.
    ///
    /// One guard covers the whole test: `CAPTURE` is a plain mutex, so asking twice without
    /// dropping the first guard would wait on itself.
    fn capturing_log() -> std::sync::MutexGuard<'static, ()> {
        let guard = CAPTURE.lock().unwrap_or_else(|error| error.into_inner());
        let _ = log::set_logger(&CAPTURING);
        // Only warnings: `ureq` traces every request at trace level, and what these tests are about
        // is what the bridge reported, not what the transport underneath it did.
        log::set_max_level(log::LevelFilter::Warn);
        CAPTURED_LOGS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        CAPTURING_LOGS.store(true, Ordering::SeqCst);
        guard
    }

    /// What has been captured so far, leaving capture on: a caller waiting for a line to appear
    /// asks again rather than switching the recorder off under the thread that is writing to it.
    fn captured_logs_so_far() -> Vec<String> {
        CAPTURED_LOGS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// Stops capturing and hands back what was written, leaving the guard in place.
    fn take_captured_logs() -> Vec<String> {
        CAPTURING_LOGS.store(false, Ordering::SeqCst);
        captured_logs_so_far()
    }

    /// A loopback port nothing listens on, for a bridge whose endpoint must stop answering.
    ///
    /// Binding and dropping leaves the address free, and the number it hands back is one a
    /// connect attempt will fail on immediately, which is what "the service moved" looks like to
    /// a bridge that still holds the old endpoint.
    fn closed_endpoint() -> ServiceEndpoint {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("port should bind");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        test_endpoint(port)
    }

    fn assert_event_response_rejected(directory: &Path, response: Vec<u8>) {
        let (bridge, server) = mock_event_response(directory, response);
        let error = match event_stream(&bridge) {
            Err(error) => error,
            Ok(reader) => {
                drop(reader);
                panic!("malformed event response should be rejected");
            }
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        bridge.clear_event_socket();
        let request = server.join().expect("mock server should finish");
        let request_text = String::from_utf8_lossy(&request);
        let directory_header = request_text
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case(DIRECTORY_HEADER))
            .map(|(_, value)| value.trim().to_string());
        let expected_directory = directory.to_string_lossy().into_owned();
        assert_eq!(
            directory_header.as_deref(),
            Some(expected_directory.as_str()),
            "the SSE stream must be scoped to its checkout"
        );
    }

    /// The service is a third party on its own release cadence, so a session that names no agent,
    /// no model, no parent and no timestamps still reads — while one that names no `id` is refused,
    /// because there is nothing to attribute the answer to.
    #[test]
    fn a_session_absent_every_descriptive_field_still_reads_and_one_without_an_id_does_not() {
        let bare = serde_json::json!({"id": "ses_bare"});
        let session: ApiSession = serde_json::from_value(bare).expect("a bare session still reads");
        assert_eq!(session.id, "ses_bare");
        assert_eq!(session.title, "");
        assert_eq!(session.agent, None);
        assert!(session.model.is_none());
        assert_eq!(session.time.updated, 0);

        assert!(
            serde_json::from_value::<ApiSession>(serde_json::json!({"title": "no id"})).is_err()
        );
        assert!(
            serde_json::from_value::<ApiModel>(serde_json::json!({"providerID": "anthropic"}))
                .is_err()
        );
        assert!(serde_json::from_value::<ApiAgent>(serde_json::json!({"name": "build"})).is_err());
    }

    #[test]
    fn bounded_line_accepts_exact_limit_at_eof_and_rejects_the_next_byte() {
        let limit = 16;
        let mut exact = std::io::BufReader::with_capacity(8, Cursor::new(vec![b'x'; limit]));
        let mut line = Vec::new();
        assert_eq!(
            read_bounded_line(&mut exact, &mut line, limit).unwrap(),
            limit
        );
        assert_eq!(line.len(), limit);
        assert_eq!(read_bounded_line(&mut exact, &mut line, limit).unwrap(), 0);

        let mut over = std::io::BufReader::with_capacity(8, Cursor::new(vec![b'x'; limit + 1]));
        let error = read_bounded_line(&mut over, &mut line, limit)
            .expect_err("an additional byte must exceed the limit");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(line.len(), limit);
        assert_eq!(read_bounded_line(&mut over, &mut line, limit).unwrap(), 1);
        assert_eq!(line.len(), 1);
    }

    #[test]
    fn an_oversized_line_is_bounded() {
        let input = vec![b'x'; MAX_EVENT_HEADER_LINE_BYTES * 2 + 7];
        let mut reader = std::io::BufReader::with_capacity(1024, Cursor::new(input));
        let mut line = Vec::new();

        let error = read_bounded_line(&mut reader, &mut line, MAX_EVENT_HEADER_LINE_BYTES)
            .expect_err("an oversized line must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(line.len(), MAX_EVENT_HEADER_LINE_BYTES);

        // The rejected bytes stay unconsumed, so the same line is rejected again rather than
        // being handed to the caller a second time as if it had been read.
        assert_eq!(
            read_bounded_line(&mut reader, &mut line, MAX_EVENT_HEADER_LINE_BYTES)
                .expect_err("an oversized line must stay rejected")
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(line.len(), MAX_EVENT_HEADER_LINE_BYTES);
    }

    #[test]
    fn event_stream_bounds_header_lines_and_aggregate_header_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let mut long_line = b"HTTP/1.1 200 OK\r\nX: ".to_vec();
        long_line.extend(vec![b'a'; MAX_EVENT_HEADER_LINE_BYTES]);
        long_line.extend_from_slice(b"\r\n\r\n");
        assert_event_response_rejected(directory.path(), long_line);

        let mut many_lines = b"HTTP/1.1 200 OK\r\n".to_vec();
        for _ in 0..=(MAX_EVENT_HEADERS_BYTES / 6) {
            many_lines.extend_from_slice(b"X: a\r\n");
        }
        many_lines.extend_from_slice(b"\r\n");
        assert_event_response_rejected(directory.path(), many_lines);

        assert_event_response_rejected(
            directory.path(),
            b"HTTP/1.1 200 OK\r\nMalformed\r\n\r\n".to_vec(),
        );
    }

    #[test]
    fn sse_frames_bound_single_lines_multiline_data_and_line_count() {
        let mut oversized_line = b"data: ".to_vec();
        oversized_line.extend(vec![b'x'; MAX_SSE_FRAME_BYTES]);
        oversized_line.extend_from_slice(b"\n\n");
        let error = read_sse_frame(&mut Cursor::new(oversized_line))
            .expect_err("an oversized data line must close the stream");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        let mut oversized_frame = Vec::new();
        for _ in 0..2 {
            oversized_frame.extend_from_slice(b"data: ");
            oversized_frame.extend(vec![b'x'; MAX_SSE_FRAME_BYTES / 2]);
            oversized_frame.push(b'\n');
        }
        let error = read_sse_frame(&mut Cursor::new(oversized_frame))
            .expect_err("the aggregate multiline frame must stay bounded");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        let mut exact_bytes = b"data: {}\n".to_vec();
        exact_bytes.resize(MAX_SSE_FRAME_BYTES, b':');
        assert_eq!(
            read_sse_frame(&mut Cursor::new(exact_bytes.clone()))
                .unwrap()
                .as_deref(),
            Some(&b"{}"[..])
        );
        exact_bytes.push(b'x');
        assert_eq!(
            read_sse_frame(&mut Cursor::new(exact_bytes))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );

        let exact_lines = [
            b"data: {}\n".as_slice(),
            &b":\n".repeat(MAX_SSE_FRAME_LINES - 1),
        ]
        .concat();
        assert_eq!(
            read_sse_frame(&mut Cursor::new(exact_lines.clone()))
                .unwrap()
                .as_deref(),
            Some(&b"{}"[..])
        );
        let mut extra_line = exact_lines;
        extra_line.extend_from_slice(b":\n");
        assert_eq!(
            read_sse_frame(&mut Cursor::new(extra_line))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn sse_frames_accumulate_crlf_multiline_data_and_dispatch_at_eof() {
        let mut reader = Cursor::new(
            ": heartbeat\r\ndata: {\"type\":\"session.idle\",\r\ndata: \"data\":{\"text\":\"café\"}}\r\n\r\n"
                .as_bytes()
                .to_vec(),
        );
        let payload = read_sse_frame(&mut reader)
            .expect("the CRLF multiline frame should parse")
            .expect("the data lines form one frame");
        let parsed: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(parsed["data"]["text"], "café");
        assert!(read_sse_frame(&mut reader).unwrap().is_none());

        let mut eof = Cursor::new(b"data: {\"type\":\"done\"}\r\n".to_vec());
        assert_eq!(
            read_sse_frame(&mut eof).unwrap().as_deref(),
            Some(&b"{\"type\":\"done\"}"[..])
        );
        assert!(read_sse_frame(&mut eof).unwrap().is_none());

        let mut invalid_utf8 = Cursor::new(b"data: \xff\n\n".to_vec());
        assert_eq!(
            read_sse_frame(&mut invalid_utf8).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn sse_payloads_reject_malformed_json_and_foreign_scopes() {
        let local = tempfile::tempdir().unwrap();
        let foreign = tempfile::tempdir().unwrap();
        let bridge = test_event_bridge(local.path(), 0);
        let location = foreign.path().to_string_lossy();
        let payload = serde_json::json!({
            "type": "session.idle",
            "location": {"directory": location},
            "data": {"sessionID": "ses_foreign"}
        })
        .to_string();
        assert!(event_from_payload(payload.as_bytes(), &bridge)
            .unwrap()
            .is_none());

        let location = local.path().to_string_lossy();
        let payload = serde_json::json!({
            "type": "session.idle",
            "location": {"directory": location},
            "data": {"sessionID": "ses_local"}
        })
        .to_string();
        let event = event_from_payload(payload.as_bytes(), &bridge)
            .unwrap()
            .expect("a local event should pass the scope check");
        assert_eq!(event.checkout_id, "test-checkout");
        assert_eq!(event.session_id.as_deref(), Some("ses_local"));
        assert_eq!(
            event_from_payload(b"not-json", &bridge).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn asked_events_keep_the_turn_tracked_instead_of_treating_it_as_failure() {
        use crate::domain::agent::AgentEventKind;
        let agents = AgentService::without_service();
        agents.set_event_sink(Arc::new(|_| {}));
        let sink = agents.sink.lock().unwrap().clone().unwrap();
        for kind in [
            AgentEventKind::PermissionAsked,
            AgentEventKind::QuestionAsked,
        ] {
            sink(AgentEvent {
                checkout_id: "local".into(),
                session_id: Some("ses_waiting".into()),
                kind,
                raw_type: "asked".into(),
                data: serde_json::json!({}),
            });
            assert!(agents.busy_turns.lock().unwrap()["local"].contains_key("ses_waiting"));
        }
    }

    #[test]
    fn pending_requests_survive_reads_until_every_form_and_permission_is_answered() {
        let directory = tempfile::tempdir().unwrap();
        let location = serde_json::json!({"directory": directory.path().to_string_lossy()});
        let listed = serde_json::json!({"data": [{
            "id": "ses_waiting", "title": "Waiting", "location": location,
            "time": {"created": 1, "updated": 2, "idle": 3}
        }]});
        let forms = serde_json::json!({"location": location, "data": [
            {"id": "frm_one", "sessionID": "ses_waiting"},
            {"id": "frm_two", "sessionID": "ses_waiting"}
        ]});
        let permissions = serde_json::json!({"location": location, "data": [
            {"id": "per_one", "sessionID": "ses_waiting"}
        ]});
        let empty = serde_json::json!({"location": location, "data": []});
        let inactive = serde_json::json!({"data": {}});
        let (port, server) = mock_json_responses(vec![
            listed.clone(),
            inactive.clone(),
            forms.clone(),
            permissions.clone(),
            listed.clone(),
            inactive.clone(),
            empty.clone(),
            permissions.clone(),
            listed.clone(),
            inactive.clone(),
            empty.clone(),
            empty.clone(),
            // A fresh scoped read (no preceding events), followed by the removal guard.
            listed.clone(),
            inactive.clone(),
            forms.clone(),
            empty.clone(),
            listed,
            forms,
            empty,
            inactive,
        ]);
        let agents = AgentService::with_test_server(port);
        for pending in [true, true, false] {
            agents.candidates.lock().unwrap().cached = None;
            let sessions = agents
                .candidate_sessions("local", directory.path())
                .unwrap();
            assert_eq!(sessions[0].awaiting_reply, Some(pending));
            assert!(!sessions[0].running, "pending must not fabricate running");
        }
        assert_eq!(
            agents.sessions("local", directory.path()).unwrap()[0].awaiting_reply,
            Some(true)
        );
        let guarded = agents.active_worktree_agent_sessions("local").unwrap();
        assert_eq!(
            guarded.len(),
            1,
            "an untracked pending turn blocks removal even with an idle timestamp"
        );
        assert_eq!(guarded[0].awaiting_reply, Some(true));
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 20);
        assert!(requests
            .iter()
            .filter(|request| request.0.contains("/api/form")
                || request.0.contains("/api/permission/request"))
            .all(|request| request
                .3
                .iter()
                .any(|(name, value)| name.eq_ignore_ascii_case(DIRECTORY_HEADER)
                    && value == directory.path().to_string_lossy().as_ref())));
    }

    #[test]
    fn pending_reads_refuse_foreign_scopes_malformed_payloads_and_http_failure() {
        let local = tempfile::tempdir().unwrap();
        let foreign = tempfile::tempdir().unwrap();
        let (port, server) = mock_json_responses(vec![serde_json::json!({
            "location": {"directory": foreign.path().to_string_lossy()}, "data": []
        })]);
        let bridge = test_event_bridge(local.path(), port);
        assert!(matches!(
            bridge.awaiting_sessions(local.path(), JSON_REQUEST_TIMEOUT),
            Err(BridgeError::Foreign(_))
        ));
        server.join().unwrap();
        let (port, server) =
            mock_json_responses(vec![serde_json::json!({"data": [{"id": "frm_broken"}]})]);
        let bridge = test_event_bridge(local.path(), port);
        assert!(bridge
            .awaiting_sessions(local.path(), JSON_REQUEST_TIMEOUT)
            .is_err());
        server.join().unwrap();
        let (bridge, server) = mock_event_response(
            local.path(),
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
        );
        assert!(
            bridge
                .awaiting_sessions(local.path(), JSON_REQUEST_TIMEOUT)
                .is_err(),
            "404 is not an empty pending list"
        );
        server.join().unwrap();
    }

    #[test]
    fn pending_read_failures_keep_only_safe_categories() {
        let cases = [
            (
                BridgeError::Unavailable("the agent server request timed out".into()),
                PendingReadFailure::Timeout,
            ),
            (
                BridgeError::Failed("the agent server answered 500".into()),
                PendingReadFailure::Http,
            ),
            (
                BridgeError::Stale("the agent server rejected Muster's credentials".into()),
                PendingReadFailure::Http,
            ),
            (
                BridgeError::Foreign("foreign location".into()),
                PendingReadFailure::Scope,
            ),
            (
                BridgeError::Failed("unexpected agent server reply: private question text".into()),
                PendingReadFailure::Decode,
            ),
            (
                BridgeError::Stale("the agent server is not reachable: private text".into()),
                PendingReadFailure::Transport,
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(PendingReadFailure::classify(&error), expected);
            assert_eq!(
                PendingReadFailure::classify(&error).label(),
                expected.label()
            );
        }

        let mut slot = CandidateSlot::new();
        let timeout = [PendingReadFailure::Timeout];
        assert_eq!(
            slot.pending_failures_to_report(&timeout),
            Some(timeout.to_vec())
        );
        assert!(slot.pending_failures_to_report(&timeout).is_none());
        assert!(slot.pending_failures_to_report(&[]).is_none());
        assert!(slot.pending_failures_to_report(&timeout).is_some());
    }

    #[test]
    fn pending_failure_keeps_scoped_reads_and_removal_guards_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let listed = serde_json::json!({"data": [{
            "id": "ses_local", "title": "Local",
            "location": {"directory": directory.path().to_string_lossy()},
            "time": {"created": 1, "updated": 2}
        }]});
        let broken = serde_json::json!({"data": [{"id": "frm_missing_session"}]});
        let (port, server) = mock_json_responses(vec![
            listed.clone(),
            serde_json::json!({"data": {}}),
            broken.clone(),
            listed,
            broken,
        ]);
        let agents = AgentService::with_test_server(port);
        assert!(agents.sessions("local", directory.path()).is_err());
        assert!(agents.active_worktree_agent_sessions("local").is_err());
        server.join().unwrap();
    }

    #[test]
    fn candidate_pending_failure_keeps_sibling_state_and_all_ambiguous_titles() {
        let local = tempfile::tempdir().unwrap();
        let sibling = tempfile::tempdir().unwrap();
        let listed = serde_json::json!({"data": [
            {"id": "ses_local", "title": "Duplicate",
             "location": {"directory": local.path().to_string_lossy()},
             "time": {"created": 1, "updated": 2}},
            {"id": "ses_sibling", "title": "Duplicate",
             "location": {"directory": sibling.path().to_string_lossy()},
             "time": {"created": 1, "updated": 2}}
        ]});
        // Every pending response belongs to the sibling: local validation fails regardless
        // of HashSet iteration order, while the sibling completes both validated reads.
        let pending = serde_json::json!({
            "location": {"directory": sibling.path().to_string_lossy()},
            "data": [{"id": "frm_one", "sessionID": "ses_sibling"}]
        });
        let (port, server) = mock_json_responses(vec![
            listed,
            serde_json::json!({"data": {}}),
            pending.clone(),
            pending.clone(),
            pending,
        ]);
        let agents = AgentService::with_test_server(port);
        let sessions = agents.candidate_sessions("local", local.path()).unwrap();
        assert_eq!(
            sessions.len(),
            2,
            "failed locations must retain ambiguity candidates"
        );
        assert_eq!(
            sessions
                .iter()
                .filter(|session| session.title == "Duplicate")
                .count(),
            2
        );
        assert_eq!(sessions[0].awaiting_reply, None);
        assert_eq!(sessions[1].awaiting_reply, Some(true));
        assert!(!sessions[1].running, "pending must not fabricate running");
        assert_eq!(
            serde_json::to_value(&sessions[0]).unwrap()["awaitingReply"],
            serde_json::Value::Null
        );
        server.join().unwrap();
    }

    #[test]
    fn a_slow_location_does_not_hold_the_snapshot_or_blank_its_sibling() {
        let healthy = tempfile::tempdir().unwrap();
        let slow = tempfile::tempdir().unwrap();
        let listed = serde_json::json!({"data": [
            {"id": "ses_healthy", "title": "Healthy",
             "location": {"directory": healthy.path().to_string_lossy()},
             "time": {"created": 1, "updated": 2}},
            {"id": "ses_slow", "title": "Slow",
             "location": {"directory": slow.path().to_string_lossy()},
             "time": {"created": 1, "updated": 2}}
        ]});
        let pending = HashMap::from([(healthy.path().to_path_buf(), "ses_healthy".to_string())]);
        let port =
            serving_scoped_pending(listed, &pending, &[slow.path().to_path_buf()], None, None);
        let mut agents = AgentService::with_test_server(port);
        agents.pending_read_budget = Duration::from_millis(300);

        let started = Instant::now();
        let sessions = agents.candidate_sessions("local", healthy.path()).unwrap();
        let elapsed = started.elapsed();

        let row = |id: &str| {
            sessions
                .iter()
                .find(|session| session.id == id)
                .unwrap_or_else(|| panic!("{id} is still a candidate"))
        };
        assert_eq!(row("ses_healthy").awaiting_reply, Some(true));
        assert_eq!(
            row("ses_slow").awaiting_reply,
            None,
            "a location that would not answer is unknown, never idle"
        );
        assert_eq!(row("ses_slow").title, "Slow");
        assert!(
            elapsed < Duration::from_secs(2),
            "the healthy location waited for the slow one to be given up on: {elapsed:?}"
        );
    }

    #[test]
    fn pending_endpoints_share_one_deadline_and_expired_location_is_unknown() {
        let directory = tempfile::tempdir().unwrap();
        let listed = serde_json::json!({"data": [{
            "id": "ses_slow", "title": "Slow",
            "location": {"directory": directory.path().to_string_lossy()},
            "time": {"created": 1, "updated": 2}
        }]});
        let path = directory.path().to_path_buf();
        let port = serving_scoped_pending(
            listed,
            &HashMap::new(),
            std::slice::from_ref(&path),
            Some((path.clone(), Duration::from_millis(500))),
            None,
        );
        let mut agents = AgentService::with_test_server(port);
        agents.pending_read_budget = Duration::from_millis(600);

        let started = Instant::now();
        let sessions = agents
            .candidate_sessions("local", directory.path())
            .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(sessions[0].awaiting_reply, None);
        assert!(
            elapsed >= Duration::from_millis(450),
            "form endpoint did not consume most of the budget: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(900),
            "permission endpoint got a fresh timeout: {elapsed:?}"
        );
    }

    #[test]
    fn candidate_budget_covers_slow_initial_session_list() {
        // Far beyond the bound rather than just past it: the budget is what has to end this read, and
        // a fixture that answered a moment after the bound would make the assertion a coin toss
        // between the deadline working and the runner being slow. Nothing waits for the delay, so
        // what it costs the suite is nothing.
        let (port, _, arrived) = serving_candidate_reads(Vec::new(), &[], Duration::from_secs(4));
        let agents = AgentService::with_test_server(port);
        let bridge = agents.bridge("local", Path::new("/")).unwrap();
        let budget = Duration::from_millis(100);

        let started = Instant::now();
        let result = read_candidates(&bridge, budget);
        let elapsed = started.elapsed();

        assert!(arrived.recv_timeout(Duration::from_secs(1)).is_ok());
        assert!(
            result.is_err(),
            "a partial candidate snapshot was published"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "initial list exceeded total budget: {elapsed:?}"
        );
    }

    #[test]
    fn overlapping_relocation_poll_reoffers_pending_moves_and_expires_them() {
        let agents = AgentService::with_test_server(0);
        let move_ = AgentRelocation {
            session_id: "ses_pending".into(),
            from_checkout_id: "origin".into(),
            to_checkout_id: "destination".into(),
            observed_at: 1,
        };
        agents.pending_moves.lock().unwrap().insert(
            move_.session_id.clone(),
            PendingMove {
                relocation: move_.clone(),
                since: Instant::now(),
            },
        );
        let guard = agents.relocation_snapshot.lock().unwrap();
        assert_eq!(agents.relocations(&[]).unwrap(), vec![move_.clone()]);
        drop(guard);
        assert!(agents
            .offer_pending_moves(Instant::now() + PENDING_MOVES_TTL + Duration::from_millis(1))
            .unwrap()
            .is_empty());

        let poisoned = Arc::new(AgentService::with_test_server(0));
        let service = Arc::clone(&poisoned);
        let _ = std::thread::spawn(move || {
            let _guard = service.relocation_snapshot.lock().unwrap();
            panic!("poison snapshot lock for test");
        })
        .join();
        assert!(matches!(
            poisoned.relocations(&[]),
            Err(BridgeError::Failed(_))
        ));
    }

    #[test]
    fn slow_relocation_list_does_not_hold_state_lock_or_queue_a_stale_snapshot() {
        let (port, _, arrived) =
            serving_candidate_reads(Vec::new(), &[], Duration::from_millis(300));
        let agents = Arc::new(AgentService::with_test_server(port));
        let checkout = tempfile::tempdir().unwrap();
        agents.bridge("local", checkout.path()).unwrap();
        let worker = Arc::clone(&agents);
        let checkouts = vec![("local".to_string(), checkout.path().to_path_buf())];
        let poll = std::thread::spawn(move || worker.relocations(&checkouts).unwrap());

        assert!(arrived.recv_timeout(Duration::from_secs(1)).is_ok());
        assert!(
            agents.session_work.try_lock().is_ok(),
            "network I/O held the relocation state lock"
        );
        assert!(agents
            .relocations(&[("local".into(), checkout.path().to_path_buf())])
            .unwrap()
            .is_empty());
        assert!(poll.join().unwrap().is_empty());
    }

    #[test]
    fn pending_request_deadline_covers_a_stalled_response_body() {
        let directory = tempfile::tempdir().unwrap();
        let listed = serde_json::json!({"data": [{
            "id": "ses_slow", "title": "Slow",
            "location": {"directory": directory.path().to_string_lossy()},
            "time": {"created": 1, "updated": 2}
        }]});
        let path = directory.path().to_path_buf();
        // The mock sends headers and half a valid empty envelope promptly, then finishes the body
        // only long after the pending-read deadline has expired. Four seconds is far enough past it
        // that the bound below is about the deadline and not about how quickly the rest arrived.
        let port = serving_scoped_pending(
            listed,
            &HashMap::new(),
            &[],
            None,
            Some((path, Duration::from_secs(4))),
        );
        let mut agents = AgentService::with_test_server(port);
        agents.pending_read_budget = Duration::from_millis(400);

        let started = Instant::now();
        let sessions = agents
            .candidate_sessions("local", directory.path())
            .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(
            sessions[0].awaiting_reply, None,
            "a body timeout is unknown, not idle"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "the body read outlived its shared deadline: {elapsed:?}"
        );
    }

    #[test]
    fn budget_exhaustion_leaves_untouched_locations_unknown_with_their_titles() {
        // More slow locations than there are readers, so some are never even asked: one title
        // repeated across them, which is the ambiguity these rows exist to resolve.
        let slow: Vec<tempfile::TempDir> = (0..PENDING_READ_CONCURRENCY + 2)
            .map(|_| tempfile::tempdir().unwrap())
            .collect();
        let mut data = Vec::new();
        for (index, directory) in slow.iter().enumerate() {
            data.push(serde_json::json!({
                "id": format!("ses_slow{index}"), "title": "Duplicate",
                "location": {"directory": directory.path().to_string_lossy()},
                "time": {"created": 1, "updated": 2}
            }));
        }
        let pending = HashMap::new();
        let slow_paths: Vec<PathBuf> = slow
            .iter()
            .map(|directory| directory.path().to_path_buf())
            .collect();
        let port = serving_scoped_pending(
            serde_json::json!({"data": data}),
            &pending,
            &slow_paths,
            None,
            None,
        );
        let mut agents = AgentService::with_test_server(port);
        agents.pending_read_budget = Duration::from_millis(300);

        let sessions = agents.candidate_sessions("local", slow[0].path()).unwrap();

        assert_eq!(
            sessions.len(),
            slow.len(),
            "every candidate title is kept for ambiguity matching"
        );
        for session in &sessions {
            assert_eq!(session.title, "Duplicate");
            assert_eq!(
                session.awaiting_reply, None,
                "{} was not read inside the budget, so it is unknown rather than idle",
                session.id
            );
        }
    }

    #[test]
    fn candidate_sessions_offer_a_sibling_worktrees_session_while_sessions_do_not() {
        // The measured case: a terminal filed under one worktree had a session open that lives in a
        // sibling one, so the scoped list could not name it and the row drew no state at all.
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let owned = serde_json::json!({
            "id": "ses_owned",
            "title": "Owned session",
            "location": {"directory": first.path().to_string_lossy()},
            "time": {"created": 1, "updated": 2, "idle": 3}
        });
        let sibling = serde_json::json!({
            "id": "ses_sibling",
            "title": "Session from the sibling worktree",
            "location": {"directory": second.path().to_string_lossy()},
            "time": {"created": 4, "updated": 5}
        });
        let (port, server) = mock_json_responses(vec![
            serde_json::json!({"data": [owned.clone(), sibling.clone()]}),
            serde_json::json!({"data": {"ses_sibling": {"type": "running"}}}),
            serde_json::json!({"data": []}),
            serde_json::json!({"data": []}),
            serde_json::json!({"data": [owned.clone(), sibling.clone()]}),
            serde_json::json!({"data": {"ses_sibling": {"type": "running"}}}),
            serde_json::json!({"data": []}),
            serde_json::json!({"data": []}),
            serde_json::json!({"data": []}),
            serde_json::json!({"data": []}),
        ]);
        let agents = AgentService::with_test_server(port);

        // What a review round gets: this worktree's own sessions only.
        let scoped = agents.sessions("first", first.path()).unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].id, "ses_owned");
        // What a terminal row gets: both, because a row asks which session THIS TERMINAL has open and
        // the only evidence for that is the title the terminal wrote. The candidate that is running is
        // the sibling's, and it is running because the service said so.
        let repo = agents.candidate_sessions("first", first.path()).unwrap();
        assert_eq!(repo.len(), 2);
        let sibling_row = repo
            .iter()
            .find(|session| session.id == "ses_sibling")
            .unwrap();
        assert!(sibling_row.running);

        let requests = server.join().expect("mock server should finish");
        let route = |line: &str| line.split('?').next().unwrap_or_default().to_string();
        assert_eq!(route(&requests[0].0), "GET /api/session");
        assert_eq!(route(&requests[4].0), "GET /api/session");
        // The answer above only exists because the second read of the same route named no
        // directory. This is the assertion that was missing: the mock replays the same sessions for
        // either request, so the test passed while the read was scoped and the real service — which
        // filters by that directory — answered with the two sessions this worktree owns.
        assert!(
            !requests[4].0.contains("directory="),
            "the candidate read must not be scoped: {}",
            requests[4].0
        );
        assert!(!requests[4]
            .3
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(DIRECTORY_HEADER)));
        // It asks for more than the route's own fifty, because a terminal left open on an older
        // session is still a row in the panel.
        assert!(requests[4]
            .0
            .contains(&format!("limit={CANDIDATE_SESSION_LIMIT}")));
    }

    #[test]
    fn one_unscoped_read_answers_every_checkout_that_asks_in_the_same_tick() {
        // The service's session list is the same for every checkout, so a poll that arrives once per
        // checkout must share one snapshot. The mock serves exactly the responses one read makes;
        // a second read would find nothing listening and fail, which is the regression this pins.
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let listed = serde_json::json!({
            "data": [{
                "id": "ses_one",
                "title": "One",
                "location": {"directory": second.path().to_string_lossy()},
                "time": {"created": 1, "updated": 2}
            }]
        });
        let (port, server) = mock_json_responses(vec![
            listed.clone(),
            serde_json::json!({"data": {"ses_one": {"type": "running"}}}),
            serde_json::json!({"location": {"directory": second.path().to_string_lossy()}, "data": [{"id": "frm_one", "sessionID": "ses_one"}]}),
            serde_json::json!({"data": []}),
        ]);
        let agents = AgentService::with_test_server(port);

        let asked_first = agents.candidate_sessions("first", first.path()).unwrap();
        let asked_second = agents.candidate_sessions("second", second.path()).unwrap();
        assert_eq!(asked_first.len(), 1);
        // Each checkout answers with its own id on the sessions, because that is what the row and the
        // review round are given, while the list behind them was read once.
        assert_eq!(asked_first[0].checkout_id, "first");
        assert_eq!(asked_second[0].checkout_id, "second");
        assert_eq!(asked_second[0].id, "ses_one");
        assert!(asked_second[0].running);
        assert_eq!(asked_first[0].awaiting_reply, Some(true));
        assert_eq!(asked_second[0].awaiting_reply, Some(true));
        let requests = server.join().expect("mock server should finish");
        assert_eq!(requests.len(), 4);
        assert!(
            requests[2]
                .3
                .iter()
                .any(|(name, value)| name.eq_ignore_ascii_case(DIRECTORY_HEADER)
                    && value == second.path().to_string_lossy().as_ref()),
            "pending must be read at the candidate's location, not the caller's"
        );
    }

    /// One session as the mock candidate service answers it, in `directory`.
    fn listed_session(directory: &Path, id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "title": "Listed",
            "location": {"directory": directory.to_string_lossy()},
            "time": {"created": 1, "updated": 2}
        })
    }

    #[test]
    fn nine_checkouts_polling_at_once_share_one_service_wide_read() {
        // A poll arrives once per checkout, so nine of them in the same tick must cost one read: the
        // mock counts the requests, and the list is served to whoever asked from the answer of the
        // read that was already running. Nine callers arriving after that read finished would be
        // answered from the one-second window instead, which is why this cannot tell the two apart —
        // so the callers are all released while the read is on the wire.
        let directory = tempfile::tempdir().unwrap();
        let (port, counts, arrived) = serving_candidate_reads(
            vec![listed_session(directory.path(), "ses_one")],
            &["ses_one"],
            Duration::from_millis(400),
        );
        let agents = Arc::new(AgentService::with_test_server(port));
        let (answers, answered) = mpsc::channel();

        let first = {
            let agents = Arc::clone(&agents);
            let answers = answers.clone();
            let directory = directory.path().to_path_buf();
            std::thread::spawn(move || {
                let _ = answers.send(agents.candidate_sessions("first", &directory));
            })
        };
        arrived
            .recv_timeout(Duration::from_secs(5))
            .expect("the first read should reach the service");
        let mut callers = vec![first];
        for index in 0..8 {
            let agents = Arc::clone(&agents);
            let directory = directory.path().to_path_buf();
            let answers = answers.clone();
            callers.push(std::thread::spawn(move || {
                let _ = answers
                    .send(agents.candidate_sessions(&format!("checkout-{index}"), &directory));
            }));
        }

        for _ in 0..9 {
            // The bound is the point of the assertion as much as the count is: a caller that queued
            // behind a lock the read held would have to wait for the whole of it, and a deadlock
            // would be a hang instead of a failure.
            let sessions = answered
                .recv_timeout(Duration::from_secs(10))
                .expect("every caller that shared the read should answer");
            assert_eq!(sessions.expect("the read should succeed").len(), 1);
        }
        for caller in callers {
            caller.join().expect("the caller thread should finish");
        }
        assert_eq!(
            counts.requests.load(Ordering::SeqCst),
            4,
            "nine callers must share the list, running answer and pending lookups"
        );
    }

    #[test]
    fn a_caller_arriving_during_a_read_finds_the_cache_lock_free() {
        // The read used to happen with the cache lock held, so everything that has to
        // touch the cache — including the event reader, which reaches it from its own thread — was
        // stuck for the whole of it. The lock is only ever held for the state now, which a test can
        // take while a read is on the wire, and a caller that arrives then shares that read rather
        // than start one of its own.
        let directory = tempfile::tempdir().unwrap();
        let (port, counts, arrived) = serving_candidate_reads(
            vec![listed_session(directory.path(), "ses_one")],
            &[],
            Duration::from_millis(400),
        );
        let agents = Arc::new(AgentService::with_test_server(port));
        let (answers, answered) = mpsc::channel();

        let first = {
            let agents = Arc::clone(&agents);
            let answers = answers.clone();
            let directory = directory.path().to_path_buf();
            std::thread::spawn(move || {
                let _ = answers.send(agents.candidate_sessions("first", &directory));
            })
        };
        arrived
            .recv_timeout(Duration::from_secs(5))
            .expect("the first read should reach the service");
        assert!(
            agents.candidates.try_lock().is_ok(),
            "the candidate cache lock must not be held while a read is on the wire"
        );
        let second = {
            let agents = Arc::clone(&agents);
            let answers = answers.clone();
            let directory = directory.path().to_path_buf();
            std::thread::spawn(move || {
                let _ = answers.send(agents.candidate_sessions("second", &directory));
            })
        };

        for caller in [first, second] {
            let sessions = answered
                .recv_timeout(Duration::from_secs(10))
                .expect("a caller arriving during a read should answer when that read does");
            assert_eq!(sessions.expect("the shared read should succeed").len(), 1);
            caller.join().expect("the caller thread should finish");
        }
        assert_eq!(
            counts.requests.load(Ordering::SeqCst),
            4,
            "the caller that arrived during the read must share it"
        );
    }

    #[test]
    fn a_bridge_reads_over_one_pooled_connection() {
        // One agent per request is one connection pool per request, so every read opened its own
        // socket to a service one loopback hop away. The mock counts the sockets it had to accept,
        // and a candidate snapshot with pending lookups should reuse one connection.
        let directory = tempfile::tempdir().unwrap();
        let (port, counts, _arrived) = serving_candidate_reads(
            vec![listed_session(directory.path(), "ses_one")],
            &[],
            Duration::ZERO,
        );
        let agents = AgentService::with_test_server(port);

        let sessions = agents
            .candidate_sessions("first", directory.path())
            .expect("the read should succeed");

        assert_eq!(sessions.len(), 1);
        assert_eq!(counts.requests.load(Ordering::SeqCst), 4);
        assert_eq!(
            counts.connections.load(Ordering::SeqCst),
            1,
            "one read and its pending lookups should travel over one pooled connection"
        );
    }

    #[test]
    fn the_agent_catalog_is_cached_per_checkout_and_never_shared_across_directories() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let first_catalog = serde_json::json!({
            "data": [{"id": "build", "name": "Build", "mode": "primary"}],
            "location": {"directory": first.path().to_string_lossy()}
        });
        let second_catalog = serde_json::json!({
            "data": [{"id": "local", "name": "Local", "mode": "subagent"}],
            "location": {"directory": second.path().to_string_lossy()}
        });
        let (port, server) =
            mock_json_responses(vec![first_catalog.clone(), second_catalog.clone()]);
        let agents = AgentService::with_test_server(port);

        // Asked twice, read once: a catalog is a palette, not a turn state.
        assert_eq!(agents.agents("first", first.path()).unwrap()[0].id, "build");
        assert_eq!(agents.agents("first", first.path()).unwrap()[0].id, "build");
        // And the second directory's answer is never the first one's. The mock is gone by now, so a
        // cache that ignored the checkout would have served `build` here and passed.
        assert_eq!(
            agents.agents("second", second.path()).unwrap()[0].id,
            "local"
        );
        assert_eq!(server.join().expect("mock server should finish").len(), 2);
    }

    #[test]
    fn owned_loopback_api_auth_scopes_catalog_and_prompt_contract_to_checkout() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let first_directory = first.path().to_string_lossy().into_owned();
        let second_directory = second.path().to_string_lossy().into_owned();
        let owned_session = serde_json::json!({
            "id": "ses_owned",
            "title": "Owned session",
            "location": {"directory": first_directory},
            "time": {"created": 1, "updated": 2, "idle": 3}
        });
        let foreign_session = serde_json::json!({
            "id": "ses_foreign",
            "title": "Foreign session",
            "location": {"directory": second_directory}
        });
        let (first_port, first_server) = mock_json_responses(vec![
            serde_json::json!({
                "data": [{"id": "build", "name": "Build", "mode": "primary", "color": "#123456"}],
                "location": {"directory": first_directory}
            }),
            serde_json::json!({"data": [owned_session.clone(), foreign_session]}),
            // The service's own running answer for the list read just above.
            serde_json::json!({"data": {"ses_owned": {"type": "running"}}}),
            serde_json::json!({"data": []}),
            serde_json::json!({"data": []}),
            serde_json::json!({"data": owned_session.clone()}),
            serde_json::json!({"data": {"accepted": true}}),
            serde_json::json!({"data": owned_session.clone()}),
            serde_json::json!({"data": {"ses_owned": {"type": "running"}}}),
            serde_json::json!({"data": []}),
            serde_json::json!({"data": []}),
        ]);
        let (second_port, second_server) = mock_json_responses(vec![serde_json::json!({
            "data": {
                "id": "ses_owned",
                "title": "Wrong checkout",
                "location": {"directory": first_directory}
            }
        })]);
        let agents = AgentService::with_test_server(first_port);
        let other_agents = AgentService::with_test_server(second_port);

        let catalog = agents.agents("first", first.path()).unwrap();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].id, "build");
        let sessions = agents.sessions("first", first.path()).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "ses_owned");
        // `running` is the service's own answer from `/api/session/active`, not an inference
        // from an idle time, so a turn this client never prompted still reads as running.
        assert!(sessions[0].running);
        let prompted = agents
            .prompt("first", first.path(), "ses_owned", "  send one prompt  ")
            .unwrap();
        assert_eq!(prompted.id, "ses_owned");
        assert_eq!(prompted.idle_at, Some(3));
        assert!(matches!(
            other_agents.owned_session("second", second.path(), "ses_owned"),
            Err(BridgeError::Foreign(_))
        ));

        let first_requests = first_server
            .join()
            .expect("first mock server should finish");
        assert_eq!(first_requests.len(), 11);
        // Compared on the route only: the query carries this checkout's own temporary path,
        // which differs on every run and is asserted on in the scoping test below.
        let route = |line: &str| line.split('?').next().unwrap_or_default().to_string();
        assert_eq!(
            first_requests
                .iter()
                .map(|request| route(&request.0))
                .collect::<Vec<_>>(),
            [
                "GET /api/agent",
                "GET /api/session",
                "GET /api/session/active",
                "GET /api/form",
                "GET /api/permission/request",
                "GET /api/session/ses_owned",
                "POST /api/session/ses_owned/prompt",
                "GET /api/session/ses_owned",
                "GET /api/session/active",
                "GET /api/form",
                "GET /api/permission/request",
            ]
        );
        // Every one of them is scoped, which is what makes the answers above this checkout's.
        assert!(first_requests
            .iter()
            .all(|request| request.0.contains("directory=") && request.0.ends_with(" HTTP/1.1")));
        assert!(first_requests.iter().all(|request| request
            .3
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case(DIRECTORY_HEADER)
                && value == &first_directory)));
        let first_authorization = crate::services::opencode::authorization("test");
        assert!(first_requests
            .iter()
            .all(|request| request.1 == first_authorization));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&first_requests[6].2).unwrap(),
            serde_json::json!({"text": "send one prompt"})
        );

        let second_requests = second_server
            .join()
            .expect("second mock server should finish");
        assert_eq!(second_requests.len(), 1);
        assert_eq!(route(&second_requests[0].0), "GET /api/session/ses_owned");
        assert_eq!(second_requests[0].1, first_authorization);
    }

    /// A service whose `ses_bad` history is refused and whose `ses_good` history reports a turn that
    /// ran in `destination`.
    ///
    /// `refusal_delay` is served before the refusal, which is what lets a test tell a read that was
    /// abandoned at the budget from one that was waited out. The channel says when the refused read
    /// reached the wire rather than when the refusal came back, because what a test needs to know
    /// is that the read was attempted at all: without it a budget spent before the request says
    /// nothing here, and the failure surfaces later as an unexplained cursor that never advanced.
    fn serving_refused_history(
        origin: &Path,
        destination: &Path,
        refusal_delay: Duration,
    ) -> (u16, mpsc::Receiver<()>) {
        let sessions = ["ses_bad", "ses_good"].map(|id| {
            serde_json::json!({
                "id": id,
                "time": {"updated": 2},
                "location": {"directory": origin}
            })
        });
        let listed = Arc::new(serde_json::json!({"data": sessions}));
        let good_history = Arc::new(serde_json::json!({"data": [{
            "type": "assistant",
            "content": [{
                "type": "tool", "name": "shell", "time": {"completed": 10},
                "state": {"status": "completed", "input": {"workdir": destination}, "metadata": {}}
            }]
        }]}));
        let (refused_tx, refused) = mpsc::channel();
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock service should bind");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for incoming in listener.incoming() {
                let Ok(mut stream) = incoming else { break };
                let listed = Arc::clone(&listed);
                let good_history = Arc::clone(&good_history);
                let refused_tx = refused_tx.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                    let mut request = String::new();
                    if reader.read_line(&mut request).unwrap_or(0) == 0 {
                        return;
                    }
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                            break;
                        }
                    }
                    // Only the refused read is meant to outlast a budget; the fixed costs stay small
                    // so a loaded runner cannot spend the budget before that read is attempted.
                    let (status, body, delay) = if request.starts_with("GET /api/session/active") {
                        (
                            "200 OK",
                            serde_json::json!({"data": {}}),
                            Duration::from_millis(10),
                        )
                    } else if request.starts_with("GET /api/session?") {
                        ("200 OK", (*listed).clone(), Duration::from_millis(10))
                    } else if request.starts_with("GET /api/session/ses_bad/message") {
                        let _ = refused_tx.send(());
                        (
                            "503 Service Unavailable",
                            serde_json::json!({"data": []}),
                            refusal_delay,
                        )
                    } else if request.starts_with("GET /api/session/ses_good/message") {
                        ("200 OK", (*good_history).clone(), Duration::ZERO)
                    } else {
                        ("200 OK", serde_json::json!({"data": []}), Duration::ZERO)
                    };
                    sleep(delay);
                    let body = serde_json::to_vec(&body).expect("the reply should encode");
                    let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    let _ = stream.write_all(&body);
                });
            }
        });
        (port, refused)
    }

    /// A panel that has already seen both sessions work in `origin`, with `ses_bad` carrying a target
    /// verified before anything went wrong, and the fair cursor parked on `ses_good`.
    ///
    /// Parking the cursor is what makes the next round start at `ses_bad`, so the failing read is
    /// the first thing it attempts; `ses_good` is what the round after that has to let through.
    /// Returns the service, the checkouts a read is asked about, and the target `ses_bad` starts with.
    fn service_awaiting_a_refused_history(
        port: u16,
        origin: &Path,
        destination: &Path,
    ) -> (AgentService, Vec<(String, PathBuf)>, PathBuf) {
        let origin_path = origin.to_path_buf();
        let agents = AgentService::with_test_server(port);
        agents.bridge("origin", &origin_path).unwrap();
        let now = Instant::now();
        let previous_target = origin_path.join("verified-before-failure");
        {
            let mut work = agents.session_work.lock().unwrap();
            for id in ["ses_bad", "ses_good"] {
                work.insert(
                    id.into(),
                    SessionWork {
                        origin: origin_path.clone(),
                        effective: Some(origin_path.clone()),
                        targets: if id == "ses_bad" {
                            vec![previous_target.clone()]
                        } else {
                            Vec::new()
                        },
                        completed: 1,
                        updated: 1,
                        running: false,
                        seen: now,
                    },
                );
            }
            *agents.relocation_history_cursor.lock().unwrap() = Some("ses_good".into());
        }
        for id in ["ses_bad", "ses_good"] {
            agents.session_locations.lock().unwrap().insert(
                id.into(),
                SessionLocation {
                    checkout_id: Some("origin".into()),
                    seen: now,
                },
            );
        }
        let checkouts = vec![
            ("origin".into(), origin_path),
            ("destination".into(), destination.to_path_buf()),
        ];
        (agents, checkouts, previous_target)
    }

    /// Long enough that a read which waited for the reply could not be mistaken for one that gave up
    /// on it, and short enough that the mock's own threads, which nothing joins, are asleep again
    /// long before a test binary that runs them in parallel is done.
    const REFUSED_HISTORY_DELAY: Duration = Duration::from_secs(4);

    /// What a round is given. The refused read is four seconds away from this, so the two cannot be
    /// confused for one another the way a budget and a marginally longer delay can.
    const RELOCATION_BUDGET: Duration = Duration::from_millis(350);

    #[test]
    fn failed_history_advances_fair_cursor_and_keeps_other_session_progress() {
        let origin = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        // The refusal is as slow as the other test's, and that is not a timing claim here: what the
        // slow read buys is that the round runs out of budget on the failing session, which is the
        // situation this test describes. A refusal answered instantly would let the same round read
        // `ses_good` too, and "the next round lets it through" would stop being a statement about
        // fairness at all. The budget itself is measured elsewhere.
        let (port, refused) =
            serving_refused_history(origin.path(), destination.path(), REFUSED_HISTORY_DELAY);
        let (agents, checkouts, previous_target) =
            service_awaiting_a_refused_history(port, origin.path(), destination.path());

        let first = agents
            .relocations_with_budget(&checkouts, false, RELOCATION_BUDGET)
            .unwrap();

        refused
            .recv_timeout(Duration::from_secs(1))
            .expect("the refused history read was never attempted");
        assert!(first.is_empty());
        let after_first = agents.session_work.lock().unwrap();
        assert_eq!(
            after_first["ses_bad"].updated, 1,
            "failed history was acknowledged"
        );
        assert_eq!(after_first["ses_bad"].targets, vec![previous_target]);
        assert_eq!(
            *agents.relocation_history_cursor.lock().unwrap(),
            Some("ses_bad".into())
        );
        drop(after_first);

        let second = agents
            .relocations_with_budget(&checkouts, false, RELOCATION_BUDGET)
            .unwrap();
        assert!(second
            .iter()
            .any(|move_| move_.session_id == "ses_good" && move_.to_checkout_id == "destination"));
        assert!(agents.session_work.lock().unwrap()["ses_good"].completed == 10);
    }

    #[test]
    fn a_history_read_that_will_not_answer_is_abandoned_at_the_budget() {
        let origin = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let (port, refused) =
            serving_refused_history(origin.path(), destination.path(), REFUSED_HISTORY_DELAY);
        let (agents, checkouts, _previous_target) =
            service_awaiting_a_refused_history(port, origin.path(), destination.path());

        let started = Instant::now();
        let first = agents
            .relocations_with_budget(&checkouts, false, RELOCATION_BUDGET)
            .unwrap();
        let elapsed = started.elapsed();

        refused
            .recv_timeout(Duration::from_secs(1))
            .expect("the read that never answers was never attempted");
        assert!(first.is_empty());
        // Generous enough that a loaded runner passes it, and still well short of the four seconds
        // the refusal takes: a round that waited for the reply would land here at about four.
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    }

    #[test]
    fn relocation_baseline_refreshes_tool_targets_without_replaying_disabled_work() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let third = tempfile::tempdir().unwrap();
        let fourth = tempfile::tempdir().unwrap();
        let first_path = first.path().to_string_lossy().into_owned();
        let second_path = second.path().to_string_lossy().into_owned();
        let third_path = third.path().to_string_lossy().into_owned();
        let fourth_path = fourth.path().to_string_lossy().into_owned();
        let checkouts = vec![
            ("first".to_string(), first.path().to_path_buf()),
            ("second".to_string(), second.path().to_path_buf()),
            ("third".to_string(), third.path().to_path_buf()),
            ("fourth".to_string(), fourth.path().to_path_buf()),
        ];
        let list = |updated| {
            serde_json::json!({
                "data": [{
                    "id": "ses_move",
                    "time": {"updated": updated},
                    "location": {"directory": first_path}
                }]
            })
        };
        let target = |path: &str, completed| {
            serde_json::json!({
                "data": [{
                    "type": "assistant",
                    "content": [{
                        "type": "tool",
                        "name": "shell",
                        "time": {"completed": completed},
                        "state": {
                            "status": "completed",
                            "input": {"workdir": path, "command": "git status"},
                            "metadata": {}
                        }
                    }]
                }]
            })
        };
        let completed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            + 60_000;
        let (port, server) = mock_json_responses(vec![
            serde_json::json!({"data": [{"id": "build", "name": "Build", "mode": "primary"}]}),
            list(1),
            list(2),
            serde_json::json!({"data": {}}),
            target(&second_path, completed),
            list(3),
            serde_json::json!({"data": {}}),
            target(&third_path, completed + 60_000),
            list(3),
            serde_json::json!({"data": {}}),
            list(4),
            serde_json::json!({"data": {}}),
            target(&fourth_path, completed + 120_000),
        ]);
        let agents = AgentService::with_test_server(port);
        agents.agents("first", first.path()).unwrap();
        assert!(agents.relocations(&checkouts).unwrap().is_empty());
        assert_relocation(
            &agents.relocations(&checkouts).unwrap(),
            "ses_move",
            "first",
            "second",
        );
        // Work changes again while follow is off; the baseline refreshes the tool-derived target.
        assert!(agents
            .relocations_with_baseline(&checkouts, true)
            .unwrap()
            .is_empty());
        assert!(agents.relocations(&checkouts).unwrap().is_empty());
        assert_relocation(
            &agents.relocations(&checkouts).unwrap(),
            "ses_move",
            "third",
            "fourth",
        );
        server.join().expect("mock server should finish");
    }

    /// Every request that asks about a worktree names the directory twice, because the service
    /// reads the scope two ways.
    ///
    /// This is not redundancy: with only the header, `/api/session` answers for every location the
    /// service knows (50 sessions where the checkout has 2), and with only the query, `/api/agent`
    /// answers for the service's own working directory. A worktree's own session list and its agent
    /// catalog are built out of these answers, so a missing half of it is a wrong answer.
    ///
    /// The candidate read a terminal row is built from is the documented exception: it asks which
    /// session a terminal has open, and a session list filtered by directory cannot answer that for a
    /// terminal whose session lives in another worktree. `candidate_sessions_offer_a_sibling_
    /// worktrees_session_while_sessions_do_not` pins that it names no directory at all.
    #[test]
    fn every_request_carries_the_directory_as_a_header_and_as_a_query_parameter() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_string_lossy().into_owned();
        let (port, server) = mock_json_responses(vec![serde_json::json!({
            "data": [{"id": "build", "name": "Build", "mode": "primary"}],
            "location": {"directory": path}
        })]);
        let bridge = test_event_bridge(directory.path(), port);
        let catalog: Vec<serde_json::Value> = bridge.get_json("/api/agent").unwrap();
        assert_eq!(catalog.len(), 1);

        let requests = server.join().expect("mock server should finish");
        let request_line = &requests[0].0;
        assert!(
            request_line.starts_with("GET /api/agent?directory="),
            "{request_line}"
        );
        // The value is percent-encoded, so the path cannot be read as a second query parameter.
        assert!(!request_line.contains("?directory=/"), "{request_line}");
        let header = requests[0]
            .3
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(DIRECTORY_HEADER))
            .expect("every request should name the directory it is scoped to");
        assert_eq!(header.1, path);
    }

    #[test]
    fn tool_work_follows_its_session_without_a_location_change_or_worktree_guess() {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join(".worktrees/test");
        for args in [
            vec!["init", "--initial-branch=main"],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "fixture",
            ],
            vec!["worktree", "add", "-b", "test", nested.to_str().unwrap()],
        ] {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(root.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let file = nested.join("created.txt");
        std::fs::write(&file, "created by the agent").unwrap();
        let main = vec![
            ("home".to_string(), home.path().to_path_buf()),
            ("main".to_string(), root.path().to_path_buf()),
        ];
        let both = vec![
            ("home".to_string(), home.path().to_path_buf()),
            ("main".to_string(), root.path().to_path_buf()),
            ("test".to_string(), nested.clone()),
        ];
        let completed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            + 60_000;
        let session = |id: &str, updated| {
            serde_json::json!({
                "id": id, "time": {"updated": updated}, "location": {"directory": root.path()}
            })
        };
        let list = |updated| {
            serde_json::json!({"data": [session("ses_writer", updated), session("ses_other", 1),
            {"id": "ses_child", "parentID": "ses_writer", "location": {"directory": nested}}]})
        };
        let message = |name: &str, input: serde_json::Value, metadata: serde_json::Value| {
            serde_json::json!({"data": [{
                "type": "assistant", "content": [{"type": "tool", "name": name,
                    "time": {"completed": completed}, "state": {"status": "completed", "input": input, "metadata": metadata}}]
            }]})
        };
        let (port, server) = mock_json_responses(vec![
            serde_json::json!({"data": [{"id": "build", "name": "Build", "mode": "primary"}]}),
            list(1),
            list(2),
            serde_json::json!({"data": {"ses_writer": {"type": "running"}}}),
            // The command creates a worktree, but contains no execution directory. No inference.
            message(
                "shell",
                serde_json::json!({"command": "git worktree add .worktrees/test"}),
                serde_json::json!({}),
            ),
            list(2),
            serde_json::json!({"data": {"ses_writer": {"type": "running"}, "ses_other": {"type": "running"}}}),
            // Another session's work remains attributable to that session, not the newest worktree.
            message(
                "execute",
                serde_json::json!({"code": "not inspected"}),
                serde_json::json!({"toolCalls": [
                    {"tool": "lean-ctx.ctx_shell", "status": "completed", "input": {"cwd": root.path(), "command": "git status"}}
                ]}),
            ),
            message(
                "write",
                serde_json::json!({"path": file, "content": "console.log(\"Hello, world!\");"}),
                serde_json::json!({}),
            ),
            list(2),
            serde_json::json!({"data": {}}),
            message(
                "shell",
                serde_json::json!({"workdir": root.path(), "command": "git status"}),
                serde_json::json!({}),
            ),
            // Read once after running -> idle, even if session.time.updated did not change.
            message(
                "patch",
                serde_json::json!({"patchText": format!("*** Begin Patch\n*** Add File: {}\n+created\n*** End Patch", file.display())}),
                serde_json::json!({}),
            ),
            list(2),
            serde_json::json!({"data": {}}),
        ]);
        let agents = AgentService::with_test_server(port);
        // The terminal is filed under Home, while the agent session lives in the registered repo.
        agents.agents("home", home.path()).unwrap();
        assert!(agents.relocations(&main).unwrap().is_empty());
        // The worktree and file already exist, which alone must never move a terminal.
        assert!(agents.relocations(&both).unwrap().is_empty());
        // File work happened before the worktree was registered. Keep the original target.
        assert!(agents.relocations(&main).unwrap().is_empty());
        let moved = agents.relocations(&both).unwrap();
        assert_relocation(&moved, "ses_writer", "main", "test");
        // The API still says main, but cannot pull the terminal back after the tool turn ends.
        assert_eq!(agents.relocations(&both).unwrap(), moved);
        let requests = server.join().unwrap();
        let histories: Vec<_> = requests
            .iter()
            .filter(|request| request.0.contains("/message?"))
            .collect();
        assert_eq!(histories.len(), 5);
        assert!(histories
            .iter()
            .all(|request| !request.0.contains("ses_child")));
        assert!(histories[0].0.contains("/ses_writer/message?"));
        assert!(histories[1].0.contains("/ses_other/message?"));
        assert!(histories[2].0.contains("/ses_writer/message?"));
        assert!(histories[3].0.contains("/ses_other/message?"));
        assert!(histories[4].0.contains("/ses_writer/message?"));
        for request in histories {
            assert_eq!(request.1, crate::services::opencode::authorization("test"));
            assert!(!request
                .3
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(DIRECTORY_HEADER)));
        }
    }

    #[test]
    fn tool_work_rejects_ambiguous_unexecuted_unbased_and_read_only_targets() {
        use super::{latest_work, work_directory};
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let checkouts = vec![
            ("first".into(), first.path().to_path_buf()),
            ("second".into(), second.path().to_path_buf()),
        ];
        assert!(work_directory(&checkouts, &[first.path().into(), second.path().into()]).is_none());
        assert!(work_directory(&checkouts, &[PathBuf::from("relative")]).is_none());
        let foreign = tempfile::tempdir().unwrap();
        assert!(work_directory(&checkouts, &[foreign.path().into()]).is_none());
        let tool = |status: &str| {
            serde_json::json!({"type":"assistant", "content": [{
                "type":"tool", "name":"execute", "time":{"completed":10},
                "state":{"status":status, "metadata":{"toolCalls":[
                    {"tool":"lean-ctx.ctx_patch", "status":"completed", "input":{"path":first.path()}},
                    {"tool":"lean-ctx.ctx_patch", "status":"completed", "input":{"path":second.path()}}
                ]}}
            }]})
        };
        let (stamp, targets) = latest_work(&[tool("completed")], 9, first.path()).unwrap();
        assert_eq!(stamp, 10);
        assert!(work_directory(&checkouts, &targets).is_none());
        assert!(latest_work(&[tool("running"), tool("error")], 9, first.path()).is_none());
        assert!(latest_work(&[tool("completed")], 10, first.path()).is_none());
        let mut single = tool("completed");
        single["content"][0]["state"]["metadata"]["toolCalls"]
            .as_array_mut()
            .unwrap()
            .truncate(1);
        let (_, targets) = latest_work(&[single.clone()], 9, first.path()).unwrap();
        assert_eq!(
            work_directory(&checkouts, &targets),
            Some(first.path().canonicalize().unwrap())
        );
        single["content"][0]["state"]["metadata"]["truncated"] = serde_json::json!(true);
        assert!(latest_work(&[single], 9, first.path()).is_none());
        let read = serde_json::json!({"type":"assistant", "content":[{
            "type":"tool", "name":"execute", "time":{"completed":11}, "state":{"status":"completed",
            "input":{"code":"ctx_patch at another worktree"}, "metadata":{"toolCalls":[
                {"tool":"lean-ctx.ctx_read", "status":"completed", "input":{"path":second.path()}}
            ]}}
        }]});
        assert!(latest_work(&[read], 9, first.path()).is_none());
    }

    #[test]
    fn opencode_mutation_tools_resolve_documented_relative_paths_from_session_location() {
        use super::{latest_work, work_directory};

        let sandbox = tempfile::tempdir().unwrap();
        let root = sandbox.path().join("checkout");
        let source = root.join("src");
        let foreign = sandbox.path().join("foreign");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&foreign).unwrap();
        let edited = source.join("main.rs");
        let written = root.join("created.rs");
        let deleted = source.join("removed.rs");
        let escaped = foreign.join("outside.rs");
        for path in [&edited, &written, &deleted, &escaped] {
            std::fs::write(path, "fixture").unwrap();
        }
        std::fs::remove_file(&deleted).unwrap();

        let checkouts = vec![("root".to_string(), root.clone())];
        let message = |name: &str, input: serde_json::Value| {
            serde_json::json!({"type":"assistant", "content":[{
                "type":"tool", "name":name, "time":{"completed":10},
                "state":{"status":"completed", "input":input}
            }]})
        };
        let cases = [
            (
                "edit",
                serde_json::json!({"path":"src/main.rs", "oldString":"a", "newString":"b"}),
                edited,
            ),
            (
                "write",
                serde_json::json!({"path":"created.rs", "content":"created"}),
                written,
            ),
            (
                "shell",
                serde_json::json!({"workdir":source.to_str().unwrap(), "command":"not parsed"}),
                source,
            ),
            (
                "patch",
                serde_json::json!({"patchText":"*** Begin Patch\n*** Delete File: src/removed.rs\n*** End Patch"}),
                deleted,
            ),
        ];
        for (name, input, target) in cases {
            let (_, targets) = latest_work(&[message(name, input)], 9, &root).unwrap();
            assert_eq!(targets, vec![target]);
            assert_eq!(
                work_directory(&checkouts, &targets),
                Some(root.canonicalize().unwrap())
            );
        }
        let (_, targets) = latest_work(
            &[message(
                "shell",
                serde_json::json!({"workdir":"src", "command":"not parsed"}),
            )],
            9,
            &root,
        )
        .unwrap();
        assert!(work_directory(&checkouts, &targets).is_none());

        let (_, targets) = latest_work(
            &[message(
                "edit",
                serde_json::json!({"path":"../foreign/outside.rs", "oldString":"a", "newString":"b"}),
            )],
            9,
            &root,
        )
        .unwrap();
        assert!(work_directory(&checkouts, &targets).is_none());

        // lean-ctx's cwd/path base is not specified as the owning OpenCode Location.
        let (_, targets) = latest_work(
            &[message(
                "lean-ctx.ctx_patch",
                serde_json::json!({"path":"src/main.rs"}),
            )],
            9,
            &root,
        )
        .unwrap();
        assert!(work_directory(&checkouts, &targets).is_none());
    }

    #[test]
    fn subagent_tool_input_does_not_attribute_child_work_to_its_parent() {
        use super::latest_work;

        let root = tempfile::tempdir().unwrap();
        let parent = serde_json::json!({"type":"assistant", "content":[{
            "type":"tool", "name":"execute", "time":{"completed":10},
            "state":{"status":"completed", "input":{"code":"not inspected"}, "metadata":{"toolCalls":[
                {"tool":"subagent", "status":"completed", "input":{
                    "agent":"worker", "description":"make a change", "prompt":"update a source file"
                }}
            ]}}
        }]});
        assert!(latest_work(&[parent], 9, root.path()).is_none());
    }

    #[test]
    fn history_reads_rotate_after_the_last_attempt_for_deterministic_fairness() {
        use super::{rotate_sessions_after, ApiSession};

        let sessions = || {
            ["ses_first", "ses_middle", "ses_last"]
                .into_iter()
                .map(|id| {
                    serde_json::from_value::<ApiSession>(serde_json::json!({"id":id})).unwrap()
                })
                .collect::<Vec<_>>()
        };
        let ids = |sessions: &[ApiSession]| {
            sessions
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>()
                .join(",")
        };

        let mut page = sessions();
        rotate_sessions_after(&mut page, Some("ses_first"));
        assert_eq!(ids(&page), "ses_middle,ses_last,ses_first");
        // If that first read consumed the whole budget, the late session gets first chance next tick.
        let mut page = sessions();
        rotate_sessions_after(&mut page, Some("ses_middle"));
        assert_eq!(ids(&page), "ses_last,ses_first,ses_middle");
        let mut page = sessions();
        rotate_sessions_after(&mut page, Some("ses_last"));
        assert_eq!(ids(&page), "ses_first,ses_middle,ses_last");
    }

    /// A session that changes directory is the only sign of the move, and it can only be seen by
    /// reading every location at once: with `?directory=` set, the session is simply not in the
    /// list its old worktree asks for.
    #[test]
    fn relocations_report_a_session_that_changed_worktree_and_nothing_else() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let first_path = first.path().to_string_lossy().into_owned();
        let second_path = second.path().to_string_lossy().into_owned();
        let elsewhere_path = elsewhere.path().to_string_lossy().into_owned();
        let checkouts = vec![
            ("first".to_string(), first.path().to_path_buf()),
            ("second".to_string(), second.path().to_path_buf()),
        ];
        let located = |session_id: &str, directory: &str| {
            serde_json::json!({
                "id": session_id,
                "location": {"directory": directory}
            })
        };
        let (port, server) = mock_json_responses(vec![
            // Opens the bridge the relocation reads are then asked of. A session can only have
            // moved out of a checkout the panel is already talking to, and a read that connected
            // to a worktree nobody has a terminal in would leave an event reader running.
            serde_json::json!({"data": [{"id": "build", "name": "Build", "mode": "primary"}]}),
            // The first read only records where each session is. A session that has always been
            // in a worktree has not moved, and saying it has would move a terminal on launch.
            serde_json::json!({"data": [
                located("ses_stayed", &first_path),
                located("ses_moved", &first_path),
                located("ses_foreign", &elsewhere_path),
            ]}),
            serde_json::json!({"data": [
                located("ses_stayed", &first_path),
                located("ses_moved", &second_path),
                located("ses_foreign", &elsewhere_path),
            ]}),
            // A session that appears for the first time is not a relocation either.
            serde_json::json!({"data": [
                located("ses_stayed", &first_path),
                located("ses_moved", &second_path),
                located("ses_late", &first_path),
            ]}),
            serde_json::json!({"data": [
                located("ses_stayed", &first_path),
                located("ses_moved", &second_path),
                located("ses_late", &first_path),
            ]}),
        ]);
        let agents = AgentService::with_test_server(port);

        // Nothing is connected yet, so there is nothing to ask and nothing is opened to ask it.
        assert!(agents.relocations(&checkouts).unwrap().is_empty());
        agents.agents("first", first.path()).unwrap();
        assert!(agents.relocations(&checkouts).unwrap().is_empty());
        let moved = agents.relocations(&checkouts).unwrap();
        assert_relocation(&moved, "ses_moved", "first", "second");
        // The next read brings a session that has just appeared, which is not a move, and the move
        // from before is offered again. Handing one over is the only thing this side can do about
        // delivery, and repeating it costs nothing: the terminal is looked up in the worktree it
        // left, and a move already applied leaves nothing there to move.
        assert_eq!(agents.relocations(&checkouts).unwrap(), moved);
        assert_eq!(agents.relocations(&checkouts).unwrap(), moved);
        // A workspace with no checkout has nothing to match a session against, so it never asks.
        assert!(agents.relocations(&[]).unwrap().is_empty());

        let requests = server.join().expect("mock server should finish");
        assert_eq!(requests.len(), 5);
        for request in &requests[1..] {
            // `limit` is asked for even though 2.0.23 answers with fifty whatever it says: it costs nothing
            // and a server that honours it would widen the window.
            assert_eq!(
                request.0,
                format!("GET /api/session?parentID=null&limit={SESSION_LIST_LIMIT} HTTP/1.1")
            );
            // Unscoped on purpose: naming a directory here is what hides the move. Auth is not.
            assert!(!request
                .3
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(DIRECTORY_HEADER)));
            assert_eq!(request.1, crate::services::opencode::authorization("test"));
        }
    }

    #[test]
    fn terminal_moves_serialize_with_removal_of_either_checkout() {
        for removed_checkout in ["checkout:a", "checkout:b"] {
            let agents = Arc::new(AgentService::without_service());
            let (held_tx, held_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let remover = Arc::clone(&agents);
            let removal_thread = std::thread::spawn(move || {
                remover.with_checkout_operation(removed_checkout, || {
                    let _reservation = remover.reserve_worktree_removal(removed_checkout).unwrap();
                    held_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                });
            });
            held_rx.recv_timeout(Duration::from_secs(1)).unwrap();

            let (attempt_tx, attempt_rx) = mpsc::channel();
            let (moved_tx, moved_rx) = mpsc::channel();
            let mover = Arc::clone(&agents);
            let move_thread = std::thread::spawn(move || {
                attempt_tx.send(()).unwrap();
                mover.with_checkout_operations(&["checkout:a", "checkout:b"], || {
                    moved_tx.send(()).unwrap();
                });
            });
            attempt_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            let serialized = moved_rx.recv_timeout(Duration::from_millis(50)).is_err();
            release_tx.send(()).unwrap();
            moved_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            removal_thread.join().unwrap();
            move_thread.join().unwrap();
            assert!(
                serialized,
                "move crossed the removal reservation for {removed_checkout}"
            );
        }
    }

    /// The commonest move there is: the agent walks into a worktree Muster has not registered yet,
    /// and the worktree only appears on the panel afterwards. Forgetting where the session came
    /// from when it was somewhere this panel had no checkout for would leave the row behind for
    /// good, which is the one outcome the whole feature exists to prevent.
    #[test]
    fn relocations_survive_a_worktree_that_is_registered_after_the_session_arrives() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let first_path = first.path().to_string_lossy().into_owned();
        let second_path = second.path().to_string_lossy().into_owned();
        let located = |session_id: &str, directory: &str| serde_json::json!({"id": session_id, "location": {"directory": directory}});
        let (port, server) = mock_json_responses(vec![
            serde_json::json!({"data": [{"id": "build", "name": "Build", "mode": "primary"}]}),
            serde_json::json!({"data": [located("ses_walk", &first_path)]}),
            // The session is already in the new worktree, and this panel has no checkout for it.
            serde_json::json!({"data": [located("ses_walk", &second_path)]}),
            serde_json::json!({"data": [located("ses_walk", &second_path)]}),
        ]);
        let agents = AgentService::with_test_server(port);

        // Only the first worktree is on the panel to begin with.
        let registered = vec![("first".to_string(), first.path().to_path_buf())];
        agents.agents("first", first.path()).unwrap();
        assert!(agents.relocations(&registered).unwrap().is_empty());
        // The session walks into a directory this panel cannot name. Nothing is reported, because
        // there is no destination to report, and nothing is forgotten.
        assert!(agents.relocations(&registered).unwrap().is_empty());

        // The worktree is registered afterwards, and the move the panel could not name while it
        // happened is reported now.
        let with_both = vec![
            ("first".to_string(), first.path().to_path_buf()),
            ("second".to_string(), second.path().to_path_buf()),
        ];
        assert_relocation(
            &agents.relocations(&with_both).unwrap(),
            "ses_walk",
            "first",
            "second",
        );

        server.join().expect("mock server should finish");
    }

    /// The service answers with the newest fifty whatever `limit` says, so a session that goes
    /// quiet falls out of the read. That must not read as a session that has just appeared: the
    /// worktree it was in is kept until the entry is old enough to say nothing at all.
    #[test]
    fn a_session_that_falls_out_of_the_read_keeps_the_worktree_it_was_in() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let checkouts = vec![
            ("first".to_string(), first.path().to_path_buf()),
            ("second".to_string(), second.path().to_path_buf()),
        ];
        let located = |directory: &Path| vec![("ses_quiet".to_string(), directory.to_path_buf())];
        let agents = AgentService::with_test_server(0);
        let start = Instant::now();

        assert!(agents
            .record_relocations(&checkouts, located(first.path()), start)
            .unwrap()
            .is_empty());
        // Out of the page: the read does not mention it at all, and nothing is reported.
        assert!(agents
            .record_relocations(&checkouts, Vec::new(), start + Duration::from_secs(2))
            .unwrap()
            .is_empty());
        // It comes back in the worktree it moved to. That is still a move, not a new session.
        assert_relocation(
            &agents
                .record_relocations(
                    &checkouts,
                    located(second.path()),
                    start + Duration::from_secs(4),
                )
                .unwrap(),
            "ses_quiet",
            "first",
            "second",
        );
    }

    /// A move is handed over again because this side cannot know whether the caller kept it, and it
    /// stops being handed over once nobody has taken it: a terminal that stayed behind for the
    /// whole window is not going to move now, and asking again forever would be asking to move a
    /// terminal that is no longer there.
    #[test]
    fn a_reported_move_is_offered_again_until_nobody_takes_it() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let checkouts = vec![
            ("first".to_string(), first.path().to_path_buf()),
            ("second".to_string(), second.path().to_path_buf()),
        ];
        let located = |directory: &Path| vec![("ses_walk".to_string(), directory.to_path_buf())];
        let agents = AgentService::with_test_server(0);
        let start = Instant::now();
        assert!(agents
            .record_relocations(&checkouts, located(first.path()), start)
            .unwrap()
            .is_empty());
        let moved = agents
            .record_relocations(&checkouts, located(second.path()), start)
            .unwrap();
        assert_relocation(&moved, "ses_walk", "first", "second");
        // Read again well inside the window: the move is offered once more, because the read that
        // got it is not known to have kept it.
        assert_eq!(
            agents
                .record_relocations(
                    &checkouts,
                    located(second.path()),
                    start + PENDING_MOVES_TTL / 2
                )
                .unwrap(),
            moved
        );
        // Past the window nobody has taken it, and a move this old says nothing about now.
        assert!(agents
            .record_relocations(
                &checkouts,
                located(second.path()),
                start + PENDING_MOVES_TTL * 2
            )
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_baseline_discards_disabled_moves_and_seeds_the_effective_location() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let third = tempfile::tempdir().unwrap();
        let fourth = tempfile::tempdir().unwrap();
        let checkouts = vec![
            ("first".to_string(), first.path().to_path_buf()),
            ("second".to_string(), second.path().to_path_buf()),
            ("third".to_string(), third.path().to_path_buf()),
            ("fourth".to_string(), fourth.path().to_path_buf()),
        ];
        let located = |directory: &Path| vec![("ses_walk".to_string(), directory.to_path_buf())];
        let agents = AgentService::with_test_server(0);
        let start = Instant::now();

        assert!(agents
            .record_relocations(&checkouts, located(first.path()), start)
            .unwrap()
            .is_empty());
        assert_relocation(
            &agents
                .record_relocations(&checkouts, located(second.path()), start)
                .unwrap(),
            "ses_walk",
            "first",
            "second",
        );
        // A tool target discovered after follow resumes is the baseline, not a move from stale state.
        assert!(agents
            .record_relocations_with_baseline(
                &checkouts,
                vec![("ses_walk".to_string(), Some(third.path().to_path_buf()))],
                start + Duration::from_secs(1),
                true,
            )
            .unwrap()
            .is_empty());
        assert!(agents
            .record_relocations(
                &checkouts,
                located(third.path()),
                start + Duration::from_secs(2)
            )
            .unwrap()
            .is_empty());
        assert_relocation(
            &agents
                .record_relocations(
                    &checkouts,
                    located(fourth.path()),
                    start + Duration::from_secs(3),
                )
                .unwrap(),
            "ses_walk",
            "third",
            "fourth",
        );

        // An unresolved tool target must not retain the old effective checkout and replay it later.
        assert!(agents
            .record_relocations_with_baseline(
                &checkouts,
                vec![("ses_walk".to_string(), None)],
                start + Duration::from_secs(4),
                true,
            )
            .unwrap()
            .is_empty());
        assert!(agents
            .record_relocations(
                &checkouts,
                located(first.path()),
                start + Duration::from_secs(5)
            )
            .unwrap()
            .is_empty());
    }

    /// Two hops while the first is still on offer. The answer carries the hop the session last
    /// took and not the one it superseded, because an answer with both asks one terminal to
    /// travel twice: the first hop ends where the second one starts, so whichever order they are
    /// applied in, the second is refused or lands on a worktree the terminal has already left.
    #[test]
    fn a_session_that_moves_twice_is_reported_once_as_the_hop_it_last_took() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let third = tempfile::tempdir().unwrap();
        let checkouts = vec![
            ("first".to_string(), first.path().to_path_buf()),
            ("second".to_string(), second.path().to_path_buf()),
            ("third".to_string(), third.path().to_path_buf()),
        ];
        let located = |directory: &Path| vec![("ses_walk".to_string(), directory.to_path_buf())];
        let agents = AgentService::with_test_server(0);
        let start = Instant::now();

        assert!(agents
            .record_relocations(&checkouts, located(first.path()), start)
            .unwrap()
            .is_empty());
        let moved = agents
            .record_relocations(&checkouts, located(second.path()), start)
            .unwrap();
        assert_relocation(&moved, "ses_walk", "first", "second");
        // The second hop arrives before anybody took the first, which is what a caller that threw
        // the answer away looks like from here.
        let latest = agents
            .record_relocations(
                &checkouts,
                located(third.path()),
                start + Duration::from_secs(1),
            )
            .unwrap();
        assert_relocation(&latest, "ses_walk", "second", "third");
        // Still one answer for one session: the hop before it is not handed over again beside it.
        assert_eq!(
            agents
                .record_relocations(
                    &checkouts,
                    located(third.path()),
                    start + Duration::from_secs(2)
                )
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn old_bridge_events_are_dropped_after_checkout_reopens() {
        let state = Arc::new(Mutex::new(RemovalState::default()));
        let delivered = Arc::new(AtomicUsize::new(0));
        let forward = Arc::clone(&delivered);
        let sink: EventSink = Arc::new(move |_| {
            forward.fetch_add(1, Ordering::SeqCst);
        });
        let old =
            generation_scoped_sink(Arc::clone(&state), "checkout".into(), 0, Arc::clone(&sink));
        let event = || AgentEvent {
            checkout_id: "checkout".into(),
            session_id: None,
            kind: crate::domain::agent::AgentEventKind::Unknown,
            raw_type: "test".into(),
            data: serde_json::json!({}),
        };

        old(event());
        assert_eq!(delivered.load(Ordering::SeqCst), 1);
        state
            .lock()
            .unwrap()
            .generations
            .insert("checkout".into(), 1);
        old(event());
        assert_eq!(delivered.load(Ordering::SeqCst), 1);
        generation_scoped_sink(state, "checkout".into(), 1, sink)(event());
        assert_eq!(delivered.load(Ordering::SeqCst), 2);
    }

    fn required_live_directory(name: &str) -> PathBuf {
        let directory = std::env::var(name)
            .ok()
            .filter(|directory| !directory.is_empty())
            .unwrap_or_else(|| {
                panic!(
                    "{name} is required for ignored live agent tests; set it to a dedicated temporary directory"
                )
            });
        PathBuf::from(directory)
    }

    /// A loopback service address. Port 1 is never one anything answers on, which is how the tests
    /// that must not touch a service name an address that cannot reach anything.
    fn test_endpoint(port: u16) -> ServiceEndpoint {
        ServiceEndpoint {
            url: format!("http://127.0.0.1:{port}"),
            port,
            password: "test".into(),
        }
    }

    /// What `fake_bridge` holds, and what discovery answers with in the lifecycle tests.
    fn fake_endpoint() -> ServiceEndpoint {
        test_endpoint(1)
    }

    fn fake_bridge(checkout_id: &str) -> Arc<AgentBridge> {
        // These tests cover the service lifecycle, not any connection to a service.
        Arc::new(AgentBridge {
            checkout_id: checkout_id.to_string(),
            credentials: ServerCredentials {
                endpoint: fake_endpoint(),
                directory: PathBuf::new(),
            },
            client: ureq::Agent::new_with_defaults(),
            event_socket: Mutex::new(None),
            reader: Mutex::new(None),
            stopped: std::sync::atomic::AtomicBool::new(false),
        })
    }

    struct ReaderExitSignal(Option<mpsc::Sender<()>>);

    impl Drop for ReaderExitSignal {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    fn test_service(
        launcher: impl Fn(&str, &Path) -> Result<Arc<AgentBridge>, BridgeError> + Send + Sync + 'static,
        readers: Arc<AtomicUsize>,
        stops: Arc<AtomicUsize>,
    ) -> AgentService {
        AgentService::with_lifecycle(
            Arc::new(launcher),
            Arc::new(move |_, _| {
                readers.fetch_add(1, Ordering::SeqCst);
            }),
            Arc::new(move |_, _| {
                stops.fetch_add(1, Ordering::SeqCst);
            }),
            // The lifecycle tests are about starting, waiting and stopping, not about finding a
            // service again: discovery finds the address `fake_bridge` already holds, so no request
            // in them can retire a bridge.
            Arc::new(|| Ok(fake_endpoint())),
            Duration::from_millis(400),
        )
    }

    #[test]
    fn concurrent_requests_share_one_start_and_reader() {
        const CALLERS: usize = 20;
        let starts = Arc::new(AtomicUsize::new(0));
        let readers = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let (results_tx, results_rx) = mpsc::channel();
        let launch_starts = Arc::clone(&starts);
        let service = Arc::new(test_service(
            move |checkout_id, _| {
                launch_starts.fetch_add(1, Ordering::SeqCst);
                started_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
                Ok(fake_bridge(checkout_id))
            },
            Arc::clone(&readers),
            Arc::clone(&stops),
        ));
        service.set_event_sink(Arc::new(|_| {}));
        let barrier = Arc::new(std::sync::Barrier::new(CALLERS + 1));
        let callers = (0..CALLERS)
            .map(|_| {
                let service = Arc::clone(&service);
                let barrier = Arc::clone(&barrier);
                let results_tx = results_tx.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let _ = results_tx.send(service.bridge("checkout", Path::new(".")));
                })
            })
            .collect::<Vec<_>>();
        drop(results_tx);

        barrier.wait();
        let launch_started = started_rx.recv_timeout(Duration::from_secs(2));
        // Release every possible launcher so a duplicate-start regression fails assertions
        // instead of stranding a worker on the injected gate.
        for _ in 0..CALLERS {
            release_tx.send(()).unwrap();
        }
        let bridges = (0..CALLERS)
            .map(|_| {
                results_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("all callers should complete")
                    .expect("bridge start should succeed")
            })
            .collect::<Vec<_>>();
        drop(callers);

        assert!(launch_started.is_ok(), "no caller launched the checkout");
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(readers.load(Ordering::SeqCst), 1);
        assert!(bridges
            .iter()
            .all(|bridge| Arc::ptr_eq(bridge, &bridges[0])));
        service.stop("checkout");
        assert_eq!(stops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_waiter_timeout_does_not_cancel_a_slower_shared_start() {
        let starts = Arc::new(AtomicUsize::new(0));
        let readers = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let starts_for_launcher = Arc::clone(&starts);
        let release_for_launcher = Arc::clone(&release_rx);
        let service = test_service(
            move |checkout_id, _| {
                starts_for_launcher.fetch_add(1, Ordering::SeqCst);
                started_tx.send(()).unwrap();
                release_for_launcher.lock().unwrap().recv().unwrap();
                Ok(fake_bridge(checkout_id))
            },
            Arc::clone(&readers),
            Arc::clone(&stops),
        );
        let mut service = service;
        service.startup_wait = Duration::from_millis(50);
        let service = Arc::new(service);
        service.set_event_sink(Arc::new(|_| {}));

        let starter_service = Arc::clone(&service);
        let (starter_result_tx, starter_result_rx) = mpsc::channel();
        let starter = std::thread::spawn(move || {
            let _ = starter_result_tx.send(starter_service.bridge("checkout", Path::new(".")));
        });
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("the slow launcher should start");

        let waiter_service = Arc::clone(&service);
        let (waiter_result_tx, waiter_result_rx) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let _ = waiter_result_tx.send(waiter_service.bridge("checkout", Path::new(".")));
        });
        assert!(waiter_result_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("the waiter should finish at its deadline")
            .is_err());

        for _ in 0..2 {
            release_tx.send(()).unwrap();
        }
        assert!(starter_result_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("the original starter should finish")
            .is_ok());
        starter.join().unwrap();
        waiter.join().unwrap();
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(readers.load(Ordering::SeqCst), 1);
        service.stop("checkout");
        assert_eq!(stops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn different_checkouts_start_in_parallel() {
        let starts = Arc::new(AtomicUsize::new(0));
        let readers = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_a_tx, release_a_rx) = mpsc::channel();
        let (release_b_tx, release_b_rx) = mpsc::channel();
        let release_a_rx = Arc::new(Mutex::new(release_a_rx));
        let release_b_rx = Arc::new(Mutex::new(release_b_rx));
        let (results_tx, results_rx) = mpsc::channel();
        let launch_starts = Arc::clone(&starts);
        let service = Arc::new(test_service(
            move |checkout_id, _| {
                launch_starts.fetch_add(1, Ordering::SeqCst);
                started_tx.send(checkout_id.to_string()).unwrap();
                match checkout_id {
                    "a" => release_a_rx.lock().unwrap().recv().unwrap(),
                    "b" => release_b_rx.lock().unwrap().recv().unwrap(),
                    _ => unreachable!(),
                };
                Ok(fake_bridge(checkout_id))
            },
            Arc::clone(&readers),
            Arc::clone(&stops),
        ));
        service.set_event_sink(Arc::new(|_| {}));
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let a_service = Arc::clone(&service);
        let a_barrier = Arc::clone(&barrier);
        let a_results = results_tx.clone();
        let a = std::thread::spawn(move || {
            a_barrier.wait();
            let _ = a_results.send(a_service.bridge("a", Path::new(".")));
        });
        let b_service = Arc::clone(&service);
        let b_barrier = Arc::clone(&barrier);
        let b_results = results_tx.clone();
        let b = std::thread::spawn(move || {
            b_barrier.wait();
            let _ = b_results.send(b_service.bridge("b", Path::new(".")));
        });
        drop(results_tx);
        barrier.wait();

        let first = started_rx.recv_timeout(Duration::from_secs(5));
        let second = started_rx.recv_timeout(Duration::from_secs(5));
        release_a_tx.send(()).unwrap();
        release_b_tx.send(()).unwrap();
        let bridge_one = results_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first checkout should complete")
            .expect("first bridge should start");
        let bridge_two = results_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("second checkout should complete")
            .expect("second bridge should start");
        drop((a, b));

        assert!(
            first.is_ok() && second.is_ok(),
            "checkouts did not boot in parallel"
        );
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert_eq!(readers.load(Ordering::SeqCst), 2);
        let mut started = [
            first.expect("first checkout should reach the launcher"),
            second.expect("second checkout should reach the launcher"),
        ];
        started.sort();
        assert_eq!(started, ["a", "b"]);
        let mut bridge_ids = [
            bridge_one.checkout_id.as_str(),
            bridge_two.checkout_id.as_str(),
        ];
        bridge_ids.sort();
        assert_eq!(bridge_ids, ["a", "b"]);
        service.stop_all();
        assert_eq!(stops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn stopping_during_start_prevents_publication_and_stops_the_child() {
        let readers = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let (start_result_tx, start_result_rx) = mpsc::channel();
        let (stop_result_tx, stop_result_rx) = mpsc::channel();
        let service = Arc::new(test_service(
            move |checkout_id, _| {
                started_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
                Ok(fake_bridge(checkout_id))
            },
            Arc::clone(&readers),
            Arc::clone(&stops),
        ));
        service.set_event_sink(Arc::new(|_| {}));
        let starter_service = Arc::clone(&service);
        let starter = std::thread::spawn(move || {
            let _ = start_result_tx.send(starter_service.bridge("checkout", Path::new(".")));
        });
        let launch_started = started_rx.recv_timeout(Duration::from_secs(2));
        let slot = Arc::clone(
            service
                .bridges
                .lock()
                .unwrap()
                .get("checkout")
                .expect("starting slot is published"),
        );
        let stop_service = Arc::clone(&service);
        let stopper = std::thread::spawn(move || {
            stop_service.stop("checkout");
            let _ = stop_result_tx.send(());
        });

        let mut state = slot.state.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !matches!(
            &*state,
            BridgeState::Starting {
                stop_requested: true
            }
        ) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let (next, timed_out) = slot.changed.wait_timeout(state, remaining).unwrap();
            state = next;
            if timed_out.timed_out() {
                break;
            }
        }
        let stop_requested = matches!(
            &*state,
            BridgeState::Starting {
                stop_requested: true
            }
        );
        drop(state);
        release_tx.send(()).unwrap();
        assert!(launch_started.is_ok(), "startup did not reach the launcher");
        stop_result_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("stop should finish after startup is released");
        assert!(start_result_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("startup should complete")
            .is_err());
        drop((starter, stopper));
        assert!(stop_requested, "stop did not cancel the starting slot");
        assert!(service.bridges.lock().unwrap().get("checkout").is_none());
        assert_eq!(readers.load(Ordering::SeqCst), 0);
        assert_eq!(stops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn stop_all_cancels_an_in_flight_start_before_returning() {
        let starts = Arc::new(AtomicUsize::new(0));
        let readers = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        let (launch_tx, launch_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let (start_result_tx, start_result_rx) = mpsc::channel();
        let (stop_result_tx, stop_result_rx) = mpsc::channel();
        let launch_starts = Arc::clone(&starts);
        let service = Arc::new(test_service(
            move |checkout_id, _| {
                launch_starts.fetch_add(1, Ordering::SeqCst);
                launch_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
                Ok(fake_bridge(checkout_id))
            },
            Arc::clone(&readers),
            Arc::clone(&stops),
        ));
        service.set_event_sink(Arc::new(|_| {}));
        let starter_service = Arc::clone(&service);
        let starter = std::thread::spawn(move || {
            let _ = start_result_tx.send(starter_service.bridge("checkout", Path::new(".")));
        });
        let launch_started = launch_rx.recv_timeout(Duration::from_secs(2));
        let slot = Arc::clone(
            service
                .bridges
                .lock()
                .unwrap()
                .get("checkout")
                .expect("starting slot is published"),
        );
        let stop_service = Arc::clone(&service);
        let stopper = std::thread::spawn(move || {
            stop_service.stop_all();
            let _ = stop_result_tx.send(());
        });

        let mut state = slot.state.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !matches!(
            &*state,
            BridgeState::Starting {
                stop_requested: true
            }
        ) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let (next, timed_out) = slot.changed.wait_timeout(state, remaining).unwrap();
            state = next;
            if timed_out.timed_out() {
                break;
            }
        }
        let stop_requested = matches!(
            &*state,
            BridgeState::Starting {
                stop_requested: true
            }
        );
        drop(state);
        release_tx.send(()).unwrap();
        let stop_finished = stop_result_rx.recv_timeout(Duration::from_secs(2));
        let start_result = start_result_rx.recv_timeout(Duration::from_secs(2));
        drop((starter, stopper));

        assert!(launch_started.is_ok(), "startup did not reach the launcher");
        assert!(stop_requested, "stop_all did not cancel the starting slot");
        assert!(stop_finished.is_ok(), "stop_all did not finish");
        assert!(start_result.expect("startup should complete").is_err());
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(readers.load(Ordering::SeqCst), 0);
        assert_eq!(stops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn concurrent_stops_stop_a_checkout_once() {
        let starts = Arc::new(AtomicUsize::new(0));
        let readers = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        let launch_starts = Arc::clone(&starts);
        let service = Arc::new(test_service(
            move |checkout_id, _| {
                launch_starts.fetch_add(1, Ordering::SeqCst);
                Ok(fake_bridge(checkout_id))
            },
            Arc::clone(&readers),
            Arc::clone(&stops),
        ));
        service.bridge("checkout", Path::new(".")).unwrap();

        let barrier = Arc::new(std::sync::Barrier::new(3));
        let (results_tx, results_rx) = mpsc::channel();
        let callers = (0..2)
            .map(|_| {
                let service = Arc::clone(&service);
                let barrier = Arc::clone(&barrier);
                let results_tx = results_tx.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    service.stop("checkout");
                    let _ = results_tx.send(());
                })
            })
            .collect::<Vec<_>>();
        drop(results_tx);
        barrier.wait();
        let first = results_rx.recv_timeout(Duration::from_secs(2));
        let second = results_rx.recv_timeout(Duration::from_secs(2));
        drop(callers);

        assert!(
            first.is_ok() && second.is_ok(),
            "both stop calls should return"
        );
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(stops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_startup_stop_wait_is_bounded() {
        let slot = Arc::new(super::BridgeSlot::starting());
        let stopper: Arc<BridgeStopper> = Arc::new(|_, _| {});
        let (done_tx, done_rx) = mpsc::channel();
        let stopping_slot = Arc::clone(&slot);
        let stopping_callback = Arc::clone(&stopper);
        let stop = std::thread::spawn(move || {
            stopping_slot.stop(stopping_callback.as_ref(), Duration::from_millis(20));
            let _ = done_tx.send(());
        });

        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("stop should time out instead of waiting forever");
        drop(stop);
        assert!(matches!(*slot.state.lock().unwrap(), BridgeState::Stopped));
    }

    fn error_message(error: &BridgeError) -> &str {
        match error {
            BridgeError::Unavailable(message)
            | BridgeError::Stale(message)
            | BridgeError::Foreign(message)
            | BridgeError::Failed(message) => message,
        }
    }

    #[test]
    fn waiting_for_a_start_is_bounded() {
        let slot = super::BridgeSlot::starting();
        let started = Instant::now();
        let error = match slot.wait_with_timeout(Duration::from_millis(20)) {
            Ok(_) => panic!("a starting slot without a publisher must time out"),
            Err(error) => error,
        };

        assert!(error_message(&error).contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(matches!(
            *slot.state.lock().unwrap(),
            BridgeState::Starting {
                stop_requested: false
            }
        ));
    }

    #[test]
    fn a_reader_join_does_not_outlive_its_budget() {
        let (release_tx, release_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let _ = release_rx.recv();
            let _ = finished_tx.send(());
        });
        let started = Instant::now();
        join_reader(reader, Instant::now() + Duration::from_millis(20));

        assert!(started.elapsed() < Duration::from_millis(200));
        release_tx.send(()).unwrap();
        finished_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("the detached test reader should eventually exit");
    }

    #[test]
    fn failure_cleanup_preserves_a_replacement_slot() {
        let failed = Arc::new(super::BridgeSlot::starting());
        *failed.state.lock().unwrap() = BridgeState::Failed(BridgeError::Failed("failed".into()));
        let mut bridges = HashMap::new();
        bridges.insert("checkout".to_string(), Arc::clone(&failed));

        // stop removes failed S1; a later caller inserts S2 before S1's cleanup reaches map.
        let removed = bridges.remove("checkout").unwrap();
        let stopper: Arc<BridgeStopper> = Arc::new(|_, _| {});
        removed.stop(stopper.as_ref(), Duration::from_millis(20));
        let replacement = Arc::new(super::BridgeSlot::starting());
        bridges.insert("checkout".to_string(), Arc::clone(&replacement));
        remove_slot_if_current(&mut bridges, "checkout", &failed);

        assert!(Arc::ptr_eq(bridges.get("checkout").unwrap(), &replacement));
    }

    #[test]
    fn failed_start_can_be_retried() {
        let starts = Arc::new(AtomicUsize::new(0));
        let readers = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        let launch_starts = Arc::clone(&starts);
        let service = test_service(
            move |checkout_id, _| {
                if launch_starts.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err(BridgeError::Unavailable("injected failure".into()))
                } else {
                    Ok(fake_bridge(checkout_id))
                }
            },
            Arc::clone(&readers),
            Arc::clone(&stops),
        );
        service.set_event_sink(Arc::new(|_| {}));

        assert!(service.bridge("checkout", Path::new(".")).is_err());
        assert!(service.bridge("checkout", Path::new(".")).is_ok());
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert_eq!(readers.load(Ordering::SeqCst), 1);
        service.stop("checkout");
        assert_eq!(stops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_panicking_launcher_releases_the_starting_slot() {
        let starts = Arc::new(AtomicUsize::new(0));
        let readers = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        let launch_starts = Arc::clone(&starts);
        let service = test_service(
            move |checkout_id, _| {
                if launch_starts.fetch_add(1, Ordering::SeqCst) == 0 {
                    panic!("injected launcher panic");
                }
                Ok(fake_bridge(checkout_id))
            },
            Arc::clone(&readers),
            Arc::clone(&stops),
        );

        assert!(service.bridge("checkout", Path::new(".")).is_err());
        assert!(service.bridge("checkout", Path::new(".")).is_ok());
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        service.stop("checkout");
        assert_eq!(stops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn stop_all_stops_each_child_once() {
        let starts = Arc::new(AtomicUsize::new(0));
        let readers = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        let launch_starts = Arc::clone(&starts);
        let service = test_service(
            move |checkout_id, _| {
                launch_starts.fetch_add(1, Ordering::SeqCst);
                Ok(fake_bridge(checkout_id))
            },
            Arc::clone(&readers),
            Arc::clone(&stops),
        );
        service.set_event_sink(Arc::new(|_| {}));
        service.bridge("a", Path::new(".")).unwrap();
        service.bridge("b", Path::new(".")).unwrap();

        service.stop_all();
        service.stop_all();
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert_eq!(readers.load(Ordering::SeqCst), 2);
        assert_eq!(stops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn stop_all_signals_every_child_before_waiting_for_cleanup() {
        let starts = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        let bridges = Arc::new(Mutex::new(Vec::<Arc<AgentBridge>>::new()));
        let (cleanup_started_tx, cleanup_started_rx) = mpsc::channel();
        let (release_cleanup_tx, release_cleanup_rx) = mpsc::channel();
        let release_cleanup_rx = Arc::new(Mutex::new(release_cleanup_rx));
        let launch_starts = Arc::clone(&starts);
        let launch_bridges = Arc::clone(&bridges);
        let cleanup_bridges = Arc::clone(&bridges);
        let cleanup_stops = Arc::clone(&stops);
        let service = Arc::new(AgentService::with_lifecycle(
            Arc::new(move |checkout_id, _| {
                launch_starts.fetch_add(1, Ordering::SeqCst);
                let bridge = fake_bridge(checkout_id);
                launch_bridges.lock().unwrap().push(Arc::clone(&bridge));
                Ok(bridge)
            }),
            Arc::new(|_, _| {}),
            Arc::new(move |_, _| {
                if cleanup_stops.fetch_add(1, Ordering::SeqCst) == 0 {
                    let signaled = cleanup_bridges
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|bridge| bridge.stopped.load(Ordering::SeqCst))
                        .count();
                    cleanup_started_tx.send(signaled).unwrap();
                    release_cleanup_rx.lock().unwrap().recv().unwrap();
                }
            }),
            Arc::new(|| Ok(fake_endpoint())),
            Duration::from_millis(400),
        ));
        service.bridge("a", Path::new(".")).unwrap();
        service.bridge("b", Path::new(".")).unwrap();

        let stopping_service = Arc::clone(&service);
        let (stopped_tx, stopped_rx) = mpsc::channel();
        let stop_all = std::thread::spawn(move || {
            stopping_service.stop_all();
            let _ = stopped_tx.send(());
        });
        let signaled = cleanup_started_rx.recv_timeout(Duration::from_secs(2));
        release_cleanup_tx.send(()).unwrap();
        let stopped = stopped_rx.recv_timeout(Duration::from_secs(2));
        drop(stop_all);

        assert_eq!(signaled.expect("cleanup should start"), 2);
        assert!(
            stopped.is_ok(),
            "stop_all should finish after cleanup is released"
        );
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert_eq!(stops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn stop_all_passes_the_remaining_shared_budget_to_each_slot() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&received);
        let service = AgentService::with_lifecycle_and_slot_stopper(
            Arc::new(|_, _| panic!("stop_all should not launch bridges")),
            Arc::new(|_, _| {}),
            Arc::new(|_, _| {}),
            Arc::new(|| Ok(fake_endpoint())),
            Duration::from_millis(400),
            Arc::new(move |slot, timeout| {
                let call = {
                    let mut received = observed.lock().unwrap();
                    received.push(timeout);
                    received.len()
                };
                *slot.state.lock().unwrap() = BridgeState::Stopped;
                slot.changed.notify_all();
                if call == 1 {
                    // Exhaust the shared budget deterministically before the second slot.
                    std::thread::sleep(Duration::from_millis(500));
                }
            }),
        );
        let first = Arc::new(super::BridgeSlot::starting());
        let second = Arc::new(super::BridgeSlot::starting());
        {
            let mut slots = service.bridges.lock().unwrap();
            slots.insert("a".into(), Arc::clone(&first));
            slots.insert("b".into(), Arc::clone(&second));
        }

        service.stop_all();
        let received = received.lock().unwrap();
        assert_eq!(received.len(), 2);
        assert_eq!(received[1], Duration::ZERO);
    }

    #[test]
    fn stop_all_cancels_multiple_starts_within_smoke_limit() {
        let starts = Arc::new(AtomicUsize::new(0));
        let readers = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_a_tx, release_a_rx) = mpsc::channel();
        let (release_b_tx, release_b_rx) = mpsc::channel();
        let release_a_rx = Arc::new(Mutex::new(release_a_rx));
        let release_b_rx = Arc::new(Mutex::new(release_b_rx));
        let launch_starts = Arc::clone(&starts);
        let service = Arc::new(test_service(
            move |checkout_id, _| {
                launch_starts.fetch_add(1, Ordering::SeqCst);
                started_tx.send(checkout_id.to_string()).unwrap();
                match checkout_id {
                    "a" => release_a_rx.lock().unwrap().recv().unwrap(),
                    "b" => release_b_rx.lock().unwrap().recv().unwrap(),
                    _ => unreachable!(),
                };
                Ok(fake_bridge(checkout_id))
            },
            Arc::clone(&readers),
            Arc::clone(&stops),
        ));
        let (result_tx, result_rx) = mpsc::channel();
        let a_service = Arc::clone(&service);
        let a_result = result_tx.clone();
        let a = std::thread::spawn(move || {
            let _ = a_result.send(a_service.bridge("a", Path::new(".")));
        });
        let b_service = Arc::clone(&service);
        let b_result = result_tx.clone();
        let b = std::thread::spawn(move || {
            let _ = b_result.send(b_service.bridge("b", Path::new(".")));
        });
        drop(result_tx);
        let first_started = started_rx.recv_timeout(Duration::from_secs(2));
        let second_started = started_rx.recv_timeout(Duration::from_secs(2));

        let (stopped_tx, stopped_rx) = mpsc::channel();
        let stopping_service = Arc::clone(&service);
        let stop_all = std::thread::spawn(move || {
            stopping_service.stop_all();
            let _ = stopped_tx.send(());
        });
        // Deadline sharing is pinned structurally above; this is only a generous hang check.
        let stopped = stopped_rx.recv_timeout(Duration::from_secs(10));
        release_a_tx.send(()).unwrap();
        release_b_tx.send(()).unwrap();
        let first_result = result_rx.recv_timeout(Duration::from_secs(2));
        let second_result = result_rx.recv_timeout(Duration::from_secs(2));
        drop((a, b, stop_all));

        assert!(first_started.is_ok() && second_started.is_ok());
        assert!(stopped.is_ok(), "stop_all exceeded the smoke limit");
        assert!(first_result.expect("first start should complete").is_err());
        assert!(second_result
            .expect("second start should complete")
            .is_err());
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert_eq!(readers.load(Ordering::SeqCst), 0);
        assert_eq!(stops.load(Ordering::SeqCst), 2);
    }
    #[test]
    fn stopping_interrupts_an_idle_sse_reader() {
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("test listener should bind");
        let port = listener.local_addr().unwrap().port();
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("SSE request should connect");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: keep-alive\r\n\r\n",
                )
                .unwrap();
            accepted_tx.send(()).unwrap();
            let _ = release_rx.recv();
        });
        let bridge = Arc::new(bridge_at("idle-sse", directory.path(), port));
        let (reader_done_tx, reader_done_rx) = mpsc::channel();
        let reader_exit = ReaderExitSignal(Some(reader_done_tx));
        let sink: EventSink = Arc::new(move |_: AgentEvent| {
            let _ = &reader_exit;
        });
        bridge.start_reader(&sink);
        accepted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("the reader should connect to the fixture");

        let budget = Duration::from_millis(200);
        let started = Instant::now();
        bridge.stop_with_timeout(budget);
        assert!(started.elapsed() <= budget + Duration::from_millis(100));
        drop(sink);
        reader_done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("the SSE reader should terminate after stop");
        release_tx.send(()).unwrap();
        server.join().unwrap();
    }

    /// A bridge pointed at a loopback port that is serving an event stream, for the tests that cover
    /// the reader's own behaviour rather than how a service was found.
    fn bridge_at(checkout_id: &str, directory: &Path, port: u16) -> AgentBridge {
        AgentBridge {
            checkout_id: checkout_id.to_string(),
            credentials: ServerCredentials {
                endpoint: ServiceEndpoint {
                    url: format!("http://127.0.0.1:{port}"),
                    port,
                    password: "secret".into(),
                },
                directory: directory.to_path_buf(),
            },
            client: ureq::Agent::new_with_defaults(),
            event_socket: Mutex::new(None),
            reader: Mutex::new(None),
            stopped: std::sync::atomic::AtomicBool::new(false),
        }
    }

    #[test]
    fn truncated_chunked_sse_eof_returns_without_spinning() {
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("test listener should bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("SSE request should connect");
            let mut request = Vec::new();
            let mut buffer = [0; 512];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream
                    .read(&mut buffer)
                    .expect("the request should be readable");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n")
                .unwrap();
            let _ = stream.shutdown(Shutdown::Write);
        });
        let bridge = bridge_at("truncated-sse", directory.path(), port);
        let mut reader = event_stream(&bridge).expect("the truncated stream should connect");
        let started = Instant::now();
        let mut body = Vec::new();
        let result = reader.read_to_end(&mut body);

        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert!(body.is_empty());
        bridge.clear_event_socket();
        server.join().unwrap();
    }

    #[test]
    fn truncated_chunk_cannot_dispatch_a_complete_json_line() {
        let directory = tempfile::tempdir().unwrap();
        let body =
            b"data: {\"type\":\"session.idle\",\"data\":{\"sessionID\":\"ses_truncated\"}}\n";
        let mut response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        response.extend_from_slice(format!("{:x}\r\n", body.len() + 8).as_bytes());
        response.extend_from_slice(body);
        let (bridge, server) = mock_event_response(directory.path(), response);
        let mut reader =
            std::io::BufReader::new(event_stream(&bridge).expect("mock stream should connect"));

        let error = read_sse_frame(&mut reader)
            .expect_err("EOF inside a declared chunk must not dispatch the pending frame");

        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        bridge.clear_event_socket();
        server.join().expect("mock server should finish");
    }

    #[test]
    fn truncated_chunk_at_exact_frame_byte_limit_is_not_accepted_as_eof() {
        let directory = tempfile::tempdir().unwrap();
        let mut body = b"data: {}\n".to_vec();
        body.resize(MAX_SSE_FRAME_BYTES, b':');
        let mut response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        response.extend_from_slice(format!("{:x}\r\n", body.len() + 1).as_bytes());
        response.extend_from_slice(&body);
        let (bridge, server) = mock_event_response(directory.path(), response);
        let mut reader =
            std::io::BufReader::new(event_stream(&bridge).expect("mock stream should connect"));

        let error = read_sse_frame(&mut reader)
            .expect_err("truncated chunk EOF must not finalize an exact-cap frame");

        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        bridge.clear_event_socket();
        server.join().expect("mock server should finish");
    }

    #[test]
    fn truncated_chunk_terminator_cannot_dispatch_a_complete_frame() {
        let directory = tempfile::tempdir().unwrap();
        let body = b"data: {\"type\":\"session.idle\",\"data\":{}}\n\n";
        let mut response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        response.extend_from_slice(format!("{:x}\r\n", body.len()).as_bytes());
        response.extend_from_slice(body);
        let (bridge, server) = mock_event_response(directory.path(), response);
        let mut reader =
            std::io::BufReader::new(event_stream(&bridge).expect("mock stream should connect"));

        let error = read_sse_frame(&mut reader)
            .expect_err("EOF before the chunk CRLF must not dispatch its event");

        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        bridge.clear_event_socket();
        server.join().expect("mock server should finish");
    }

    #[test]
    fn a_truncated_event_stream_is_reported_without_repeating_the_frame() {
        // The reader used to drop the cause of every disconnect, which left a service that died
        // mid-frame indistinguishable from one that had closed cleanly. The frame it did read
        // carries `marker`, so a line that quoted the payload — the rule this file keeps everywhere
        // else — is caught by the second assertion.
        let directory = tempfile::tempdir().unwrap();
        let marker = "SESSION-TEXT-THE-REPORT-MUST-NOT-REPEAT";
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("test listener should bind");
        let port = listener.local_addr().unwrap().port();
        let frame = format!(
            "data: {}\n\n",
            serde_json::json!({
                "type": "session.idle",
                "location": {"directory": directory.path().to_string_lossy()},
                "data": {"sessionID": "ses_one", "text": marker}
            })
        );
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("SSE request should connect");
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .expect("the stream headers should be writable");
            // One complete frame, then a chunk header promising a body that never arrives: the
            // service died between the two writes.
            write!(stream, "{:x}\r\n", frame.len()).expect("the chunk header should be writable");
            stream
                .write_all(frame.as_bytes())
                .expect("the frame should be writable");
            stream
                .write_all(b"\r\n400\r\n")
                .expect("the chunk should be writable");
            let _ = stream.shutdown(Shutdown::Both);
        });
        // The checkout this reader answers for, named so the assertions can ignore every other line the
        // binary's other tests wrote while this one was capturing.
        const CHECKOUT: &str = "truncated-report";
        let bridge = Arc::new(bridge_at(CHECKOUT, directory.path(), port));
        let (delivered, seen) = mpsc::channel();
        let sink: EventSink = Arc::new(move |event: AgentEvent| {
            let _ = delivered.send(event);
        });
        let _capture = capturing_log();
        let reader = {
            let bridge = Arc::clone(&bridge);
            std::thread::spawn(move || read_events(bridge, sink))
        };

        let event = seen
            .recv_timeout(Duration::from_secs(5))
            .expect("the frame the service did send should be dispatched");
        assert!(
            event.data.to_string().contains(marker),
            "the reader should have dispatched the frame it could read"
        );

        // The reader reports on its own schedule — it has to notice the break first — so the test
        // waits for its line rather than assuming the thread got there before the stop below.
        let deadline = Instant::now() + Duration::from_secs(5);
        let logs = loop {
            let logs = captured_logs_so_far();
            if logs.iter().any(|line| line.contains(CHECKOUT)) {
                break logs;
            }
            assert!(
                Instant::now() < deadline,
                "an abrupt end must be reported instead of dropped: {logs:?}"
            );
            sleep(Duration::from_millis(20));
        };
        bridge.stop_with_timeout(Duration::from_secs(2));
        reader.join().expect("the reader thread should finish");
        server.join().expect("mock server should finish");

        let reported = logs
            .iter()
            .filter(|line| line.contains(CHECKOUT))
            .collect::<Vec<_>>();
        assert!(
            reported
                .iter()
                .any(|line| line.contains("ended in the middle of a frame")),
            "an abrupt end must say so instead of dropping the cause: {reported:?}"
        );
        assert!(
            !reported.iter().any(|line| line.contains(marker)),
            "an event payload carries session text and must never reach the log: {reported:?}"
        );
    }

    #[test]
    fn a_service_that_stays_down_is_reported_once_per_wait_not_once_per_attempt() {
        // The reconnect loop runs forever, so a line per attempt is a log a service that is down
        // fills by itself. The backoff is the throttle: one line per tier, and no line at all for
        // the attempts in between.
        let bridge = bridge_at("stuck-service", Path::new("/muster-no-such-directory"), 1);
        let loss = StreamLoss::Refused(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "nothing is listening",
        ));
        let _capture = capturing_log();
        let mut reported = Duration::ZERO;

        for _ in 0..50 {
            report_stream_loss(&bridge, &mut reported, Duration::from_millis(250), &loss);
        }
        assert_eq!(
            take_captured_logs().len(),
            1,
            "fifty attempts at the same wait are one line"
        );

        // A tier the backoff has not reached yet is worth saying, which is how a service that never
        // came back still ends up visible.
        CAPTURED_LOGS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        CAPTURING_LOGS.store(true, Ordering::SeqCst);
        let mut reported = Duration::ZERO;
        report_stream_loss(&bridge, &mut reported, Duration::from_millis(250), &loss);
        report_stream_loss(&bridge, &mut reported, Duration::from_secs(30), &loss);
        let logs = take_captured_logs();
        assert_eq!(logs.len(), 2, "{logs:?}");
        assert!(logs[0].contains("nothing is listening"), "{logs:?}");
        assert!(logs[0].contains("stuck-service"), "{logs:?}");
    }

    #[test]
    fn a_panicking_reader_starter_does_not_poison_the_slot() {
        let starts = Arc::new(AtomicUsize::new(0));
        let readers = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        let launch_starts = Arc::clone(&starts);
        let reader_starts = Arc::clone(&readers);
        let stop_calls = Arc::clone(&stops);
        let service = AgentService::with_lifecycle(
            Arc::new(move |checkout_id, _| {
                launch_starts.fetch_add(1, Ordering::SeqCst);
                Ok(fake_bridge(checkout_id))
            }),
            Arc::new(move |_, _| {
                if reader_starts.fetch_add(1, Ordering::SeqCst) == 0 {
                    panic!("injected reader starter panic");
                }
            }),
            Arc::new(move |_, _| {
                stop_calls.fetch_add(1, Ordering::SeqCst);
            }),
            Arc::new(|| Ok(fake_endpoint())),
            Duration::from_millis(400),
        );
        service.set_event_sink(Arc::new(|_| {}));

        assert!(service.bridge("checkout", Path::new(".")).is_err());
        assert!(service.bridge("checkout", Path::new(".")).is_ok());
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert_eq!(readers.load(Ordering::SeqCst), 2);
        service.stop("checkout");
        assert_eq!(stops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn sec_09_session_ids_from_another_checkout_cannot_be_addressed() {
        // A well-formed id from checkout A is still only meaningful in A's server; the
        // value itself must never be usable as a path fragment.
        for hostile in [
            "ses_../../etc/passwd",
            "ses_a/b",
            "ses_ ",
            "ses_",
            "not-a-session",
            "",
            "ses_a?directory=/other",
            "ses_a#",
        ] {
            assert!(
                validate_session_id(hostile).is_err(),
                "accepted a hostile session id: {hostile:?}"
            );
        }
        assert!(validate_session_id("ses_f21a52baaffeAS3HejslJJxVor").is_ok());
    }

    #[test]
    fn compares_directories_after_resolving_symlinks() {
        let directory = std::env::temp_dir();
        assert!(same_directory(&directory, &directory));
        assert!(!same_directory(
            &directory,
            Path::new("/definitely/not/here")
        ));
    }

    #[test]
    fn keeps_prompt_limits_and_reports_unavailable_agents_distinctly() {
        // A review larger than this is rejected instead of being split: the contract is
        // that one round is one message.
        assert_eq!(MAX_PROMPT_BYTES, 512 * 1024);
        let unavailable = BridgeError::Unavailable("no binary".into())
            .into_ipc(crate::domain::ipc::IpcErrorCode::OperationFailed);
        assert_eq!(
            unavailable.code,
            crate::domain::ipc::IpcErrorCode::AgentUnavailable
        );
        let foreign = BridgeError::Foreign("other checkout".into())
            .into_ipc(crate::domain::ipc::IpcErrorCode::OperationFailed);
        assert_eq!(
            foreign.code,
            crate::domain::ipc::IpcErrorCode::AgentOwnershipMismatch
        );
    }

    /// The service the person running the tests already has open.
    ///
    /// The live tests talk to that one rather than to a service of their own, because this app does
    /// not start one; a person running them is expected to have OpenCode running for them to pass.
    fn live_service() -> AgentService {
        let home = std::env::var_os("HOME").expect("a live test needs a home to find OpenCode in");
        AgentService::new(PathBuf::from(home))
    }

    #[test]
    fn an_empty_catalog_is_a_ready_service_once_the_deadline_passes() {
        let directory = tempfile::tempdir().unwrap();
        // A service that answers for this checkout with an empty catalog, which is what the first
        // moments after someone starts OpenCode look like. It keeps answering, because how many
        // times a readiness loop asks is a function of how fast it answers, not something a test
        // should have to predict; the loop is bounded by the deadline instead.
        let port = serving_empty_catalog(directory.path());
        let credentials = ServerCredentials {
            endpoint: ServiceEndpoint {
                url: format!("http://127.0.0.1:{port}"),
                port,
                password: "secret".into(),
            },
            directory: directory.path().to_path_buf(),
        };

        let started = Instant::now();
        let accepted = ready(credentials.clone(), started + Duration::from_secs(1))
            .expect("a service that answers is ready, whatever the catalog holds");

        assert_eq!(accepted, credentials);
        // The catalog never fills, so the deadline is the only thing that can end the wait. That is
        // the point: the answer must not depend on whether a reply landed just before the deadline
        // or just after it. The lower bound is what proves the loop ran to the end rather than
        // deciding early; the upper bound is slack enough that a loaded machine cannot fail it.
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_millis(800), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    }

    #[test]
    fn a_service_that_never_answers_is_not_ready_when_the_deadline_arrives() {
        let directory = tempfile::tempdir().unwrap();
        // The other half of the same rule: an answer is what makes a service ready, so a service
        // that stays silent is unavailable by the deadline however much of it it was given.
        let port = serving_silence();
        let credentials = ServerCredentials {
            endpoint: ServiceEndpoint {
                url: format!("http://127.0.0.1:{port}"),
                port,
                password: "secret".into(),
            },
            directory: directory.path().to_path_buf(),
        };

        let error = ready(credentials, Instant::now() + Duration::from_secs(1))
            .expect_err("a service that never answered is not a ready service");

        assert_eq!(
            error_message(&error),
            "the OpenCode service did not become ready in time"
        );
    }

    #[test]
    fn reports_rejected_credentials_distinctly() {
        assert!(status_detail(401).contains("rejected Muster's credentials"));
    }

    /// A service whose address a test can move, the way a restart on another port moves it.
    ///
    /// Both the launcher and discovery read the same cell, which is what makes the case real: a
    /// bridge holds the address the cell held when it started, and discovery answers with whatever
    /// the cell holds now. Returns the service with the two counters a test asserts on — how many
    /// bridges were launched and how many times the service was looked up again.
    fn service_behind_a_movable_endpoint(
        endpoint: ServiceEndpoint,
    ) -> (
        AgentService,
        Arc<Mutex<ServiceEndpoint>>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
    ) {
        let address = Arc::new(Mutex::new(endpoint));
        let launched = Arc::new(AtomicUsize::new(0));
        let discovered = Arc::new(AtomicUsize::new(0));
        let launch_address = Arc::clone(&address);
        let launch_counts = Arc::clone(&launched);
        let find_address = Arc::clone(&address);
        let find_counts = Arc::clone(&discovered);
        let service = AgentService::with_lifecycle(
            Arc::new(move |checkout_id, directory| {
                launch_counts.fetch_add(1, Ordering::SeqCst);
                Ok(Arc::new(AgentBridge {
                    checkout_id: checkout_id.to_string(),
                    credentials: ServerCredentials {
                        endpoint: launch_address.lock().unwrap().clone(),
                        directory: directory.to_path_buf(),
                    },
                    client: ureq::Agent::new_with_defaults(),
                    event_socket: Mutex::new(None),
                    reader: Mutex::new(None),
                    stopped: std::sync::atomic::AtomicBool::new(false),
                }))
            }),
            Arc::new(|_, _| {}),
            Arc::new(|_, _| {}),
            Arc::new(move || {
                find_counts.fetch_add(1, Ordering::SeqCst);
                Ok(find_address.lock().unwrap().clone())
            }),
            Duration::from_millis(50),
        );
        (service, address, launched, discovered)
    }

    #[test]
    fn a_service_that_moved_is_found_again_and_the_bridge_replaced_by_a_new_one() {
        let directory = tempfile::tempdir().unwrap();
        let (port, server) = mock_json_responses(vec![serde_json::json!({
            "data": [{"id": "build", "name": "Build", "mode": "primary"}],
        })]);
        let (service, address, launched, discovered) =
            service_behind_a_movable_endpoint(closed_endpoint());
        // The bridge that exists holds the address nothing answers on, which is what a restart
        // leaves behind: the service is running, on another port.
        let stale = service.bridge("checkout", directory.path()).unwrap();
        *address.lock().unwrap() = test_endpoint(port);

        // The read against the stale endpoint reports the failure once: answering it by looking
        // again is what keeps this from being "the agent is not running" forever while it plainly
        // is.
        assert!(service.agents("checkout", directory.path()).is_err());
        assert_eq!(discovered.load(Ordering::SeqCst), 1);

        let catalog = service
            .agents("checkout", directory.path())
            .expect("the next read should reach the service that is running");
        assert_eq!(catalog[0].id, "build");
        assert_eq!(
            launched.load(Ordering::SeqCst),
            2,
            "the bridge was not replaced"
        );
        // The replacement is what later calls get, and the bridge that held the dead endpoint is
        // stopped: its reader would otherwise keep reconnecting to an address nobody serves.
        let current = service.bridge("checkout", directory.path()).unwrap();
        assert_eq!(current.credentials.endpoint, test_endpoint(port));
        assert!(
            stale.is_stopped(),
            "the retired bridge still has a live reader"
        );
        assert_eq!(
            server.join().expect("mock service should finish").len(),
            1,
            "only the read on the live endpoint should have reached the service"
        );
    }

    #[test]
    fn a_replaced_bridge_moves_the_generation_its_events_were_scoped_to() {
        // The retired reader is stopped and the sink it was given stops being current in the same
        // step, so an event already in flight cannot cross into the replacement.
        let directory = tempfile::tempdir().unwrap();
        let (service, address, _launched, _discovered) =
            service_behind_a_movable_endpoint(closed_endpoint());
        assert_eq!(service.checkout_generation("checkout").unwrap(), 0);
        service.bridge("checkout", directory.path()).unwrap();

        *address.lock().unwrap() = test_endpoint(closed_endpoint().port);
        assert!(service.agents("checkout", directory.path()).is_err());

        assert_eq!(
            service.checkout_generation("checkout").unwrap(),
            1,
            "the retired bridge's events would still count as current"
        );
    }

    #[test]
    fn a_service_that_is_only_unreachable_keeps_its_bridge() {
        // Nothing answers right now, but discovery finds the same registration, so the address is
        // not stale: replacing the bridge would trade a bridge that is about to answer for one that
        // has not proved anything.
        let directory = tempfile::tempdir().unwrap();
        let (service, _address, launched, discovered) =
            service_behind_a_movable_endpoint(closed_endpoint());

        assert!(service.agents("checkout", directory.path()).is_err());
        assert!(service.agents("checkout", directory.path()).is_err());

        assert_eq!(discovered.load(Ordering::SeqCst), 2);
        assert_eq!(
            launched.load(Ordering::SeqCst),
            1,
            "the bridge was replaced for nothing"
        );
    }

    #[test]
    fn a_status_the_service_chose_does_not_invalidate_the_bridge() {
        // A rejected request is the service's business, and it says nothing about the address: the
        // bridge is still pointed at the running service, so it must survive the refusal.
        let directory = tempfile::tempdir().unwrap();
        let (port, requests) = serving_status_per_method(
            "200 OK",
            "400 Bad Request",
            serde_json::json!({"data": []}).to_string(),
        );
        let (service, _address, launched, discovered) =
            service_behind_a_movable_endpoint(test_endpoint(port));
        let before = service.bridge("checkout", directory.path()).unwrap();

        let refused = service
            .create_session("checkout", directory.path(), "refused")
            .expect_err("the service refused the creation");
        assert!(error_message(&refused).contains("400"), "{refused:?}");

        assert_eq!(
            discovered.load(Ordering::SeqCst),
            0,
            "discovery ran for a business refusal"
        );
        assert_eq!(launched.load(Ordering::SeqCst), 1);
        let after = service.bridge("checkout", directory.path()).unwrap();
        assert!(
            Arc::ptr_eq(&before, &after),
            "a business error must not cost this app its bridge"
        );
        assert_eq!(
            requests.lock().unwrap().len(),
            1,
            "the refused request is not made again"
        );
    }

    #[test]
    fn a_refused_prompt_is_never_sent_again() {
        // The service that refuses the password is the one case where a stale endpoint looks like a
        // refusal, so it must not turn a prompt into a retry: the body may already have been taken.
        let directory = tempfile::tempdir().unwrap();
        let session = serde_json::json!({
            "id": "ses_owned",
            "title": "Owned session",
            "location": {"directory": directory.path().to_string_lossy()},
            "time": {"created": 1, "updated": 2}
        });
        let (port, requests) = serving_status_per_method(
            "200 OK",
            "401 Unauthorized",
            serde_json::json!({"data": session}).to_string(),
        );
        let (service, _address, _launched, discovered) =
            service_behind_a_movable_endpoint(test_endpoint(port));

        let refused = service
            .prompt("checkout", directory.path(), "ses_owned", "one review")
            .expect_err("the service refused the prompt");
        assert!(
            error_message(&refused).contains("rejected Muster's credentials"),
            "{refused:?}"
        );

        let asked = requests.lock().unwrap().clone();
        let sends: Vec<&String> = asked
            .iter()
            .filter(|line| line.starts_with("POST"))
            .collect();
        assert_eq!(
            sends.len(),
            1,
            "the prompt was sent more than once: {sends:?}"
        );
        // Discovery did run, because the refusal is about the endpoint rather than the request.
        assert_eq!(discovered.load(Ordering::SeqCst), 1);
    }

    /// Proves the round marker really reaches the session, which is what makes the
    /// reconnect path safe.
    ///
    /// `cargo test a_round_marker_reaches_the_real_session -- --ignored --nocapture`
    #[ignore = "live OpenCode integration; requires a configured provider and sends a prompt"]
    #[test]
    fn a_round_marker_reaches_the_real_session() {
        let directory = required_live_directory("MUSTER_AGENT_BRIDGE_DIR");
        let agents = live_service();
        let created = agents
            .create_session("checkout:marker", &directory, "marker probe")
            .expect("create a session on the real server");
        let marker = review_round_marker("round:marker-probe");

        // Before the send, the marker is genuinely absent: an absent marker after a crash
        // is the signal that the message never arrived, so this must not be a false match.
        assert!(
            !agents
                .session_mentions("checkout:marker", &directory, &created.id, &marker)
                .expect("read the transcript"),
            "the marker was found before anything was sent"
        );

        agents
            .prompt(
                "checkout:marker",
                &directory,
                &created.id,
                &build_round_prompt("# Code Review\n", &marker),
            )
            .expect("send the round prompt");

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if agents
                .session_mentions("checkout:marker", &directory, &created.id, &marker)
                .expect("read the transcript")
            {
                break;
            }
            assert!(
                Instant::now() <= deadline,
                "the marker never reached the session transcript"
            );
            sleep(Duration::from_millis(500));
        }
        agents.stop("checkout:marker");
    }

    /// Proves what the server can and cannot report about a turn.
    ///
    /// The session's `idle` field is absent both for a session that never ran and one that is
    /// working; `/api/session/active` is the separate running signal. This test pins the
    /// ambiguity of `idle_at` alone while checking how that timestamp changes after a turn.
    ///
    /// `cargo test a_turn_is_observable_through_the_idle_time -- --ignored --nocapture`
    #[ignore = "live OpenCode integration; requires a configured provider and sends a prompt"]
    #[test]
    fn a_turn_is_observable_through_the_idle_time() {
        let directory = required_live_directory("MUSTER_AGENT_BRIDGE_DIR");
        let agents = live_service();
        let created = agents
            .create_session("checkout:turn", &directory, "turn probe")
            .expect("create a session on the real server");

        let idle_at = |id: &str| {
            agents
                .owned_session("checkout:turn", &directory, id)
                .map(|session| session.idle_at)
        };
        // A session that never ran reports no idle time. `idle_at` alone cannot distinguish it
        // from an active turn; the separate `/api/session/active` answer does that.
        assert_eq!(
            idle_at(&created.id).ok().flatten(),
            None,
            "a fresh session has no idle time"
        );

        agents
            .prompt(
                "checkout:turn",
                &directory,
                &created.id,
                "Reply with the single word OK and do nothing else.",
            )
            .expect("send a prompt");

        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            if idle_at(&created.id).ok().flatten().is_some() {
                break;
            }
            assert!(
                Instant::now() <= deadline,
                "the session never reported an idle time"
            );
            sleep(Duration::from_millis(400));
        }
        agents.stop("checkout:turn");
    }

    /// Proves that the fields Muster renders a row from are really on the wire.
    ///
    /// The agent name, the model, the parent of a subagent and the color OpenCode paints an
    /// agent with are all read off routes that exist in 2.0.18, and this is the only thing that
    /// says they still are.
    ///
    /// `cargo test the_agent_catalog_is_readable_from_a_real_server -- --ignored --nocapture`
    #[ignore = "live OpenCode integration; requires the local OpenCode server and creates a session"]
    #[test]
    fn the_agent_catalog_is_readable_from_a_real_server() {
        let directory = required_live_directory("MUSTER_AGENT_BRIDGE_DIR");
        let agents = live_service();
        let agents_for_catalog = agents
            .agents("checkout:catalog", &directory)
            .expect("read the agent catalog");

        // A server with nothing configured still offers its built-in agents, which is what
        // makes a fallback for the missing color reachable at all.
        assert!(
            !agents_for_catalog.is_empty(),
            "the catalog came back empty"
        );
        for agent in &agents_for_catalog {
            assert!(!agent.id.is_empty(), "an agent with no id: {agent:?}");
            assert!(
                ["primary", "subagent", "all", ""].contains(&agent.mode.as_str()),
                "an agent with an unknown mode: {agent:?}"
            );
            // A color, where there is one, is something a stylesheet can take.
            if let Some(color) = &agent.color {
                assert!(
                    color.starts_with('#') && color.len() > 3,
                    "a color that is not a hex: {agent:?}"
                );
            }
        }

        // The session carries the agent and the model the row paints.
        let created = agents
            .create_session("checkout:catalog", &directory, "catalog probe")
            .expect("create a session on the real server");
        let resolved = agents
            .owned_session("checkout:catalog", &directory, &created.id)
            .expect("read the session back");
        assert_eq!(resolved.id, created.id);
        // A session nobody has prompted has run no turn, so both are absent: that absence is
        // what the row has to render as "nothing to say" rather than as a value.
        assert_eq!(resolved.idle_at, None);
        assert!(
            resolved.outcome.is_none() || resolved.outcome.as_deref() == Some("succeeded"),
            "a session that never ran reported an outcome: {resolved:?}"
        );
        agents.stop("checkout:catalog");
    }

    /// Talks to a real `opencode serve`; it is ignored by default because it needs the
    /// OpenCode binary and a working provider.
    ///
    /// `cargo test bridge_talks_to_a_real_server -- --ignored --nocapture`
    #[ignore = "live OpenCode integration; requires a configured provider and sends a prompt"]
    #[test]
    fn bridge_talks_to_a_real_server() {
        let first = required_live_directory("MUSTER_AGENT_BRIDGE_DIR");
        let second = required_live_directory("MUSTER_AGENT_BRIDGE_OTHER_DIR");
        let agents = live_service();

        // Collect normalized events so the SSE reader is exercised, not just the requests.
        let seen: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        agents.set_event_sink(Arc::new(move |event: AgentEvent| {
            if let Ok(mut seen) = sink.lock() {
                seen.push(event);
            }
        }));

        let created = agents
            .create_session("checkout:first", &first, "bridge probe")
            .expect("create a session on the real server");
        assert!(created.id.starts_with("ses_"), "{}", created.id);

        // The same id in another checkout's server must not resolve.
        let leaked = agents.owned_session("checkout:second", &second, &created.id);
        assert!(
            leaked.is_err(),
            "a session leaked across checkouts: {leaked:?}"
        );

        let sent = agents
            .prompt(
                "checkout:first",
                &first,
                &created.id,
                "Reply with the single word OK and do nothing else.",
            )
            .expect("send a prompt to the real server");
        assert_eq!(sent.id, created.id);

        let sessions = agents
            .sessions("checkout:first", &first)
            .expect("list sessions");
        assert!(
            sessions.iter().any(|session| session.id == created.id),
            "the session vanished: {sessions:?}"
        );

        // The sink is app-wide, so both checkouts' events arrive tagged. What must hold is
        // that this session's turn is reported as ours, and never as the other checkout's.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let snapshot = seen.lock().map(|events| events.clone()).unwrap_or_default();
            assert!(
                snapshot
                    .iter()
                    .filter(|event| event.session_id.as_deref() == Some(created.id.as_str()))
                    .all(|event| event.checkout_id == "checkout:first"),
                "an event for this session was attributed to another checkout: {snapshot:?}"
            );
            if snapshot.iter().any(|event| {
                event.kind == crate::domain::agent::AgentEventKind::TurnStarted
                    && event.session_id.as_deref() == Some(created.id.as_str())
                    && event.checkout_id == "checkout:first"
            }) {
                break;
            }
            assert!(
                Instant::now() <= deadline,
                "no turnStarted event arrived: {snapshot:?}"
            );
            sleep(Duration::from_millis(250));
        }

        agents.stop("checkout:first");
        agents.stop("checkout:second");
    }

    /// D2-05, D2-09, D2-10 and D2-15 in one pass, against two real servers running in two
    /// different checkouts: the shape the acceptance runbook asks for.
    ///
    /// What is proven here is the loop itself: one message carrying the whole review, a turn
    /// that ends, a line the agent actually rewrote, and a second checkout that stays out of
    /// the way. What is not proven here is the presentation layer, which is judged from the
    /// diff by the frontend and cannot be seen from a test.
    ///
    /// `cargo test two_checkouts_run_the_review_loop -- --ignored --nocapture`
    #[ignore = "live OpenCode integration; requires a configured provider, sends prompts, and edits files"]
    #[test]
    fn two_checkouts_run_the_review_loop() {
        let first = required_live_directory("MUSTER_AGENT_BRIDGE_DIR");
        let second = required_live_directory("MUSTER_AGENT_BRIDGE_OTHER_DIR");
        let target = first.join("muster_loop_target.txt");
        let second_file = first.join("muster_second_target.txt");
        let original = "alpha\nbravo\n";
        std::fs::write(&target, original).expect("write the file under review");
        std::fs::write(&second_file, "one\ntwo\n").expect("write the second file under review");

        let agents = live_service();
        let mine = agents
            .create_session("checkout:loop", &first, "loop")
            .expect("session in the first checkout");
        let other = agents
            .create_session("checkout:loop-other", &second, "other loop")
            .expect("session in the second checkout");

        // Two agents means two servers: an id from one checkout is meaningless in the other.
        assert!(
            agents
                .owned_session("checkout:loop-other", &second, &mine.id)
                .is_err(),
            "a session id resolved in another checkout's server"
        );

        // D2-05: both files' drafts leave as one message, marker included. Each instruction
        // is unambiguous on purpose: a review the model has to guess at makes it stop and
        // ask, and an unanswered question is not something this server version can settle.
        let marker = review_round_marker("round:loop");
        let review = [
            "### `muster_loop_target.txt`".to_string(),
            String::new(),
            "- line 1: replace the whole file with exactly: `done by agent`.".to_string(),
            String::new(),
            "### `muster_second_target.txt`".to_string(),
            String::new(),
            "- line 1: replace the whole file with exactly: `also done by agent`.".to_string(),
        ]
        .join("\n");
        let prompt = build_round_prompt(&review, &marker);
        assert!(prompt.contains(&marker), "the marker is not in the prompt");
        assert!(
            prompt.contains("muster_loop_target.txt")
                && prompt.contains("muster_second_target.txt"),
            "the two files did not leave in the same message"
        );
        agents
            .prompt("checkout:loop", &first, &mine.id, &prompt)
            .expect("send the round");

        // D2-09 needs the turn to be over before anything can be judged, and v2.0.18 reports
        // that only as an idle time on the session.
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            let session = agents.owned_session("checkout:loop", &first, &mine.id);
            let finished = matches!(&session, Ok(session) if session.idle_at.is_some());
            let seen = match &session {
                Ok(session) => format!("still running, idle_at={:?}", session.idle_at),
                Err(error) => format!("session read failed: {error:?}"),
            };
            if finished {
                break;
            }
            assert!(
                Instant::now() <= deadline,
                "the review turn never came back to idle ({seen})"
            );
            sleep(Duration::from_millis(500));
        }

        // D2-05, second half: the message that carried the review is really in the
        // transcript, so a reconnect would recognize this exact send.
        assert!(
            agents
                .session_mentions("checkout:loop", &first, &mine.id, &marker)
                .expect("read the transcript"),
            "the round marker never landed in the session"
        );

        // D2-15: the agent kept working, so the working tree moved — on both files, which is
        // what makes this a batch rather than a single comment that got through.
        let changed = std::fs::read_to_string(&target).expect("read the file again");
        assert_ne!(changed, original, "the agent did not touch the first file");
        assert!(
            std::fs::read_to_string(&second_file)
                .expect("read the second file")
                .contains("also done by agent"),
            "the second file's comment did not reach the agent's work"
        );

        // D2-10: a note anchored on the old first line has drifted, and that is exactly what
        // marks it outdated — the hash is what decides, not who edited the line.
        assert_ne!(
            review_anchor_hash(original),
            review_anchor_hash(&changed),
            "the rewritten line still hashes the same"
        );

        // The second agent was never dragged into this: its session exists, has no turn, and
        // does not know about the first checkout's session.
        let untouched = agents
            .sessions("checkout:loop-other", &second)
            .expect("list the other checkout's sessions");
        assert!(
            untouched.iter().any(|session| session.id == other.id),
            "the second agent's session vanished"
        );
        assert!(
            untouched.iter().all(|session| session.id != mine.id),
            "the first checkout's session leaked into the second"
        );
        assert!(
            untouched
                .iter()
                .find(|session| session.id == other.id)
                .and_then(|session| session.idle_at)
                .is_none(),
            "the second agent reports a turn it never ran"
        );

        agents.stop("checkout:loop");
        agents.stop("checkout:loop-other");
    }

    fn interrupted_error() -> ureq::Error {
        ureq::Error::Io(io::Error::from(io::ErrorKind::Interrupted))
    }

    #[test]
    fn an_interrupted_read_does_not_retry_after_its_deadline() {
        let deadline = Instant::now() + Duration::from_millis(30);
        let mut attempts = 0;
        let error = retry_interrupted_until(deadline, |_timeout| {
            attempts += 1;
            sleep(Duration::from_millis(40));
            Err::<(), _>(interrupted_error())
        })
        .expect_err("the retry budget expired");

        assert!(matches!(error, BridgeError::Unavailable(_)));
        assert_eq!(attempts, 1, "no HTTP attempt starts after the deadline");
    }

    #[test]
    fn a_read_a_signal_interrupted_is_asked_again_and_the_answer_arrives() {
        let mut attempts = 0;
        let answer = retry_interrupted(|| {
            attempts += 1;
            if attempts == 1 {
                Err(interrupted_error())
            } else {
                Ok("catalog")
            }
        });

        assert_eq!(answer.expect("the repeated read answers"), "catalog");
        assert_eq!(attempts, 2, "the interrupted read was made again");
    }

    #[test]
    fn a_read_a_signal_keeps_interrupting_gives_up_on_its_budget() {
        let mut attempts = 0;
        let error = retry_interrupted(|| {
            attempts += 1;
            Err::<(), _>(interrupted_error())
        })
        .expect_err("a socket that is never readable is not repeated forever");

        assert!(
            is_interrupted(&error),
            "the interrupt that ended it is reported"
        );
        assert_eq!(attempts, INTERRUPTED_READ_ATTEMPTS as usize);
    }

    #[test]
    fn a_transport_failure_that_is_not_a_signal_is_reported_after_one_attempt() {
        let mut attempts = 0;
        let error = retry_interrupted(|| {
            attempts += 1;
            Err::<(), _>(ureq::Error::HostNotFound)
        })
        .expect_err("a connection that was refused is reported");

        assert!(!is_interrupted(&error));
        assert_eq!(
            attempts, 1,
            "a failure repeating cannot fix is not repeated"
        );
    }

    #[test]
    fn an_interrupted_syscall_is_not_reported_as_an_unreachable_server() {
        let interrupted = send_json::<serde_json::Value>(Err(ureq::Error::Io(io::Error::from(
            io::ErrorKind::Interrupted,
        ))))
        .expect_err("an interrupted request did not answer");

        let message = match interrupted {
            BridgeError::Unavailable(message) => message,
            other => {
                panic!("an interrupted request is not a server that could not answer: {other:?}")
            }
        };
        assert!(
            message.starts_with(INTERRUPTED_REQUEST),
            "a signal ended the syscall, and saying so beats claiming the server is gone: {message}"
        );
        assert!(
            message.contains(&io::Error::from(io::ErrorKind::Interrupted).to_string()),
            "the cause is kept for whoever reads it: {message}"
        );
    }
}
