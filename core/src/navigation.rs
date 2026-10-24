//! Read-only directory discovery and native navigation persistence.
//!
//! Discovery follows Legacy's `git::resolve_repository` rules: only Git's exact
//! non-repository answer permits a plain folder; worktrees share the primary
//! root's identity, and an explicitly opened submodule is its own repository.
//! The NUL-delimited worktree listing preserves spaces, quotes and newlines.
//!
//! The native host calls these blocking operations on a worker, and serializes
//! saves. Its state file is separate from the Legacy database: no live sessions,
//! terminal layouts, compatibility readers or schema migrations are stored here.

use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::workspace::{checkout_id_for_path, repo_id_for_path, Checkout, Repo, RepoKind};

const GIT_READ_TIMEOUT: Duration = Duration::from_secs(30);
const GIT_OUTPUT_CAP: usize = 16 * 1024 * 1024;
const GIT_DIAGNOSTIC_CAP: usize = 64 * 1024;
const STATE_CAP: u64 = 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

mod changes;
mod diffs;
mod discovery;
mod git;
mod instance;
mod persistence;
mod worktrees;

#[cfg(test)]
use diffs::{parse_numstat_totals, untracked_lines};
#[cfg(test)]
use discovery::open_directory_within;
use discovery::{canonical_or_original, parse_worktrees, path_text, Worktree};
use git::*;
use persistence::validate_path;
use worktrees::default_ref_with_budget;
#[cfg(test)]
use worktrees::*;

#[cfg(test)]
pub(crate) use changes::contained_existing_ancestor;
#[cfg(test)]
pub(crate) use changes::scan;
pub use changes::{file_diff, status, ChangedFile, DiffHunk, FileDiff, GitStatus, ImageFormat};
pub use diffs::{diff_totals, diff_totals_for_checkout, DiffTotals};
pub use discovery::{open_directory, open_directory_with_budget, OpenedDirectory};
pub use git::GitReadBudget;
pub use instance::NativeInstanceLock;
pub use persistence::{load, save, state_path, NavigationState};
pub use worktrees::{
    create_worktree, remove_worktree, worktree_defaults, worktree_removal_info, RemovedWorktree,
    WorktreeDefaults, WorktreeRemovalConfirmation, WorktreeRemovalInfo,
};

/// Whether a path is the filesystem root, which is not a workdir.
///
/// A workdir is a directory somebody works in. The root holds every other
/// directory rather than being one of them, and it is where an application
/// opened from the desktop starts, so it would be offered on every launch.
pub fn is_filesystem_root(path: &Path) -> bool {
    path.is_absolute() && path.parent().is_none()
}

/// Match a cwd to the deepest unique available checkout, by path segments.
/// Missing/ambiguous checkouts never become automatic-movement destinations.
pub fn checkout_for_directory(repos: &[Repo], path: &Path) -> Option<String> {
    let canonical = path.canonicalize().ok();
    let mut best: Option<(&Checkout, usize)> = None;
    let mut ambiguous = false;
    for checkout in repos
        .iter()
        .flat_map(|repo| &repo.checkouts)
        .filter(|checkout| !checkout.is_missing)
    {
        let depth = [&checkout.path, &checkout.canonical_path]
            .into_iter()
            .filter_map(|root| {
                let root = Path::new(root);
                (path.starts_with(root)
                    || canonical
                        .as_ref()
                        .is_some_and(|path| path.starts_with(root)))
                .then(|| root.components().count())
            })
            .max();
        let Some(depth) = depth else { continue };
        match best {
            Some((_, previous)) if depth < previous => {}
            Some((previous, old_depth)) if depth == old_depth => {
                ambiguous |= previous.id != checkout.id;
            }
            _ => {
                best = Some((checkout, depth));
                ambiguous = false;
            }
        }
    }
    (!ambiguous)
        .then(|| best.map(|(checkout, _)| checkout.id.clone()))
        .flatten()
}

#[cfg(test)]
mod tests;
