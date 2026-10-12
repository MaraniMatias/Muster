//! The workspace shapes the reused Tauri modules refer to.
//!
//! Only `workspace.rs` is needed, and it is the real file: `TerminalBackend`
//! returns `TerminalSessionStatus`/`TerminalProcessState` from it, and it in turn
//! needs nothing from the rest of `src-tauri/src/domain`.

#[path = "../../src-tauri/src/domain/workspace.rs"]
pub mod workspace;