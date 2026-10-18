//! Muster's core: what a host needs to run a shell and describe what came back.
//!
//! Nothing here knows what a window is. That is the point of the split: the PTY
//! backend was the one part of the old Tauri tree with no `tauri::` import in it,
//! so a native host could drive it directly, and this crate makes that a
//! dependency instead of a path that reaches across the repository.
//!
//! What is here is what a host needs and what nothing else should have to own:
//!
//! * [`pty`] -- process lifetime, bounded writes, group-safe shutdown. One
//!   implementation, whichever host calls it.
//! * [`workspace`] -- the status shapes the backend hands back.
//!
//! What is deliberately *not* here: the VT parser, the grid, the painting, and
//! the keyboard encoding. Those belong to a host. Nothing else parses terminal
//! output, so a second crate for them would exist for one caller.
//!
//! Named `pty` rather than `terminal` because that is the whole of what it does
//! -- process lifetime, bounded writes, group-safe shutdown -- and `terminal` is
//! the name the parser and grid want. Nothing in that subtree refers to itself
//! by path, so the rename costs nothing outside this file.

/// `src-tauri/src/terminal/`, moved here whole: `TerminalBackend`,
/// `SpawnOptions`, `OutputSink` and the process-introspection module.
pub mod pty;

/// `src-tauri/src/domain/workspace.rs`, moved here whole: it is where the status
/// shapes the terminal backend returns are declared, and it depends on nothing
/// else in the old tree.
pub mod workspace;

/// Checkout and worktree discovery, the Git diffs they answer with, the worktree
/// mutations, and the native host's isolated navigation state.
pub mod navigation;

/// Safe, bounded checkout file listing and previews, and the writes a document
/// save and a review export go through. Reading a file and replacing one are the
/// same path policy on purpose, so there is one of them.
pub mod files;

/// Hands a file or a link to the application this machine has for it.
pub mod opener;

/// The preferences a person sets once and forgets they made, in `~/.muster/settings.json`.
///
/// Kept out of [`navigation`] on purpose: that is what a workspace is, and this is how things
/// look. No window is needed to read a preference, so it belongs here rather than in the host.
pub mod settings;

/// Review notes anchored to source lines in diffs.
pub mod review;

/// A client for the OpenCode service a person started, and the durable rounds that
/// make sending a review to it safe to retry.
pub mod opencode;
