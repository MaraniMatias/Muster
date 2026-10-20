//! Muster's desktop interface: windows, widgets, the document editor and the VT terminal.
//!
//! Backend operations live in [`muster_core`], independently of the GUI toolkit.

/// The workspace window, retained terminal registry, and native sidebar.
pub mod app;

/// The About panel the macOS app menu's first item opens.
pub mod about;

/// The central document surface: an editor, the draft it holds, and the save it
/// writes through `muster_core::files`.
pub mod document;

/// Cross-platform confirmation dialogs.
mod confirmation;

/// Central checkout diff previews.
mod diff;

/// Line-anchored review drafts and Markdown export.
mod review;

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

/// What the OpenCode service says, read in the background and drawn in the panel.
pub mod agent_watch;

/// Following the work a terminal is doing into another worktree.
pub mod follow;

/// The crumb line and the searchable menu each crumb opens.
pub mod titlebar;

/// The preferences panel: the draft it edits and the dialog it draws.
pub mod settings;

/// Who owns a PTY, and the window's end of it.
pub mod session_owner;

/// The serial, bounded writer behind `core::navigation`'s state file.
pub mod state_writer;

/// Short-lived, dismissible messages raised by the workspace window.
mod toast;

/// The window's tooltips: the wait a trigger pays, and the popup it wears.
pub mod tooltip;

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

/// Repository, checkout and terminal status types shared with the backend.
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
