pub mod agent;
pub mod files;
pub mod folder;
pub mod ipc;
pub mod review;
pub mod terminal_layout;
// `muster-core` owns this module now; re-exported so the ~67 `crate::domain::workspace::`
// paths in this tree keep resolving to the one definition.
pub use muster_core::workspace;
