//! Review rounds: one batch of notes delivered to an OpenCode session as one message.
//!
//! A round is recorded **before** the service is called, with its marker embedded in
//! the exact prompt it will be sent with. That ordering is the whole point: a send
//! whose answer was lost leaves the round in [`RoundStatus::Dispatching`], and the
//! marker in the session's messages is what says whether it landed.
//!
//! ```text
//! sent ──marker found──▶ dispatched ──▶ acked
//!   │        ▲
//!   │        └──marker found── dispatching ──marker absent──▶ unknown
//!   └──2xx───▶ dispatched        │
//!                             └──never connected──▶ queued ──▶ dispatching
//! ```
//!
//! Only `queued` is ever offered to the service, and `queued` is only reachable by a
//! round that has *provably* not been taken: it is recorded before the call, or it is a
//! send whose `connect` failed. `dispatching` is an unanswered question, and `unknown`
//! is the answer to the one question nobody can answer — the service may still take a
//! prompt it accepted and answered late, so an absent marker is not proof it did not.
//! A message that may have landed is never sent a second time on this app's own: the
//! way out of `unknown` is a person reading the round and asking for it ([`RoundStore::resend_round`]).
//!
//! A round also *claims* the notes it carries, by id and by what they said at the time.
//! Two windows asking to send one review therefore get one round rather than two, and a
//! note somebody has edited since is a new comment and may be sent again.
//!
//! The file lives in `~/.muster`, outside every checkout, because it is app state and
//! not the checkout's: reading or writing it never touches a working tree. Every
//! mutation is serialized by an OS-held lock on a sibling lock file — held across the
//! service calls too, so two windows cannot both offer the same round — and each is
//! written to a private temporary file and renamed into place, so a reader never sees
//! half a file and a crash never leaves one behind.
//!
//! It is also bounded, and the bound is a policy rather than a wall. A write that gets
//! close to the cap gives up what nothing can ask for again — the prompt of a round the
//! service has already answered for — and a person can forget delivered notes from
//! **Settings → Forget delivered reviews…**. The same dialog offers a deliberate last
//! resort that also removes settled claims; after that, unchanged notes can be sent again.
//! Neither action touches a round that still owes the service a decision. Claims use a
//! SHA-256 fingerprint of each canonical note snapshot, not a mathematical equality proof.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use super::{same_directory, validate_session_id, Client, DispatchError};
use crate::{
    review::{
        build_round_prompt, check_review_note, check_round_prompt, review_note_key,
        review_round_marker, ReviewNote, MAX_ROUND_NOTES,
    },
    workspace::{checkout_id_for_path, Checkout},
};

/// The store holds rounds, not a transcript. Past this it is not this app's file.
const STATE_CAP: u64 = 8 * 1024 * 1024;
/// Where a write has to start giving something up rather than be refused.
///
/// Three quarters of the cap, because what a settled round gives up is only the prompt
/// it will never be offered again: giving it up early costs nothing observable, and a
/// review that cannot be stored is a review that never goes out.
const COMPACT_ABOVE: u64 = STATE_CAP / 4 * 3;
const STORE_FILE: &str = "opencode-rounds.json";
const LOCK_FILE: &str = "opencode-rounds.lock";
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(1);

/// Where one checkout's reviews go. Remembered per checkout, because a review is
/// written about one checkout and is not interchangeable with another.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Destination {
    /// Exported as Markdown; where is the workspace's review storage setting.
    Markdown,
    /// Sent as a message to this OpenCode session.
    Opencode { session_id: String },
    /// OpenCode is selected, but the app has not created a session yet.
    /// Creating one is deferred until the person presses Send.
    OpencodeNew,
}

/// Where a round is in its life. The names are the contract: `dispatching` is
/// deliberately not an error state, because the one thing that cannot be known is
/// whether the service took the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundStatus {
    /// Decided and stored, and provably never taken by the service: safe to offer.
    Queued,
    /// Offered, or offered-and-crashed: the outcome is unknown.
    Dispatching,
    /// Offered, the answer was lost, and the transcript does not show it either. This is
    /// the blocked state: an absent marker does not prove the service did not take the
    /// prompt, so Muster does not re-offer it on its own.
    Unknown,
    /// The service answered that it took the message, or the marker was found in the
    /// transcript.
    Dispatched,
    /// Finished with; nothing further is owed for this round.
    Acked,
}

impl RoundStatus {
    /// Whether this round may still be offered, or is still a question to settle.
    pub fn is_pending(self) -> bool {
        matches!(self, Self::Queued | Self::Dispatching)
    }

    /// Whether the service has answered for this round and nothing is owed for it.
    ///
    /// The opposite of pending in the sense that matters for storage rather than for
    /// sending: a settled round is never offered, never reconciled and never read back by
    /// a person, so its prompt is a payload nothing can ask for again.
    pub fn is_settled(self) -> bool {
        matches!(self, Self::Dispatched | Self::Acked)
    }
}

/// One batch of notes, as recorded before it was offered to the service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewRound {
    pub id: String,
    /// The registered checkout this review is about. The session below was created in
    /// that checkout's directory and no other.
    pub checkout_id: String,
    pub session_id: String,
    pub status: RoundStatus,
    /// Embedded in the prompt so a reconnect can recognize this exact send.
    pub marker: String,
    /// The message, stored as it will be sent. A round that goes out later, possibly
    /// after a restart, has to carry exactly the bytes the user accepted.
    pub prompt: String,
    /// The notes this round claims, sorted. The claim is what stops two windows from
    /// sending one set of notes as two rounds.
    pub note_ids: Vec<String>,
    /// One key per claimed note ([`review_note_key`]), one per line.
    ///
    /// This is the whole of what a duplicate send is decided against, and it is kept
    /// after the notes are gone: what a note said is unbounded, a key is not, so a
    /// round that has given its notes up still fingerprints what it took. SHA-256 makes
    /// accidental or adversarial collisions impractical, though not mathematically
    /// impossible. The same notes, edited, produce a different key.
    pub claim: String,
    /// The notes the review was built from, captured when the round was recorded, so a
    /// sent review can still be shown and exported without re-reading the diff.
    pub notes: Vec<ReviewNote>,
    pub created_at: String,
    pub updated_at: String,
}

impl ReviewRound {
    /// The notes this round already took, as the key each one is judged by.
    ///
    /// Answered the same way whether the round still carries its notes or has given
    /// them up, because a key is derived from a note rather than being the note: a
    /// round that can no longer show a review still cannot send it twice.
    pub fn claimed_keys(&self) -> Vec<String> {
        if !self.notes.is_empty() {
            return self.notes.iter().map(review_note_key).collect();
        }
        self.claim.lines().map(str::to_owned).collect()
    }
}

/// What the store holds. Every field is required and no field is unknown, and
/// there is no version in the file: one shape, changed in place, so the shape
/// itself is the whole contract and a file this build cannot read is refused by
/// name rather than read through defaults, because a round read as "nothing
/// happened" would be re-sent.
///
/// The claim is one key per claimed note, because a claim that carries the notes
/// cannot be given up: it is the reviews themselves, held per round, for as long
/// as the store exists.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RoundFile {
    rounds: Vec<ReviewRound>,
    destinations: BTreeMap<String, Destination>,
}

impl RoundFile {
    /// Gives up what no round in this state can be asked for again, and answers whether
    /// that released anything.
    ///
    /// A settled round keeps everything it is judged by — the notes it took, the keys they
    /// are judged by, its marker, its id and where it went — and drops only the prompt it
    /// will never be offered again. A round that still owes the service a decision is not
    /// touched at all: `queued` may be offered, `dispatching` and `unknown` are questions
    /// a person settles by reading the round, and all three need their exact bytes.
    fn compact(&mut self) -> bool {
        let mut released = false;
        for round in &mut self.rounds {
            if round.status.is_settled() && !round.prompt.is_empty() {
                round.prompt.clear();
                released = true;
            }
        }
        released
    }
}

/// What the caller has decided to send: the registered checkout it is about, the
/// session to send it to, and the notes it was built from.
pub struct RoundRequest<'a> {
    pub checkout: &'a Checkout,
    pub session_id: &'a str,
    pub notes: &'a [ReviewNote],
    pub review_markdown: &'a str,
}

/// The durable store. Cheap to hold: every operation opens the lock it needs.
#[derive(Clone, Debug)]
pub struct RoundStore {
    path: PathBuf,
    directory: PathBuf,
}

impl RoundStore {
    /// The store for this machine's `~/.muster`, beside the native workspace file.
    pub fn open(home: &Path) -> Result<Self, DispatchError> {
        if !home.is_absolute() {
            return Err(DispatchError::Failed(
                "the review round store needs an absolute home directory".into(),
            ));
        }
        Self::open_in(&home.join(".muster"))
    }

    /// The store in one state directory, which is what a profile or a test uses.
    pub fn open_in(directory: &Path) -> Result<Self, DispatchError> {
        private_directory(directory)?;
        Ok(Self {
            path: directory.join(STORE_FILE),
            directory: directory.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every round of a checkout, oldest first. Reads take no lock: a write is a
    /// rename over a complete file, so a reader sees the round that was there or the
    /// round that replaced it, never a mixture.
    pub fn rounds(&self, checkout_id: &str) -> Result<Vec<ReviewRound>, DispatchError> {
        Ok(self
            .read()?
            .rounds
            .into_iter()
            .filter(|round| round.checkout_id == checkout_id)
            .collect())
    }

    /// One round, or that there is not one.
    pub fn round(
        &self,
        checkout_id: &str,
        round_id: &str,
    ) -> Result<Option<ReviewRound>, DispatchError> {
        Ok(self
            .rounds(checkout_id)?
            .into_iter()
            .find(|round| round.id == round_id))
    }

    /// Rounds that still owe the service a decision.
    pub fn pending(&self, checkout_id: &str) -> Result<Vec<ReviewRound>, DispatchError> {
        Ok(self
            .rounds(checkout_id)?
            .into_iter()
            .filter(|round| round.status.is_pending())
            .collect())
    }

    /// Where this checkout's reviews are sent.
    pub fn destination(&self, checkout_id: &str) -> Result<Option<Destination>, DispatchError> {
        Ok(self.read()?.destinations.get(checkout_id).cloned())
    }

    /// Records where this checkout's reviews go, replacing what was there.
    pub fn set_destination(
        &self,
        checkout_id: &str,
        destination: Destination,
    ) -> Result<(), DispatchError> {
        let _lock = self.lock()?;
        let mut state = self.read()?;
        state
            .destinations
            .insert(checkout_id.to_string(), destination);
        self.write(&mut state)
    }

    /// Claims `request`'s notes and sends the review, as one locked step.
    ///
    /// The claim and the message are written **before** the request goes out, so a second
    /// window asking for the same notes gets this round back instead of starting another
    /// one, and a crash between the claim and the answer leaves a round that can be
    /// settled rather than a message that might be sent twice.
    ///
    /// A failure the caller may act on is returned, and the round is left where the truth
    /// about it is: `dispatched` when the service answered, `queued` when the connection
    /// was never made (nothing was written, so there is nothing to settle), and
    /// `dispatching` otherwise — the service may have taken the prompt and lost the
    /// answer, which is a question and not a retry.
    ///
    /// `client` has to be the one scoped to this checkout. A client for another directory
    /// would answer about that directory, so the review is refused before anything is
    /// recorded or offered.
    pub fn send_round(
        &self,
        client: &Client,
        request: RoundRequest<'_>,
    ) -> Result<ReviewRound, DispatchError> {
        if !same_directory(
            client.directory(),
            Path::new(&request.checkout.canonical_path),
        ) {
            return Err(DispatchError::Foreign(
                "this agent connection is scoped to another checkout".into(),
            ));
        }
        let _lock = self.lock()?;
        let mut state = self.read()?;
        let (index, offer) = claim_round(&mut state, request)?;
        if !offer {
            // Another window already offered these notes and is answering for this round.
            return Ok(state.rounds[index].clone());
        }
        // Durable before the request, whatever this round was waiting as: a round this
        // caller is about to offer is no longer provably untaken, and a crash between the
        // claim and the answer has to leave a question rather than a retry, which is what
        // offering a still-`queued` round and then dying would have done.
        set_status(&mut state, index, RoundStatus::Dispatching);
        self.write(&mut state)?;
        let round = state.rounds[index].clone();
        let sent = client.prompt(&round.session_id, &round.prompt);
        match &sent {
            Ok(()) => set_status(&mut state, index, RoundStatus::Dispatched),
            Err(error) if error.never_sent() => set_status(&mut state, index, RoundStatus::Queued),
            // Left `dispatching` on purpose: the marker is what lets reconciliation learn
            // whether it landed, and requeueing a send that may have landed is how one
            // review arrives as two.
            Err(_) => {}
        }
        let stored = state.rounds[index].clone();
        self.write(&mut state)?;
        // The error is the caller's report and the round is already durable, so a caller
        // that wants to draw the outcome reads the rounds back rather than being handed a
        // copy that could disagree with the file.
        sent.map(|()| stored)
    }

    /// Gives up the notes of every round the service has already answered for, in every
    /// checkout, and reports how many rounds were reduced.
    ///
    /// The deliberate half of capacity recovery, for the store the automatic one cannot
    /// relieve: automatic compaction gives up a settled round's prompt, and this gives up
    /// what the prompt was standing in for. What each forgotten review took stays in the
    /// round's claim, which is one key per note rather than a copy of the note, so the
    /// same review asked for again is answered from that round rather than sent a second
    /// time with SHA-256 collision resistance — what is lost is being able to show or
    /// export those reviews, which is the person's decision to make and is said so where
    /// they make it.
    ///
    /// A round that still owes the service a decision is never touched.
    pub fn forget_settled_history(&self) -> Result<usize, DispatchError> {
        let _lock = self.lock()?;
        let mut state = self.read()?;
        let mut forgotten = 0;
        for round in &mut state.rounds {
            if !round.status.is_settled() || round.notes.is_empty() {
                continue;
            }
            round.prompt.clear();
            round.notes.clear();
            forgotten += 1;
        }
        if forgotten > 0 {
            self.write(&mut state)?;
        }
        Ok(forgotten)
    }

    /// Last-resort recovery: remove settled rounds and their duplicate-protection keys.
    ///
    /// This deliberately resets duplicate scope: unchanged delivered notes can be sent
    /// again afterward. Queued, dispatching, and unknown rounds are never removed.
    pub fn reset_settled_duplicate_scope(&self) -> Result<usize, DispatchError> {
        let _lock = self.lock()?;
        let mut state = self.read()?;
        let before = state.rounds.len();
        state.rounds.retain(|round| !round.status.is_settled());
        let removed = before - state.rounds.len();
        if removed > 0 {
            self.write(&mut state)?;
        }
        Ok(removed)
    }

    /// Puts a blocked round back in line to be offered, because a person asked for it.
    ///
    /// The only way out of [`RoundStatus::Unknown`], and it is never automatic: Muster will
    /// not re-offer a message it cannot prove was not taken, but the person reading the
    /// round can decide that it is worth the risk of landing twice.
    pub fn resend_round(
        &self,
        checkout_id: &str,
        round_id: &str,
    ) -> Result<ReviewRound, DispatchError> {
        self.transition(
            checkout_id,
            round_id,
            &[RoundStatus::Unknown],
            RoundStatus::Queued,
        )
    }

    /// Records that the round is finished with.
    pub fn ack_round(
        &self,
        checkout_id: &str,
        round_id: &str,
    ) -> Result<ReviewRound, DispatchError> {
        self.transition(
            checkout_id,
            round_id,
            &[RoundStatus::Dispatched],
            RoundStatus::Acked,
        )
    }

    /// Asks the service whether a round's marker is in the session, and settles the
    /// round on the answer.
    ///
    /// A marker that is there means the send landed, whatever happened to the answer, and
    /// the round is settled. A marker that is absent proves nothing: the service may have
    /// accepted a prompt and appended it after this question was answered, or accepted it
    /// and been restarted before persisting it, so an absent marker is the answer "not
    /// yet, or never" and not "not delivered". The round therefore becomes
    /// [`RoundStatus::Unknown`] and stays there — Muster does not put a message it cannot
    /// prove was not taken back in line to be offered.
    ///
    /// A question the service did not answer leaves the round exactly where it was: this
    /// never sends anything. The lock is held across the question, so a window cannot
    /// reconcile a round that another window is sending right now.
    pub fn reconcile_round(
        &self,
        client: &Client,
        checkout_id: &str,
        round_id: &str,
    ) -> Result<ReviewRound, DispatchError> {
        let _lock = self.lock()?;
        let mut state = self.read()?;
        let index = round_index(&state, checkout_id, round_id)?;
        let round = state.rounds[index].clone();
        if !round.status.is_pending() {
            // Already settled: a round the service answered for is never re-opened,
            // however the transcript looks now.
            return Ok(round);
        }
        let landed = client.session_mentions(&round.session_id, &round.marker)?;
        set_status(
            &mut state,
            index,
            if landed {
                RoundStatus::Dispatched
            } else {
                RoundStatus::Unknown
            },
        );
        let stored = state.rounds[index].clone();
        self.write(&mut state)?;
        Ok(stored)
    }

    /// Offers every round this checkout still owes the service, oldest first, and
    /// reports how many were actually sent.
    ///
    /// Only `queued` is offered, which means only rounds that provably were never taken:
    /// one that was never offered, or one whose connection failed before a byte was
    /// written. A round in `unknown` is not among them — that is the state for a send
    /// this app cannot prove was lost, and it waits for [`RoundStore::resend_round`].
    ///
    /// Each round is checked against the transcript **before** it is offered, so a round
    /// that went out and lost its answer is confirmed rather than sent twice. The whole
    /// flush holds the store's lock: that is what stops a second window from asking about
    /// a round that is in flight right now.
    pub fn flush_rounds(&self, client: &Client, checkout_id: &str) -> Result<usize, DispatchError> {
        let _lock = self.lock()?;
        let mut state = self.read()?;
        let mut sent = 0;
        // The file is in the order rounds were recorded, so index order is age order.
        for index in 0..state.rounds.len() {
            if state.rounds[index].checkout_id != checkout_id
                || state.rounds[index].status != RoundStatus::Queued
            {
                continue;
            }
            let round = state.rounds[index].clone();
            // Already there: this round went out and its answer was lost.
            if client.session_mentions(&round.session_id, &round.marker)? {
                set_status(&mut state, index, RoundStatus::Dispatched);
                self.write(&mut state)?;
                continue;
            }
            // Claim before the call, and write the claim before making it: a crash
            // between here and the answer leaves `dispatching`, which is a question.
            set_status(&mut state, index, RoundStatus::Dispatching);
            self.write(&mut state)?;
            let offered = client.prompt(&round.session_id, &round.prompt);
            match &offered {
                Ok(()) => {
                    set_status(&mut state, index, RoundStatus::Dispatched);
                    sent += 1;
                }
                // The connection was never made, so nothing was written and the round is a
                // retry rather than a question.
                Err(error) if error.never_sent() => {
                    set_status(&mut state, index, RoundStatus::Queued);
                }
                // Left `dispatching`: requeueing a send that may have landed duplicates it.
                Err(_) => {}
            }
            self.write(&mut state)?;
            offered?;
        }
        Ok(sent)
    }

    fn transition(
        &self,
        checkout_id: &str,
        round_id: &str,
        from: &[RoundStatus],
        to: RoundStatus,
    ) -> Result<ReviewRound, DispatchError> {
        let _lock = self.lock()?;
        let mut state = self.read()?;
        let index = round_index(&state, checkout_id, round_id)?;
        let current = state.rounds[index].status;
        if !from.contains(&current) {
            return Err(DispatchError::Failed(format!(
                "review round {round_id} is {current:?} and cannot become {to:?} from here"
            )));
        }
        set_status(&mut state, index, to);
        let stored = state.rounds[index].clone();
        self.write(&mut state)?;
        Ok(stored)
    }

    fn read(&self) -> Result<RoundFile, DispatchError> {
        let unreadable = |error: std::io::Error| {
            DispatchError::Failed(format!(
                "could not read the review round store {}: {error}",
                self.path.display()
            ))
        };
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // `O_NOFOLLOW` refuses a store that is a link, and `O_NONBLOCK` is what keeps
            // a device node from parking this thread in the read below.
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = match options.open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RoundFile::default())
            }
            // A store that is a link is not this app's state, and nothing follows it.
            Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
                return Err(DispatchError::Foreign(
                    "the review round store must be a regular file, not a symlink".into(),
                ))
            }
            Err(error) => return Err(unreadable(error)),
        };
        let metadata = file.metadata().map_err(|error| {
            DispatchError::Failed(format!("could not inspect the review round store: {error}"))
        })?;
        if !metadata.is_file() || metadata.len() > STATE_CAP {
            return Err(DispatchError::Foreign(
                "the review round store is not a regular file within the 8 MiB limit".into(),
            ));
        }
        let mut bytes = Vec::new();
        file.take(STATE_CAP + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                DispatchError::Failed(format!("could not inspect the review round store: {error}"))
            })?;
        if bytes.len() as u64 > STATE_CAP {
            return Err(DispatchError::Foreign(
                "the review round store exceeds the 8 MiB limit".into(),
            ));
        }
        let state: RoundFile = serde_json::from_slice(&bytes).map_err(|error| {
            DispatchError::Foreign(format!(
                "the review round store {} could not be read: {error}, and deleting {} starts an \
                 empty review history",
                self.path.display(),
                self.path.display()
            ))
        })?;
        Ok(state)
    }

    /// The exclusive lock every mutation holds, including across its service calls.
    ///
    /// OS-held and released by drop or by process death, which is what makes this work
    /// for a window that is killed mid-send. The lock file is never unlinked: replacing
    /// its inode would let a second window lock a different file.
    fn lock(&self) -> Result<File, DispatchError> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
            options.mode(0o600);
        }
        let file = options
            .open(self.directory.join(LOCK_FILE))
            .map_err(|error| {
                DispatchError::Failed(format!("could not open the review round lock: {error}"))
            })?;
        let metadata = file.metadata().map_err(|error| {
            DispatchError::Failed(format!("could not inspect the review round lock: {error}"))
        })?;
        if !metadata.is_file() {
            return Err(DispatchError::Foreign(
                "the review round lock must be a regular file".into(),
            ));
        }
        file.lock().map_err(|error| {
            DispatchError::Failed(format!("could not lock the review round store: {error}"))
        })?;
        Ok(file)
    }

    /// Validate, give up what nothing can read again if the cap is close, then write
    /// privately in the same directory, sync and atomically replace.
    ///
    /// `state` is compacted in place rather than only on the way to the file, so a caller
    /// that goes on using it is looking at the same rounds the next reader will see.
    fn write(&self, state: &mut RoundFile) -> Result<(), DispatchError> {
        let mut bytes = encode(state)?;
        if bytes.len() as u64 > COMPACT_ABOVE && state.compact() {
            bytes = encode(state)?;
        }
        if bytes.len() as u64 > STATE_CAP {
            // Refused before the temporary file is opened, so the store on disk is the
            // file that was there and nothing was truncated trying to write this.
            return Err(DispatchError::Failed(
                "the review round store is full at 8 MiB; in Settings, forget delivered reviews, \
                 or reset duplicate protection if their notes were already forgotten"
                    .into(),
            ));
        }
        let name = self.path.file_name().ok_or_else(|| {
            DispatchError::Failed("the review round store has no filename".into())
        })?;
        let temp = self.directory.join(format!(
            ".{}.{}-{}.tmp",
            name.to_string_lossy(),
            std::process::id(),
            NEXT_SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
            options.mode(0o600);
        }
        let mut file = options.open(&temp).map_err(|error| {
            DispatchError::Failed(format!(
                "could not create the store temporary file: {error}"
            ))
        })?;
        let written = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temp, &self.path)?;
            File::open(&self.directory)?.sync_all()
        })();
        if let Err(error) = written {
            let _ = fs::remove_file(&temp);
            return Err(DispatchError::Failed(format!(
                "could not save the review round store {}: {error}",
                self.path.display()
            )));
        }
        Ok(())
    }
}

/// The notes a new round could carry: the ones no round has already sent.
///
/// A note a round captured *is* that review, so sending it again delivers the same comment
/// twice. A note edited since is a new comment on the same line and is kept: that is how a
/// review is asked for again after somebody fixed what the first one said. This is what the
/// window offers, so restoring a checkout's rounds cannot put an already-sent review back on
/// the wire.
///
/// The comparison is on each note's key ([`review_note_key`]), which a round keeps
/// whether or not it still carries the note: an unchanged note a delivered round already
/// took is not offered again, and an edited one still is.
pub fn unsent_notes(notes: &[ReviewNote], rounds: &[ReviewRound]) -> Vec<ReviewNote> {
    let claimed: HashSet<String> = rounds.iter().flat_map(ReviewRound::claimed_keys).collect();
    notes
        .iter()
        .filter(|note| !claimed.contains(&review_note_key(note)))
        .cloned()
        .collect()
}

/// What one batch of notes claims, as a single string.
///
/// Sorted, so the order the caller happened to hand them over is not part of the claim:
/// two windows looking at one review produce the same string, which is the whole
/// reason a claim can be compared at all. What is compared is one key per note rather
/// than the notes, because a claim that carries the notes is a claim that can never be
/// given up.
fn round_claim(notes: &[ReviewNote]) -> String {
    let mut keys: Vec<String> = notes.iter().map(review_note_key).collect();
    keys.sort();
    keys.join("\n")
}

/// Claims `request`'s notes in `state`, or answers with the round that already holds them.
///
/// The round is returned with whether this caller may offer it: a round that already holds
/// these notes is the same review, so it is handed back rather than sent twice, and only a
/// `queued` one — never taken by the service — is still a send.
fn claim_round(
    state: &mut RoundFile,
    request: RoundRequest<'_>,
) -> Result<(usize, bool), DispatchError> {
    let checkout_id = checkout_identity(request.checkout)?;
    if request.notes.is_empty() || request.notes.len() > MAX_ROUND_NOTES {
        return Err(DispatchError::Failed(format!(
            "a review round needs between 1 and {MAX_ROUND_NOTES} notes"
        )));
    }
    // The round names the session in a route and settles itself against that session's
    // transcript, so the id is the one shape a session can have before it is recorded.
    validate_session_id(request.session_id.trim())?;
    if request
        .notes
        .iter()
        .any(|note| note.checkout_id != checkout_id)
    {
        return Err(DispatchError::Foreign(
            "a review round cannot carry notes from another checkout".into(),
        ));
    }
    // What the round would record is what the agent would read, so it is checked here,
    // where a note becomes durable: a round that cannot be delivered is not one to claim.
    for note in request.notes {
        check_review_note(note).map_err(DispatchError::Failed)?;
    }
    let ids: BTreeSet<&str> = request.notes.iter().map(|note| note.id.as_str()).collect();
    if ids.len() != request.notes.len() {
        return Err(DispatchError::Failed(
            "a review round cannot carry the same note twice".into(),
        ));
    }
    let claim = round_claim(request.notes);
    for (index, round) in state.rounds.iter().enumerate() {
        if round.checkout_id == checkout_id && round.claim == claim {
            // One review is one round: the second window is handed the first one's answer
            // instead of starting a second send of the same notes.
            return Ok((index, round.status == RoundStatus::Queued));
        }
    }
    for round in &state.rounds {
        if round.checkout_id != checkout_id {
            continue;
        }
        // A note another round holds *as it stands* is that round's review, and sending it
        // again delivers the same comment twice. A note edited since is a new comment on
        // the same line, which is not this, and is what asking for a review again means.
        let claimed = round.claimed_keys();
        if let Some(held) = request
            .notes
            .iter()
            .find(|note| claimed.contains(&review_note_key(note)))
        {
            return Err(DispatchError::Foreign(format!(
                "note {} is already part of review round {}",
                held.id, round.id
            )));
        }
    }
    let now = now_millis();
    let id = format!(
        "r{now}-{}-{}",
        std::process::id(),
        NEXT_SERIAL.fetch_add(1, Ordering::Relaxed)
    );
    // The marker is derived from the id, so what the store holds and what the
    // session will hold are the same string by construction.
    let marker = review_round_marker(&id);
    let round = ReviewRound {
        id,
        checkout_id,
        session_id: request.session_id.trim().to_string(),
        status: RoundStatus::Dispatching,
        marker: marker.clone(),
        // Trimmed once, here, so the stored prompt and the one the client puts on
        // the wire are the same string.
        prompt: build_round_prompt(request.review_markdown, &marker)
            .trim()
            .to_string(),
        note_ids: ids.iter().map(|id| (*id).to_string()).collect(),
        claim,
        notes: request.notes.to_vec(),
        created_at: now.clone(),
        updated_at: now,
    };
    check_round_prompt(&round.prompt).map_err(DispatchError::Failed)?;
    state.rounds.push(round);
    Ok((state.rounds.len() - 1, true))
}

fn round_index(
    state: &RoundFile,
    checkout_id: &str,
    round_id: &str,
) -> Result<usize, DispatchError> {
    state
        .rounds
        .iter()
        .position(|round| round.id == round_id && round.checkout_id == checkout_id)
        .ok_or_else(|| {
            DispatchError::Foreign("review round does not belong to the requested checkout".into())
        })
}

fn set_status(state: &mut RoundFile, index: usize, status: RoundStatus) {
    state.rounds[index].status = status;
    state.rounds[index].updated_at = now_millis();
}

/// The registered identity of a checkout, and why it is the only identity accepted.
///
/// The client is scoped to `checkout.path` and every answer is refused unless it is
/// about that directory, so a round recorded under another identity would be a review
/// sent under one checkout's name and answered for another.
fn checkout_identity(checkout: &Checkout) -> Result<String, DispatchError> {
    let forged =
        || DispatchError::Foreign("checkout is not the one this workspace registered".to_owned());
    if checkout.id != checkout_id_for_path(&checkout.canonical_path)
        || checkout.path != checkout.canonical_path
        || checkout.is_missing
    {
        return Err(forged());
    }
    let root = Path::new(&checkout.canonical_path);
    let resolved = root
        .canonicalize()
        .map_err(|error| DispatchError::Failed(format!("could not resolve checkout: {error}")))?;
    if resolved != root || !root.is_dir() {
        return Err(forged());
    }
    Ok(checkout.id.clone())
}

/// The store's state directory, checked and required: the one place the store is written.
///
/// `~/.muster` holds review prompts and the lock that serializes them, so it is opened as
/// a directory itself — never through a link, never as somebody else's, never
/// world-readable — and the files below it are then opened with `O_NOFOLLOW`, so none of
/// them is a link either. That is the rule the native workspace state is held to, for the
/// same reason: the prompts are this person's, and there is one directory they may be in.
///
/// The path is opened as written, so a platform alias in an ancestor (macOS `/var`) is
/// fine; only the component this store names has to be the directory itself. Files
/// below it are opened with `O_NOFOLLOW` as well.
fn private_directory(directory: &Path) -> Result<(), DispatchError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    // Another window may have created it first. The no-follow open below validates
    // the resulting directory rather than trusting the mkdir result.
    builder.create(directory).or_else(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            Ok(())
        } else {
            Err(DispatchError::Failed(format!(
                "could not create the review round directory {}: {error}",
                directory.display()
            )))
        }
    })?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let dir = match options.open(directory) {
        Ok(dir) => dir,
        // A link in the place of the directory is not a store this app may keep: the open
        // refuses to follow it and answers `ENOTDIR` or `ELOOP` for it.
        Err(error) if matches!(error.raw_os_error(), Some(libc::ENOTDIR | libc::ELOOP)) => {
            return Err(DispatchError::Foreign(
                "the review round directory must be a directory, not a symlink".into(),
            ))
        }
        Err(error) => {
            return Err(DispatchError::Failed(format!(
                "could not open the review round directory {}: {error}",
                directory.display()
            )))
        }
    };
    let metadata = dir.metadata().map_err(|error| {
        DispatchError::Failed(format!(
            "could not inspect the review round directory {}: {error}",
            directory.display()
        ))
    })?;
    if !metadata.is_dir() {
        return Err(DispatchError::Foreign(
            "the review round directory must be a directory, not a file".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err(DispatchError::Foreign(
                "the review round directory must be private and owned by the current user".into(),
            ));
        }
    }
    Ok(())
}

/// The file exactly as it is written, so its size can be asked twice: once of what the
/// state says, and once of what it says once the state has given something up.
fn encode(state: &RoundFile) -> Result<Vec<u8>, DispatchError> {
    serde_json::to_vec_pretty(state)
        .map_err(|error| DispatchError::Failed(format!("could not encode the store: {error}")))
}

fn now_millis() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

#[cfg(test)]
mod tests;
