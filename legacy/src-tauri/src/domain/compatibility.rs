use serde::Serialize;

#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompatibilitySnapshot {
    pub pi_installed: bool,
    pub open_code_installed: bool,
}
