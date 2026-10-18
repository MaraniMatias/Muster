use crate::{domain::compatibility::CompatibilitySnapshot, services::executable};

#[tauri::command]
pub async fn compatibility_snapshot() -> CompatibilitySnapshot {
    tauri::async_runtime::spawn_blocking(|| CompatibilitySnapshot {
        pi_installed: executable::find_executable("pi").is_some(),
        open_code_installed: executable::find_executable("opencode").is_some(),
    })
    .await
    .unwrap_or_default()
}
