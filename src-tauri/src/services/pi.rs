//! The Pi bridge: how an interactive Pi in a Muster terminal reports itself.
//!
//! OpenCode runs a server that answers questions about itself, so this app reads it. Pi has no
//! such server and no sidecar to read: the thing running in the terminal is a TUI a person is
//! typing into. What Pi does have is an extension API, and an extension inside that process can
//! see its own session and say so. This is the other end of that sentence — the loopback service
//! an extension reports to, and the registry of the sessions that have reported.
//!
//! Three properties are the whole of the design, and each one exists because of how the process
//! on the other side actually behaves.
//!
//! **It is one-way.** Everything here reads. Nothing in this file writes to Pi, because the only
//! thing Muster needs from an interactive Pi is to know what it is doing, and a Pi a person is
//! typing into must never be written to behind their back. That also means there is no command
//! transport to secure: a connection can at worst lie about a session that does not exist.
//!
//! **It is identified by the terminal, not by the process name.** A terminal's foreground program
//! is whatever the OS reports, and Pi is a Node program, so that name is `node` for every Pi and
//! for every other Node thing on the machine. It cannot say which terminal a session is in. What
//! can is the terminal itself: Muster mints a credential when it spawns a shell, the shell hands
//! it to everything it starts, and Pi inherits it. That value is this app's own identity, so the
//! one thing a session is matched by is a value Muster issued rather than a string the agent
//! wrote about itself.
//!
//! **It cannot slow Pi down.** Every failure a reporter can have — a refused frame, a dropped
//! socket, a version it does not speak — ends that reporter's connection and nothing else. There
//! is no retry loop an extension has to wait on and no handshake it can block the TUI in.

use std::collections::{HashMap, HashSet};
use std::io::BufRead;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::domain::agent::{AgentEvent, AgentEventKind, AgentProvider, AgentSession};
use crate::domain::review::is_review_round_marker;

/// Where normalized events go. Injected so the service does not depend on Tauri.
pub type EventSink = Arc<dyn Fn(AgentEvent) + Send + Sync>;

/// The wire version every frame carries.
///
/// A number rather than a shape test, because the two failure modes are different: a frame that
/// parses is not the same as a frame that means anything. A future build that changes the meaning
/// of a field bumps this, and this build refuses the frame rather than reading a field it no
/// longer understands.
pub const PROTOCOL_VERSION: u32 = 2;

/// The largest single frame, newline included.
///
/// Bounded because the reader is a loop that a process on the other side controls: without a cap,
/// a stream that never sends a newline is memory this app keeps allocating for someone who did not
/// ask it to. It is generous past anything a session identity needs, so it never truncates a
/// legitimate report.
pub const MAX_FRAME_BYTES: usize = 16 * 1024;

/// How long a connection has to introduce itself before it is dropped.
///
/// Only the handshake. After `hello` the connection is allowed to stay silent for as long as the
/// person leaves Pi alone, because a Pi sitting at an idle prompt sends nothing for minutes and a
/// read timeout applied there would disconnect every session that is merely waiting.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a session survives losing its connection.
///
/// Pi tears its extension runtime down on `/reload`, and the new runtime opens a new socket; the
/// gap between the two is short but real, and a row that vanished and came back would have been
/// reporting a flicker nobody observed. After this long with nothing on the other end, the
/// process is gone and the row is dropped at the next read rather than by a timer — see
/// [`PiBridge::sessions`].
const RECONNECT_GRACE: Duration = Duration::from_secs(30);

/// The environment variable carrying the bridge address and a terminal's own credential.
///
/// Absent when this app did not start the terminal, which is the case the extension checks first:
/// an extension installed for a person who also runs Pi outside Muster must find nothing to do
/// and leave that Pi alone.
pub const BRIDGE_ENV: &str = "MUSTER_PI_BRIDGE";

/// The terminal's own id, which is how a reporter says which terminal it is in.
///
/// Carried separately from the credential so the id on the wire can be checked against the id the
/// credential was issued for, rather than being believed.
pub const TERMINAL_ENV: &str = "MUSTER_TERMINAL_SESSION_ID";

/// The checkout the terminal belongs to, which a reporter cannot work out for itself.
///
/// It is handed down rather than derived: the reporter's own idea of where it is (a session file
/// path, a working directory) is something it reports about, and a value a process supplies about
/// itself is not evidence of what it is allowed to speak for.
pub const CHECKOUT_ENV: &str = "MUSTER_TERMINAL_CHECKOUT_ID";

/// What a terminal was issued, kept so a connection can be checked against what Muster minted
/// rather than against anything the connecting process says about itself.
#[derive(Clone)]
struct Credential {
    terminal_id: String,
    /// The checkout the terminal's environment was minted for, which a Pi keeps claiming after a move.
    checkout_id: String,
    /// The terminal's current checkout, changed without rotating that credential.
    owner_checkout_id: String,
}

/// What the extension says it currently is.
///
/// Whole snapshot rather than a delta: the reporter already holds every field, so sending it all
/// makes a repeated frame idempotent, and makes the reconnect case — the one that matters — restore
/// current state with no replay and no merge logic on this side.
///
/// The clock is deliberately absent. `idle_at` is stamped here, from a clock this app owns, because
/// it is a duration a row is drawn with and a reporter's idea of the time is not something the
/// sidebar may measure.
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    running: bool,
    #[serde(default)]
    awaiting: bool,
    /// How the last turn ended. Pi owns the distinction between these; this side only checks that
    /// it is one it has a reading for.
    #[serde(default)]
    outcome: Option<String>,
    /// The reviews this session was shown, as receipt identities. Absent when there are none,
    /// which is every session that was not sent one.
    #[serde(default)]
    receipts: Option<Vec<ReportedReceipt>>,
    /// Successful edit/write targets from the current Pi turn. Required by protocol v2.
    #[serde(default)]
    work: Option<ReportedWork>,
}

/// One receipt as it arrives.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReportedReceipt {
    #[serde(default)]
    marker: Option<String>,
    #[serde(default)]
    entry: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReportedWork {
    generation: String,
    sequence: u64,
    targets: Vec<String>,
    ambiguous: bool,
}

#[derive(Clone, Debug)]
pub struct PiWorkSession {
    pub session_id: String,
    pub checkout_id: String,
    pub generation: String,
    pub sequence: u64,
    pub targets: Vec<PathBuf>,
    pub ambiguous: bool,
}

#[derive(Clone)]
struct ActivePiSession {
    terminal_id: String,
    checkout_id: String,
    session_id: String,
    title: String,
    owner: u64,
    unavailable: bool,
}

/// The outcomes a turn can end in. A value outside this set is refused rather than stored: an
/// unknown string would reach a row that draws a colour from it.
const OUTCOMES: [&str; 3] = ["succeeded", "failed", "interrupted"];

/// How many receipts a snapshot may carry.
///
/// Bounded, and bounded for the same reason the frame is: the list is part of a whole snapshot
/// that has to fit [`MAX_FRAME_BYTES`], so a session with a long transcript cannot push the rest
/// of the report off the wire. Sixteen is far more reviews than one Pi is ever sent, because a
/// send opens a Pi rather than messaging a running one.
const MAX_RECEIPTS: usize = 16;

/// The longest a marker or an entry id may be, so a frame cannot spend its budget on one field.
const MAX_RECEIPT_FIELD_BYTES: usize = 256;
const MAX_WORK_TARGETS: usize = 16;
const MAX_WORK_PATH_BYTES: usize = 512;
const MAX_WORK_GENERATION_BYTES: usize = 64;

/// One receipt: a round's marker, and the id of the message inside Pi that carried it.
///
/// Two identities and no text. The marker names the round and the entry names the exact
/// transcript entry, which together pin a delivery to one message rather than to anything
/// that merely contains the marker's characters. Nothing a person wrote is ever sent over
/// this path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiReceipt {
    pub marker: String,
    pub entry: String,
}

/// What one reported session can prove about the rounds it was sent.
///
/// The terminal comes from the credential rather than from the frame, so everything read here
/// is already scoped to one terminal of one checkout, and a session of one checkout cannot be
/// asked about another's round.
pub struct PiDelivery {
    pub terminal_id: String,
    /// The checkout the original review round belongs to, retained when its terminal moves.
    pub round_checkout_id: String,
    pub receipts: Vec<PiReceipt>,
    /// Whether this session has been observed to settle a turn. See [`Live::settled`].
    pub settled: bool,
}

/// One reported session.
///
/// Keyed by terminal, which is the invariant the whole file rests on: a terminal is one terminal,
/// so a second session claiming it replaces the first rather than sitting beside it. That is what
/// lets a row be matched by identity and be refused when there is a contradiction to report.
struct Live {
    session_id: String,
    checkout_id: String,
    /// The review owner's checkout is immutable even when the terminal is rehomed.
    round_checkout_id: String,
    terminal_id: String,
    snapshot: Snapshot,
    created_at: i64,
    updated_at: i64,
    /// The reviews this session reported reading, newest first and bounded.
    ///
    /// Kept beside the snapshot rather than inside it because a snapshot is replaced whole and
    /// these must survive being absent from one: a reporter that reports its receipts on some
    /// frames and not others would otherwise un-confirm a delivery by saying nothing.
    receipts: Vec<PiReceipt>,
    /// Whether a turn has been observed to settle in this session.
    ///
    /// Set only where `agent_settled` is reported, and never on a lost connection or a row
    /// that stopped running for want of anyone asking. It survives a `/reload`, because that
    /// ends the runtime rather than the session.
    settled: bool,
    /// When this session was last touched by its own clock, which is the only clock a row draws.
    idle_at: Option<i64>,
    /// When the connection carrying it went away, kept so a reloading extension finds its row
    /// still here. `None` while it is connected.
    disconnected_at: Option<Instant>,
    /// Which connection is speaking for this row right now.
    ///
    /// A reload opens a second connection before the first one is necessarily gone, so without
    /// this the old socket's eventual close would mark the new session disconnected — a row
    /// losing its state to a connection that was replaced before it ever noticed. A frame from a
    /// connection that no longer owns the row is read and dropped instead.
    owner: u64,
}

impl Live {
    fn to_agent_session(&self) -> AgentSession {
        AgentSession {
            id: self.session_id.clone(),
            checkout_id: self.checkout_id.clone(),
            provider: AgentProvider::Pi,
            // The one field OpenCode can never fill, and the reason a Pi row is matched rather
            // than inferred.
            terminal_id: Some(self.terminal_id.clone()),
            title: self
                .snapshot
                .title
                .clone()
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| "Pi session".to_string()),
            idle_at: self.idle_at,
            // Absent rather than false while disconnected: nobody is answering, so whether a reply
            // is owed is not known. That is a different fact from "no reply is owed", and drawing
            // the second one would show a session as quiet when it was simply unreachable.
            awaiting_reply: self
                .disconnected_at
                .is_none()
                .then_some(self.snapshot.awaiting),
            // Pi has no agent catalog and no colors, so there is no name to report and a Pi row
            // is named by its own title instead.
            agent: None,
            model: self.snapshot.model.clone(),
            // Pi has no subagent parentage to report.
            parent_id: None,
            running: self.snapshot.running,
            outcome: self.snapshot.outcome.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// One line of the wire, whatever it turns out to be.
///
/// One shape for every frame because the only thing that decides what a frame means is its `type`,
/// and splitting this into a hello, a state and a bye before reading it would put three parsers in
/// front of a stream that is refused on one rule — the version — and one line length cap.
#[derive(Deserialize)]
struct Frame {
    #[serde(rename = "v")]
    version: Option<u32>,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    token: Option<String>,
    #[serde(rename = "terminalId")]
    terminal_id: Option<String>,
    #[serde(rename = "checkoutId")]
    checkout_id: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(flatten)]
    snapshot: Snapshot,
}

struct Listener {
    address: SocketAddr,
    handle: JoinHandle<()>,
}

struct Inner {
    credentials: Mutex<HashMap<String, Credential>>,
    sessions: Mutex<HashMap<String, Live>>,
    active: Mutex<HashMap<String, ActivePiSession>>,
    removing: Mutex<HashSet<String>>,
    sink: Mutex<Option<EventSink>>,
    listener: Mutex<Option<Listener>>,
    stopped: AtomicBool,
    /// Names the connection that owns a row. See [`Live::owner`].
    next_owner: AtomicU64,
}

impl Inner {
    /// Takes the lock through a poison rather than propagating it.
    ///
    /// A panic anywhere in a reporter thread must not take the whole registry with it: the sessions
    /// are still facts about processes that are still running.
    fn credentials(&self) -> MutexGuard<'_, HashMap<String, Credential>> {
        self.credentials
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn sessions(&self) -> MutexGuard<'_, HashMap<String, Live>> {
        self.sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn active(&self) -> MutexGuard<'_, HashMap<String, ActivePiSession>> {
        self.active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn removing(&self) -> MutexGuard<'_, HashSet<String>> {
        self.removing
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// Reports a normalized event, to whoever is listening, if anyone is.
    ///
    /// The frames name what happened rather than being mapped onto OpenCode's vocabulary wholesale:
    /// the two turn shapes are alike enough for the same three kinds, and a session appearing or
    /// leaving is not a turn event at all, so it is carried as its own type for the UI to ask
    /// about by name.
    fn emit(
        &self,
        checkout_id: &str,
        session_id: Option<&str>,
        kind: AgentEventKind,
        raw_type: &str,
    ) {
        let sink = self
            .sink
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let Some(sink) = sink {
            sink(AgentEvent {
                checkout_id: checkout_id.to_string(),
                session_id: session_id.map(str::to_string),
                kind,
                raw_type: raw_type.to_string(),
                data: serde_json::Value::Null,
            });
        }
    }
}

/// The service Pi extensions report to.
///
/// Held by [`crate::services::agent::AgentService`] so that both ends of this file's contract are
/// reachable from one place: a terminal that is being spawned asks it for a credential, and the
/// session read surfaces read from it.
pub struct PiBridge {
    inner: Arc<Inner>,
}

impl PiBridge {
    /// A bridge that is not listening yet.
    ///
    /// The listener is started when the first terminal asks for a credential rather than here,
    /// because most of this app's life has no Pi in it and an idle app should hold no socket.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(Inner {
                credentials: Mutex::new(HashMap::new()),
                sessions: Mutex::new(HashMap::new()),
                active: Mutex::new(HashMap::new()),
                removing: Mutex::new(HashSet::new()),
                sink: Mutex::new(None),
                listener: Mutex::new(None),
                stopped: AtomicBool::new(false),
                next_owner: AtomicU64::new(1),
            }),
        })
    }

    /// Sets where normalized events go.
    pub fn set_event_sink(&self, sink: EventSink) {
        *self
            .inner
            .sink
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(sink);
    }

    /// The address the bridge is listening on, once something has asked it to listen.
    pub fn address(&self) -> Option<SocketAddr> {
        self.inner
            .listener
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|listener| listener.address)
    }

    #[cfg(test)]
    pub(crate) fn credential_count(&self) -> usize {
        self.inner.credentials().len()
    }

    /// Mints the value a terminal's environment carries, and returns `None` when there is nowhere
    /// to point it.
    ///
    /// `None` is not a failure to report: a machine that cannot open a loopback socket still runs
    /// every terminal it can, and a terminal with no bridge value is simply one whose Pi is not
    /// observed.
    pub fn issue_credential(
        self: &Arc<Self>,
        terminal_id: &str,
        checkout_id: &str,
    ) -> Option<String> {
        let address = self.ensure_listener()?;
        let removing = self.inner.removing();
        if removing.contains(checkout_id) {
            return None;
        }
        let token = random_token()?;
        self.inner.credentials().insert(
            token.clone(),
            Credential {
                terminal_id: terminal_id.to_string(),
                checkout_id: checkout_id.to_string(),
                owner_checkout_id: checkout_id.to_string(),
            },
        );
        drop(removing);
        Some(format!("{address}:{token}"))
    }

    /// The sessions reported for one checkout.
    ///
    /// Expiry happens here rather than on a timer, and that is the point: a row for a process that
    /// is gone has to stop being drawn, and the only moment that can be settled without guessing is
    /// the moment something asks what is there. A reloading extension reconnects inside
    /// [`RECONNECT_GRACE`] and finds its row already in place.
    pub fn sessions(&self, checkout_id: &str) -> Vec<AgentSession> {
        let mut sessions = self.inner.sessions();
        expire(&mut sessions);
        sessions
            .values()
            .filter(|live| live.checkout_id == checkout_id)
            .map(Live::to_agent_session)
            .collect()
    }

    /// Activity and initial checkout identity used by shared worktree relocation.
    pub fn work_sessions(&self) -> Vec<PiWorkSession> {
        let mut sessions = self.inner.sessions();
        expire(&mut sessions);
        sessions
            .values()
            .filter_map(|live| {
                let work = live.snapshot.work.as_ref()?;
                Some(PiWorkSession {
                    session_id: live.session_id.clone(),
                    checkout_id: live.checkout_id.clone(),
                    generation: work.generation.clone(),
                    sequence: work.sequence,
                    targets: work.targets.iter().map(PathBuf::from).collect(),
                    ambiguous: work.ambiguous,
                })
            })
            .collect()
    }

    /// Pi sessions remain removal blockers while disconnected; only a reported exit or a closed
    /// terminal clears this fail-closed record.
    pub fn active_worktree_sessions(&self, checkout_id: &str) -> Vec<AgentSession> {
        self.inner
            .active()
            .values()
            .filter(|active| active.checkout_id == checkout_id)
            .map(|active| AgentSession {
                id: active.session_id.clone(),
                checkout_id: active.checkout_id.clone(),
                provider: AgentProvider::Pi,
                terminal_id: Some(active.terminal_id.clone()),
                title: format!(
                    "{} — terminal {}{}",
                    active.title,
                    active.terminal_id,
                    if active.unavailable {
                        " (status unknown)"
                    } else {
                        ""
                    }
                ),
                idle_at: None,
                awaiting_reply: (!active.unavailable).then_some(false),
                agent: None,
                model: None,
                parent_id: None,
                running: true,
                outcome: None,
                created_at: 0,
                updated_at: 0,
            })
            .collect()
    }

    pub(crate) fn reserve_checkout_removal(&self, checkout_id: &str) -> PiRemovalGuard {
        self.inner.removing().insert(checkout_id.to_string());
        PiRemovalGuard {
            inner: Arc::clone(&self.inner),
            checkout_id: checkout_id.to_string(),
        }
    }

    /// Rehomes a terminal's live Pi identity without changing the credential its process carries.
    pub fn rehome_terminal(
        &self,
        terminal_id: &str,
        source_checkout_id: &str,
        target_checkout_id: &str,
    ) {
        if source_checkout_id == target_checkout_id {
            return;
        }
        let removing = self.inner.removing();
        if removing.contains(source_checkout_id) || removing.contains(target_checkout_id) {
            return;
        }
        let mut credentials = self.inner.credentials();
        let mut rehomed = false;
        for credential in credentials.values_mut().filter(|item| {
            item.terminal_id == terminal_id && item.owner_checkout_id == source_checkout_id
        }) {
            credential.owner_checkout_id = target_checkout_id.to_string();
            rehomed = true;
        }
        if !rehomed {
            return;
        }
        let session_id = self.inner.sessions().get_mut(terminal_id).and_then(|live| {
            if live.checkout_id == source_checkout_id {
                live.checkout_id = target_checkout_id.to_string();
            }
            (live.checkout_id == target_checkout_id).then(|| live.session_id.clone())
        });
        if let Some(active) = self.inner.active().get_mut(terminal_id) {
            if active.checkout_id == source_checkout_id {
                active.checkout_id = target_checkout_id.to_string();
            }
        }
        drop(credentials);
        drop(removing);
        if let Some(session_id) = session_id {
            // To the destination checkout this session has just appeared, even though the Pi process
            // and its authenticated connection never restarted.
            self.inner.emit(
                target_checkout_id,
                Some(&session_id),
                AgentEventKind::Unknown,
                "pi.session_started",
            );
        }
    }

    /// Removes every reporter credential and row belonging to a terminal that has closed.
    pub fn forget_terminal(&self, terminal_id: &str) {
        let _removing = self.inner.removing();
        self.inner
            .credentials()
            .retain(|_, credential| credential.terminal_id != terminal_id);
        let removed = self.inner.sessions().remove(terminal_id);
        self.inner.active().remove(terminal_id);
        if let Some(live) = removed {
            self.inner.emit(
                &live.checkout_id,
                Some(&live.session_id),
                AgentEventKind::Unknown,
                "pi.session_ended",
            );
        }
    }

    /// What one reported session can prove about the rounds it was sent.
    ///
    /// `None` for a session this app has never had a `hello` from, which is the whole of what is
    /// unknown: no receipts means nothing was read, and no row means there is nothing to read
    /// from. Scoped to the checkout as well as the session, so a session of another checkout
    /// cannot be asked about this one's rounds even by a caller that names it.
    pub fn delivery(&self, checkout_id: &str, session_id: &str) -> Option<PiDelivery> {
        let mut sessions = self.inner.sessions();
        expire(&mut sessions);
        sessions
            .values()
            .find(|live| live.session_id == session_id && live.checkout_id == checkout_id)
            .map(|live| PiDelivery {
                terminal_id: live.terminal_id.clone(),
                round_checkout_id: live.round_checkout_id.clone(),
                receipts: live.receipts.clone(),
                settled: live.settled,
            })
    }

    /// Stops accepting and drops every reported session.
    pub fn stop(&self) {
        if self.inner.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        let listener = self
            .inner
            .listener
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(listener) = listener {
            // Wake the blocking accept so the thread can observe `stopped` and release its socket.
            let _ = TcpStream::connect(listener.address);
            let _ = listener.handle.join();
        }
        self.inner.sessions().clear();
        self.inner.active().clear();
        self.inner.credentials().clear();
    }

    /// Opens the listener the first time it is asked for.
    fn ensure_listener(self: &Arc<Self>) -> Option<SocketAddr> {
        let mut slot = self
            .inner
            .listener
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(listener) = slot.as_ref() {
            return Some(listener.address);
        }
        if self.inner.stopped.load(Ordering::SeqCst) {
            return None;
        }
        // Loopback, and an ephemeral port, so nothing is reachable from off the machine and two
        // copies of this app do not collide.
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).ok()?;
        let address = listener.local_addr().ok()?;
        let bridge = Arc::clone(self);
        let handle = std::thread::spawn(move || accept_forever(bridge, listener));
        *slot = Some(Listener { address, handle });
        Some(address)
    }
}

impl Drop for PiBridge {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) struct PiRemovalGuard {
    inner: Arc<Inner>,
    checkout_id: String,
}

impl Drop for PiRemovalGuard {
    fn drop(&mut self) {
        self.inner.removing().remove(&self.checkout_id);
    }
}

/// Accepts connections until the bridge stops.
fn accept_forever(bridge: Arc<PiBridge>, listener: TcpListener) {
    while !bridge.inner.stopped.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                if bridge.inner.stopped.load(Ordering::SeqCst) {
                    return;
                }
                let bridge = Arc::clone(&bridge);
                // One thread per reporter. These are a handful of terminals at most, they are
                // parked on a socket read for the life of a Pi process, and a shared pool would
                // mean one person's idle session could occupy the thread another's hello needs.
                let _ = std::thread::Builder::new()
                    .name("muster-pi-client".into())
                    .spawn(move || {
                        read_reporter(bridge, stream);
                    });
            }
            Err(_) => {
                if bridge.inner.stopped.load(Ordering::SeqCst) {
                    return;
                }
                // A refused accept is not a reason to spin: back off long enough that a machine
                // out of descriptors waits rather than burning the core that would free one.
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Reads one reporter for as long as it stays connected.
///
/// Every exit from here is an exit for that reporter alone. Nothing in this function can return an
/// error to anybody: the only thing it can affect is whether one session is currently described.
fn read_reporter(bridge: Arc<PiBridge>, stream: TcpStream) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(HELLO_TIMEOUT));
    let Ok(write_half) = stream.try_clone() else {
        return;
    };
    let mut frames = Frames::new(std::io::BufReader::new(stream));
    // The terminal this connection is speaking for, established once by the handshake and fixed
    // for the life of the connection: every later frame describes that session and nothing else,
    // so a frame cannot move a session from one terminal to another. The owner travels with it so
    // a connection that has been replaced cannot speak for the row that replaced it.
    let mut bound: Option<(String, u64)> = None;

    while !bridge.inner.stopped.load(Ordering::SeqCst) {
        let Some(frame) = frames.next() else {
            break;
        };
        let Ok(frame) = serde_json::from_slice::<Frame>(&frame) else {
            break;
        };
        if frame.version != Some(PROTOCOL_VERSION) {
            break;
        }
        match frame.kind.as_str() {
            "hello" => {
                let Some(claimed) = accept_hello(&bridge, &frame) else {
                    break;
                };
                // The handshake timeout has done its work. Left in place it would disconnect a Pi
                // that is simply sitting at an idle prompt — which is most of the time, and is not
                // a connection that has gone quiet in any sense worth refusing.
                //
                // Set through the cloned handle because the reader owns the original: the read
                // timeout is a property of the socket underneath, so the clone reaches it.
                let _ = write_half.set_read_timeout(None);
                bound = Some(claimed);
            }
            "state" => {
                let Some((terminal_id, owner)) = bound.clone() else {
                    break;
                };
                if frame.snapshot.work.is_none() {
                    break;
                }
                apply_state(&bridge, &terminal_id, owner, frame.snapshot);
            }
            "bye" => {
                if let Some((terminal_id, owner)) = bound.take() {
                    // A reload is not the end of the session, it is the end of the runtime that
                    // was reporting it, so the row waits for the one that replaces it. Everything
                    // else really did end the session.
                    if frame.reason.as_deref() != Some("reload") {
                        drop_session(&bridge, &terminal_id, owner);
                    } else {
                        mark_disconnected(&bridge, &terminal_id, owner);
                    }
                }
                let _ = write_half.shutdown(std::net::Shutdown::Both);
                return;
            }
            // A frame type this build does not know is a build older or newer than the reporter,
            // and reading past it is what keeps one added field from ending a session.
            _ => {}
        }
    }

    // The socket ended without a `bye`, which is a process that died, a reload whose replacement
    // has not arrived, or a version refused mid-stream. None of those is a session that finished,
    // so the row is left describing nothing new rather than being closed off as settled.
    if let Some((terminal_id, owner)) = bound {
        mark_disconnected(&bridge, &terminal_id, owner);
    }
}

/// Checks a handshake against what this app minted, and records the session it describes.
///
/// The terminal is resolved from the token rather than read from the frame, so the frame's own
/// claim is a check and not the answer: a connection holding a terminal's credential can report
/// for that terminal and is refused for any other.
///
/// Returns the terminal together with the owner this connection now holds, because the caller
/// carries both for the life of the connection and a frame that arrives without the matching
/// owner is a frame from a runtime that has already been replaced.
fn accept_hello(bridge: &PiBridge, frame: &Frame) -> Option<(String, u64)> {
    let token = frame.token.as_deref()?;
    let claimed_checkout = frame.checkout_id.as_deref()?;
    let removing = bridge.inner.removing();
    let credential = bridge.inner.credentials().get(token)?.clone();
    if frame.terminal_id.as_deref()? != credential.terminal_id
        || claimed_checkout != credential.checkout_id
        || removing.contains(&credential.owner_checkout_id)
        || frame.snapshot.work.is_none()
    {
        return None;
    }
    let checkout_id = credential.owner_checkout_id.clone();
    let session_id = frame.session_id.clone().filter(|id| !id.is_empty())?;
    let now = now_ms();
    let owner = bridge.inner.next_owner.fetch_add(1, Ordering::Relaxed);
    // A `/reload` introduces the same session again through a new runtime, and what the
    // session has already been told and has already settled are facts about the session rather
    // than about the connection that is introducing it.
    let mut sessions = bridge.inner.sessions();
    let carried = sessions
        .get(&credential.terminal_id)
        .filter(|live| live.session_id == session_id)
        .map(|live| {
            (
                live.created_at,
                live.settled,
                live.idle_at,
                live.snapshot.outcome.clone(),
                live.receipts.clone(),
            )
        });
    let (created_at, settled, idle_at, previous_outcome, previous_receipts) =
        carried.unwrap_or((now, false, None, None, Vec::new()));
    let mut snapshot = clean(frame.snapshot.clone());
    let receipts = std::mem::take(&mut snapshot.receipts)
        .map(clean_receipts)
        .filter(|receipts| !receipts.is_empty())
        .unwrap_or(previous_receipts);
    if snapshot.outcome.is_none() {
        snapshot.outcome = previous_outcome;
    }
    let title = snapshot
        .title
        .clone()
        .unwrap_or_else(|| "Pi session".to_string());
    sessions.insert(
        credential.terminal_id.clone(),
        Live {
            session_id: session_id.clone(),
            checkout_id: checkout_id.clone(),
            round_checkout_id: credential.checkout_id.clone(),
            terminal_id: credential.terminal_id.clone(),
            snapshot,
            created_at,
            updated_at: now,
            receipts,
            settled,
            idle_at,
            disconnected_at: None,
            owner,
        },
    );
    bridge.inner.active().insert(
        credential.terminal_id.clone(),
        ActivePiSession {
            terminal_id: credential.terminal_id.clone(),
            checkout_id: checkout_id.clone(),
            session_id: session_id.clone(),
            title,
            owner,
            unavailable: false,
        },
    );
    drop(sessions);
    drop(removing);
    bridge.inner.emit(
        &checkout_id,
        Some(&session_id),
        AgentEventKind::Unknown,
        "pi.session_started",
    );
    Some((credential.terminal_id, owner))
}

/// Records a reported state, and reports the turn boundaries it crosses.
///
/// The boundaries are transitions, not the frames themselves: Pi says a turn started and said one
/// settled, and the difference between "the first one arrived" and "the session was not already
/// running" is the difference between one event and a row that reports a turn starting for a turn
/// that was already under way.
fn apply_state(bridge: &PiBridge, terminal_id: &str, owner: u64, snapshot: Snapshot) {
    let mut snapshot = clean(snapshot);
    let receipts = std::mem::take(&mut snapshot.receipts)
        .map(clean_receipts)
        .unwrap_or_default();
    let now = now_ms();
    let mut sessions = bridge.inner.sessions();
    let Some(live) = sessions.get_mut(terminal_id) else {
        return;
    };
    if live.owner != owner {
        return;
    }
    let was_running = live.snapshot.running;
    if snapshot.running && !was_running {
        live.idle_at = None;
        bridge.inner.emit(
            &live.checkout_id,
            Some(&live.session_id),
            AgentEventKind::TurnStarted,
            "pi.agent_start",
        );
    }
    if !snapshot.running && was_running {
        // A turn that ended is stamped by this side's clock, and only when it actually went from
        // running to not: a connection lost is not a turn that finished, so it never stamps here.
        live.idle_at = Some(now);
        // The reporter only clears `running` on Pi's own `agent_settled`, so this transition is
        // that announcement and nothing else. `turn_end` and `agent_end` are boundaries Pi can
        // keep working after, and neither clears it.
        live.settled = true;
        let failed = matches!(snapshot.outcome.as_deref(), Some("failed" | "interrupted"));
        bridge.inner.emit(
            &live.checkout_id,
            Some(&live.session_id),
            if failed {
                AgentEventKind::TurnFailed
            } else {
                AgentEventKind::TurnFinished
            },
            "pi.agent_settled",
        );
    }
    if snapshot.awaiting && !live.snapshot.awaiting {
        bridge.inner.emit(
            &live.checkout_id,
            Some(&live.session_id),
            AgentEventKind::QuestionAsked,
            "pi.ui_prompt_start",
        );
    }
    if !snapshot.awaiting && live.snapshot.awaiting {
        bridge.inner.emit(
            &live.checkout_id,
            Some(&live.session_id),
            AgentEventKind::Unknown,
            "pi.ui_prompt_end",
        );
    }
    // Replaced whole, like the rest of the snapshot, but only by a frame that carries receipts:
    // a reporter is free to leave the field off a frame, and treating that as an empty list would
    // un-confirm a delivery by saying nothing about it.
    if !receipts.is_empty() {
        live.receipts = receipts;
    }
    let incoming_work = snapshot.work.take().expect("protocol v2 state has work");
    let existing_work = live
        .snapshot
        .work
        .as_ref()
        .expect("accepted hello has work");
    if incoming_work.generation == existing_work.generation
        && incoming_work.sequence <= existing_work.sequence
    {
        snapshot.work = Some(existing_work.clone());
    } else {
        snapshot.work = Some(incoming_work);
    }
    live.snapshot = snapshot;
    live.updated_at = now;
    live.disconnected_at = None;
    if let Some(active) = bridge.inner.active().get_mut(terminal_id) {
        if active.owner == owner {
            if let Some(title) = live.snapshot.title.as_ref() {
                active.title = title.clone();
            }
            active.unavailable = false;
        }
    }
}

/// A session whose connection ended without saying goodbye.
///
/// The row stays, describing what was last observed, and nothing is stamped: a connection that
/// dropped is not a turn that finished, and reporting it as one is exactly how a row comes to say
/// "idle" about a process nobody can reach.
fn mark_disconnected(bridge: &PiBridge, terminal_id: &str, owner: u64) {
    let mut sessions = bridge.inner.sessions();
    if let Some(live) = sessions.get_mut(terminal_id) {
        if live.owner == owner {
            live.disconnected_at = Some(Instant::now());
        }
    }
    if let Some(active) = bridge.inner.active().get_mut(terminal_id) {
        if active.owner == owner {
            active.unavailable = true;
        }
    }
}

/// Removes a session that really has ended.
fn drop_session(bridge: &PiBridge, terminal_id: &str, owner: u64) {
    let mut sessions = bridge.inner.sessions();
    // Only the connection that owns the row may end it: a runtime replaced by a reload still runs
    // its shutdown, and one that is no longer speaking must not delete the session that replaced it.
    if sessions
        .get(terminal_id)
        .is_some_and(|live| live.owner != owner)
        || bridge
            .inner
            .active()
            .get(terminal_id)
            .is_some_and(|active| active.owner != owner)
    {
        return;
    }
    let removed = sessions.remove(terminal_id);
    bridge.inner.active().remove(terminal_id);
    if let Some(live) = removed {
        bridge.inner.emit(
            &live.checkout_id,
            Some(&live.session_id),
            AgentEventKind::Unknown,
            "pi.session_ended",
        );
    }
}

/// Keeps only what a row has a reading for.
fn clean(snapshot: Snapshot) -> Snapshot {
    Snapshot {
        outcome: snapshot
            .outcome
            .filter(|outcome| OUTCOMES.contains(&outcome.as_str())),
        title: snapshot
            .title
            .map(|title| title.trim().to_string())
            .filter(|title| !title.is_empty()),
        work: snapshot.work.map(clean_work),
        ..snapshot
    }
}

/// Keeps only bounded absolute paths and marks an untrustworthy target set ambiguous.
fn clean_work(mut work: ReportedWork) -> ReportedWork {
    let mut targets = Vec::new();
    let invalid = work.generation.is_empty()
        || work.generation.len() > MAX_WORK_GENERATION_BYTES
        || work.targets.len() > MAX_WORK_TARGETS
        || work
            .targets
            .iter()
            .any(|target| target.len() > MAX_WORK_PATH_BYTES || !Path::new(target).is_absolute());
    if invalid || work.ambiguous {
        work.targets.clear();
        work.ambiguous = true;
        return work;
    }
    for target in work.targets {
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    work.targets = targets;
    work
}

/// Drops rows whose process is gone for longer than [`RECONNECT_GRACE`].
///
/// Shared by every read rather than run on a timer, because the only moment a row can be
/// retired without guessing is the moment something asks what is there.
fn expire(sessions: &mut HashMap<String, Live>) {
    let expired: Vec<String> = sessions
        .iter()
        .filter(|(_, live)| {
            live.disconnected_at
                .is_some_and(|gone| gone.elapsed() > RECONNECT_GRACE)
        })
        .map(|(terminal, _)| terminal.clone())
        .collect();
    for terminal in expired {
        sessions.remove(&terminal);
    }
}

/// Keeps only the receipts that are a round's marker and an entry id, newest first.
///
/// Every part is checked rather than trimmed: a marker this build did not write is not a round,
/// and an entry with no id cannot pin anything. Dropped receipts are dropped for good rather
/// than kept in reserve, because the one that matters is the one a send is waiting on.
fn clean_receipts(reported: Vec<ReportedReceipt>) -> Vec<PiReceipt> {
    let mut receipts: Vec<PiReceipt> = Vec::new();
    for receipt in reported {
        let (Some(marker), Some(entry)) = (receipt.marker, receipt.entry) else {
            continue;
        };
        if marker.len() > MAX_RECEIPT_FIELD_BYTES
            || entry.is_empty()
            || entry.len() > MAX_RECEIPT_FIELD_BYTES
            || !is_review_round_marker(&marker)
            || receipts
                .iter()
                .any(|kept: &PiReceipt| kept.marker == marker && kept.entry == entry)
        {
            continue;
        }
        receipts.push(PiReceipt { marker, entry });
        if receipts.len() == MAX_RECEIPTS {
            break;
        }
    }
    receipts
}

/// Newline-delimited frames, bounded, over a buffered reader.
///
/// The buffer belongs to the connection rather than to a frame, so two frames arriving in one
/// segment are both read; the cap is checked against what has been accumulated rather than trusted
/// to the reader, which is what makes an endless line without a newline a refused connection
/// instead of an allocation this app keeps making.
struct Frames<R: BufRead> {
    reader: R,
}

impl<R: BufRead> Frames<R> {
    fn new(reader: R) -> Self {
        Self { reader }
    }

    /// The next frame without its newline, or `None` for end of stream or a refused length.
    ///
    /// A loop rather than a loop-with-recursion because blank lines are legal on this wire and a
    /// stream of them would otherwise put one stack frame per line on a thread this app does not
    /// own the depth of.
    fn next(&mut self) -> Option<Vec<u8>> {
        loop {
            let mut line = Vec::new();
            loop {
                let available = self.reader.fill_buf().ok()?;
                if available.is_empty() {
                    return None;
                }
                match available.iter().position(|byte| *byte == b'\n') {
                    Some(at) => {
                        line.extend_from_slice(&available[..=at]);
                        self.reader.consume(at + 1);
                        break;
                    }
                    None => {
                        line.extend_from_slice(available);
                        let read = available.len();
                        self.reader.consume(read);
                        if line.len() > MAX_FRAME_BYTES {
                            return None;
                        }
                    }
                }
            }
            if line.len() > MAX_FRAME_BYTES {
                return None;
            }
            while line
                .last()
                .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
            {
                line.pop();
            }
            if !line.is_empty() {
                return Some(line);
            }
        }
    }
}

/// A credential this app issues and nothing else can be.
///
/// Refuses rather than falls back: a token that could not be random would still be a credential,
/// and the failure it hides is the failure everything here is built to prevent.
fn random_token() -> Option<String> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).ok()?;
    Some(
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
    )
}

/// The one clock this file stamps with, in Unix milliseconds.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Read, Write};
    use std::net::TcpStream;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::{
        Frames, PiBridge, HELLO_TIMEOUT, MAX_FRAME_BYTES, MAX_RECEIPTS, OUTCOMES, PROTOCOL_VERSION,
    };

    /// A reporter that speaks the wire protocol from the outside, so these tests exercise the same
    /// bytes the shipped extension writes rather than this module's own idea of them.
    struct Reporter {
        stream: TcpStream,
    }

    impl Reporter {
        fn connect(bridge: &PiBridge) -> Self {
            let address = bridge.address().expect("the bridge is listening");
            let mut last = None;
            for _ in 0..50 {
                match TcpStream::connect(address) {
                    Ok(stream) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                        return Self { stream };
                    }
                    Err(error) => {
                        last = Some(error);
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
            }
            panic!("could not reach the bridge: {last:?}");
        }

        fn send(&mut self, frame: serde_json::Value) {
            self.stream
                .write_all(format!("{frame}\n").as_bytes())
                .unwrap();
            let _ = self.stream.flush();
        }

        fn send_raw(&mut self, bytes: &[u8]) {
            self.stream.write_all(bytes).unwrap();
            let _ = self.stream.flush();
        }
    }

    fn bridge_with_terminal() -> (std::sync::Arc<PiBridge>, String, String) {
        let bridge = PiBridge::new();
        let credential = bridge
            .issue_credential("terminal:one", "checkout:one")
            .expect("a credential");
        let token = credential.rsplit(':').next().unwrap().to_string();
        (bridge, credential, token)
    }

    fn hello(
        token: &str,
        terminal_id: &str,
        checkout_id: &str,
        session_id: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "v": PROTOCOL_VERSION,
            "type": "hello",
            "token": token,
            "terminalId": terminal_id,
            "checkoutId": checkout_id,
            "sessionId": session_id,
            "work": {
                "generation": "test-generation",
                "sequence": 0,
                "targets": [],
                "ambiguous": false,
            },
        })
    }

    fn state(running: bool, awaiting: bool, outcome: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "v": PROTOCOL_VERSION,
            "type": "state",
            "running": running,
            "awaiting": awaiting,
            "outcome": outcome,
            "work": {
                "generation": "test-generation",
                "sequence": 0,
                "targets": [],
                "ambiguous": false,
            },
        })
    }

    fn state_with_work(sequence: u64, targets: &[&str]) -> serde_json::Value {
        let mut frame = state(false, false, None);
        frame["work"] = serde_json::json!({
            "generation": "test-generation",
            "sequence": sequence,
            "targets": targets,
            "ambiguous": false,
        });
        frame
    }

    /// Waits for a condition the reporting thread is expected to reach, rather than sleeping a
    /// fixed time: these assertions are about what eventually happened, not about how fast.
    fn eventually(what: &str, check: impl Fn() -> bool) {
        for _ in 0..200 {
            if check() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("{what} did not happen");
    }

    #[test]
    fn a_reported_session_is_read_as_a_pi_session_named_by_its_terminal() {
        let (bridge, _credential, token) = bridge_with_terminal();
        let mut reporter = Reporter::connect(&bridge);
        reporter.send(hello(&token, "terminal:one", "checkout:one", "pi-session"));
        reporter.send(state(false, false, None));

        eventually("the session was recorded", || {
            bridge.sessions("checkout:one").len() == 1
        });
        let sessions = bridge.sessions("checkout:one");
        let session = &sessions[0];
        assert_eq!(session.id, "pi-session");
        assert_eq!(session.provider, crate::domain::agent::AgentProvider::Pi);
        // The whole reason this file exists: identity, not a title someone wrote.
        assert_eq!(session.terminal_id.as_deref(), Some("terminal:one"));
        // Pi has no catalog and no subagent parentage, so both stay absent rather than borrowing
        // OpenCode's answers.
        assert_eq!(session.agent, None);
        assert_eq!(session.parent_id, None);
        assert_eq!(session.awaiting_reply, Some(false));
        assert_eq!(bridge.sessions("checkout:two").len(), 0);
    }

    #[test]
    fn a_moved_terminal_rehomes_pi_without_rotating_its_credential_or_review_owner() {
        let (bridge, _credential, token) = bridge_with_terminal();
        let mut reporter = Reporter::connect(&bridge);
        reporter.send(hello(&token, "terminal:one", "checkout:one", "pi-session"));
        eventually("the session was recorded", || {
            bridge.sessions("checkout:one").len() == 1
        });

        bridge.rehome_terminal("terminal:one", "checkout:one", "checkout:two");
        assert!(bridge.sessions("checkout:one").is_empty());
        assert_eq!(bridge.sessions("checkout:two")[0].id, "pi-session");
        assert!(bridge.active_worktree_sessions("checkout:one").is_empty());
        assert_eq!(
            bridge.active_worktree_sessions("checkout:two")[0].id,
            "pi-session"
        );
        assert_eq!(
            bridge
                .delivery("checkout:two", "pi-session")
                .unwrap()
                .round_checkout_id,
            "checkout:one",
            "the round remains bound to the checkout it was launched from"
        );

        drop(reporter);
        let mut reconnect = Reporter::connect(&bridge);
        // Pi's extension continues to claim the checkout in its original environment. The token is
        // still authenticated against that claim, while the bridge restores its current owner.
        reconnect.send(hello(&token, "terminal:one", "checkout:one", "pi-session"));
        eventually("the moved session reconnected", || {
            bridge.sessions("checkout:two").len() == 1
        });
        assert!(bridge.sessions("checkout:one").is_empty());
        assert_eq!(
            bridge.credential_count(),
            1,
            "moving did not rotate or duplicate the credential"
        );
    }

    #[test]
    fn successful_work_targets_are_kept_and_disconnected_sessions_remain_removal_blockers() {
        let (bridge, _credential, token) = bridge_with_terminal();
        let mut reporter = Reporter::connect(&bridge);
        reporter.send(hello(&token, "terminal:one", "checkout:one", "pi-session"));
        reporter.send(state_with_work(1, &["/worktree/file.ts"]));

        eventually("the work target was recorded", || {
            bridge
                .work_sessions()
                .iter()
                .any(|session| session.targets == [PathBuf::from("/worktree/file.ts")])
        });
        drop(reporter);
        eventually("the active session became unavailable", || {
            bridge.active_worktree_sessions("checkout:one")[0]
                .title
                .contains("status unknown")
        });

        bridge.forget_terminal("terminal:one");
        assert!(bridge.active_worktree_sessions("checkout:one").is_empty());
        assert!(bridge.work_sessions().is_empty());
    }

    #[test]
    fn protocol_v2_requires_work_on_hello() {
        let (bridge, _credential, token) = bridge_with_terminal();
        let mut frame = hello(&token, "terminal:one", "checkout:one", "pi-session");
        frame.as_object_mut().unwrap().remove("work");
        let mut reporter = Reporter::connect(&bridge);
        reporter.send(frame);

        std::thread::sleep(Duration::from_millis(100));
        assert!(bridge.sessions("checkout:one").is_empty());
    }

    #[test]
    fn a_credential_only_speaks_for_the_terminal_it_was_issued_to() {
        let (bridge, _credential, token) = bridge_with_terminal();
        let mut reporter = Reporter::connect(&bridge);
        // The frame names a different terminal than the one holding the token, which is the whole
        // impersonation this refuses.
        reporter.send(hello(
            &token,
            "terminal:other",
            "checkout:one",
            "pi-session",
        ));
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            bridge.sessions("checkout:one").is_empty(),
            "a credential reported for a terminal it was not issued to"
        );

        // And the checkout it was issued for is equally fixed.
        let mut other = Reporter::connect(&bridge);
        other.send(hello(
            &token,
            "terminal:one",
            "checkout:other",
            "pi-session",
        ));
        std::thread::sleep(Duration::from_millis(100));
        assert!(bridge.sessions("checkout:one").is_empty());
    }

    #[test]
    fn a_frame_this_build_does_not_speak_is_refused_rather_than_read() {
        for version in [Some(PROTOCOL_VERSION + 1), Some(0), None] {
            let (bridge, _credential, token) = bridge_with_terminal();
            let mut reporter = Reporter::connect(&bridge);
            let mut frame = hello(&token, "terminal:one", "checkout:one", "pi-session");
            frame["v"] = version.into();
            reporter.send(frame);
            std::thread::sleep(Duration::from_millis(100));
            assert!(
                bridge.sessions("checkout:one").is_empty(),
                "a frame at version {version:?} was read"
            );
        }
    }

    #[test]
    fn a_frame_larger_than_the_cap_ends_the_connection_instead_of_the_process() {
        let (bridge, _credential, token) = bridge_with_terminal();
        let mut reporter = Reporter::connect(&bridge);
        reporter.send(hello(&token, "terminal:one", "checkout:one", "pi-session"));
        eventually("the session was recorded", || {
            bridge.sessions("checkout:one").len() == 1
        });
        // No newline, and more than the cap allows: a stream that keeps going must cost the
        // connection, not the memory of everything it has sent.
        let mut oversized = vec![b' '; MAX_FRAME_BYTES + 64];
        oversized.extend_from_slice(b"{\"v\":1,\"type\":\"state\"}");
        reporter.send_raw(&oversized);

        eventually("the connection was dropped", || {
            // The row is kept, because a dropped connection is not a finished session, so what is
            // observable is that no later state is ever applied again.
            let sessions = bridge.sessions("checkout:one");
            sessions.len() == 1 && sessions[0].awaiting_reply.is_none()
        });
    }

    #[test]
    fn two_frames_in_one_write_are_both_read() {
        let (bridge, _credential, token) = bridge_with_terminal();
        let mut reporter = Reporter::connect(&bridge);
        // The reader buffers across the connection, so a reporter that writes two frames at once
        // is not silently losing the second one.
        reporter.send_raw(
            format!(
                "{}\n{}\n",
                hello(&token, "terminal:one", "checkout:one", "pi-session"),
                state(true, false, None)
            )
            .as_bytes(),
        );
        eventually("the running turn was recorded", || {
            bridge
                .sessions("checkout:one")
                .first()
                .is_some_and(|session| session.running)
        });
    }

    #[test]
    fn a_turn_is_stamped_when_it_settles_and_never_when_the_connection_drops() {
        let (bridge, _credential, token) = bridge_with_terminal();
        let mut reporter = Reporter::connect(&bridge);
        reporter.send(hello(&token, "terminal:one", "checkout:one", "pi-session"));
        reporter.send(state(true, false, None));
        eventually("the turn started", || {
            bridge
                .sessions("checkout:one")
                .first()
                .is_some_and(|session| session.running)
        });

        reporter.send(state(false, false, Some("succeeded")));
        eventually("the turn settled", || {
            bridge
                .sessions("checkout:one")
                .first()
                .is_some_and(|session| session.idle_at.is_some())
        });

        // Now the same sequence cut off mid-turn. Nothing about that is a turn that finished, so
        // the row must not acquire an idle stamp or an outcome.
        let mut second = Reporter::connect(&bridge);
        second.send(hello(&token, "terminal:one", "checkout:one", "pi-session"));
        second.send(state(true, false, None));
        eventually("the second turn started", || {
            bridge
                .sessions("checkout:one")
                .first()
                .is_some_and(|session| session.running)
        });
        drop(second);
        eventually("the connection was noticed as gone", || {
            bridge
                .sessions("checkout:one")
                .first()
                .is_some_and(|session| session.awaiting_reply.is_none())
        });
        let session = &bridge.sessions("checkout:one")[0];
        assert_eq!(
            session.idle_at, None,
            "a lost connection was reported as an idle turn"
        );
        assert_eq!(session.outcome, None);
    }

    #[test]
    fn a_reconnect_restores_the_current_state_and_an_outcome_muster_cannot_read_is_refused() {
        let (bridge, _credential, token) = bridge_with_terminal();
        let mut reporter = Reporter::connect(&bridge);
        reporter.send(hello(&token, "terminal:one", "checkout:one", "pi-session"));
        reporter.send(state(true, false, None));
        eventually("the turn started", || {
            bridge
                .sessions("checkout:one")
                .first()
                .is_some_and(|session| session.running)
        });
        // The connection drops mid-turn, which is what a reload looks like from here.
        drop(reporter);

        // The replacement reports the state it actually has rather than replaying anything.
        let mut reloaded = Reporter::connect(&bridge);
        reloaded.send(hello(&token, "terminal:one", "checkout:one", "pi-session"));
        reloaded.send(state(true, false, Some("not-an-outcome")));
        eventually("the state was restored", || {
            bridge
                .sessions("checkout:one")
                .first()
                .is_some_and(|session| {
                    // Reconnected, so the row is answerable again, and the turn is still the one that
                    // was already running — neither half is something the reload may have lost.
                    session.awaiting_reply.is_some() && session.running
                })
        });
        assert_eq!(
            bridge.sessions("checkout:one")[0].outcome,
            None,
            "an outcome this build has no reading for was stored"
        );
        for outcome in OUTCOMES {
            let mut frame = state(false, false, Some(outcome));
            frame["type"] = serde_json::json!("state");
            reloaded.send(frame);
        }
        eventually("a readable outcome was stored", || {
            bridge
                .sessions("checkout:one")
                .first()
                .is_some_and(|session| session.outcome.as_deref() == Some("interrupted"))
        });
    }

    #[test]
    fn a_reload_keeps_the_row_and_a_quit_removes_it() {
        let (bridge, _credential, token) = bridge_with_terminal();

        let mut first = Reporter::connect(&bridge);
        first.send(hello(&token, "terminal:one", "checkout:one", "pi-session"));
        eventually("the session was recorded", || {
            bridge.sessions("checkout:one").len() == 1
        });
        first.send(serde_json::json!({
            "v": PROTOCOL_VERSION,
            "type": "bye",
            "reason": "reload",
        }));
        // A reload ends the runtime, not the session: the row waits for the runtime replacing it.
        eventually("the row waited for its replacement", || {
            bridge
                .sessions("checkout:one")
                .first()
                .is_some_and(|session| session.awaiting_reply.is_none())
        });
        assert_eq!(bridge.sessions("checkout:one").len(), 1);

        let mut second = Reporter::connect(&bridge);
        second.send(hello(&token, "terminal:one", "checkout:one", "pi-session"));
        eventually("the replacement took the row over", || {
            bridge
                .sessions("checkout:one")
                .first()
                .is_some_and(|session| session.awaiting_reply.is_some())
        });
        second.send(serde_json::json!({
            "v": PROTOCOL_VERSION,
            "type": "bye",
            "reason": "quit",
        }));
        eventually("the session was removed", || {
            bridge.sessions("checkout:one").is_empty()
        });
    }

    #[test]
    fn a_session_that_reports_without_introducing_itself_is_dropped() {
        let (bridge, _credential, _token) = bridge_with_terminal();
        let mut reporter = Reporter::connect(&bridge);
        // No handshake, straight to a state: there is no terminal to attach it to.
        reporter.send(state(true, false, None));
        std::thread::sleep(Duration::from_millis(100));
        assert!(bridge.sessions("checkout:one").is_empty());
    }

    #[test]
    fn a_connection_that_never_introduces_itself_is_closed() {
        let (bridge, _credential, _token) = bridge_with_terminal();
        let mut silent = TcpStream::connect(bridge.address().unwrap()).unwrap();
        silent
            .set_read_timeout(Some(HELLO_TIMEOUT + Duration::from_secs(2)))
            .unwrap();
        // Nothing is ever written, so the read only ends when the bridge gives up on it.
        let mut answer = [0u8; 1];
        assert!(silent.read(&mut answer).is_ok());
        assert!(bridge.sessions("checkout:one").is_empty());
    }

    #[test]
    fn a_reporter_cannot_move_a_session_between_terminals() {
        let bridge = PiBridge::new();
        let first = bridge
            .issue_credential("terminal:one", "checkout:one")
            .unwrap();
        let second = bridge
            .issue_credential("terminal:two", "checkout:one")
            .unwrap();
        let first_token = first.rsplit(':').next().unwrap().to_string();
        let second_token = second.rsplit(':').next().unwrap().to_string();

        let mut reporter = Reporter::connect(&bridge);
        reporter.send(hello(
            &first_token,
            "terminal:one",
            "checkout:one",
            "pi-session",
        ));
        eventually("the session was recorded", || {
            bridge.sessions("checkout:one").len() == 1
        });
        // A later state frame carries no identity at all: the connection is the identity, so
        // there is no field here that could point the session somewhere else.
        reporter.send_raw(
            format!(
                "{}\n",
                serde_json::json!({
                    "v": PROTOCOL_VERSION,
                    "type": "state",
                    "terminalId": "terminal:two",
                    "checkoutId": "checkout:one",
                    "sessionId": "pi-session",
                })
            )
            .as_bytes(),
        );
        eventually("the foreign identity was ignored", || {
            bridge.sessions("checkout:one").len() == 1
        });
        assert_eq!(
            bridge.sessions("checkout:one")[0].terminal_id.as_deref(),
            Some("terminal:one")
        );
        assert_ne!(first_token, second_token);
    }

    #[test]
    fn a_runtime_replaced_by_a_reload_cannot_end_the_session_that_replaced_it() {
        let (bridge, _credential, token) = bridge_with_terminal();
        // The outgoing runtime, still connected because its socket has not been noticed as closed.
        let mut outgoing = Reporter::connect(&bridge);
        outgoing.send(hello(&token, "terminal:one", "checkout:one", "pi-session"));
        eventually("the session was recorded", || {
            bridge.sessions("checkout:one").len() == 1
        });

        // The incoming one takes the row over.
        let mut incoming = Reporter::connect(&bridge);
        incoming.send(hello(&token, "terminal:one", "checkout:one", "pi-reloaded"));
        incoming.send(state(true, false, None));
        eventually("the replacement took over", || {
            bridge
                .sessions("checkout:one")
                .first()
                .is_some_and(|session| session.id == "pi-reloaded" && session.running)
        });

        // The outgoing runtime now shuts down and its socket ends. Neither half of that may touch
        // the session that replaced it: deleting it would lose a running turn, and marking it
        // disconnected would blank the very state the reload was meant to restore.
        outgoing.send(serde_json::json!({
            "v": PROTOCOL_VERSION,
            "type": "bye",
            "reason": "reload",
        }));
        drop(outgoing);
        std::thread::sleep(Duration::from_millis(200));
        let session = &bridge.sessions("checkout:one")[0];
        assert_eq!(session.id, "pi-reloaded");
        assert!(session.running);
        assert_eq!(
            session.awaiting_reply,
            Some(false),
            "a replaced runtime's shutdown was reported as the session losing its connection"
        );
    }

    #[test]
    fn a_session_left_alone_is_not_disconnected_for_being_idle() {
        let (bridge, _credential, token) = bridge_with_terminal();
        let mut reporter = Reporter::connect(&bridge);
        reporter.send(hello(&token, "terminal:one", "checkout:one", "pi-session"));
        eventually("the session was recorded", || {
            bridge.sessions("checkout:one").len() == 1
        });

        // A Pi at an idle prompt sends nothing, for minutes at a time. The handshake has a timeout
        // because a connection that never introduces itself is not a session; if that timeout were
        // left in place it would close a session for sitting still, which is most of its life.
        std::thread::sleep(HELLO_TIMEOUT + Duration::from_millis(300));
        let session = &bridge.sessions("checkout:one")[0];
        assert_eq!(
            session.awaiting_reply,
            Some(false),
            "a session that said nothing was treated as disconnected"
        );

        // And it is still answerable, which is the only thing the timeout could have broken.
        reporter.send(state(true, false, None));
        eventually("the session still reports", || {
            bridge
                .sessions("checkout:one")
                .first()
                .is_some_and(|session| session.running)
        });
    }

    #[test]
    fn events_reach_the_sink_the_app_registered() {
        let (bridge, _credential, token) = bridge_with_terminal();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let collector = Arc::clone(&seen);
        bridge.set_event_sink(Arc::new(move |event| {
            collector
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(event);
        }));

        let mut reporter = Reporter::connect(&bridge);
        reporter.send(hello(&token, "terminal:one", "checkout:one", "pi-session"));
        reporter.send(state(true, false, None));
        reporter.send(state(false, false, Some("failed")));
        reporter.send(serde_json::json!({
            "v": PROTOCOL_VERSION,
            "type": "bye",
            "reason": "quit",
        }));

        eventually("the session lifecycle was reported", || {
            let events = seen.lock().unwrap();
            let kinds: Vec<&str> = events.iter().map(|event| event.raw_type.as_str()).collect();
            kinds.contains(&"pi.agent_start")
                && kinds.contains(&"pi.agent_settled")
                && kinds.contains(&"pi.session_ended")
        });
        let events = seen.lock().unwrap();
        let settled = events
            .iter()
            .find(|event| event.raw_type == "pi.agent_settled")
            .expect("the settled event");
        // A failed turn is not a finished one, and the difference is what a row draws.
        assert_eq!(
            settled.kind,
            crate::domain::agent::AgentEventKind::TurnFailed
        );
        assert_eq!(settled.session_id.as_deref(), Some("pi-session"));
        assert_eq!(settled.checkout_id, "checkout:one");
    }

    #[test]
    fn nothing_is_listening_until_a_terminal_asks_for_one() {
        let bridge = PiBridge::new();
        assert_eq!(bridge.address(), None);
        assert!(bridge
            .issue_credential("terminal:one", "checkout:one")
            .is_some());
        assert!(bridge.address().is_some());
    }

    #[test]
    fn stopping_releases_the_loopback_listener() {
        let bridge = PiBridge::new();
        bridge
            .issue_credential("terminal:one", "checkout:one")
            .expect("the listener should start");
        let address = bridge.address().expect("the listener address");

        bridge.stop();

        assert!(bridge.address().is_none());
        assert!(TcpStream::connect(address).is_err());
    }

    /// A `hello` carrying the reviews this session was shown, which is where a launch's first
    /// receipt arrives: Pi records the prompt before it introduces itself.
    fn hello_with_receipts(token: &str, receipts: serde_json::Value) -> serde_json::Value {
        let mut frame = hello(token, "terminal:one", "checkout:one", "pi-session");
        frame["receipts"] = receipts;
        frame
    }

    fn receipt(marker: &str, entry: &str) -> serde_json::Value {
        serde_json::json!({"marker": marker, "entry": entry})
    }

    const MARKER: &str = "muster-review:round:1762000000:4242:7";

    /// Delivery is evidence only because the session names the round and the message. Anything
    /// that is not a marker this build wrote is refused here rather than travelling further and
    /// being matched against something.
    #[test]
    fn only_receipts_naming_a_round_and_an_entry_are_kept() {
        let (bridge, _credential, token) = bridge_with_terminal();
        let mut reporter = Reporter::connect(&bridge);
        reporter.send(hello_with_receipts(
            &token,
            serde_json::json!([
                receipt(MARKER, "entry:one"),
                receipt("please review muster-review:round:1:2:3", "entry:two"),
                receipt("muster-review:round:1:2", "entry:three"),
                receipt(MARKER, ""),
                {"marker": MARKER},
                {"entry": "entry:four"},
            ]),
        ));
        eventually("the session was recorded", || {
            bridge.sessions("checkout:one").len() == 1
        });
        let delivery = bridge
            .delivery("checkout:one", "pi-session")
            .expect("the session can prove something");
        assert_eq!(delivery.terminal_id, "terminal:one");
        assert_eq!(delivery.receipts.len(), 1, "{:?}", delivery.receipts);
        assert_eq!(delivery.receipts[0].marker, MARKER);
        assert_eq!(delivery.receipts[0].entry, "entry:one");
    }

    /// A snapshot has to stay inside the frame cap no matter how much a session was sent, and the
    /// newest is what is kept: a Pi is opened per send, so the newest is the one a round is
    /// waiting on.
    #[test]
    fn receipts_are_bounded_to_a_fixed_number() {
        let (bridge, _credential, token) = bridge_with_terminal();
        let mut reporter = Reporter::connect(&bridge);
        reporter.send(hello_with_receipts(
            &token,
            serde_json::json!((0..MAX_RECEIPTS * 3)
                .map(|index| {
                    receipt(
                        &format!("muster-review:round:1762000000:4242:{index}"),
                        &format!("entry:{index}"),
                    )
                })
                .collect::<Vec<_>>()),
        ));
        eventually("the session was recorded", || {
            bridge.sessions("checkout:one").len() == 1
        });
        let receipts = bridge
            .delivery("checkout:one", "pi-session")
            .expect("the session can prove something")
            .receipts;
        assert_eq!(receipts.len(), MAX_RECEIPTS);
        assert_eq!(receipts[0].entry, "entry:0");
    }

    /// A frame that says nothing about receipts is not a frame that says there are none. Reading
    /// it as an empty list would un-confirm a delivery by omission, which is how a reconnect
    /// would quietly take back something that had already happened.
    #[test]
    fn a_frame_that_omits_receipts_does_not_take_back_the_ones_already_reported() {
        let (bridge, _credential, token) = bridge_with_terminal();
        let mut reporter = Reporter::connect(&bridge);
        reporter.send(hello_with_receipts(
            &token,
            serde_json::json!([receipt(MARKER, "entry:one")]),
        ));
        eventually("the receipt was recorded", || {
            bridge
                .delivery("checkout:one", "pi-session")
                .is_some_and(|delivery| delivery.receipts.len() == 1)
        });

        reporter.send(state(false, false, Some("succeeded")));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            bridge
                .delivery("checkout:one", "pi-session")
                .expect("still there")
                .receipts
                .len(),
            1
        );
    }

    #[test]
    fn a_reload_without_receipts_preserves_the_session_receipt_and_idle_state() {
        let (bridge, _credential, token) = bridge_with_terminal();
        let mut reporter = Reporter::connect(&bridge);
        reporter.send(hello_with_receipts(
            &token,
            serde_json::json!([receipt(MARKER, "entry:one")]),
        ));
        reporter.send(state(true, false, None));
        reporter.send(state(false, false, Some("failed")));
        eventually("the turn settled", || {
            bridge
                .delivery("checkout:one", "pi-session")
                .is_some_and(|delivery| delivery.settled && delivery.receipts.len() == 1)
        });
        let before = bridge.sessions("checkout:one").remove(0);
        drop(reporter);
        eventually("the runtime disconnected", || {
            bridge.sessions("checkout:one")[0].awaiting_reply.is_none()
        });

        let mut reloaded = Reporter::connect(&bridge);
        reloaded.send(hello(&token, "terminal:one", "checkout:one", "pi-session"));
        eventually("the reloaded runtime restored the session", || {
            bridge
                .delivery("checkout:one", "pi-session")
                .is_some_and(|delivery| delivery.receipts.len() == 1 && delivery.settled)
        });
        let after = &bridge.sessions("checkout:one")[0];
        assert_eq!(after.idle_at, before.idle_at);
        assert_eq!(after.outcome, before.outcome);
        assert_eq!(
            bridge
                .delivery("checkout:one", "pi-session")
                .unwrap()
                .receipts[0]
                .entry,
            "entry:one"
        );
    }

    /// Completion is the settlement Pi announced and nothing else. A lost connection leaves the
    /// flag down, and a reload keeps it up, because both are facts about the same session rather
    /// than about the runtime that was reporting it.
    #[test]
    fn only_a_reported_settlement_says_a_turn_finished() {
        let (bridge, _credential, token) = bridge_with_terminal();
        let mut reporter = Reporter::connect(&bridge);
        reporter.send(hello(&token, "terminal:one", "checkout:one", "pi-session"));
        reporter.send(state(true, false, None));
        eventually("the turn started", || {
            bridge
                .delivery("checkout:one", "pi-session")
                .is_some_and(|delivery| !delivery.settled)
        });
        assert!(
            !bridge
                .delivery("checkout:one", "pi-session")
                .expect("the session is there")
                .settled,
            "a running turn was reported as finished"
        );

        // A connection that goes away mid-turn is not a turn that finished.
        drop(reporter);
        eventually("the connection was noticed as gone", || {
            bridge
                .sessions("checkout:one")
                .first()
                .is_some_and(|session| session.awaiting_reply.is_none())
        });
        assert!(
            !bridge
                .delivery("checkout:one", "pi-session")
                .expect("the row outlives its connection")
                .settled
        );

        // The runtime that replaces it reports the same session, and what that session has
        // already settled is not something the reload may take away.
        let mut reloaded = Reporter::connect(&bridge);
        reloaded.send(hello(&token, "terminal:one", "checkout:one", "pi-session"));
        reloaded.send(state(true, false, None));
        reloaded.send(state(false, false, Some("succeeded")));
        eventually("the turn settled", || {
            bridge
                .delivery("checkout:one", "pi-session")
                .is_some_and(|delivery| delivery.settled)
        });
        let mut after = Reporter::connect(&bridge);
        after.send(hello(&token, "terminal:one", "checkout:one", "pi-session"));
        eventually("the reload restored the row", || {
            bridge
                .delivery("checkout:one", "pi-session")
                .is_some_and(|delivery| delivery.settled)
        });
    }

    /// A session is asked about by its own id, in its own checkout. Naming another's session is
    /// not an error and not a lookup across the workspace: there is nothing there to find.
    #[test]
    fn a_session_of_another_checkout_proves_nothing_about_this_one() {
        let bridge = PiBridge::new();
        let one = bridge
            .issue_credential("terminal:one", "checkout:one")
            .unwrap();
        let two = bridge
            .issue_credential("terminal:two", "checkout:two")
            .unwrap();
        let mut reporter = Reporter::connect(&bridge);
        reporter.send(hello(
            two.rsplit(':').next().unwrap(),
            "terminal:two",
            "checkout:two",
            "pi-elsewhere",
        ));
        eventually("the session was recorded", || {
            bridge.sessions("checkout:two").len() == 1
        });
        assert!(bridge.delivery("checkout:one", "pi-elsewhere").is_none());
        assert!(bridge.delivery("checkout:two", "pi-elsewhere").is_some());
        assert!(bridge
            .delivery("checkout:two", "pi-never-started")
            .is_none());
        drop(one);
    }

    #[test]
    fn frames_are_split_on_newlines_and_bounded() {
        let joined = b"first\nsecond\r\n\n\nthird\n".to_vec();
        let mut frames = Frames::new(BufReader::new(&joined[..]));
        assert_eq!(frames.next().unwrap(), b"first");
        assert_eq!(frames.next().unwrap(), b"second");
        assert_eq!(frames.next().unwrap(), b"third");
        assert_eq!(frames.next(), None);

        // Blank lines are legal and cost a frame's worth of nothing rather than a stack frame each:
        // a reporter that pads its stream must not be able to run this thread out of stack.
        let padded = vec![b'\n'; 100_000];
        let mut padded_frames = Frames::new(BufReader::new(&padded[..]));
        assert_eq!(padded_frames.next(), None);

        let endless = vec![b'x'; MAX_FRAME_BYTES * 2];
        assert_eq!(Frames::new(BufReader::new(&endless[..])).next(), None);

        let mut empty = Frames::new(BufReader::new(&[][..]));
        assert_eq!(empty.next(), None);
    }
}
