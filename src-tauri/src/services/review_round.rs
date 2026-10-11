//! Review rounds: one batch of notes delivered to an agent as a single message.
//!
//! The round is recorded *before* the agent is called, and its marker is embedded in the
//! prompt. That ordering is the whole point: if Muster dies mid-send, the round is left in
//! `dispatching` and reconciliation can ask the session whether the message actually
//! landed, instead of guessing and sending the review twice.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::{
    domain::{
        agent::AgentProvider,
        ipc::{IpcError, IpcErrorCode},
        review::{
            review_round_marker, ReviewNote, ReviewRound, AGENT_PROMPT_PREFIX, MAX_ROUND_NOTES,
            MAX_ROUND_PROMPT_BYTES, ROUND_MARKER_PREFIX,
        },
    },
    persistence::{timestamp, Database},
    services::agent::AgentService,
};

static NEXT_ROUND_ID: AtomicU64 = AtomicU64::new(1);

fn invalid(message: &str) -> IpcError {
    IpcError::new(IpcErrorCode::InvalidPath, message)
}

fn foreign(message: &str) -> IpcError {
    IpcError::new(IpcErrorCode::CheckoutOwnershipMismatch, message)
}

fn failed(message: impl Into<String>) -> IpcError {
    IpcError::new(IpcErrorCode::OperationFailed, message)
}

/// Builds the prompt for a round: the fixed instructions, the round marker, and the notes.
///
/// `marker` already carries the `muster-review:` prefix (see `review_round_marker`), so it
/// is embedded verbatim: the deduplication key must be exactly what the session will hold.
pub fn build_round_prompt(review_markdown: &str, marker: &str) -> String {
    format!("{AGENT_PROMPT_PREFIX}\n[{marker}]\n{review_markdown}")
}

pub fn rounds(database: &Database, checkout_id: &str) -> Result<Vec<ReviewRound>, IpcError> {
    database
        .review_rounds(checkout_id)
        .map_err(|error| IpcError::new(IpcErrorCode::InvalidCheckout, error))
}

/// Records a round as `dispatching` with its marker, and links its notes to it.
///
/// The notes are marked `sent` here too, before the agent is called, so a crash cannot leave
/// notes looking delivered when they were not, nor undelivered when they were.
pub fn begin_round(
    database: &Database,
    checkout_id: &str,
    provider: AgentProvider,
    session_id: &str,
    note_ids: &[String],
    markdown: &str,
) -> Result<ReviewRound, IpcError> {
    record_round(
        database,
        checkout_id,
        provider,
        RoundTarget::Session(session_id),
        note_ids,
        markdown,
        "dispatching",
    )
}

/// Records the round a Pi launch is about to open a terminal for.
///
/// The ordering is the same one every other round is recorded on — before the message can reach
/// the agent — and it is the only thing that makes a half-finished launch recognizable afterwards
/// rather than being retried blindly. What is different is what the round can honestly name yet:
/// the session does not exist, because the terminal running it has not been created. So the round
/// carries the provider and nothing else, and stays `dispatching` until a session reports itself
/// into the terminal bound to it.
///
/// **This never records delivery.** A Pi round reaching `dispatched` is the next stage's claim, and
/// it is one only after something has read the marker back out of the session.
pub fn begin_pi_round(
    database: &Database,
    checkout_id: &str,
    note_ids: &[String],
    markdown: &str,
) -> Result<ReviewRound, IpcError> {
    begin_pi_round_with_id(database, checkout_id, note_ids, markdown, &new_round_id())
}

pub fn begin_pi_round_with_id(
    database: &Database,
    checkout_id: &str,
    note_ids: &[String],
    markdown: &str,
    id: &str,
) -> Result<ReviewRound, IpcError> {
    record_round_with_id(
        database,
        checkout_id,
        AgentProvider::Pi,
        RoundTarget::NewPi,
        note_ids,
        markdown,
        "dispatching",
        id,
    )
}

/// Names the terminal a Pi round was launched into.
///
/// Bound the moment the terminal exists, so a session that appears in it can be traced back to the
/// round that asked for it. Scoped to rounds that still have no session: a Pi that replaces its own
/// session inside the same terminal is a new session, and the round records the one that received
/// the review.
pub fn bind_pi_round(
    database: &Database,
    checkout_id: &str,
    round_id: &str,
    terminal_id: &str,
) -> Result<ReviewRound, IpcError> {
    database
        .bind_pi_round_terminal(round_id, checkout_id, terminal_id)
        .map_err(|error| foreign(&error))
}

/// Records the same round, but waiting for the agent to become free.
///
/// Everything is decided and stored up front (the notes, the marker, and the message
/// itself) because the send happens later, possibly after a restart, and has to carry
/// exactly what the user accepted.
///
/// **OpenCode only.** Pi is never queued: a send to it opens a terminal and is running by the
/// time it returns, so there is no busy destination to wait for and nothing to flush later.
pub fn queue_round(
    database: &Database,
    checkout_id: &str,
    session_id: &str,
    note_ids: &[String],
    markdown: &str,
) -> Result<ReviewRound, IpcError> {
    record_round(
        database,
        checkout_id,
        AgentProvider::OpenCode,
        RoundTarget::Session(session_id),
        note_ids,
        markdown,
        "queued",
    )
}

/// What a round is aimed at, which is the whole difference between the two providers.
///
/// A terminal is not one of these: a Pi round is recorded before its terminal exists and is bound
/// to it afterwards, so naming a terminal here would mean inventing one.
enum RoundTarget<'a> {
    /// A session that is already running and can be sent a message.
    Session(&'a str),
    /// A Pi that has not been started yet, and whose session therefore does not exist.
    NewPi,
}

/// Records one round in one of its states.
///
/// What each provider has here is the opposite of what it ends up carrying, and the mismatch is
/// refused rather than stored: an OpenCode round names the session it is going to, and a Pi round
/// names none because there is none to name.
fn record_round(
    database: &Database,
    checkout_id: &str,
    provider: AgentProvider,
    target: RoundTarget<'_>,
    note_ids: &[String],
    markdown: &str,
    status: &str,
) -> Result<ReviewRound, IpcError> {
    record_round_with_id(
        database,
        checkout_id,
        provider,
        target,
        note_ids,
        markdown,
        status,
        &new_round_id(),
    )
}

#[allow(clippy::too_many_arguments)]
fn record_round_with_id(
    database: &Database,
    checkout_id: &str,
    provider: AgentProvider,
    target: RoundTarget<'_>,
    note_ids: &[String],
    markdown: &str,
    status: &str,
    id: &str,
) -> Result<ReviewRound, IpcError> {
    if note_ids.is_empty() || note_ids.len() > MAX_ROUND_NOTES {
        return Err(failed(format!(
            "a review round needs between 1 and {MAX_ROUND_NOTES} notes"
        )));
    }
    let session_id = match target {
        // A round with no session is a send with nowhere to go.
        RoundTarget::Session("") => return Err(invalid("a review round needs a target session")),
        RoundTarget::Session(session_id) => Some(session_id),
        RoundTarget::NewPi => None,
    };
    let now = timestamp();
    let round = ReviewRound {
        id: id.to_string(),
        checkout_id: checkout_id.to_string(),
        provider,
        session_id: session_id.map(str::to_string),
        terminal_id: None,
        status: status.into(),
        marker: review_round_marker(id),
        note_ids: note_ids.to_vec(),
        created_at: now.clone(),
        updated_at: now,
    };
    // Stored with the message it will actually be sent with, marker included: a round that
    // goes out later must carry exactly the bytes the user accepted, not a re-derivation.
    check_round_prompt_size(markdown, &round.marker)?;
    let prompt = build_round_prompt(markdown, &round.marker);
    database
        .add_review_round(&round, note_ids, Some(&prompt))
        .map_err(|error| foreign(&error))
}

/// Records that the message reached the agent.
pub fn confirm_round(
    database: &Database,
    checkout_id: &str,
    round_id: &str,
) -> Result<ReviewRound, IpcError> {
    database
        .set_review_round_status(round_id, checkout_id, "dispatching", "dispatched")
        .map_err(|error| foreign(&error))
}

/// Closes a round once its turn finished and the notes were re-checked.
pub fn ack_round(
    database: &Database,
    checkout_id: &str,
    round_id: &str,
) -> Result<ReviewRound, IpcError> {
    database
        .set_review_round_status(round_id, checkout_id, "dispatched", "acked")
        .map_err(|error| foreign(&error))
}

/// Resolves whether a `dispatching` round actually landed, and requeues it if not.
///
/// Called on reconnect. A round whose marker is in the session's transcript is confirmed;
/// one whose marker is absent never reached the agent, so putting it back to `queued` is
/// safe: the notes are not duplicated because the message was never delivered.
pub fn reconcile_round(
    database: &Database,
    agents: &AgentService,
    checkout_id: &str,
    directory: &std::path::Path,
    round_id: &str,
    generation: Option<u64>,
) -> Result<ReviewRound, IpcError> {
    let round = database
        .review_rounds(checkout_id)
        .map_err(failed)?
        .into_iter()
        .find(|item| item.id == round_id)
        .ok_or_else(|| foreign("review round does not belong to the requested checkout"))?;
    if !round.is_pending() || round.provider != AgentProvider::OpenCode {
        return Ok(round);
    }
    let Some(session_id) = round.session_id.clone() else {
        // Never had a target: nothing to check, so it waits for one.
        return requeue(database, checkout_id, &round);
    };
    let landed = match generation {
        Some(generation) => agents.session_mentions_at_generation(
            checkout_id,
            directory,
            &session_id,
            &round.marker,
            generation,
            || {
                database
                    .terminal_checkout_path(checkout_id)
                    .is_ok_and(|current| current.as_path() == directory)
            },
        ),
        None => agents.session_mentions(checkout_id, directory, &session_id, &round.marker),
    }
    .map_err(agent_error)?;
    if landed {
        database
            .set_review_round_status(round_id, checkout_id, "dispatching", "dispatched")
            .map_err(|error| foreign(&error))
    } else {
        requeue(database, checkout_id, &round)
    }
}

fn requeue(
    database: &Database,
    checkout_id: &str,
    round: &ReviewRound,
) -> Result<ReviewRound, IpcError> {
    if round.status == "queued" {
        return Ok(round.clone());
    }
    database
        .set_review_round_status(&round.id, checkout_id, "dispatching", "queued")
        .map_err(|error| foreign(&error))
}

/// Puts every unconfirmed OpenCode round of the checkout back to `queued` so they can be retried.
pub fn requeue_all(database: &Database, checkout_id: &str) -> Result<usize, IpcError> {
    database.requeue_review_rounds(checkout_id).map_err(failed)
}

/// Sends the rounds that are waiting for the agent to be free, oldest first.
///
/// Each round is checked against the transcript before it is sent: a marker that is already
/// there means the message landed earlier, so the round is confirmed instead of sent again.
/// That is what makes this safe to run after a reconnect has requeued a send which in fact
/// went out: the duplicate is what this check exists to prevent.
///
/// Returns how many rounds were actually sent.
pub fn flush_rounds(
    database: &Database,
    agents: &AgentService,
    checkout_id: &str,
    directory: &std::path::Path,
    generation: Option<u64>,
) -> Result<usize, IpcError> {
    // Oldest first: a batch of queued reviews should reach the agent in the order they were
    // written, not in the order the list happens to return them.
    let pending = database
        .review_rounds(checkout_id)
        .map_err(failed)?
        .into_iter()
        .filter(|round| round.status == "queued")
        .collect::<Vec<_>>();
    let mut sent = 0;
    for round in pending.into_iter().rev() {
        // The transcript this flush reads before it sends anything is OpenCode's. A Pi round that
        // ended up `queued` did not fail its send, so there is nothing here that could settle it
        // and nothing that should try.
        if round.provider != AgentProvider::OpenCode {
            continue;
        }
        let Some(session_id) = round.session_id.clone() else {
            // Never had a target, so there is nowhere to send it.
            continue;
        };
        // Read before asking the agent: a round with no stored message cannot go out, so
        // there is no reason to query a server about it.
        let Some(prompt) = database
            .review_round_prompt(&round.id, checkout_id)
            .map_err(failed)?
        else {
            // A round recorded before its message was stored cannot be rebuilt from the
            // notes alone: the Markdown belongs to the UI. It waits rather than guessing.
            continue;
        };
        let landed = match generation {
            Some(generation) => agents.session_mentions_at_generation(
                checkout_id,
                directory,
                &session_id,
                &round.marker,
                generation,
                || {
                    database
                        .terminal_checkout_path(checkout_id)
                        .is_ok_and(|current| current.as_path() == directory)
                },
            ),
            None => agents.session_mentions(checkout_id, directory, &session_id, &round.marker),
        }
        .map_err(agent_error)?;
        if landed {
            database
                .set_review_round_status(&round.id, checkout_id, "queued", "dispatched")
                .map_err(|error| foreign(&error))?;
            continue;
        }
        // Claim before the call, so a second flush cannot send this one while it is in flight.
        database
            .set_review_round_status(&round.id, checkout_id, "queued", "dispatching")
            .map_err(|error| foreign(&error))?;
        let result = match generation {
            Some(generation) => agents.prompt_at_generation(
                checkout_id,
                directory,
                &session_id,
                &prompt,
                generation,
                || {
                    database
                        .terminal_checkout_path(checkout_id)
                        .is_ok_and(|current| current.as_path() == directory)
                },
            ),
            None => agents.prompt(checkout_id, directory, &session_id, &prompt),
        };
        match result {
            Ok(_) => {
                database
                    .set_review_round_status(&round.id, checkout_id, "dispatching", "dispatched")
                    .map_err(|error| foreign(&error))?;
                sent += 1;
            }
            // Left `dispatching` on purpose: the marker is what lets reconciliation learn
            // whether this send landed, and requeueing blindly could duplicate it.
            Err(error) => return Err(agent_error(error)),
        }
    }
    Ok(sent)
}

/// Ties a waiting Pi review round to the session that appeared in the terminal it was launched
/// into.
///
/// **This records identity, not delivery.** The round stays `dispatching`; whether the review was
/// read is settled from a receipt the session reports, which is [`settle_pi_rounds`]'s work and
/// is not something a session appearing can answer.
pub fn bind_pi_round_session(
    database: &Database,
    checkout_id: &str,
    terminal_id: &str,
    session_id: &str,
) -> Result<bool, IpcError> {
    // A read that cannot be written is not a reason to fail a session list: the round waits,
    // and the next read tries again.
    database
        .bind_pi_round_session(checkout_id, terminal_id, session_id)
        .map_err(failed)
}

/// Settles what one Pi session's own reports prove about the rounds it was sent.
///
/// This is the Pi counterpart to `reconcile_round`, and it works from the other end of the same
/// question. There is no transcript to ask and nothing to send: the session names the rounds it
/// was shown, and this asks the bridge what that session has reported.
///
/// Three things are settled, in this order, and none of them is a guess about the row:
///
/// - **Which session received it.** A round waiting for the terminal that session is in is bound
///   to it here rather than at the next session read, because a receipt that arrived before the
///   binding would otherwise have nothing to match.
/// - **Delivery**, and only from a receipt carrying this round's own marker, reported by the
///   session bound to the round in the terminal the launch opened. A receipt of another marker,
///   of another session, of another terminal or of another checkout confirms nothing.
/// - **Completion**, and only from the settlement Pi announced. `turn_end` and `agent_end` are
///   boundaries Pi can keep working past, and neither clears the flag this reads.
///
/// Nothing here can start a Pi. A round this leaves unconfirmed is left where it is, because the
/// only route to a Pi is opening one, and opening one is the user's send and nobody else's.
///
/// Idempotent throughout: every transition is refused once taken, so being asked again after a
/// reconnect or a second settlement changes nothing.
pub fn settle_pi_rounds(
    database: &Database,
    agents: &AgentService,
    checkout_id: &str,
    session_id: &str,
) -> Result<Vec<ReviewRound>, IpcError> {
    let Some(delivery) = agents.pi().delivery(checkout_id, session_id) else {
        return Ok(Vec::new());
    };
    let round_checkout_id = &delivery.round_checkout_id;
    let _ = bind_pi_round_session(
        database,
        round_checkout_id,
        &delivery.terminal_id,
        session_id,
    );
    let markers: Vec<&str> = delivery
        .receipts
        .iter()
        .map(|receipt| receipt.marker.as_str())
        .collect();
    let mut settled = Vec::new();
    for round in database
        .review_rounds(round_checkout_id)
        .map_err(failed)?
        .into_iter()
        .filter(|round| {
            round.provider == AgentProvider::Pi
                && round.terminal_id.as_deref() == Some(delivery.terminal_id.as_str())
                && round.session_id.as_deref() == Some(session_id)
        })
    {
        // Delivery is one-way, so a round that is already past it is left for the settlement
        // below rather than being reported again as a confirmation.
        if round.status == "dispatching" && markers.contains(&round.marker.as_str()) {
            if let Some(confirmed) = database
                .confirm_pi_round_delivery(
                    round_checkout_id,
                    &delivery.terminal_id,
                    session_id,
                    &round.marker,
                )
                .map_err(|error| foreign(&error))?
            {
                settled.push(confirmed);
            }
        }
    }
    if delivery.settled {
        if let Some(acked) = database
            .ack_pi_round(round_checkout_id, &delivery.terminal_id, session_id)
            .map_err(|error| foreign(&error))?
        {
            settled.push(acked);
        }
    }
    Ok(settled)
}

/// Marks an outstanding note as resolved by the user.
pub fn resolve_note(
    database: &Database,
    checkout_id: &str,
    id: &str,
) -> Result<ReviewNote, IpcError> {
    database
        .resolve_review_note(id, checkout_id)
        .map_err(|error| foreign(&error))
}

/// Rejects a missing review body before any launch preparation has side effects.
pub fn check_review_markdown(markdown: &str) -> Result<(), IpcError> {
    if markdown.trim().is_empty() {
        return Err(failed("a review round cannot be empty"));
    }
    Ok(())
}

/// Checks the exact final prompt size without copying the review body.
pub fn check_round_prompt_size(markdown: &str, marker: &str) -> Result<(), IpcError> {
    check_review_markdown(markdown)?;
    let prompt_bytes = AGENT_PROMPT_PREFIX.len() + marker.len() + markdown.len() + 4;
    if prompt_bytes > MAX_ROUND_PROMPT_BYTES {
        return Err(failed(
            "the review is too large to send to the agent in one message",
        ));
    }
    Ok(())
}

/// Rejects an already-built prompt that cannot be delivered as one message.
pub fn check_prompt_size(prompt: &str) -> Result<(), IpcError> {
    if prompt.trim().is_empty() {
        return Err(failed("a review round cannot be empty"));
    }
    if prompt.len() > MAX_ROUND_PROMPT_BYTES {
        return Err(failed(
            "the review is too large to send to the agent in one message",
        ));
    }
    Ok(())
}

pub fn new_round_id() -> String {
    format!(
        "round:{}:{}:{}",
        timestamp(),
        std::process::id(),
        NEXT_ROUND_ID.fetch_add(1, Ordering::Relaxed)
    )
}

/// The instructions and marker that `build_round_prompt` prepends to a round's review.
///
/// The caller knows the review body but not the marker yet, so this is how much room the
/// prefix needs, letting the size be checked before anything is recorded.
pub fn marker_budget() -> usize {
    AGENT_PROMPT_PREFIX.len() + ROUND_MARKER_PREFIX.len() + 128
}

pub fn agent_error(error: crate::services::agent::BridgeError) -> IpcError {
    crate::services::agent::map_error(error)
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        path::{Path, PathBuf},
        thread,
    };

    use tempfile::{tempdir, TempDir};

    use crate::{
        domain::{
            agent::AgentProvider,
            ipc::IpcError,
            review::{
                review_anchor_hash, review_round_marker, ReviewNote, ReviewRound,
                AGENT_PROMPT_PREFIX, MAX_ROUND_PROMPT_BYTES,
            },
        },
        persistence::Database,
        services::agent::AgentService,
    };

    use super::{
        ack_round, begin_pi_round, begin_round, bind_pi_round, build_round_prompt,
        check_prompt_size, check_review_markdown, check_round_prompt_size, confirm_round,
        flush_rounds, queue_round, reconcile_round, requeue_all, resolve_note, rounds,
        settle_pi_rounds,
    };

    /// Every round below is an OpenCode round, so the provider is stated once here rather than
    /// at each call site. A Pi round is written the other way round and is covered separately.
    fn opencode_round(
        database: &Database,
        checkout_id: &str,
        session_id: &str,
        note_ids: &[String],
        markdown: &str,
    ) -> Result<ReviewRound, IpcError> {
        begin_round(
            database,
            checkout_id,
            AgentProvider::OpenCode,
            session_id,
            note_ids,
            markdown,
        )
    }

    fn mock_json_responses(
        responses: Vec<serde_json::Value>,
    ) -> (u16, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock server should bind");
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            responses
                .into_iter()
                .map(|response| {
                    let (mut stream, _) = listener.accept().expect("request should connect");
                    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                    let mut request_line = String::new();
                    reader
                        .read_line(&mut request_line)
                        .expect("request line should be readable");
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).expect("headers should be readable");
                        if line == "\r\n" || line.is_empty() {
                            break;
                        }
                    }
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
                    request_line.trim().to_string()
                })
                .collect()
        });
        (port, server)
    }

    fn note(overrides: &ReviewNote) -> ReviewNote {
        ReviewNote {
            id: format!("note:{}", overrides.id),
            checkout_id: overrides.checkout_id.clone(),
            path: "src/foo.js".into(),
            side: "new".into(),
            line_start: 10,
            line_end: None,
            content: "check this".into(),
            code: "const result = a + b;".into(),
            status: "draft".into(),
            code_hash: review_anchor_hash("const result = a + b;"),
            outdated: false,
            round_id: None,
            created_at: "1".into(),
            updated_at: "1".into(),
        }
    }

    fn database_with_two_checkouts(temp: &TempDir) -> (Database, String, String) {
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        let first = crate::domain::workspace::Repo::plain(&first, "now").unwrap();
        let second = crate::domain::workspace::Repo::plain(&second, "now").unwrap();
        database.register_plain_repo(first.clone()).unwrap();
        database.register_plain_repo(second.clone()).unwrap();
        (
            database,
            first.checkouts[0].id.clone(),
            second.checkouts[0].id.clone(),
        )
    }

    fn add_note(database: &Database, checkout_id: &str, id: &str) {
        database
            .add_review_note(&note(&ReviewNote {
                id: id.into(),
                checkout_id: checkout_id.into(),
                path: "src/foo.js".into(),
                side: "new".into(),
                line_start: 10,
                line_end: None,
                content: "check this".into(),
                code: "const result = a + b;".into(),
                status: "draft".into(),
                code_hash: String::new(),
                outdated: false,
                round_id: None,
                created_at: "1".into(),
                updated_at: "1".into(),
            }))
            .unwrap();
    }

    #[test]
    fn a_round_is_recorded_before_it_is_sent_and_carries_its_marker() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "a");

        let round = opencode_round(
            &database,
            &checkout_id,
            "ses_target",
            &["note:a".to_string()],
            "# Code Review",
        )
        .unwrap();
        assert_eq!(round.status, "dispatching");
        assert_eq!(round.session_id.as_deref(), Some("ses_target"));
        assert!(round.marker.starts_with("muster-review:"));

        // The note is already linked and marked sent before any agent call happens.
        let stored = &database.review_notes(&checkout_id).unwrap()[0];
        assert_eq!(stored.status, "sent");
        assert_eq!(stored.round_id.as_deref(), Some(round.id.as_str()));

        let prompt = build_round_prompt("# Code Review", &round.marker);
        assert!(prompt.contains(&round.marker));

        let confirmed = confirm_round(&database, &checkout_id, &round.id).unwrap();
        assert_eq!(confirmed.status, "dispatched");
        let acked = ack_round(&database, &checkout_id, &round.id).unwrap();
        assert_eq!(acked.status, "acked");
        // A closed round cannot be moved again.
        assert!(ack_round(&database, &checkout_id, &round.id).is_err());
        assert_eq!(rounds(&database, &checkout_id).unwrap().len(), 1);
    }

    #[test]
    fn an_unconfirmed_round_is_requeued_for_retry() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "a");
        let round = opencode_round(
            &database,
            &checkout_id,
            "ses_target",
            &["note:a".to_string()],
            "# Code Review",
        )
        .unwrap();

        assert_eq!(requeue_all(&database, &checkout_id).unwrap(), 1);
        let requeued = &rounds(&database, &checkout_id).unwrap()[0];
        assert_eq!(requeued.status, "queued");
        // Requeuing twice must not double count or fail.
        assert_eq!(requeue_all(&database, &checkout_id).unwrap(), 0);
        assert_eq!(requeued.id, round.id);
    }

    #[test]
    fn reconciliation_confirms_a_present_marker_and_requeues_an_absent_one() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "landed");
        add_note(&database, &checkout_id, "missing");
        let landed = opencode_round(
            &database,
            &checkout_id,
            "ses_target",
            &["note:landed".to_string()],
            "# Landed",
        )
        .unwrap();
        let missing = opencode_round(
            &database,
            &checkout_id,
            "ses_target",
            &["note:missing".to_string()],
            "# Missing",
        )
        .unwrap();
        let directory = temp.path().join("first");
        let session = serde_json::json!({
            "id": "ses_target",
            "location": {"directory": directory.to_string_lossy()}
        });
        let (port, server) = mock_json_responses(vec![
            serde_json::json!({"data": session.clone()}),
            serde_json::json!({"data": [{"text": landed.marker}]}),
            serde_json::json!({"data": session}),
            serde_json::json!({"data": [{"text": "unrelated transcript"}]}),
        ]);
        let agents = AgentService::with_test_server(port);

        let confirmed = reconcile_round(
            &database,
            &agents,
            &checkout_id,
            &directory,
            &landed.id,
            None,
        )
        .unwrap();
        assert_eq!(confirmed.status, "dispatched");
        let retried = reconcile_round(
            &database,
            &agents,
            &checkout_id,
            &directory,
            &missing.id,
            None,
        )
        .unwrap();
        assert_eq!(retried.status, "queued");
        // Compared on the route: each request also carries `?directory=…`, which is what scopes
        // the answer to this checkout. See the scoping test in `services::agent`.
        assert_eq!(
            server
                .join()
                .expect("mock server should finish")
                .iter()
                .map(|line| line.split('?').next().unwrap_or_default().to_string())
                .collect::<Vec<_>>(),
            [
                "GET /api/session/ses_target",
                "GET /api/session/ses_target/message",
                "GET /api/session/ses_target",
                "GET /api/session/ses_target/message",
            ]
        );
    }

    #[test]
    fn a_relocated_round_is_not_reconciled_against_its_old_session() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "relocated");
        let round = queue_round(
            &database,
            &checkout_id,
            "ses_old_checkout",
            &["note:relocated".to_string()],
            "# Review",
        )
        .unwrap();
        let relocated = database
            .set_review_round_status(&round.id, &checkout_id, "queued", "relocated")
            .unwrap();
        // Any marker lookup would fail against this closed local test port; no provider is started.
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let agents = AgentService::with_test_server(port);
        let directory = temp.path().join("first");

        let reconciled = reconcile_round(
            &database,
            &agents,
            &checkout_id,
            &directory,
            &round.id,
            None,
        )
        .unwrap();

        assert_eq!(reconciled, relocated);
        assert_eq!(reconciled.status, "relocated");
    }

    #[test]
    fn a_queued_round_stores_the_message_it_will_be_sent_with() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "a");

        let round = queue_round(
            &database,
            &checkout_id,
            "ses_target",
            &["note:a".to_string()],
            "# Code Review",
        )
        .unwrap();
        assert_eq!(round.status, "queued");
        // The notes are committed to it before it goes out, exactly as an immediate send is.
        assert_eq!(
            database.review_notes(&checkout_id).unwrap()[0].status,
            "sent"
        );

        // What is stored is the whole message, marker included, so the send that happens
        // later carries the bytes the user accepted rather than a re-derivation.
        let stored = database
            .review_round_prompt(&round.id, &checkout_id)
            .unwrap()
            .expect("a queued round has its message");
        assert_eq!(stored, build_round_prompt("# Code Review", &round.marker));
        assert!(stored.contains(&round.marker));
        // Another checkout cannot read it.
        assert!(database
            .review_round_prompt(&round.id, "checkout:other")
            .unwrap()
            .is_none());
    }

    #[test]
    fn flushing_a_round_without_a_stored_message_leaves_it_waiting() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "a");
        // A round written before its message was stored: the notes are there, the body is not.
        let round = ReviewRound {
            id: "round:legacy".into(),
            checkout_id: checkout_id.clone(),
            provider: AgentProvider::OpenCode,
            session_id: Some("ses_target".into()),
            terminal_id: None,
            status: "queued".into(),
            marker: review_round_marker("round:legacy"),
            note_ids: vec!["note:a".to_string()],
            created_at: "1".into(),
            updated_at: "1".into(),
        };
        database
            .add_review_round(&round, &["note:a".to_string()], None)
            .unwrap();
        // It has nothing to send. It must not be guessed from the notes, and it must not be
        // dropped either.

        let agents = AgentService::without_service();
        // The directory does not exist, so reaching a server would fail loudly if tried.
        let sent = flush_rounds(
            &database,
            &agents,
            &checkout_id,
            Path::new("/nonexistent"),
            None,
        )
        .expect("a round with no message is skipped without touching a server");
        assert_eq!(sent, 0);
        assert_eq!(rounds(&database, &checkout_id).unwrap()[0].status, "queued");
    }

    #[test]
    fn pi_review_validation_checks_the_exact_final_size_and_rejects_empty_body() {
        let marker = review_round_marker("round:1:2:3");
        let fixed_bytes = AGENT_PROMPT_PREFIX.len() + marker.len() + 4;
        let body = "x".repeat(MAX_ROUND_PROMPT_BYTES - fixed_bytes);
        check_review_markdown(&body).expect("a nonempty review is valid");
        check_round_prompt_size(&body, &marker).expect("the exact limit is accepted");
        assert_eq!(
            build_round_prompt(&body, &marker).len(),
            MAX_ROUND_PROMPT_BYTES
        );

        let oversized = format!("{body}x");
        assert!(check_round_prompt_size(&oversized, &marker).is_err());
        assert!(check_round_prompt_size(" \n\t", &marker).is_err());
    }

    #[test]
    fn only_outstanding_notes_of_this_checkout_can_be_resolved() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, other_checkout_id) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "a");
        add_note(&database, &checkout_id, "b");
        let ids = vec!["note:a".to_string()];
        opencode_round(&database, &checkout_id, "ses_target", &ids, "# Code Review").unwrap();

        let resolved = resolve_note(&database, &checkout_id, "note:a").unwrap();
        assert_eq!(resolved.status, "resolved");
        // A draft was never outstanding, and another checkout's note is not ours.
        assert!(resolve_note(&database, &checkout_id, "note:b").is_err());
        assert!(resolve_note(&database, &other_checkout_id, "note:a").is_err());
        // Resolving twice is not a transition.
        assert!(resolve_note(&database, &checkout_id, "note:a").is_err());
    }

    #[test]
    fn sec_10_a_round_cannot_carry_notes_from_another_checkout() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, other_checkout_id) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "a");
        add_note(&database, &other_checkout_id, "foreign");
        let agents = AgentService::without_service();

        assert!(opencode_round(
            &database,
            &checkout_id,
            "ses_target",
            &["note:foreign".to_string()],
            "# Code Review",
        )
        .is_err());
        // Nothing was written, so a foreign note is not even linked.
        assert!(database.review_rounds(&checkout_id).unwrap().is_empty());
        assert_eq!(
            database.review_notes(&checkout_id).unwrap()[0].status,
            "draft"
        );

        // And a round of this checkout cannot be confirmed through another one.
        let round = opencode_round(
            &database,
            &checkout_id,
            "ses_target",
            &["note:a".to_string()],
            "# Code Review",
        )
        .unwrap();
        assert!(super::reconcile_round(
            &database,
            &agents,
            &other_checkout_id,
            Path::new("/nonexistent"),
            &round.id,
            None,
        )
        .is_err());
    }

    #[test]
    fn the_prompt_is_the_instructions_then_the_marker_then_the_review() {
        // The marker is the deduplication key, so it must be inside the message the agent
        // receives and not a side channel Muster keeps to itself.
        let prompt = build_round_prompt("# Code Review 2026-09-26", "muster-review:round:1");
        assert!(prompt.starts_with(crate::domain::review::AGENT_PROMPT_PREFIX));
        let marker_at = prompt
            .find("[muster-review:round:1]")
            .expect("marker in the prompt");
        let review_at = prompt.find("# Code Review").expect("review in the prompt");
        assert!(
            marker_at < review_at,
            "the marker must come before the review"
        );
    }

    /// Q4 against a real `opencode serve`: a round held back while the agent was busy goes
    /// out on its own once flushed, and a second flush does not send it twice.
    ///
    /// `cargo test a_queued_round_goes_out_when_flushed -- --ignored --nocapture`
    #[ignore = "live OpenCode integration; requires a configured provider and sends a prompt"]
    #[test]
    fn a_queued_round_goes_out_when_flushed() {
        let directory = std::env::var("MUSTER_AGENT_BRIDGE_DIR")
            .ok()
            .filter(|directory| !directory.is_empty())
            .map(PathBuf::from)
            .expect(
                "MUSTER_AGENT_BRIDGE_DIR is required for this ignored live test; set it to a dedicated temporary directory (see docs/agent-live-tests.md)",
            );
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "a");

        let agents = AgentService::without_service();
        let session = agents
            .create_session(&checkout_id, &directory, "queue probe")
            .expect("create a session on the real server");
        let round = queue_round(
            &database,
            &checkout_id,
            &session.id,
            &["note:a".to_string()],
            "# Code Review",
        )
        .unwrap();
        assert_eq!(round.status, "queued");
        // Held back: recording the round must not have talked to the agent.
        assert!(
            !agents
                .session_mentions(&checkout_id, &directory, &session.id, &round.marker)
                .expect("read the transcript"),
            "the marker reached the session before it was flushed"
        );

        assert_eq!(
            flush_rounds(&database, &agents, &checkout_id, &directory, None).expect("flush"),
            1
        );
        assert_eq!(
            rounds(&database, &checkout_id).unwrap()[0].status,
            "dispatched"
        );
        assert!(
            agents
                .session_mentions(&checkout_id, &directory, &session.id, &round.marker)
                .expect("read the transcript"),
            "the flushed round never reached the session"
        );

        // Nothing is queued any more, so flushing again sends nothing.
        assert_eq!(
            flush_rounds(&database, &agents, &checkout_id, &directory, None).expect("flush again"),
            0
        );
        agents.stop(&checkout_id);
    }

    #[test]
    fn a_round_needs_a_target_and_at_least_one_note() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "a");

        assert!(opencode_round(&database, &checkout_id, "ses_target", &[], "#").is_err());
        assert!(opencode_round(&database, &checkout_id, "", &["note:a".into()], "#").is_err());
        assert!(check_prompt_size("   ").is_err());
        assert!(check_prompt_size("fix the review").is_ok());
        assert!(check_prompt_size(&"x".repeat(600 * 1024)).is_err());
    }

    /// What a Pi round may claim at each moment, and what it may not.
    ///
    /// Recorded before a terminal exists, so it names the provider and nothing else. Bound to the
    /// terminal that was created for it, then to the session that appeared in that terminal, and
    /// **`dispatching` throughout** — because a terminal running and a session appearing say a Pi
    /// was started, not that it read the review.
    #[test]
    fn a_pi_round_names_its_terminal_then_its_session_and_is_never_delivered_here() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "one");

        let round = begin_pi_round(
            &database,
            &checkout_id,
            &["note:one".to_string()],
            "# Code Review\n\n> needs quotes: ' and $(rm -rf /)\n",
        )
        .expect("a Pi round is recorded");
        assert_eq!(round.provider, AgentProvider::Pi);
        assert_eq!(round.status, "dispatching");
        assert_eq!(
            round.session_id, None,
            "a round cannot name a session that has not been started"
        );
        assert_eq!(round.terminal_id, None);

        // The message travels with the round, marker included, so what Pi is given is exactly what
        // the round will later be recognised by.
        let prompt = database
            .review_round_prompt(&round.id, &checkout_id)
            .unwrap()
            .expect("the prompt is stored with the round");
        assert!(prompt.contains(&round.marker), "{prompt}");

        let bound = bind_pi_round(&database, &checkout_id, &round.id, "session:terminal:1")
            .expect("the terminal is bound");
        assert_eq!(bound.terminal_id.as_deref(), Some("session:terminal:1"));
        assert_eq!(bound.session_id, None);
        assert_eq!(bound.status, "dispatching", "a terminal is not a delivery");

        assert!(database
            .bind_pi_round_session(&checkout_id, "session:terminal:1", "ses_pi")
            .unwrap());
        let bound = database
            .review_rounds(&checkout_id)
            .unwrap()
            .into_iter()
            .find(|item| item.id == round.id)
            .unwrap();
        assert_eq!(bound.session_id.as_deref(), Some("ses_pi"));
        assert_eq!(
            bound.status, "dispatching",
            "a session appearing is not proof that it read anything"
        );

        // Binding again is refused rather than rewriting the session that received the review.
        assert!(!database
            .bind_pi_round_session(&checkout_id, "session:terminal:1", "ses_other")
            .unwrap());
        // And a session in some other terminal is not this round's to claim.
        assert!(!database
            .bind_pi_round_session(&checkout_id, "session:terminal:other", "ses_other")
            .unwrap());
    }

    /// The reconciliation machinery is OpenCode's, and none of it may move a Pi round.
    ///
    /// A Pi round is pending for a different reason and cannot be settled by looking for its marker
    /// in a transcript, so `requeue_all`, `reconcile_round` and `flush_rounds` must leave it exactly
    /// where it is rather than putting it in a state nothing will ever act on.
    #[test]
    fn opencode_reconciliation_leaves_a_pi_round_where_it_is() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "pi");
        add_note(&database, &checkout_id, "opencode");
        let pi = begin_pi_round(
            &database,
            &checkout_id,
            &["note:pi".to_string()],
            "# Code Review",
        )
        .unwrap();
        let opencode = opencode_round(
            &database,
            &checkout_id,
            "ses_one",
            &["note:opencode".to_string()],
            "# Code Review",
        )
        .unwrap();

        assert_eq!(
            requeue_all(&database, &checkout_id).unwrap(),
            1,
            "only the OpenCode round is retryable"
        );
        let statuses = |id: &str| {
            database
                .review_rounds(&checkout_id)
                .unwrap()
                .into_iter()
                .find(|item| item.id == id)
                .unwrap()
                .status
        };
        assert_eq!(statuses(&pi.id), "dispatching");
        assert_eq!(statuses(&opencode.id), "queued");

        // Reconciliation asks a session whether its marker arrived. A Pi round answers before it
        // gets that far, and the OpenCode round cannot be checked at all with no service running —
        // neither of which is allowed to move the Pi round.
        let directory = temp.path().join("first");
        reconcile_round(
            &database,
            &AgentService::without_service(),
            &checkout_id,
            &directory,
            &pi.id,
            None,
        )
        .expect("a Pi round is not the reconciler's business at all");
        assert_eq!(statuses(&pi.id), "dispatching");
        let _ = flush_rounds(
            &database,
            &AgentService::without_service(),
            &checkout_id,
            &directory,
            None,
        );
        assert_eq!(
            statuses(&pi.id),
            "dispatching",
            "a flush is not something that can act on a Pi round"
        );
    }

    /// A round with nothing to send to is refused at record time, whichever provider it is.
    #[test]
    fn a_round_without_a_target_is_refused() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "one");
        assert!(
            begin_pi_round(&database, &checkout_id, &["note:one".to_string()], "review").is_ok()
        );
        assert!(begin_round(
            &database,
            &checkout_id,
            AgentProvider::OpenCode,
            "",
            &["note:one".to_string()],
            "review",
        )
        .is_err());
    }

    // -----------------------------------------------------------------------------------------
    // What a Pi session reports, and what it may therefore settle.
    //
    // Everything below drives the real bridge over a real socket rather than reaching into it: a
    // receipt only means anything because of the credential that carried it, and a test that
    // skipped the handshake would be asserting against a shape rather than against the trust.
    // -----------------------------------------------------------------------------------------

    /// A reporter that speaks the wire protocol from the outside.
    struct Reporter {
        stream: std::net::TcpStream,
    }

    impl Reporter {
        fn connect(bridge: &crate::services::pi::PiBridge) -> Self {
            let address = bridge.address().expect("the bridge is listening");
            let mut last = None;
            for _ in 0..50 {
                match std::net::TcpStream::connect(address) {
                    Ok(stream) => return Self { stream },
                    Err(error) => {
                        last = Some(error);
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                }
            }
            panic!("could not reach the bridge: {last:?}");
        }

        fn send(&mut self, frame: serde_json::Value) {
            use std::io::Write;
            self.stream
                .write_all(format!("{frame}\n").as_bytes())
                .unwrap();
            let _ = self.stream.flush();
        }

        /// Introduces this session, optionally carrying the reviews it was shown.
        fn hello(
            &mut self,
            token: &str,
            terminal_id: &str,
            checkout_id: &str,
            session_id: &str,
            receipts: serde_json::Value,
        ) {
            self.send(serde_json::json!({
                "v": crate::services::pi::PROTOCOL_VERSION,
                "type": "hello",
                "token": token,
                "terminalId": terminal_id,
                "checkoutId": checkout_id,
                "sessionId": session_id,
                "running": false,
                "awaiting": false,
                "receipts": receipts,
                "work": {
                    "generation": "review-round-test",
                    "sequence": 0,
                    "targets": [],
                    "ambiguous": false,
                },
            }));
        }

        fn state(&mut self, running: bool) {
            self.send(serde_json::json!({
                "v": crate::services::pi::PROTOCOL_VERSION,
                "type": "state",
                "running": running,
                "awaiting": false,
                "work": {
                    "generation": "review-round-test",
                    "sequence": 0,
                    "targets": [],
                    "ambiguous": false,
                },
            }));
        }
    }

    fn receipt(marker: &str, entry: &str) -> serde_json::Value {
        serde_json::json!({"marker": marker, "entry": entry})
    }

    /// Waits for the bridge's reporting thread to have applied what was written to it.
    fn eventually(what: &str, check: impl Fn() -> bool) {
        for _ in 0..200 {
            if check() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("{what} did not happen");
    }

    /// A launched Pi round, bound to its terminal and to the session reported in it.
    ///
    /// The session is left bound to it, which is what a launch settles and what the tests about
    /// scoping would otherwise have to re-derive.
    fn launched_pi_round(
        database: &Database,
        checkout_id: &str,
        terminal_id: &str,
        session_id: &str,
    ) -> ReviewRound {
        let round = begin_pi_round(
            database,
            checkout_id,
            &["note:one".to_string()],
            "# Code Review",
        )
        .unwrap();
        let round = bind_pi_round(database, checkout_id, &round.id, terminal_id).unwrap();
        database
            .bind_pi_round_session(checkout_id, terminal_id, session_id)
            .unwrap();
        round
    }

    fn credential(agents: &AgentService, terminal_id: &str, checkout_id: &str) -> String {
        let credential = agents
            .pi()
            .issue_credential(terminal_id, checkout_id)
            .expect("a credential");
        credential.rsplit(':').next().unwrap().to_string()
    }

    fn status_of(database: &Database, checkout_id: &str, round: &ReviewRound) -> String {
        database
            .review_rounds(checkout_id)
            .unwrap()
            .into_iter()
            .find(|item| item.id == round.id)
            .unwrap()
            .status
    }

    /// The whole claim: delivery is confirmed by the marker the session reported reading, and
    /// only then does a settlement close the round. Both are one-way, so asking again after a
    /// reconnect is not a second delivery.
    #[test]
    fn a_reported_marker_confirms_delivery_and_a_settlement_closes_it() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "one");
        let agents = AgentService::without_service();
        let token = credential(&agents, "terminal:one", &checkout_id);
        let round = launched_pi_round(&database, &checkout_id, "terminal:one", "ses_pi");

        // A session that has said nothing has proved nothing, however long it has been running.
        let mut reporter = Reporter::connect(agents.pi());
        reporter.hello(
            &token,
            "terminal:one",
            &checkout_id,
            "ses_pi",
            serde_json::json!([]),
        );
        eventually("the session reported itself", || {
            agents.pi().delivery(&checkout_id, "ses_pi").is_some()
        });
        assert!(settle_pi_rounds(&database, &agents, &checkout_id, "ses_pi")
            .expect("a quiet session is not a failure")
            .is_empty());
        assert_eq!(status_of(&database, &checkout_id, &round), "dispatching");

        // The marker this round carries, reported by the session it was bound to.
        let mut second = Reporter::connect(agents.pi());
        second.hello(
            &token,
            "terminal:one",
            &checkout_id,
            "ses_pi",
            serde_json::json!([receipt(&round.marker, "entry:one")]),
        );
        eventually("the receipt was reported", || {
            agents
                .pi()
                .delivery(&checkout_id, "ses_pi")
                .is_some_and(|delivery| !delivery.receipts.is_empty())
        });
        let settled = settle_pi_rounds(&database, &agents, &checkout_id, "ses_pi").unwrap();
        assert_eq!(
            settled
                .iter()
                .map(|item| item.status.clone())
                .collect::<Vec<_>>(),
            vec!["dispatched"],
            "a reported marker is the only thing that confirms delivery"
        );
        assert_eq!(status_of(&database, &checkout_id, &round), "dispatched");

        // Delivered is not finished. Only the settlement Pi announced closes the round, and it
        // is the run that follows the delivery that counts.
        assert!(settle_pi_rounds(&database, &agents, &checkout_id, "ses_pi")
            .unwrap()
            .is_empty());
        assert_eq!(
            status_of(&database, &checkout_id, &round),
            "dispatched",
            "a delivery was reported as a finished turn"
        );
        second.state(true);
        eventually("the turn started", || {
            agents
                .pi()
                .sessions(&checkout_id)
                .first()
                .is_some_and(|session| session.running)
        });
        assert_eq!(status_of(&database, &checkout_id, &round), "dispatched");
        second.state(false);
        eventually("the turn settled", || {
            agents
                .pi()
                .delivery(&checkout_id, "ses_pi")
                .is_some_and(|delivery| delivery.settled)
        });
        let settled = settle_pi_rounds(&database, &agents, &checkout_id, "ses_pi").unwrap();
        assert_eq!(
            settled
                .iter()
                .map(|item| item.status.clone())
                .collect::<Vec<_>>(),
            vec!["acked"]
        );
        assert_eq!(status_of(&database, &checkout_id, &round), "acked");

        // Asking again moves nothing. A reconnect re-reports everything it knows, and the whole
        // of it has to be a no-op rather than a second delivery of a review already acted on.
        assert!(settle_pi_rounds(&database, &agents, &checkout_id, "ses_pi")
            .unwrap()
            .is_empty());
        assert_eq!(status_of(&database, &checkout_id, &round), "acked");
    }

    #[test]
    fn a_moved_pi_terminal_settles_its_round_in_the_original_checkout() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, target_checkout_id) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "one");
        let agents = AgentService::without_service();
        let token = credential(&agents, "terminal:one", &checkout_id);
        let round = launched_pi_round(&database, &checkout_id, "terminal:one", "ses_pi");

        let mut first = Reporter::connect(agents.pi());
        first.hello(
            &token,
            "terminal:one",
            &checkout_id,
            "ses_pi",
            serde_json::json!([]),
        );
        eventually("the initial session appeared", || {
            agents.pi().delivery(&checkout_id, "ses_pi").is_some()
        });
        agents
            .pi()
            .rehome_terminal("terminal:one", &checkout_id, &target_checkout_id);
        drop(first);

        let mut moved = Reporter::connect(agents.pi());
        moved.hello(
            &token,
            "terminal:one",
            &checkout_id,
            "ses_pi",
            serde_json::json!([receipt(&round.marker, "entry:one")]),
        );
        eventually("the moved session reported its receipt", || {
            agents
                .pi()
                .delivery(&target_checkout_id, "ses_pi")
                .is_some_and(|delivery| !delivery.receipts.is_empty())
        });
        assert_eq!(
            settle_pi_rounds(&database, &agents, &target_checkout_id, "ses_pi").unwrap()[0].status,
            "dispatched"
        );
        assert_eq!(status_of(&database, &checkout_id, &round), "dispatched");

        moved.state(true);
        eventually("the moved turn started", || {
            agents
                .pi()
                .sessions(&target_checkout_id)
                .first()
                .is_some_and(|session| session.running)
        });
        moved.state(false);
        eventually("the moved turn settled", || {
            agents
                .pi()
                .delivery(&target_checkout_id, "ses_pi")
                .is_some_and(|delivery| delivery.settled)
        });
        assert_eq!(
            settle_pi_rounds(&database, &agents, &target_checkout_id, "ses_pi").unwrap()[0].status,
            "acked"
        );
        assert_eq!(status_of(&database, &checkout_id, &round), "acked");
    }

    /// A round is confirmed by its own marker, reported by the session bound to it, in the
    /// terminal its launch opened, of the checkout that launch was for. Every one of those is
    /// checked, because a receipt that names a real round from anywhere else is still not proof
    /// that this round was read.
    #[test]
    fn a_receipt_only_confirms_the_round_whose_own_session_reported_it() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, other_checkout_id) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "one");
        add_note(&database, &other_checkout_id, "foreign");
        let agents = AgentService::without_service();
        let round = launched_pi_round(&database, &checkout_id, "terminal:one", "ses_pi");

        // A session of another terminal, reporting this round's own marker.
        let other_token = credential(&agents, "terminal:other", &checkout_id);
        let mut elsewhere = Reporter::connect(agents.pi());
        elsewhere.hello(
            &other_token,
            "terminal:other",
            &checkout_id,
            "ses_other",
            serde_json::json!([receipt(&round.marker, "entry:one")]),
        );
        // And a session of another checkout, in this round's own terminal name.
        let foreign_token = credential(&agents, "terminal:foreign", &other_checkout_id);
        let mut foreign = Reporter::connect(agents.pi());
        foreign.hello(
            &foreign_token,
            "terminal:foreign",
            &other_checkout_id,
            "ses_foreign",
            serde_json::json!([receipt(&round.marker, "entry:one")]),
        );
        // And this round's own session, reporting a marker that is not this round's.
        let token = credential(&agents, "terminal:one", &checkout_id);
        let mut reporter = Reporter::connect(agents.pi());
        reporter.hello(
            &token,
            "terminal:one",
            &checkout_id,
            "ses_pi",
            serde_json::json!([receipt(
                &review_round_marker("round:1762000000:4242:999"),
                "entry:other"
            )]),
        );
        eventually("all three sessions reported themselves", || {
            agents.pi().sessions(&checkout_id).len() == 2
                && agents.pi().sessions(&other_checkout_id).len() == 1
        });

        for (checkout, session) in [
            (&checkout_id, "ses_other"),
            (&other_checkout_id, "ses_foreign"),
            (&checkout_id, "ses_pi"),
        ] {
            assert!(
                settle_pi_rounds(&database, &agents, checkout, session)
                    .unwrap()
                    .is_empty(),
                "{session} confirmed a round it was never sent"
            );
        }
        assert_eq!(
            status_of(&database, &checkout_id, &round),
            "dispatching",
            "a receipt from the wrong session, terminal or checkout confirmed this round"
        );
        // And the foreign checkout gained nothing at all out of being named.
        assert!(database
            .review_rounds(&other_checkout_id)
            .unwrap()
            .is_empty());
        assert_eq!(
            database.review_notes(&other_checkout_id).unwrap()[0].status,
            "draft"
        );
    }

    /// A round and its confirmation survive the app that recorded them.
    ///
    /// A Pi's terminal is closed when Muster exits, so there is no session left to report
    /// anything afterwards. What must survive is the round's own state, because the next
    /// process reads it rather than re-deriving it — and a round that has been confirmed must
    /// not be treated as a send that still needs one.
    #[test]
    fn a_confirmation_is_still_there_after_the_process_that_recorded_it_is_gone() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("workspace.sqlite3");
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "one");
        let agents = AgentService::without_service();
        let token = credential(&agents, "terminal:one", &checkout_id);
        let round = launched_pi_round(&database, &checkout_id, "terminal:one", "ses_pi");
        let mut reporter = Reporter::connect(agents.pi());
        reporter.hello(
            &token,
            "terminal:one",
            &checkout_id,
            "ses_pi",
            serde_json::json!([receipt(&round.marker, "entry:one")]),
        );
        eventually("the receipt was reported", || {
            agents
                .pi()
                .delivery(&checkout_id, "ses_pi")
                .is_some_and(|delivery| !delivery.receipts.is_empty())
        });
        settle_pi_rounds(&database, &agents, &checkout_id, "ses_pi").unwrap();
        drop(reporter);
        drop(agents);
        drop(database);

        let reopened = Database::open(&path).unwrap();
        let saved = &reopened.review_rounds(&checkout_id).unwrap()[0];
        assert_eq!(saved.id, round.id);
        assert_eq!(saved.status, "dispatched");
        assert_eq!(saved.marker, round.marker);
        assert_eq!(saved.terminal_id.as_deref(), Some("terminal:one"));
        assert_eq!(saved.session_id.as_deref(), Some("ses_pi"));

        // Nothing about a restart turns it back into a send: requeueing is OpenCode's, and a Pi
        // round has no transcript to reconcile against and no message to flush.
        assert_eq!(requeue_all(&reopened, &checkout_id).unwrap(), 0);
        assert_eq!(
            flush_rounds(
                &reopened,
                &AgentService::without_service(),
                &checkout_id,
                Path::new("/nonexistent"),
                None,
            )
            .expect("a Pi round is not something a flush can act on"),
            0
        );
        assert_eq!(status_of(&reopened, &checkout_id, saved), "dispatched");
    }

    /// A launch that never got as far as a terminal leaves nothing behind to settle.
    ///
    /// The round and its notes are committed before the spawn, so a spawn that fails has to undo
    /// them: notes marked `sent` for a Pi that was never started would be a claim about a
    /// delivery that cannot have happened, and nothing would ever move them again.
    #[test]
    fn a_launch_that_never_reached_a_terminal_leaves_no_round_and_no_sent_notes() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, other_checkout_id) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "one");
        let round = begin_pi_round(
            &database,
            &checkout_id,
            &["note:one".to_string()],
            "# Code Review",
        )
        .unwrap();
        let note = &database.review_notes(&checkout_id).unwrap()[0];
        assert_eq!(
            note.status, "sent",
            "the notes are committed before the spawn"
        );

        assert!(database.discard_pi_round(&round.id, &checkout_id).unwrap());
        assert!(
            database.review_rounds(&checkout_id).unwrap().is_empty(),
            "a round for a terminal that never existed is still on record"
        );
        let note = &database.review_notes(&checkout_id).unwrap()[0];
        assert_eq!(note.status, "draft", "the notes are drafts again");
        assert_eq!(note.round_id, None);
        // And it is not a thing to do twice, or to do for somebody else's checkout.
        assert!(!database.discard_pi_round(&round.id, &checkout_id).unwrap());
        assert!(!database
            .discard_pi_round(&round.id, &other_checkout_id)
            .unwrap());
    }

    /// Once a round has a terminal, discarding is no longer this function's business: a Pi that
    /// was started may well have read the review, and undoing the record of that would be
    /// guessing in the other direction.
    #[test]
    fn a_round_that_reached_a_terminal_is_never_discarded() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "one");
        let round = launched_pi_round(&database, &checkout_id, "terminal:one", "ses_pi");

        assert!(!database.discard_pi_round(&round.id, &checkout_id).unwrap());
        assert_eq!(status_of(&database, &checkout_id, &round), "dispatching");
        assert_eq!(
            database.review_notes(&checkout_id).unwrap()[0].status,
            "sent"
        );
    }

    /// Two Pi terminals in one checkout, each with a round. A settlement says which session
    /// finished, so it may only close that session's round.
    #[test]
    fn a_settlement_closes_only_the_round_of_the_session_that_settled() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "one");
        add_note(&database, &checkout_id, "two");
        let agents = AgentService::without_service();
        let first =
            begin_pi_round(&database, &checkout_id, &["note:one".to_string()], "# One").unwrap();
        let first = bind_pi_round(&database, &checkout_id, &first.id, "terminal:one").unwrap();
        let second =
            begin_pi_round(&database, &checkout_id, &["note:two".to_string()], "# Two").unwrap();
        let second = bind_pi_round(&database, &checkout_id, &second.id, "terminal:two").unwrap();

        let first_token = credential(&agents, "terminal:one", &checkout_id);
        let second_token = credential(&agents, "terminal:two", &checkout_id);
        let mut one = Reporter::connect(agents.pi());
        one.hello(
            &first_token,
            "terminal:one",
            &checkout_id,
            "ses_one",
            serde_json::json!([
                receipt(&first.marker, "entry:one"),
                // And, impossibly, the other round's marker: a receipt is matched against the
                // round bound to this session, not against whatever this session claims to have.
                receipt(&second.marker, "entry:two"),
            ]),
        );
        let mut two = Reporter::connect(agents.pi());
        two.hello(
            &second_token,
            "terminal:two",
            &checkout_id,
            "ses_two",
            serde_json::json!([]),
        );
        eventually("both sessions reported themselves", || {
            agents.pi().sessions(&checkout_id).len() == 2
        });

        settle_pi_rounds(&database, &agents, &checkout_id, "ses_one").unwrap();
        assert_eq!(status_of(&database, &checkout_id, &first), "dispatched");
        assert_eq!(
            status_of(&database, &checkout_id, &second),
            "dispatching",
            "this session confirmed a round bound to another terminal"
        );

        one.state(true);
        eventually("the first turn started", || {
            agents
                .pi()
                .sessions(&checkout_id)
                .iter()
                .any(|session| session.id == "ses_one" && session.running)
        });
        one.state(false);
        eventually("the first turn settled", || {
            agents
                .pi()
                .delivery(&checkout_id, "ses_one")
                .is_some_and(|delivery| delivery.settled)
        });
        settle_pi_rounds(&database, &agents, &checkout_id, "ses_one").unwrap();
        assert_eq!(status_of(&database, &checkout_id, &first), "acked");
        assert_eq!(
            status_of(&database, &checkout_id, &second),
            "dispatching",
            "a session's settlement closed a round it was never sent"
        );
    }

    /// Nothing reachable from a settlement can start a Pi.
    ///
    /// There is no queue, no flush and no retry for this destination: a send to Pi is opening a
    /// terminal, and that is the user's send. A round this leaves unconfirmed stays unconfirmed,
    /// which is why it is reported as waiting rather than as delivered or as lost-and-retried.
    #[test]
    fn an_unconfirmed_pi_round_is_waiting_and_nothing_sends_it_again() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);
        add_note(&database, &checkout_id, "one");
        let agents = AgentService::without_service();
        let round = launched_pi_round(&database, &checkout_id, "terminal:one", "ses_pi");
        let token = credential(&agents, "terminal:one", &checkout_id);

        // A session that appears, says nothing, and settles. It ran a turn; it was simply never
        // sent this review, so there is nothing about it that can confirm one.
        let mut reporter = Reporter::connect(agents.pi());
        reporter.hello(
            &token,
            "terminal:one",
            &checkout_id,
            "ses_pi",
            serde_json::json!([]),
        );
        eventually("the session reported itself", || {
            agents.pi().delivery(&checkout_id, "ses_pi").is_some()
        });
        reporter.state(true);
        eventually("the turn started", || {
            agents.pi().sessions(&checkout_id)[0].running
        });
        reporter.state(false);
        eventually("the turn settled", || {
            agents
                .pi()
                .delivery(&checkout_id, "ses_pi")
                .is_some_and(|delivery| delivery.settled)
        });
        assert!(settle_pi_rounds(&database, &agents, &checkout_id, "ses_pi")
            .unwrap()
            .is_empty());
        assert_eq!(
            status_of(&database, &checkout_id, &round),
            "dispatching",
            "a settled turn with no marker was reported as a delivery"
        );

        // And the round is still not retryable by anything.
        assert_eq!(requeue_all(&database, &checkout_id).unwrap(), 0);
        assert_eq!(
            flush_rounds(
                &database,
                &agents,
                &checkout_id,
                Path::new("/nonexistent"),
                None
            )
            .unwrap(),
            0
        );
        assert!(reconcile_round(
            &database,
            &agents,
            &checkout_id,
            Path::new("/nonexistent"),
            &round.id,
            None,
        )
        .is_ok());
        assert_eq!(status_of(&database, &checkout_id, &round), "dispatching");
    }
}
