use std::path::PathBuf;

use tauri::State;

use crate::{
    domain::{
        agent::{AgentAgent, AgentRelocation, AgentSession},
        ipc::{IpcError, IpcErrorCode},
    },
    persistence::Database,
    services::agent::{self, AgentService},
};
use std::sync::Arc;

fn operation_error(error: impl std::fmt::Display) -> IpcError {
    IpcError::new(IpcErrorCode::OperationFailed, error.to_string())
}

/// The directory a checkout lives in, resolved once and reused by every agent call.
pub fn checkout_directory(database: &Database, checkout_id: &str) -> Result<PathBuf, IpcError> {
    database
        .terminal_checkout_path(checkout_id)
        .map_err(|error| IpcError::new(IpcErrorCode::InvalidCheckout, error))
}

fn with_current_checkout<T>(
    database: &Database,
    agents: &AgentService,
    checkout_id: &str,
    directory: &std::path::Path,
    generation: u64,
    operation: impl FnOnce() -> Result<T, agent::BridgeError>,
) -> Result<T, IpcError> {
    agents
        .with_checkout_generation(
            checkout_id,
            generation,
            || {
                database
                    .terminal_checkout_path(checkout_id)
                    .is_ok_and(|current| current == directory)
            },
            operation,
        )
        .map_err(agent::map_error)
}

#[tauri::command]
pub async fn agent_sessions(
    checkout_id: String,
    database: State<'_, Database>,
    agents: State<'_, Arc<AgentService>>,
) -> Result<Vec<AgentSession>, IpcError> {
    let generation = agents
        .checkout_generation(&checkout_id)
        .map_err(agent::map_error)?;
    let database = database.inner().clone();
    let agents: Arc<AgentService> = Arc::clone(agents.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let directory = checkout_directory(&database, &checkout_id)?;
        with_current_checkout(
            &database,
            &agents,
            &checkout_id,
            &directory,
            generation,
            || agents.sessions(&checkout_id, &directory),
        )
    })
    .await
    .map_err(operation_error)?
}

/// The candidate sessions a sidebar row matches its terminal's own title against.
///
/// Deliberately separate from `agent_sessions`: that one is scoped to the worktree because a review
/// round must not be sent somewhere the reader did not ask about, and a row needs a wider list than
/// that. See [`AgentService::candidate_sessions`].
#[tauri::command]
pub async fn agent_candidate_sessions(
    checkout_id: String,
    database: State<'_, Database>,
    agents: State<'_, Arc<AgentService>>,
) -> Result<Vec<AgentSession>, IpcError> {
    let generation = agents
        .checkout_generation(&checkout_id)
        .map_err(agent::map_error)?;
    let database = database.inner().clone();
    let agents: Arc<AgentService> = Arc::clone(agents.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let directory = checkout_directory(&database, &checkout_id)?;
        with_current_checkout(
            &database,
            &agents,
            &checkout_id,
            &directory,
            generation,
            || agents.candidate_sessions(&checkout_id, &directory),
        )
    })
    .await
    .map_err(operation_error)?
}

/// The sessions that have moved to another worktree since the last read.
///
/// There is no checkout to ask this of: the whole point is that a session has left the one it
/// was in, so it is named by the workspace rather than by a directory to scope it to.
#[tauri::command]
pub async fn agent_relocations(
    database: State<'_, Database>,
    agents: State<'_, Arc<AgentService>>,
    baseline: Option<bool>,
) -> Result<Vec<AgentRelocation>, IpcError> {
    let database = database.inner().clone();
    let agents: Arc<AgentService> = Arc::clone(agents.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let checkouts = database
            .checkout_directories()
            .map_err(|error| IpcError::new(IpcErrorCode::InvalidCheckout, error))?;
        agents
            .relocations_with_baseline(&checkouts, baseline.unwrap_or(false))
            .map_err(agent::map_error)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn agent_agents(
    checkout_id: String,
    database: State<'_, Database>,
    agents: State<'_, Arc<AgentService>>,
) -> Result<Vec<AgentAgent>, IpcError> {
    let generation = agents
        .checkout_generation(&checkout_id)
        .map_err(agent::map_error)?;
    let database = database.inner().clone();
    let agents: Arc<AgentService> = Arc::clone(agents.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let directory = checkout_directory(&database, &checkout_id)?;
        with_current_checkout(
            &database,
            &agents,
            &checkout_id,
            &directory,
            generation,
            || agents.agents(&checkout_id, &directory),
        )
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn agent_session_create(
    checkout_id: String,
    title: String,
    database: State<'_, Database>,
    agents: State<'_, Arc<AgentService>>,
) -> Result<AgentSession, IpcError> {
    let generation = agents
        .checkout_generation(&checkout_id)
        .map_err(agent::map_error)?;
    let database = database.inner().clone();
    let agents: Arc<AgentService> = Arc::clone(agents.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let directory = checkout_directory(&database, &checkout_id)?;
        agents
            .create_session_at_generation(&checkout_id, &directory, &title, generation, || {
                database
                    .terminal_checkout_path(&checkout_id)
                    .is_ok_and(|current| current == directory)
            })
            .map_err(agent::map_error)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn agent_prompt(
    checkout_id: String,
    session_id: String,
    text: String,
    database: State<'_, Database>,
    agents: State<'_, Arc<AgentService>>,
) -> Result<AgentSession, IpcError> {
    let generation = agents
        .checkout_generation(&checkout_id)
        .map_err(agent::map_error)?;
    let database = database.inner().clone();
    let agents: Arc<AgentService> = Arc::clone(agents.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let directory = checkout_directory(&database, &checkout_id)?;
        agents
            .prompt_at_generation(
                &checkout_id,
                &directory,
                &session_id,
                &text,
                generation,
                || {
                    database
                        .terminal_checkout_path(&checkout_id)
                        .is_ok_and(|current| current == directory)
                },
            )
            .map_err(agent::map_error)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn agent_stop(
    checkout_id: String,
    database: State<'_, Database>,
    agents: State<'_, Arc<AgentService>>,
) -> Result<(), IpcError> {
    let generation = agents
        .checkout_generation(&checkout_id)
        .map_err(agent::map_error)?;
    let database = database.inner().clone();
    let agents: Arc<AgentService> = Arc::clone(agents.inner());
    tauri::async_runtime::spawn_blocking(move || {
        agents
            .stop_at_generation(&checkout_id, generation, || {
                database.terminal_checkout_path(&checkout_id).is_ok()
            })
            .map_err(agent::map_error)
    })
    .await
    .map_err(operation_error)?
}
