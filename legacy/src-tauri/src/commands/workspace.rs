use std::path::PathBuf;

use tauri::State;

use crate::{
    domain::{
        ipc::{IpcError, IpcErrorCode},
        workspace::{RecentPath, WorkspaceState},
    },
    persistence::Database,
    services,
};

#[tauri::command]
pub async fn restore_workspace(
    database: State<'_, Database>,
    home: State<'_, services::workspace::HomeDirectory>,
) -> Result<WorkspaceState, IpcError> {
    let database = database.inner().clone();
    let home = home.inner().clone();
    tauri::async_runtime::spawn_blocking(move || services::workspace::restore(&database, &home))
        .await
        .map_err(operation_error)?
}

/// Reads one repository's worktrees again, so a worktree another hand added reaches the panel
/// while the app is open. `None` is an answer too: the caller already holds the workspace, and a
/// change that turned out to be nothing should not make it re-read one.
#[tauri::command]
pub async fn sync_workspace_repo(
    repo_id: String,
    database: State<'_, Database>,
    agents: State<'_, std::sync::Arc<crate::services::agent::AgentService>>,
) -> Result<Option<WorkspaceState>, IpcError> {
    let database = database.inner().clone();
    let agents = agents.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        services::workspace::sync_repo(&database, &agents, &repo_id)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn register_folder(
    path: PathBuf,
    database: State<'_, Database>,
) -> Result<WorkspaceState, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        services::workspace::register_folder(&database, &path)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn list_recent_paths(database: State<'_, Database>) -> Result<Vec<RecentPath>, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || services::workspace::list_recent_paths(&database))
        .await
        .map_err(operation_error)?
}

#[tauri::command]
pub async fn locate_missing_checkout(
    checkout_id: String,
    path: PathBuf,
    database: State<'_, Database>,
    agents: State<'_, std::sync::Arc<crate::services::agent::AgentService>>,
) -> Result<WorkspaceState, IpcError> {
    let database = database.inner().clone();
    let agents = agents.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        services::workspace::locate_missing_checkout(&database, &agents, &checkout_id, &path)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn close_checkout(
    checkout_id: String,
    database: State<'_, Database>,
    backend: State<'_, std::sync::Arc<crate::terminal::TerminalBackend>>,
    agents: State<'_, std::sync::Arc<crate::services::agent::AgentService>>,
) -> Result<WorkspaceState, IpcError> {
    let database = database.inner().clone();
    let backend = backend.inner().clone();
    let agents = agents.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        services::workspace::close_checkout(&database, &backend, &agents, &checkout_id)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn archive_checkout(
    checkout_id: String,
    database: State<'_, Database>,
    agents: State<'_, std::sync::Arc<crate::services::agent::AgentService>>,
) -> Result<WorkspaceState, IpcError> {
    let database = database.inner().clone();
    let agents = agents.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        services::workspace::archive_checkout(&database, &agents, &checkout_id)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn restore_archived_worktrees(
    repo_id: String,
    database: State<'_, Database>,
    agents: State<'_, std::sync::Arc<crate::services::agent::AgentService>>,
) -> Result<WorkspaceState, IpcError> {
    let database = database.inner().clone();
    let agents = agents.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        services::workspace::restore_archived_worktrees(&database, &agents, &repo_id)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn close_missing_checkout(
    checkout_id: String,
    database: State<'_, Database>,
    backend: State<'_, std::sync::Arc<crate::terminal::TerminalBackend>>,
    agents: State<'_, std::sync::Arc<crate::services::agent::AgentService>>,
) -> Result<WorkspaceState, IpcError> {
    let database = database.inner().clone();
    let backend = backend.inner().clone();
    let agents = agents.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        services::workspace::close_missing_checkout(&database, &backend, &agents, &checkout_id)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn set_default_branch(
    repo_id: String,
    branch: String,
    database: State<'_, Database>,
) -> Result<WorkspaceState, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        services::workspace::set_default_branch(&database, &repo_id, &branch)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn select_checkout(
    checkout_id: Option<String>,
    database: State<'_, Database>,
) -> Result<WorkspaceState, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        database
            .select_checkout(checkout_id)
            .map_err(|error| IpcError::new(IpcErrorCode::InvalidCheckout, error))
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn select_session(
    session_id: Option<String>,
    database: State<'_, Database>,
) -> Result<WorkspaceState, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        database
            .select_session(session_id)
            .map_err(|error| IpcError::new(IpcErrorCode::InvalidCheckout, error))
    })
    .await
    .map_err(operation_error)?
}

fn operation_error(error: tauri::Error) -> IpcError {
    IpcError::new(
        IpcErrorCode::OperationFailed,
        format!("workspace operation could not complete: {error}"),
    )
}
