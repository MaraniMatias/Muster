use std::sync::Arc;

use tauri::State;

use crate::{
    domain::{ipc::IpcError, review::ReviewRound},
    persistence::Database,
    services::{
        agent::{self, AgentService},
        review::{self, NewReviewNote, NoteAnchorCheck},
        review_round,
    },
};

fn operation_error(error: impl std::fmt::Display) -> IpcError {
    IpcError::new(
        crate::domain::ipc::IpcErrorCode::OperationFailed,
        error.to_string(),
    )
}

#[tauri::command]
pub async fn review_notes(
    checkout_id: String,
    database: State<'_, Database>,
) -> Result<Vec<crate::domain::review::ReviewNote>, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || review::notes(&database, &checkout_id))
        .await
        .map_err(operation_error)?
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewNoteCreateRequest {
    checkout_id: String,
    path: String,
    side: String,
    line_start: i64,
    line_end: Option<i64>,
    content: String,
    code: String,
}

#[tauri::command]
pub async fn review_note_create(
    request: ReviewNoteCreateRequest,
    database: State<'_, Database>,
) -> Result<crate::domain::review::ReviewNote, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        review::add_note(
            &database,
            &request.checkout_id,
            NewReviewNote {
                path: request.path,
                side: request.side,
                line_start: request.line_start,
                line_end: request.line_end,
                content: request.content,
                code: request.code,
            },
        )
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn review_note_update(
    checkout_id: String,
    id: String,
    content: String,
    database: State<'_, Database>,
) -> Result<crate::domain::review::ReviewNote, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        review::update_note(&database, &checkout_id, &id, &content)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn review_note_delete(
    checkout_id: String,
    id: String,
    database: State<'_, Database>,
) -> Result<(), IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || review::delete_note(&database, &checkout_id, &id))
        .await
        .map_err(operation_error)?
}

#[tauri::command]
pub async fn review_notes_mark_sent(
    checkout_id: String,
    ids: Vec<String>,
    database: State<'_, Database>,
) -> Result<(), IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || review::mark_sent(&database, &checkout_id, &ids))
        .await
        .map_err(operation_error)?
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewAnchorCheckRequest {
    pub id: String,
    pub current_code: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewAnchorsVerifyRequest {
    pub checkout_id: String,
    pub path: String,
    pub checks: Vec<ReviewAnchorCheckRequest>,
}

#[tauri::command]
pub async fn review_note_anchors_verify(
    request: ReviewAnchorsVerifyRequest,
    database: State<'_, Database>,
) -> Result<Vec<crate::domain::review::ReviewNote>, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let checks: Vec<NoteAnchorCheck> = request
            .checks
            .into_iter()
            .map(|check| NoteAnchorCheck {
                id: check.id,
                current_code: check.current_code,
            })
            .collect();
        review::verify_note_anchors(&database, &request.checkout_id, &request.path, &checks)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn review_note_outdated_clear(
    checkout_id: String,
    id: String,
    database: State<'_, Database>,
) -> Result<crate::domain::review::ReviewNote, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        review::clear_outdated(&database, &checkout_id, &id)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn review_note_resolve(
    checkout_id: String,
    id: String,
    database: State<'_, Database>,
) -> Result<crate::domain::review::ReviewNote, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        review_round::resolve_note(&database, &checkout_id, &id)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn review_rounds(
    checkout_id: String,
    database: State<'_, Database>,
) -> Result<Vec<ReviewRound>, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || review_round::rounds(&database, &checkout_id))
        .await
        .map_err(operation_error)?
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRoundDispatchRequest {
    pub checkout_id: String,
    /// The session chosen for this round; must belong to this checkout's server.
    pub session_id: String,
    pub ids: Vec<String>,
    /// The exported review. Built by the caller, which owns the Markdown format.
    pub markdown: String,
    /// Wait for the agent instead of interrupting what it is doing now.
    ///
    /// Required, not defaulted: this request and the caller that writes it ship in the same build,
    /// so a request without the field is a caller that cannot be talking to this backend.
    pub queue: bool,
}

/// Delivers one review round to one agent session as a single message.
///
/// The order is deliberate: the round and its marker are recorded *before* the agent is
/// called, so an interrupted send is recognizable afterwards instead of being repeated.
#[tauri::command]
pub async fn review_round_dispatch(
    database: State<'_, Database>,
    agents: State<'_, Arc<AgentService>>,
    request: ReviewRoundDispatchRequest,
) -> Result<ReviewRound, IpcError> {
    let generation = agents
        .checkout_generation(&request.checkout_id)
        .map_err(agent::map_error)?;
    let database = database.inner().clone();
    let agents: Arc<AgentService> = Arc::clone(agents.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let directory = super::agent::checkout_directory(&database, &request.checkout_id)?;
        // Checked against the marker that will be prepended, so the limit is the real one.
        review_round::check_prompt_size(
            &(request.markdown.clone() + &"x".repeat(review_round::marker_budget())),
        )?;
        // Whichever the caller asked for, this links the notes — which is also what rejects a
        // foreign note id — and stores the exact message the round will be sent with.
        //
        // The queued case has to be recorded as `queued`, not as `dispatching`: `queued` is the
        // state `flush_rounds` selects, so a round recorded as `dispatching` reads as a send in
        // flight, is never flushed, and sits there until some reconnect happens to requeue it.
        let record = if request.queue {
            review_round::queue_round
        } else {
            review_round::begin_round
        };
        let round = record(
            &database,
            &request.checkout_id,
            &request.session_id,
            &request.ids,
            &request.markdown,
        )?;
        // Queued: the round is recorded and the notes are committed to it, but the agent is
        // left alone. Nothing about the send has to be remembered by the caller.
        if request.queue {
            return Ok(round);
        }
        let prompt = review_round::build_round_prompt(&request.markdown, &round.marker);
        match agents.prompt_at_generation(
            &request.checkout_id,
            &directory,
            &request.session_id,
            &prompt,
            generation,
            || {
                database
                    .terminal_checkout_path(&request.checkout_id)
                    .is_ok_and(|current| current == directory)
            },
        ) {
            Ok(_) => review_round::confirm_round(&database, &request.checkout_id, &round.id),
            // Left as `dispatching` on purpose: the marker is what lets reconciliation learn
            // whether this send landed, and requeueing blindly could duplicate it.
            Err(error) => Err(review_round::agent_error(error)),
        }
    })
    .await
    .map_err(operation_error)?
}

/// Sends the rounds the user chose to hold back until the agent was free.
///
/// Called when a turn ends, which is the moment those rounds were waiting for. Each round is
/// checked against the transcript first, so a send that actually landed before a reconnect
/// requeued it is confirmed rather than repeated.
#[tauri::command]
pub async fn review_round_flush(
    database: State<'_, Database>,
    agents: State<'_, Arc<AgentService>>,
    checkout_id: String,
) -> Result<usize, IpcError> {
    let generation = agents
        .checkout_generation(&checkout_id)
        .map_err(agent::map_error)?;
    let database = database.inner().clone();
    let agents: Arc<AgentService> = Arc::clone(agents.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let directory = super::agent::checkout_directory(&database, &checkout_id)?;
        review_round::flush_rounds(
            &database,
            &agents,
            &checkout_id,
            &directory,
            Some(generation),
        )
    })
    .await
    .map_err(operation_error)?
}

/// Marks every unconfirmed round of the checkout as retryable. Called on reconnect.
#[tauri::command]
pub async fn review_rounds_requeue(
    checkout_id: String,
    database: State<'_, Database>,
) -> Result<usize, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || review_round::requeue_all(&database, &checkout_id))
        .await
        .map_err(operation_error)?
}

/// Asks the session whether a round's message actually arrived, and acts on the answer.
#[tauri::command]
pub async fn review_round_reconcile(
    database: State<'_, Database>,
    agents: State<'_, Arc<AgentService>>,
    checkout_id: String,
    round_id: String,
) -> Result<ReviewRound, IpcError> {
    let generation = agents
        .checkout_generation(&checkout_id)
        .map_err(agent::map_error)?;
    let database = database.inner().clone();
    let agents: Arc<AgentService> = Arc::clone(agents.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let directory = super::agent::checkout_directory(&database, &checkout_id)?;
        review_round::reconcile_round(
            &database,
            &agents,
            &checkout_id,
            &directory,
            &round_id,
            Some(generation),
        )
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn review_round_ack(
    checkout_id: String,
    round_id: String,
    database: State<'_, Database>,
) -> Result<ReviewRound, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        review_round::ack_round(&database, &checkout_id, &round_id)
    })
    .await
    .map_err(operation_error)?
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        sync::Arc,
        thread,
    };

    use tauri::Manager;

    use tempfile::tempdir;

    use crate::{
        domain::workspace::Repo,
        persistence::Database,
        services::{
            agent::AgentService,
            review::{self, NewReviewNote},
        },
    };

    use super::{review_round_dispatch, review_round_flush, ReviewRoundDispatchRequest};

    /// Answers one request per response, body included, so a send can be walked end to end.
    fn mock_responses(responses: Vec<serde_json::Value>) -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock server should bind");
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                reader
                    .read_line(&mut String::new())
                    .expect("request line should be readable");
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
                        if name.trim().eq_ignore_ascii_case("content-length") {
                            content_length = value.trim().parse().expect("valid body length");
                        }
                    }
                }
                let mut sent = vec![0; content_length];
                reader
                    .read_exact(&mut sent)
                    .expect("request body should be readable");
                let body = serde_json::to_vec(&response).expect("response should encode");
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .expect("response headers should be writable");
                stream
                    .write_all(&body)
                    .expect("response body should be writable");
            }
        });
        port
    }

    /// Queued has to persist `queued`, because that is the state the send path selects.
    ///
    /// `queue_round` already had unit coverage and the dispatch was still wrong, because the bug
    /// was in the command, which recorded every round with `begin_round` and returned. So this one
    /// enters through `review_round_dispatch` and leaves through `review_round_flush`, against a
    /// server that answers the routes a send walks: a round stored as anything else is never
    /// selected, so the flush reports nothing sent and the round never leaves `dispatching`.
    #[test]
    fn a_queued_dispatch_is_the_round_the_flush_sends() {
        let temp = tempdir().unwrap();
        let directory = temp.path().join("first");
        std::fs::create_dir_all(&directory).unwrap();
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        let repo = Repo::plain(&directory, "now").unwrap();
        let checkout_id = repo.checkouts[0].id.clone();
        database.register_plain_repo(repo).unwrap();
        let note = review::add_note(
            &database,
            &checkout_id,
            NewReviewNote {
                path: "src/foo.js".into(),
                side: "new".into(),
                line_start: 10,
                line_end: None,
                content: "check this".into(),
                code: "const result = a + b;".into(),
            },
        )
        .unwrap();

        let session = serde_json::json!({
            "id": "ses_target",
            "location": {"directory": directory.to_string_lossy()},
        });
        let port = mock_responses(vec![
            // The transcript the flush reads before it sends anything: no marker, so the round
            // reads as never delivered and the stored message has to go out.
            serde_json::json!({"data": session.clone()}),
            serde_json::json!({"data": [{"text": "unrelated transcript"}]}),
            serde_json::json!({"data": session.clone()}),
            serde_json::json!({"data": {"accepted": true}}),
            serde_json::json!({"data": session}),
            serde_json::json!({"data": {}}),
            serde_json::json!({"data": []}),
            serde_json::json!({"data": []}),
        ]);
        let agents = Arc::new(AgentService::with_test_server(port));
        let app = tauri::test::mock_app();
        app.manage(database.clone());
        app.manage(Arc::clone(&agents));

        let round = tauri::async_runtime::block_on(review_round_dispatch(
            app.state::<Database>(),
            app.state::<Arc<AgentService>>(),
            ReviewRoundDispatchRequest {
                checkout_id: checkout_id.clone(),
                session_id: "ses_target".into(),
                ids: vec![note.id],
                markdown: "# Code Review".into(),
                queue: true,
            },
        ))
        .expect("a queued dispatch records the round");
        assert_eq!(round.status, "queued");
        assert_eq!(
            database.review_rounds(&checkout_id).unwrap()[0].status,
            "queued"
        );

        let sent = tauri::async_runtime::block_on(review_round_flush(
            app.state::<Database>(),
            app.state::<Arc<AgentService>>(),
            checkout_id.clone(),
        ))
        .expect("the queued round is flushable");
        assert_eq!(
            sent, 1,
            "the flush did not select the round that was dispatched"
        );
        assert_eq!(
            database.review_rounds(&checkout_id).unwrap()[0].status,
            "dispatched"
        );
    }

    /// The bridge contract is one build on both ends, so `queue` is required rather than defaulted:
    /// a request missing it is refused instead of being read as "send it now".
    #[test]
    fn a_dispatch_without_queue_is_refused() {
        let missing = serde_json::json!({
            "checkoutId": "checkout:one",
            "sessionId": "ses_one",
            "ids": ["note:1"],
            "markdown": "# Code Review",
        });
        assert!(serde_json::from_value::<ReviewRoundDispatchRequest>(missing).is_err());

        let present = serde_json::json!({
            "checkoutId": "checkout:one",
            "sessionId": "ses_one",
            "ids": ["note:1"],
            "markdown": "# Code Review",
            "queue": false,
        });
        assert!(
            !serde_json::from_value::<ReviewRoundDispatchRequest>(present)
                .unwrap()
                .queue
        );
    }
}
