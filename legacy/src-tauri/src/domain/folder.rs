use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct OpenedFolder {
    pub path: String,
    pub name: String,
}
