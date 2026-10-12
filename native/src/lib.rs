//! Muster's native crate.
//!
//! The shipping app is still `src-tauri`; this crate is the port. What it reuses
//! from the Tauri build is reached through module paths, not through a shared
//! library crate: the PTY backend is `src-tauri/src/terminal`, brought in whole so
//! there is exactly one implementation of process lifetime, bounded writes and
//! group-safe shutdown.

/// `src-tauri/src/domain/workspace.rs`, verbatim. It is where the status shapes
/// the terminal backend returns are declared, and it depends on nothing else in
/// the Tauri tree.
pub mod domain;

/// The window: one shell, the line above it, and the two ways this app ends.
pub mod app;

/// `src-tauri/src/terminal/`, verbatim: `TerminalBackend`, `SpawnOptions`,
/// `OutputSink` and the process-introspection module. No Tauri import exists in
/// that subtree, which is what lets a native window drive it directly.
///
/// Named `pty` rather than `terminal` because that is the whole of what it does
/// -- process lifetime, bounded writes, group-safe shutdown -- and `terminal` is
/// the name the parser and grid below want. Nothing in that subtree refers to
/// itself by path, so the rename costs nothing outside this file.
#[path = "../../src-tauri/src/terminal/mod.rs"]
pub mod pty;

/// The half of the terminal that is not the drawing: the VT parser and the grid
/// on the PTY reader thread, handing the window a bounded snapshot.
pub mod terminal;

/// Test-only for now: it pins the vendored parser's keyboard-mode and title handling,
/// and nothing in the shipping path drives the parser yet — no native window owns a
/// `Term`, so these cases have no production caller to share with.
#[cfg(test)]
mod terminal_parser;