use std::sync::{Arc, Mutex};

use tauri::{
    ipc::{Channel, Response},
    AppHandle, EventId, Listener, State,
};

use crate::{
    config::{self, ConfigFile},
    domain::{
        ipc::{IpcError, IpcErrorCode},
        terminal_layout::CheckoutTerminalLayout,
        workspace::{Session, TerminalSessionStatus, WorkspaceState},
    },
    persistence::Database,
    services::terminal,
    terminal::{
        OutputGate, OutputSink, TerminalBackend, MAX_TERMINAL_INPUT_BYTES, OUTPUT_RESUME_TIMEOUT,
    },
};

#[tauri::command]
pub async fn terminal_layout_load(
    checkout_id: String,
    database: State<'_, Database>,
) -> Result<Option<CheckoutTerminalLayout>, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        database
            .load_terminal_layout(&checkout_id)
            .map_err(operation_error)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn terminal_layout_save(
    checkout_id: String,
    layout: CheckoutTerminalLayout,
    database: State<'_, Database>,
) -> Result<(), IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        database
            .save_terminal_layout(&checkout_id, &layout)
            .map_err(operation_error)
    })
    .await
    .map_err(operation_error)?
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedTerminal {
    session: Session,
    workspace: WorkspaceState,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalCreateRequest {
    checkout_id: String,
    cols: u16,
    rows: u16,
    prompt: Option<String>,
}

/// Where the window says how far behind it is.
///
/// A Tauri event rather than a command, because this is the one message in the terminal that runs
/// backwards: everything else about a session is an answer, and the reader cannot ask for more
/// output without somewhere to put what it already has. The event is registered per session
/// rather than once for the app, and each registration recognises its own answers and ignores the
/// rest, so the session id is what tells them apart.
const OUTPUT_FLOW_EVENT: &str = "terminal-output-flow";

/// What one window reports about the output it has parsed so far.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OutputFlow {
    session_id: String,
    /// Bytes the window has parsed in total, or absent once it is gone for good.
    ///
    /// A total rather than a pending count because the reader's own count is taken before the
    /// bytes leave this side: the difference between the two totals is what is still owed, and a
    /// pending count would leave everything still in the channel out of it.
    parsed: Option<usize>,
}

/// The registration through which one terminal's window answers its own reader and nobody else's.
struct OutputFlowListener {
    app: AppHandle,
    session_id: Arc<Mutex<Option<String>>>,
    listener: Arc<Mutex<Option<EventId>>>,
}

impl OutputFlowListener {
    fn attach(app: &AppHandle, gate: Arc<OutputGate>) -> Self {
        let session_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let listener: Arc<Mutex<Option<EventId>>> = Arc::new(Mutex::new(None));
        let watched = Arc::clone(&session_id);
        let registered = Arc::clone(&listener);
        let answering = Arc::clone(&gate);
        let listener_app = app.clone();
        let id = app.listen(OUTPUT_FLOW_EVENT, move |event| {
            let Ok(flow) = serde_json::from_str::<OutputFlow>(event.payload()) else {
                return;
            };
            let Ok(session) = watched.lock() else {
                return;
            };
            // An answer from any other terminal, and any answer at all before this session has a
            // name, is not this reader's to answer.
            if session.as_deref() != Some(flow.session_id.as_str()) {
                return;
            }
            drop(session);
            match flow.parsed {
                Some(parsed) => answering.acknowledge(parsed),
                None => {
                    // The window is gone, so nothing will ever answer this gate again and the
                    // reader goes back to draining on its own. The registration goes with it:
                    // there is no session left for it to answer.
                    answering.release();
                    if let Some(id) = registered
                        .lock()
                        .ok()
                        .and_then(|mut registered| registered.take())
                    {
                        listener_app.unlisten(id);
                    }
                }
            }
        });
        *listener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(id);
        Self {
            app: app.clone(),
            session_id,
            listener,
        }
    }

    /// The session this reader belongs to, which only exists once the terminal has been created.
    fn name(&self, session_id: String) {
        *self
            .session_id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(session_id);
    }

    /// Takes the registration away, for the case where there is no session to answer for.
    fn stop(&self) {
        if let Some(id) = self
            .listener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            self.app.unlisten(id);
        }
    }
}

#[tauri::command]
pub async fn terminal_create(
    request: TerminalCreateRequest,
    on_output: Channel<Response>,
    app: AppHandle,
    database: State<'_, Database>,
    backend: State<'_, std::sync::Arc<TerminalBackend>>,
    config: State<'_, ConfigFile>,
) -> Result<CreatedTerminal, IpcError> {
    validate_dimensions(request.cols, request.rows)?;
    let database = database.inner().clone();
    let backend = backend.inner().clone();
    // Beside `config.yml`, in the app's own folder in the user's home, and where the shell
    // integration script a terminal sources is written.
    let script_dir = config::dir_of(config.inner()).map(|dir| dir.to_path_buf());
    let config = config.inner().0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // The reader's half of the flow control with the window that draws this session's output:
        // one gate per session, and the only thing that can open it is that window saying how far
        // behind it is.
        let gate = Arc::new(OutputGate::new(OUTPUT_RESUME_TIMEOUT));
        let flow = OutputFlowListener::attach(&app, Arc::clone(&gate));
        let output: OutputSink = Box::new(move |bytes| {
            // Held before the hand-off rather than after it: a window that is already two
            // megabytes behind must not be handed another chunk, and while this waits the PTY is
            // not drained, so the process behind it blocks in `write` the way it would behind a
            // full terminal. Nothing is dropped here, which is the whole of the bargain.
            gate.hold();
            // Counted before the hand-off rather than after it, which is the only order in which no
            // byte can fall between the two counts: the window can be parsing this chunk and
            // answering about it before the send carrying it has returned, and a total that
            // arrives ahead of its own bytes would clamp itself to a count they were counted
            // outside of. Nothing here discards or retries output to make room — the bytes are
            // handed over whole, in order, or the session has no window left to hand them to.
            gate.delivered(bytes.len());
            if let Err(error) = on_output.send(Response::new(bytes.to_vec())) {
                // Nobody is left to answer a gate whose window is gone, and the reader has to keep
                // draining either way, so from here this is not a gate. That is also what retires
                // the bytes counted above for a hand-off that did not happen.
                gate.release();
                return Err(error.to_string());
            }
            Ok(())
        });
        // Read here rather than asked for over IPC, which is what keeps the setting off the wire: the
        // shape of `terminal_create` is unchanged, so a frontend that has never heard of the setting
        // produces terminals that honour it.
        let shell_integration = config::load(&config)
            .ok()
            .map(|settings| settings.terminal.shell_integration);
        match terminal::create_with_settings(
            &database,
            &backend,
            &request.checkout_id,
            terminal::TerminalOptions {
                cols: request.cols,
                rows: request.rows,
                prompt: request.prompt,
            },
            &shell_integration,
            &script_dir,
            output,
        ) {
            Ok(created) => {
                // The window answers for this session only, and cannot be named until the session
                // exists. An answer that gets here first is not this session's and is ignored; the
                // gate opens on its own timeout rather than holding anything in the meantime.
                flow.name(created.session.id.clone());
                Ok(CreatedTerminal {
                    session: created.session,
                    workspace: created.workspace,
                })
            }
            Err(error) => {
                // No session, so nothing will ever answer this gate.
                flow.stop();
                Err(operation_error(error))
            }
        }
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn terminal_write(
    checkout_id: String,
    session_id: String,
    bytes: Vec<u8>,
    database: State<'_, Database>,
    backend: State<'_, std::sync::Arc<TerminalBackend>>,
) -> Result<(), IpcError> {
    if bytes.len() > MAX_TERMINAL_INPUT_BYTES {
        return Err(operation_error(format!(
            "terminal input exceeds the {MAX_TERMINAL_INPUT_BYTES}-byte limit"
        )));
    }
    let database = database.inner().clone();
    let backend = backend.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        validate_session_owner(&database, &session_id, &checkout_id)?;
        backend.write(&session_id, &bytes).map_err(operation_error)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn terminal_resize(
    checkout_id: String,
    session_id: String,
    cols: u16,
    rows: u16,
    database: State<'_, Database>,
    backend: State<'_, std::sync::Arc<TerminalBackend>>,
) -> Result<(), IpcError> {
    let database = database.inner().clone();
    let backend = backend.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        validate_session_owner(&database, &session_id, &checkout_id)?;
        validate_dimensions(cols, rows)?;
        backend
            .resize(&session_id, cols, rows)
            .map(|_| ())
            .map_err(operation_error)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn terminal_status(
    checkout_id: String,
    session_id: String,
    database: State<'_, Database>,
    backend: State<'_, std::sync::Arc<TerminalBackend>>,
) -> Result<TerminalSessionStatus, IpcError> {
    let database = database.inner().clone();
    let backend = backend.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        validate_session_owner(&database, &session_id, &checkout_id)?;
        backend.status(&session_id).map_err(operation_error)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn terminal_close(
    checkout_id: String,
    session_id: String,
    database: State<'_, Database>,
    backend: State<'_, std::sync::Arc<TerminalBackend>>,
) -> Result<WorkspaceState, IpcError> {
    let database = database.inner().clone();
    let backend = backend.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        validate_session_owner(&database, &session_id, &checkout_id)?;
        terminal::close(&database, &backend, &session_id).map_err(operation_error)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn terminal_rename(
    checkout_id: String,
    session_id: String,
    name: String,
    database: State<'_, Database>,
) -> Result<WorkspaceState, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        validate_session_owner(&database, &session_id, &checkout_id)?;
        terminal::rename(&database, &session_id, &name).map_err(operation_error)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn terminal_move(
    checkout_id: String,
    session_id: String,
    target_checkout_id: String,
    select_target: bool,
    database: State<'_, Database>,
) -> Result<WorkspaceState, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        validate_session_owner(&database, &session_id, &checkout_id)?;
        database
            .move_terminal_session(&session_id, &target_checkout_id, select_target)
            .map_err(operation_error)
    })
    .await
    .map_err(operation_error)?
}

fn validate_session_owner(
    database: &Database,
    session_id: &str,
    checkout_id: &str,
) -> Result<(), IpcError> {
    let owner = database
        .terminal_session_checkout(session_id)
        .map_err(operation_error)?
        .ok_or_else(|| {
            IpcError::new(
                IpcErrorCode::TerminalSessionMissing,
                "terminal session ID is not registered",
            )
        })?;
    if owner != checkout_id {
        return Err(IpcError::new(
            IpcErrorCode::TerminalOwnershipMismatch,
            "terminal session does not belong to the requested checkout",
        ));
    }
    Ok(())
}

fn validate_dimensions(cols: u16, rows: u16) -> Result<(), IpcError> {
    if cols == 0 || rows == 0 {
        return Err(IpcError::new(
            IpcErrorCode::InvalidTerminalDimensions,
            "PTY dimensions must be non-zero",
        ));
    }
    Ok(())
}

fn operation_error(error: impl std::fmt::Display) -> IpcError {
    IpcError::new(IpcErrorCode::OperationFailed, error.to_string())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::{domain::workspace::Repo, persistence::Database};

    use super::{validate_dimensions, validate_session_owner};

    #[test]
    fn invalid_dimensions_have_a_typed_ipc_error() {
        let error = validate_dimensions(0, 24).unwrap_err();
        assert_eq!(
            error.code,
            crate::domain::ipc::IpcErrorCode::InvalidTerminalDimensions
        );
        assert_eq!(error.message, "PTY dimensions must be non-zero");
    }

    #[test]
    fn sec_06_session_ids_are_validated_against_the_requested_checkout() {
        let temp = tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        let first = Repo::plain(&first, "now").unwrap();
        let second = Repo::plain(&second, "now").unwrap();
        database.register_plain_repo(first.clone()).unwrap();
        database.register_plain_repo(second.clone()).unwrap();
        database
            .add_terminal_session(&crate::domain::workspace::Session {
                id: "session:first".into(),
                session_type: crate::domain::workspace::SessionType::Shell,
                checkout_id: first.checkouts[0].id.clone(),
                name: "shell".into(),
                created_at: "now".into(),
                status: crate::domain::workspace::SessionStatus::Active,
            })
            .unwrap();

        assert!(validate_session_owner(&database, "session:first", &first.checkouts[0].id).is_ok());
        let missing = validate_session_owner(&database, "session:unknown", &first.checkouts[0].id)
            .unwrap_err();
        assert_eq!(
            missing.code,
            crate::domain::ipc::IpcErrorCode::TerminalSessionMissing
        );
        let foreign = validate_session_owner(&database, "session:first", &second.checkouts[0].id)
            .unwrap_err();
        assert_eq!(
            foreign.code,
            crate::domain::ipc::IpcErrorCode::TerminalOwnershipMismatch
        );
        assert_eq!(
            foreign.message,
            "terminal session does not belong to the requested checkout"
        );
        assert_eq!(
            serde_json::to_value(foreign).unwrap(),
            serde_json::json!({
                "code": "terminal_ownership_mismatch",
                "message": "terminal session does not belong to the requested checkout"
            })
        );
    }
}
