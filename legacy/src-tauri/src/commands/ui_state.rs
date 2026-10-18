use tauri::State;

use crate::{
    config::{self, AppSettings, ConfigFile},
    domain::ipc::{IpcError, IpcErrorCode},
    persistence::{AppLayoutState, CheckoutUiState, Database},
};

#[tauri::command]
pub async fn ui_layout_load(database: State<'_, Database>) -> Result<AppLayoutState, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || database.load_app_layout())
        .await
        .map_err(operation_error)?
        .map_err(validation_error)
}

#[tauri::command]
pub async fn ui_layout_save(
    layout: AppLayoutState,
    database: State<'_, Database>,
) -> Result<(), IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || database.save_app_layout(&layout))
        .await
        .map_err(operation_error)?
        .map_err(validation_error)
}

#[tauri::command]
pub async fn settings_load(config_file: State<'_, ConfigFile>) -> Result<AppSettings, IpcError> {
    let path = config_file.0.clone();
    tauri::async_runtime::spawn_blocking(move || config::load(&path))
        .await
        .map_err(operation_error)?
}

#[tauri::command]
pub async fn settings_save(
    settings: AppSettings,
    config_file: State<'_, ConfigFile>,
) -> Result<(), IpcError> {
    let path = config_file.0.clone();
    tauri::async_runtime::spawn_blocking(move || config::save(&path, &settings))
        .await
        .map_err(operation_error)?
}

#[tauri::command]
pub async fn checkout_ui_state_load(
    checkout_id: String,
    database: State<'_, Database>,
) -> Result<CheckoutUiState, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || database.load_checkout_ui_state(&checkout_id))
        .await
        .map_err(operation_error)?
        .map_err(validation_error)
}

#[tauri::command]
pub async fn checkout_ui_state_save(
    checkout_id: String,
    state: CheckoutUiState,
    database: State<'_, Database>,
) -> Result<(), IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        database.save_checkout_ui_state(&checkout_id, &state)
    })
    .await
    .map_err(operation_error)?
    .map_err(validation_error)
}

#[tauri::command]
pub async fn review_target_load(
    checkout_id: String,
    database: State<'_, Database>,
) -> Result<String, IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || database.review_target(&checkout_id))
        .await
        .map_err(operation_error)?
        .map_err(validation_error)
}

#[tauri::command]
pub async fn review_target_save(
    checkout_id: String,
    target: String,
    database: State<'_, Database>,
) -> Result<(), IpcError> {
    let database = database.inner().clone();
    tauri::async_runtime::spawn_blocking(move || database.set_review_target(&checkout_id, &target))
        .await
        .map_err(operation_error)?
        .map_err(validation_error)
}

fn operation_error(error: tauri::Error) -> IpcError {
    IpcError::new(
        IpcErrorCode::OperationFailed,
        format!("UI state operation could not complete: {error}"),
    )
}

fn validation_error(error: String) -> IpcError {
    IpcError::new(IpcErrorCode::InvalidCheckout, error)
}
