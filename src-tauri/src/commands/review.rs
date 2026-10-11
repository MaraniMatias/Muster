use std::sync::Arc;

use tauri::{
    ipc::{Channel, Response},
    AppHandle, State,
};

use crate::{
    config::ConfigFile,
    domain::{
        agent::AgentProvider,
        ipc::IpcError,
        review::{review_round_marker, ReviewRound},
    },
    persistence::Database,
    services::{
        agent::{self, AgentService},
        pi_launch,
        review::{self, NewReviewNote, NoteAnchorCheck},
        review_round,
        terminal::{self, TerminalProgram},
    },
    terminal::TerminalBackend,
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
        review_round::check_review_markdown(&request.markdown)?;
        // Whichever the caller asked for, this links the notes — which is also what rejects a
        // foreign note id — and stores the exact message the round will be sent with.
        //
        // The queued case has to be recorded as `queued`, not as `dispatching`: `queued` is the
        // state `flush_rounds` selects, so a round recorded as `dispatching` reads as a send in
        // flight, is never flushed, and sits there until some reconnect happens to requeue it.
        // This command is the OpenCode path: it names a session that already exists on that
        // checkout's server. A Pi send opens a terminal instead, so it arrives by another command.
        let round = if request.queue {
            review_round::queue_round(
                &database,
                &request.checkout_id,
                &request.session_id,
                &request.ids,
                &request.markdown,
            )?
        } else {
            review_round::begin_round(
                &database,
                &request.checkout_id,
                AgentProvider::OpenCode,
                &request.session_id,
                &request.ids,
                &request.markdown,
            )?
        };
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

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRoundLaunchPiRequest {
    pub checkout_id: String,
    /// The notes this round carries. Validated by the round itself, which is what rejects an id
    /// belonging to another checkout.
    pub ids: Vec<String>,
    /// The exported review. Built by the caller, which owns the Markdown format.
    pub markdown: String,
    pub cols: u16,
    pub rows: u16,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRoundLaunched {
    pub round: ReviewRound,
    pub session: crate::domain::workspace::Session,
    pub workspace: crate::domain::workspace::WorkspaceState,
}

/// Records a Pi round and writes its startup prompt, undoing the round if preparation fails.
fn prepare_pi_review_launch(
    database: &Database,
    checkout_id: &str,
    ids: &[String],
    markdown: &str,
    round_id: &str,
    app_dir: &std::path::Path,
) -> Result<(ReviewRound, std::path::PathBuf), IpcError> {
    let round =
        review_round::begin_pi_round_with_id(database, checkout_id, ids, markdown, round_id)?;
    let prompt_file = (|| {
        let prompt = database
            .review_round_prompt(&round.id, checkout_id)
            .map_err(operation_error)?
            .ok_or_else(|| operation_error("the recorded round has no message to send"))?;
        pi_launch::write_prompt(app_dir, &round.marker, &prompt).map_err(operation_error)
    })();
    match prompt_file {
        Ok(path) => Ok((round, path)),
        Err(error) => {
            let _ = database.discard_pi_round(&round.id, checkout_id);
            Err(error)
        }
    }
}

fn pi_delivery_confirmed(status: &str) -> bool {
    matches!(status, "dispatched" | "acked")
}

/// Opens an interactive Pi with one review round already in front of it.
///
/// **There is no session to choose and no busy agent to wait for**, which is why this is its own
/// command rather than a case of `review_round_dispatch`: a Pi round is delivered by starting a
/// Pi, so a destination that existed already is not a thing the caller could have named. Every
/// send opens a new terminal in the checkout asked for, and each one carries its own round.
///
/// The order is the one every round is recorded on — the round and its marker exist before the
/// message can reach the agent — with the parts that can fail entirely in front of it: the program,
/// the bridge file and the prompt file are all resolved and written before a row is recorded, so a
/// machine with no Pi installed produces an error and no claim. What is left after a successful
/// spawn is a round that names its provider and its terminal and is otherwise unconfirmed. It is
/// **not** `dispatched`, because nobody has yet read anything back out of the Pi that received it.
#[tauri::command]
pub async fn review_round_launch_pi(
    request: ReviewRoundLaunchPiRequest,
    on_output: Channel<Response>,
    app: AppHandle,
    database: State<'_, Database>,
    backend: State<'_, Arc<TerminalBackend>>,
    config: State<'_, ConfigFile>,
    agents: State<'_, Arc<AgentService>>,
) -> Result<ReviewRoundLaunched, IpcError> {
    super::terminal::validate_dimensions(request.cols, request.rows)?;
    let database = database.inner().clone();
    let backend = backend.inner().clone();
    let agents = Arc::clone(agents.inner());
    let checkout_id = request.checkout_id.clone();
    // The app's own folder in the user's home, where `config.yml` lives. Nothing Pi needs is ever
    // written into the user's Pi configuration; see `docs/pi-integration.md`.
    let app_dir = crate::config::dir_of(config.inner())
        .map(|dir| dir.to_path_buf())
        .ok_or_else(|| operation_error("this app's settings folder could not be located"))?;
    tauri::async_runtime::spawn_blocking(move || {
        agents.with_checkout_operation(&checkout_id, || {
            review_round::check_review_markdown(&request.markdown)?;
            let round_id = review_round::new_round_id();
            let marker = review_round_marker(&round_id);
            review_round::check_round_prompt_size(&request.markdown, &marker)?;
            let program = pi_launch::resolve_program()
                .ok_or_else(|| operation_error("Pi is not installed, or not on this app's PATH"))?;
            let bridge = pi_launch::bridge_path(&app_dir).map_err(operation_error)?;
            let (round, prompt_file) = prepare_pi_review_launch(
                &database,
                &request.checkout_id,
                &request.ids,
                &request.markdown,
                &round_id,
                &app_dir,
            )?;

            let (flow, output) = super::terminal::terminal_output(&app, on_output);
            let mut bound_round = None;
            let created = match terminal::create_program_with_pre_spawn(
                &database,
                &backend,
                &request.checkout_id,
                terminal::TerminalSize {
                    cols: request.cols,
                    rows: request.rows,
                },
                TerminalProgram {
                    program,
                    args: pi_launch::arguments(&bridge, &prompt_file),
                    env: Vec::new(),
                    // What the row in the sidebar says, so this terminal is recognisable among the
                    // shells it was opened next to.
                    name: "pi".into(),
                },
                Some(Arc::clone(agents.pi())),
                output,
                |session| {
                    bound_round = Some(
                        review_round::bind_pi_round(
                            &database,
                            &request.checkout_id,
                            &round.id,
                            &session.id,
                        )
                        .map_err(|error| error.message)?,
                    );
                    Ok(())
                },
            ) {
                Ok(created) => created,
                Err(error) => {
                    // No session, so nothing will ever answer this gate.
                    flow.stop();
                    // Nothing was ever handed to a terminal, so there is no claim left to settle and
                    // the round goes back the way it came: its notes are drafts again rather than
                    // notes marked sent for a Pi that was never started, and the file it would have
                    // been read from is deleted rather than left for a day.
                    let _ = database.discard_pi_round(&round.id, &request.checkout_id);
                    pi_launch::remove_prompt(&app_dir, &round.marker);
                    return Err(operation_error(error));
                }
            };
            // The window answers for this session only, and cannot be named until the session exists.
            flow.name(created.session.id.clone());
            let round =
                bound_round.expect("pre-spawn binding succeeded before the process started");
            Ok(ReviewRoundLaunched {
                round,
                session: created.session,
                workspace: created.workspace,
            })
        })
    })
    .await
    .map_err(operation_error)?
}

/// Settles what one Pi session's own reports prove about the rounds it was sent.
///
/// Asked from the two Pi reports that mean something — the session appearing, and its turn
/// settling — and never as part of a send, so nothing reachable from here can start a second Pi.
/// A round that is still unconfirmed when this answers is left exactly where it was: there is no
/// automatic retransmission, because the only route to a Pi is opening one and opening one is
/// the user's send.
///
/// A confirmed delivery also removes the file its review was written to, which is safe only
/// because a receipt cannot exist while Pi is still reading it: Pi reads the `@file` during
/// startup and records the prompt before it introduces itself.
#[tauri::command]
pub async fn review_round_settle_pi(
    checkout_id: String,
    session_id: String,
    database: State<'_, Database>,
    agents: State<'_, Arc<AgentService>>,
    config: State<'_, ConfigFile>,
) -> Result<Vec<ReviewRound>, IpcError> {
    let database = database.inner().clone();
    let agents = Arc::clone(agents.inner());
    // Looked for here rather than inside the blocking task, and not required: a settlement that
    // confirmed nothing has no file to clean up, and failing one over a settings folder it did not
    // need would be worse than the file it would have removed.
    let app_dir = crate::config::dir_of(config.inner()).map(|dir| dir.to_path_buf());
    tauri::async_runtime::spawn_blocking(move || {
        let settled =
            review_round::settle_pi_rounds(&database, &agents, &checkout_id, &session_id)?;
        if let Some(app_dir) = app_dir {
            for round in &settled {
                if pi_delivery_confirmed(&round.status) {
                    pi_launch::remove_prompt(&app_dir, &round.marker);
                }
            }
        }
        Ok(settled)
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

    use super::{
        pi_delivery_confirmed, prepare_pi_review_launch, review_round_dispatch, review_round_flush,
        ReviewRoundDispatchRequest,
    };

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

    #[test]
    fn a_confirmed_or_acked_pi_round_is_safe_to_clean_up() {
        assert!(!pi_delivery_confirmed("dispatching"));
        assert!(pi_delivery_confirmed("dispatched"));
        assert!(pi_delivery_confirmed("acked"));
    }

    #[test]
    fn failed_pi_prompt_preparation_discards_the_round_and_restores_its_notes() {
        let temp = tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        let repo = Repo::plain(&checkout, "now").unwrap();
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
        let app_dir = temp.path().join("app");
        std::fs::create_dir_all(app_dir.join("pi")).unwrap();
        std::fs::write(app_dir.join("pi/prompts"), "not a directory").unwrap();

        assert!(prepare_pi_review_launch(
            &database,
            &checkout_id,
            &[note.id],
            "# Code Review",
            "round:1:2:3",
            &app_dir,
        )
        .is_err());
        assert!(database.review_rounds(&checkout_id).unwrap().is_empty());
        let note = &database.review_notes(&checkout_id).unwrap()[0];
        assert_eq!(note.status, "draft");
        assert_eq!(note.round_id, None);
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
