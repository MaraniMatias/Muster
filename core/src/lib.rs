//! Muster's backend: processes, Git, checkout files, persistence and agent integrations.
//!
//! This crate has no GUI dependency, so backend checks can run without compiling
//! the toolkit. The application crate owns windows, widgets and the VT parser.

/// PTY process lifetime, bounded writes, group-safe shutdown and process introspection.
pub mod pty;

/// Repository, checkout and terminal status types.
pub mod workspace;

/// Checkout and worktree discovery, the Git diffs they answer with, the worktree
/// mutations, and the application's persisted navigation state.
pub mod navigation;

/// Safe, bounded checkout file listing and previews, and the writes a document
/// save and a review export go through. Reading a file and replacing one are the
/// same path policy on purpose, so there is one of them.
pub mod files;

/// Hands a file or a link to the application this machine has for it.
pub mod opener;

/// The preferences a person sets once and forgets they made, in `~/.muster/settings.json`.
///
/// Preferences are stored separately from the workspace managed by [`navigation`].
pub mod settings;

/// Review notes anchored to source lines in diffs.
pub mod review;

/// A client for the OpenCode service a person started, and the durable rounds that
/// make sending a review to it safe to retry.
pub mod opencode;

/// Where Pi is, and the private file a review is written to before Pi reads it.
pub mod pi;
