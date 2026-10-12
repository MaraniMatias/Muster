//! Muster's native crate.
//!
//! The window and the half of the terminal that draws it. What it does not own
//! is process lifetime: [`muster_core::pty`] is the PTY backend the Tauri app
//! used, reached as a dependency instead of a path into its tree, so there is
//! exactly one implementation of bounded writes and group-safe shutdown.

/// The workspace window, retained terminal registry, and native sidebar.
pub mod app;

/// Navigation projection and selection/merge rules for the native sidebar.
mod navigation;

/// `muster_core::workspace`, re-exported so a host-side module can reach the
/// status shapes through one path.
pub use muster_core::workspace;

/// `muster_core::pty`: `TerminalBackend`, `SpawnOptions`, `OutputSink` and the
/// process-introspection module.
pub use muster_core::pty;

/// The half of the terminal that is not the drawing: the VT parser and the grid
/// on the PTY reader thread, handing the window a bounded snapshot.
pub mod terminal;

/// Test-only for now: it pins the vendored parser's keyboard-mode and title handling,
/// and nothing in the shipping path drives the parser yet — no native window owns a
/// `Term`, so these cases have no production caller to share with.
#[cfg(test)]
mod terminal_parser;
