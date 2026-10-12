use std::path::PathBuf;

use tauri::State;

use crate::{
    domain::ipc::{IpcError, IpcErrorCode},
    persistence::Database,
    services::{
        agent::AgentService,
        worktree::{
            self, CreatedWorktree, RemovedWorktree, WorktreeDefaults, WorktreeRemovalConfirmation,
            WorktreeRemovalInfo,
        },
    },
    terminal::TerminalBackend,
};

#[tauri::command]
pub async fn worktree_defaults(
    checkout_id: String,
    database: State<'_, Database>,
) -> Result<WorktreeDefaults, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || worktree::defaults(&database, &checkout_id))
        .await
        .map_err(operation_error)?
}

#[tauri::command]
pub async fn worktree_create(
    checkout_id: String,
    task_name: String,
    branch: String,
    location: PathBuf,
    database: State<'_, Database>,
) -> Result<CreatedWorktree, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        worktree::create(&database, &checkout_id, &task_name, &branch, &location)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn worktree_removal_info(
    checkout_id: String,
    database: State<'_, Database>,
    agents: State<'_, std::sync::Arc<AgentService>>,
) -> Result<WorktreeRemovalInfo, IpcError> {
    let database = database.inner().clone();
    let agents = agents.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        worktree::removal_info(&database, &agents, &checkout_id)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn worktree_remove(
    checkout_id: String,
    confirmation: WorktreeRemovalConfirmation,
    database: State<'_, Database>,
    backend: State<'_, std::sync::Arc<TerminalBackend>>,
    agents: State<'_, std::sync::Arc<AgentService>>,
) -> Result<RemovedWorktree, IpcError> {
    let database = database.inner().clone();
    let backend = backend.inner().clone();
    let agents = agents.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        worktree::remove(&database, &backend, &agents, &checkout_id, &confirmation)
    })
    .await
    .map_err(operation_error)?
}

fn operation_error(error: tauri::Error) -> IpcError {
    IpcError::new(
        IpcErrorCode::OperationFailed,
        format!("worktree operation could not complete: {error}"),
    )
}
