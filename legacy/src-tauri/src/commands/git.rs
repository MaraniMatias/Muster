use tauri::{AppHandle, Emitter, State};

use crate::{
    domain::ipc::{IpcError, IpcErrorCode},
    persistence::Database,
    services::git::{
        self, GitCheckoutDiffStats, GitDiffPage, GitFileDiff, GitFileDiffStats, GitStatus,
        GitWatcherManager,
    },
};

#[tauri::command]
pub async fn git_status(
    checkout_id: String,
    database: State<'_, Database>,
    watchers: State<'_, std::sync::Arc<GitWatcherManager>>,
) -> Result<GitStatus, IpcError> {
    let database = database.inner().clone();
    let watchers = watchers.inner().clone();
    tauri::async_runtime::spawn_blocking(move || git::status(&database, &watchers, &checkout_id))
        .await
        .map_err(operation_error)?
}

#[tauri::command]
pub async fn git_diff_stats(
    checkout_id: String,
    database: State<'_, Database>,
    watchers: State<'_, std::sync::Arc<GitWatcherManager>>,
) -> Result<GitFileDiffStats, IpcError> {
    let database = database.inner().clone();
    let watchers = watchers.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        git::diff_stats(&database, &watchers, &checkout_id)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn git_checkout_diff_stats(
    database: State<'_, Database>,
    watchers: State<'_, std::sync::Arc<GitWatcherManager>>,
) -> Result<GitCheckoutDiffStats, IpcError> {
    let database = database.inner().clone();
    let watchers = watchers.inner().clone();
    tauri::async_runtime::spawn_blocking(move || git::checkout_diff_stats(&database, &watchers))
        .await
        .map_err(operation_error)?
}

#[tauri::command]
pub async fn git_diff(
    checkout_id: String,
    path: String,
    database: State<'_, Database>,
) -> Result<GitFileDiff, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || git::diff(&database, &checkout_id, &path))
        .await
        .map_err(operation_error)?
}

#[tauri::command]
pub async fn git_diff_page(
    checkout_id: String,
    path: String,
    offset: usize,
    limit: usize,
    database: State<'_, Database>,
) -> Result<GitDiffPage, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        git::diff_page(&database, &checkout_id, &path, offset, limit)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn git_viewed_files(
    checkout_id: String,
    database: State<'_, Database>,
) -> Result<Vec<String>, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || git::viewed_files(&database, &checkout_id))
        .await
        .map_err(operation_error)?
}

#[tauri::command]
pub async fn git_mark_viewed(
    checkout_id: String,
    path: String,
    app: AppHandle,
    database: State<'_, Database>,
) -> Result<(), IpcError> {
    let database = database.inner().clone();
    let event_checkout_id = checkout_id.clone();
    tauri::async_runtime::spawn_blocking(move || git::mark_viewed(&database, &checkout_id, &path))
        .await
        .map_err(operation_error)??;
    let _ = app.emit("git-viewed-changed", &event_checkout_id);
    Ok(())
}

/// Watches a whole repository rather than one checkout. Its worktrees share a Git directory, so
/// a commit in one of them moves the merge base every sibling counts its lines against, and a
/// watcher per checkout would report that commit N times while still leaving the N-1 rows
/// describing a change set that no longer exists.
#[tauri::command]
pub async fn git_watch_repo(
    repo_id: String,
    registration_id: String,
    expected_checkout_ids: Vec<String>,
    app: AppHandle,
    database: State<'_, Database>,
    watchers: State<'_, std::sync::Arc<GitWatcherManager>>,
) -> Result<(), IpcError> {
    let database = database.inner().clone();
    let watchers = watchers.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let plan = git::watch_plan(&database, &repo_id)?;
        if !git::watch_plan_matches_request(&plan, &expected_checkout_ids) {
            return Err(IpcError::new(
                IpcErrorCode::OperationFailed,
                "repository checkout plan changed while starting its watcher",
            ));
        }
        watchers.watch(app, repo_id, registration_id, plan)
    })
    .await
    .map_err(operation_error)?
}

#[tauri::command]
pub async fn git_unwatch_repo(
    repo_id: String,
    database: State<'_, Database>,
    watchers: State<'_, std::sync::Arc<GitWatcherManager>>,
) -> Result<(), IpcError> {
    let database = database.inner().clone();
    let watchers = watchers.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let checkout_ids = git::repo_checkout_ids(&database, &repo_id);
        watchers.unwatch(&repo_id, &checkout_ids);
    })
    .await
    .map_err(operation_error)
}

fn operation_error(error: tauri::Error) -> IpcError {
    IpcError::new(
        IpcErrorCode::OperationFailed,
        format!("Git operation could not complete: {error}"),
    )
}
