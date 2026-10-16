//! Muster's native crate.
//!
//! The window and the half of the terminal that draws it. What it does not own
//! is process lifetime: [`muster_core::pty`] is the PTY backend the Tauri app
//! used, reached as a dependency instead of a path into its tree, so there is
//! exactly one implementation of bounded writes and group-safe shutdown.

/// The workspace window, retained terminal registry, and native sidebar.
pub mod app;

/// The About panel the macOS app menu's first item opens.
pub mod about;

/// Read-only central document surface.
pub mod document;

/// Central checkout diff previews.
mod diff;

/// Lazy checkout Files inspector.
pub mod inspector;

/// The macOS menu bar, and the app-level keys its items carry.
pub mod menus;

/// The icon a file or directory wears, cut into the inks it paints with.
mod file_icons;

/// The left panel: repositories, their worktrees, and the terminals in them.
pub mod sidebar;

/// Navigation projection and selection/merge rules for the native sidebar.
mod navigation;

/// The worktree creation and removal dialogs.
pub mod worktree_dialog;

/// The crumb line and the searchable menu each crumb opens.
pub mod titlebar;

/// Who owns a PTY, and the window's end of it.
pub mod session_owner;

/// The serial, bounded writer behind `core::navigation`'s state file.
pub mod state_writer;

/// The application's semantic colour roles, read from the toolkit theme.
pub mod theme;

/// Where this crate's log records go.
pub mod logging;

/// A finite, reproducible cost check for the terminal's hot paths.
///
/// Not part of the suite: every case is ignored and prints rather than
/// asserts. `scripts/perf-native.sh` is how it is run.
#[cfg(test)]
mod perf;

/// `muster_core::workspace`, re-exported so a host-side module can reach the
/// status shapes through one path.
pub use muster_core::workspace;

/// `muster_core::pty`: `TerminalBackend`, `SpawnOptions`, `OutputSink` and the
/// process-introspection module.
pub use muster_core::pty;

/// The half of the terminal that is not the drawing: the VT parser and the grid
/// on the PTY reader thread, handing the window a bounded snapshot.
pub mod terminal;

/// Direct parser-level security regressions for the vendored parser, independent
/// of PTY delivery and window timing. The shipping terminal also drives this parser.
#[cfg(test)]
mod terminal_parser;
