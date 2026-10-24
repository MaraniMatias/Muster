//! The OpenCode bridge: the service a person starts, and the review it is sent to.
//!
//! Muster is a client of that service and never its owner. The service registers
//! itself so its clients can find it, [`discover`] reads that registration and
//! checks that the service answers there. Nothing here starts a server and nothing
//! here ends one: a service that is not running leaves the app disconnected, which
//! is a state to wait in, not a failure to clean up after.
//!
//! Discovery is **fail-closed**, and every refusal happens before the registration's
//! password is on the wire:
//!
//! * The API major has to be one this app speaks. A service on another major answers
//!   a different contract, and a request shaped for this one would be sent against it.
//! * The address has to be a loopback socket over plain HTTP. The service binds
//!   loopback, so an address that is not loopback is either a different machine or a
//!   rewritten registration file, and either way the password does not belong there.
//! * A password has to be there. An empty one is a registration that cannot say who
//!   is asking.
//! * The service has to answer the probe with that password, and answer it with a 2xx. A
//!   service that is gone and a service that refuses this app are different problems, and
//!   the difference is what tells a person whether to look at the file or start OpenCode.
//!
//! Once past discovery the transport is pinned as well, because it is the only way a
//! request could leave loopback: **no redirect is followed**, so a `3xx` is a refusal
//! rather than an instruction, and **no proxy is read from the environment**, so a
//! `HTTP_PROXY` somebody exported cannot put the registration's password and the review
//! in a third party's hands.
//!
//! A session is bound to a checkout the same way: every request carries the
//! checkout's directory twice (once as the `x-opencode-directory` header, which
//! decides which location answers, and once as the `directory` query parameter,
//! which filters session lists), and an answer that names another directory is
//! refused rather than read. A `ses_…` id from one checkout is meaningless in
//! another's directory.
//!
//! What the service cannot tell us is deliberately not guessed at: OpenCode has no
//! client registry, so a session cannot be attributed to the terminal showing it. A
//! review names the session it targets, and the caller owns that choice.

use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{self, Read},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use base64::Engine as _;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

#[cfg(test)]
mod mock;
pub mod observe;
pub mod relocate;
pub mod rounds;

pub use observe::{
    agent_color, agent_label, elapsed_seconds, is_hex_color, match_session_title, now_millis,
    terminal_session_title, AgentAgent, AgentSession, Attention, MatchedSession,
};
pub use rounds::{unsent_notes, Destination, ReviewRound, RoundStatus, RoundStore};

/// Where the service registers itself, and the name it registers under.
const STATE_DIR_ENV: &str = "XDG_STATE_HOME";
const STATE_DIR_FROM_HOME: &str = ".local/state";
const SERVICE_DIR: &str = "opencode";
const SERVICE_FILE: &str = "service.json";

/// The API major this app speaks.
const SUPPORTED_MAJOR: &str = "2";

/// A discovery probe is a single loopback request. A service that is gone fails to
/// connect at once, and one that is wedged is not worth holding the UI for long.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// A registration is a handful of fields written by a service the user started. Past
/// this it is not that file, and reading it unbounded is how a path that is not a file —
/// a fifo would block this thread forever — becomes a problem.
const REGISTRATION_CAP: u64 = 64 * 1024;

/// The rest of the requests: a service busy answering a turn can delay them, but a
/// review is one message and waiting for a wedge is not worth it either.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The entire observation, including discovery, pending lists and catalogs.
pub const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(4);

/// An observation never waits this long again for every endpoint in a snapshot.
const READ_REQUEST_TIMEOUT: Duration = Duration::from_secs(1);

/// One shared deadline and cancellation flag for an off-thread service read.
///
/// Clones share cancellation. Requests check it before touching the wire and use the
/// remaining deadline (at most one second) as their HTTP timeout, including the body.
#[derive(Clone, Debug)]
pub struct ReadBudget {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    /// The longest a single request may take.
    request: Duration,
}

impl ReadBudget {
    /// A budget for an observation: one deadline for the whole read, and a short cap
    /// on each request so one slow endpoint cannot spend it all.
    pub fn new(timeout: Duration) -> Self {
        Self::with_request_cap(timeout, READ_REQUEST_TIMEOUT)
    }

    /// A budget for a caller that is waiting for one answer: the same total deadline,
    /// but a request may take as long as that deadline allows.
    ///
    /// An observation asks about a service that may be busy with a turn, and a short
    /// request is what keeps the panel painting. An action — a send, a retry — is
    /// waiting for the service on purpose, and a timeout there is a question about a
    /// review rather than a row that could not be painted.
    pub fn patient(timeout: Duration) -> Self {
        Self::with_request_cap(timeout, timeout)
    }

    fn with_request_cap(timeout: Duration, request: Duration) -> Self {
        Self {
            deadline: Instant::now() + timeout,
            cancelled: Arc::new(AtomicBool::new(false)),
            request,
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// A short slice of this read, sharing its cancellation and outer deadline.
    pub fn slice(&self, timeout: Duration) -> Self {
        Self {
            deadline: self.deadline.min(Instant::now() + timeout),
            cancelled: Arc::clone(&self.cancelled),
            request: self.request.min(timeout),
        }
    }

    /// The time left before this read's deadline.
    ///
    /// A read that is given a share of what is left keeps the rest for the work that
    /// comes after it, rather than spending the whole deadline on itself.
    pub fn remaining(&self) -> Result<Duration, DispatchError> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(DispatchError::Unavailable(
                "the OpenCode observation was cancelled".into(),
            ));
        }
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                DispatchError::Unavailable("the OpenCode observation deadline expired".into())
            })
    }

    /// The longest one request may take inside this budget.
    pub fn request_timeout(&self) -> Result<Duration, DispatchError> {
        Ok(self.remaining()?.min(self.request))
    }
}

/// The header the service reads to decide which location answers a request.
const DIRECTORY_HEADER: &str = "x-opencode-directory";

/// What a person sees when there is no service to talk to.
pub const NOT_RUNNING: &str = "OpenCode is not running. Start it and this app connects to it.";

/// The prefix of the header value the service expects: basic auth over a fixed
/// username, because the registration carries the password and nothing else
/// identifies a client.
const AUTH_USERNAME: &str = "opencode";

/// Everything that can go wrong talking to the service, kept apart because the
/// caller acts on the difference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchError {
    /// The service is not there, or not answering. A state to wait in.
    Unavailable(String),
    /// Something answered, and it is not this app's to talk to: a foreign
    /// registration, another major, a refused password, or a session that belongs to
    /// another directory. Never retried against the same answer.
    Foreign(String),
    /// The request itself did not work. A send that fails this way may still have
    /// landed, so the caller keeps its round unresolved rather than assuming.
    Failed(String),
    /// The connection was never made, so no byte of the request was written: the service
    /// provably did not take it. The one failure a send may be repeated after on its own.
    NotDelivered(String),
}

impl DispatchError {
    /// Whether the request provably never reached the service, which is what makes a send
    /// safe to offer again rather than a question to settle.
    pub fn never_sent(&self) -> bool {
        matches!(self, Self::NotDelivered(_))
    }
}

impl std::fmt::Display for DispatchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Unavailable(message)
            | Self::Foreign(message)
            | Self::Failed(message)
            | Self::NotDelivered(message) => message,
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for DispatchError {}

/// A running service, as its own registration describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceEndpoint {
    /// The origin every request goes to, without a trailing slash. It carries the
    /// authority the registration gave, so the port is here rather than beside it.
    pub url: String,
    /// The password the registration carries, which every request presents.
    pub password: String,
}

/// What the service writes about itself. The rest of the file is its business.
#[derive(Debug, Deserialize)]
struct Registration {
    url: String,
    password: String,
    version: String,
}

/// One answer from the service: the payload, and the location that produced it.
#[derive(Debug, Deserialize)]
struct ApiEnvelope<T> {
    data: T,
    /// Absent from an answer that is not scoped to a location.
    #[serde(default)]
    location: Option<ApiLocation>,
}

#[derive(Debug, Deserialize)]
struct ApiLocation {
    directory: String,
}

/// One session as `POST /api/session` and `GET /api/session/{id}` answer it.
///
/// Every `#[serde(default)]` here is tolerance of a third party's wire, not
/// compatibility with an older Muster. `id` carries no default: without it there is
/// nothing to attribute the answer to, so it is refused.
#[derive(Debug, Deserialize)]
struct ApiSession {
    id: String,
    /// The title the session's own TUI writes into the terminal it runs in.
    #[serde(default)]
    title: String,
    /// When the session was created, touched and last went idle.
    #[serde(default)]
    time: ApiSessionTime,
    /// Present inside the session object, unlike the envelope-level `location`.
    #[serde(default)]
    location: Option<ApiLocation>,
    /// The agent running it, named by the catalog.
    #[serde(default)]
    agent: Option<String>,
    /// The session this one is a child of, when it is a subagent.
    #[serde(default, rename = "parentID")]
    parent_id: Option<String>,
    /// How the last turn ended.
    #[serde(default)]
    outcome: Option<String>,
    /// The model the session runs on.
    #[serde(default)]
    model: Option<ApiModel>,
}

/// The model's own shape, which carries a provider of its own.
#[derive(Debug, Deserialize)]
struct ApiModel {
    #[serde(default)]
    provider_id: String,
    id: String,
    #[serde(default)]
    variant: Option<String>,
}

impl ApiModel {
    /// The model as one string, in the `provider/id#variant` form OpenCode writes in
    /// its configuration.
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

/// The times a session carries, in epoch milliseconds.
#[derive(Debug, Default, Deserialize)]
struct ApiSessionTime {
    #[serde(default)]
    created: i64,
    #[serde(default)]
    updated: i64,
    #[serde(default)]
    idle: Option<i64>,
}

/// Where the service may have registered itself, most specific first.
///
/// The environment is read here and nowhere else in this module, so a caller
/// that wants to check the paths themselves can hand [`registration_files_in`] the
/// state directory it means rather than the one the process happens to carry.
pub fn registration_files(home: &Path) -> Vec<PathBuf> {
    registration_files_in(home, std::env::var_os(STATE_DIR_ENV).as_deref())
}

/// [`registration_files`] for a state directory the caller names, or for none.
///
/// The split is so the path this builds can be asserted against a directory the
/// test chooses. Setting the process's own `XDG_STATE_HOME` to get there would
/// write to every other test running beside it.
fn registration_files_in(home: &Path, state: Option<&OsStr>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(state) = state.filter(|state| !state.is_empty()) {
        files.push(PathBuf::from(state).join(SERVICE_DIR).join(SERVICE_FILE));
    }
    files.push(
        home.join(STATE_DIR_FROM_HOME)
            .join(SERVICE_DIR)
            .join(SERVICE_FILE),
    );
    files
}

/// The authorization header value the service expects.
pub fn authorization(password: &str) -> String {
    let credentials = format!("{AUTH_USERNAME}:{password}");
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(credentials)
    )
}

/// An OpenCode session id is `ses_` plus an opaque suffix. Rejecting anything else
/// keeps a value that did not come from the service out of a path we interpolate.
fn validate_session_id(session_id: &str) -> Result<(), DispatchError> {
    let valid = session_id.len() > 4
        && session_id.starts_with("ses_")
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(DispatchError::Foreign(
            "that is not a valid OpenCode session id".into(),
        ))
    }
}

/// Finds the running service, or says why there is not one.
///
/// A registration that outlives its service is the normal way this fails: the file is
/// written once at startup and is still there after the service is gone. The probe is
/// what tells those two apart, which is why reading the file is not finding a service.
pub fn discover(home: &Path) -> Result<ServiceEndpoint, DispatchError> {
    let registration = read_registration(home)?;
    supported_version(&registration.version)?;
    let endpoint = endpoint(&registration.url, registration.password)?;
    probe(&endpoint)?;
    Ok(endpoint)
}

fn read_registration(home: &Path) -> Result<Registration, DispatchError> {
    for file in registration_files(home) {
        let Some(contents) = read_registration_file(&file)? else {
            // Nothing registered here; the next location is the next candidate.
            continue;
        };
        return serde_json::from_str(&contents).map_err(|_| {
            DispatchError::Foreign("the OpenCode service registration could not be read".into())
        });
    }
    Err(DispatchError::Unavailable(NOT_RUNNING.into()))
}

/// The registration, read as a bounded regular file.
///
/// The file is where a service a person started writes it, and it is read the way the rest
/// of this app's state is read: never a symlink, never a device, never more than the cap.
/// `read_to_string` alone would follow a link to somebody else's file and block forever on
/// a fifo, and this is a file that decides where a password goes.
fn read_registration_file(path: &Path) -> Result<Option<String>, DispatchError> {
    let unreadable = |error: io::Error| {
        DispatchError::Failed(format!(
            "could not inspect the OpenCode service registration {}: {error}",
            path.display()
        ))
    };
    let too_large =
        || DispatchError::Foreign("the OpenCode service registration is not a 64 KiB file".into());
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unreadable(error)),
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(DispatchError::Foreign(
                "the OpenCode service registration must be a regular file, not a symlink".into(),
            ))
        }
        Ok(metadata) if metadata.len() > REGISTRATION_CAP => return Err(too_large()),
        Ok(_) => {}
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // `O_NOFOLLOW` closes the swap between the check and the open; `O_NONBLOCK` is what
        // keeps a device node from parking this thread in a read.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(unreadable)?;
    let metadata = file.metadata().map_err(unreadable)?;
    if !metadata.is_file() || metadata.len() > REGISTRATION_CAP {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    file.take(REGISTRATION_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if bytes.len() as u64 > REGISTRATION_CAP {
        return Err(too_large());
    }
    String::from_utf8(bytes).map(Some).map_err(|_| {
        DispatchError::Foreign("the OpenCode service registration is not UTF-8".into())
    })
}

fn supported_version(version: &str) -> Result<(), DispatchError> {
    match version.split('.').next() {
        Some(major) if major == SUPPORTED_MAJOR => Ok(()),
        _ => Err(DispatchError::Foreign(format!(
            "the running OpenCode service is version {version}, and this app speaks version \
             {SUPPORTED_MAJOR}"
        ))),
    }
}

/// Reads the origin out of the registration and keeps it only if it is one this app
/// may send the password to. Plain HTTP is what the service serves, so a scheme this
/// client has no TLS support for is refused rather than downgraded.
fn endpoint(url: &str, password: String) -> Result<ServiceEndpoint, DispatchError> {
    let refused = || {
        DispatchError::Foreign(format!(
            "the OpenCode service registered an address Muster cannot use: {url}"
        ))
    };
    let authority = url
        .strip_prefix("http://")
        .ok_or_else(refused)?
        .split('/')
        .next()
        .unwrap_or_default();
    let address: SocketAddr = authority.parse().map_err(|_| refused())?;
    if !address.ip().is_loopback() {
        return Err(refused());
    }
    if password.is_empty() {
        return Err(DispatchError::Foreign(
            "the OpenCode service registration carries no password".into(),
        ));
    }
    Ok(ServiceEndpoint {
        url: format!("http://{authority}"),
        password,
    })
}

/// Asks the service whether it is there, with the password that says this client may ask.
fn probe(endpoint: &ServiceEndpoint) -> Result<(), DispatchError> {
    probe_with_timeout(endpoint, PROBE_TIMEOUT)
}

fn probe_with_timeout(endpoint: &ServiceEndpoint, timeout: Duration) -> Result<(), DispatchError> {
    let agent = agent(timeout);
    let response = agent
        .get(&format!("{}/api/info", endpoint.url))
        .header("authorization", authorization(&endpoint.password))
        .call();
    match response {
        // Only a 2xx is the service. A redirect is not followed, and it is not "found"
        // either: answering `302` is what something standing in front of the service does,
        // and this client would rather be refused than believe it.
        Ok(response) if response.status().is_success() => Ok(()),
        Ok(response) => Err(refused_status(response.status().as_u16())),
        Err(ureq::Error::StatusCode(status)) => Err(refused_status(status)),
        Err(error) if failed_before_send(&error) => {
            Err(DispatchError::Unavailable(NOT_RUNNING.into()))
        }
        Err(error) => Err(map_error(error)),
    }
}

/// A service that is there but will not have this app is a different problem from one
/// that is not running, and the difference is what tells a person whether to look at
/// the registration file.
fn refused_status(status: u16) -> DispatchError {
    match status {
        401 | 403 => DispatchError::Foreign(format!(
            "the running OpenCode service refused this app with status {status}"
        )),
        status => DispatchError::Failed(format!(
            "the OpenCode service answered the discovery probe with status {status}"
        )),
    }
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .timeout_connect(Some(timeout.min(PROBE_TIMEOUT)))
            // No redirect is followed. The service is one loopback origin, and following a
            // `Location` is the one thing on this path that would carry the registration's
            // password to an address this client never validated.
            .max_redirects(0)
            // No proxy from the environment. `ureq`'s default reads `HTTP_PROXY`, which
            // would hand every review and this client's password to whoever exported it.
            .proxy(None)
            .build(),
    )
}

/// One conversation with the service, scoped to one checkout's directory.
///
/// A client with no directory is a client of the whole service: the reads that answer
/// for every location the service knows carry no scope at all, because a session that
/// has moved is not listed by the directory it left.
#[derive(Clone)]
pub struct Client {
    endpoint: ServiceEndpoint,
    directory: PathBuf,
    /// Whether the requests carry the directory. A client without one asks about the
    /// service rather than about a checkout.
    scoped: bool,
    agent: ureq::Agent,
    read_budget: Option<ReadBudget>,
}

impl Client {
    /// Reaches the service the user started, if there is one, for one checkout.
    ///
    /// Discovery is the whole of startup: there is no port to pick and no process to
    /// wait for, so this either has a service to talk to or reports that there is not
    /// one yet.
    pub fn connect(home: &Path, directory: &Path) -> Result<Self, DispatchError> {
        Ok(Self {
            endpoint: discover(home)?,
            directory: directory.to_path_buf(),
            scoped: true,
            agent: agent(REQUEST_TIMEOUT),
            read_budget: None,
        })
    }

    /// A client of the whole service, for the reads that answer for every location.
    ///
    /// The service registers itself once and its clients find it there, so this is the
    /// same discovery a scoped client does: there is no port to pick and no process to
    /// wait for.
    pub fn connect_service(home: &Path) -> Result<Self, DispatchError> {
        Self::connect_service_with_budget(home, ReadBudget::new(OBSERVATION_TIMEOUT))
    }

    /// Uses one caller-owned deadline for discovery and every read that follows it.
    /// Cancel the budget when the window or observation is superseded.
    pub fn connect_service_with_budget(
        home: &Path,
        budget: ReadBudget,
    ) -> Result<Self, DispatchError> {
        budget.remaining()?;
        let registration = read_registration(home)?;
        supported_version(&registration.version)?;
        let endpoint = endpoint(&registration.url, registration.password)?;
        probe_with_timeout(&endpoint, budget.request_timeout()?)?;
        Ok(Self {
            endpoint,
            directory: PathBuf::new(),
            scoped: false,
            agent: agent(REQUEST_TIMEOUT),
            read_budget: Some(budget),
        })
    }

    /// Bounds subsequent reads, including on a client already scoped to a checkout.
    pub fn with_read_budget(mut self, budget: ReadBudget) -> Self {
        self.read_budget = Some(budget);
        self
    }

    fn request_timeout(&self) -> Result<Duration, DispatchError> {
        match &self.read_budget {
            Some(budget) => budget.request_timeout(),
            None => Ok(REQUEST_TIMEOUT),
        }
    }

    /// A checkout-scoped client sharing the observation's discovery and read deadline.
    pub fn connect_with_budget(
        home: &Path,
        directory: &Path,
        budget: ReadBudget,
    ) -> Result<Self, DispatchError> {
        let mut client = Self::connect_service_with_budget(home, budget)?;
        client.directory = directory.to_path_buf();
        client.scoped = true;
        Ok(client)
    }

    pub fn endpoint(&self) -> &ServiceEndpoint {
        &self.endpoint
    }

    /// The checkout directory this client talks about. Every request is scoped to it, so
    /// a caller holding a checkout can tell whether this client is the one for it.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Starts a session in this checkout and returns its id.
    ///
    /// The session belongs to Muster: nothing else in the service knows about it, and
    /// it exists so the first review has somewhere to go that is not somebody's
    /// working conversation.
    pub fn create_session(&self, title: &str) -> Result<String, DispatchError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(DispatchError::Failed(
                "an OpenCode session needs a title".into(),
            ));
        }
        let created: ApiSession =
            self.post_json("/api/session", &serde_json::json!({ "title": title }))?;
        validate_session_id(&created.id)?;
        self.check_location(created.location.as_ref())?;
        Ok(created.id)
    }

    /// Confirms the session exists in this checkout, so a foreign id fails before
    /// anything is sent.
    pub fn resolve_session(&self, session_id: &str) -> Result<(), DispatchError> {
        validate_session_id(session_id)?;
        let session: ApiSession = self.get_json(&format!("/api/session/{session_id}"))?;
        if session.id != session_id {
            return Err(DispatchError::Foreign(
                "the agent server answered about a different session".into(),
            ));
        }
        self.check_location(session.location.as_ref())
    }

    /// Sends the review as one message.
    ///
    /// A returned error is *uncertain*: the service may have taken the prompt and lost
    /// the answer on the way back. The caller resolves that with
    /// [`crate::review::review_round_marker`] in the transcript, never by sending again
    /// on its own.
    pub fn prompt(&self, session_id: &str, text: &str) -> Result<(), DispatchError> {
        validate_session_id(session_id)?;
        let text = text.trim();
        crate::review::check_round_prompt(text).map_err(DispatchError::Failed)?;
        // Resolve first so a foreign id fails before the prompt is offered.
        self.resolve_session(session_id)?;
        let _: serde_json::Value = self.post_json(
            &format!("/api/session/{session_id}/prompt"),
            &serde_json::json!({ "text": text }),
        )?;
        Ok(())
    }

    /// Whether the session's messages mention `marker`.
    ///
    /// The route answers JSON and the message shape differs between OpenCode versions,
    /// so the payload is re-serialized and searched rather than modelled: the only
    /// question asked is whether a given marker is present.
    pub fn session_mentions(&self, session_id: &str, marker: &str) -> Result<bool, DispatchError> {
        if marker.is_empty() {
            return Ok(false);
        }
        self.resolve_session(session_id)?;
        let messages: serde_json::Value =
            self.get_json(&format!("/api/session/{session_id}/message"))?;
        let transcript = serde_json::to_string(&messages).map_err(|error| {
            DispatchError::Failed(format!("could not read the transcript: {error}"))
        })?;
        Ok(transcript.contains(marker))
    }

    /// Refuses a session or an answer that belongs to another directory than the
    /// checkout this client was scoped to.
    fn check_location(&self, location: Option<&ApiLocation>) -> Result<(), DispatchError> {
        if self.scoped {
            check_scope(location, &self.directory)
        } else {
            Ok(())
        }
    }

    fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, DispatchError> {
        let scope = self.scope()?;
        let mut request = self.agent.get(&format!("{}{path}", self.endpoint.url));
        if let Some(scope) = &scope {
            request = request
                .query("directory", scope)
                .header(DIRECTORY_HEADER, scope);
        }
        let response = request
            .config()
            .timeout_global(Some(self.request_timeout()?))
            .build()
            .header("authorization", authorization(&self.endpoint.password))
            .header("accept", "application/json")
            .call()
            .map_err(map_error)?;
        let envelope: ApiEnvelope<T> = read_json(response)?;
        self.check_location(envelope.location.as_ref())?;
        Ok(envelope.data)
    }

    /// Fetches a path that carries its own query, with no scope at all.
    ///
    /// The path is given whole because the one read that asks about the whole service
    /// asks for a page of sessions with `limit`, and a second query parameter appended
    /// to that would be a second `?`.
    fn get_json_unscoped<T: DeserializeOwned>(
        &self,
        path_and_query: &str,
    ) -> Result<T, DispatchError> {
        let response = self
            .agent
            .get(&format!("{}{path_and_query}", self.endpoint.url))
            .config()
            .timeout_global(Some(self.request_timeout()?))
            .build()
            .header("authorization", authorization(&self.endpoint.password))
            .header("accept", "application/json")
            .call()
            .map_err(map_error)?;
        let envelope: ApiEnvelope<T> = read_json(response)?;
        // There is no scope to check the answer against, so there is nothing to refuse.
        Ok(envelope.data)
    }

    /// Fetches a path as one other location answers it.
    ///
    /// The catalog and the pending lists answer for one location, so a client that
    /// talks about the whole service still has to ask each location about its own.
    fn get_json_for<T: DeserializeOwned>(
        &self,
        path: &str,
        directory: &Path,
    ) -> Result<T, DispatchError> {
        let scope = scope_of(directory)?;
        let response = self
            .agent
            .get(&format!("{}{path}", self.endpoint.url))
            .config()
            .timeout_global(Some(self.request_timeout()?))
            .build()
            .query("directory", &scope)
            .header("authorization", authorization(&self.endpoint.password))
            .header(DIRECTORY_HEADER, &scope)
            .header("accept", "application/json")
            .call()
            .map_err(map_error)?;
        let envelope: ApiEnvelope<T> = read_json(response)?;
        check_scope(envelope.location.as_ref(), directory)?;
        Ok(envelope.data)
    }

    fn post_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, DispatchError> {
        let scope = self.scope()?;
        let mut request = self.agent.post(&format!("{}{path}", self.endpoint.url));
        if let Some(scope) = &scope {
            request = request
                .query("directory", scope)
                .header(DIRECTORY_HEADER, scope);
        }
        let response = request
            .config()
            .timeout_global(Some(self.request_timeout()?))
            .build()
            .header("authorization", authorization(&self.endpoint.password))
            .send_json(body)
            .map_err(map_error)?;
        let envelope: ApiEnvelope<T> = read_json(response)?;
        self.check_location(envelope.location.as_ref())?;
        Ok(envelope.data)
    }

    /// The checkout path as both the query and the header carry it. A path that
    /// cannot be spelled in a header is refused rather than sent half-encoded.
    ///
    /// `None` for a client of the whole service: a read with no directory is the one
    /// read that answers for every location the service knows.
    fn scope(&self) -> Result<Option<String>, DispatchError> {
        if !self.scoped {
            return Ok(None);
        }
        scope_of(&self.directory).map(Some)
    }
}

/// A transport failure is `Unavailable`: the service is not answering. A status it
/// chose for itself is what it says about the request, not about the endpoint.
fn map_error(error: ureq::Error) -> DispatchError {
    match error {
        ureq::Error::StatusCode(status) => refused_status(status),
        ureq::Error::Timeout(_) => DispatchError::Unavailable(format!(
            "the OpenCode service did not answer within its request deadline (at most {}s)",
            REQUEST_TIMEOUT.as_secs()
        )),
        error if failed_before_send(&error) => {
            DispatchError::NotDelivered(format!("the OpenCode service is not listening: {error}"))
        }
        error => {
            DispatchError::Unavailable(format!("the OpenCode service is not answering: {error}"))
        }
    }
}

/// Whether the failure happened before the request could be written: `connect` itself
/// failed, so there is nothing on the service's side to settle.
///
/// Everything else is uncertain, and the difference is the whole of what a round's state
/// rests on. A timeout, a reset, or a body that stopped mid-answer all happen *after* the
/// service may already have taken the prompt, and a message that may have landed is the
/// one thing this app will not send twice on its own.
fn failed_before_send(error: &ureq::Error) -> bool {
    let ureq::Error::Io(error) = error else {
        return false;
    };
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::NetworkUnreachable
            | io::ErrorKind::HostUnreachable
            | io::ErrorKind::NetworkDown
            | io::ErrorKind::AddrNotAvailable
    )
}

fn read_json<T: DeserializeOwned>(
    mut response: ureq::http::Response<ureq::Body>,
) -> Result<T, DispatchError> {
    // Redirects are not followed, so a `3xx` reaching here is the service asking this
    // client to go somewhere else. That is a refusal, not an answer to parse.
    if !response.status().is_success() {
        return Err(refused_status(response.status().as_u16()));
    }
    response
        .body_mut()
        .read_json()
        .map_err(|error| DispatchError::Failed(format!("could not read the agent answer: {error}")))
}

fn same_directory(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// Refuses an answer that names another directory than the one that was asked about.
///
/// An answer with no location at all is not refused: it says nothing about where it
/// came from, and refusing it would turn a service that omits the field into a service
/// this app cannot talk to.
fn check_scope(location: Option<&ApiLocation>, expected: &Path) -> Result<(), DispatchError> {
    let Some(location) = location else {
        return Ok(());
    };
    if same_directory(Path::new(&location.directory), expected) {
        Ok(())
    } else {
        Err(DispatchError::Foreign(
            "the agent server answered for a different directory".into(),
        ))
    }
}

/// A path as the query and the header carry it.
///
/// A path that cannot be spelled in a header is refused rather than sent half-encoded:
/// a scope that arrives mangled is a scope that names a directory this app does not
/// know, and the answer would be read as somebody else's.
fn scope_of(directory: &Path) -> Result<String, DispatchError> {
    let scope = directory.to_string_lossy().into_owned();
    if scope.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(DispatchError::Failed(
            "the checkout path cannot be represented in an HTTP header".into(),
        ));
    }
    Ok(scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    use crate::opencode::mock::Mock;
    use crate::workspace::Repo;

    fn home_with_registration(contents: &str) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join(STATE_DIR_FROM_HOME).join(SERVICE_DIR);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(SERVICE_FILE), contents).unwrap();
        home
    }

    fn registration_for(port: u16, password: &str, version: &str) -> String {
        serde_json::json!({
            "id": "9360a1e9",
            "version": version,
            "url": format!("http://127.0.0.1:{port}"),
            "pid": 4242,
            "password": password,
        })
        .to_string()
    }

    /// A checkout that canonicalizes to itself, which is what the identity check wants.
    fn checkout() -> crate::workspace::Checkout {
        let path = std::env::temp_dir().canonicalize().unwrap();
        Repo::plain(&path, "now").unwrap().checkouts[0].clone()
    }

    #[test]
    fn a_service_that_answers_is_found_without_anything_being_started() {
        let mock = Mock::start(|_| mock::reply(200, serde_json::json!({ "data": {} })));
        let home = home_with_registration(&registration_for(mock.port(), "secret", "2.0.22"));

        let endpoint = discover(home.path()).unwrap();

        assert_eq!(endpoint.url, format!("http://127.0.0.1:{}", mock.port()));
        assert_eq!(endpoint.password, "secret");
        // The probe is the only request made, and it is scoped to nothing.
        assert_eq!(mock.requests()[0].path, "/api/info");
        assert!(mock.requests()[0].query.is_none());
    }

    #[test]
    fn nobody_running_reads_as_absent_rather_than_as_a_failure() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            discover(home.path()).unwrap_err(),
            DispatchError::Unavailable(NOT_RUNNING.into())
        );
    }

    #[test]
    fn a_registration_whose_service_is_gone_reads_as_absent() {
        let taken = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = taken.local_addr().unwrap().port();
        drop(taken);
        let home = home_with_registration(&registration_for(port, "secret", "2.0.22"));

        assert_eq!(
            discover(home.path()).unwrap_err(),
            DispatchError::Unavailable(NOT_RUNNING.into())
        );
    }

    #[test]
    fn a_wrong_password_is_reported_as_a_refusal_rather_than_as_absent() {
        let mock = Mock::start(|_| mock::reply(401, serde_json::json!({})));
        let home = home_with_registration(&registration_for(mock.port(), "a-stale-one", "2.0.22"));

        let error = discover(home.path()).unwrap_err();

        assert!(matches!(error, DispatchError::Foreign(_)), "{error}");
        assert!(error.to_string().contains("401"), "{error}");
    }

    #[test]
    fn another_api_major_is_refused_before_the_password_is_sent() {
        let mock = Mock::start(|_| mock::reply(200, serde_json::json!({ "data": {} })));
        let home = home_with_registration(&registration_for(mock.port(), "secret", "1.9.0"));

        let error = discover(home.path()).unwrap_err();

        assert!(error.to_string().contains("version 1.9.0"), "{error}");
        assert!(mock.requests().is_empty());
    }

    #[test]
    fn an_address_off_the_machine_is_refused_because_the_password_is_not_sent_to_it() {
        for url in [
            "http://192.168.1.10:4096",
            "https://127.0.0.1:4096",
            "http://127.0.0.1",
            "not a url",
        ] {
            let home = home_with_registration(
                &serde_json::json!({
                    "version": "2.0.22",
                    "url": url,
                    "password": "secret",
                })
                .to_string(),
            );

            let error = discover(home.path()).unwrap_err();

            assert!(
                matches!(error, DispatchError::Foreign(_))
                    && error.to_string().contains("cannot use"),
                "{url}: {error}"
            );
        }
    }

    /// A service that answers the observation routes.
    fn observing_service(
        handler: impl Fn(&crate::opencode::mock::Request) -> crate::opencode::mock::Reply
            + Send
            + Sync
            + 'static,
    ) -> (crate::opencode::mock::Mock, tempfile::TempDir) {
        let mock = Mock::start(move |request| {
            if request.path == "/api/info" {
                return mock::reply(200, serde_json::json!({ "data": {} }));
            }
            handler(request)
        });
        let home = home_with_registration(&registration_for(mock.port(), "secret", "2.0.22"));
        (mock, home)
    }

    /// One session, in one directory, with the fields the panel reads.
    fn observed_session(id: &str, title: &str, directory: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "title": title,
            "time": { "created": 1, "updated": 2, "idle": 3 },
            "location": { "directory": directory },
            "agent": "build"
        })
    }

    #[test]
    fn the_service_wide_session_list_is_read_without_a_directory() {
        let directory = std::env::temp_dir().to_string_lossy().into_owned();
        let (mock, home) = observing_service(move |request| match request.path.as_str() {
            "/api/session" => mock::reply(
                200,
                serde_json::json!({ "data": [observed_session("ses_a", "work", &directory)] }),
            ),
            "/api/session/active" => mock::reply(200, serde_json::json!({ "data": {} })),
            _ => mock::reply(200, serde_json::json!({ "data": [] })),
        });

        let client = Client::connect_service(home.path()).unwrap();
        let sessions = client.service_sessions().unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title, "work");
        let listed = mock
            .requests()
            .into_iter()
            .find(|request| request.path == "/api/session")
            .expect("the session list was asked for");
        assert!(
            listed.query_param("directory").is_none(),
            "a session that moved is not listed by the directory it left"
        );
        assert!(listed.header("x-opencode-directory").is_none());
    }

    #[test]
    fn a_pending_list_that_cannot_be_read_leaves_the_session_unknown_rather_than_idle() {
        let directory = std::env::temp_dir().to_string_lossy().into_owned();
        let (mock, home) = observing_service(move |request| match request.path.as_str() {
            "/api/session" => mock::reply(
                200,
                serde_json::json!({ "data": [observed_session("ses_a", "work", &directory)] }),
            ),
            "/api/session/active" => mock::reply(200, serde_json::json!({ "data": {} })),
            "/api/form" => mock::reply(500, serde_json::json!({})),
            _ => mock::reply(200, serde_json::json!({ "data": [] })),
        });

        let client = Client::connect_service(home.path()).unwrap();
        let sessions = client.service_sessions().unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].awaiting_reply, None,
            "a question this app could not see is not a question that was not asked"
        );
        assert!(mock
            .requests()
            .iter()
            .any(|request| request.path == "/api/form"));
    }

    #[test]
    fn a_pending_request_names_the_session_that_is_waiting() {
        let directory = std::env::temp_dir().to_string_lossy().into_owned();
        let (_mock, home) = observing_service(move |request| match request.path.as_str() {
            "/api/session" => mock::reply(
                200,
                serde_json::json!({ "data": [observed_session("ses_a", "work", &directory)] }),
            ),
            "/api/session/active" => mock::reply(
                200,
                serde_json::json!({ "data": { "ses_a": { "type": "running" } } }),
            ),
            "/api/form" => mock::reply(
                200,
                serde_json::json!({ "data": [{ "sessionID": "ses_a" }] }),
            ),
            _ => mock::reply(200, serde_json::json!({ "data": [] })),
        });

        let client = Client::connect_service(home.path()).unwrap();
        let sessions = client.service_sessions().unwrap();

        assert!(sessions[0].running);
        assert_eq!(sessions[0].awaiting_reply, Some(true));
    }

    #[test]
    fn a_catalog_answered_for_another_directory_is_refused() {
        let elsewhere = std::env::temp_dir()
            .join("elsewhere")
            .to_string_lossy()
            .into_owned();
        let (_mock, home) = observing_service(move |_| {
            mock::reply(
                200,
                serde_json::json!({ "data": [], "location": { "directory": elsewhere } }),
            )
        });

        let client = Client::connect_service(home.path()).unwrap();
        let error = client
            .agents(&std::env::temp_dir().join("here"))
            .unwrap_err();

        assert!(matches!(error, DispatchError::Foreign(_)), "{error}");
    }

    #[test]
    fn a_registration_without_a_password_is_refused_before_it_is_probed() {
        let mock = Mock::start(|_| mock::reply(200, serde_json::json!({ "data": {} })));
        let home = home_with_registration(&registration_for(mock.port(), "", "2.0.22"));

        let error = discover(home.path()).unwrap_err();

        assert!(error.to_string().contains("no password"), "{error}");
        assert!(mock.requests().is_empty());
    }

    #[test]
    fn the_registration_path_follows_the_state_directory_when_one_is_set() {
        let home = tempfile::tempdir().unwrap();
        let state = home.path().join("state");

        let files = super::registration_files_in(home.path(), Some(state.as_os_str()));

        assert_eq!(
            files,
            vec![
                state.join(SERVICE_DIR).join(SERVICE_FILE),
                home.path()
                    .join(STATE_DIR_FROM_HOME)
                    .join(SERVICE_DIR)
                    .join(SERVICE_FILE),
            ]
        );
    }

    #[test]
    fn a_state_directory_that_is_unset_or_empty_leaves_only_the_home_one() {
        let home = tempfile::tempdir().unwrap();
        let expected = home
            .path()
            .join(STATE_DIR_FROM_HOME)
            .join(SERVICE_DIR)
            .join(SERVICE_FILE);

        for unset in [None, Some(OsStr::new(""))] {
            assert_eq!(
                super::registration_files_in(home.path(), unset),
                vec![expected.clone()],
                "{unset:?}"
            );
        }
    }

    #[test]
    fn the_registration_files_the_environment_chooses_always_include_the_home_one() {
        // The entry point still reads the environment, so whichever directory
        // this machine sets -- or does not -- the fallback the service registers
        // itself under is the last place discovery looks.
        let home = tempfile::tempdir().unwrap();

        assert_eq!(
            registration_files(home.path()).pop(),
            Some(
                home.path()
                    .join(STATE_DIR_FROM_HOME)
                    .join(SERVICE_DIR)
                    .join(SERVICE_FILE)
            )
        );
    }

    #[test]
    fn the_authorization_header_is_the_password_over_a_fixed_username() {
        assert_eq!(
            authorization("secret"),
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode("opencode:secret")
            )
        );
    }

    #[test]
    fn a_session_is_created_scoped_to_the_checkout_and_is_refused_from_another() {
        let checkout = checkout();
        let session_id = "ses_created";
        let directory = checkout.path.clone();
        let mock = Mock::start(move |request| {
            let answer = |data: serde_json::Value| {
                mock::reply(
                    200,
                    serde_json::json!({ "data": data, "location": { "directory": directory } }),
                )
            };
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/api/info") => mock::reply(200, serde_json::json!({ "data": {} })),
                ("POST", "/api/session") => {
                    // Echoing the header is what scopes the answer to the checkout.
                    answer(serde_json::json!({
                        "id": session_id,
                        "location": { "directory": request.header("x-opencode-directory").unwrap_or_default() },
                    }))
                }
                ("GET", "/api/session/ses_created") => {
                    answer(serde_json::json!({ "id": session_id }))
                }
                ("POST", "/api/session/ses_created/prompt") => answer(serde_json::json!({})),
                ("GET", "/api/session/ses_created/message") => {
                    answer(serde_json::json!([{ "text": "earlier" }]))
                }
                _ => mock::reply(404, serde_json::json!({})),
            }
        });
        let home = home_with_registration(&registration_for(mock.port(), "secret", "2.0.22"));

        let client = Client::connect(home.path(), Path::new(&checkout.path)).unwrap();
        assert_eq!(client.create_session("Muster review").unwrap(), session_id);
        client.prompt(session_id, "the review").unwrap();
        assert!(!client
            .session_mentions(session_id, "muster-review:x")
            .unwrap());

        let requests = mock.requests();
        let probe = 1;
        assert_eq!(requests[probe].method, "POST");
        assert_eq!(
            requests[probe].query_param("directory").as_deref(),
            Some(checkout.path.as_str())
        );
        assert_eq!(
            requests[probe].header("x-opencode-directory"),
            Some(checkout.path.as_str())
        );
        assert_eq!(
            requests[probe].header("authorization"),
            Some(authorization("secret").as_str())
        );
        assert_eq!(requests[probe].json()["title"], "Muster review");
        assert_eq!(
            requests[probe + 2].path,
            "/api/session/ses_created/prompt",
            "the session is resolved before the prompt is offered"
        );
        assert_eq!(requests[probe + 2].json()["text"], "the review");
    }

    #[test]
    fn a_session_answered_for_another_directory_is_refused() {
        let checkout = checkout();
        let mock = Mock::start(|_| {
            mock::reply(
                200,
                serde_json::json!({ "data": { "id": "ses_elsewhere" }, "location": { "directory": "/somewhere/else" } }),
            )
        });
        let home = home_with_registration(&registration_for(mock.port(), "secret", "2.0.22"));

        let client = Client::connect(home.path(), Path::new(&checkout.path)).unwrap();
        let error = client.create_session("Muster review").unwrap_err();

        assert!(matches!(error, DispatchError::Foreign(_)), "{error}");
        assert!(error.to_string().contains("different directory"), "{error}");
    }

    #[test]
    fn an_id_that_is_not_a_session_never_reaches_the_wire() {
        let mock = Mock::start(|_| mock::reply(200, serde_json::json!({ "data": {} })));
        let home = home_with_registration(&registration_for(mock.port(), "secret", "2.0.22"));
        let client = Client::connect(home.path(), &std::env::temp_dir()).unwrap();

        for id in ["", "ses_", "../api/session", "ses_a/b", "abc"] {
            assert!(client.resolve_session(id).is_err(), "{id} was accepted");
        }
        assert_eq!(
            mock.requests().len(),
            1,
            "only the discovery probe went out"
        );
    }

    #[test]
    fn an_oversized_review_is_refused_before_the_prompt_is_offered() {
        let mock =
            Mock::start(|_| mock::reply(200, serde_json::json!({ "data": { "id": "ses_big" } })));
        let home = home_with_registration(&registration_for(mock.port(), "secret", "2.0.22"));
        let client = Client::connect(home.path(), &std::env::temp_dir()).unwrap();

        let error = client
            .prompt(
                "ses_big",
                &"x".repeat(crate::review::MAX_ROUND_PROMPT_BYTES + 1),
            )
            .unwrap_err();

        assert!(matches!(error, DispatchError::Failed(_)), "{error}");
        assert!(
            !mock
                .requests()
                .iter()
                .any(|request| request.path.ends_with("/prompt")),
            "an oversized prompt was offered to the service"
        );
    }

    #[test]
    fn a_service_that_stops_answering_leaves_the_client_waiting_rather_than_failing() {
        let mock = Mock::start(|_| mock::reply(200, serde_json::json!({ "data": {} })));
        let home = home_with_registration(&registration_for(mock.port(), "secret", "2.0.22"));
        let client = Client::connect(home.path(), &std::env::temp_dir()).unwrap();
        // The person quit OpenCode; the client is still holding its registration.
        mock.stop();

        let error = client.create_session("Muster review").unwrap_err();

        // Nothing is listening, which is the one answer that says the request was never
        // written: whatever the caller was about to send, the service did not take it.
        assert!(error.never_sent(), "{error}");
        assert!(matches!(error, DispatchError::NotDelivered(_)), "{error}");
    }

    #[test]
    fn a_redirect_is_refused_rather_than_followed_off_the_machine() {
        let elsewhere = Mock::start(|_| mock::reply(200, serde_json::json!({ "data": {} })));
        let target = elsewhere.port();
        let mock =
            Mock::start(move |_| mock::redirect(format!("http://127.0.0.1:{target}/api/info")));
        let home = home_with_registration(&registration_for(mock.port(), "secret", "2.0.22"));

        let error = discover(home.path()).unwrap_err();

        // A `3xx` is not the service: the password was offered to the address the
        // registration named, and nothing was sent to the one it points at.
        assert!(error.to_string().contains("302"), "{error}");
        assert!(
            elsewhere.requests().is_empty(),
            "the redirect target must never be contacted"
        );
    }

    #[test]
    fn a_redirect_on_a_request_is_refused_rather_than_answered_from_another_address() {
        let elsewhere = Mock::start(|_| mock::reply(200, serde_json::json!({ "data": {} })));
        let target = elsewhere.port();
        let mock = Mock::start(move |request| {
            // The service is there, and everything past the probe answers with a redirect.
            if request.path == "/api/info" {
                mock::reply(200, serde_json::json!({ "data": {} }))
            } else {
                mock::redirect(format!("http://127.0.0.1:{target}{}", request.path))
            }
        });
        let home = home_with_registration(&registration_for(mock.port(), "secret", "2.0.22"));
        let client = Client::connect(home.path(), &std::env::temp_dir()).unwrap();

        let error = client.resolve_session("ses_review").unwrap_err();

        assert!(error.to_string().contains("302"), "{error}");
        assert!(
            elsewhere.requests().is_empty(),
            "the redirect target must never be contacted"
        );
    }

    #[test]
    fn a_registration_that_is_not_a_bounded_regular_file_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join(STATE_DIR_FROM_HOME).join(SERVICE_DIR);
        fs::create_dir_all(&directory).unwrap();
        let registration = directory.join(SERVICE_FILE);

        // Something else is standing where the registration is written.
        let elsewhere = home.path().join("elsewhere");
        fs::write(&elsewhere, b"untouched").unwrap();
        std::os::unix::fs::symlink(&elsewhere, &registration).unwrap();
        assert!(matches!(
            discover(home.path()).unwrap_err(),
            DispatchError::Foreign(_)
        ));

        // And a file past what a registration can be.
        fs::remove_file(&registration).unwrap();
        fs::write(&registration, vec![b'a'; 128 * 1024]).unwrap();
        assert!(matches!(
            discover(home.path()).unwrap_err(),
            DispatchError::Foreign(_)
        ));
    }
}
