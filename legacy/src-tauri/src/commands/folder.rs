use std::path::PathBuf;

use crate::{
    domain::{
        folder::OpenedFolder,
        ipc::{IpcError, IpcErrorCode},
    },
    services,
};

#[tauri::command]
pub async fn open_folder(path: PathBuf) -> Result<OpenedFolder, IpcError> {
    tauri::async_runtime::spawn_blocking(move || services::folder::open_folder(&path))
        .await
        .map_err(|error| {
            IpcError::new(
                IpcErrorCode::OperationFailed,
                format!("folder operation could not complete: {error}"),
            )
        })?
}
