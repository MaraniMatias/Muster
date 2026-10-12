use std::{
    collections::{hash_map::DefaultHasher, BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    hash::{Hash, Hasher},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt, process::CommandExt},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(test)]
use std::cell::{Cell, RefCell};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::{
    domain::{
        ipc::{IpcError, IpcErrorCode},
        workspace::{Checkout, Repo, RepoKind},
    },
    persistence::Database,
    services::checkout::resolve_checkout_path,
};

const WATCH_DEBOUNCE: Duration = Duration::from_millis(220);
const WATCH_MAX_BATCH: Duration = Duration::from_secs(1);
const STATUS_CHANGED_EVENT: &str = "git-status-changed";
const FILE_ACTIVITY_EVENT: &str = "checkout-file-activity";
const WORKTREES_CHANGED_EVENT: &str = "git-worktrees-changed";
const WATCH_FAILED_EVENT: &str = "git-watch-failed";
const SMALL_DIFF_LINES: usize = 1000;
const SMALL_DIFF_BYTES: usize = 512 * 1024;
const MAX_DIFF_LINES: usize = 100_000;
const MAX_DIFF_BYTES: usize = 32 * 1024 * 1024;
const MAX_DIFF_LINE_BYTES: usize = 64 * 1024;
const MAX_DIFF_HUNKS: usize = 10_000;
/// How much of a file is read looking for the NUL byte that makes Git call it binary.
const BINARY_SNIFF_BYTES: usize = 8_000;
/// How much of a file is in memory while its lines are counted, and is also how much of it a
/// first read hands over, which is why it is larger than `BINARY_SNIFF_BYTES`: a file that is
/// longer than the window has all of the window in hand before the window is judged. An
/// untracked file in a working checkout is an unignored build artefact more often than not, and
/// this is the ceiling on what counting one of those costs.
const COUNT_BLOCK_BYTES: usize = 64 * 1024;
/// How much of a file's own text a diff carries so its syntax can be read in context.
///
/// A grammar reads a file, not a hunk: the lines inside `<script setup lang="ts">` are markup to a
/// grammar that never saw the opening tag, so a patch on its own highlights the wrong language or
/// none at all. The text is what the grammar reads and not what the diff draws (the patch still
/// draws every line). So the cap is a cap on the reading of one file, and past it the file is read
/// the way this app read every file before any of this. `SMALL_DIFF_BYTES` is the same size a patch
/// is allowed to be, so a file that small in this repository is carried whole.
const MAX_SYNTAX_CONTEXT_BYTES: usize = SMALL_DIFF_BYTES;
/// How many checkouts are read at once when the sidebar names all of them. Each reading is a
/// handful of `git` processes, so a workspace with many worktrees would otherwise fork all of
/// them into the machine in the same moment. The point is to overlap the work, not to let the
/// work starve itself.
const PARALLEL_READS: usize = 8;
/// How long a read-only Git process may take before it is stopped.
///
/// A read that never finishes is not a slow checkout: it is a process nobody is going to answer. An
/// `index.lock` another process is holding, a credential helper waiting on a question nobody can
/// see, `gpg` waiting for a PIN, an LFS filter waiting on a network that is not there. The IPC call
/// that started it sits on a `spawn_blocking` slot that would not come back, so the deadline is what
/// keeps the app answering at all. It is longer than any of these commands takes in a repository of
/// any ordinary size -- a status, a list of changed names, a ref lookup, a merge base -- because the
/// cost of being wrong in that direction is one slower refresh and the cost of being wrong in the
/// other is a checkout that never shows its changes again.
pub(crate) const GIT_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// How often a running read is asked whether it is done. Short first so a fast read pays almost
/// nothing for having a deadline, long later so a hung one does not spend the wait asking.
const GIT_READ_POLL_MIN: Duration = Duration::from_millis(1);
const GIT_READ_POLL_MAX: Duration = Duration::from_millis(25);
/// How long Git's own output is collected for after the process has been stopped.
const GIT_PIPE_DRAIN: Duration = Duration::from_secs(2);
/// How much of what a stopped Git said travels in the error it leaves behind.
const GIT_DIAGNOSTIC_LINES: usize = 3;
const GIT_DIAGNOSTIC_WIDTH: usize = 120;
/// How much of what a read's Git printed on its standard output is kept.
///
/// The largest thing this runner carries is one checkout's change list: `status --porcelain=v2 -z`,
/// `diff --name-status -z` and `diff --numstat -z` each print on the order of a hundred bytes per
/// changed file, path included. 16 MiB is therefore about a hundred thousand changed files in one
/// checkout -- a working tree nobody reads in a sidebar, and half of what `MAX_DIFF_BYTES` already
/// accepts for the patch of a single file.
///
/// The deadline bounds how long a read may take and this bounds how much of it is kept, because
/// they are different limits: a Git that prints for thirty seconds and a Git that prints thirty
/// gigabytes both satisfy the first one. What arrives past this is an error rather than what came
/// first. Every caller here reads what came back as the state of a whole checkout, and the first
/// sixteen megabytes of a `status` is not a status.
const GIT_OUTPUT_CAP: usize = 16 * 1024 * 1024;
/// How much of what Git said about a failure is kept.
///
/// A diagnostic travels under `GIT_DIAGNOSTIC_LINES` lines of `GIT_DIAGNOSTIC_WIDTH` characters, so
/// a mebibyte of one is orders of magnitude more than any error message is long. It is a ceiling
/// rather than a summary for the reason the summary is not enough on its own: callers read an
/// empty pipe as an answer of its own -- a ref that is not there says so by printing nothing -- and
/// half a diagnostic is not an answer.
const GIT_DIAGNOSTIC_CAP: usize = 1024 * 1024;
/// How many paths a burst may carry before the batch stops being filtered by path.
///
/// The paths are what `check-ignore` is asked about, one call for the whole batch, and a batch that
/// stays inside this is answered by asking Git rather than by re-reading every checkout. `pnpm
/// install` and a build write tens of thousands of files and stay inside it; a generator writing
/// hundreds of thousands does not, and pays a refresh instead -- which is the safe direction,
/// because a refresh is a few `git` processes and a burst that was never asked about Git's rules is
/// a checkout whose row describes a change set nobody looked at.
const WATCH_PATH_BUDGET: usize = 50_000;
/// How many bytes of those paths, path plus the NUL `check-ignore --stdin` is fed one per path.
///
/// Counted in bytes as well as in paths because the copy that is made of the whole batch to hand it
/// to Git is what the burst costs, and a build writing deep paths costs more per file than one
/// writing shallow ones.
const WATCH_PATH_BYTE_BUDGET: usize = 4 * 1024 * 1024;
pub const MAX_DIFF_PAGE_LINES: usize = 32;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitChangedFile {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub status: String,
    /// Absent rather than zero when Git has no count for the file: `--numstat` prints `-` for
    /// both columns of one it cannot measure, a binary file among them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additions: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deletions: Option<u64>,
}

/// The lines a change set adds and removes.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffStats {
    pub additions: u64,
    pub deletions: u64,
}

/// The totals of every registered Git checkout, keyed by checkout id.
pub type GitCheckoutDiffStats = BTreeMap<String, GitDiffStats>;

/// The changed files of one checkout, each with the lines it adds and removes.
pub type GitFileDiffStats = Vec<GitChangedFile>;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    pub default_branch: String,
    pub ahead_count: u32,
    pub files: Vec<GitChangedFile>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileDiff {
    pub path: String,
    pub patch: String,
    /// What this diff is of, hashed from the lines the scan read. It is what a reader compares two
    /// answers with, and `patch` cannot be that: a large, a too large and a binary diff carry no
    /// patch at all, so any two of them compare equal while the pages drawn behind them are not.
    /// Changing when what is on screen changes and not when the workdir happens to have been written
    /// to, so a refresh that moved nothing leaves the open note composer in state.
    pub revision: String,
    /// The text of each side of the diff, whole, which is what a grammar reads rather than the
    /// patch's hunks. Absent rather than empty when there is no text to read: a binary or symlink
    /// diff, a diff too large to hold, a side that does not exist (an added file's old side, a
    /// removed file's new one), and a file past `MAX_SYNTAX_CONTEXT_BYTES`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_content: Option<String>,
    pub is_binary: bool,
    pub large: bool,
    pub too_large: bool,
    pub total_lines: usize,
    pub hunks: Vec<GitDiffHunk>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffHunk {
    pub start_line: usize,
    pub end_line: usize,
    pub title: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffPageLine {
    pub index: usize,
    pub kind: GitDiffLineKind,
    pub text: String,
    pub old_line_number: Option<u64>,
    pub new_line_number: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GitDiffLineKind {
    Hunk,
    Added,
    Removed,
    Context,
    Meta,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffPage {
    pub path: String,
    pub start_line: usize,
    pub total_lines: usize,
    pub lines: Vec<GitDiffPageLine>,
}

#[derive(Default)]
pub struct GitWatcherManager {
    /// One watcher per repository, not per checkout. The worktrees of a repo share a Git
    /// directory, so watching them one at a time means every write in it is reported once per
    /// worktree, the kernel walks the same tree the same number of times, and a commit still
    /// leaves the siblings holding numbers taken against a merge base that has moved.
    watchers: Arc<Mutex<BTreeMap<String, RepoWatcher>>>,
    /// What each checkout was when it was last read, kept here because the same debounced
    /// change that refreshes the file list is what makes it stale. `Arc` so a watch thread can
    /// mark its own entry without borrowing the manager.
    snapshots: Arc<GitSnapshotCache>,
}

struct RepoWatcher {
    _watcher: RecommendedWatcher,
    queue: WatchQueue,
    failed: Arc<AtomicBool>,
    token: Arc<()>,
    /// The renderer's request generation, echoed by runtime failure events.
    registration_id: String,
    /// The plan this watcher was built for, so a worktree joining or leaving the repository is
    /// answered by a watcher that knows about it rather than by the one that predates it.
    plan: RepoWatchPlan,
}

/// What one repository's watcher has to know to say which checkouts a change speaks for.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct RepoWatchPlan {
    /// The repository this plan watches, named in the one signal no checkout can speak for.
    pub repo_id: String,
    /// Each live worktree's directory, mapped to the checkout that lives in it, so a file
    /// written there is attributed to one row rather than to the whole repo.
    pub roots: BTreeMap<PathBuf, String>,
    /// Each worktree's own Git directory, which holds its index and its HEAD. Staging in one
    /// worktree is that worktree's business; only a shared ref is the repository's.
    pub git_dirs: BTreeMap<PathBuf, String>,
    /// The Git directory the worktrees share. A ref or index write there moves the merge base
    /// every sibling diffs against, so it speaks for all of them and not for one.
    pub common_dir: Option<PathBuf>,
    /// Every checkout whose Git context this watcher can actually monitor.
    pub all: Vec<String>,
    /// Live checkout IDs captured before resolving their paths. Runtime failure signals use these
    /// as their request identity, including checkouts the watcher could not observe.
    pub requested: Vec<String>,
}

/// The checkouts a batch of filesystem changes speaks for: the ones whose own files moved, the
/// whole repository when what moved was a ref every one of them reads, and the repository whose
/// own list of worktrees moved.
#[derive(Debug, Default, PartialEq, Eq)]
struct WatchUpdate {
    status: Vec<String>,
    activity: Vec<String>,
    /// Repositories a worktree joined or left. The rows a repository is made of are not a change
    /// any checkout can speak for, so the repository is named and its registration is read again
    /// rather than refreshed.
    worktrees: Vec<String>,
    /// The checkout files a batch moved, relative to the checkout they landed in, so Git can be
    /// asked once for the whole batch whether its own rules ignore them.
    ///
    /// A set rather than a list because a build writes the same file more than once and the query
    /// is one that answers per path: the same path twice is one answer, and the batch is merged
    /// here so a burst that writes a tree a thousand times does not grow a thousand copies of it.
    paths: BTreeMap<String, BTreeSet<PathBuf>>,
    /// The checkouts this batch names for something a checkout file cannot explain: their own Git
    /// directory, or the one every worktree shares. A write there moves an index or a merge base
    /// whatever the files beside it are, so no path can argue them out of being re-read.
    pinned: BTreeSet<String>,
    /// Whether this batch let its paths go rather than carry more of them than the budget allows.
    ///
    /// It travels with the answer because the meaning outlives the budget that spent it. The worker
    /// rebuilds a slot around every batch it answers, and a rebuilt slot that began with a whole
    /// budget would let the next, smaller update carry its own paths into a batch that has already
    /// decided to refresh every checkout it names -- which reaches the reader as exactly those
    /// paths, and drops the ones the oversized update was carrying.
    spent: bool,
}

/// One checkout's file activity, with the paths the batch moved inside it.
///
/// The paths are what let a reader tell whether the write is one it depends on. Activity on its own
/// says that something landed in this checkout, and a document whose own bytes came back unchanged
/// cannot answer from that whether one of the figures it draws is the file that moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileActivity {
    pub checkout_id: String,
    /// Relative to the checkout and `/`-joined, because that is the form a relative reference in a
    /// document resolves to.
    ///
    /// Empty says the batch was too large to carry what it moved (`PathBudget` lets the paths go
    /// rather than hold an unbounded payload), so it reads as "something moved here and this cannot
    /// say what", never as "nothing moved".
    pub paths: Vec<String>,
}

/// The activity signal with the paths each checkout's batch named, in the order the batch did.
fn file_activity(update: &WatchUpdate) -> Vec<FileActivity> {
    update
        .activity
        .iter()
        .map(|checkout_id| FileActivity {
            checkout_id: checkout_id.clone(),
            paths: update
                .paths
                .get(checkout_id)
                .map(|moved| {
                    moved
                        .iter()
                        .filter_map(|path| checkout_relative(path))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect()
}

/// A watched path as a relative reference in a document spells it.
///
/// `components()` rather than `to_str`: a path that is not UTF-8 is one a Markdown reference
/// cannot name either, and the lossy form simply fails to match the reference that would.
fn checkout_relative(path: &Path) -> Option<String> {
    let relative = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    (!relative.is_empty()).then_some(relative)
}

/// What a burst has been told and what it is still carrying toward the answer.
///
/// The budget is state of its own rather than two numbers written into `WatchUpdate` because it
/// spans the merges between two wakeups: it counts what the slot is carrying, so it is asked after
/// every path that arrives instead of walking the whole map every time. No `Default` on purpose:
/// a slot with no budget in it spends it on the first path, and the one place a slot is made is
/// `holding`, which brings the budget a watcher runs on.
#[derive(Debug, PartialEq, Eq)]
struct WatchSlot {
    update: WatchUpdate,
    budget: PathBudget,
}

impl WatchSlot {
    fn holding(update: WatchUpdate) -> Self {
        Self {
            update,
            budget: PathBudget::watching(),
        }
    }

    fn merge_update(&mut self, update: WatchUpdate) {
        let WatchUpdate {
            status,
            activity,
            worktrees,
            paths,
            pinned,
            spent,
        } = update;
        for checkout_id in status {
            if !self.update.status.contains(&checkout_id) {
                self.update.status.push(checkout_id);
            }
        }
        for checkout_id in activity {
            if !self.update.activity.contains(&checkout_id) {
                self.update.activity.push(checkout_id);
            }
        }
        for repo_id in worktrees {
            if !self.update.worktrees.contains(&repo_id) {
                self.update.worktrees.push(repo_id);
            }
        }
        self.update.pinned.extend(pinned);
        // Merged in before the paths are: an update whose own batch already let its paths go spends
        // this merge too, or the paths it carries would be the only ones the answer has.
        self.update.spent |= spent;
        self.budget.merge_paths(&mut self.update, paths);
    }

    /// The batch answered so far, and a whole budget behind it.
    ///
    /// Both halves at once, because taking the answer while leaving a spent budget spent is how a
    /// watcher ends up refreshing every checkout forever after one enormous build: the burst that
    /// spent it is over, and the next one starts from nothing.
    fn take(&mut self) -> WatchUpdate {
        std::mem::replace(self, Self::holding(WatchUpdate::default())).update
    }
}

/// How much of a burst's payload is carried before the batch stops being filtered by path.
///
/// A path count and a byte budget rather than one of them, because a burst of shallow paths and a
/// burst of deep ones cost very different amounts to hold and to hand to Git, and either one can be
/// the one that grows without bound. Counted as the paths arrive rather than by walking what is
/// already carried, because the budget is asked after every path of every event, and a walk would
/// make a large burst quadratic in itself.
///
/// `Default` is a budget of nothing, which is what a test asks for when it sets the two numbers it
/// cares about; a watcher runs on `watching`.
#[derive(Debug, Default, PartialEq, Eq)]
struct PathBudget {
    /// How many paths this burst may carry, and how many bytes of them.
    max_paths: usize,
    max_bytes: usize,
    /// How many it is carrying now, and how many bytes of those.
    paths: usize,
    bytes: usize,
}

impl PathBudget {
    /// The budget a watcher runs on, in the units `WATCH_PATH_BUDGET` is derived in.
    fn watching() -> Self {
        Self {
            max_paths: WATCH_PATH_BUDGET,
            max_bytes: WATCH_PATH_BYTE_BUDGET,
            ..Self::default()
        }
    }

    /// Carries one event's paths into the batch, or stops carrying them.
    ///
    /// What it reads to decide that is `WatchUpdate::spent` rather than a counter of its own: the
    /// fact that a batch let its paths go belongs to the batch, so every merge into it -- including
    /// the ones into a slot rebuilt around an answer that already spent its budget -- spends too.
    fn merge_paths(
        &mut self,
        merged: &mut WatchUpdate,
        paths: BTreeMap<String, BTreeSet<PathBuf>>,
    ) {
        // Named before they are consumed: every checkout this event speaks for is one the answer has
        // to mention, whether or not one of its paths is carried.
        let named: Vec<String> = paths.keys().cloned().collect();
        // Merged into ordered sets rather than pushed onto a list: a burst writes the same file more
        // than once, and the answer this feeds is per path, so a list would hold a thousand copies of
        // one path and ask Git about it a thousand times.
        for (checkout_id, moved) in paths {
            if merged.spent {
                break;
            }
            let carried = merged.paths.entry(checkout_id).or_default();
            for path in moved {
                // The NUL goes into the answer too: it is what `check-ignore --stdin` is fed after
                // every path, so a path costs that much whether or not this is where it is counted.
                let cost = path.as_os_str().as_bytes().len() + 1;
                if self.paths >= self.max_paths || self.bytes + cost > self.max_bytes {
                    merged.spent = true;
                    break;
                }
                if carried.insert(path) {
                    self.paths += 1;
                    self.bytes += cost;
                }
            }
        }
        if merged.spent {
            self.spend(merged, &named);
        }
    }

    /// What spending the budget costs: the paths are let go and every checkout the batch names is
    /// pinned for a refresh, and its activity still stands.
    ///
    /// A full refresh rather than a guess. `check-ignore` is what turns a burst into "nothing in
    /// here matters", and past this the batch is too big to ask about: dropping the paths instead
    /// would leave a build that wrote a hundred thousand files looking like a checkout nobody
    /// touched, which is a stale row rather than a slow one. `pinned` is the existing way of saying
    /// a checkout is re-read whatever its own files look like, and it is what keeps an emptied
    /// `paths` from being read as "every one of these was ignored".
    fn spend(&mut self, merged: &mut WatchUpdate, named: &[String]) {
        let affected: BTreeSet<String> = merged.paths.keys().chain(named).cloned().collect();
        merged.paths.clear();
        // On the batch rather than on this budget, so what the paths cost is a fact about the answer
        // and not about the counters that happened to notice it: the batch has already decided to
        // refresh every checkout it names, and a later path is not carried, but it is still a change
        // to a checkout that is about to be re-read.
        merged.spent = true;
        self.paths = 0;
        self.bytes = 0;
        for checkout_id in affected {
            if !merged.status.contains(&checkout_id) {
                merged.status.push(checkout_id.clone());
            }
            merged.pinned.insert(checkout_id);
        }
    }
}

/// What travels the queue: a wakeup, a failure, or the end.
///
/// A change is not one of these. It is merged into the slot behind the wakeup, so the worker never
/// has to read a queue of thousands to learn what a burst moved.
#[derive(Debug, PartialEq, Eq)]
enum WatchSignal {
    Wake,
    Failed(String),
    Stop,
}

/// What the callback side of a watcher sends: one wakeup of capacity, and the changes that came
/// with it.
///
/// The queue carries the wakeup rather than the change. A build writes thousands of files and a
/// checkout writes an index and a ref, and the debounce answers the whole burst as one refresh, so
/// one message per event is one message per file held in memory and merged afterwards for an answer
/// nobody reads in between. Merging into the slot first costs one message for a burst of any size,
/// and the slot is emptied as soon as the worker looks at it. `merge_update` does the merging, so a
/// burst still names every checkout it moved -- a checkout named twice is still one row, and a
/// worktree that joined is still named -- which is the same answer each event used to give on its
/// own.
///
/// `Stop` and `Failed` are not changes and do not queue behind one: they are sent, which waits for
/// the worker to take them, because a watcher that cannot end holds its thread and its watch
/// descriptors for as long as the app runs, and a watcher whose own death went unreported keeps
/// looking alive to whoever asks after it. A wakeup that will not fit is not sent at all: the one
/// already in the queue is the promise that the slot will be read, and the change that failed to
/// send is in that slot.
#[derive(Clone)]
struct WatchQueue {
    merged: Arc<Mutex<WatchSlot>>,
    wake: mpsc::SyncSender<WatchSignal>,
}

impl WatchQueue {
    fn new() -> (Self, WatchInbox) {
        let (wake, receiver) = mpsc::sync_channel(1);
        let merged = Arc::new(Mutex::new(WatchSlot::holding(WatchUpdate::default())));
        (
            Self {
                merged: merged.clone(),
                wake,
            },
            WatchInbox { receiver, merged },
        )
    }

    fn changed(&self, update: WatchUpdate) {
        if let Ok(mut merged) = self.merged.lock() {
            merged.merge_update(update);
        }
        // Merged before the wakeup is offered, so a wakeup in the queue always has this change
        // waiting behind it. Full says a wakeup is already there to read the slot, which is why
        // dropping this one loses nothing -- and why the callback is never left waiting on the
        // worker it is feeding.
        let _ = self.wake.try_send(WatchSignal::Wake);
    }

    /// Offered, never queued: a watcher whose watch stream died has to be heard about even when the
    /// queue is full, and a worker that is already gone is the one case where nobody is left to
    /// hear it. `false` says the failure is being logged instead of answered.
    fn failed(&self, message: String) -> bool {
        self.wake.send(WatchSignal::Failed(message)).is_ok()
    }

    fn stop(&self) {
        let _ = self.wake.send(WatchSignal::Stop);
    }
}

/// The worker thread's end of the queue, and the only place the merged slot is read.
struct WatchInbox {
    receiver: mpsc::Receiver<WatchSignal>,
    merged: Arc<Mutex<WatchSlot>>,
}

impl WatchInbox {
    fn recv(&self) -> Result<WatchWakeup, mpsc::RecvError> {
        self.woken(self.receiver.recv())
    }

    fn recv_timeout(&self, timeout: Duration) -> Result<WatchWakeup, mpsc::RecvTimeoutError> {
        self.woken(self.receiver.recv_timeout(timeout))
    }

    #[cfg(test)]
    fn try_recv(&self) -> Result<WatchWakeup, mpsc::TryRecvError> {
        self.woken(self.receiver.try_recv())
    }

    /// A wakeup is answered as what woke it: the changes that merged while it was on its way.
    fn woken<T>(&self, signal: Result<WatchSignal, T>) -> Result<WatchWakeup, T> {
        match signal {
            Ok(WatchSignal::Wake) => Ok(WatchWakeup::Changed(self.take_merged())),
            Ok(WatchSignal::Failed(error)) => Ok(WatchWakeup::Failed(error)),
            Ok(WatchSignal::Stop) => Ok(WatchWakeup::Stop),
            Err(error) => Err(error),
        }
    }

    fn take_merged(&self) -> WatchUpdate {
        match self.merged.lock() {
            Ok(mut merged) => merged.take(),
            Err(_) => WatchUpdate::default(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum WatchWakeup {
    Changed(WatchUpdate),
    Failed(String),
    Stop,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WatchFailure {
    repo_id: String,
    checkout_ids: Vec<String>,
    registration_id: String,
}

#[derive(Clone)]
struct GitContext {
    repo: Repo,
    checkout: Checkout,
    root: PathBuf,
}

#[derive(Clone)]
struct Snapshot {
    status: GitStatus,
    merge_base: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GitCounts {
    files: GitFileDiffStats,
    totals: GitDiffStats,
}

/// One checkout snapshot, kept next to the watchers because reading one costs several `git`
/// processes and the sidebar asks about every checkout at once.
///
/// The file list, the row totals and the merge base they were taken against are all views of
/// the same reading, so one entry answers all three and a change drops it whole. `git status`
/// and the counts of one checkout are therefore the same snapshot rather than two readings of
/// the same working tree.
///
/// Every Git checkout in the workspace is watched, so an entry is invalidated by a real change
/// signal. That is what makes holding one honest, and it is why a checkout the user is not
/// looking at carries numbers as fresh as the one they are.
#[derive(Clone)]
struct CachedGitSnapshot {
    /// Choosing another default branch moves the base ref, which no watcher can see, so the
    /// entry is matched against it instead of invalidated.
    default_branch: Option<String>,
    revision: u64,
    stale: bool,
    snapshot: Snapshot,
    /// Filled in the first time something asks for the lines rather than the file list, and
    /// read back by the next ask. The sidebar and the Changes tab both want them on every
    /// change and neither should pay for the other: a checkout nobody touched is a lookup.
    counts: Option<GitCounts>,
}

#[derive(Default)]
struct GitSnapshotCache {
    entries: Mutex<BTreeMap<String, CachedGitSnapshot>>,
    revisions: Mutex<BTreeMap<String, u64>>,
    /// The file lists being read right now. A caller that arrives while one is running waits for
    /// its answer instead of walking the same working tree a second time.
    reading_snapshots: Mutex<Readings<Snapshot>>,
    /// The line counts being read right now, for the same reason.
    reading_counts: Mutex<Readings<GitCounts>>,
}

/// What a read depends on: the checkout, the base ref it is counted against, and the revision it
/// started at.
///
/// All three are in the key because a running read cannot answer for a different one of them. A
/// change that lands mid-read moves the revision, and a caller arriving after it must read the tree
/// again rather than be handed the answer the running read is about to publish -- that answer
/// describes a working tree that has already moved. The base ref is the same argument one step back:
/// a read against one merge base is not the answer for another. Two reads instead of one is the
/// safe direction, and it costs processes rather than correctness.
type ReadKey = (String, Option<String>, Option<u64>);

/// The reads running right now, by what they are reading and what they depend on.
type Readings<T> = BTreeMap<ReadKey, Arc<ReadingInFlight<T>>>;

/// One read in flight, and the answer every caller that arrived while it ran is waiting for.
///
/// The file list and its line counts are read separately because they are read for different
/// reasons, and they are shared the same way: the first caller publishes itself, lets the lock go
/// and does the work, and the rest find that read and wait for its answer instead of starting the
/// same `git` processes again. Nothing happens under the lock but the two lookups, and the
/// processes never happen under it at all, so a read that takes its whole deadline does not keep
/// another reader of the cache waiting.
struct ReadingInFlight<T> {
    outcome: Mutex<Option<Result<T, IpcError>>>,
    published: Condvar,
}

impl<T> ReadingInFlight<T> {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            outcome: Mutex::new(None),
            published: Condvar::new(),
        })
    }

    /// Hands the answer to the waiters. Only the first publication is the answer: this read is over
    /// once, and whoever is still waiting is waiting for what it decided.
    fn publish(&self, outcome: Result<T, IpcError>) {
        if let Ok(mut published) = self.outcome.lock() {
            if published.is_none() {
                *published = Some(outcome);
            }
        }
        self.published.notify_all();
    }
}

impl<T: Clone> ReadingInFlight<T> {
    /// The answer of the read that is already running, shared rather than repeated.
    ///
    /// `None` says the read has not answered inside the wait and this caller stopped waiting for
    /// it, which is not an error: it reads the tree for itself, which costs processes and is never
    /// wrong. A wait with no bound at all is a caller that never returns, and an error here would
    /// put a Git failure on a row that only had to wait.
    fn wait(&self) -> Option<Result<T, IpcError>> {
        let deadline = Instant::now() + GIT_READ_WAIT;
        let mut published = self.outcome.lock().ok()?;
        loop {
            if let Some(outcome) = published.as_ref() {
                return Some(outcome.clone());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let (next, _) = self.published.wait_timeout(published, remaining).ok()?;
            published = next;
        }
    }
}

/// Who does a read: the caller that got there first, or one that waits for the read already
/// answering for it.
enum ReadTurn<T> {
    Reading(Arc<ReadingInFlight<T>>),
    Waiting(Arc<ReadingInFlight<T>>),
}

/// Reads one thing for one checkout once, shared with whoever asked for it while it was running.
///
/// `None` from `begin_read` says the lock could not be taken, and the caller then reads on its own:
/// a lock this poisoned cannot be shared, but the read itself still has an answer to give.
fn begin_read<T>(readings: &Mutex<Readings<T>>, key: &ReadKey) -> Option<ReadTurn<T>> {
    let mut running = readings.lock().ok()?;
    if let Some(reading) = running.get(key) {
        return Some(ReadTurn::Waiting(Arc::clone(reading)));
    }
    let reading = ReadingInFlight::new();
    running.insert(key.clone(), Arc::clone(&reading));
    Some(ReadTurn::Reading(reading))
}

/// Takes a read out of the map, if it is still the one running there.
///
/// By pointer, because a read that a checkout being forgotten outlived is not the read the next
/// caller is waiting for: removing the newer one would leave it waiting for an answer nobody holds.
/// The answer is published before this, so a caller arriving in between waits for a read that has
/// already finished rather than starting a second one.
fn finish_read<T>(
    readings: &Mutex<Readings<T>>,
    key: &ReadKey,
    reading: &Arc<ReadingInFlight<T>>,
    outcome: Result<T, IpcError>,
) {
    reading.publish(outcome);
    if let Ok(mut running) = readings.lock() {
        if running
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, reading))
        {
            running.remove(key);
        }
    }
}

/// Runs one read for one checkout and revision, or waits for the one already running for it.
fn shared_read<T: Clone>(
    readings: &Mutex<Readings<T>>,
    key: &ReadKey,
    read: impl FnOnce() -> Result<T, IpcError>,
) -> Result<T, IpcError> {
    if key.2.is_none() {
        return read();
    }
    match begin_read(readings, key) {
        Some(ReadTurn::Waiting(reading)) => reading.wait().unwrap_or_else(read),
        Some(ReadTurn::Reading(reading)) => {
            let outcome = read();
            finish_read(readings, key, &reading, outcome.clone());
            outcome
        }
        None => read(),
    }
}

/// Counts the Git processes the reads below start, per checkout root, so a test can tell one read
/// from N reads of the same working tree rather than inferring it from timing.
///
/// Test-only because it is the only thing here that exists to be observed: the reads themselves
/// fork `git` into the machine, and a duplicate read is invisible in the answer -- it is the same
/// answer, computed twice -- so the process count is the only honest witness.
#[cfg(test)]
static GIT_READS: Mutex<BTreeMap<PathBuf, usize>> = Mutex::new(BTreeMap::new());

#[cfg(test)]
fn count_git_read(root: &Path) {
    if let Ok(mut reads) = GIT_READS.lock() {
        *reads.entry(root.to_path_buf()).or_default() += 1;
    }
}

#[cfg(test)]
fn git_reads(root: &Path) -> usize {
    GIT_READS
        .lock()
        .map(|reads| reads.get(root).copied().unwrap_or(0))
        .unwrap_or(0)
}

/// How long a caller waits for a read of one checkout that is already running.
///
/// A read is a handful of `git` processes, each under `GIT_READ_TIMEOUT`, so this is that read's own
/// worst case rather than a budget invented for the waiter. Past it the waiter reads the tree for
/// itself: that costs processes and is never wrong, while an unanswered IPC call is a row that
/// stops updating.
const GIT_READ_WAIT: Duration = Duration::from_secs(180);

impl GitWatcherManager {
    pub fn watch<R: tauri::Runtime>(
        &self,
        app: AppHandle<R>,
        repo_id: String,
        registration_id: String,
        plan: RepoWatchPlan,
    ) -> Result<(), IpcError> {
        let mut watchers = self
            .watchers
            .lock()
            .map_err(|error| IpcError::new(IpcErrorCode::OperationFailed, error.to_string()))?;
        // A repository that is already watched for the plan it was given has nothing to ask for.
        // A plan that grew is another matter: the worktree that just joined has a directory and a
        // Git directory nothing is watching yet, so its row would describe changes nobody reports.
        // Replacing the watcher is what covers it, and it costs a new watch rather than a second
        // watcher per repository.
        if watchers.get(&repo_id).is_some_and(|watched| {
            watched.plan == plan
                && watched.registration_id == registration_id
                && !watched.failed.load(Ordering::Acquire)
        }) {
            return Ok(());
        }

        let (queue, inbox) = WatchQueue::new();
        let event_queue = queue.clone();
        let callback_plan = plan.clone();
        let failed = Arc::new(AtomicBool::new(false));
        let callback_failed = failed.clone();
        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                forward_watch_result(result, &callback_plan, &event_queue, &callback_failed);
            })
            .map_err(|error| {
                IpcError::new(
                    IpcErrorCode::OperationFailed,
                    format!("could not watch repository: {error}"),
                )
            })?;
        for path in plan
            .roots
            .keys()
            .chain(plan.git_dirs.keys())
            .chain(plan.common_dir.iter())
        {
            watcher
                .watch(path, RecursiveMode::Recursive)
                .map_err(|error| {
                    IpcError::new(
                        IpcErrorCode::OperationFailed,
                        format!("could not watch repository: {error}"),
                    )
                })?;
        }

        let worker_snapshots = self.snapshots.clone();
        let worker_watchers = self.watchers.clone();
        let worker_token = Arc::new(());
        let token = worker_token.clone();
        let worker_failed = failed.clone();
        let worker_plan = plan.clone();
        let worker_registration_id = registration_id.clone();
        thread::Builder::new()
            .name("muster-git-watch".into())
            .spawn(move || {
                loop {
                    match receive_debounced_change(&inbox, WATCH_DEBOUNCE, WATCH_MAX_BATCH) {
                        WatchWakeup::Changed(update) => {
                            // Git is asked what the batch ignored before anything is invalidated,
                            // so one query answers for the whole burst rather than one per file,
                            // and the checkouts whose every moved path is ignored keep the
                            // snapshot they have. The activity still goes out either way: the file
                            // explorer has to show what was written.
                            let update = without_ignored_only(&worker_plan, update);
                            // Before the event, so a refresh it triggers never reads what the same
                            // change just invalidated.
                            worker_snapshots.mark_stale(&update.status);
                            if !update.status.is_empty() {
                                let _ = app.emit(STATUS_CHANGED_EVENT, &update.status);
                            }
                            if !update.activity.is_empty() {
                                let _ = app.emit(FILE_ACTIVITY_EVENT, &file_activity(&update));
                            }
                            if !update.worktrees.is_empty() {
                                // The rows themselves are not a status refresh, so the repository is named
                                // and its registration read again: a worktree created by an agent belongs
                                // in the sidebar the same way one created here does.
                                let _ = app.emit(WORKTREES_CHANGED_EVENT, &update.worktrees);
                            }
                        }
                        WatchWakeup::Failed(error) => {
                            let update = invalidate_failed_watch(&worker_snapshots, &worker_plan);
                            log::error!(
                                "Git watcher for repository {} failed: {error}",
                                worker_plan.repo_id
                            );
                            let _ = app.emit(STATUS_CHANGED_EVENT, &update.status);
                            let _ = app.emit(
                                WATCH_FAILED_EVENT,
                                watch_failure(&worker_plan, &worker_registration_id),
                            );
                            if let Ok(mut watchers) = worker_watchers.lock() {
                                if watchers
                                    .get(&worker_plan.repo_id)
                                    .is_some_and(|current| Arc::ptr_eq(&current.token, &token))
                                {
                                    watchers.remove(&worker_plan.repo_id);
                                }
                            }
                            break;
                        }
                        WatchWakeup::Stop => break,
                    }
                }
            })
            .map_err(|error| {
                IpcError::new(
                    IpcErrorCode::OperationFailed,
                    format!("could not start repository watcher: {error}"),
                )
            })?;

        // The replaced watcher is dropped with its sender, which is what ends the worker reading
        // the plan this one supersedes.
        if let Some(previous) = watchers.get(&repo_id) {
            invalidate_replaced_plan(&self.snapshots, &previous.plan, &plan);
        }
        watchers.insert(
            repo_id,
            RepoWatcher {
                _watcher: watcher,
                queue,
                failed: worker_failed,
                token: worker_token,
                registration_id,
                plan: plan.clone(),
            },
        );
        Ok(())
    }

    pub fn unwatch(&self, repo_id: &str, checkout_ids: &[String]) {
        let mut checkout_ids: BTreeSet<_> = checkout_ids.iter().cloned().collect();
        let retired = if let Ok(mut watchers) = self.watchers.lock() {
            match watchers.remove(repo_id) {
                Some(RepoWatcher { queue, plan, .. }) => {
                    checkout_ids.extend(plan.requested);
                    Some(queue)
                }
                None => None,
            }
        } else {
            None
        };
        // Sent with the map already unlocked, because `Stop` now waits for the worker to take it and
        // that worker locks this same map to take itself out when it fails. A send from inside the
        // lock would let those two wait for each other: the worker behind a `Failed` it already
        // answered, the retired watcher behind a `Stop` nobody could read.
        if let Some(queue) = retired {
            queue.stop();
        }
        self.snapshots
            .forget_many(&checkout_ids.into_iter().collect::<Vec<_>>());
    }

    /// What is already held for a checkout, or `None` when it must be read again. Every live
    /// Git checkout is watched, so a held entry is invalidated by whatever changed it.
    fn cached_state(
        &self,
        checkout_id: &str,
        default_branch: Option<&str>,
    ) -> Option<CachedGitSnapshot> {
        self.snapshots.fresh(checkout_id, default_branch)
    }

    fn store_state(&self, checkout_id: String, state: CachedGitSnapshot, revision: Option<u64>) {
        self.snapshots.insert_at(checkout_id, state, revision);
    }
}

/// Which checkouts a filesystem change speaks for.
///
/// A path inside a worktree, or inside the Git directory that belongs to that one worktree, is
/// that worktree's business: a file written or a file staged there moves nothing for anyone
/// else. A path in the Git directory the worktrees share is the repository's, because a commit
/// there moves the merge base every sibling counts its lines against and their numbers are now
/// describing a change set that no longer exists.
///
/// Anything outside all of them belongs to none of them, and a change that touches no checkout
/// is not a change at all: Git writes an object for every commit, and re-reading the sidebar
/// because a blob landed is the cost this whole path exists to avoid.
///
/// The one exception is a change in the list of worktrees themselves, which is not a change any
/// checkout can speak for: it is answered by naming the repository, and the workspace is read
/// again to find the worktree that joined or the one that left.
fn affected_checkouts(plan: &RepoWatchPlan, event: &notify::Event) -> Option<WatchUpdate> {
    // Reading a path is not a change to it, and on Linux it is reported as an event of its own
    // for every open of every watched file. Git commands read .git/HEAD before acting, so treating
    // those reads as ref writes would refresh the whole repository on every command. Only Linux
    // reports these read events.
    if matches!(event.kind, notify::EventKind::Access(_)) {
        return None;
    }
    let mut status: BTreeSet<String> = BTreeSet::new();
    let mut activity: BTreeSet<String> = BTreeSet::new();
    let mut worktrees: BTreeSet<String> = BTreeSet::new();
    let mut paths: BTreeMap<String, BTreeSet<PathBuf>> = BTreeMap::new();
    let mut pinned: BTreeSet<String> = BTreeSet::new();
    let mut repo_wide = false;
    for path in &event.paths {
        // Git writes a ref, the index and the configuration by creating `<name>.lock`, filling it
        // and renaming it over `<name>`. The lock is where the write begins, never what it leaves
        // behind: the rename that follows is the change, and a lock cleaned up afterwards
        // describes nothing. Skipping the path rather than the event is what keeps the rename,
        // which arrives as a second path in the same event or the next one, answering for the
        // change. Only Git's own metadata is filtered this way: a file in a checkout that happens
        // to be called `notes.lock` is a file the user edited.
        if is_git_lock_file(path) && is_git_metadata_path(plan, path) {
            continue;
        }
        // Check registration before a known worktree's Git directory: its `gitdir` file is
        // inside that directory, but removing it means the row's membership changed, not just
        // that checkout's status did.
        if let Some(common) = plan.common_dir.as_deref() {
            if path.starts_with(common) && is_worktree_registration_path(event, path, common) {
                worktrees.insert(plan.repo_id.clone());
                continue;
            }
        }
        if let Some(checkout_id) = longest_match(&plan.git_dirs, path) {
            status.insert(checkout_id.clone());
            pinned.insert(checkout_id);
            continue;
        }
        if let Some(common) = plan.common_dir.as_deref() {
            if path.starts_with(common) {
                // Git checks the new worktree's branch out into `<common>/worktrees/<name>/`, and
                // the index it writes there is that checkout's own bookkeeping. The plan cannot
                // attribute it to anyone until the registration names it, and reading the whole
                // repository over it is the cost this signal must not pay.
                if is_pending_worktree_git_dir(path, common) {
                    continue;
                }
                repo_wide |= should_refresh_path(path);
                continue;
            }
        }
        if let Some((root, checkout_id)) = longest_path_match(&plan.roots, path) {
            // Muster's default location is inside the repository root. Until the registration
            // event has brought a new worktree into the plan, its files would look like primary
            // checkout edits; ignore that reserved folder and let the shared Git metadata event
            // add its real root to the next watch plan.
            if is_pending_worktree_path(path, root) {
                continue;
            }
            status.insert(checkout_id.clone());
            if is_checkout_file_activity(path, root) {
                activity.insert(checkout_id.clone());
                // Relative to the checkout, because that is the form `check-ignore` takes and the
                // form every rule in it is written against.
                if let Ok(relative) = path.strip_prefix(root) {
                    paths
                        .entry(checkout_id.clone())
                        .or_default()
                        .insert(relative.to_path_buf());
                }
            }
        }
    }
    if repo_wide {
        status.extend(plan.all.iter().cloned());
        pinned.extend(plan.all.iter().cloned());
    }
    let nothing_moved = status.is_empty() && activity.is_empty() && worktrees.is_empty();
    (!nothing_moved).then(|| WatchUpdate {
        status: status.into_iter().collect(),
        activity: activity.into_iter().collect(),
        worktrees: worktrees.into_iter().collect(),
        paths,
        pinned,
        // One event carries what one event moved; only a budget can spend what a burst needs.
        spent: false,
    })
}

/// Whether a checkout's snapshot is still current after a batch of changes, as far as this batch
/// can tell.
///
/// Everything the batch ignored is only worth asking about once: a `pnpm install` and a build write
/// thousands of files, and re-reading the Git state to discover that none of them are part of the
/// change set is several `git` processes per checkout for an answer that is always the same. So the
/// paths of the batch are handed to Git, which is the authority on what it ignores -- the user's own
/// `.gitignore`, `.git/info/exclude` and `core.excludesFile`, plus a nested repository or a file a
/// negative rule brings back inside an ignored directory, none of which a list of directory names
/// here could account for.
///
/// A checkout is held back only when *every* path the batch moved inside it is one Git ignores and
/// nothing else in the batch spoke for it: one path that is tracked, or any write in a Git
/// directory, and the checkout is re-read. The activity signal is not held back with it, because
/// the file explorer has to show what was just written whether Git tracks it or not.
///
/// `None` from Git is read as "not ignored", so a question it could not answer invalidates. That
/// is the only safe direction: a stale row is a row describing a change set that no longer exists,
/// while a re-read that was not needed costs a few processes.
fn without_ignored_only(plan: &RepoWatchPlan, update: WatchUpdate) -> WatchUpdate {
    let WatchUpdate {
        mut status,
        activity,
        worktrees,
        paths,
        pinned,
        spent,
    } = update;
    if paths.is_empty() {
        return WatchUpdate {
            status,
            activity,
            worktrees,
            paths,
            pinned,
            spent,
        };
    }
    let mut ignored_only: BTreeSet<String> = BTreeSet::new();
    for (checkout_id, moved) in &paths {
        // A checkout the batch speaks for through its own Git directory, or through the one every
        // worktree shares, is re-read whatever its files are: an index or a merge base moved.
        if pinned.contains(checkout_id) || !status.contains(checkout_id) {
            continue;
        }
        let Some(root) = checkout_root(plan, checkout_id) else {
            continue;
        };
        let paths: Vec<_> = moved.iter().cloned().collect();
        if ignored_paths(root, &paths) == Some(paths.len()) {
            ignored_only.insert(checkout_id.clone());
        }
    }
    if ignored_only.is_empty() {
        return WatchUpdate {
            status,
            activity,
            worktrees,
            paths,
            pinned,
            spent,
        };
    }
    status.retain(|checkout_id| !ignored_only.contains(checkout_id));
    WatchUpdate {
        status,
        activity,
        worktrees,
        paths,
        pinned,
        spent,
    }
}

/// The directory a checkout is watched in, which is the directory its paths are relative to.
fn checkout_root<'a>(plan: &'a RepoWatchPlan, checkout_id: &str) -> Option<&'a Path> {
    plan.roots
        .iter()
        .find(|(_, id)| id.as_str() == checkout_id)
        .map(|(root, _)| root.as_path())
}

/// How many of a checkout's paths Git says it ignores, or `None` when it could not be asked.
///
/// The count rather than the set because the paths were handed to Git relative to the checkout and
/// come back the way they went in; a path Git does not name, or does not name in the form it went
/// in, is a path this cannot claim is ignored.
///
/// Exit 1 is Git's own answer for "none of these are ignored", not a failure, so it counts as zero
/// rather than as the question having gone unanswered. Anything else -- a Git that is not there, a
/// deadline, a fatal error over a path -- is `None`, which invalidates.
fn ignored_paths(root: &Path, paths: &[PathBuf]) -> Option<usize> {
    let output = run_git_on_paths(root, ["check-ignore", "--stdin", "-z"], paths).ok()?;
    if !matches!(output.status.code(), Some(0) | Some(1)) {
        return None;
    }
    Some(
        output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
            .count(),
    )
}

/// Whether a change in the shared Git directory is the list of worktrees moving.
///
/// Git registers a linked worktree as `<common>/worktrees/<name>`, and it does so whichever hand
/// added it: a worktree an agent created with `git worktree add` in a terminal is registered the
/// same way one Muster created itself is, so the same write is what the sidebar has to notice.
///
/// The registration is also where a checkout does its own bookkeeping, and that is the part to
/// refuse. A `git add` creates `index.lock` and rewrites `index`, a commit moves `HEAD`, and a
/// checkout a user is typing in does it several times a minute: none of that adds or removes a
/// worktree, and answering it with a read of the whole repository is the cost this signal must not
/// pay. Registration directories only count as they appear or go; within them only Git's own
/// pointers to a checkout count, not the files a worktree uses.
fn is_worktree_registration_path(event: &notify::Event, path: &Path, common_dir: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(common_dir) else {
        return false;
    };
    let mut components = relative.components();
    if !matches!(components.next(), Some(Component::Normal(name)) if name.to_str() == Some("worktrees"))
    {
        return false;
    }
    match (components.next(), components.next()) {
        // The directory holding the registrations itself.
        (None, _) => is_worktree_lifecycle_event(event),
        // The worktree's own Git directory, appearing or going.
        (Some(Component::Normal(_)), None) => is_worktree_lifecycle_event(event),
        // A file directly in it, one Git writes to register a worktree rather than to use one.
        (Some(Component::Normal(_)), Some(Component::Normal(file)))
            if matches!(file.to_str(), Some("gitdir" | "commondir")) =>
        {
            matches!(
                event.kind,
                notify::EventKind::Create(_)
                    | notify::EventKind::Modify(_)
                    | notify::EventKind::Remove(_)
            )
        }
        _ => false,
    }
}

fn is_worktree_lifecycle_event(event: &notify::Event) -> bool {
    matches!(
        event.kind,
        notify::EventKind::Create(_)
            | notify::EventKind::Remove(_)
            | notify::EventKind::Modify(notify::event::ModifyKind::Name(_))
    )
}

/// The checkout owning a directory the path sits in, when it sits in one of them. The longest
/// match wins, so a worktree nested under another is its own and not its parent.
fn longest_match(dirs: &BTreeMap<PathBuf, String>, path: &Path) -> Option<String> {
    longest_path_match(dirs, path).map(|(_, checkout_id)| checkout_id.clone())
}

fn longest_path_match<'a>(
    dirs: &'a BTreeMap<PathBuf, String>,
    path: &Path,
) -> Option<(&'a PathBuf, &'a String)> {
    dirs.iter()
        .filter(|(dir, _)| path.starts_with(dir))
        .max_by_key(|(dir, _)| dir.components().count())
}

fn is_pending_worktree_path(path: &Path, checkout_root: &Path) -> bool {
    path.strip_prefix(checkout_root)
        .ok()
        .and_then(|relative| relative.components().next())
        .is_some_and(|component| {
            matches!(component, Component::Normal(name) if name.to_str() == Some(".worktrees"))
        })
}

/// Whether a change in the shared Git directory belongs to a worktree the plan has not learned
/// about yet. Git writes that worktree's `HEAD`, `index` and `ORIG_HEAD` into
/// `<common>/worktrees/<name>/` while it checks the branch out, and none of it moves anything for
/// anyone else. A worktree the plan already knows is matched before this is asked, so what is left
/// is the window between the worktree landing on disk and the membership signal that adds it.
fn is_pending_worktree_git_dir(path: &Path, common_dir: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(common_dir) else {
        return false;
    };
    let mut components = relative.components();
    matches!(
        components.next(),
        Some(Component::Normal(name)) if name.to_str() == Some("worktrees")
    ) && components.next().is_some()
}

/// Whether a path is one of Git's lock files: the scratch name it writes a ref, the index or the
/// configuration under before renaming it into place.
fn is_git_lock_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".lock"))
}

/// Whether a path belongs to Git's own bookkeeping rather than to a checkout's files: the Git
/// directory of one linked worktree, or the directory they all share. A lock in either is a write
/// in flight, and that includes a lock under a ref in the shared directory, which is why this is
/// asked separately from the lock itself.
fn is_git_metadata_path(plan: &RepoWatchPlan, path: &Path) -> bool {
    plan.git_dirs.keys().any(|dir| path.starts_with(dir))
        || plan
            .common_dir
            .as_deref()
            .is_some_and(|common| path.starts_with(common))
}

impl GitSnapshotCache {
    fn revision(&self, checkout_id: &str) -> Option<u64> {
        let revisions = self.revisions.lock().ok()?;
        Some(*revisions.get(checkout_id).unwrap_or(&0))
    }

    fn fresh(&self, checkout_id: &str, default_branch: Option<&str>) -> Option<CachedGitSnapshot> {
        let revisions = self.revisions.lock().ok()?;
        let revision = revisions.get(checkout_id).copied().unwrap_or(0);
        self.entries
            .lock()
            .ok()?
            .get(checkout_id)
            .filter(|entry| {
                entry.revision == revision
                    && !entry.stale
                    && entry.default_branch.as_deref() == default_branch
            })
            .cloned()
    }

    #[cfg(test)]
    fn insert(&self, checkout_id: String, state: CachedGitSnapshot) {
        let revision = self.revision(&checkout_id);
        self.insert_at(checkout_id, state, revision);
    }

    fn insert_at(&self, checkout_id: String, state: CachedGitSnapshot, revision: Option<u64>) {
        let Some(revision) = revision else {
            return;
        };
        let Ok(revisions) = self.revisions.lock() else {
            return;
        };
        if revisions.get(&checkout_id).copied().unwrap_or(0) != revision {
            return;
        }
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        entries.insert(
            checkout_id,
            CachedGitSnapshot {
                revision,
                stale: false,
                ..state
            },
        );
    }

    /// Marks every checkout a change speaks for, so the refresh it triggers reads the tree
    /// rather than what it looked like a moment ago.
    fn mark_stale(&self, checkout_ids: &[String]) {
        let Ok(mut revisions) = self.revisions.lock() else {
            return;
        };
        for checkout_id in checkout_ids {
            let revision = revisions.entry(checkout_id.clone()).or_default();
            *revision = revision.wrapping_add(1);
        }
        if let Ok(mut entries) = self.entries.lock() {
            for checkout_id in checkout_ids {
                if let Some(entry) = entries.get_mut(checkout_id) {
                    entry.stale = true;
                }
            }
        }
    }

    fn forget_many(&self, checkout_ids: &[String]) {
        let Ok(mut revisions) = self.revisions.lock() else {
            return;
        };
        for checkout_id in checkout_ids {
            let revision = revisions.entry(checkout_id.clone()).or_default();
            *revision = revision.wrapping_add(1);
        }
        if let Ok(mut entries) = self.entries.lock() {
            for checkout_id in checkout_ids {
                entries.remove(checkout_id);
            }
        }
        // A read of a checkout nobody watches any more is not the read the next caller of that id
        // is asking about, and leaving it in the map would make them wait for it. The read itself is
        // left alone: it is another thread's work, and it still answers the callers waiting for it.
        if let Ok(mut reading_snapshots) = self.reading_snapshots.lock() {
            reading_snapshots.retain(|(checkout_id, _, _), _| !checkout_ids.contains(checkout_id));
        }
        if let Ok(mut reading_counts) = self.reading_counts.lock() {
            reading_counts.retain(|(checkout_id, _, _), _| !checkout_ids.contains(checkout_id));
        }
    }
}

fn invalidate_replaced_plan(
    snapshots: &GitSnapshotCache,
    previous: &RepoWatchPlan,
    next: &RepoWatchPlan,
) {
    if previous == next {
        return;
    }
    let affected: BTreeSet<_> = previous
        .requested
        .iter()
        .chain(&next.requested)
        .cloned()
        .collect();
    snapshots.mark_stale(&affected.into_iter().collect::<Vec<_>>());
}

/// Waits for the directory to go quiet, then names every checkout the quiet period touched.
///
/// A burst is one answer, so the checkouts are merged as they arrive: a save that rewrites a
/// file and its lock, or a commit that moves a ref and an index, are two filesystem events
/// describing one change, and answering them apart would refresh the same rows twice. The events
/// that made the burst were merged into one slot before they were announced (`WatchQueue`), so
/// what arrives per burst here is the answer and what arrives after it is only the tail of a burst
/// that began while this answer was already being built.
fn receive_debounced_change(
    inbox: &WatchInbox,
    debounce: Duration,
    max_batch: Duration,
) -> WatchWakeup {
    let merged = match inbox.recv() {
        // A whole budget behind the answer, not the spent one it was answered under: the slot is
        // rebuilt here, and a later, smaller update merged into it must not be the only path the
        // batch carries when the burst that began this one had more than it could.
        Ok(WatchWakeup::Changed(update)) => WatchSlot::holding(update),
        Ok(WatchWakeup::Failed(error)) => return WatchWakeup::Failed(error),
        Ok(WatchWakeup::Stop) | Err(_) => return WatchWakeup::Stop,
    };
    let started = Instant::now();
    receive_debounced_updates(
        merged,
        debounce,
        max_batch,
        || started.elapsed(),
        |timeout| inbox.recv_timeout(timeout),
    )
}

fn receive_debounced_updates(
    mut merged: WatchSlot,
    debounce: Duration,
    max_batch: Duration,
    mut elapsed: impl FnMut() -> Duration,
    mut receive: impl FnMut(Duration) -> Result<WatchWakeup, mpsc::RecvTimeoutError>,
) -> WatchWakeup {
    loop {
        let remaining = max_batch.saturating_sub(elapsed());
        if remaining.is_zero() {
            return WatchWakeup::Changed(merged.take());
        }
        match receive(debounce.min(remaining)) {
            Ok(WatchWakeup::Changed(update)) => merged.merge_update(update),
            Ok(WatchWakeup::Failed(error)) => return WatchWakeup::Failed(error),
            Ok(WatchWakeup::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return WatchWakeup::Stop;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => return WatchWakeup::Changed(merged.take()),
        }
    }
}

fn failed_watch_update(plan: &RepoWatchPlan) -> WatchUpdate {
    WatchUpdate {
        status: plan.requested.clone(),
        ..WatchUpdate::default()
    }
}

fn requested_watch_ids(plan: &RepoWatchPlan) -> Vec<String> {
    let mut requested = plan.requested.clone();
    requested.sort();
    requested
}

fn watch_failure(plan: &RepoWatchPlan, registration_id: &str) -> WatchFailure {
    WatchFailure {
        repo_id: plan.repo_id.clone(),
        checkout_ids: requested_watch_ids(plan),
        registration_id: registration_id.to_owned(),
    }
}

fn invalidate_failed_watch(snapshots: &GitSnapshotCache, plan: &RepoWatchPlan) -> WatchUpdate {
    let update = failed_watch_update(plan);
    snapshots.mark_stale(&update.status);
    update
}

fn forward_watch_result(
    result: notify::Result<notify::Event>,
    plan: &RepoWatchPlan,
    queue: &WatchQueue,
    failed: &AtomicBool,
) {
    match result {
        Ok(event) if !failed.load(Ordering::Acquire) => {
            if let Some(update) = affected_checkouts(plan, &event) {
                // The queue only has to wake the worker; which checkouts moved is merged into the
                // one slot behind that wakeup, and the worker merges the rest of a burst into one
                // answer.
                queue.changed(update);
            }
        }
        Err(error) if !failed.swap(true, Ordering::AcqRel) => {
            let message = error.to_string();
            if !queue.failed(message.clone()) {
                log::error!(
                    "Git watcher for repository {} failed: {message}",
                    plan.repo_id
                );
            }
        }
        _ => {}
    }
}

fn should_refresh_path(path: &Path) -> bool {
    let components: Vec<_> = path.components().collect();
    let Some(git_directory) = components
        .iter()
        .position(|component| matches!(component, Component::Normal(name) if *name == ".git"))
    else {
        return true;
    };
    let metadata = &components[git_directory + 1..];
    let ends_with =
        |name: &str| matches!(metadata.last(), Some(Component::Normal(value)) if *value == name);
    // A linked worktree's HEAD is private to that checkout, not a repo-wide ref. Known worktree
    // Git directories are matched before this helper; an unregistered one is not a reason to
    // refresh every existing checkout while the membership signal adds its row. The scratch name
    // a lock is written under is answered by the rename that follows it, which arrives separately
    // and does not match anything here: `HEAD.lock` is not a HEAD that moved.
    let head_changed = metadata.len() == 1 && ends_with("HEAD");
    let index_changed = ends_with("index");
    let packed_refs_changed = metadata.len() == 1 && ends_with("packed-refs");
    head_changed
        || index_changed
        || packed_refs_changed
        || metadata.windows(2).any(|pair| {
        matches!(pair[0], Component::Normal(name) if name == "refs")
            && matches!(pair[1], Component::Normal(name) if matches!(name.to_str(), Some("heads" | "remotes")))
        })
}

fn is_checkout_file_activity(path: &Path, checkout_root: &Path) -> bool {
    path.strip_prefix(checkout_root)
        .ok()
        .and_then(|relative| relative.components().next())
        .is_some_and(|component| !matches!(component, Component::Normal(name) if name == ".git"))
}

pub fn status(
    database: &Database,
    watchers: &GitWatcherManager,
    checkout_id: &str,
) -> Result<GitStatus, IpcError> {
    let context = registered_git_context(database, checkout_id)?;
    Ok(snapshot_of(&context, watchers)?.status)
}

/// The counts of one checkout's changed files, from the same snapshot the file list is built
/// from, so the numbers on a row and the row's own base ref cannot disagree.
pub fn diff_stats(
    database: &Database,
    watchers: &GitWatcherManager,
    checkout_id: &str,
) -> Result<GitFileDiffStats, IpcError> {
    let context = registered_git_context(database, checkout_id)?;
    Ok(counts_of(&context, watchers)?.files)
}

/// The same counts folded into one number per checkout, for the rows that name every checkout
/// at once. A checkout Git cannot answer for is left out rather than reported as zero, so a
/// missing, plain or base-less checkout shows no counts instead of a false clean bill.
///
/// Every checkout the sidebar names is watched, so all but the ones a change just spoke for are
/// answered from what was already read. The rest are read side by side rather than one after
/// another: on a commit, a new worktree or the first paint, that is the whole sidebar at once,
/// and each reading is several `git` processes of its own.
pub fn checkout_diff_stats(
    database: &Database,
    watchers: &GitWatcherManager,
) -> Result<GitCheckoutDiffStats, IpcError> {
    let repos = workspace_repos(database)?;
    let mut totals = GitCheckoutDiffStats::new();
    let mut unread: Vec<GitContext> = Vec::new();
    for repo in &repos {
        if repo.kind != RepoKind::Git {
            continue;
        }
        for checkout in &repo.checkouts {
            if checkout.is_missing {
                continue;
            }
            let Ok(context) = git_context(&repos, &checkout.id) else {
                continue;
            };
            match held_state(&context, watchers).and_then(|held| held.counts) {
                Some(counts) => {
                    totals.insert(checkout.id.clone(), counts.totals);
                }
                None => unread.push(context),
            }
        }
    }
    for batch in unread.chunks(PARALLEL_READS) {
        for (checkout_id, counts) in thread::scope(|scope| {
            let reads: Vec<_> = batch
                .iter()
                .map(|context| {
                    let watchers = &*watchers;
                    scope.spawn(move || {
                        counts_of(context, watchers)
                            .ok()
                            .map(|counts| (context.checkout.id.clone(), counts.totals))
                    })
                })
                .collect();
            reads
                .into_iter()
                .map(|read| read.join().unwrap_or(None))
                .collect::<Vec<_>>()
        })
        .into_iter()
        .flatten()
        {
            totals.insert(checkout_id, counts);
        }
    }
    Ok(totals)
}

/// What the checkout was when it was last read, if a watcher has not said anything moved since.
///
/// The base ref is part of the entry rather than of the key alone, because choosing another
/// default branch moves it and no filesystem event describes that.
fn held_state(context: &GitContext, watchers: &GitWatcherManager) -> Option<CachedGitSnapshot> {
    watchers.cached_state(&context.checkout.id, context.repo.default_branch.as_deref())
}

/// Reads the checkout and remembers it, so the next question about it is a lookup rather than
/// another walk of the same working tree.
///
/// The sidebar and the Changes tab ask about the same checkout at the same time, and each read is a
/// burst of `git` processes over the same working tree, so a caller that arrives while the read is
/// running waits for that answer rather than asking Git the same questions twice. The revision is
/// taken before the read starts, which is what keeps a refresh that a change overtook from writing
/// its answer back over the newer one.
fn snapshot_of(context: &GitContext, watchers: &GitWatcherManager) -> Result<Snapshot, IpcError> {
    let revision = watchers.snapshots.revision(&context.checkout.id);
    let default_branch = context.repo.default_branch.clone();
    if let Some(held) = held_state(context, watchers) {
        return Ok(held.snapshot);
    }
    let read_key = (
        context.checkout.id.clone(),
        default_branch.clone(),
        revision,
    );
    shared_read(&watchers.snapshots.reading_snapshots, &read_key, || {
        let read = snapshot(context)?;
        watchers.store_state(
            context.checkout.id.clone(),
            CachedGitSnapshot {
                default_branch,
                revision: 0,
                stale: false,
                snapshot: read.clone(),
                counts: None,
            },
            revision,
        );
        Ok(read)
    })
}

/// The line counts, from the same snapshot the file list is served from, so a number and the
/// row it decorates can never describe two change sets.
fn counts_of(context: &GitContext, watchers: &GitWatcherManager) -> Result<GitCounts, IpcError> {
    let revision = watchers.snapshots.revision(&context.checkout.id);
    let default_branch = context.repo.default_branch.clone();
    if let Some(counts) = held_state(context, watchers).and_then(|held| held.counts) {
        return Ok(counts);
    }
    let read_key = (
        context.checkout.id.clone(),
        default_branch.clone(),
        revision,
    );
    shared_read(&watchers.snapshots.reading_counts, &read_key, || {
        // The file list is the expensive part and it is held; only the lines are missing. When
        // it is not held either, this is the same read `snapshot_of` shares, so two callers
        // counting one checkout are counting one change set rather than one each.
        let read = match held_state(context, watchers) {
            Some(held) => held.snapshot,
            None => snapshot_of(context, watchers)?,
        };
        let counts = counted_files(context, &read)?;
        watchers.store_state(
            context.checkout.id.clone(),
            CachedGitSnapshot {
                default_branch,
                revision: 0,
                stale: false,
                snapshot: read,
                counts: Some(counts.clone()),
            },
            revision,
        );
        Ok(counts)
    })
}

fn workspace_repos(database: &Database) -> Result<Vec<Repo>, IpcError> {
    database
        .load_workspace()
        .map(|workspace| workspace.repos)
        .map_err(|error| IpcError::new(IpcErrorCode::OperationFailed, error))
}

/// The snapshot's own file list with the lines each file adds and removes attached, and their
/// sum. Both come from the same snapshot the file list is served from, so a number and the row
/// it decorates are always about the same base ref.
fn counted_files(context: &GitContext, snapshot: &Snapshot) -> Result<GitCounts, IpcError> {
    let counted = numstat_counts(context, snapshot)?;
    let mut totals = GitDiffStats::default();
    let files = snapshot
        .status
        .files
        .iter()
        .map(|file| {
            let counts = counted.get(&file.path).copied();
            if let Some(counts) = counts {
                totals.additions += counts.additions;
                totals.deletions += counts.deletions;
            }
            GitChangedFile {
                additions: counts.map(|counts| counts.additions),
                deletions: counts.map(|counts| counts.deletions),
                ..file.clone()
            }
        })
        .collect();
    Ok(GitCounts { files, totals })
}

/// Counts the lines every changed file adds and removes, against exactly the base the file
/// list uses: `snapshot.merge_base` for the tracked files, which is the ref `diff_args` diffs a
/// tracked file against, and `/dev/null` for the untracked ones, which is how it diffs those.
fn numstat_counts(
    context: &GitContext,
    snapshot: &Snapshot,
) -> Result<BTreeMap<String, GitDiffStats>, IpcError> {
    let tracked = checked_git(
        &context.root,
        [
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--numstat",
            "-z",
            "--find-renames",
            &snapshot.merge_base,
        ],
        "could not count changed lines",
    )?;
    let mut counts = parse_numstat(&tracked.stdout)?;
    for file in snapshot
        .status
        .files
        .iter()
        .filter(|file| file.status == "??")
    {
        // A path that leaves the checkout is not one of its files, whatever is at the other
        // end of the symlink, so it is left out of the counts the same way it is left out of
        // the diff.
        let Ok(path) = contained_untracked_path(context, &file.path) else {
            continue;
        };
        if let Some(untracked) = untracked_line_count(&path) {
            counts.insert(file.path.clone(), untracked);
        }
    }
    Ok(counts)
}

/// The lines an untracked file adds, counted the way `git diff --numstat` would count them.
///
/// An untracked file is a whole file Git has never seen, so every one of its lines is an
/// addition and there is nothing to remove. That makes the count the number of lines in the
/// file, which is what Git reports, and it is why this reads the file rather than asking Git to
/// diff it against nothing: the answer needs one read, and `git diff --no-index` needs one
/// process per untracked file. A worktree an agent has been working in holds a handful of
/// them, and every sidebar refresh pays for each.
///
/// A file Git declines to count has no number at all rather than a zero: a binary file is one
/// of those, and a row showing `+0` for it would be a claim about a file Git never measured.
/// The test below is the parity that keeps this honest.
///
/// The file is read a block at a time and never whole, because what is being decided here can be
/// decided from the head: whether Git would call it binary, and if it would not, how many lines
/// it has. Holding the file to decide that put the largest untracked file in the checkout into
/// memory on every sidebar refresh, only to discard all of it as binary.
fn untracked_line_count(path: &Path) -> Option<GitDiffStats> {
    let mut file = fs::File::open(path).ok()?;
    let mut block = vec![0u8; COUNT_BLOCK_BYTES];
    let mut newlines = 0u64;
    let mut seen = false;
    let mut last = 0u8;
    loop {
        let read = file.read(&mut block).ok()?;
        if read == 0 {
            break;
        }
        let bytes = &block[..read];
        if !seen {
            // Git's rule is written over a window and not over the file, so this judges the same
            // window it does: a NUL byte inside it and nothing else makes the file binary, and a
            // file that only reads as binary after it is text, which is the answer Git gives. The
            // first block covers the whole window because the block is the larger of the two.
            if is_binary(bytes) {
                return None;
            }
            seen = true;
        }
        newlines += bytes.iter().filter(|byte| **byte == b'\n').count() as u64;
        // The last byte of the file is the last byte of the block it ended in, and a file that
        // ends exactly where a block ends ends in a full one rather than in no block at all.
        last = bytes[read - 1];
    }
    // A file whose last line has no newline of its own is still a line, which is why Git
    // reports one more for `a` and one for `a\nb` alike.
    let trailing = seen && last != b'\n';
    Some(GitDiffStats {
        additions: newlines + u64::from(trailing),
        deletions: 0,
    })
}

/// Whether Git would call this file binary, by the same rule it uses: a NUL byte anywhere in
/// the first block it looks at. A file that reads as binary after that block is counted as
/// text, which is the same answer Git gives.
fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(BINARY_SNIFF_BYTES).any(|byte| *byte == 0)
}

/// Every `--numstat` record is one NUL-terminated field, so a path that holds a newline, a
/// tab or a NUL of its own cannot be split apart: a plain change is `adds<tab>deletes<tab>path`
/// and a rename or copy is `adds<tab>deletes<tab>` followed by the old and the new path as two
/// more fields. The counts are keyed by the new path, the one the file list shows, and a
/// record Git declined to count is left out instead of being reported as zero lines.
fn parse_numstat(output: &[u8]) -> Result<BTreeMap<String, GitDiffStats>, IpcError> {
    let mut fields = output.split(|byte| *byte == 0);
    let mut stats = BTreeMap::new();
    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        let (additions, deletions, path) = numstat_columns(record).ok_or_else(malformed_numstat)?;
        let path = if path.is_empty() {
            // A rename or copy: the counts came first, then the two paths.
            fields.next().ok_or_else(malformed_numstat)?;
            fields.next().ok_or_else(malformed_numstat)?
        } else {
            path
        };
        if let (Some(additions), Some(deletions)) = (additions, deletions) {
            stats.insert(
                path_text(path)?,
                GitDiffStats {
                    additions,
                    deletions,
                },
            );
        }
    }
    Ok(stats)
}

/// Splits one `--numstat` record into its two counts and its path, or `None` when the bytes
/// are not a record at all. A `-` column is Git declining to count the file, which is not a
/// zero, and is carried out as a `None` of its own.
fn numstat_columns(record: &[u8]) -> Option<(Option<u64>, Option<u64>, &[u8])> {
    let mut columns = record.splitn(3, |byte| *byte == b'\t');
    let additions = numstat_count(columns.next()?)?;
    let deletions = numstat_count(columns.next()?)?;
    Some((additions, deletions, columns.next()?))
}

fn numstat_count(column: &[u8]) -> Option<Option<u64>> {
    let text = std::str::from_utf8(column).ok()?;
    if text == "-" {
        return Some(None);
    }
    Some(Some(text.parse().ok()?))
}

fn malformed_numstat() -> IpcError {
    IpcError::new(
        IpcErrorCode::GitFailed,
        "Git returned an invalid line count record",
    )
}

/// An untracked entry is the one path the counts read straight from the working tree, so it
/// goes through the same containment as every other path a command touches: relative to the
/// checkout, resolving to a file inside it. A link that leaves the checkout, or that points at
/// a folder, is refused rather than followed.
fn contained_untracked_path(context: &GitContext, path: &str) -> Result<PathBuf, IpcError> {
    let target = context.root.join(validate_relative_path(path)?);
    let resolved = fs::canonicalize(&target).map_err(|error| {
        IpcError::new(
            IpcErrorCode::FolderMissing,
            format!("untracked file is no longer available: {error}"),
        )
    })?;
    if !resolved.starts_with(&context.root) || !resolved.is_file() {
        return Err(IpcError::new(
            IpcErrorCode::PathOutsideCheckout,
            "untracked path does not resolve to a file inside the checkout",
        ));
    }
    Ok(target)
}

pub fn diff(database: &Database, checkout_id: &str, path: &str) -> Result<GitFileDiff, IpcError> {
    let context = registered_git_context(database, checkout_id)?;
    let snapshot = snapshot(&context)?;
    let changed_file = changed_file(&snapshot.status, path)?;
    let relative_path = validate_relative_path(path)?;
    validate_diff_target(&context, &relative_path)?;

    let working_path = context.root.join(&relative_path);
    if fs::symlink_metadata(&working_path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        let target = fs::read_link(&working_path).map_err(|error| {
            IpcError::new(
                IpcErrorCode::GitFailed,
                format!("could not read symlink target: {error}"),
            )
        })?;
        let target = target.to_string_lossy().into_owned();
        // Hash of what is drawn here rather than of the checkout, for the reason `DiffRevision`
        // gives: a symlink diff carries no patch, so a target that was repointed would otherwise
        // answer with the same identity as the one on screen and the view would never move.
        let mut revision = DiffRevision::default();
        revision.read(target.as_bytes());
        return Ok(GitFileDiff {
            path: path.to_owned(),
            patch: String::new(),
            revision: revision.finish(),
            old_content: None,
            new_content: None,
            is_binary: false,
            large: false,
            too_large: false,
            total_lines: 0,
            hunks: Vec::new(),
            symlink_target: Some(target),
        });
    }

    let scan = scan_git_diff(
        "git",
        &context.root,
        diff_args(&context, &snapshot, &changed_file, path)?,
        None,
        GIT_READ_TIMEOUT,
    )?;
    ensure_diff_succeeded(&scan.output, changed_file.status == "??", scan.too_large)?;
    let patch = if scan.large || scan.too_large || scan.is_binary {
        String::new()
    } else {
        String::from_utf8_lossy(&scan.patch).into_owned()
    };
    // Only for a diff that is drawn: a patch nothing shows is not worth two more reads of the file.
    // Each side is the text `diff_args` diffed, read the way it was diffed (the merge base's blob
    // and the file as it is on disk) rather than the head's or the index's, which the patch of this
    // diff never saw.
    let (old_content, new_content) = if patch.is_empty() {
        (None, None)
    } else {
        (
            merge_base_content(
                &context.root,
                &snapshot.merge_base,
                changed_file.old_path.as_deref().unwrap_or(path),
            ),
            worktree_content(&working_path),
        )
    };
    Ok(GitFileDiff {
        path: path.to_owned(),
        patch,
        revision: scan.revision,
        old_content,
        new_content,
        is_binary: scan.is_binary,
        large: scan.large,
        too_large: scan.too_large,
        total_lines: scan.total_lines,
        hunks: scan.hunks,
        symlink_target: None,
    })
}

/// The text of the file as it is on disk, or nothing when there is none to read within the cap.
///
/// The path is one `validate_diff_target` has resolved inside the checkout and that has already been
/// turned away if it is a symlink, so this reads a file the checkout owns. `O_NOFOLLOW` is what keeps
/// that true by the time the file is actually opened: a checkout is writable by whatever is editing
/// it, and between the check above and the open below the path could have become a link out of the
/// checkout, which is the one thing `validate_diff_target` exists to prevent. `is_file` on the open
/// handle is then a statement about the file that was read rather than about the path it was reached
/// by, which is what keeps this from being a read of a directory, or of a pipe that would never end.
/// Bytes that are not text are not carried at all, where the patch carries them lossy: text the two
/// cannot be compared line for line is text no grammar could be handed.
fn worktree_content(path: &Path) -> Option<String> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_SYNTAX_CONTEXT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_SYNTAX_CONTEXT_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// The text of the blob `spec` names, or nothing when there is none to read within the cap.
///
/// Read through a pipe under a cap rather than through `run_git`, which buffers all of whatever Git
/// prints: a blob is the size of a file, and this one is read to color a diff rather than to show
/// it. A merge base with no such path (an added file, an untracked one) fails here, which is the
/// same answer as an old side with no lines to draw; the exit status is what says so, because a
/// failure prints nothing on the pipe it would have been read from. A Git that never finishes is
/// that same answer once more: there is no text to draw from a read that never came back, and the
/// deadline is what makes it come back.
fn merge_base_content(root: &Path, merge_base: &str, path: &str) -> Option<String> {
    let blob = format!("{merge_base}:{path}");
    let bytes = run_capped_git(
        "git",
        root,
        ["cat-file", "blob", &blob],
        MAX_SYNTAX_CONTEXT_BYTES,
        GIT_READ_TIMEOUT,
    )?;
    String::from_utf8(bytes).ok()
}

/// Whether a Git process has finished, asked in a way that leaves it where it is.
///
/// `Child::try_wait` answers by reaping, and a reaped pid is a number the kernel is free to hand to
/// the next process that asks for one: from that moment `kill(-pid, ...)` is aimed at whichever
/// group answers to that number, not at Git's. `waitid` with `WNOWAIT` reports the same exit and
/// leaves the child waitable, so the number stays reserved -- and with it the identity of the group
/// `stop_git_read` has to end -- for as long as the read has not decided what to do with that group.
/// `waitpid` has no `WNOWAIT` on macOS.
fn exited_unreaped(child: &Child) -> io::Result<bool> {
    // A test can break the one status read this has to survive, and only the one that would report
    // an exit: the reads before it are ordinary polls, and breaking those would test nothing.
    #[cfg(test)]
    if BROKEN_STATUS_READ.with(Cell::get) == 1 && exited_unreaped_unbroken(child)? {
        BROKEN_STATUS_READ.with(|broken| broken.set(2));
        return Err(io::Error::other("the process status could not be read"));
    }
    exited_unreaped_unbroken(child)
}

fn exited_unreaped_unbroken(child: &Child) -> io::Result<bool> {
    // Zeroed rather than uninitialized, because a kernel that has nothing to report leaves the
    // structure as it found it and this reads it either way: what is in there before a report would
    // be whatever was on the stack, and a watch that read it as an exit would end a healthy read.
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    let found = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
        )
    };
    if found != 0 {
        return Err(io::Error::last_os_error());
    }
    // A report names the child it is about, and no report is a success that carries no process:
    // `WNOHANG` asking too early says the child is still running, and saying that as an exit would
    // end a read that was only just getting started.
    // SAFETY: `waitid` wrote the report into `info`, or wrote nothing and left the structure
    // zeroed, which is the same initialised structure either way.
    let info = unsafe { info.assume_init_ref() };
    Ok(crate::terminal::process::wait_status_pid(info) == child.id() as libc::pid_t)
}

/// Runs one Git under a deadline and reads what it printed through a pipe under a cap.
///
/// `run_command_with_timeout`'s twin and not `run_command_with_timeout` itself, because that one
/// keeps `GIT_OUTPUT_CAP` and refuses what came past it, and this reading exists precisely not to
/// refuse: a blob is the size of a file and is read to color a diff, so the cap is the whole
/// difference between a ceiling of `cap` bytes and an allocation the size of the file, and a blob
/// that happens to be larger than a checkout's whole change list is an ordinary thing to be asking
/// for. Everything else is shared on purpose -- the process group of its own, `stop_git_read` to end
/// what hung, the same poll and the same deadline -- so a blob that will not finish is ended by the
/// same machinery and for the same reasons as every other read, and a `spawn_blocking` slot does not
/// stay taken by it.
///
/// Past the cap the answer is nothing rather than a prefix, and the Git is ended as soon as the
/// reading knows that: the reading keeps draining rather than stopping, so a blob larger than the cap
/// does not block its writer on a pipe nobody is finishing, and what ends it is the cap being passed
/// instead of the deadline being reached.
fn run_capped_git<const N: usize>(
    program: &str,
    root: &Path,
    args: [&str; N],
    cap: usize,
    timeout: Duration,
) -> Option<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        // Nothing is read of Git's own account of a failure, so its pipe is never the reason a read
        // stops: a blob past the cap ends the read and Git's complaint has nowhere to go.
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        // Its own group, so what hung is ended whole rather than only the process `spawn`
        // returned: what hangs is usually not Git but something it started, and killing that alone
        // would leave it holding the write end the reading collects from.
        .process_group(0)
        .spawn()
        .ok()?;
    // `cap + 1` bytes and not `cap`, because the byte past the cap is what says the blob is past
    // it, and a reading that stopped at exactly the cap could not tell a blob of exactly the cap
    // from a bigger one. On a thread of its own, as everywhere else here: a read on a pipe is the
    // one wait with no deadline left to time it out with. A reader that cannot be started leaves a
    // Git writing into a pipe nobody will ever read, so the child is ended before giving up.
    let over_cap = Arc::new(AtomicBool::new(false));
    let pipe = match drain_pipe(
        child.stdout.take().expect("piped stdout"),
        "muster-git-stdout",
        cap + 1,
        over_cap.clone(),
    ) {
        Ok(pipe) => pipe,
        Err(_) => {
            stop_git_read(&mut child, GroupIdentity::Reserved);
            return None;
        }
    };
    let deadline = Instant::now() + timeout;
    let mut poll = GIT_READ_POLL_MIN;
    // The answer is held rather than taken straight off the channel: a Git that has closed its pipe
    // and is a moment from exiting would otherwise have its blob dropped on the floor for asking
    // about the status too early, and the one channel left to ask about it again says there is
    // nothing coming.
    let mut read: Option<Option<PipeOutput>> = None;
    // Watched and not collected, for the reason `exited_unreaped` gives: the status is taken when
    // the read has decided what to do with the group, not while it is still deciding.
    let mut exited = false;
    loop {
        // A blob past the cap is past it: nothing about the rest of what Git prints puts it back
        // under, so the reading stops caring about it here. What the reading does not do is stop --
        // it carries on draining so that Git is not left blocked on a pipe nobody finishes -- and
        // this is what ends it instead, which the deadline used to have to do.
        if over_cap.load(Ordering::Acquire) {
            stop_git_read(&mut child, GroupIdentity::Reserved);
            return None;
        }
        if read.is_none() {
            match pipe.try_recv() {
                Ok(answer) => read = Some(answer.ok()),
                // The reader is gone without an answer, so there is nothing left to wait for.
                Err(mpsc::TryRecvError::Disconnected) => read = Some(None),
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if !exited {
            match exited_unreaped(&child) {
                Ok(answer) => exited = answer,
                // It cannot be asked again, so it is ended rather than waited for.
                Err(_) => {
                    stop_git_read(&mut child, GroupIdentity::Reserved);
                    return None;
                }
            }
        }
        // Both halves are waited for, because neither answers alone: the pipe says the reading is
        // over, and the status says whether what came of it is text. A Git that has closed its pipe
        // and is a moment from exiting is the ordinary case here, and the blob is not taken on the
        // pipe's word alone.
        if exited && matches!(read, Some(Some(_))) {
            // What came of it is not text to draw in two ways: the exit status says the Git failed,
            // and the reading above already refused a blob past the cap.
            let blob = read.take().flatten().expect("the blob is in hand");
            let Ok(status) = child.wait() else {
                // Nothing else reaps a child, so a collection that could not answer is ended the
                // same way every other exit is rather than left as a zombie.
                let group = reserved_group(&child);
                stop_git_read(&mut child, group);
                return None;
            };
            return (status.success() && !blob.over_cap && blob.bytes.len() <= cap)
                .then_some(blob.bytes);
        }
        // A pipe that broke mid-blob leaves what arrived short of the whole thing, and a reader that
        // died leaves nothing at all: either way there is no blob here, and the writer is in the way
        // of nothing.
        if let Some(None) = read {
            stop_git_read(&mut child, GroupIdentity::Reserved);
            return None;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            stop_git_read(&mut child, GroupIdentity::Reserved);
            return None;
        }
        thread::sleep(poll.min(remaining));
        poll = (poll * 2).min(GIT_READ_POLL_MAX);
    }
}

pub fn diff_page(
    database: &Database,
    checkout_id: &str,
    path: &str,
    offset: usize,
    limit: usize,
) -> Result<GitDiffPage, IpcError> {
    if limit == 0 || limit > MAX_DIFF_PAGE_LINES || offset > MAX_DIFF_LINES {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "diff page range is outside the allowed limit",
        ));
    }
    let context = registered_git_context(database, checkout_id)?;
    let snapshot = snapshot(&context)?;
    let changed_file = changed_file(&snapshot.status, path)?;
    let relative_path = validate_relative_path(path)?;
    validate_diff_target(&context, &relative_path)?;
    if fs::symlink_metadata(context.root.join(&relative_path))
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "symlink diffs do not have text pages",
        ));
    }
    let scan = scan_git_diff(
        "git",
        &context.root,
        diff_args(&context, &snapshot, &changed_file, path)?,
        Some((offset, limit)),
        GIT_READ_TIMEOUT,
    )?;
    ensure_diff_succeeded(&scan.output, changed_file.status == "??", scan.too_large)?;
    if scan.is_binary || scan.too_large {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "this diff does not have accessible text pages",
        ));
    }
    Ok(GitDiffPage {
        path: path.to_owned(),
        start_line: offset,
        total_lines: scan.total_lines,
        lines: scan.page,
    })
}

pub fn viewed_files(database: &Database, checkout_id: &str) -> Result<Vec<String>, IpcError> {
    registered_git_context(database, checkout_id)?;
    database
        .viewed_files(checkout_id)
        .map_err(|error| IpcError::new(IpcErrorCode::OperationFailed, error))
}

pub fn mark_viewed(database: &Database, checkout_id: &str, path: &str) -> Result<(), IpcError> {
    let context = registered_git_context(database, checkout_id)?;
    let snapshot = snapshot(&context)?;
    changed_file(&snapshot.status, path)?;
    let relative_path = validate_relative_path(path)?;
    validate_diff_target(&context, &relative_path)?;
    database
        .mark_file_viewed(checkout_id, path)
        .map_err(|error| IpcError::new(IpcErrorCode::OperationFailed, error))
}

fn changed_file(status: &GitStatus, path: &str) -> Result<GitChangedFile, IpcError> {
    status
        .files
        .iter()
        .find(|file| file.path == path)
        .cloned()
        .ok_or_else(|| {
            IpcError::new(
                IpcErrorCode::InvalidPath,
                "requested path is not a changed file in this checkout",
            )
        })
}

fn diff_args(
    context: &GitContext,
    snapshot: &Snapshot,
    changed_file: &GitChangedFile,
    path: &str,
) -> Result<Vec<OsString>, IpcError> {
    if changed_file.status == "??" {
        let relative_path = validate_relative_path(path)?;
        Ok(vec![
            "diff".into(),
            "--no-index".into(),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--no-color".into(),
            "--unified=3".into(),
            "--".into(),
            "/dev/null".into(),
            context.root.join(relative_path).into_os_string(),
        ])
    } else {
        let mut args = vec![
            "diff".into(),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--no-color".into(),
            "--find-renames".into(),
            "--unified=3".into(),
            snapshot.merge_base.clone().into(),
            "--".into(),
        ];
        if let Some(old_path) = &changed_file.old_path {
            args.push(literal_pathspec(old_path));
        }
        args.push(literal_pathspec(path));
        Ok(args)
    }
}

#[derive(Debug)]
struct ScannedDiff {
    output: Output,
    patch: Vec<u8>,
    revision: String,
    page: Vec<GitDiffPageLine>,
    hunks: Vec<GitDiffHunk>,
    total_lines: usize,
    is_binary: bool,
    large: bool,
    too_large: bool,
}

/// What the diff of one file is of, hashed from the lines the scan read.
///
/// Derived from the lines rather than from what is kept of them, because `patch` is what a large
/// diff throws away and it is also what two large diffs both answer with: an empty string. A view
/// that asks "is this the diff I am already showing" on the patch alone cannot tell two large diffs
/// apart, so it keeps serving the pages of the first one and the file on screen stops moving.
///
/// Every consumed line is hashed, including the ones whose bytes went into `patch.clear()`: those
/// are the lines the view is drawn from, so an identity that stopped looking at them would be blind
/// to exactly the content that is on screen.
///
/// A `DefaultHasher` and not a digest, because this answers one question inside one running app --
/// "is this the same diff as the one on screen" -- and is never persisted, never compared against
/// anything written by another build, and never sent anywhere a caller could not recompute.
#[derive(Default)]
struct DiffRevision(DefaultHasher);

impl DiffRevision {
    fn read(&mut self, bytes: &[u8]) {
        bytes.hash(&mut self.0);
    }

    fn finish(self) -> String {
        format!("{:016x}", self.0.finish())
    }
}

/// Reads one Git diff under a deadline, ends it as a whole group if it will not finish.
///
/// The parsing is on a thread of its own and the child stays here, which is the split every other
/// runner in this file makes and for the reason `spawn_and_collect` gives: a read on a pipe is the
/// one wait here with no deadline left to time it out with. So the scan streams and this thread
/// polls -- on the answer from the scan, on the exit of the process, and on `GIT_READ_TIMEOUT` --
/// and is the only one that may end it, because it is the only one holding a `Child` whose number
/// the kernel has not given away.
///
/// stderr is drained on a thread of its own, and that is not tidiness. Git is given its own process
/// group, so a diff filter it started cannot be waited for and cannot be pointed at; the two pipes
/// have to be read at once or a Git that fills the buffer of one blocks on writing it while this
/// thread blocks reading the other, and the pair of them waits on each other with nothing to break
/// it. The scan cannot be moved onto `spawn_and_collect` instead, because it stops early on
/// `MAX_DIFF_BYTES`, `MAX_DIFF_LINES`, `MAX_DIFF_HUNKS` and an oversized hunk header and builds
/// what it hands back as it goes; a whole diff collected first would be a whole diff held in memory,
/// which is the one thing those caps exist to prevent.
///
/// Nothing here compares Git's own wording, so nothing pins its locale: what Git said about a
/// failure is quoted, not looked for, and `ensure_diff_succeeded` reads the status and the bytes
/// rather than a sentence.
///
/// `program` and `timeout` are what `run_capped_git` takes as well, and for the same reason: a test
/// needs to put a shell where Git is and a deadline short enough to run inside one.
fn scan_git_diff(
    program: &str,
    root: &Path,
    args: Vec<OsString>,
    page_range: Option<(usize, usize)>,
    timeout: Duration,
) -> Result<ScannedDiff, IpcError> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Its own group, so what hung is ended whole rather than only the process `spawn`
        // returned: a diff filter is what blocks a `diff` most of the time, and it is never the
        // process named here.
        .process_group(0)
        .spawn()
        .map_err(|error| {
            IpcError::new(
                IpcErrorCode::GitFailed,
                format!("could not start Git: {error}"),
            )
        })?;
    let stdout = child.stdout.take().expect("piped stdout");
    // The scan is a thread and not an inlined loop because `read_diff_line` blocks with nothing left
    // to time it out with, and it ends through a channel because a join cannot be waited on for a
    // deadline either. A thread that could not be started leaves a Git writing into a pipe nobody
    // will ever read, so the child is ended before the failure is handed back.
    let (scanned, scans) = mpsc::sync_channel(1);
    if let Err(error) = start_read_thread("muster-git-diff", move || {
        let _ = scanned.send(scan_diff_lines(stdout, page_range));
    }) {
        stop_git_read(&mut child, GroupIdentity::Reserved);
        return Err(IpcError::new(
            IpcErrorCode::GitFailed,
            format!("could not read Git diff: {error}"),
        ));
    }
    // What Git says about a failure is the diagnosis a caller is handed, so it is kept under
    // `GIT_DIAGNOSTIC_CAP` like everywhere else. The answer here is the diff rather than what Git
    // printed, so there is no refusal to make at the cap: the reader carries on and drops, which is
    // what keeps a chatty diff off the pipe nobody would finish reading.
    let stderr = match PipeReader::take(
        child.stderr.take().expect("piped stderr"),
        "stderr",
        "muster-git-diff-stderr",
        GIT_DIAGNOSTIC_CAP,
    ) {
        Ok(pipe) => pipe,
        Err(error) => {
            stop_git_read(&mut child, GroupIdentity::Reserved);
            return Err(IpcError::new(
                IpcErrorCode::GitFailed,
                format!("could not read Git diff: {error}"),
            ));
        }
    };
    let deadline = Instant::now() + timeout;
    let mut poll = GIT_READ_POLL_MIN;
    // Held rather than taken straight off the channel, for the reason `run_capped_git` holds its
    // answer: a Git that has printed its whole diff and closed its pipe is a moment from exiting,
    // and a scan that asked about the status too early would lose the diff to asking again.
    let mut scan = None;
    // Watched and not collected, for the reason `exited_unreaped` gives.
    let mut exited = false;
    loop {
        if scan.is_none() {
            match scans.try_recv() {
                Ok(answer) => scan = Some(answer),
                // The scan is gone without an answer, so there is nothing left to wait for.
                Err(mpsc::TryRecvError::Disconnected) => {
                    stop_git_read(&mut child, GroupIdentity::Reserved);
                    return Err(git_pipe_failure(&PipeFailure::ReaderGone {
                        stream: "diff",
                    }));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        // A pipe that broke mid-diff leaves what arrived short of the whole thing, and a scan that
        // stopped at one of its caps has stopped reading altogether: either way the writer is in the
        // way of nothing, and the whole group is ended for the reason `stop_git_read` gives -- the
        // process doing the printing may be a diff filter rather than Git.
        if scan.as_ref().is_some_and(|scan| match scan {
            Ok(scan) => scan.too_large,
            Err(_) => true,
        }) {
            stop_git_read(&mut child, GroupIdentity::Reserved);
            break;
        }
        if !exited {
            match exited_unreaped(&child) {
                Ok(answer) => exited = answer,
                // It cannot be asked again, so it is ended rather than waited for, and the words it
                // got out first are still the diagnosis.
                Err(error) => {
                    stop_git_read(&mut child, GroupIdentity::Reserved);
                    let said = summarize_git_output(&[], &stderr.collect());
                    return Err(IpcError::new(
                        IpcErrorCode::GitFailed,
                        format!("could not read from Git: {error}: {said}"),
                    ));
                }
            }
        }
        // Both halves are waited for, because neither answers alone: the scan says the diff is over,
        // and the status says whether what came of it is text. A Git that has closed its pipe and is
        // a moment from exiting is the ordinary case here.
        if exited && scan.is_some() {
            break;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            stop_git_read(&mut child, GroupIdentity::Reserved);
            return Err(git_read_timeout(timeout, &[], &stderr.collect()));
        }
        thread::sleep(poll.min(remaining));
        poll = (poll * 2).min(GIT_READ_POLL_MAX);
    }
    // The loop ends on an answer from the scan in every case: either the scan is what ended it, or
    // the group was ended because the scan had stopped reading. What Git said is collected under the
    // drain deadline rather than waited on, because a helper that left the group can still hold the
    // write end and the answer here is the diff rather than the diagnostic.
    let scan = match scan.expect("the loop ends on an answer from the scan") {
        Ok(scan) => scan,
        Err(error) => {
            return Err(IpcError::new(
                IpcErrorCode::GitFailed,
                format!("could not read Git diff: {error}"),
            ))
        }
    };
    // The status is what says whether the diff that arrived is the diff Git meant to print, so it is
    // taken here even though the exit was watched above rather than collected. A collection that
    // cannot answer is ended the way every other exit is rather than left as a zombie.
    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            let group = reserved_group(&child);
            stop_git_read(&mut child, group);
            return Err(IpcError::new(
                IpcErrorCode::GitFailed,
                format!("could not finish Git diff: {error}"),
            ));
        }
    };
    Ok(ScannedDiff {
        output: Output {
            status,
            // The scan read the diff off this pipe line by line and nothing kept it, so the output
            // carries the exit status and what Git said rather than the diff itself.
            stdout: Vec::new(),
            stderr: stderr.collect(),
        },
        patch: scan.patch,
        revision: scan.revision,
        page: scan.page,
        hunks: scan.hunks,
        total_lines: scan.total_lines,
        is_binary: scan.is_binary,
        large: scan.large,
        too_large: scan.too_large,
    })
}

/// What one scan of a diff consumed, with the process that printed it left to the caller.
struct DiffScan {
    patch: Vec<u8>,
    revision: String,
    page: Vec<GitDiffPageLine>,
    hunks: Vec<GitDiffHunk>,
    total_lines: usize,
    is_binary: bool,
    large: bool,
    too_large: bool,
}

/// Parses one diff off a pipe, line by line, and stops as soon as it is past one of its caps.
///
/// Bounded memory is the whole of what this is for, which is why it streams and never collects: a
/// `Vec` that grows until the end is a whole diff held in memory, and `MAX_DIFF_BYTES` is the size
/// of the cap on that. The revision is hashed as the lines go by, so it sees the ones the patch
/// dropped too.
fn scan_diff_lines(
    stdout: impl Read,
    page_range: Option<(usize, usize)>,
) -> Result<DiffScan, io::Error> {
    let mut reader = BufReader::new(stdout);
    let mut patch = Vec::with_capacity(SMALL_DIFF_BYTES.min(64 * 1024));
    let mut revision = DiffRevision::default();
    let mut page = Vec::new();
    let mut hunks: Vec<GitDiffHunk> = Vec::new();
    let mut total_lines = 0;
    let mut total_bytes = 0;
    let mut old_line = None;
    let mut new_line = None;
    let mut is_binary = false;
    let mut large = false;
    let mut too_large = false;
    loop {
        let raw_line = match read_diff_line(&mut reader) {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                too_large = true;
                break;
            }
            // A pipe that broke mid-diff leaves what arrived short of the whole thing, which is not a
            // diff to draw: handed on, it is the prefix wearing the clothes of an answer. The caller
            // ends the group and reports this instead.
            Err(error) => return Err(error),
        };
        revision.read(&raw_line);
        total_bytes += raw_line.len();
        if total_bytes > MAX_DIFF_BYTES {
            too_large = true;
            break;
        }
        if raw_line.starts_with(b"@@") && raw_line.len() > 4096 {
            too_large = true;
            break;
        }
        is_binary |=
            raw_line.starts_with(b"Binary files ") || raw_line.starts_with(b"GIT binary patch");
        if !large && patch.len() + raw_line.len() <= SMALL_DIFF_BYTES {
            patch.extend_from_slice(&raw_line);
        } else if !large {
            large = true;
            patch.clear();
        }
        if let Some(mut line) = parse_diff_display_line(&raw_line, &mut old_line, &mut new_line) {
            if matches!(line.kind, GitDiffLineKind::Hunk) {
                if let Some(previous) = hunks.last_mut() {
                    previous.end_line = total_lines;
                }
                if hunks.len() == MAX_DIFF_HUNKS {
                    too_large = true;
                    break;
                }
                hunks.push(GitDiffHunk {
                    start_line: total_lines,
                    end_line: 0,
                    title: line.text.clone(),
                });
            }
            line.index = total_lines;
            if page_range.is_some_and(|(start, count)| {
                total_lines >= start && total_lines < start.saturating_add(count)
            }) {
                page.push(line);
            }
            total_lines += 1;
            if total_lines > MAX_DIFF_LINES {
                too_large = true;
                break;
            }
            if total_lines > SMALL_DIFF_LINES && !large {
                large = true;
                patch.clear();
            }
        }
    }
    if let Some(hunk) = hunks.last_mut() {
        hunk.end_line = total_lines;
    }
    Ok(DiffScan {
        patch,
        revision: revision.finish(),
        page,
        hunks,
        total_lines,
        is_binary,
        large: large || too_large,
        too_large,
    })
}

fn read_diff_line(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let has_newline = available.contains(&b'\n');
        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if line.len() + count > MAX_DIFF_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "diff line exceeds the size limit",
            ));
        }
        line.extend_from_slice(&available[..count]);
        reader.consume(count);
        if has_newline {
            return Ok(Some(line));
        }
    }
}

fn parse_diff_display_line(
    raw_line: &[u8],
    old_line: &mut Option<u64>,
    new_line: &mut Option<u64>,
) -> Option<GitDiffPageLine> {
    let text = String::from_utf8_lossy(raw_line)
        .trim_end_matches(['\n', '\r'])
        .to_owned();
    let (kind, old_number, new_number) = if text.starts_with("@@ ") {
        let Some((old_start, new_start)) = hunk_start_lines(&text) else {
            *old_line = None;
            *new_line = None;
            return None;
        };
        *old_line = Some(old_start);
        *new_line = Some(new_start);
        (GitDiffLineKind::Hunk, None, None)
    } else if old_line.is_some() && text.starts_with('-') {
        let line = *old_line;
        *old_line = old_line.map(|value| value.saturating_add(1));
        (GitDiffLineKind::Removed, line, None)
    } else if old_line.is_some() && text.starts_with('+') {
        let line = *new_line;
        *new_line = new_line.map(|value| value.saturating_add(1));
        (GitDiffLineKind::Added, None, line)
    } else if old_line.is_some() && text.starts_with(' ') {
        let old = *old_line;
        let new = *new_line;
        *old_line = old_line.map(|value| value.saturating_add(1));
        *new_line = new_line.map(|value| value.saturating_add(1));
        (GitDiffLineKind::Context, old, new)
    } else if text.starts_with('\\') && old_line.is_some() {
        (GitDiffLineKind::Meta, None, None)
    } else {
        return None;
    };
    Some(GitDiffPageLine {
        index: 0,
        kind,
        text,
        old_line_number: old_number,
        new_line_number: new_number,
    })
}

fn hunk_start_lines(header: &str) -> Option<(u64, u64)> {
    fn range_start(field: &str, marker: char) -> Option<u64> {
        let range = field.strip_prefix(marker)?;
        let (start, count) = range.split_once(',').unwrap_or((range, "1"));
        let start = start.parse::<u64>().ok()?;
        let count = count.parse::<u64>().ok()?;
        start.checked_add(count)?;
        Some(start)
    }

    let mut fields = header.split_whitespace();
    if fields.next()? != "@@" {
        return None;
    }
    let old_start = range_start(fields.next()?, '-')?;
    let new_start = range_start(fields.next()?, '+')?;
    (fields.next()? == "@@").then_some((old_start, new_start))
}

fn ensure_diff_succeeded(
    output: &Output,
    untracked: bool,
    too_large: bool,
) -> Result<(), IpcError> {
    if !too_large && !output.status.success() && !(untracked && output.status.code() == Some(1)) {
        return Err(git_error_without_stdout(
            "could not diff changed file",
            output,
        ));
    }
    Ok(())
}

/// What the repository watcher needs to know: every worktree still on disk, and the Git
/// directory they share.
///
/// A missing checkout is left out because there is nothing to watch for it, and nothing to read
/// either, so its row shows no counts rather than a false clean bill. A checkout whose
/// directory cannot be resolved is left out for the same reason rather than failing the whole
/// repository, which would leave every other worktree of it unwatched.
pub fn watch_plan(database: &Database, repo_id: &str) -> Result<RepoWatchPlan, IpcError> {
    let repos = workspace_repos(database)?;
    let repo = repos
        .iter()
        .find(|repo| repo.id == repo_id)
        .ok_or_else(|| {
            IpcError::new(
                IpcErrorCode::InvalidCheckout,
                "repository is not registered",
            )
        })?;
    if repo.kind != RepoKind::Git {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "only a Git repository has changes to watch",
        ));
    }
    let mut plan = RepoWatchPlan {
        repo_id: repo_id.to_owned(),
        ..RepoWatchPlan::default()
    };
    for checkout in &repo.checkouts {
        if checkout.is_missing {
            continue;
        }
        plan.requested.push(checkout.id.clone());
        let Ok(context) = git_context(&repos, &checkout.id) else {
            continue;
        };
        if plan.common_dir.is_none() {
            plan.common_dir = common_git_dir(&context);
        }
        plan.all.push(checkout.id.clone());
        plan.roots.insert(context.root.clone(), checkout.id.clone());
        // The primary repository's own Git directory *is* the shared one, so it is left out:
        // a ref written under it moves every sibling's merge base, and attributing it to the
        // checkout that happens to live there would leave the other rows describing a change
        // set that no longer exists.
        if let Some(git_dir) =
            own_git_dir(&context).filter(|dir| Some(dir) != plan.common_dir.as_ref())
        {
            plan.git_dirs.insert(git_dir, checkout.id.clone());
        }
    }
    if plan.roots.is_empty() {
        return Err(IpcError::new(
            IpcErrorCode::FolderMissing,
            "no worktree of this repository is on disk",
        ));
    }
    Ok(plan)
}

pub fn watch_plan_matches_request(plan: &RepoWatchPlan, expected_checkout_ids: &[String]) -> bool {
    let mut expected = expected_checkout_ids.to_vec();
    expected.sort();
    requested_watch_ids(plan) == expected
}

/// The checkout ids a repository is registered with, so unwatching can drop exactly the
/// snapshots that repository held.
pub fn repo_checkout_ids(database: &Database, repo_id: &str) -> Vec<String> {
    workspace_repos(database)
        .map(|repos| {
            repos
                .iter()
                .filter(|repo| repo.id == repo_id)
                .flat_map(|repo| repo.checkouts.iter())
                .map(|checkout| checkout.id.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// The Git directory that belongs to this checkout alone, as an absolute path: the primary
/// repository's own `.git`, or the `worktrees/<name>` directory a linked worktree gets. It is
/// where that worktree's index and HEAD live, and staging there moves nothing for its siblings.
fn own_git_dir(context: &GitContext) -> Option<PathBuf> {
    let text = optional_git_text(&context.root, ["rev-parse", "--absolute-git-dir"])?;
    absolute_git_path(&context.root, &text)
}

/// The Git directory the checkout shares with its siblings, as an absolute path.
///
/// A linked worktree resolves this to the primary repository's `.git`, which is what makes it
/// worth watching: it is where a commit in one worktree is published to the others.
fn common_git_dir(context: &GitContext) -> Option<PathBuf> {
    let text = optional_git_text(&context.root, ["rev-parse", "--git-common-dir"])?;
    absolute_git_path(&context.root, &text)
}

fn absolute_git_path(root: &Path, text: &str) -> Option<PathBuf> {
    let path = Path::new(text);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    absolute.canonicalize().ok()
}

fn registered_git_context(database: &Database, checkout_id: &str) -> Result<GitContext, IpcError> {
    let (repo, checkout) = database
        .load_registered_checkout(checkout_id)
        .map_err(|error| IpcError::new(IpcErrorCode::OperationFailed, error))?
        .ok_or_else(|| {
            IpcError::new(
                IpcErrorCode::InvalidCheckout,
                "checkout ID is not registered",
            )
        })?;
    git_context_for_checkout(&repo, &checkout)
}

fn git_context(repos: &[Repo], checkout_id: &str) -> Result<GitContext, IpcError> {
    let (repo, checkout) = crate::services::checkout::registered_checkout(
        repos,
        checkout_id,
        "checkout ID is not registered",
    )?;
    git_context_for_checkout(repo, checkout)
}

fn git_context_for_checkout(repo: &Repo, checkout: &Checkout) -> Result<GitContext, IpcError> {
    if repo.kind != RepoKind::Git {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "Git changes are only available for Git checkouts",
        ));
    }
    if checkout.is_missing {
        return Err(IpcError::new(
            IpcErrorCode::FolderMissing,
            "checkout is no longer available",
        ));
    }
    let root = resolve_checkout_path(repo, &checkout.id, Path::new("."))?;
    Ok(GitContext {
        repo: repo.clone(),
        checkout: checkout.clone(),
        root,
    })
}

fn snapshot(context: &GitContext) -> Result<Snapshot, IpcError> {
    let default = resolve_default_ref(
        &context.root,
        context.repo.default_branch.as_deref(),
        Some(Path::new(&context.repo.root)),
    )?;
    let merge_base_output = run_git(
        &context.root,
        vec![
            "merge-base".into(),
            default.reference.clone().into(),
            "HEAD".into(),
        ],
    )?;
    if !merge_base_output.status.success() {
        return Err(IpcError::new(
            IpcErrorCode::GitFailed,
            format!(
                "Git could not find a merge base between HEAD and {}",
                default.branch
            ),
        ));
    }
    let merge_base = output_text(&merge_base_output);

    let porcelain = checked_git(
        &context.root,
        ["status", "--porcelain=v2", "-z", "--untracked-files=all"],
        "could not read Git status",
    )?;
    let mut files = parse_porcelain_v2(&porcelain.stdout)?;

    let committed = checked_git(
        &context.root,
        [
            "diff",
            "--name-status",
            "-z",
            "--find-renames",
            &merge_base,
            "HEAD",
        ],
        "could not list committed changes",
    )?;
    for file in parse_name_status(&committed.stdout)? {
        files
            .entry(file.path.clone())
            .and_modify(|current| {
                if current.old_path.is_none() {
                    current.old_path = file.old_path.clone();
                }
            })
            .or_insert(file);
    }

    let files: Vec<_> = files.into_values().collect();
    let branch = optional_git_text(
        &context.root,
        ["symbolic-ref", "--quiet", "--short", "HEAD"],
    );
    let head = optional_git_text(&context.root, ["rev-parse", "--short", "HEAD"]);
    let ahead_count = checked_git(
        &context.root,
        ["rev-list", "--count", &format!("{merge_base}..HEAD")],
        "could not count commits ahead of the default branch",
    )?
    .stdout;
    let ahead_count = String::from_utf8_lossy(&ahead_count)
        .trim()
        .parse()
        .map_err(|error| {
            IpcError::new(
                IpcErrorCode::GitFailed,
                format!("Git returned an invalid ahead count: {error}"),
            )
        })?;

    Ok(Snapshot {
        status: GitStatus {
            branch,
            head,
            default_branch: default.branch,
            ahead_count,
            files,
        },
        merge_base,
    })
}

#[derive(Debug)]
pub(crate) struct DefaultRef {
    pub(crate) reference: String,
    pub(crate) branch: String,
}

pub(crate) fn resolve_default_ref(
    root: &Path,
    persisted: Option<&str>,
    primary_root: Option<&Path>,
) -> Result<DefaultRef, IpcError> {
    if let Some(branch) = persisted.map(|branch| branch.strip_prefix("origin/").unwrap_or(branch)) {
        if let Some(default) = resolve_branch_ref(root, branch) {
            return Ok(default);
        }
    }

    if let Ok(output) = run_git(
        root,
        vec![
            "symbolic-ref".into(),
            "--quiet".into(),
            "refs/remotes/origin/HEAD".into(),
        ],
    ) {
        if output.status.success() {
            let reference = output_text(&output);
            if let Some(branch) = reference.strip_prefix("refs/remotes/origin/") {
                if let Some(default) = resolve_branch_ref(root, branch) {
                    return Ok(default);
                }
            }
        }
    }

    if let Some(primary_root) = primary_root
        .filter(|path| path.is_dir())
        .filter(|path| share_git_common_dir(root, path))
    {
        if let Some(branch) =
            optional_git_text(primary_root, ["symbolic-ref", "--quiet", "--short", "HEAD"])
        {
            if let Some(default) = resolve_branch_ref(root, &branch) {
                return Ok(default);
            }
        }
    }

    Err(IpcError::new(
        IpcErrorCode::DefaultBranchUnknown,
        "Git could not determine a valid default branch; choose one before viewing changes",
    ))
}

fn share_git_common_dir(first: &Path, second: &Path) -> bool {
    fn common_dir(path: &Path) -> Option<PathBuf> {
        let text = optional_git_text(path, ["rev-parse", "--git-common-dir"])?;
        let common = Path::new(&text);
        if common.is_absolute() {
            common.canonicalize().ok()
        } else {
            path.join(common).canonicalize().ok()
        }
    }
    common_dir(first)
        .zip(common_dir(second))
        .is_some_and(|(first, second)| first == second)
}

pub(crate) fn resolve_branch_ref(root: &Path, branch: &str) -> Option<DefaultRef> {
    if branch.is_empty()
        || !run_git(
            root,
            vec![
                "check-ref-format".into(),
                format!("refs/heads/{branch}").into(),
            ],
        )
        .is_ok_and(|output| output.status.success())
    {
        return None;
    }
    for prefix in ["refs/remotes/origin/", "refs/heads/"] {
        let reference = format!("{prefix}{branch}");
        if run_git(
            root,
            vec![
                "rev-parse".into(),
                "--verify".into(),
                "--quiet".into(),
                format!("{reference}^{{commit}}").into(),
            ],
        )
        .is_ok_and(|output| output.status.success())
        {
            return Some(DefaultRef {
                reference,
                branch: branch.to_owned(),
            });
        }
    }
    None
}

pub(crate) fn parse_porcelain_v2(
    output: &[u8],
) -> Result<BTreeMap<String, GitChangedFile>, IpcError> {
    let mut records = output.split(|byte| *byte == 0).peekable();
    let mut files = BTreeMap::new();
    while let Some(record) = records.next() {
        if record.is_empty() || record.first() == Some(&b'#') {
            continue;
        }
        let (path, old_path, status) = match record.first() {
            Some(b'?') if record.starts_with(b"? ") => {
                (record.get(2..).unwrap_or_default(), None, "??".to_owned())
            }
            Some(b'1') => (
                path_after_fields(record, 8).ok_or_else(malformed_status)?,
                None,
                status_from_xy(record.get(2..4).unwrap_or_default()),
            ),
            Some(b'2') => {
                let path = path_after_fields(record, 9).ok_or_else(malformed_status)?;
                let old_path = records.next().ok_or_else(malformed_status)?;
                (
                    path,
                    Some(path_text(old_path)?),
                    status_from_xy(record.get(2..4).unwrap_or_default()),
                )
            }
            Some(b'u') => (
                path_after_fields(record, 10).ok_or_else(malformed_status)?,
                None,
                status_from_xy(record.get(2..4).unwrap_or_default()),
            ),
            _ => return Err(malformed_status()),
        };
        let path = path_text(path)?;
        files.insert(
            path.clone(),
            GitChangedFile {
                path,
                old_path,
                status,
                additions: None,
                deletions: None,
            },
        );
    }
    Ok(files)
}

fn parse_name_status(output: &[u8]) -> Result<Vec<GitChangedFile>, IpcError> {
    let records: Vec<_> = output.split(|byte| *byte == 0).collect();
    let mut files = Vec::new();
    let mut index = 0;
    while index < records.len() {
        let status_record = records[index];
        index += 1;
        if status_record.is_empty() {
            continue;
        }
        let status = status_record
            .first()
            .copied()
            .ok_or_else(malformed_status)?;
        if matches!(status, b'R' | b'C') {
            let old_path = records.get(index).ok_or_else(malformed_status)?;
            let path = records.get(index + 1).ok_or_else(malformed_status)?;
            index += 2;
            let path = path_text(path)?;
            files.push(GitChangedFile {
                path,
                old_path: Some(path_text(old_path)?),
                status: String::from_utf8_lossy(&status_record[..1]).into_owned(),
                additions: None,
                deletions: None,
            });
        } else {
            let path = records.get(index).ok_or_else(malformed_status)?;
            index += 1;
            let path = path_text(path)?;
            files.push(GitChangedFile {
                path,
                old_path: None,
                status: String::from_utf8_lossy(&status_record[..1]).into_owned(),
                additions: None,
                deletions: None,
            });
        }
    }
    Ok(files)
}

fn path_after_fields(record: &[u8], field_count: usize) -> Option<&[u8]> {
    let mut remaining = record;
    for _ in 0..field_count {
        let separator = remaining.iter().position(|byte| *byte == b' ')?;
        remaining = &remaining[separator + 1..];
    }
    Some(remaining)
}

fn status_from_xy(xy: &[u8]) -> String {
    let status = String::from_utf8_lossy(xy).trim().to_owned();
    if status.is_empty() {
        "M".to_owned()
    } else {
        status
    }
}

fn path_text(path: &[u8]) -> Result<String, IpcError> {
    String::from_utf8(path.to_vec())
        .map(|path| path.replace(std::path::MAIN_SEPARATOR, "/"))
        .map_err(|_| {
            IpcError::new(
                IpcErrorCode::InvalidPath,
                "Git paths that are not valid UTF-8 cannot be displayed in Changes",
            )
        })
}

fn malformed_status() -> IpcError {
    IpcError::new(
        IpcErrorCode::GitFailed,
        "Git returned an invalid porcelain status record",
    )
}

fn validate_relative_path(path: &str) -> Result<PathBuf, IpcError> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            ) || matches!(component, Component::Normal(name) if name == ".git")
        })
    {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "diff paths must be relative to the checkout and cannot access .git",
        ));
    }
    Ok(path.to_path_buf())
}

fn validate_diff_target(context: &GitContext, path: &Path) -> Result<(), IpcError> {
    let mut parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    loop {
        match resolve_checkout_path(&context.repo, &context.checkout.id, parent) {
            Ok(_) => break,
            Err(error) if matches!(error.code, IpcErrorCode::FolderMissing) => {
                parent = parent
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."));
                if parent == Path::new(".") {
                    resolve_checkout_path(&context.repo, &context.checkout.id, parent)?;
                    break;
                }
            }
            Err(error) => return Err(error),
        }
    }
    let target = context.root.join(path);
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => {
            resolve_checkout_path(&context.repo, &context.checkout.id, path)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(IpcError::new(
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                IpcErrorCode::PermissionDenied
            } else {
                IpcErrorCode::OperationFailed
            },
            format!("could not inspect diff path: {error}"),
        )),
    }
}

fn literal_pathspec(path: &str) -> OsString {
    OsString::from(format!(":(literal){path}"))
}

fn optional_git_text<const N: usize>(root: &Path, args: [&str; N]) -> Option<String> {
    checked_git(root, args, "Git query failed")
        .ok()
        .map(|output| output_text(&output))
        .filter(|text| !text.is_empty())
}

fn checked_git<const N: usize>(
    root: &Path,
    args: [&str; N],
    action: &str,
) -> Result<Output, IpcError> {
    let output = run_git(root, args.into_iter().map(OsString::from).collect())?;
    if !output.status.success() {
        return Err(git_error(action, &output));
    }
    Ok(output)
}

/// Runs Git to read, never to write, and with the optional locks off so the read leaves nothing
/// behind.
///
/// `git status` takes the index lock to refresh stat data, and the index of a linked worktree
/// lives inside the very directory the watcher is watching. A refresh would then announce itself,
/// the announcement would ask for the refresh that wrote it, and the repository would spend the
/// rest of its life re-reading itself. Git answers the same question without the lock, and the
/// index stays exactly where the last command that meant to change it left it.
///
/// The read also runs under a deadline. Everything that reaches here is a read -- a status, a list
/// of changed names, a ref lookup, a merge base -- so ending a Git that will not finish cannot
/// leave a commit half written, a lock behind it or an index caught mid-refresh; there is nothing
/// of its own to be in the middle of. What does write goes through `git::run_git`, which has no
/// deadline for exactly the opposite reason: a commit stopped at one would be a commit that half
/// happened.
fn run_git(root: &Path, args: Vec<OsString>) -> Result<Output, IpcError> {
    run_command_with_timeout("git", root, args, GIT_READ_TIMEOUT)
}

/// A Git read run by a module outside this one, under a deadline of the caller's.
///
/// The reads elsewhere in the app need this machinery rather than a copy of it. The deadline is
/// only the half of what keeps a read from being a wait that never ends; the other half is the
/// process group of its own, the pipes drained on threads of their own and the diagnosis that
/// travels with the failure, and a module that grew its own `Command::output` would be the one
/// place a read stopped being bounded. The deadline is passed in rather than taken from here so
/// `GIT_READ_TIMEOUT` stays the one answer to how long a Git read may take, and so a test can
/// watch a read end a Git that will not finish without waiting that long to find out that it does.
pub(crate) fn run_git_read(
    root: &Path,
    args: &[&str],
    within: Duration,
) -> Result<Output, IpcError> {
    run_command_with_timeout(
        "git",
        root,
        args.iter().map(OsString::from).collect(),
        within,
    )
}

/// One Git process under the read deadline, told what to read on its standard input.
///
/// `check-ignore --stdin` takes a whole burst of paths that way, which is what keeps the query one
/// call per checkout rather than one per file. Everything else about it is `run_command_with_timeout`
/// -- the optional locks off, the process group of its own, the same deadline -- so a check-ignore
/// that will not finish is ended by the same machinery and for the same reasons as every other read.
fn run_git_on_paths(root: &Path, args: [&str; 3], paths: &[PathBuf]) -> Result<Output, IpcError> {
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(path.as_os_str().as_bytes());
        input.push(0);
    }
    run_command_with_input("git", root, args, GIT_READ_TIMEOUT, &input)
}

/// Runs one Git process under a deadline and hands back what it printed either way.
fn run_command_with_input(
    program: &str,
    root: &Path,
    args: [&str; 3],
    timeout: Duration,
    input: &[u8],
) -> Result<Output, IpcError> {
    spawn_and_collect(
        program,
        root,
        args.iter().map(OsString::from).collect(),
        timeout,
        Some(input.to_vec()),
    )
}

/// Runs one Git process under a deadline and hands back what it printed either way.
fn run_command_with_timeout(
    program: &str,
    root: &Path,
    args: Vec<OsString>,
    timeout: Duration,
) -> Result<Output, IpcError> {
    spawn_and_collect(program, root, args, timeout, None)
}

/// Runs one Git process under a deadline and hands back what it printed either way.
///
/// The deadline is on the process and not on the reading, so both pipes are drained on threads of
/// their own: a Git that never exits holds a pipe open, and a read on that pipe is the one wait
/// here with nothing left to time it out with. Its process group is its own, so what hung can be
/// ended whole rather than only the process `spawn` returned, and the pipes are collected under a
/// deadline of their own, because a helper that left the group can still hold the write end and a
/// collection that waited for it forever would be the failure this whole path exists to remove.
///
/// That deadline also bounds what a read can come back with, so what a pipe failed to give is
/// reported rather than handed on: a Git that finished inside its deadline and printed something
/// this never read is not a Git that printed nothing, and only one of those is a checkout with no
/// changes in it.
///
/// A bound on the volume as well is a different kind of answer, and it is here because a deadline
/// says nothing about how much a Git prints while it has it: a `status` of a repository with a very
/// large change set was accumulated whole before anything summarized or parsed it. Each pipe keeps
/// `GIT_OUTPUT_CAP` or `GIT_DIAGNOSTIC_CAP` bytes and says so when there was more, which is an error
/// rather than what arrived first -- every caller reads this as the state of a whole checkout, and
/// the first sixteen megabytes of a `status` is not a status. The reader carries on past the cap and
/// discards, because a Git that prints more than the cap and then keeps talking is a Git that blocks
/// on a pipe nobody is reading, and ending it for printing is not the same ending it for hanging.
///
/// Every way out of here after the `spawn` ends the child, which is why each thread it needs is
/// started through `start_read_thread`: a read that could not start one is a machine with no threads
/// left, not a reason to leave a Git running and unreaped behind a panic.
///
/// `input` is what the process is told to read, fed on a thread of its own for the same reason the
/// output pipes are drained on threads of their own: a pipe read or written on this one is a wait
/// here with no deadline left to time it out with.
fn spawn_and_collect(
    program: &str,
    root: &Path,
    args: Vec<OsString>,
    timeout: Duration,
    input: Option<Vec<u8>>,
) -> Result<Output, IpcError> {
    #[cfg(test)]
    count_git_read(root);
    let mut child = Command::new(program)
        .args(args)
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        // Every diagnostic this file reads back as a fact about the repository is Git's own wording,
        // and one of them is what tells a plain folder from a repository whose config cannot be read
        // (`is_plain_directory`). A translated Git answers in the user's locale, and that sentence
        // is then not the one being compared, so a plain folder is reported as a broken repository
        // instead. The locale is pinned on the child rather than in this process so a user's own
        // locale still governs everything they see themselves; `LANGUAGE` is dropped because gettext
        // consults it ahead of `LC_ALL` and it would override the pin. Nothing else is compared to
        // Git's words, and nothing here is shown to a user untranslated.
        .env("LC_ALL", "C")
        .env_remove("LANGUAGE")
        // Nothing a read asks of Git can be answered by a person: a prompt with no terminal behind
        // it is the wait this deadline exists for, so the reading end is closed the way the
        // non-interactive `Command::output` this replaced closed it -- unless the read asked for a
        // list on it, which `check-ignore --stdin` is.
        .stdin(match &input {
            Some(_) => Stdio::piped(),
            None => Stdio::null(),
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| {
            IpcError::new(
                IpcErrorCode::GitFailed,
                format!("could not start Git: {error}"),
            )
        })?;
    if let (Some(bytes), Some(pipe)) = (input, child.stdin.take()) {
        // The write end is dropped when the bytes are all there, which is what tells a read that is
        // waiting for its list that the list is complete: a `--stdin` read that never sees the end
        // waits forever, and the deadline would end it having answered nothing.
        if let Err(error) = start_read_thread("muster-git-input", move || {
            let mut pipe = pipe;
            let _ = pipe.write_all(&bytes);
        }) {
            // Git is running with a list half fed and no thread to feed the rest of it, so it is
            // ended and collected here: returning the failure on its own would leave it waiting on a
            // write end that nobody is left to close.
            stop_git_read(&mut child, GroupIdentity::Reserved);
            return Err(IpcError::new(
                IpcErrorCode::GitFailed,
                format!("could not feed Git: {error}"),
            ));
        }
    }
    // From here on there is a Git to end on every way out of this function, which is why a reader
    // that cannot be started is a failure to report rather than a panic to unwind: the panic would
    // leave the child behind, running, and holding the pipes nobody drained.
    let pipes = match GitPipes::take(&mut child) {
        Ok(pipes) => pipes,
        Err(error) => {
            stop_git_read(&mut child, GroupIdentity::Reserved);
            return Err(IpcError::new(
                IpcErrorCode::GitFailed,
                format!("could not read Git's output: {error}"),
            ));
        }
    };
    let deadline = Instant::now() + timeout;
    let mut poll = GIT_READ_POLL_MIN;
    // Watched and not collected, and collected only once the pipes have said what they are going
    // to say: the exit is read the way `exited_unreaped` reads it because the group this read may
    // still have to end is named after this pid, and a pid the kernel has handed back is a number
    // that can belong to somebody else by the time a signal goes to it.
    loop {
        match exited_unreaped(&child) {
            Ok(true) => break,
            Ok(false) => {}
            Err(error) => {
                // It cannot be asked again, so it is ended rather than waited for, and the words it
                // got out first are still the diagnosis.
                stop_git_read(&mut child, GroupIdentity::Reserved);
                let (stdout, stderr) = pipes.collect();
                return Err(IpcError::new(
                    IpcErrorCode::GitFailed,
                    format!(
                        "could not read from Git: {error}: {}",
                        summarize_git_output(&stdout, &stderr)
                    ),
                ));
            }
        }
        // Asked while Git is still running, because a Git printing past what this read keeps is a
        // Git that will not exit inside the deadline: reported as a read that ran out of time, it
        // would be answered with the reason it did not.
        if let Some((stream, cap)) = pipes.overflowed() {
            stop_git_read(&mut child, GroupIdentity::Reserved);
            let (stdout, stderr) = pipes.collect();
            return Err(git_output_over_cap(stream, cap, &stdout, &stderr));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            stop_git_read(&mut child, GroupIdentity::Reserved);
            let (stdout, stderr) = pipes.collect();
            return Err(git_read_timeout(timeout, &stdout, &stderr));
        }
        thread::sleep(poll.min(remaining));
        poll = (poll * 2).min(GIT_READ_POLL_MAX);
    }
    let (stdout, stderr) = match pipes.read() {
        Ok(printed) => printed,
        Err(failure) => {
            // The leader above is a zombie and not a pid the kernel has reused, so the group it
            // leads is still the group this read started and still answers to a number nothing
            // else can be handed. That is what makes the signal below Git's own rather than a
            // stranger's, and it holds exactly as long as the leader is left uncollected: it is
            // collected in `stop_git_read` and nowhere before. What a pipe still held can be is a
            // member of that group -- a helper of Git's holding the write end long after Git
            // itself was gone -- and it has to be ended for the reason `stop_git_read` gives, not
            // a moment later. Only a pipe that is still held is signalled, because that is the one
            // failure with a member left in the group. A pipe that failed, or whose reader is gone,
            // proves no such thing, and a writer that left the group cannot be told apart from one
            // that is still in it -- so the leader is collected and the group is left to it.
            if failure.writer_left() {
                stop_git_read(&mut child, GroupIdentity::Reserved);
            } else {
                let _ = child.wait();
            }
            return Err(git_pipe_failure(&failure));
        }
    };
    // Past a cap is refused here rather than handed on as what arrived first, for the reason
    // `git_output_over_cap` gives. Read after the exit and not during the loop above because the
    // bytes are the whole answer only once the writer is gone; the loop asked early only to end a
    // Git that was printing without end.
    if let Some((stream, cap, _)) = [
        ("stdout", GIT_OUTPUT_CAP, &stdout),
        ("stderr", GIT_DIAGNOSTIC_CAP, &stderr),
    ]
    .into_iter()
    .find(|(_, _, printed)| printed.over_cap)
    {
        let _ = child.wait();
        return Err(git_output_over_cap(
            stream,
            cap,
            &stdout.bytes,
            &stderr.bytes,
        ));
    }
    // Collected here, where everything the group could still be asked about is settled.
    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            let group = reserved_group(&child);
            stop_git_read(&mut child, group);
            return Err(IpcError::new(
                IpcErrorCode::GitFailed,
                format!("could not read from Git: {error}"),
            ));
        }
    };
    Ok(Output {
        status,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
    })
}

/// The error a read leaves behind when Git printed more than the read keeps.
///
/// Refused rather than trimmed, because every caller reads what came back as the state of a whole
/// checkout: the first `GIT_OUTPUT_CAP` bytes of a `status` is not a status, and handing it on is
/// a wrong answer wearing the clothes of a right one. What Git said on its way out still travels as
/// the diagnosis, under the same summary limit as every other failure.
fn git_output_over_cap(stream: &str, cap: usize, stdout: &[u8], stderr: &[u8]) -> IpcError {
    let over = format!(
        "Git printed more than {} on its {stream}, which this read does not keep: the answer is \
         refused rather than trimmed into a shorter one",
        say_bytes(cap)
    );
    let said = summarize_git_output(stdout, stderr);
    IpcError::new(
        IpcErrorCode::GitFailed,
        if said.is_empty() {
            over
        } else {
            format!("{over}: {said}")
        },
    )
}

/// How a cap is said out loud, in the unit it was chosen in.
fn say_bytes(cap: usize) -> String {
    format!("{} MiB", cap / (1024 * 1024))
}

/// Ends what a read left running, and collects the process `spawn` returned.
///
/// The whole process group is signalled rather than only the process `spawn` returned, because what
/// hangs is usually not Git but something it started and is waiting for: an LFS filter on a network
/// that is not answering, a credential helper waiting on a prompt with no terminal, `gpg` waiting
/// for a PIN. Killing Git alone would leave those holding the pipes the output is collected from, so
/// the collection would go on waiting for processes nothing is going to end. `process_group(0)` put
/// Git and everything it spawned into a group of its own, so the signal reaches that whole tree and
/// nothing else of ours, and `SIGKILL` because a process that ignored one deadline does not answer a
/// polite request. The `wait` afterwards is what collects it: a read that stopped Git and left it
/// unreached would leave a zombie behind for every hung read in the session.
///
/// Both callers reach here with the leader uncollected, and that is the whole of what keeps the aim
/// honest: an unreaped pid is a number the kernel has not handed to anyone else, so `-pid` is the
/// group this read started rather than whichever group answers to a number that was free a moment
/// ago. One caller arrives while the leader is still running and ends the whole tree at once; the
/// other arrives after the leader exited and its pipes did not, and ends the group it left behind.
/// A helper that left that group is out of reach either way -- a held pipe proves a writer and never
/// which group it writes from -- and that is the price of not answering the same question with a
/// signal nobody can point at anything.
///
/// `group` is that claim made at the call site rather than assumed here, because there are two ways
/// out of a read and only one of them still holds it: a collection that has already happened, or
/// failed in a way this cannot interpret, leaves a number the kernel is free to hand to somebody
/// else, and a signal to `-pid` then aims at whatever answers to a number that was free a moment
/// ago. `Spent` ends the process `spawn` returned and nothing else, which is the direction that
/// leaves a helper behind rather than the one that signals a stranger.
fn stop_git_read(child: &mut Child, group: GroupIdentity) {
    if group == GroupIdentity::Reserved {
        let group = libc::pid_t::try_from(child.id())
            .ok()
            .filter(|pid| *pid > 0)
            .map(|pid| -pid);
        if let Some(group) = group {
            // A group that is already gone says nothing worth reporting: the read is over either way,
            // and the direct kill below covers the process itself. One that is not is this read's own,
            // because the leader is still here to hold its number.
            unsafe { libc::kill(group, libc::SIGKILL) };
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Whether the number a group is named after is still this read's own.
///
/// `Reserved` is the claim `stop_git_read` is allowed to act on, and it holds exactly while this
/// read holds an uncollected child: the kernel cannot hand out the number of a process that is
/// still its own, so `-pid` is the group this read started. `Spent` is the admission that a
/// collection has happened or has failed in a way that leaves the state unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupIdentity {
    Reserved,
    Spent,
}

/// What the identity of the group is, asked of the only thing that can answer it.
///
/// Used where a collection has already been attempted and failed: the process may be a zombie
/// still, and asking is what tells it apart from a number the kernel has already given away. A
/// `waitid` that fails answers nothing, so the claim is given up and only the process `spawn`
/// returned is ended -- which is the direction that can leave a helper behind rather than the one
/// that signals whatever group has the number by now.
fn reserved_group(child: &Child) -> GroupIdentity {
    match exited_unreaped(child) {
        Ok(_) => GroupIdentity::Reserved,
        Err(_) => GroupIdentity::Spent,
    }
}

/// The error a read that ran out of time leaves behind.
///
/// What Git said on its way out is the diagnosis -- a ref it could not lock, a helper it could not
/// reach, a signature it could not make -- so it travels with the error under the existing summary
/// limit. The summary truncates; it does not redact paths.
fn git_read_timeout(timeout: Duration, stdout: &[u8], stderr: &[u8]) -> IpcError {
    let stopped = format!("Git stopped reading after {}s", timeout.as_secs_f32());
    let said = summarize_git_output(stdout, stderr);
    IpcError::new(
        IpcErrorCode::GitFailed,
        if said.is_empty() {
            stopped
        } else {
            format!("{stopped}: {said}")
        },
    )
}

pub(crate) fn summarize_git_output(stdout: &[u8], stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(if stderr.is_empty() { stdout } else { stderr });
    let mut summary: Vec<String> = text
        .lines()
        .rev()
        .filter_map(|line| {
            let line = line.trim();
            (!line.is_empty()).then(|| {
                let kept: String = line.chars().take(GIT_DIAGNOSTIC_WIDTH).collect();
                if kept.len() == line.len() {
                    kept
                } else {
                    format!("{kept}…")
                }
            })
        })
        .take(GIT_DIAGNOSTIC_LINES)
        .collect();
    summary.reverse();
    summary.join(" / ")
}

/// What a Git process printed, drained on threads of its own.
///
/// The threads are what make the deadline above a wait on the process rather than a read on a pipe.
/// They hand their bytes over through a channel rather than a join because a join cannot be waited
/// on for a deadline either, and the only wait left with room to give is the read itself.
struct GitPipes {
    stdout: PipeReader,
    stderr: PipeReader,
}

impl GitPipes {
    /// Takes both pipes of a Git that was started with them piped, under their own caps.
    ///
    /// Fails when a reader cannot be started, which is said rather than panicked: the Git is already
    /// running, and `spawn_and_collect` ends and collects it rather than unwinding a worker with a
    /// live child behind it.
    fn take(child: &mut Child) -> io::Result<Self> {
        Ok(Self {
            stdout: PipeReader::take(
                child.stdout.take().expect("piped stdout"),
                "stdout",
                "muster-git-stdout",
                GIT_OUTPUT_CAP,
            )?,
            stderr: PipeReader::take(
                child.stderr.take().expect("piped stderr"),
                "stderr",
                "muster-git-stderr",
                GIT_DIAGNOSTIC_CAP,
            )?,
        })
    }

    /// Which pipe is already carrying more than this read keeps, if either is.
    ///
    /// Asked with the same non-blocking question the read loop asks about the process, and read off
    /// a flag rather than off the answer, because the answer of a pipe whose writer has not closed
    /// it yet is not there to ask for: a Git printing without end holds its own pipes open until it
    /// is ended. A loop that waited for the answer would report a read that ran out of time, which
    /// is not the reason it ran out.
    fn overflowed(&self) -> Option<(&'static str, usize)> {
        if self.stdout.overflowed() {
            Some((self.stdout.stream, self.stdout.cap))
        } else if self.stderr.overflowed() {
            Some((self.stderr.stream, self.stderr.cap))
        } else {
            None
        }
    }

    /// What Git printed, or as much of it as arrived inside the drain deadline.
    ///
    /// Best effort on purpose, because this is what a read that has *already* failed leaves behind
    /// as its diagnosis: a pipe that failed or was still held is the smaller of the two failures,
    /// and the one that came first is the one the caller is being told about. Only `read` reports a
    /// pipe's own failure, and only `read` is on a path that has not failed yet.
    fn collect(self) -> (Vec<u8>, Vec<u8>) {
        (self.stdout.collect(), self.stderr.collect())
    }

    /// What Git printed, or why the answer is not known.
    ///
    /// The same wait as `collect`, but a pipe that could not be read and a pipe nobody finished
    /// writing are answers rather than silences. Handed on as empty they are a read that failed
    /// wearing the clothes of a read that answered, and the caller cannot tell the two apart --
    /// which for the reads this whole path serves is the difference between "this checkout has no
    /// changes" and "this checkout was never read".
    fn read(self) -> Result<(PipeOutput, PipeOutput), PipeFailure> {
        let stdout = self.stdout.read();
        let stderr = self.stderr.read();
        // Both pipes are waited for even once one of them has failed, because the other is where
        // Git's own words would have been and because a pipe nobody drained is a reader thread
        // nobody let go. The first failure is the one named: the pipe that failed says the reading
        // itself went wrong, and it does not matter which pipe was asked first.
        match (stdout, stderr) {
            (Ok(stdout), Ok(stderr)) => Ok((stdout, stderr)),
            (Err(failure), _) | (_, Err(failure)) => Err(failure),
        }
    }
}

/// What one of Git's pipes carried: the bytes up to a cap, and whether more of them arrived.
///
/// The cap bounds what is kept and never what is read. A Git still writing into a pipe nobody is
/// reading blocks on it, so the reading carries on to the end and drops the rest: the answer
/// becomes "there was more than this", which a caller can refuse, instead of a process parked on a
/// pipe until a deadline ends it.
#[derive(Debug, Default)]
struct PipeOutput {
    bytes: Vec<u8>,
    over_cap: bool,
}

/// One of Git's pipes, read on a thread of its own and answered through a channel.
struct PipeReader {
    stream: &'static str,
    cap: usize,
    /// Set by the reader the moment it drops a byte past the cap, so a read that is still waiting
    /// for the process can already tell that the answer is not coming whole. The answer it sends
    /// when the pipe closes says the same thing; this is what says it before then.
    over_cap: Arc<AtomicBool>,
    pipe: mpsc::Receiver<io::Result<PipeOutput>>,
}

impl PipeReader {
    fn take<T: Read + Send + 'static>(
        pipe: T,
        stream: &'static str,
        thread: &'static str,
        cap: usize,
    ) -> io::Result<Self> {
        let over_cap = Arc::new(AtomicBool::new(false));
        Ok(Self {
            stream,
            cap,
            pipe: drain_pipe(pipe, thread, cap, over_cap.clone())?,
            over_cap,
        })
    }

    /// Whether Git has printed more than this read keeps, asked without waiting for the pipe.
    fn overflowed(&self) -> bool {
        self.over_cap.load(Ordering::Acquire)
    }

    /// What Git printed, or as much of it as arrived inside the drain deadline.
    ///
    /// Best effort on purpose, because this is what a read that has *already* failed leaves behind
    /// as its diagnosis: a pipe that failed or was still held is the smaller of the two failures,
    /// and the one that came first is the one the caller is being told about. Only `read` reports a
    /// pipe's own failure, and only `read` is on a path that has not failed yet.
    fn collect(self) -> Vec<u8> {
        self.read().unwrap_or_default().bytes
    }

    /// What Git printed, or why the answer is not known.
    ///
    /// A pipe that could not be read and a pipe nobody finished writing are answers rather than
    /// silences. Handed on as empty they are a read that failed wearing the clothes of a read that
    /// answered, and the caller cannot tell the two apart -- which for the reads this whole path
    /// serves is the difference between "this checkout has no changes" and "this checkout was
    /// never read".
    fn read(self) -> Result<PipeOutput, PipeFailure> {
        pipe_read(self.pipe, self.stream)
    }
}

/// Why one of Git's pipes did not give up everything Git printed, and which of the two it was.
///
/// An empty pipe and an unreadable one are the same bytes and not the same answer: only the first
/// is a Git that had nothing to say, and the second is a Git whose words went somewhere this cannot
/// reach. They are kept apart for that reason, and so that a failure is described by what it was
/// rather than by a single "could not read it".
#[derive(Debug)]
enum PipeFailure {
    /// The read failed on the way, so what is on that pipe is not all of what Git said.
    Read {
        stream: &'static str,
        error: io::Error,
    },
    /// The read was still going when the drain deadline was, which means a writer nobody ended is
    /// still on the other end: Git is gone and something it started kept its pipe.
    StillHeld { stream: &'static str },
    /// The reader is gone without having sent anything, so there is nothing left to wait for and
    /// nothing left to say what Git printed.
    ReaderGone { stream: &'static str },
}

impl PipeFailure {
    /// Whether something of Git's is still on the other end of the pipe, writing into it.
    ///
    /// The only failure of the three that leaves a process running, and so the only one a group can
    /// be signalled for: the other two say the reading itself went wrong and say nothing at all
    /// about what is still alive.
    fn writer_left(&self) -> bool {
        matches!(self, Self::StillHeld { .. })
    }
}

/// Reads one of Git's pipes to its end on a thread of its own, keeping up to `cap` bytes of it.
///
/// Reading to the end is what the cap does not stop: a pipe whose reader stops is a pipe its writer
/// blocks on, so a Git that printed more than the cap and then kept talking would be parked on it
/// until the deadline ended it. So the rest is read and dropped, and `over_cap` is what says there
/// was more -- a refusal the caller can make, rather than a process stopped for want of a reader.
///
/// This thread is not ended when the answer is given up on, and that is deliberate rather than an
/// oversight: a read on a pipe blocks with no deadline of its own, so the only ways out of one are
/// interrupting the thread (unsafe, and unsafe around the buffers this closure owns) or closing the
/// reading end. Closing it is what a cancellation flag plus a non-blocking `poll` loop would do, and
/// it is not done here for the reason `stop_git_read` gives: the only thing that proves there is a
/// writer out there is that the pipe is still held, which says nothing about which process it is or
/// which group it writes from. Ending the read therefore ends the writer -- by `EPIPE`, then by the
/// `SIGPIPE` that follows -- for a process this code cannot point at, which is the same signal
/// through the same unverified aim that `stop_git_read` refuses to send.
///
/// What is left is this: a helper that left the group and kept its write end open holds one thread
/// and one file descriptor until that writer is gone, and no deadline here can end it. It is the
/// cost of not guessing, it is paid once per read whose writer escapes, and the read itself is still
/// bounded by `GIT_PIPE_DRAIN` -- which is what keeps it from becoming the failure this path exists
/// to remove. Draining past the cap leaks the same way when the writer escaped: the overflow path
/// stops the group and then collects best effort, so the same thread stays on the same pipe and its
/// answer is swallowed rather than reported, which `collect` is already best effort for. A fix has to
/// make the writer identifiable before it may make the reader stoppable, and nothing this runner
/// knows is enough to do that.
fn drain_pipe<T: Read + Send + 'static>(
    pipe: T,
    thread: &'static str,
    cap: usize,
    over_cap: Arc<AtomicBool>,
) -> io::Result<mpsc::Receiver<io::Result<PipeOutput>>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    start_read_thread(thread, move || {
        let mut pipe = pipe;
        let mut bytes = Vec::new();
        let mut chunk = [0u8; GIT_DRAIN_CHUNK];
        let read = loop {
            match pipe.read(&mut chunk) {
                Ok(0) => {
                    break Ok(PipeOutput {
                        bytes,
                        over_cap: over_cap.load(Ordering::Acquire),
                    })
                }
                Ok(read) => {
                    // Never past the cap, and never re-read to find out how much of it was kept.
                    let kept = cap.saturating_sub(bytes.len()).min(read);
                    bytes.extend_from_slice(&chunk[..kept]);
                    if kept < read {
                        over_cap.store(true, Ordering::Release);
                    }
                }
                Err(error) => break Err(error),
            }
        };
        // The reading end is gone only once this process has been answered, and a reader that
        // outlives its answer has nobody left to report to.
        let _ = sender.send(read);
    })?;
    Ok(receiver)
}

/// How much of a pipe is read at a time, which is a stack block on the reader's thread and
/// nothing else: it bounds neither the answer nor the reading.
const GIT_DRAIN_CHUNK: usize = 32 * 1024;

/// Starts one of the threads a read needs, or says why it could not.
///
/// The writer that feeds a Git its list and the readers that drain what it prints are waits with
/// no deadline left to time them out with, which is why they are threads -- and either of them can
/// fail to be one, on a machine that has no threads left. That is said here instead of unwrapped
/// with `expect` because the Git they were started for is already running: a panic leaves it
/// running and unreaped, and the panic is the smaller of the two failures.
fn start_read_thread(name: &'static str, task: impl FnOnce() + Send + 'static) -> io::Result<()> {
    #[cfg(test)]
    if let Some(error) = impossible_read_thread(name) {
        return Err(error);
    }
    thread::Builder::new()
        .name(name.into())
        .spawn(task)
        .map(|_| ())
}

/// The failure a test has arranged for this thread, having first waited out its instant.
#[cfg(test)]
fn impossible_read_thread(name: &'static str) -> Option<io::Error> {
    IMPOSSIBLE_READ_THREAD.with(|impossible| {
        let impossible = impossible.borrow();
        let (unwanted, from) = (*impossible)?;
        if unwanted != name {
            return None;
        }
        // Waited out rather than refused outright: the Git this read started is a moment old, and a
        // test asserting that it is gone has to be able to ask what its process was first. A test
        // that does not care says zero and gets the failure at once.
        thread::sleep(from.saturating_duration_since(Instant::now()));
        Some(io::Error::other("no thread could be started"))
    })
}

#[cfg(test)]
thread_local! {
    /// A thread this test has made impossible to start, and the instant it is to be refused from.
    ///
    /// Per thread on purpose: the reads these tests break are on this thread, and a hook any other
    /// test's read could trip over would be a source of failures that look like nothing else.
    static IMPOSSIBLE_READ_THREAD: RefCell<Option<(&'static str, Instant)>> =
        const { RefCell::new(None) };
}

/// Holds a thread unstartable for as long as it is alive, and lets it go on drop.
#[cfg(test)]
struct ImpossibleReadThread;

#[cfg(test)]
impl ImpossibleReadThread {
    fn new(name: &'static str, after: Duration) -> Self {
        IMPOSSIBLE_READ_THREAD.with(|impossible| {
            *impossible.borrow_mut() = Some((name, Instant::now() + after));
        });
        Self
    }
}

#[cfg(test)]
impl Drop for ImpossibleReadThread {
    fn drop(&mut self) {
        IMPOSSIBLE_READ_THREAD.with(|impossible| *impossible.borrow_mut() = None);
    }
}

#[cfg(test)]
thread_local! {
    /// Set by a test to make the first status read that would report this Git's exit fail instead,
    /// which is the status read a kernel has no reason to refuse and this code has to survive.
    static BROKEN_STATUS_READ: Cell<u8> = const { Cell::new(0) };
}

/// Holds the next exit-reporting status read broken, and lets it go on drop.
#[cfg(test)]
struct BrokenStatusRead;

#[cfg(test)]
impl BrokenStatusRead {
    fn armed() -> Self {
        BROKEN_STATUS_READ.with(|broken| broken.set(1));
        Self
    }
}

#[cfg(test)]
impl Drop for BrokenStatusRead {
    fn drop(&mut self) {
        BROKEN_STATUS_READ.with(|broken| broken.set(0));
    }
}

fn pipe_read(
    pipe: mpsc::Receiver<io::Result<PipeOutput>>,
    stream: &'static str,
) -> Result<PipeOutput, PipeFailure> {
    match pipe.recv_timeout(GIT_PIPE_DRAIN) {
        // A pipe that closed with nothing on it is Git that had nothing to say, which is an answer
        // and an ordinary one: a `rev-parse` that printed no ref, a status with no changes in it.
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(PipeFailure::Read { stream, error }),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(PipeFailure::StillHeld { stream }),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(PipeFailure::ReaderGone { stream }),
    }
}

/// The error a pipe that did not give up its answer leaves behind.
///
/// What the pipe failed to carry does not travel with it: the reading failed, so there is no saying
/// what Git printed. The message names which pipe and why; it does not quote output that was not
/// successfully read.
fn git_pipe_failure(failure: &PipeFailure) -> IpcError {
    let (stream, because) = match failure {
        PipeFailure::Read { stream, error } => (*stream, error.to_string()),
        PipeFailure::StillHeld { stream } => {
            (*stream, "still held open after Git finished".to_owned())
        }
        PipeFailure::ReaderGone { stream } => {
            (*stream, "the reader stopped without an answer".to_owned())
        }
    };
    IpcError::new(
        IpcErrorCode::GitFailed,
        format!("could not read Git's {stream}: {because}"),
    )
}

pub(crate) fn git_error(action: &str, output: &Output) -> IpcError {
    git_error_with_output(action, output, &output.stdout)
}

fn git_error_without_stdout(action: &str, output: &Output) -> IpcError {
    git_error_with_output(action, output, &[])
}

fn git_error_with_output(action: &str, output: &Output, stdout: &[u8]) -> IpcError {
    let said = summarize_git_output(stdout, &output.stderr);
    let exit = output.status.code().map_or_else(
        || "terminated without an exit code".to_owned(),
        |code| format!("exit code {code}"),
    );
    let message = if said.is_empty() {
        format!("{action} ({exit}; Git returned no diagnostic output)")
    } else {
        format!("{action} ({exit}): {said}")
    };
    IpcError::new(IpcErrorCode::GitFailed, message)
}

fn output_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout)
        .trim_end_matches(['\r', '\n'])
        .to_owned()
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        collections::{BTreeMap, BTreeSet, VecDeque},
        fs, io,
        os::unix::process::ExitStatusExt,
        path::Path,
        process::{Command, ExitStatus, Output, Stdio},
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc, Barrier, Mutex,
        },
        thread,
        time::{Duration, Instant},
    };

    use notify::Watcher;
    use tempfile::tempdir;

    use crate::{
        domain::{ipc::IpcErrorCode, workspace::RepoKind},
        persistence::Database,
        services::workspace,
    };

    use super::{
        affected_checkouts, checked_git, diff, diff_page, drain_pipe, exited_unreaped,
        failed_watch_update, file_activity, forward_watch_result, git_error,
        git_error_without_stdout, git_pipe_failure, git_reads, ignored_paths,
        invalidate_failed_watch, merge_base_content, parse_diff_display_line, parse_name_status,
        parse_numstat, parse_porcelain_v2, pipe_read, receive_debounced_change,
        receive_debounced_updates, requested_watch_ids, reserved_group, resolve_default_ref,
        run_capped_git, run_command_with_input, run_command_with_timeout, scan_git_diff,
        shared_read, should_refresh_path, status, untracked_line_count, watch_failure,
        watch_plan_matches_request, without_ignored_only, CachedGitSnapshot, FileActivity,
        GitCounts, GitDiffStats, GitSnapshotCache, GitStatus, GitWatcherManager, GroupIdentity,
        IpcError, PathBudget, PathBuf, ReadingInFlight, RepoWatchPlan, WatchInbox, WatchQueue,
        WatchSlot, WatchUpdate, WatchWakeup, BINARY_SNIFF_BYTES, COUNT_BLOCK_BYTES, GIT_OUTPUT_CAP,
        MAX_SYNTAX_CONTEXT_BYTES, WATCH_PATH_BUDGET, WATCH_PATH_BYTE_BUDGET,
    };
    use super::{BrokenStatusRead, ImpossibleReadThread};

    fn failed_output(stdout: &[u8], stderr: &[u8]) -> Output {
        Output {
            status: ExitStatus::from_raw(1 << 8),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }
    }

    #[test]
    fn git_errors_prefer_stderr_and_fall_back_to_stdout() {
        let from_stderr = failed_output(b"", b"fatal: stderr reason");
        assert_eq!(
            git_error("Git query failed", &from_stderr).message,
            "Git query failed (exit code 1): fatal: stderr reason"
        );
        let from_stdout = failed_output(b"stdout fallback", b"");
        assert_eq!(
            git_error("Git query failed", &from_stdout).message,
            "Git query failed (exit code 1): stdout fallback"
        );
    }

    #[test]
    fn git_error_summary_bounds_long_unicode_diagnostics_without_redacting_paths() {
        let path = "/Users/matias/Projects/秘密/🪶.rs";
        let stderr = (0..8)
            .map(|line| format!("fatal: {path} line-{line} {}", "界".repeat(500)))
            .collect::<Vec<_>>()
            .join("\n");
        let error = git_error("Git query failed", &failed_output(b"", stderr.as_bytes()));
        let summary = error.message.split_once(": ").unwrap().1;

        assert_eq!(summary.split(" / ").count(), 3);
        assert!(summary.contains(path));
        assert!(summary.contains('界'));
        assert!(summary.split(" / ").all(|line| line.chars().count() <= 121));
    }

    #[test]
    fn empty_git_error_keeps_action_and_exit_code() {
        let error = git_error("could not list Git worktrees", &failed_output(b"", b""));

        assert_eq!(
            error.message,
            "could not list Git worktrees (exit code 1; Git returned no diagnostic output)"
        );
    }

    #[test]
    fn diff_error_does_not_include_stdout_source() {
        let output = failed_output(b"diff --git a/source.rs\n+private source", b"");

        let error = git_error_without_stdout("could not diff changed file", &output);

        assert!(error
            .message
            .contains("could not diff changed file (exit code 1"));
        assert!(!error.message.contains("private source"));
    }

    fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("Git is installed");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo(root: &Path) {
        fs::create_dir_all(root).unwrap();
        git(root, &["init", "-b", "trunk"]);
        git(root, &["config", "user.name", "Muster test"]);
        git(root, &["config", "user.email", "muster@example.invalid"]);
        fs::write(root.join("base.txt"), "base\n").unwrap();
        git(root, &["add", "base.txt"]);
        git(root, &["commit", "-m", "base"]);
    }

    #[test]
    fn malformed_hunk_headers_do_not_attach_following_lines_to_partial_hunks() {
        let mut old_line = Some(1);
        let mut new_line = Some(1);
        assert!(parse_diff_display_line(
            b"@@ -broken +2 @@ partial\n",
            &mut old_line,
            &mut new_line,
        )
        .is_none());
        assert_eq!(old_line, None);
        assert_eq!(new_line, None);
        assert!(parse_diff_display_line(
            b"+must not be assigned a line number\n",
            &mut old_line,
            &mut new_line,
        )
        .is_none());
    }

    fn git_database(temp: &Path, root: &Path) -> (Database, String) {
        let database = Database::open(temp.join("workspace.sqlite3")).unwrap();
        let state = workspace::register_folder(&database, root).unwrap();
        let checkout_id = state.repos[0].checkouts[0].id.clone();
        workspace::set_default_branch(&database, &state.repos[0].id, "trunk").unwrap();
        (database, checkout_id)
    }

    #[test]
    fn lists_feature_commits_from_merge_base_without_default_branch_commits() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        git(&root, &["checkout", "-b", "feature"]);
        fs::write(root.join("feature.txt"), "feature\n").unwrap();
        git(&root, &["add", "feature.txt"]);
        git(&root, &["commit", "-m", "feature"]);
        git(&root, &["checkout", "trunk"]);
        fs::write(root.join("default-only.txt"), "default\n").unwrap();
        git(&root, &["add", "default-only.txt"]);
        git(&root, &["commit", "-m", "new default commit"]);
        git(&root, &["checkout", "feature"]);
        fs::write(root.join("feature.txt"), "feature edited\n").unwrap();
        let (database, checkout_id) = git_database(temp.path(), &root);
        let watchers = GitWatcherManager::default();

        let snapshot = status(&database, &watchers, &checkout_id).unwrap();

        assert!(snapshot.files.iter().any(|file| file.path == "feature.txt"));
        assert_eq!(
            snapshot
                .files
                .iter()
                .filter(|file| file.path == "feature.txt")
                .count(),
            1
        );
        assert!(!snapshot
            .files
            .iter()
            .any(|file| file.path == "default-only.txt"));
        assert_eq!(snapshot.ahead_count, 1);
        assert_eq!(snapshot.default_branch, "trunk");
    }

    #[test]
    fn staged_and_unstaged_edits_and_untracked_files_are_listed_once_and_diffed_as_one_change() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        let (database, checkout_id) = git_database(temp.path(), &root);
        fs::write(root.join("base.txt"), "staged\n").unwrap();
        git(&root, &["add", "base.txt"]);
        fs::write(root.join("base.txt"), "unstaged final\n").unwrap();
        fs::write(root.join("new file.txt"), "untracked\n").unwrap();
        fs::write(root.join("new binary.dat"), [0, 1, 2]).unwrap();
        let watchers = GitWatcherManager::default();

        let snapshot = status(&database, &watchers, &checkout_id).unwrap();
        let paths: Vec<_> = snapshot
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(paths, ["base.txt", "new binary.dat", "new file.txt"]);

        let tracked = diff(&database, &checkout_id, "base.txt").unwrap();
        assert!(tracked.patch.contains("+unstaged final"));
        assert!(!tracked.patch.contains("+staged\n"));
        assert_eq!(tracked.patch.matches("diff --git").count(), 1);
        let untracked = diff(&database, &checkout_id, "new file.txt").unwrap();
        assert!(untracked.patch.contains("+untracked"));
        let binary = diff(&database, &checkout_id, "new binary.dat").unwrap();
        assert!(binary.is_binary);
        assert!(binary.patch.is_empty());
    }

    #[test]
    fn sec_03_git_keeps_shell_metacharacters_and_newlines_in_a_single_path_argument() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        let filename = "space 'quote' \"double\" ; $(touch pwned)
line.txt";
        fs::write(root.join(filename), "literal file content\n").unwrap();
        let (database, checkout_id) = git_database(temp.path(), &root);
        let watchers = GitWatcherManager::default();

        let status = status(&database, &watchers, &checkout_id).unwrap();
        assert!(status.files.iter().any(|file| file.path == filename));
        let diff = diff(&database, &checkout_id, filename).unwrap();

        assert!(diff.patch.contains("+literal file content"));
        assert!(!temp.path().join("pwned").exists());
    }

    #[test]
    fn reports_binary_changes_without_returning_binary_patch_text() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        fs::write(root.join("binary.dat"), [0, 1, 2]).unwrap();
        git(&root, &["add", "binary.dat"]);
        git(&root, &["commit", "-m", "add binary"]);
        let (database, checkout_id) = git_database(temp.path(), &root);
        fs::write(root.join("binary.dat"), [0, 2, 3]).unwrap();

        let result = diff(&database, &checkout_id, "binary.dat").unwrap();

        assert!(result.is_binary);
        assert!(result.patch.is_empty());
    }

    #[test]
    fn large_diff_pages_are_bounded_and_reach_the_last_changed_line() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        let original = (0..5000)
            .map(|line| format!("old-{line}\n"))
            .collect::<String>();
        fs::write(root.join("large.txt"), original).unwrap();
        git(&root, &["add", "large.txt"]);
        git(&root, &["commit", "-m", "large file"]);
        let (database, checkout_id) = git_database(temp.path(), &root);
        let changed = (0..5000)
            .map(|line| format!("new-{line}\n"))
            .collect::<String>();
        fs::write(root.join("large.txt"), changed).unwrap();

        let result = diff(&database, &checkout_id, "large.txt").unwrap();

        assert!(result.large);
        assert!(!result.too_large);
        assert!(result.total_lines > 4000);
        assert!(result.patch.is_empty());

        let offset = result.total_lines - 8;
        let page = diff_page(&database, &checkout_id, "large.txt", offset, 8).unwrap();
        assert_eq!(page.start_line, offset);
        assert_eq!(page.lines.len(), 8);
        assert!(page.lines.iter().all(|line| line.index >= offset));
        assert!(page.lines.iter().any(|line| line.text == "+new-4999"));
    }

    #[test]
    fn viewed_state_is_scoped_to_registered_changed_files() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        let (database, checkout_id) = git_database(temp.path(), &root);
        fs::write(root.join("base.txt"), "changed\n").unwrap();

        super::mark_viewed(&database, &checkout_id, "base.txt").unwrap();
        assert_eq!(
            super::viewed_files(&database, &checkout_id).unwrap(),
            ["base.txt"]
        );
        assert!(super::mark_viewed(&database, &checkout_id, "not-changed.txt").is_err());
        assert!(super::mark_viewed(&database, &checkout_id, "../outside.txt").is_err());
        assert!(super::viewed_files(&database, "checkout:unknown").is_err());
        assert!(diff_page(&database, &checkout_id, "base.txt", 0, 33).is_err());
    }

    #[test]
    fn deleted_nested_files_still_have_diffs() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        fs::create_dir(root.join("nested")).unwrap();
        fs::write(root.join("nested/removed.txt"), "remove me\n").unwrap();
        git(&root, &["add", "nested/removed.txt"]);
        git(&root, &["commit", "-m", "nested file"]);
        let (database, checkout_id) = git_database(temp.path(), &root);
        fs::remove_dir_all(root.join("nested")).unwrap();

        let result = diff(&database, &checkout_id, "nested/removed.txt").unwrap();

        assert!(result.patch.contains("-remove me"));
    }

    #[test]
    fn a_diff_carries_the_whole_text_of_each_of_its_sides() {
        // A grammar reads a file, not the hunks of a diff: the lines inside the `<script>` of a Vue
        // file are markup to a grammar that was never shown the tag that opened them. So each side
        // travels whole, and each is read the way `diff_args` diffed it — the merge base's blob and
        // the file as it is on disk.
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        let before = "<template>\n  <p>one</p>\n</template>\n\n<script setup lang=\"ts\">\nconst n = 1;\n</script>\n";
        let after = "<template>\n  <p>two</p>\n</template>\n\n<script setup lang=\"ts\">\nconst n = 2;\n</script>\n";
        fs::write(root.join("Widget.vue"), before).unwrap();
        git(&root, &["add", "Widget.vue"]);
        git(&root, &["commit", "-m", "widget"]);
        let (database, checkout_id) = git_database(temp.path(), &root);
        fs::write(root.join("Widget.vue"), after).unwrap();

        let result = diff(&database, &checkout_id, "Widget.vue").unwrap();

        assert_eq!(result.old_content.as_deref(), Some(before));
        assert_eq!(result.new_content.as_deref(), Some(after));
    }

    #[test]
    fn an_added_file_carries_no_old_text_and_a_removed_one_no_new() {
        // Each side the diff does not have is absent rather than empty: an added file has no old side
        // and a removed one no new side, and text that is not there is not something to read.
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        let gone = "const removed = 1;\n";
        fs::write(root.join("gone.ts"), gone).unwrap();
        git(&root, &["add", "gone.ts"]);
        git(&root, &["commit", "-m", "gone"]);
        let (database, checkout_id) = git_database(temp.path(), &root);
        fs::write(root.join("added.ts"), "const added = 1;\n").unwrap();
        fs::remove_file(root.join("gone.ts")).unwrap();

        let added = diff(&database, &checkout_id, "added.ts").unwrap();
        let removed = diff(&database, &checkout_id, "gone.ts").unwrap();

        assert_eq!(added.old_content, None);
        assert_eq!(added.new_content.as_deref(), Some("const added = 1;\n"));
        assert_eq!(removed.old_content.as_deref(), Some(gone));
        assert_eq!(removed.new_content, None);
    }

    #[test]
    fn a_renamed_files_old_text_is_the_path_the_rename_came_from() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        fs::create_dir_all(root.join("before")).unwrap();
        let original = (0..40)
            .map(|line| format!("const value{line} = {line};\n"))
            .collect::<String>();
        fs::write(root.join("before/View.ts"), &original).unwrap();
        git(&root, &["add", "before/View.ts"]);
        git(&root, &["commit", "-m", "view"]);
        let (database, checkout_id) = git_database(temp.path(), &root);
        fs::create_dir_all(root.join("after")).unwrap();
        git(&root, &["mv", "before/View.ts", "after/View.ts"]);
        let renamed = original.replace("const value20 = 20;\n", "const value20 = 21;\n");
        fs::write(root.join("after/View.ts"), &renamed).unwrap();

        let result = diff(&database, &checkout_id, "after/View.ts").unwrap();

        assert_eq!(result.old_content.as_deref(), Some(original.as_str()));
        assert_eq!(result.new_content.as_deref(), Some(renamed.as_str()));
    }

    #[test]
    fn no_text_travels_with_a_binary_diff_or_with_a_file_past_the_cap() {
        // There is nothing to read in the first case, and in the second there is more of it than a
        // diff is worth carrying: both are absent, which is what leaves the diff to draw itself.
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        fs::write(root.join("bytes.dat"), [0, 1, 2]).unwrap();
        git(&root, &["add", "bytes.dat"]);
        git(&root, &["commit", "-m", "bytes"]);
        let big = "line\n".repeat(MAX_SYNTAX_CONTEXT_BYTES / 5 + 1);
        fs::write(root.join("big.txt"), &big).unwrap();
        git(&root, &["add", "big.txt"]);
        git(&root, &["commit", "-m", "big"]);
        let (database, checkout_id) = git_database(temp.path(), &root);
        fs::write(root.join("bytes.dat"), [0, 2, 3]).unwrap();
        fs::write(root.join("big.txt"), big.replacen("line\n", "LINE\n", 1)).unwrap();

        let binary = diff(&database, &checkout_id, "bytes.dat").unwrap();
        let large = diff(&database, &checkout_id, "big.txt").unwrap();

        assert_eq!((binary.old_content, binary.new_content), (None, None));
        assert!(!binary.is_binary || binary.patch.is_empty());
        // The change is small, so the patch is still drawn: it is the file's text that does not fit.
        assert!(!large.patch.is_empty());
        assert_eq!((large.old_content, large.new_content), (None, None));
    }

    #[test]
    fn plain_folders_do_not_run_git_and_changes_are_rejected() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("plain");
        fs::create_dir_all(&root).unwrap();
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        let state = workspace::register_folder(&database, &root).unwrap();
        assert_eq!(state.repos[0].kind, RepoKind::Plain);

        let watchers = GitWatcherManager::default();
        let error = status(&database, &watchers, &state.repos[0].checkouts[0].id).unwrap_err();

        assert!(matches!(error.code, IpcErrorCode::InvalidCheckout));
    }

    #[test]
    fn resolves_only_a_valid_persisted_branch_or_origin_head() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);

        let unknown = resolve_default_ref(&root, None, None).unwrap_err();
        assert!(matches!(unknown.code, IpcErrorCode::DefaultBranchUnknown));

        git(&root, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
        git(
            &root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/trunk",
            ],
        );
        let resolved = resolve_default_ref(&root, None, None).unwrap();

        assert_eq!(resolved.branch, "trunk");
        assert_eq!(resolved.reference, "refs/remotes/origin/trunk");
    }

    #[test]
    fn porcelain_v2_parses_unusual_names_and_renames_without_splitting_paths() {
        let parsed = parse_porcelain_v2(
            b"1 .M N... 100644 100644 100644 abc def file with spaces.txt\0? new\nline.txt\0",
        )
        .unwrap();
        assert!(parsed.contains_key("file with spaces.txt"));
        assert!(parsed.contains_key("new\nline.txt"));

        let rename = parse_name_status(b"R100\0old name.txt\0new name.txt\0").unwrap();
        assert_eq!(rename[0].old_path.as_deref(), Some("old name.txt"));
        assert_eq!(rename[0].path, "new name.txt");
    }

    fn touched(ids: &[&str]) -> WatchUpdate {
        watch_update(ids, &[], &[])
    }

    fn watch_update(status: &[&str], activity: &[&str], worktrees: &[&str]) -> WatchUpdate {
        WatchUpdate {
            status: status.iter().map(|id| (*id).to_owned()).collect(),
            activity: activity.iter().map(|id| (*id).to_owned()).collect(),
            worktrees: worktrees.iter().map(|id| (*id).to_owned()).collect(),
            ..WatchUpdate::default()
        }
    }

    #[test]
    fn watcher_debounces_a_burst_into_one_signal_naming_every_checkout_it_touched() {
        let (queue, inbox) = WatchQueue::new();
        queue.changed(touched(&["a"]));
        queue.changed(touched(&["b"]));
        queue.changed(touched(&["a"]));

        // One save rewrites a file and its lock, and a commit moves a ref and an index. Those
        // are separate filesystem events describing one change, and answering them apart would
        // refresh the same rows twice. A checkout named twice is still one row.
        assert_eq!(
            receive_debounced_change(&inbox, Duration::from_millis(1), Duration::from_secs(1),),
            WatchWakeup::Changed(touched(&["a", "b"]))
        );
        assert!(inbox.try_recv().is_err());
    }

    /// The ordering the whole slot rests on, which a burst cannot show: a wakeup offered before the
    /// change it answers is merged is a worker told to look at a slot that does not have that change
    /// yet, and the change waits there for the next event to announce it.
    ///
    /// Holding the slot is what a worker reading it looks like from the callback's side, and it is
    /// enough to tell the two orders apart: with the change merged first, the callback cannot reach
    /// the wakeup at all while this thread holds the slot.
    #[test]
    fn a_wakeup_is_offered_only_after_the_change_it_answers_is_in_the_slot() {
        let (queue, inbox) = WatchQueue::new();
        let slot = queue.merged.clone();
        // A slot held by a thread of its own: held by this one, the callback below and the reading
        // below would be the same thread, and the reading would wait on what this one is holding.
        let holding = slot.clone();
        let (held_tx, held_rx) = mpsc::channel();
        let holder = thread::spawn(move || {
            let guard = holding.lock().expect("the slot");
            let _ = held_tx.send(());
            thread::sleep(Duration::from_millis(300));
            drop(guard);
        });
        held_rx.recv().expect("the slot to be held");
        let offered = thread::spawn(move || queue.changed(touched(&["late"])));

        thread::sleep(Duration::from_millis(100));
        assert!(
            matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "a wakeup was offered before the change it answers was merged"
        );
        holder.join().expect("the slot to be released");
        offered.join().expect("the callback finished");

        assert_eq!(
            inbox.try_recv().expect("the change answered"),
            WatchWakeup::Changed(touched(&["late"]))
        );
    }

    /// The plan a burst is read against: two worktrees over one shared Git directory, so the events
    /// below move one row, both rows and the list of worktrees between them.
    fn two_worktree_plan() -> RepoWatchPlan {
        RepoWatchPlan {
            repo_id: "repo".to_owned(),
            roots: [
                (PathBuf::from("/repo"), "main".to_owned()),
                (PathBuf::from("/repo-task"), "task".to_owned()),
            ]
            .into_iter()
            .collect(),
            git_dirs: BTreeMap::from([(
                PathBuf::from("/repo/.git/worktrees/task"),
                "task".to_owned(),
            )]),
            common_dir: Some(PathBuf::from("/repo/.git")),
            all: vec!["main".to_owned(), "task".to_owned()],
            requested: vec!["main".to_owned(), "task".to_owned()],
        }
    }

    fn event(path: &str) -> notify::Event {
        notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![PathBuf::from(path)],
            attrs: Default::default(),
        }
    }

    /// A build and a checkout: a long burst that touches a file in each worktree, moves a ref every
    /// worktree counts against and registers a worktree, and repeats all of it.
    fn burst_events(times: usize) -> Vec<notify::Event> {
        (0..times)
            .flat_map(|_| {
                [
                    event("/repo/built-a.txt"),
                    event("/repo-task/built-b.txt"),
                    event("/repo/.git/refs/heads/trunk"),
                    event("/repo-task/.git/worktrees/task/index"),
                    event("/repo/.git/worktrees/joined/gitdir"),
                ]
            })
            .collect()
    }

    #[test]
    fn a_burst_of_thousands_of_events_merges_into_the_checkouts_it_merged_before() {
        let plan = two_worktree_plan();
        let events = burst_events(1_000);

        // What one message per event used to produce: the same `merge_update` the worker used to run
        // over the queue it read, run here over the events themselves. The answer must be this one.
        let mut merged_before = WatchSlot::holding(WatchUpdate::default());
        for event in &events {
            merged_before.merge_update(
                affected_checkouts(&plan, event).expect("the plan describes these paths"),
            );
        }
        let (queue, inbox) = WatchQueue::new();
        for event in &events {
            queue
                .changed(affected_checkouts(&plan, event).expect("the plan describes these paths"));
        }

        // One answer, and the queue behind it never held more than that one wakeup: the burst of
        // five thousand events is still the answer the queue of five thousand messages gave.
        let delivered =
            receive_debounced_change(&inbox, Duration::from_millis(1), Duration::from_secs(1));
        assert_eq!(delivered, WatchWakeup::Changed(merged_before.take()));
        let WatchWakeup::Changed(answer) = delivered else {
            panic!("the burst was not answered as a change: {delivered:?}");
        };
        assert_eq!(
            answer.status,
            vec!["main".to_owned(), "task".to_owned()],
            "the burst must still name both rows a shared ref moves"
        );
        assert_eq!(answer.worktrees, vec!["repo".to_owned()]);
        assert!(inbox.try_recv().is_err());
    }

    /// The worker thread of a watcher, read the way it is written in production: on until the signal
    /// that ends it. `None` is the deadline passing with no terminal signal in sight, which is what a
    /// retirement that was never delivered looks like from here.
    fn read_worker_thread(inbox: &WatchInbox, deadline: Instant) -> Option<WatchWakeup> {
        loop {
            if Instant::now() >= deadline {
                return None;
            }
            if let terminal @ (WatchWakeup::Stop | WatchWakeup::Failed(_)) =
                receive_debounced_change(inbox, Duration::from_millis(1), Duration::from_millis(50))
            {
                return Some(terminal);
            }
        }
    }

    #[test]
    fn a_stop_sent_while_the_queue_is_saturated_still_ends_the_watcher_thread() {
        let (queue, inbox) = WatchQueue::new();
        let retired = queue.clone();
        let producer = thread::spawn(move || {
            for burst in 0..2_000 {
                let id = format!("checkout:{}", burst % 40);
                retired.changed(touched(&[&id]));
            }
            retired.stop();
        });

        // Nothing is reading yet, so the queue is still holding its one wakeup with two thousand
        // events merged behind it. The stop cannot be there until it is read, which is what makes it
        // a signal that was kept rather than one that was dropped.
        thread::sleep(Duration::from_millis(200));
        assert!(
            !producer.is_finished(),
            "a stop behind a full queue was dropped instead of waiting for the worker"
        );

        // A retired watcher has to end its thread whatever the queue is doing: the burst behind the
        // stop is dropped with it, which is what retiring means, but the stop itself must arrive or
        // the thread and its watch descriptors stay alive for the rest of the session.
        let terminal = read_worker_thread(&inbox, Instant::now() + Duration::from_secs(10));
        assert_eq!(
            terminal,
            Some(WatchWakeup::Stop),
            "a stop behind a full queue never reached the worker"
        );
        producer
            .join()
            .expect("the retired watcher finished sending");
        // The queue is still held here, so what the worker read was the signal and not the sender
        // going away.
        assert!(
            inbox.try_recv().is_err(),
            "the worker ended on the queue rather than on the stop"
        );
    }

    #[test]
    fn a_failure_sent_while_the_queue_is_saturated_is_still_answered() {
        let (queue, inbox) = WatchQueue::new();
        for burst in 0..2_000 {
            let id = format!("checkout:{}", burst % 40);
            queue.changed(touched(&[&id]));
        }
        let failed = queue.clone();
        let reporter = thread::spawn(move || failed.failed("watch stream failed".to_owned()));

        // The queue is full and nobody is reading, so the failure cannot be reported yet. It has to
        // be waiting rather than gone: a watch stream that died has to be heard about even while a
        // build is filling the queue, because the watcher that reported it is gone and one that
        // stayed silent about its own death keeps looking alive, holding a snapshot nothing can
        // refresh.
        thread::sleep(Duration::from_millis(200));
        assert!(
            !reporter.is_finished(),
            "a failure behind a full queue was dropped instead of waiting for the worker"
        );

        let terminal = read_worker_thread(&inbox, Instant::now() + Duration::from_secs(10));
        assert_eq!(
            terminal,
            Some(WatchWakeup::Failed("watch stream failed".to_owned())),
            "a failure behind a full queue never reached the worker"
        );
        assert!(
            reporter.join().expect("the failed watcher reported"),
            "there was no worker left to report to"
        );
    }

    #[test]
    fn a_git_read_that_never_finishes_fails_after_the_deadline_and_is_collected() {
        let temp = tempdir().unwrap();
        let git_pid = temp.path().join("git-pid");
        let helper_pid = temp.path().join("helper-pid");
        // A Git that hangs the way a real one does: it says why on its way out, then waits on a
        // helper nothing is going to answer, which is the pair of processes one deadline has to end.
        let script = format!(
            "echo $$ > \"{git}\"; echo \"fatal: {}\" >&2; sleep 30 & echo $! > \"{helper}\"; wait",
            "cannot lock ref 'refs/heads/trunk'".repeat(8),
            git = git_pid.display(),
            helper = helper_pid.display()
        );

        let started = Instant::now();
        let error = run_command_with_timeout(
            "sh",
            temp.path(),
            vec!["-c".into(), script.into()],
            Duration::from_millis(300),
        )
        .expect_err("a Git that never finishes cannot answer");

        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the deadline did not end the read"
        );
        assert_eq!(error.code, IpcErrorCode::GitFailed);
        // What Git said is the diagnosis, under the summary limit. This limit bounds the message; it
        // does not redact paths.
        assert!(
            error.message.starts_with("Git stopped reading after 0.3s"),
            "{error:?}"
        );
        assert!(error.message.contains("cannot lock ref"), "{error:?}");
        assert!(
            error.message.chars().count() < 200,
            "Git's own words were not cut down: {}",
            error.message
        );

        // Neither process is left behind. The Git itself because a stopped read that skipped the
        // wait would leave a zombie per hung read, and the helper because a deadline that ended only
        // the process it started would leave the pipes the output is collected from held open.
        let deadline = Instant::now() + Duration::from_secs(5);
        let pids = [read_pid(&git_pid), read_pid(&helper_pid)];
        while pids.iter().any(|pid| process_exists(*pid)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        for pid in pids {
            assert!(
                !process_exists(pid),
                "the stopped Git left process {pid} behind"
            );
        }
    }

    /// What the deadline replaced was a runner that already told Git not to take the optional
    /// locks, and that answer has to survive the rewrite: the index of a linked worktree lives
    /// inside the directory the watcher is watching, so a `git status` that refreshed it would
    /// announce itself and ask for the refresh that wrote it, for as long as the app runs.
    #[test]
    fn a_read_still_tells_git_to_leave_the_optional_locks_alone() {
        let temp = tempdir().unwrap();
        let output = run_command_with_timeout(
            "sh",
            temp.path(),
            vec!["-c".into(), "printf %s \"$GIT_OPTIONAL_LOCKS\"".into()],
            Duration::from_secs(5),
        )
        .expect("the read ran");

        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"0");
    }

    /// The read that decides whether a folder is a plain one compares Git's own wording, and a
    /// translated Git does not say the words being compared.
    ///
    /// The environment a read runs Git under is pinned rather than inherited, so this holds whatever
    /// locale the machine (or the person using it) is set to: what the child is told is `C`, and
    /// `LANGUAGE` is dropped because gettext reads it ahead of `LC_ALL` and it would win otherwise.
    /// Nothing global is set to achieve it -- a user's own locale still governs everything they read.
    #[test]
    fn a_read_pins_the_language_git_answers_in_without_touching_the_process() {
        let temp = tempdir().unwrap();
        // What this process had, so the assertion below is about the child and not about a machine
        // that happens to run in English.
        let ambient = std::env::var("LANGUAGE").ok();
        let output = run_command_with_timeout(
            "sh",
            temp.path(),
            vec![
                "-c".into(),
                "printf '%s|%s' \"$LC_ALL\" \"${LANGUAGE-unset}\"".into(),
            ],
            Duration::from_secs(5),
        )
        .expect("the read ran");

        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"C|unset");
        assert_eq!(std::env::var("LANGUAGE").ok(), ambient);
    }

    fn read_pid(path: &Path) -> libc::pid_t {
        fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("the hanging Git recorded its process: {error}"))
            .trim()
            .parse()
            .expect("a process id")
    }

    fn process_exists(pid: libc::pid_t) -> bool {
        // A process that has been waited for is gone; one that was only killed is still in the
        // process table as a zombie and still answers this.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// An empty pipe is Git saying nothing, which is an answer and an ordinary one. It is the answer
    /// the drain must not stop giving, because it is the same bytes a pipe that could not be read
    /// would have handed on.
    #[test]
    fn a_git_that_prints_nothing_is_a_read_that_answered() {
        let temp = tempdir().unwrap();

        let output = run_command_with_timeout(
            "sh",
            temp.path(),
            vec!["-c".into(), "exit 0".into()],
            Duration::from_secs(10),
        )
        .expect("a Git that printed nothing still answered");

        assert!(output.status.success(), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
    }

    #[test]
    fn a_git_that_finished_with_its_pipe_still_held_is_a_failed_read_and_leaves_nothing() {
        // What the drain deadline had no answer for: Git finished inside its own deadline and says
        // it worked, while a helper it started is still holding the write end. What came back from
        // here was an exit status with nothing in either pipe, which every caller above reads as a
        // Git that had nothing to say -- in the changes panel, a checkout with no changes in it,
        // reported with the failure nowhere in sight.
        let temp = tempdir().unwrap();
        let git_pid = temp.path().join("git-pid");
        let helper_pid = temp.path().join("helper-pid");
        let script = format!(
            "echo $$ > \"{git}\"; sleep 30 & echo $! > \"{helper}\"; \
             echo \"fatal: cannot read /private/nowhere/secret.txt\" >&2; exit 0",
            git = git_pid.display(),
            helper = helper_pid.display()
        );

        let started = Instant::now();
        let error = run_command_with_timeout(
            "sh",
            temp.path(),
            vec!["-c".into(), script.into()],
            Duration::from_secs(10),
        )
        .expect_err("a read that could not finish is not an empty read");

        assert_eq!(error.code, IpcErrorCode::GitFailed);
        assert!(
            error
                .message
                .starts_with("could not read Git's stdout: still held open"),
            "{error:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the drain deadline did not end the read"
        );
        // What Git said on its way out does not travel with this one either: the reading failed, so
        // there is no vouching for what was in it, and a path has no business in a message the app
        // logs and shows.
        assert!(!error.message.contains("nowhere"), "{error:?}");
        assert!(!error.message.contains("secret.txt"), "{error:?}");
        assert!(
            !error.message.contains(&temp.path().display().to_string()),
            "{error:?}"
        );
        // Both, for the reason `stop_git_read` gives: the helper because it is the one holding the
        // pipe that could not be read and it was still in the group Git led, and the Git because
        // ending that group and collecting the leader is the same operation here.
        assert_processes_gone(&[read_pid(&git_pid), read_pid(&helper_pid)]);
    }

    /// The exit is watched and not taken away, which is what keeps the number a signal is aimed at
    /// still reserved.
    ///
    /// `try_wait` answers the same question by reaping, and a reaped pid is a number the kernel
    /// hands to the next process that asks for one. A watched Git can be asked again; a collected
    /// one cannot be asked about, and neither can the group it led be named afterwards.
    #[test]
    fn a_finished_git_is_watched_and_not_collected() {
        // The shell holds its exit until the reading end is closed, so "has not finished" is a
        // fact here rather than a race with a shell that is free to exit before the first ask.
        let mut child = Command::new("sh")
            .args(["-c", "read line; exit 0"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("a shell to watch");
        assert!(
            !exited_unreaped(&child).expect("a running Git can be watched"),
            "a Git that has not finished is not an exit"
        );
        drop(child.stdin.take());

        let deadline = Instant::now() + Duration::from_secs(5);
        while !exited_unreaped(&child).expect("a running Git can be watched") {
            assert!(Instant::now() < deadline, "the shell never exited");
            thread::sleep(Duration::from_millis(10));
        }
        // Watched, not collected: the question has the same answer the second time, which is what
        // a child that was reaped the first time could not say.
        assert!(
            exited_unreaped(&child).expect("an exited Git can still be watched"),
            "an exit reported once was collected on the way out"
        );
        assert_eq!(
            child.wait().expect("a collected Git").code(),
            Some(0),
            "collecting a watched exit still hands back the exit"
        );
    }

    /// A pipe held open proves a writer and not which group the writer is in.
    ///
    /// Job control gives the helper a group of its own, which is the part of `setsid` that matters
    /// to a signal: the writer is holding the pipe and is not in the group the read would aim at.
    /// What must not follow from that is a wider aim -- the leader's number belongs to the kernel
    /// once the leader is collected, so the group is ended while the leader is still there to hold
    /// it, and the writer that left is left.
    ///
    /// This is also where the reader thread left behind by that writer is asserted not to have
    /// turned into a signal aimed at it: the read gives up on the pipe after its drain deadline, and
    /// what it must not do to end with the writer holding it is close the reading end, which would
    /// hand the same unverified writer an `EPIPE` and then a `SIGPIPE`. What it leaves instead is
    /// described in `drain_pipe`: the thread stays on the pipe until that writer is gone, which is
    /// the price of not guessing, and it is paid without the read itself hanging on it.
    #[test]
    fn a_helper_that_left_the_group_is_left_rather_than_signalled() {
        let temp = tempdir().unwrap();
        let git_pid = temp.path().join("git-pid");
        let helper_pid = temp.path().join("helper-pid");
        let script = format!(
            "set -m; echo $$ > \"{git}\"; trap '' HUP; sleep 30 & echo $! > \"{helper}\"; exit 0",
            git = git_pid.display(),
            helper = helper_pid.display()
        );

        let started = Instant::now();
        let error = run_command_with_timeout(
            "sh",
            temp.path(),
            vec!["-c".into(), script.into()],
            Duration::from_secs(10),
        )
        .expect_err("a read that could not finish is not an empty read");

        // The same failed read as a helper that stayed in the group: a writer on the other end of
        // a pipe nobody could finish reading, reported rather than handed on as an empty answer.
        assert_eq!(error.code, IpcErrorCode::GitFailed);
        assert!(
            error
                .message
                .starts_with("could not read Git's stdout: still held open"),
            "{error:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the drain deadline did not end the read"
        );

        let (git, helper) = (read_pid(&git_pid), read_pid(&helper_pid));
        // The leader was collected rather than left as a zombie, which is the whole reason the exit
        // above was watched instead of taken.
        assert_processes_gone(&[git]);
        assert!(
            process_exists(helper),
            "a writer that was never in the group was signalled through a number that \
             proves nothing about it"
        );
        unsafe { libc::kill(helper, libc::SIGKILL) };
    }

    /// A read that failed on the way cannot be forced on a live pipe from here -- it would take a
    /// broken descriptor under a running Git -- so what is under test is the seam it arrives
    /// through: what the drain thread sends, and the error that becomes.
    #[test]
    fn a_pipe_that_failed_to_read_is_an_error_and_not_an_empty_read() {
        let (sender, receiver) = mpsc::sync_channel(1);
        sender
            .send(Err(io::Error::other("the pipe went away")))
            .expect("a channel with room for the reader's answer");

        let failure = pipe_read(receiver, "stdout")
            .expect_err("a pipe that could not be read did not come back as no output");

        assert!(!failure.writer_left(), "{failure:?}");
        let error = git_pipe_failure(&failure);
        assert_eq!(error.code, IpcErrorCode::GitFailed);
        assert!(
            error.message.contains("the pipe went away"),
            "the reason the read failed is the diagnosis: {error:?}"
        );
    }

    #[test]
    fn a_drain_thread_that_stopped_without_an_answer_is_not_an_empty_read() {
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(sender);

        let failure = pipe_read(receiver, "stderr")
            .expect_err("a reader that died with nothing to say did not come back as no output");

        let error = git_pipe_failure(&failure);
        assert_eq!(error.code, IpcErrorCode::GitFailed);
        assert!(
            error
                .message
                .starts_with("could not read Git's stderr: the reader stopped"),
            "{error:?}"
        );
        assert!(!failure.writer_left(), "{failure:?}");
    }

    /// The exit status is still the failure when the output was read whole: the new errors are about
    /// an answer that never arrived, and a Git that answered and failed has failed the way it
    /// always did.
    #[test]
    fn a_git_that_really_failed_still_fails_with_its_own_words() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);

        let error = checked_git(
            &root,
            ["rev-parse", "--verify", "refs/heads/nothing"],
            "read the ref",
        )
        .expect_err("a ref that is not there is not a ref");

        assert_eq!(error.code, IpcErrorCode::GitFailed);
        assert!(
            error.message.starts_with("read the ref (exit code "),
            "{error:?}"
        );
        assert!(
            error.message.contains("fatal: Needed a single revision"),
            "{error:?}"
        );
        assert!(
            !error.message.contains("could not read Git's"),
            "a Git that answered did not fail to read: {error:?}"
        );
    }

    /// The threads a read needs can fail to start, and the Git they were started for is running
    /// already. The failure is reported and the child is ended here; before, the writer's was `?`
    /// and the readers' was a panic, so a read that could not start one of them returned or
    /// unwound with the child behind it.
    #[test]
    fn a_git_whose_list_could_not_be_fed_is_stopped_and_collected() {
        let temp = tempdir().unwrap();
        let git_pid = temp.path().join("git-pid");
        // Reads the list and then stays up, so the only thing that can end it is this read: the
        // write end is dropped with the thread that never started, so `cat` sees the end of a list
        // that never came.
        let script = format!(
            "echo $$ > \"{}\"; cat > /dev/null; sleep 30",
            git_pid.display()
        );
        let _writer = ImpossibleReadThread::new("muster-git-input", Duration::from_millis(300));

        let started = Instant::now();
        let error = run_command_with_input(
            "sh",
            temp.path(),
            ["-c", &script, ""],
            Duration::from_secs(10),
            b"build/a.js\0",
        )
        .expect_err("a Git that was never fed a list did not read one");

        assert_eq!(error.code, IpcErrorCode::GitFailed);
        assert!(
            error.message.starts_with("could not feed Git:"),
            "the reason it could not be fed is the diagnosis: {error:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the unwritable Git was not ended: {:?}",
            started.elapsed()
        );
        assert_processes_gone(&[read_pid(&git_pid)]);
    }

    /// The same for a reader, and both of them: `GitPipes::take` asks for the two in order, so a
    /// refusal has to be able to arrive on either. A panic on the second one would unwind with the
    /// first still running and both pipes half drained.
    #[test]
    fn a_git_whose_output_could_not_be_drained_is_stopped_and_collected() {
        let temp = tempdir().unwrap();
        let git_pid = temp.path().join("git-pid");
        let script = format!("echo $$ > \"{}\"; sleep 30", git_pid.display());

        for reader in ["muster-git-stdout", "muster-git-stderr"] {
            let _reader = ImpossibleReadThread::new(reader, Duration::from_millis(300));
            let started = Instant::now();
            let error = run_command_with_timeout(
                "sh",
                temp.path(),
                vec!["-c".into(), script.clone().into()],
                Duration::from_secs(10),
            )
            .expect_err("a Git nobody was reading was not answered");

            assert_eq!(error.code, IpcErrorCode::GitFailed);
            assert!(
                error.message.starts_with("could not read Git's output:"),
                "{reader}: {error:?}"
            );
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "{reader}: the unreadable Git was not ended: {:?}",
                started.elapsed()
            );
            assert_processes_gone_named(&[read_pid(&git_pid)], reader);
        }
    }

    /// `run_capped_git` drains on the same thread and for the same reason, so the same refusal ends
    /// its Git rather than leaving it writing into a pipe that will never be read again.
    #[test]
    fn a_blob_whose_reader_could_not_be_started_is_nothing_and_leaves_nothing_running() {
        let temp = tempdir().unwrap();
        let git_pid = temp.path().join("git-pid");
        let script = format!("echo $$ > \"{}\"; sleep 30", git_pid.display());
        let _reader = ImpossibleReadThread::new("muster-git-stdout", Duration::from_millis(300));

        let read = run_capped_git(
            "sh",
            temp.path(),
            ["-c", &script],
            MAX_SYNTAX_CONTEXT_BYTES,
            Duration::from_secs(10),
        );

        assert_eq!(read, None, "a blob nobody could read is not text to draw");
        assert_processes_gone(&[read_pid(&git_pid)]);
    }

    /// The status read that fails is the one this code cannot ask again, so the read has to end the
    /// Git and collect it rather than keep waiting for an answer it will never have. And because the
    /// leader is still uncollected at that point, the group behind its number is still this read's
    /// own: the helper holding the pipe is ended with it, and nothing that is not in that group is
    /// signalled at all.
    #[test]
    fn a_status_read_that_fails_ends_the_whole_group_it_started_and_nothing_else() {
        let temp = tempdir().unwrap();
        let git_pid = temp.path().join("git-pid");
        let helper_pid = temp.path().join("helper-pid");
        let script = format!(
            "echo $$ > \"{git}\"; sleep 30 & echo $! > \"{helper}\"; exit 0",
            git = git_pid.display(),
            helper = helper_pid.display()
        );
        // A process of its own, outside every group of this read, as the one thing a signal aimed at
        // the wrong number would show up on.
        let mut bystander = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("a bystander to watch");
        let _broken = BrokenStatusRead::armed();

        let started = Instant::now();
        let error = run_command_with_timeout(
            "sh",
            temp.path(),
            vec!["-c".into(), script.into()],
            Duration::from_secs(10),
        )
        .expect_err("a read whose status could not be read is not a read that answered");

        assert_eq!(error.code, IpcErrorCode::GitFailed);
        assert!(
            error
                .message
                .starts_with("could not read from Git: the process status could not be read"),
            "{error:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the unreadable status did not end the read: {:?}",
            started.elapsed()
        );
        // Both, for the reason `stop_git_read` gives: the helper because it is what still holds the
        // pipe, and the leader because ending that group and collecting the leader are one operation.
        assert_processes_gone(&[read_pid(&git_pid), read_pid(&helper_pid)]);
        assert!(
            process_exists(bystander.id() as libc::pid_t),
            "a signal aimed at a number the kernel had given away would land here"
        );
        let _ = bystander.kill();
        let _ = bystander.wait();
    }

    /// A collection that already happened leaves a number the kernel is free to hand to somebody
    /// else, so the group behind it is not this read's to signal. Only the process `spawn`
    /// returned is ended there, and this is the pair of answers that decides it: a leader the
    /// kernel still holds is a reserved number, and a status read that fails leaves nothing to
    /// prove one way or the other.
    #[test]
    fn a_group_is_only_signalled_while_the_number_behind_it_is_still_this_reads() {
        let mut child = Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("a shell to watch");
        assert_eq!(
            reserved_group(&child),
            GroupIdentity::Reserved,
            "a child this read has not collected is still a number the kernel holds"
        );

        child.wait().expect("a collected shell");
        assert_eq!(
            reserved_group(&child),
            GroupIdentity::Spent,
            "a collected child is a number the kernel may have handed to somebody else"
        );
    }

    /// The deadline bounds how long a read may take; it never bounded how much of one is kept, and a
    /// `status` of a repository with a very large change set printed all of it into a `Vec` before
    /// anything summarized or parsed it. Past the cap the answer is refused: what arrived first is
    /// not the state of the checkout, and every caller here reads it as one.
    #[test]
    fn output_past_the_cap_is_refused_rather_than_trimmed() {
        let temp = tempdir().unwrap();
        let git_pid = temp.path().join("git-pid");
        let script = format!(
            "echo $$ > \"{}\"; head -c {} /dev/zero; exit 0",
            git_pid.display(),
            GIT_OUTPUT_CAP + 1
        );

        let error = run_command_with_timeout(
            "sh",
            temp.path(),
            vec!["-c".into(), script.into()],
            Duration::from_secs(30),
        )
        .expect_err("an answer past the cap is not a whole answer");

        assert_eq!(error.code, IpcErrorCode::GitFailed);
        assert!(
            error
                .message
                .contains("Git printed more than 16 MiB on its stdout"),
            "{error:?}"
        );
        assert!(
            error.message.contains("refused rather than trimmed"),
            "the failure has to say the answer was refused, not shortened: {error:?}"
        );
        assert_processes_gone(&[read_pid(&git_pid)]);
    }

    /// The byte on the other side of the cap is the whole difference: at the cap the read is a
    /// `status` of a large checkout, one byte more is not a `status` at all.
    #[test]
    fn output_exactly_at_the_cap_is_still_a_whole_answer() {
        let temp = tempdir().unwrap();
        let script = format!("head -c {GIT_OUTPUT_CAP} /dev/zero; exit 0");

        let output = run_command_with_timeout(
            "sh",
            temp.path(),
            vec!["-c".into(), script.into()],
            Duration::from_secs(30),
        )
        .expect("an answer of exactly the cap is an answer");

        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout.len(), GIT_OUTPUT_CAP);
        assert!(output.stderr.is_empty(), "{output:?}");
    }

    /// Two caps and two streams: the diagnostic is measured on its own, so a Git that says more
    /// about a failure than the summary keeps is refused over the stream that said it rather than
    /// over the answer every caller parses.
    #[test]
    fn each_stream_is_refused_over_its_own_cap() {
        let temp = tempdir().unwrap();
        let script = "printf 'answer\\n'; yes x >&2 | head -c 1100000; exit 0";

        let error = run_command_with_timeout(
            "sh",
            temp.path(),
            vec!["-c".into(), script.into()],
            Duration::from_secs(30),
        )
        .expect_err("a diagnosis past its own cap is not a diagnosis");

        assert!(
            error
                .message
                .contains("Git printed more than 1 MiB on its stderr"),
            "{error:?}"
        );
        assert!(
            !error.message.contains("stdout"),
            "the answer that did arrive whole is not what was refused: {error:?}"
        );
    }

    /// What Git said about the failure still travels with a refused answer, because the refusal is
    /// about the volume and the diagnosis is about why the volume happened.
    #[test]
    fn a_refused_answer_keeps_what_git_said_about_itself() {
        let temp = tempdir().unwrap();
        let script = format!(
            "echo 'fatal: could not lock config file .git/config' >&2; \
             head -c {} /dev/zero",
            GIT_OUTPUT_CAP + 1
        );

        let error = run_command_with_timeout(
            "sh",
            temp.path(),
            vec!["-c".into(), script.into()],
            Duration::from_secs(30),
        )
        .expect_err("an answer past the cap is not a whole answer");

        assert!(
            error.message.contains("could not lock config file"),
            "{error:?}"
        );
    }

    /// A Git printing without end is the case a deadline does not answer: it is not slow, it is
    /// past what the read keeps, and saying so is what makes the difference between a bounded read
    /// and a wait that ends only when the deadline is.
    ///
    /// It is also the case that proves the reader does not stop at the cap: a reader that did would
    /// block the writer on a full pipe, and the read would end at the deadline instead, thirty
    /// seconds later and saying something else.
    #[test]
    fn a_git_that_prints_without_ending_is_refused_for_printing_and_not_for_running_out_of_time() {
        let temp = tempdir().unwrap();
        let git_pid = temp.path().join("git-pid");
        let script = format!("echo $$ > \"{}\"; yes x", git_pid.display());

        let started = Instant::now();
        let error = run_command_with_timeout(
            "sh",
            temp.path(),
            vec!["-c".into(), script.into()],
            Duration::from_secs(30),
        )
        .expect_err("a Git that prints without end has no whole answer");

        assert!(
            error
                .message
                .contains("Git printed more than 16 MiB on its stdout"),
            "{error:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the flood was only ended by the deadline: {:?}",
            started.elapsed()
        );
        assert_processes_gone(&[read_pid(&git_pid)]);
    }

    #[test]
    fn a_blob_is_read_whole_through_the_cap() {
        // What the deadline was added to has to answer exactly as it did before it: a blob under
        // the cap is the text of the file, byte for byte, and nothing about having a deadline on it
        // may show in the diff that draws it.
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        fs::write(root.join("view.ts"), "const n = 1;\n").unwrap();
        git(&root, &["add", "view.ts"]);
        git(&root, &["commit", "-m", "view"]);

        let blob = merge_base_content(&root, "HEAD", "view.ts");

        assert_eq!(blob.as_deref(), Some("const n = 1;\n"));
    }

    #[test]
    fn a_blob_that_is_not_text_and_one_past_the_cap_are_both_nothing() {
        // The two ways a reading of a blob ends without text: bytes that are not text at all, which
        // no grammar could be handed, and more of it than a diff is worth carrying in order to color.
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        fs::write(root.join("bytes.dat"), [0x80, 0xff, 0xfe]).unwrap();
        let big = "line\n".repeat(MAX_SYNTAX_CONTEXT_BYTES / 5 + 1);
        fs::write(root.join("big.txt"), &big).unwrap();
        git(&root, &["add", "bytes.dat", "big.txt"]);
        git(&root, &["commit", "-m", "sides"]);

        assert_eq!(merge_base_content(&root, "HEAD", "bytes.dat"), None);
        assert_eq!(merge_base_content(&root, "HEAD", "big.txt"), None);
        // A blob the merge base does not have is the same nothing, and it is Git's exit status that
        // says so: a failure prints nothing on the pipe it would have been read from.
        assert_eq!(
            merge_base_content(&root, "HEAD", "base.txt"),
            Some("base\n".to_owned())
        );
        assert_eq!(merge_base_content(&root, "HEAD", "added.txt"), None);
    }

    #[test]
    fn a_writer_that_closes_its_pipe_before_it_exits_is_still_its_text() {
        // The reading arrives before the process does, which is ordinary rather than broken: a Git
        // that has printed the whole blob and closed its pipe is a moment from exiting. Its bytes
        // are held until the status that vouches for them is in, because a reading that asked about
        // the status too early and took the answer anyway would lose the blob to the one question
        // left to ask about it.
        let temp = tempdir().unwrap();
        let script = "printf 'const n = 1;\\n'; exec 1>&-; sleep 1";

        let read = run_capped_git(
            "sh",
            temp.path(),
            ["-c", script],
            MAX_SYNTAX_CONTEXT_BYTES,
            Duration::from_secs(10),
        );

        assert_eq!(read.as_deref(), Some(&b"const n = 1;\n"[..]));
    }

    /// The cap cuts what is kept and not what is read, and this is what says so at the level where it
    /// is decided. A reader that stopped at the cap would hand over its answer without ever asking
    /// for the end of the pipe, which is a Git writing into a pipe nobody is reading: a blob is the
    /// size of a file, so that is the whole file blocking, not one buffer.
    #[test]
    fn a_pipe_past_the_cap_is_drained_to_its_end_and_says_so() {
        struct UntilTheEnd {
            left: usize,
            at_the_end: Arc<AtomicBool>,
        }
        impl io::Read for UntilTheEnd {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if self.left == 0 {
                    self.at_the_end.store(true, Ordering::Release);
                    return Ok(0);
                }
                let read = buffer.len().min(self.left);
                buffer[..read].fill(b'x');
                self.left -= read;
                Ok(read)
            }
        }
        let drain = |printed: usize, cap: usize| {
            let at_the_end = Arc::new(AtomicBool::new(false));
            let pipe = drain_pipe(
                UntilTheEnd {
                    left: printed,
                    at_the_end: at_the_end.clone(),
                },
                "muster-git-stdout",
                cap,
                Arc::new(AtomicBool::new(false)),
            )
            .expect("a thread to drain");
            let answer = pipe
                .recv_timeout(Duration::from_secs(5))
                .expect("the drain to finish")
                .expect("a pipe that could be read");
            (answer, at_the_end.load(Ordering::Acquire))
        };

        let (at_the_cap, drained) = drain(1024, 1024);
        assert_eq!(at_the_cap.bytes.len(), 1024);
        assert!(
            !at_the_cap.over_cap,
            "a reading of exactly the cap is a reading of all of it"
        );
        assert!(drained);

        let (past_the_cap, drained) = drain(4096, 1024);
        assert_eq!(past_the_cap.bytes.len(), 1024, "what is kept is the cap");
        assert!(past_the_cap.over_cap, "there was more than the cap");
        assert!(
            drained,
            "the rest was read and dropped, not left in a pipe the writer blocks on"
        );
    }

    /// A blob past the cap is ended as soon as the reading knows it, which is what a reading that
    /// stopped at the cap used to get from the broken pipe it left behind: the blob is not coming
    /// back under the cap, so there is nothing to wait for, and the deadline is no longer what ends
    /// it.
    #[test]
    fn a_blob_past_the_cap_ends_its_git_as_soon_as_it_is_past_it() {
        let temp = tempdir().unwrap();
        let git_pid = temp.path().join("git-pid");
        // More than a pipe holds, then a Git that would not finish inside the deadline at all.
        let script = format!(
            "echo $$ > \"{}\"; head -c 1048576 /dev/zero; sleep 30",
            git_pid.display()
        );

        let started = Instant::now();
        let read = run_capped_git(
            "sh",
            temp.path(),
            ["-c", &script],
            1024,
            Duration::from_secs(10),
        );

        assert_eq!(read, None, "a blob past the cap is not text to draw");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the reading only ended when the deadline was: {:?}",
            started.elapsed()
        );
        assert_processes_gone(&[read_pid(&git_pid)]);
    }

    #[test]
    fn a_cat_file_that_never_finishes_is_nothing_and_is_collected() {
        // What the deadline replaced here was a `wait` with no deadline on it at all, over a read
        // that was itself the wait: a `cat-file` blocked on a filter nobody can reach held the
        // `spawn_blocking` slot that draws the diff for as long as it chose to hang. Nothing to
        // draw is the right answer for it, so it is stopped and it leaves nothing behind.
        let temp = tempdir().unwrap();
        let git_pid = temp.path().join("git-pid");
        let helper_pid = temp.path().join("helper-pid");
        let script = format!(
            "echo $$ > \"{git}\"; sleep 30 & echo $! > \"{helper}\"; wait",
            git = git_pid.display(),
            helper = helper_pid.display()
        );

        let started = Instant::now();
        let read = run_capped_git(
            "sh",
            temp.path(),
            ["-c", &script],
            MAX_SYNTAX_CONTEXT_BYTES,
            Duration::from_millis(300),
        );

        assert_eq!(read, None, "a Git that never finishes has no blob to read");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the deadline did not end the read"
        );
        // Both processes, for the reason `stop_git_read` gives: the Git itself because a read that
        // stopped it and skipped the wait would leave a zombie, and the helper because a deadline
        // that ended only the process it started would leave it holding the pipe being read.
        assert_processes_gone(&[read_pid(&git_pid), read_pid(&helper_pid)]);
    }

    #[test]
    fn a_writer_past_the_cap_that_never_finishes_is_nothing_and_is_collected() {
        // The second way a blob ends a reading early: the blob is already past the cap while the Git
        // that is writing it keeps running, so the reading has nothing left to wait for and ends it
        // itself rather than leaving it writing on a thread that already holds its answer. The
        // answer is `None` either way -- the blob is past the cap -- and the process is collected.
        let temp = tempdir().unwrap();
        let git_pid = temp.path().join("git-pid");
        let helper_pid = temp.path().join("helper-pid");
        // Both processes record themselves before anything is printed, which is what makes them
        // findable afterwards: the reading ends as soon as the cap is passed, which can be before a
        // shell that prints first would have got as far as writing anything down.
        let script = format!(
            "echo $$ > \"{git}\"; sleep 30 & echo $! > \"{helper}\"; \
             head -c 4096 /dev/zero | tr '\\0' 'a'; wait",
            git = git_pid.display(),
            helper = helper_pid.display()
        );

        let started = Instant::now();
        let read = run_capped_git(
            "sh",
            temp.path(),
            ["-c", &script],
            1024,
            Duration::from_millis(300),
        );

        assert_eq!(read, None, "a blob past the cap is not text to draw");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the deadline did not end the writer"
        );
        assert_processes_gone(&[read_pid(&git_pid), read_pid(&helper_pid)]);
    }

    /// What the two-pipe deadlock looks like from the diff scanner: it reads its standard output and
    /// leaves the diagnostic pipe to `wait_with_output` at the end, so a Git that fills that pipe's
    /// buffer blocks on the write while this thread blocks on the read, and the pair of them waits on
    /// each other with nothing to break it. A diff filter is an ordinary way to say that much.
    ///
    /// The stderr is drained past the cap rather than refused, which is the other half of it: what a
    /// diff printed is not what this read keeps, so the answer is the diff and the diagnosis travels
    /// only inside an error.
    #[test]
    fn a_diff_that_fills_its_diagnostic_pipe_is_read_rather_than_waited_on() {
        let temp = tempdir().unwrap();
        let git_pid = temp.path().join("git-pid");
        // More than a pipe holds on stderr, and the diff itself after it, so the deadlock needs both
        // sides writing: one until the pipe is full, the other until this reading stops.
        let script = format!(
            "echo $$ > \"{git}\"; \
             head -c 262144 /dev/zero | tr '\\0' 'x' >&2; \
             printf 'diff --git a/a.txt b/a.txt\\n@@ -1 +1 @@\\n-old\\n+new\\n'",
            git = git_pid.display()
        );

        let started = Instant::now();
        let scan = scan_git_diff(
            "sh",
            temp.path(),
            vec!["-c".into(), script.into()],
            None,
            Duration::from_secs(20),
        )
        .expect("a diff behind a full diagnostic pipe is still a diff");

        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the two pipes waited on each other: {:?}",
            started.elapsed()
        );
        // The hunk header and the line on each side of it, which is the whole of this diff.
        assert_eq!(scan.total_lines, 3);
        assert!(String::from_utf8_lossy(&scan.patch).contains("+new"));
        // Ended as a whole group and collected, for the reason `stop_git_read` gives.
        assert_processes_gone(&[read_pid(&git_pid)]);
    }

    /// The other half of the same pair: a diff whose Git, or whose diff filter, never finishes. The
    /// scan is a read on a pipe and has no deadline of its own, so the deadline belongs to the thread
    /// holding the child -- and it is that one which ends the group, because it is the one still
    /// holding a pid the kernel has not given away.
    #[test]
    fn a_diff_whose_git_never_finishes_fails_after_the_deadline_and_leaves_nothing_running() {
        let temp = tempdir().unwrap();
        let git_pid = temp.path().join("git-pid");
        let helper_pid = temp.path().join("helper-pid");
        // Prints the whole diff and then waits on a helper nothing is going to answer, which is what a
        // diff driver on a network looks like: the scan has its answer and the read has to end anyway.
        let script = format!(
            "echo $$ > \"{git}\"; sleep 30 & echo $! > \"{helper}\"; \
             printf 'diff --git a/a.txt b/a.txt\\n@@ -1 +1 @@\\n-old\\n+new\\n'; wait",
            git = git_pid.display(),
            helper = helper_pid.display()
        );

        let started = Instant::now();
        let error = scan_git_diff(
            "sh",
            temp.path(),
            vec!["-c".into(), script.into()],
            None,
            Duration::from_millis(300),
        )
        .expect_err("a Git that never finishes has no diff to draw");

        assert_eq!(error.code, IpcErrorCode::GitFailed);
        assert!(
            error.message.starts_with("Git stopped reading after 0.3s"),
            "{error:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the deadline did not end the scan: {:?}",
            started.elapsed()
        );
        // Both, for the reason `stop_git_read` gives: the Git because a scan that stopped it and
        // skipped the wait would leave a zombie, and the helper because a deadline that ended only
        // the process it started would leave it holding the pipe being read.
        assert_processes_gone(&[read_pid(&git_pid), read_pid(&helper_pid)]);
    }

    /// A diff past a cap is answered with the prefix that was read, and the Git behind the rest of it
    /// is ended as a whole group rather than left printing into a pipe the scan has stopped reading.
    #[test]
    fn a_diff_that_is_too_large_ends_its_whole_group_and_leaves_nothing_running() {
        let temp = tempdir().unwrap();
        let git_pid = temp.path().join("git-pid");
        let helper_pid = temp.path().join("helper-pid");
        // An ordinary diff, then far more of one than `MAX_DIFF_HUNKS` asks for, and a helper still
        // holding the write end when the scan has stopped at the cap.
        let script = format!(
            "echo $$ > \"{git}\"; sleep 30 & echo $! > \"{helper}\"; \
             yes '@@ -1,1 +1,1 @@ a' ; wait",
            git = git_pid.display(),
            helper = helper_pid.display()
        );

        let scan = scan_git_diff(
            "sh",
            temp.path(),
            vec!["-c".into(), script.into()],
            None,
            Duration::from_secs(20),
        )
        .expect("a diff past a cap is answered with what was read of it");

        assert!(scan.too_large && scan.large);
        assert!(!String::from_utf8_lossy(&scan.patch).contains("@@"));
        assert_processes_gone(&[read_pid(&git_pid), read_pid(&helper_pid)]);
    }

    /// What the scan hashes is the lines it consumed, including the ones a large diff threw away from
    /// its patch: two large diffs are both an empty patch, so an identity that stopped looking at the
    /// dropped bytes would be the same answer twice and the view behind it would keep serving the
    /// first diff's pages.
    #[test]
    fn two_large_diffs_of_the_same_file_are_told_apart_by_their_revision() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        let original = (0..5000)
            .map(|line| format!("old-{line}\n"))
            .collect::<String>();
        fs::write(root.join("large.txt"), original).unwrap();
        git(&root, &["add", "large.txt"]);
        git(&root, &["commit", "-m", "large file"]);
        let (database, checkout_id) = git_database(temp.path(), &root);
        let changed = (0..5000)
            .map(|line| format!("new-{line}\n"))
            .collect::<String>();
        fs::write(root.join("large.txt"), changed).unwrap();
        let first = diff(&database, &checkout_id, "large.txt").unwrap();
        assert!(first.large && first.patch.is_empty());

        // A second version of the same file, whose patch is equally empty and whose lines are the same
        // ones the first one read.
        let moved = (0..5000)
            .map(|line| format!("newer-{line}\n"))
            .collect::<String>();
        fs::write(root.join("large.txt"), moved).unwrap();
        let second = diff(&database, &checkout_id, "large.txt").unwrap();

        assert!(second.large && second.patch.is_empty());
        assert_ne!(
            first.revision, second.revision,
            "two large diffs are the same answer when nothing but the patch is compared"
        );

        // And the same diff read again is the same answer, which is what keeps a refresh that moved
        // nothing from rebuilding the reading around the open note composer.
        assert_eq!(
            second.revision,
            diff(&database, &checkout_id, "large.txt").unwrap().revision,
            "a diff that had not moved answered with another identity"
        );
    }

    /// The identity is set on the path that has no scan behind it at all, and a symlink's target can
    /// be repointed while its patch stays empty.
    #[test]
    fn a_symlink_diff_is_told_apart_by_the_target_it_points_at() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        std::os::unix::fs::symlink("one.txt", root.join("link.txt")).unwrap();
        let (database, checkout_id) = git_database(temp.path(), &root);
        fs::write(root.join("one.txt"), "one\n").unwrap();
        fs::write(root.join("two.txt"), "two\n").unwrap();

        let first = diff(&database, &checkout_id, "link.txt").unwrap();
        assert_eq!(first.symlink_target.as_deref(), Some("one.txt"));
        assert!(first.patch.is_empty());

        fs::remove_file(root.join("link.txt")).unwrap();
        std::os::unix::fs::symlink("two.txt", root.join("link.txt")).unwrap();
        let second = diff(&database, &checkout_id, "link.txt").unwrap();

        assert_eq!(second.symlink_target.as_deref(), Some("two.txt"));
        assert_ne!(
            first.revision, second.revision,
            "a symlink repointed while the panel was on it answered with the same identity"
        );
    }

    fn assert_processes_gone(pids: &[libc::pid_t]) {
        assert_processes_gone_named(pids, "the stopped read");
    }

    fn assert_processes_gone_named(pids: &[libc::pid_t], what: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while pids.iter().any(|pid| process_exists(*pid)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        for pid in pids {
            assert!(!process_exists(*pid), "{what} left process {pid} behind");
        }
    }

    /// The budget is the answer to "a burst that will not stop, while the worker waits on Git":
    /// the wakeups were capped, the payload behind them was not, and paths accumulated in sets for
    /// as long as the merge went on. Two budgets rather than one -- a count and a byte budget --
    /// because either one alone leaves a burst that grows in the other direction.
    #[test]
    fn a_burst_past_the_path_budget_refreshes_what_it_named_and_keeps_its_activity() {
        let mut slot = slot_with_budget(2, 4096);
        slot.merge_update(moved_paths("main", &["build/a.js", "build/b.js"]));

        // At the budget is not past it: the paths are still carried, still what `check-ignore`
        // would be asked about, and the checkout is not pinned by spending.
        assert_eq!(slot.update.paths[&"main".to_owned()].len(), 2);
        assert!(slot.budget.paths == 2 && !slot.update.spent);

        slot.merge_update(moved_paths("main", &["build/c.js"]));
        let batch = slot.take();

        assert!(
            batch.paths.is_empty(),
            "the payload is what the budget is for"
        );
        assert_eq!(
            batch.status,
            ["main"],
            "a burst that big is not a clean checkout"
        );
        assert_eq!(
            batch.activity,
            ["main"],
            "the file explorer shows what was written, budget or not"
        );
        assert_eq!(
            batch.pinned,
            BTreeSet::from(["main".to_owned()]),
            "pinned is what keeps an emptied `paths` from reading as all ignored"
        );
    }

    /// The bytes are counted with the NUL `check-ignore --stdin` is fed after each path, and they are
    /// counted at all because a burst of deep paths costs more per file than one of shallow ones:
    /// the same number of files is a different burst, and the one that spends the budget must be the
    /// one that was expensive.
    #[test]
    fn a_burst_of_deep_paths_spends_the_byte_budget_before_the_path_one() {
        let inside = "a".repeat(23);
        let mut within = slot_with_budget(1_000, 24);
        within.merge_update(moved_paths("main", &[&inside]));
        assert_eq!(
            within.update.paths[&"main".to_owned()].len(),
            1,
            "23 bytes of path and the NUL behind it is exactly the budget"
        );

        let past = "a".repeat(24);
        let mut over = slot_with_budget(1_000, 24);
        over.merge_update(moved_paths("main", &[&past]));
        let batch = over.take();
        assert!(batch.paths.is_empty());
        assert_eq!(batch.status, ["main"]);
        assert_eq!(batch.pinned, BTreeSet::from(["main".to_owned()]));
    }

    /// Nothing is dropped silently. A checkout named after the budget is spent is still a checkout
    /// something was written in, and the batch that says otherwise is what leaves a row describing a
    /// change set that no longer exists.
    #[test]
    fn a_change_after_the_budget_is_refreshed_rather_than_dropped() {
        let mut slot = slot_with_budget(1, 4096);
        slot.merge_update(moved_paths("main", &["build/a.js", "build/b.js"]));
        slot.merge_update(moved_paths("task", &["build/c.js"]));
        slot.merge_update(moved_paths("task", &["build/d.js"]));

        let batch = slot.take();

        assert!(batch.paths.is_empty());
        assert_eq!(batch.status, ["main", "task"]);
        assert_eq!(batch.activity, ["main", "task"]);
        assert_eq!(
            batch.pinned,
            BTreeSet::from(["main".to_owned(), "task".to_owned()])
        );
    }

    /// The budget belongs to the burst and not to the watcher: taking the batch hands the next one a
    /// whole one, or a single enormous build would leave every checkout after it being refreshed for
    /// the rest of the session.
    #[test]
    fn the_batch_after_an_overspent_one_carries_its_paths_again() {
        let mut slot = slot_with_budget(1, 4096);
        slot.merge_update(moved_paths("main", &["build/a.js", "build/b.js"]));
        assert!(slot.update.spent);
        slot.take();

        slot.merge_update(moved_paths("main", &["build/c.js"]));
        assert_eq!(slot.update.paths[&"main".to_owned()].len(), 1);
        assert!(!slot.update.spent);
    }

    /// The same funnel the worker reads: the queue, the merge behind the wakeup and the terminal
    /// signals that end the worker. A budget that ran out is a payload decision and cannot leave the
    /// watcher unable to stop or unable to report that its watch stream died.
    #[test]
    fn a_watcher_whose_burst_ran_out_of_budget_still_answers_refreshes_and_still_stops() {
        let (queue, inbox) = WatchQueue::new();
        if let Ok(mut slot) = queue.merged.lock() {
            slot.budget = PathBudget {
                max_paths: 1,
                max_bytes: 4096,
                ..PathBudget::default()
            };
        }
        queue.changed(moved_paths("main", &["build/a.js"]));
        queue.changed(moved_paths("main", &["build/b.js"]));
        queue.changed(moved_paths("task", &["build/c.js"]));

        assert_eq!(
            receive_debounced_change(&inbox, Duration::from_millis(1), Duration::from_secs(1)),
            WatchWakeup::Changed(WatchUpdate {
                status: vec!["main".to_owned(), "task".to_owned()],
                activity: vec!["main".to_owned(), "task".to_owned()],
                paths: BTreeMap::new(),
                pinned: BTreeSet::from(["main".to_owned(), "task".to_owned()]),
                spent: true,
                ..WatchUpdate::default()
            })
        );

        assert!(queue.failed("the watch stream died".to_owned()));
        assert_eq!(
            receive_debounced_change(&inbox, Duration::from_millis(1), Duration::from_secs(1)),
            WatchWakeup::Failed("the watch stream died".to_owned())
        );
        queue.stop();
        assert_eq!(
            receive_debounced_change(&inbox, Duration::from_millis(1), Duration::from_secs(1)),
            WatchWakeup::Stop
        );
    }

    /// The whole composed path a burst spends its budget on: the answer, the slot the worker rebuilds
    /// around it, and the tail of the same burst that arrives while that answer is being built.
    ///
    /// Spending is a fact about the batch, not about the budget that noticed it, and the rebuild is
    /// where a budget goes back to whole. A tail that carries paths into the rebuilt slot would
    /// otherwise be the whole of what the answer names: the reader takes it as every path that moved,
    /// and the ones the oversized update gave up are never asked about again.
    #[test]
    fn a_spent_batch_is_not_laundered_into_the_paths_of_the_update_that_follows_it() {
        let (queue, inbox) = WatchQueue::new();
        if let Ok(mut slot) = queue.merged.lock() {
            slot.budget = PathBudget {
                max_paths: 1,
                max_bytes: 4096,
                ..PathBudget::default()
            };
        }
        queue.changed(moved_paths("main", &["build/a.js", "build/b.js"]));

        // The two steps of `receive_debounced_change`: the answer, then the slot merged into.
        let WatchWakeup::Changed(answered) = inbox.try_recv().expect("the burst that spent it")
        else {
            panic!("the burst was not answered as a change");
        };
        assert!(
            answered.paths.is_empty(),
            "the burst that ran out of budget is what let its paths go"
        );
        let mut tail = VecDeque::from([WatchWakeup::Changed(moved_paths("main", &["build/c.js"]))]);

        let delivered = receive_debounced_updates(
            WatchSlot::holding(answered),
            Duration::from_millis(100),
            Duration::from_secs(1),
            || Duration::ZERO,
            |_| match tail.pop_front() {
                Some(wakeup) => Ok(wakeup),
                None => Err(mpsc::RecvTimeoutError::Timeout),
            },
        );

        let WatchWakeup::Changed(batch) = delivered else {
            panic!("the batch was not answered as a change: {delivered:?}");
        };
        assert!(
            batch.paths.is_empty(),
            "a later update carrying one path does not make the batch a batch that carries only one"
        );
        assert_eq!(
            file_activity(&batch),
            vec![FileActivity {
                checkout_id: "main".to_owned(),
                paths: Vec::new(),
            }],
            "an empty list is the batch declining to say what it moved, which a reader asks everything about"
        );
    }

    /// The cap `check-ignore` is given is what turns a burst into "nothing in here matters", and an
    /// emptied `paths` on a batch Git never answered about is the one reading of it that is not a
    /// refresh. So Git is not asked at all: there are no paths left to ask about, and the checkout
    /// is re-read rather than held back.
    #[test]
    fn a_batch_that_ran_out_of_budget_is_not_read_as_every_path_being_ignored() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::write(root.join("node_modules/pkg/index.js"), "x\n").unwrap();
        let (database, checkout_id) = git_database(temp.path(), &root);
        let repo_id = database.load_workspace().unwrap().repos[0].id.clone();
        let plan = super::watch_plan(&database, &repo_id).unwrap();

        let mut slot = slot_with_budget(1, 4096);
        slot.merge_update(moved_paths(
            &checkout_id,
            &["node_modules/pkg/index.js", "node_modules/pkg/other.js"],
        ));
        let before = git_reads(&root);

        let answered = without_ignored_only(&plan, slot.take());

        assert_eq!(
            answered.status[..],
            [checkout_id.as_str()],
            "every path Git was never asked about cannot be a path Git said it ignores"
        );
        assert_eq!(answered.activity, [checkout_id]);
        assert_eq!(
            git_reads(&root) - before,
            0,
            "there were no paths left to ask about"
        );
    }

    /// The budget the watcher runs on, at the numbers it runs on: `WATCH_PATH_BUDGET` paths of an
    /// ordinary depth fit, and the one past the count is what spends it. The byte budget is not what
    /// binds here, which is the point of having both -- a burst of shallow paths is stopped by the
    /// count and a burst of deep ones by the bytes, and neither is a smaller cap wearing the other's
    /// name.
    #[test]
    fn the_production_budget_carries_its_own_paths_and_spends_on_the_next_one() {
        let path = |index: usize| format!("build/{index:05}.js");
        let mut slot = WatchSlot::holding(WatchUpdate::default());
        assert_eq!(slot.budget.max_paths, WATCH_PATH_BUDGET);
        assert_eq!(slot.budget.max_bytes, WATCH_PATH_BYTE_BUDGET);

        slot.merge_update(WatchUpdate {
            status: vec!["main".to_owned()],
            paths: BTreeMap::from([(
                "main".to_owned(),
                (0..WATCH_PATH_BUDGET)
                    .map(|index| PathBuf::from(path(index)))
                    .collect(),
            )]),
            ..WatchUpdate::default()
        });
        assert!(
            !slot.update.spent && slot.budget.paths == WATCH_PATH_BUDGET,
            "{WATCH_PATH_BUDGET} ordinary paths are inside the budget: {:?}",
            slot.budget
        );
        assert!(slot.budget.bytes < WATCH_PATH_BYTE_BUDGET);

        slot.merge_update(moved_paths("main", &[&path(WATCH_PATH_BUDGET)]));
        let batch = slot.take();
        assert!(batch.paths.is_empty());
        assert_eq!(batch.status, ["main"]);
        assert_eq!(batch.pinned, BTreeSet::from(["main".to_owned()]));
    }

    fn slot_with_budget(max_paths: usize, max_bytes: usize) -> WatchSlot {
        let mut slot = WatchSlot::holding(WatchUpdate::default());
        slot.budget = PathBudget {
            max_paths,
            max_bytes,
            ..PathBudget::default()
        };
        slot
    }

    fn moved_paths(checkout_id: &str, paths: &[&str]) -> WatchUpdate {
        WatchUpdate {
            status: vec![checkout_id.to_owned()],
            activity: vec![checkout_id.to_owned()],
            paths: BTreeMap::from([(
                checkout_id.to_owned(),
                paths.iter().map(PathBuf::from).collect(),
            )]),
            ..WatchUpdate::default()
        }
    }

    #[test]
    fn watcher_caps_a_continuous_batch_and_preserves_changes_for_the_next_batch() {
        let elapsed = Cell::new(Duration::ZERO);
        let mut pending = VecDeque::from([
            (
                Duration::from_millis(50),
                WatchWakeup::Changed(watch_update(
                    &["early"],
                    &["activity-early"],
                    &["repo-early"],
                )),
            ),
            (
                Duration::from_millis(120),
                WatchWakeup::Changed(watch_update(
                    &["middle"],
                    &["activity-middle"],
                    &["repo-middle"],
                )),
            ),
            (
                Duration::from_millis(190),
                WatchWakeup::Changed(watch_update(
                    &["initial"],
                    &["activity-early"],
                    &["repo-early"],
                )),
            ),
            (
                Duration::from_millis(240),
                WatchWakeup::Changed(watch_update(
                    &["near-limit"],
                    &["activity-near-limit"],
                    &["repo-near-limit"],
                )),
            ),
            (
                Duration::from_millis(260),
                WatchWakeup::Changed(touched(&["next-batch"])),
            ),
        ]);
        let mut waits = Vec::new();
        let mut receives = 0;

        let wakeup = receive_debounced_updates(
            WatchSlot::holding(watch_update(
                &["initial"],
                &["activity-initial"],
                &["repo-initial"],
            )),
            Duration::from_millis(100),
            Duration::from_millis(250),
            || elapsed.get(),
            |timeout| {
                receives += 1;
                waits.push(timeout);
                let deadline = elapsed.get() + timeout;
                if pending.front().is_some_and(|(at, _)| *at < deadline) {
                    let (at, message) = pending.pop_front().unwrap();
                    elapsed.set(at);
                    Ok(message)
                } else {
                    elapsed.set(deadline);
                    Err(mpsc::RecvTimeoutError::Timeout)
                }
            },
        );

        assert_eq!(
            wakeup,
            WatchWakeup::Changed(watch_update(
                &["initial", "early", "middle", "near-limit"],
                &[
                    "activity-initial",
                    "activity-early",
                    "activity-middle",
                    "activity-near-limit",
                ],
                &[
                    "repo-initial",
                    "repo-early",
                    "repo-middle",
                    "repo-near-limit"
                ],
            ))
        );
        assert_eq!(
            waits,
            [
                Duration::from_millis(100),
                Duration::from_millis(100),
                Duration::from_millis(100),
                Duration::from_millis(60),
                Duration::from_millis(10),
            ]
        );
        assert_eq!(elapsed.get(), Duration::from_millis(250));
        assert_eq!(receives, 5);
        assert!(
            matches!(pending.front(), Some((at, WatchWakeup::Changed(_))) if *at == Duration::from_millis(260))
        );
    }

    #[test]
    fn watcher_stops_if_the_channel_disconnects_during_a_batch() {
        let (queue, inbox) = WatchQueue::new();
        queue.changed(touched(&["a"]));
        drop(queue);

        assert_eq!(
            receive_debounced_change(&inbox, Duration::from_millis(1), Duration::from_secs(1),),
            WatchWakeup::Stop
        );
    }

    #[test]
    fn repository_watcher_observes_file_edits_and_says_which_worktree_they_landed_in() {
        let temp = tempdir().unwrap();
        fs::create_dir_all(temp.path().join("main")).unwrap();
        fs::create_dir_all(temp.path().join("task")).unwrap();
        // Resolved, because the filesystem reports resolved paths: a checkout reached
        // through a symlink would otherwise match no event and quietly go stale forever.
        let root = temp.path().join("main").canonicalize().unwrap();
        let worktree = temp.path().join("task").canonicalize().unwrap();
        let plan = RepoWatchPlan {
            repo_id: "repo".to_owned(),
            roots: [
                (root.clone(), "main".to_owned()),
                (worktree.clone(), "task".to_owned()),
            ]
            .into_iter()
            .collect(),
            git_dirs: BTreeMap::new(),
            common_dir: None,
            all: vec!["main".to_owned(), "task".to_owned()],
            requested: vec!["main".to_owned(), "task".to_owned()],
        };
        let (queue, inbox) = WatchQueue::new();
        let event_queue = queue.clone();
        let event_plan = plan.clone();
        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                if let Some(update) = result
                    .ok()
                    .and_then(|event| affected_checkouts(&event_plan, &event))
                {
                    event_queue.changed(update);
                }
            })
            .unwrap();
        for path in [root, worktree.clone()] {
            watcher
                .watch(&path, notify::RecursiveMode::Recursive)
                .unwrap();
        }

        // Draining before the write is what keeps the startup event out of the write's answer.
        // `notify` puts every watched path into one FSEventStream, FSEvents names each of those
        // paths once when the stream starts, and the queue merges whatever arrives together into a
        // single update -- so on a loaded runner the untouched checkout's name and the write are
        // one update, and the assertion below would read as if the save had reached it.
        while inbox.recv_timeout(Duration::from_millis(250)).is_ok() {}

        fs::write(worktree.join("terminal-edit.txt"), "changed\n").unwrap();

        // Only the worktree that was written to is named. The row beside it is untouched, and
        // saying otherwise would make a save in one worktree cost a re-read of every other.
        //
        // The wait is for the update that names the written worktree, the same way
        // `assert_membership_signal` waits for the worktree it asked for: the claim under test
        // is still that one update, and it still has to name that worktree and nothing else.
        let deadline = Instant::now() + Duration::from_secs(5);
        let update = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match inbox.recv_timeout(remaining) {
                Ok(WatchWakeup::Changed(update)) if update.status.contains(&"task".to_owned()) => {
                    break update;
                }
                Ok(WatchWakeup::Changed(_)) => continue,
                Ok(WatchWakeup::Failed(error)) => panic!("watch failed: {error}"),
                Ok(WatchWakeup::Stop) => panic!("watcher stopped"),
                Err(_) => panic!("the write into the worktree was never reported"),
            }
        };
        assert_eq!(update.status, vec!["task".to_owned()]);
        assert_eq!(update.activity, vec!["task".to_owned()]);
    }

    #[test]
    fn a_ref_write_speaks_for_every_worktree_because_a_commit_moves_the_merge_base() {
        let plan = RepoWatchPlan {
            repo_id: "repo".to_owned(),
            roots: [
                (PathBuf::from("/repo"), "main".to_owned()),
                (PathBuf::from("/repo-task"), "task".to_owned()),
            ]
            .into_iter()
            .collect(),
            // The primary's own Git directory is the shared one, so it is not here: a ref
            // written under it is the repository's change, not the primary checkout's.
            git_dirs: BTreeMap::from([(
                PathBuf::from("/repo/.git/worktrees/task"),
                "task".to_owned(),
            )]),
            common_dir: Some(PathBuf::from("/repo/.git")),
            all: vec!["main".to_owned(), "task".to_owned()],
            requested: vec!["main".to_owned(), "task".to_owned()],
        };
        let write = notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![PathBuf::from("/repo/.git/refs/heads/main")],
            attrs: Default::default(),
        };

        // The sibling on another branch counted its lines against a merge base that just moved,
        // so its row is now describing a change set that does not exist.
        assert_eq!(
            affected_checkouts(&plan, &write).map(|update| update.status),
            Some(vec!["main".to_owned(), "task".to_owned()])
        );
    }

    #[test]
    fn staging_in_one_worktree_does_not_re_read_the_siblings() {
        let plan = RepoWatchPlan {
            repo_id: "repo".to_owned(),
            roots: [
                (PathBuf::from("/repo"), "main".to_owned()),
                (PathBuf::from("/repo-task"), "task".to_owned()),
            ]
            .into_iter()
            .collect(),
            // The primary's own Git directory is the shared one, so it is not here: a ref
            // written under it is the repository's change, not the primary checkout's.
            git_dirs: BTreeMap::from([(
                PathBuf::from("/repo/.git/worktrees/task"),
                "task".to_owned(),
            )]),
            common_dir: Some(PathBuf::from("/repo/.git")),
            all: vec!["main".to_owned(), "task".to_owned()],
            requested: vec!["main".to_owned(), "task".to_owned()],
        };
        let staged = notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![PathBuf::from("/repo/.git/worktrees/task/index")],
            attrs: Default::default(),
        };

        // The sibling is on another branch with another index: nothing it counts moved. An
        // agent turn stages in the worktree it is working in, several times, and each of those
        // is a save in every other row if the whole repository is re-read.
        assert_eq!(
            affected_checkouts(&plan, &staged).map(|update| update.status),
            Some(vec!["task".to_owned()])
        );
    }

    #[test]
    fn a_worktree_joining_the_repository_asks_for_its_own_list_to_be_read_again() {
        let plan = RepoWatchPlan {
            repo_id: "repo".to_owned(),
            roots: [(PathBuf::from("/repo"), "main".to_owned())]
                .into_iter()
                .collect(),
            git_dirs: BTreeMap::new(),
            common_dir: Some(PathBuf::from("/repo/.git")),
            all: vec!["main".to_owned()],
            requested: vec!["main".to_owned()],
        };
        // Git registers the worktree, writes its metadata and only then the checkout, wherever
        // the new directory landed: the registration is the one write every way of adding a
        // worktree goes through, and an agent running `git worktree add` is the same event.
        for (path, kind) in [
            (
                "/repo/.git/worktrees/task",
                notify::EventKind::Create(notify::event::CreateKind::Folder),
            ),
            (
                "/repo/.git/worktrees/task/gitdir",
                notify::EventKind::Create(notify::event::CreateKind::File),
            ),
        ] {
            let event = notify::Event {
                kind,
                paths: vec![PathBuf::from(path)],
                attrs: Default::default(),
            };
            // No row of the repository is stale — there is no row for the new worktree yet — so
            // the answer is the repository itself, and nothing is refreshed for the worktree it
            // just joined.
            assert_eq!(
                affected_checkouts(&plan, &event),
                Some(WatchUpdate {
                    status: Vec::new(),
                    activity: Vec::new(),
                    worktrees: vec!["repo".to_owned()],
                    ..WatchUpdate::default()
                }),
                "{path}"
            );
        }
    }

    #[test]
    fn a_real_git_worktree_add_and_remove_emit_membership_changes() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        fs::create_dir_all(root.join(".worktrees")).unwrap();
        git(&root, &["branch", "external"]);
        let (database, _) = git_database(temp.path(), &root);
        let repo_id = database.load_workspace().unwrap().repos[0].id.clone();
        let plan = super::watch_plan(&database, &repo_id).unwrap();
        let common_dir = plan.common_dir.clone().unwrap();
        let (queue, inbox) = WatchQueue::new();
        let event_queue = queue.clone();
        let event_plan = plan.clone();
        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                if let Some(update) = result
                    .ok()
                    .and_then(|event| affected_checkouts(&event_plan, &event))
                {
                    event_queue.changed(update);
                }
            })
            .unwrap();
        watcher
            .watch(&common_dir, notify::RecursiveMode::Recursive)
            .unwrap();
        for root in plan.roots.keys() {
            watcher
                .watch(root, notify::RecursiveMode::Recursive)
                .unwrap();
        }

        let added = root.join(".worktrees/external");
        git(
            &root,
            &["worktree", "add", added.to_str().unwrap(), "external"],
        );
        assert_membership_signal(&inbox, &repo_id);

        git(&root, &["worktree", "remove", added.to_str().unwrap()]);
        assert_membership_signal(&inbox, &repo_id);
    }

    fn assert_membership_signal(inbox: &WatchInbox, repo_id: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let update = match inbox.recv_timeout(remaining) {
                Ok(WatchWakeup::Changed(update)) => update,
                Ok(WatchWakeup::Failed(error)) => panic!("watch failed: {error}"),
                Ok(WatchWakeup::Stop) => panic!("watcher stopped"),
                Err(_) => panic!("Git worktree add/remove should notify the shared Git directory"),
            };
            if update.worktrees.iter().any(|id| id == repo_id) {
                return;
            }
            assert!(
                update.status.is_empty() && update.activity.is_empty(),
                "the new worktree's files must not be reported as primary-checkout changes"
            );
        }
    }

    #[test]
    fn removing_a_known_worktree_registration_is_not_mistaken_for_status() {
        let plan = RepoWatchPlan {
            repo_id: "repo".to_owned(),
            roots: [(PathBuf::from("/repo"), "main".to_owned())]
                .into_iter()
                .collect(),
            git_dirs: BTreeMap::from([(
                PathBuf::from("/repo/.git/worktrees/task"),
                "task".to_owned(),
            )]),
            common_dir: Some(PathBuf::from("/repo/.git")),
            all: vec!["main".to_owned(), "task".to_owned()],
            requested: vec!["main".to_owned(), "task".to_owned()],
        };
        let removed = notify::Event {
            kind: notify::EventKind::Remove(notify::event::RemoveKind::File),
            paths: vec![PathBuf::from("/repo/.git/worktrees/task/gitdir")],
            attrs: Default::default(),
        };

        let update = affected_checkouts(&plan, &removed).unwrap();

        assert!(update.status.is_empty());
        assert_eq!(update.worktrees, ["repo"]);
    }

    #[test]
    fn opening_primary_files_while_creating_a_worktree_is_not_a_checkout_change() {
        let plan = RepoWatchPlan {
            repo_id: "repo".to_owned(),
            roots: [(PathBuf::from("/repo"), "main".to_owned())]
                .into_iter()
                .collect(),
            common_dir: Some(PathBuf::from("/repo/.git")),
            all: vec!["main".to_owned()],
            ..RepoWatchPlan::default()
        };
        let path = PathBuf::from("/repo/base.txt");

        // Linux inotify surfaces opens and read-only closes as Access events.
        for kind in [
            notify::EventKind::Access(notify::event::AccessKind::Open(
                notify::event::AccessMode::Any,
            )),
            notify::EventKind::Access(notify::event::AccessKind::Close(
                notify::event::AccessMode::Read,
            )),
        ] {
            let event = notify::Event {
                kind,
                paths: vec![path.clone()],
                attrs: Default::default(),
            };
            assert_eq!(affected_checkouts(&plan, &event), None);
        }

        let write = notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Any,
            )),
            paths: vec![path],
            attrs: Default::default(),
        };
        assert_eq!(
            affected_checkouts(&plan, &write),
            Some(WatchUpdate {
                status: vec!["main".to_owned()],
                activity: vec!["main".to_owned()],
                worktrees: Vec::new(),
                paths: BTreeMap::from([(
                    "main".to_owned(),
                    BTreeSet::from([PathBuf::from("base.txt")]),
                )]),
                pinned: BTreeSet::new(),
                spent: false,
            })
        );
    }

    #[test]
    fn reading_a_ref_or_an_index_is_not_a_change_in_one() {
        let plan = RepoWatchPlan {
            repo_id: "repo".to_owned(),
            roots: [(PathBuf::from("/repo"), "main".to_owned())]
                .into_iter()
                .collect(),
            common_dir: Some(PathBuf::from("/repo/.git")),
            all: vec!["main".to_owned()],
            ..RepoWatchPlan::default()
        };
        // A path alone cannot tell a read from a write, and Linux reports every open of a watched
        // path as an event of its own. Every Git command opens the HEAD and the refs it reads
        // before it changes anything, so answering those as the writes they resemble refreshed the
        // whole repository on every command run inside it.
        for path in [
            "/repo/.git/HEAD",
            "/repo/.git/index",
            "/repo/.git/refs/heads/trunk",
            "/repo/base.txt",
        ] {
            let read = notify::Event {
                kind: notify::EventKind::Access(notify::event::AccessKind::Open(
                    notify::event::AccessMode::Any,
                )),
                paths: vec![PathBuf::from(path)],
                attrs: Default::default(),
            };

            assert_eq!(affected_checkouts(&plan, &read), None, "{path}");
        }
    }

    #[test]
    fn a_worktree_under_the_reserved_container_is_not_counted_as_primary_checkout_activity() {
        let mut plan = RepoWatchPlan {
            repo_id: "repo".to_owned(),
            roots: [(PathBuf::from("/repo"), "main".to_owned())]
                .into_iter()
                .collect(),
            common_dir: Some(PathBuf::from("/repo/.git")),
            all: vec!["main".to_owned()],
            ..RepoWatchPlan::default()
        };
        let added_file = notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![PathBuf::from("/repo/.worktrees/task/new-file.txt")],
            attrs: Default::default(),
        };

        // The default worktree lives inside the root, but it is not part of the primary checkout.
        // Before the shared Git event registers it, treating these writes as primary edits would
        // repeatedly refresh and announce files from the new checkout.
        assert_eq!(affected_checkouts(&plan, &added_file), None);

        plan.roots
            .insert(PathBuf::from("/repo/.worktrees/task"), "task".to_owned());
        plan.all.push("task".to_owned());

        let update = affected_checkouts(&plan, &added_file).unwrap();
        assert_eq!(update.status, ["task"]);
        assert_eq!(update.activity, ["task"]);
    }

    #[test]
    fn a_worktree_staging_does_not_ask_the_whole_repository_to_be_read_again() {
        let plan = RepoWatchPlan {
            repo_id: "repo".to_owned(),
            roots: [(PathBuf::from("/repo"), "main".to_owned())]
                .into_iter()
                .collect(),
            git_dirs: BTreeMap::from([(
                PathBuf::from("/repo/.git/worktrees/task"),
                "task".to_owned(),
            )]),
            common_dir: Some(PathBuf::from("/repo/.git")),
            all: vec!["main".to_owned(), "task".to_owned()],
            requested: vec!["main".to_owned(), "task".to_owned()],
        };
        // A checkout's own index, lock and HEAD move on every command, and the lock appears and
        // disappears, so kind alone cannot tell them from a registration. Reading the whole
        // repository because an agent ran `git add` is the cost this signal has to refuse.
        let events = [
            notify::EventKind::Modify(notify::event::ModifyKind::Any),
            notify::EventKind::Create(notify::event::CreateKind::File),
            notify::EventKind::Remove(notify::event::RemoveKind::File),
        ];
        for path in [
            "/repo/.git/worktrees/task/HEAD",
            "/repo/.git/worktrees/task/index",
            "/repo/.git/worktrees/task/index.lock",
            "/repo/.git/worktrees/task/ORIG_HEAD",
        ] {
            for kind in events {
                let event = notify::Event {
                    kind,
                    paths: vec![PathBuf::from(path)],
                    attrs: Default::default(),
                };
                // The lock is filtered before anything is attributed, so it produces no update at
                // all. The other three still speak for the checkout that owns them, and say
                // nothing about the repository's worktrees.
                let expected = if path.ends_with(".lock") {
                    None
                } else {
                    Some(Vec::new())
                };
                assert_eq!(
                    affected_checkouts(&plan, &event).map(|update| update.worktrees),
                    expected,
                    "{path}"
                );
            }
        }
    }

    /// The self-inflicted loop: reading a checkout refreshed the index of a linked worktree, and
    /// the index is inside the directory the watcher watches, so every read announced itself and
    /// the next read was already on its way. A lock is a write in flight, and reading must not
    /// be one, so no lock in Git's own metadata speaks for anything.
    #[test]
    fn a_lock_in_git_metadata_is_not_a_change_while_the_rename_that_finishes_it_is() {
        let plan = RepoWatchPlan {
            repo_id: "repo".to_owned(),
            roots: [
                (PathBuf::from("/repo"), "main".to_owned()),
                (PathBuf::from("/repo-task"), "task".to_owned()),
            ]
            .into_iter()
            .collect(),
            // The primary's own Git directory is the shared one, so it is not here: what is written
            // under it is the repository's change, not the primary checkout's.
            git_dirs: BTreeMap::from([(
                PathBuf::from("/repo/.git/worktrees/task"),
                "task".to_owned(),
            )]),
            common_dir: Some(PathBuf::from("/repo/.git")),
            all: vec!["main".to_owned(), "task".to_owned()],
            requested: vec!["main".to_owned(), "task".to_owned()],
        };
        let kinds = [
            notify::EventKind::Modify(notify::event::ModifyKind::Any),
            notify::EventKind::Create(notify::event::CreateKind::File),
            notify::EventKind::Remove(notify::event::RemoveKind::File),
        ];
        for path in [
            // A linked worktree's own index, which is the one a `git status` there would lock.
            "/repo/.git/worktrees/task/index.lock",
            "/repo/.git/worktrees/task/HEAD.lock",
            // The primary checkout's index, which reaches the shared directory instead and is the
            // one that would re-read every worktree in the repository.
            "/repo/.git/index.lock",
            "/repo/.git/HEAD.lock",
            // A ref is written the same way, and a lock under one would move the merge base
            // every sibling counts its lines against.
            "/repo/.git/refs/heads/trunk.lock",
            "/repo/.git/packed-refs.lock",
        ] {
            for kind in kinds {
                let event = notify::Event {
                    kind,
                    paths: vec![PathBuf::from(path)],
                    attrs: Default::default(),
                };
                assert_eq!(affected_checkouts(&plan, &event), None, "{path}");
            }
        }

        // The rename that lands the write is the change, and it is the path that has to speak.
        // Both the checkout's own index and the repository's are asked here, because the first
        // names one worktree and the second names all of them.
        let staged = notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![PathBuf::from("/repo/.git/worktrees/task/index")],
            attrs: Default::default(),
        };
        assert_eq!(
            affected_checkouts(&plan, &staged).map(|update| update.status),
            Some(vec!["task".to_owned()])
        );
        let shared = notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![PathBuf::from("/repo/.git/index")],
            attrs: Default::default(),
        };
        assert_eq!(
            affected_checkouts(&plan, &shared).map(|update| update.status),
            Some(vec!["main".to_owned(), "task".to_owned()])
        );
    }

    /// A backend that does not batch reports the rename of a lock over the file it replaces as
    /// one event carrying both paths. Dropping the event because one of its paths is a lock would
    /// lose the change, so the lock is skipped and the destination still answers.
    #[test]
    fn a_rename_onto_a_git_file_answers_with_the_file_it_landed_on() {
        let plan = RepoWatchPlan {
            repo_id: "repo".to_owned(),
            roots: [(PathBuf::from("/repo"), "main".to_owned())]
                .into_iter()
                .collect(),
            git_dirs: BTreeMap::from([(
                PathBuf::from("/repo/.git/worktrees/task"),
                "task".to_owned(),
            )]),
            common_dir: Some(PathBuf::from("/repo/.git")),
            all: vec!["main".to_owned(), "task".to_owned()],
            requested: vec!["main".to_owned(), "task".to_owned()],
        };
        let rename = notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::Both,
            )),
            paths: vec![
                PathBuf::from("/repo/.git/worktrees/task/index.lock"),
                PathBuf::from("/repo/.git/worktrees/task/index"),
            ],
            attrs: Default::default(),
        };

        assert_eq!(
            affected_checkouts(&plan, &rename).map(|update| update.status),
            Some(vec!["task".to_owned()])
        );
    }

    /// The filter is about where Git writes, not about the shape of a name. A file in a checkout
    /// that ends in `.lock` is a file the user saved, and treating it as bookkeeping would leave
    /// the row describing an older version of the worktree than the one on disk.
    #[test]
    fn a_file_the_user_named_like_a_lock_is_still_a_change_to_their_checkout() {
        let plan = RepoWatchPlan {
            repo_id: "repo".to_owned(),
            roots: [(PathBuf::from("/repo"), "main".to_owned())]
                .into_iter()
                .collect(),
            git_dirs: BTreeMap::new(),
            common_dir: Some(PathBuf::from("/repo/.git")),
            all: vec!["main".to_owned()],
            requested: vec!["main".to_owned()],
        };
        let saved = notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![PathBuf::from("/repo/notes.lock")],
            attrs: Default::default(),
        };

        let update = affected_checkouts(&plan, &saved).unwrap();
        assert_eq!(update.status, ["main"]);
        assert_eq!(update.activity, ["main"]);
    }

    /// The half of the loop that does not depend on the watcher: a read that takes the optional
    /// index lock writes into the directory being watched, which is enough to keep the repository
    /// re-reading itself forever. Reading has to answer the same question from what is already on
    /// disk and leave the index alone.
    #[test]
    fn reading_the_status_leaves_the_index_exactly_as_git_wrote_it() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        fs::write(root.join("edited.txt"), "first\n").unwrap();
        git(root, &["add", "edited.txt"]);
        git(root, &["commit", "-m", "second"]);

        // A file saved with the content it already had, and a timestamp Git has not seen: the
        // answer is that nothing changed, and getting there means comparing the file, because the
        // index on disk still describes the old timestamp. Recording the new one is the write the
        // optional lock exists for, and it is a write into the directory being watched.
        fs::write(root.join("base.txt"), "base\n").unwrap();
        Command::new("touch")
            .args(["-t", "202001010000", "base.txt"])
            .current_dir(root)
            .output()
            .expect("touch is installed");
        // Something genuinely edited, so the read has to be live and not merely quiet.
        fs::write(root.join("edited.txt"), "second\n").unwrap();

        let index = root.join(".git").join("index");
        let before = fs::read(&index).unwrap();
        let output = checked_git(
            root,
            ["status", "--porcelain=v2"],
            "could not read Git status",
        )
        .expect("Git status");
        let reported = String::from_utf8_lossy(&output.stdout);

        assert!(
            reported.contains("edited.txt"),
            "the edit was not reported: {reported}"
        );
        assert!(
            !reported.contains("base.txt"),
            "a file saved unchanged is still unchanged: {reported}"
        );
        assert_eq!(
            before,
            fs::read(&index).unwrap(),
            "reading the status rewrote the index it was reading"
        );
    }

    #[test]
    fn an_unregistered_worktree_writing_its_own_index_is_not_a_repository_wide_change() {
        let mut plan = RepoWatchPlan {
            repo_id: "repo".to_owned(),
            roots: [(PathBuf::from("/repo"), "main".to_owned())]
                .into_iter()
                .collect(),
            git_dirs: BTreeMap::new(),
            common_dir: Some(PathBuf::from("/repo/.git")),
            all: vec!["main".to_owned()],
            requested: vec!["main".to_owned()],
        };
        let event = notify::Event {
            kind: notify::EventKind::Create(notify::event::CreateKind::File),
            paths: vec![PathBuf::from("/repo/.git/worktrees/task/index")],
            attrs: Default::default(),
        };

        // Git writes the new worktree's index while it is still checking the branch out, before
        // the membership signal has named it. Every checkout holds an index of its own, so
        // answering that write with a read of all of them is the cost this signal refuses, and on
        // a backend that does not batch events it is the update that arrives instead.
        assert_eq!(affected_checkouts(&plan, &event), None);

        plan.git_dirs.insert(
            PathBuf::from("/repo/.git/worktrees/task"),
            "task".to_owned(),
        );
        plan.all.push("task".to_owned());

        let update = affected_checkouts(&plan, &event).unwrap();
        assert_eq!(update.status, ["task"]);
    }

    /// The whole chain a worktree an agent created travels, as far as the backend is concerned.
    /// A plan names the worktrees that are registered, so it is built from the database rather
    /// than from disk, and it grows only once something has read the disk. That something is the
    /// sync the worktree signal asks for, and the plan the watcher is then rebuilt for is the one
    /// that has the new worktree's directory and Git directory in it — a plan that did not grow
    /// is a watcher whose reach stops before the worktree the user is about to look at.
    #[test]
    fn a_worktree_added_outside_muster_reaches_the_plan_and_is_therefore_watched() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        let (database, _) = git_database(temp.path(), &root);
        let repo_id = database.load_workspace().unwrap().repos[0].id.clone();
        git(
            &root,
            &[
                "worktree",
                "add",
                "-b",
                "task",
                temp.path().join("task").to_str().unwrap(),
            ],
        );
        // Resolved, because the plan resolves: the filesystem reports resolved paths, and a
        // temporary directory reached through the `/var` symlink names a checkout the plan would
        // not match an event against.
        let added = temp.path().join("task").canonicalize().unwrap();

        // On disk and in Git, but nothing has registered it: the plan still watches the one
        // worktree, which is the whole reason the signal exists.
        let before = super::watch_plan(&database, &repo_id).unwrap();
        assert!(!before.roots.keys().any(|path| path == &added));

        workspace::sync_repo(
            &database,
            &crate::services::agent::AgentService::without_service(),
            &repo_id,
        )
        .unwrap();

        let after = super::watch_plan(&database, &repo_id).unwrap();
        assert!(after.roots.keys().any(|path| path == &added));
        assert_ne!(before, after);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_watch_keeps_requested_ids_for_a_symlink_invalid_checkout() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        let linked = temp.path().join("task");
        let outside = temp.path().join("outside");
        init_repo(&root);
        git(
            &root,
            &["worktree", "add", "-b", "task", linked.to_str().unwrap()],
        );
        let (database, _) = git_database(temp.path(), &root);
        let state = workspace::register_folder(&database, &linked).unwrap();
        let repo = &state.repos[0];
        let mut requested: Vec<_> = repo
            .checkouts
            .iter()
            .filter(|checkout| !checkout.is_missing)
            .map(|checkout| checkout.id.clone())
            .collect();
        requested.sort();
        let primary = repo
            .checkouts
            .iter()
            .find(|checkout| checkout.is_primary)
            .unwrap()
            .id
            .clone();

        fs::remove_dir_all(&linked).unwrap();
        fs::create_dir(&outside).unwrap();
        symlink(&outside, &linked).unwrap();

        let plan = super::watch_plan(&database, &repo.id).unwrap();

        assert_eq!(plan.all, vec![primary]);
        assert_eq!(requested_watch_ids(&plan), requested);
        assert!(watch_plan_matches_request(&plan, &requested));
        assert!(!watch_plan_matches_request(&plan, &plan.all));
        assert_eq!(failed_watch_update(&plan).status, plan.requested);
        let failure = watch_failure(&plan, "registration-7");
        assert_eq!(failure.checkout_ids, requested);
        assert_eq!(failure.registration_id, "registration-7");
        let payload = serde_json::to_value(failure).unwrap();
        assert_eq!(payload["registrationId"], "registration-7");
        assert_eq!(payload["checkoutIds"], serde_json::json!(requested));
    }

    #[test]
    fn a_write_outside_every_worktree_and_the_git_directory_is_not_a_change() {
        let plan = RepoWatchPlan {
            repo_id: "repo".to_owned(),
            roots: [(PathBuf::from("/repo"), "main".to_owned())]
                .into_iter()
                .collect(),
            git_dirs: BTreeMap::new(),
            common_dir: Some(PathBuf::from("/repo/.git")),
            all: vec!["main".to_owned()],
            requested: vec!["main".to_owned()],
        };
        // Objects and logs churn on every Git command. Re-reading every checkout because a
        // blob was written is the cost this whole path exists to avoid.
        for path in ["/repo/.git/objects/aa/object", "/repo/.git/logs/HEAD"] {
            let event = notify::Event {
                kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
                paths: vec![PathBuf::from(path)],
                attrs: Default::default(),
            };
            assert_eq!(affected_checkouts(&plan, &event), None, "{path}");
        }
    }

    #[test]
    fn watcher_ignores_git_object_churn_but_tracks_status_metadata_and_checkout_files() {
        assert!(!should_refresh_path(Path::new(
            "/repo/.git/objects/aa/object"
        )));
        assert!(!should_refresh_path(Path::new("/repo/.git/logs/HEAD")));
        assert!(should_refresh_path(Path::new("/repo/.git/index")));
        assert!(!should_refresh_path(Path::new(
            "/repo/.git/worktrees/task/HEAD"
        )));
        assert!(should_refresh_path(Path::new(
            "/repo/.git/refs/heads/feature"
        )));
        assert!(should_refresh_path(Path::new("/repo/src/file.rs")));
    }

    #[test]
    fn file_activity_only_signals_paths_inside_checkout_outside_git_metadata() {
        let root = Path::new("/repo");
        assert!(super::is_checkout_file_activity(
            Path::new("/repo/src/file.rs"),
            root
        ));
        assert!(!super::is_checkout_file_activity(Path::new("/repo"), root));
        assert!(!super::is_checkout_file_activity(
            Path::new("/repo/.git/index"),
            root
        ));
        assert!(!super::is_checkout_file_activity(
            Path::new("/other/file.rs"),
            root
        ));
    }

    #[test]
    fn activity_carries_the_paths_it_moved_and_leaves_a_spent_batch_unnamed() {
        let update = WatchUpdate {
            status: Vec::new(),
            activity: vec!["checkout:one".to_owned(), "checkout:two".to_owned()],
            worktrees: Vec::new(),
            paths: BTreeMap::from([(
                "checkout:one".to_owned(),
                BTreeSet::from([PathBuf::from("docs/pic.png"), PathBuf::from("src/app.ts")]),
            )]),
            pinned: BTreeSet::new(),
            spent: true,
        };

        assert_eq!(
            file_activity(&update),
            vec![
                FileActivity {
                    checkout_id: "checkout:one".to_owned(),
                    paths: vec!["docs/pic.png".to_owned(), "src/app.ts".to_owned()],
                },
                // The burst spent its budget, so it kept the checkout in its activity and let the
                // paths go. An empty list is the batch declining to say what it moved, which the
                // renderer reads as "ask again", and not as a checkout nothing touched.
                FileActivity {
                    checkout_id: "checkout:two".to_owned(),
                    paths: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn numstat_records_are_keyed_by_the_new_path_of_a_rename_and_skip_uncounted_files() {
        // The two literals are concatenated so no `\0` sits next to a digit, which would
        // read as an octal escape rather than a NUL.
        let parsed = parse_numstat(
            b"2\t0\tbase.txt\0-\t-\tbinary.dat\0\
              1\t0\t\0old name.txt\0new name.txt\0\
              2\t3\tweird\tname.txt\0",
        )
        .unwrap();

        assert_eq!(parsed.len(), 3);
        assert_eq!(
            parsed["base.txt"],
            GitDiffStats {
                additions: 2,
                deletions: 0
            }
        );
        // A rename is keyed by the path the file list shows, not the one it came from.
        assert_eq!(
            parsed["new name.txt"],
            GitDiffStats {
                additions: 1,
                deletions: 0
            }
        );
        // A tab inside a path stays inside that path, because only NUL ends a record.
        assert_eq!(
            parsed["weird\tname.txt"],
            GitDiffStats {
                additions: 2,
                deletions: 3
            }
        );
        // A file Git could not count has no numbers, rather than zero lines.
        assert!(!parsed.contains_key("binary.dat"));
        assert!(parse_numstat(b"not a record\0").is_err());
    }

    /// `bytes` of text with a newline every third byte, so a test can ask for a file larger than
    /// the block the count reads it in and still know what it put in it. It is built rather than
    /// written out because what matters about these files is how many bytes they are.
    fn text_of(bytes: usize) -> Vec<u8> {
        let mut file = vec![b'\n'; bytes];
        for (index, byte) in file.iter_mut().enumerate() {
            if index % 3 == 0 {
                *byte = b'a' + (index % 26) as u8;
            }
        }
        file
    }

    /// The same text with its one NUL byte at `at`, which is how a test asks for a file Git calls
    /// binary and for one it does not, at a size where the answer is not in doubt either way.
    fn text_with_a_nul_at(bytes: usize, at: usize) -> Vec<u8> {
        let mut file = text_of(bytes);
        file[at] = 0;
        file
    }

    /// What Git reports for a file, read off the bytes rather than asked of Git: a NUL byte
    /// anywhere in the window it looks at and nothing else makes the file binary, and otherwise
    /// the file has one line per newline, plus a last line when that one has no newline of its
    /// own.
    fn lines_git_reports(bytes: &[u8]) -> Option<u64> {
        if bytes.iter().take(BINARY_SNIFF_BYTES).any(|byte| *byte == 0) {
            return None;
        }
        let newlines = bytes.iter().filter(|byte| **byte == b'\n').count() as u64;
        Some(newlines + u64::from(!bytes.is_empty() && !bytes.ends_with(b"\n")))
    }

    /// What `git diff --no-index --numstat /dev/null <file>` prints, which is the answer the
    /// in-process count has to agree with.
    fn git_says_for_an_untracked_file(root: &Path, name: &str) -> Option<GitDiffStats> {
        let output = Command::new("git")
            .args([
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--numstat",
                "--no-index",
                "--",
                "/dev/null",
                name,
            ])
            .current_dir(root)
            .output()
            .expect("Git is installed");
        assert!(
            matches!(output.status.code(), Some(0) | Some(1)),
            "git diff --no-index failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let record = output
            .stdout
            .split(|byte| *byte == b'\n')
            .next()
            .unwrap_or(b"");
        let mut columns = record.split(|byte| *byte == b'\t');
        let additions = columns.next()?;
        let deletions = columns.next()?;
        // Git prints `-` for both columns of a file it declined to count, and so does a row
        // here: no numbers at all, rather than a zero nobody measured.
        if additions == b"-" || deletions == b"-" {
            return None;
        }
        Some(GitDiffStats {
            additions: String::from_utf8_lossy(additions).parse().ok()?,
            deletions: String::from_utf8_lossy(deletions).parse().ok()?,
        })
    }

    /// The in-process count is a reimplementation of what Git does with `git diff --no-index`
    /// against an empty file, so the only honest way to hold it to that is to ask Git.
    #[test]
    fn counting_an_untracked_file_agrees_with_asking_git_to_diff_it_against_nothing() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        let mut cases: Vec<(&str, &[u8])> = vec![
            ("empty.txt", b""),
            ("one-line.txt", b"a\n"),
            // The last line has no newline of its own and is still a line.
            ("no-trailing-newline.txt", b"a\nb\nc"),
            ("three-lines.txt", b"a\nb\nc\n"),
            ("blank-lines.txt", b"\n\n\n"),
            ("crlf.txt", b"a\r\nb\r\n"),
            // A NUL byte is what makes Git call a file binary, and it looks only at the head.
            ("binary.dat", &[0, 1, 2, 3]),
            ("binary-late.dat", b"aaaa\nbbbb\n\0\n"),
        ];
        // The count reads a file a block at a time, so the files only it can get wrong are the
        // ones it cannot read in one go: a boundary crossed inside the file, and a NUL byte past
        // the window Git looks at. Git counts both of them, and reads them whole to do it.
        let built: Vec<(&str, Vec<u8>)> = vec![
            ("multi-block.txt", text_of(2 * COUNT_BLOCK_BYTES + 13)),
            (
                "multi-block-no-trailing-newline.txt",
                text_of(2 * COUNT_BLOCK_BYTES),
            ),
            (
                "nul-past-the-window.txt",
                text_with_a_nul_at(2 * COUNT_BLOCK_BYTES + 13, BINARY_SNIFF_BYTES),
            ),
        ];
        for (name, bytes) in &built {
            cases.push((name, bytes));
        }
        for (name, bytes) in &cases {
            fs::write(root.join(name), bytes).unwrap();
        }

        for (name, _) in &cases {
            assert_eq!(
                untracked_line_count(&root.join(name)),
                git_says_for_an_untracked_file(&root, name),
                "{name} counted differently than Git counts it"
            );
        }
    }

    /// What a count owes a file too large to be read in one block: the number of lines its bytes
    /// have, whichever block they arrive in. A last line that ends exactly where a block ends is
    /// the case a reader gets wrong most easily, so both of those are here.
    #[test]
    fn a_file_larger_than_one_block_is_counted_by_the_newlines_it_has() {
        let temp = tempdir().unwrap();
        let two_blocks = text_of(2 * COUNT_BLOCK_BYTES);
        let without_a_last_newline = {
            let mut file = two_blocks.clone();
            file.pop();
            file
        };
        let cases: [(&str, Vec<u8>); 5] = [
            ("multi-block.txt", text_of(2 * COUNT_BLOCK_BYTES + 13)),
            (
                "multi-block-no-trailing-newline.txt",
                without_a_last_newline,
            ),
            // Two whole blocks, so the last line is the one that lands on the edge.
            ("ends-on-a-block-edge.txt", two_blocks),
            // The NUL byte is one past the window Git looks at, which is as far from the head as
            // a file can be and still be text. Reading the head rather than the file is allowed
            // to change what a count costs, never the number it reports.
            (
                "nul-past-the-window.txt",
                text_with_a_nul_at(2 * COUNT_BLOCK_BYTES + 13, BINARY_SNIFF_BYTES),
            ),
            // One inside the window is the file Git declines to count, however large it is, and
            // a file Git declines to count has no number rather than a zero.
            (
                "binary-big.dat",
                text_with_a_nul_at(2 * COUNT_BLOCK_BYTES + 13, 4),
            ),
        ];

        for (name, bytes) in &cases {
            fs::write(temp.path().join(name), bytes).unwrap();

            assert_eq!(
                untracked_line_count(&temp.path().join(name)),
                lines_git_reports(bytes).map(|additions| GitDiffStats {
                    additions,
                    deletions: 0
                }),
                "{name} counted differently than the bytes of it say"
            );
        }
    }

    #[test]
    fn an_untracked_file_git_cannot_read_is_left_out_rather_than_counted_as_zero() {
        let temp = tempdir().unwrap();
        assert_eq!(untracked_line_count(&temp.path().join("gone.txt")), None);
    }

    #[test]
    fn counts_lines_against_the_same_base_as_the_file_list() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        fs::write(root.join("gone.txt"), "gone\n").unwrap();
        fs::write(root.join("old name.txt"), "one\ntwo\n").unwrap();
        git(&root, &["add", "gone.txt", "old name.txt"]);
        git(&root, &["commit", "-m", "more files"]);
        let (database, checkout_id) = git_database(temp.path(), &root);
        let watchers = GitWatcherManager::default();

        git(&root, &["mv", "old name.txt", "new name.txt"]);
        fs::write(root.join("new name.txt"), "one\ntwo\nthree\n").unwrap();
        fs::write(root.join("base.txt"), "base\none\ntwo\n").unwrap();
        fs::write(root.join("added.txt"), "one\ntwo\n").unwrap();
        fs::write(root.join("binary.dat"), [0, 1, 2]).unwrap();
        fs::remove_file(root.join("gone.txt")).unwrap();

        let listed = status(&database, &watchers, &checkout_id).unwrap();
        let counted = super::diff_stats(&database, &watchers, &checkout_id).unwrap();
        let totals = super::checkout_diff_stats(&database, &watchers).unwrap();

        // The counted list is the same list, with the same paths in the same order, so a row
        // and the numbers on it can never describe different things.
        let paths: Vec<_> = counted.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(
            paths,
            listed
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>()
        );
        let counts = |path: &str| {
            let file = counted.iter().find(|file| file.path == path).unwrap();
            (file.additions, file.deletions)
        };
        assert_eq!(counts("base.txt"), (Some(2), Some(0)));
        assert_eq!(counts("added.txt"), (Some(2), Some(0)));
        assert_eq!(counts("gone.txt"), (Some(0), Some(1)));
        // A rename is counted under the path the file list shows, not the one it came from.
        assert_eq!(counts("new name.txt"), (Some(1), Some(0)));
        // Git has no count for a binary file, and the row says so instead of showing zero.
        assert_eq!(counts("binary.dat"), (None, None));
        // The per-checkout total is exactly the sum of its files.
        assert_eq!(
            totals[&checkout_id],
            GitDiffStats {
                additions: 5,
                deletions: 1
            }
        );
    }

    #[test]
    fn counts_default_to_the_whole_change_set_rather_than_the_default_branch_tip() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        git(&root, &["checkout", "-b", "feature"]);
        fs::write(root.join("feature.txt"), "feature\n").unwrap();
        git(&root, &["add", "feature.txt"]);
        git(&root, &["commit", "-m", "feature"]);
        git(&root, &["checkout", "trunk"]);
        fs::write(root.join("default-only.txt"), "default\n").unwrap();
        git(&root, &["add", "default-only.txt"]);
        git(&root, &["commit", "-m", "new default commit"]);
        git(&root, &["checkout", "feature"]);
        let (database, checkout_id) = git_database(temp.path(), &root);
        let watchers = GitWatcherManager::default();

        let listed = status(&database, &watchers, &checkout_id).unwrap();
        let counted = super::diff_stats(&database, &watchers, &checkout_id).unwrap();
        let counts: BTreeMap<_, _> = counted
            .iter()
            .map(|file| (file.path.as_str(), (file.additions, file.deletions)))
            .collect();

        assert_eq!(counts["feature.txt"], (Some(1), Some(0)));
        assert!(!counts.contains_key("default-only.txt"));
        assert!(!listed
            .files
            .iter()
            .any(|file| file.path == "default-only.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn sec_07_diff_stats_refuses_an_untracked_path_that_leaves_the_checkout() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "secret\n").unwrap();
        let (database, checkout_id) = git_database(temp.path(), &root);
        symlink(&outside, root.join("link")).unwrap();
        fs::write(root.join("added.txt"), "one\n").unwrap();

        let counted =
            super::diff_stats(&database, &GitWatcherManager::default(), &checkout_id).unwrap();
        let counts: BTreeMap<_, _> = counted
            .iter()
            .map(|file| (file.path.as_str(), file.additions))
            .collect();

        // A path that stays inside the checkout is still counted: the refusal is containment.
        assert_eq!(counts["added.txt"], Some(1));
        assert_eq!(counts["link"], None);
        assert!(!counts.contains_key("secret.txt"));
    }

    #[test]
    fn sec_08_diff_stats_answers_only_for_registered_git_checkouts() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("plain");
        fs::create_dir_all(&root).unwrap();
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        let state = workspace::register_folder(&database, &root).unwrap();
        let watchers = GitWatcherManager::default();

        let plain = state.repos[0].checkouts[0].id.clone();
        let error = super::diff_stats(&database, &watchers, &plain).unwrap_err();
        assert!(matches!(error.code, IpcErrorCode::InvalidCheckout));
        let error = super::diff_stats(&database, &watchers, "checkout:unknown").unwrap_err();
        assert!(matches!(error.code, IpcErrorCode::InvalidCheckout));
        // A plain folder is left out of the totals instead of being reported as a clean bill.
        assert_eq!(state.repos[0].kind, RepoKind::Plain);
        assert!(super::checkout_diff_stats(&database, &watchers)
            .unwrap()
            .is_empty());
    }

    fn cached_state(counts: Option<GitCounts>) -> CachedGitSnapshot {
        CachedGitSnapshot {
            default_branch: Some("trunk".to_owned()),
            revision: 0,
            stale: false,
            snapshot: super::Snapshot {
                status: GitStatus {
                    branch: None,
                    head: None,
                    default_branch: "trunk".to_owned(),
                    ahead_count: 0,
                    files: Vec::new(),
                },
                merge_base: "abc123".to_owned(),
            },
            counts,
        }
    }

    #[test]
    fn runtime_notify_error_is_reported_and_invalidates_the_repository_snapshot() {
        let plan = RepoWatchPlan {
            repo_id: "repo".to_owned(),
            all: vec!["checkout:a".to_owned(), "checkout:b".to_owned()],
            requested: vec!["checkout:a".to_owned(), "checkout:b".to_owned()],
            ..RepoWatchPlan::default()
        };
        let cache = GitSnapshotCache::default();
        for checkout_id in &plan.all {
            cache.insert(checkout_id.clone(), cached_state(None));
            assert!(cache.fresh(checkout_id, Some("trunk")).is_some());
        }

        let (queue, inbox) = WatchQueue::new();
        let failed = AtomicBool::new(false);
        forward_watch_result(
            Err(notify::Error::generic("watch stream failed")),
            &plan,
            &queue,
            &failed,
        );
        assert_eq!(
            receive_debounced_change(&inbox, Duration::ZERO, Duration::ZERO),
            WatchWakeup::Failed("watch stream failed".to_owned())
        );

        let update = invalidate_failed_watch(&cache, &plan);
        assert_eq!(update.status, plan.all);
        assert!(cache.fresh("checkout:a", Some("trunk")).is_none());
        assert!(cache.fresh("checkout:b", Some("trunk")).is_none());

        // One terminal error fails the watcher; repeated callback errors do not queue retries.
        forward_watch_result(
            Err(notify::Error::generic("second error")),
            &plan,
            &queue,
            &failed,
        );
        assert!(inbox.try_recv().is_err());
        assert!(failed.load(Ordering::Acquire));
    }

    #[test]
    fn a_checkout_is_read_once_until_its_watcher_says_a_change_moved_it() {
        let cache = GitSnapshotCache::default();

        assert!(cache.fresh("checkout:a", Some("trunk")).is_none());
        cache.insert("checkout:a".to_owned(), cached_state(None));
        // The file list is held, which is what lets a `git status` and the counts of the same
        // checkout be one reading rather than two.
        assert_eq!(
            cache
                .fresh("checkout:a", Some("trunk"))
                .map(|held| held.snapshot.merge_base),
            Some("abc123".to_owned())
        );
        // Another default branch is another base ref, which no watcher can see.
        assert!(cache.fresh("checkout:a", Some("main")).is_none());
        cache.mark_stale(&["checkout:a".to_owned()]);
        assert!(cache.fresh("checkout:a", Some("trunk")).is_none());
        cache.insert("checkout:a".to_owned(), cached_state(None));
        assert_eq!(
            cache
                .fresh("checkout:a", Some("trunk"))
                .map(|held| held.snapshot.merge_base),
            Some("abc123".to_owned())
        );
        cache.forget_many(&["checkout:a".to_owned()]);
        assert!(cache.fresh("checkout:a", Some("trunk")).is_none());
    }

    #[test]
    fn a_change_drops_the_checkouts_it_speaks_for_and_leaves_the_others_held() {
        let cache = GitSnapshotCache::default();
        cache.insert("checkout:a".to_owned(), cached_state(None));
        cache.insert("checkout:b".to_owned(), cached_state(None));

        // A commit in one worktree moves the merge base every sibling counts against, so the
        // signal names all of them. A save in one names only that one, and the rest of the
        // sidebar is a lookup rather than another walk of the same working trees.
        cache.mark_stale(&["checkout:a".to_owned()]);
        assert!(cache.fresh("checkout:a", Some("trunk")).is_none());
        assert!(cache.fresh("checkout:b", Some("trunk")).is_some());
    }

    #[test]
    fn an_entry_holds_the_counts_once_something_has_asked_for_them() {
        let cache = GitSnapshotCache::default();
        let counts = GitCounts {
            files: Vec::new(),
            totals: GitDiffStats {
                additions: 7,
                deletions: 1,
            },
        };
        cache.insert("checkout:a".to_owned(), cached_state(Some(counts.clone())));

        // The sidebar and the Changes tab both want the lines on the same change, and neither
        // should pay for the other.
        assert_eq!(
            cache
                .fresh("checkout:a", Some("trunk"))
                .and_then(|held| held.counts),
            Some(counts)
        );
    }

    #[test]
    fn a_refresh_started_before_invalidation_cannot_repopulate_the_cache() {
        let cache = GitSnapshotCache::default();
        let checkout_id = "checkout:a".to_owned();
        cache.insert(checkout_id.clone(), cached_state(None));
        let refresh_revision = cache.revision(&checkout_id);

        cache.mark_stale(std::slice::from_ref(&checkout_id));
        cache.insert_at(checkout_id.clone(), cached_state(None), refresh_revision);

        assert!(cache.fresh(&checkout_id, Some("trunk")).is_none());
    }

    #[test]
    fn archiving_and_restoring_a_worktree_invalidates_its_cached_status_and_counts() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        let worktree = temp.path().join("task");
        init_repo(&root);
        git(
            &root,
            &["worktree", "add", "-b", "task", worktree.to_str().unwrap()],
        );
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        let state = workspace::register_folder(&database, &root).unwrap();
        let repo_id = state.repos[0].id.clone();
        let worktree_path = worktree.canonicalize().unwrap();
        let checkout_id = state.repos[0]
            .checkouts
            .iter()
            .find(|checkout| checkout.canonical_path == worktree_path.to_string_lossy().as_ref())
            .unwrap()
            .id
            .clone();
        workspace::set_default_branch(&database, &repo_id, "trunk").unwrap();

        let app = tauri::test::mock_app();
        let watchers = GitWatcherManager::default();
        watchers
            .watch(
                app.handle().clone(),
                repo_id.clone(),
                "initial".to_owned(),
                super::watch_plan(&database, &repo_id).unwrap(),
            )
            .unwrap();
        assert!(status(&database, &watchers, &checkout_id)
            .unwrap()
            .files
            .is_empty());
        assert!(super::diff_stats(&database, &watchers, &checkout_id)
            .unwrap()
            .is_empty());

        workspace::archive_checkout(
            &database,
            &crate::services::agent::AgentService::without_service(),
            &checkout_id,
        )
        .unwrap();
        watchers
            .watch(
                app.handle().clone(),
                repo_id.clone(),
                "archived".to_owned(),
                super::watch_plan(&database, &repo_id).unwrap(),
            )
            .unwrap();
        assert!(watchers.cached_state(&checkout_id, Some("trunk")).is_none());

        fs::write(worktree.join("after-archive.txt"), "changed\n").unwrap();
        workspace::restore_archived_worktrees(
            &database,
            &crate::services::agent::AgentService::without_service(),
            &repo_id,
        )
        .unwrap();
        watchers
            .watch(
                app.handle().clone(),
                repo_id.clone(),
                "restored".to_owned(),
                super::watch_plan(&database, &repo_id).unwrap(),
            )
            .unwrap();

        assert!(status(&database, &watchers, &checkout_id)
            .unwrap()
            .files
            .iter()
            .any(|file| file.path == "after-archive.txt"));
        let counts = super::diff_stats(&database, &watchers, &checkout_id).unwrap();
        assert_eq!(
            counts
                .iter()
                .find(|file| file.path == "after-archive.txt")
                .unwrap()
                .additions,
            Some(1)
        );
        watchers.unwatch(&repo_id, &[]);
    }

    #[test]
    fn a_status_and_the_counts_of_one_checkout_are_the_same_reading() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        fs::write(root.join("added.txt"), "one\ntwo\n").unwrap();
        let (database, checkout_id) = git_database(temp.path(), &root);
        let watchers = GitWatcherManager::default();

        let listed = status(&database, &watchers, &checkout_id).unwrap();
        let counted = super::diff_stats(&database, &watchers, &checkout_id).unwrap();
        let totals = super::checkout_diff_stats(&database, &watchers).unwrap();

        // The file list the status reports and the file list the counts decorate are the same
        // list, which is only true if both came from one snapshot.
        assert_eq!(
            counted
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            listed
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            totals[&checkout_id],
            GitDiffStats {
                additions: 2,
                deletions: 0
            }
        );
    }

    /// The sidebar and the Changes tab ask about one checkout at the same time, and each read is a
    /// burst of `git` processes over the same working tree. N callers arriving together must cost
    /// one read, and the process count is the only witness: the answers would be identical either
    /// way, because they are the same answer computed once or several times.
    #[test]
    fn readers_that_arrive_while_one_checkout_is_being_read_share_that_read() {
        let readers = 8;
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        fs::write(root.join("added.txt"), "one\ntwo\n").unwrap();
        let (database, checkout_id) = git_database(temp.path(), &root);

        let alone = GitWatcherManager::default();
        // Read around this one read rather than after it. Registering the checkout above starts Git
        // of its own, and whether those calls are counted against this checkout's root is whether the
        // temporary directory's path is a symlink: `/var` is one on macOS and `/tmp` is not on Linux,
        // so a total taken afterwards counts the registration on one platform and not on the other.
        // The delta is one read of the checkout on both, which is what the readers below are held to.
        let before_alone = git_reads(&root);
        super::diff_stats(&database, &alone, &checkout_id).unwrap();
        let one_read = git_reads(&root) - before_alone;

        // Every reader is released at once, so the ones that did not do the read all arrive while it
        // is still running rather than after it has cached its answer.
        let together = &GitWatcherManager::default();
        let before_readers = git_reads(&root);
        let database = &database;
        let released = Arc::new(Barrier::new(readers));
        let watched = checkout_id.clone();
        let answers: Vec<_> = thread::scope(|scope| {
            let reads: Vec<_> = (0..readers)
                .map(|_| {
                    let released = Arc::clone(&released);
                    let watched = watched.clone();
                    scope.spawn(move || {
                        released.wait();
                        super::diff_stats(database, together, &watched)
                            .unwrap()
                            .iter()
                            .map(|file| file.additions)
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            reads.into_iter().map(|read| read.join().unwrap()).collect()
        });

        assert!(
            answers.iter().all(|additions| *additions == answers[0]),
            "the readers did not all get the same reading: {answers:?}"
        );
        assert_eq!(answers[0], [Some(2)]);
        // At most one read, and not one read exactly. The first read of a checkout discovers which
        // ref its base is -- `show-ref`, then whether the remote has it -- and every read after it
        // reuses that answer, so readers arriving together pay two processes less than the read that
        // warmed it. What must not happen is each of them starting one: that costs eight times what
        // a single read does here, which is what this bound is here to fail on.
        let readers_cost = git_reads(&root) - before_readers;
        assert!(
            readers_cost <= one_read,
            "{readers} readers of one checkout must cost one read, not {readers}: \
             one read started {one_read} processes and the readers started {readers_cost}"
        );
    }

    /// The rule the sharing rests on: a read in the map is the read a caller that arrives joins, and
    /// a read that has already published is still in the map for the moment between publishing and
    /// leaving it. A caller arriving there must get that answer rather than start a second read.
    #[test]
    fn a_read_that_published_is_still_shared_until_it_leaves_the_map() {
        let readings: Mutex<super::Readings<GitCounts>> = Mutex::new(BTreeMap::new());
        let checkout_a = ("checkout:a".to_owned(), Some("trunk".to_owned()), Some(0));
        let checkout_b = ("checkout:b".to_owned(), Some("trunk".to_owned()), Some(0));
        let answered = GitCounts {
            files: Vec::new(),
            totals: GitDiffStats {
                additions: 4,
                deletions: 1,
            },
        };
        let first = shared_read(&readings, &checkout_a, || Ok(answered.clone())).unwrap();

        // The map is empty again, which is what lets the next change be read rather than shared.
        assert!(readings.lock().unwrap().is_empty());
        assert_eq!(first.totals, answered.totals);

        // A caller that arrives while a read is running gets its answer, and does not read.
        let reading = ReadingInFlight::new();
        reading.publish(Ok(answered.clone()));
        readings
            .lock()
            .unwrap()
            .insert(checkout_a.clone(), Arc::clone(&reading));
        let late = shared_read(&readings, &checkout_a, || {
            panic!("a caller joined a published read and read the tree again")
        })
        .unwrap();

        assert_eq!(late, answered);
        // A read of another checkout is its own read, and a read that failed publishes the failure
        // rather than letting the next caller ask again.
        let failed = shared_read(&readings, &checkout_b, || {
            Err(IpcError::new(
                IpcErrorCode::GitFailed,
                "Git could not answer",
            ))
        });
        assert!(matches!(failed, Err(error) if error.code == IpcErrorCode::GitFailed));
    }

    #[test]
    fn a_reader_after_invalidation_does_not_join_the_previous_revision() {
        let cache = GitSnapshotCache::default();
        let checkout_id = "checkout:a".to_owned();
        let branch = Some("trunk".to_owned());
        let old_revision = cache.revision(&checkout_id);
        let old_key = (checkout_id.clone(), branch.clone(), old_revision);
        let started = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let mut old_state = cached_state(None);
        old_state.snapshot.merge_base = "old".to_owned();
        let mut new_state = cached_state(None);
        new_state.snapshot.merge_base = "new".to_owned();

        thread::scope(|scope| {
            let cache_ref = &cache;
            let counts = &cache_ref.reading_counts;
            let old_started = Arc::clone(&started);
            let old_release = Arc::clone(&release);
            let old_checkout_id = checkout_id.clone();
            let old = scope.spawn(move || {
                shared_read(counts, &old_key, || {
                    old_started.wait();
                    old_release.wait();
                    cache_ref.insert_at(old_checkout_id, old_state, old_revision);
                    Ok(GitCounts {
                        files: Vec::new(),
                        totals: GitDiffStats {
                            additions: 1,
                            deletions: 0,
                        },
                    })
                })
            });

            started.wait();
            cache.mark_stale(std::slice::from_ref(&checkout_id));
            let new_revision = cache.revision(&checkout_id);
            let new_key = (checkout_id.clone(), branch, new_revision);
            assert_eq!(
                shared_read(&cache.reading_counts, &new_key, || {
                    cache.insert_at(checkout_id.clone(), new_state, new_revision);
                    Ok(GitCounts {
                        files: Vec::new(),
                        totals: GitDiffStats {
                            additions: 2,
                            deletions: 0,
                        },
                    })
                })
                .unwrap()
                .totals
                .additions,
                2
            );
            release.wait();
            assert_eq!(old.join().unwrap().unwrap().totals.additions, 1);
        });

        assert_eq!(
            cache
                .fresh(&checkout_id, Some("trunk"))
                .unwrap()
                .snapshot
                .merge_base,
            "new"
        );
    }

    /// A build writes thousands of files under directories Git ignores, and none of them is part of
    /// the change set, so the snapshot does not move and there is nothing to re-read. The activity
    /// still goes out: the file explorer has to show what was just written.
    #[test]
    fn a_file_under_an_ignored_directory_is_activity_but_not_a_reason_to_re_read() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::write(root.join("node_modules/pkg/index.js"), "x\n").unwrap();
        let (database, checkout_id) = git_database(temp.path(), &root);
        let repo_id = database.load_workspace().unwrap().repos[0].id.clone();
        let plan = super::watch_plan(&database, &repo_id).unwrap();
        // Resolved, because the plan watches the resolved path it registered and the filesystem
        // reports resolved paths: an event under the symlinked temp path would match no checkout.
        let root = root.canonicalize().unwrap();

        let update = affected_checkouts(
            &plan,
            &event(root.join("node_modules/pkg/index.js").to_str().unwrap()),
        )
        .expect("the write is inside the checkout");
        assert_eq!(update.status, std::slice::from_ref(&checkout_id));
        let answered = without_ignored_only(&plan, update);

        assert!(
            answered.status.is_empty(),
            "an ignored file is not part of the change set and must not invalidate: {:?}",
            answered.status
        );
        assert_eq!(answered.activity, [checkout_id]);
    }

    /// The same write next to a tracked one: the tracked file is part of the change set, so the
    /// checkout is re-read even though the ignored one is beside it. A batch is one answer, so one
    /// path Git cannot ignore is enough to keep the whole thing.
    #[test]
    fn one_tracked_file_in_a_batch_keeps_the_batch_from_being_ignored() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::write(root.join("node_modules/pkg/index.js"), "x\n").unwrap();
        fs::write(root.join("tracked.txt"), "changed\n").unwrap();
        let (database, checkout_id) = git_database(temp.path(), &root);
        let repo_id = database.load_workspace().unwrap().repos[0].id.clone();
        let plan = super::watch_plan(&database, &repo_id).unwrap();
        let root = root.canonicalize().unwrap();

        let mut update = WatchSlot::holding(WatchUpdate::default());
        for moved in ["node_modules/pkg/index.js", "tracked.txt"] {
            update.merge_update(
                affected_checkouts(&plan, &event(root.join(moved).to_str().unwrap()))
                    .expect("the write is inside the checkout"),
            );
        }
        assert_eq!(update.update.paths[&checkout_id].len(), 2);

        let answered = without_ignored_only(&plan, update.take());

        assert_eq!(answered.status, [checkout_id]);
    }

    /// Git, not a list of directory names. A file tracked inside a directory the rules ignore is
    /// part of the change set whatever `.gitignore` says about the directory, and only Git knows
    /// which files are tracked. This is the case a hardcoded `node_modules`-style list gets wrong,
    /// because a user can ignore a directory and then force-add a file inside it.
    #[test]
    fn a_tracked_file_inside_an_ignored_directory_still_invalidates() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        fs::write(root.join(".gitignore"), "vendored/\n").unwrap();
        fs::create_dir_all(root.join("vendored")).unwrap();
        fs::write(root.join("vendored/kept.js"), "kept\n").unwrap();
        // Forced into the index under the directory the rules ignore, which is what `git add -f`
        // is for and what a vendored dependency in an ignored folder looks like.
        git(&root, &["add", "-f", "vendored/kept.js"]);
        git(&root, &["commit", "-m", "vendored"]);
        let (database, checkout_id) = git_database(temp.path(), &root);
        let repo_id = database.load_workspace().unwrap().repos[0].id.clone();
        let plan = super::watch_plan(&database, &repo_id).unwrap();
        let root = root.canonicalize().unwrap();

        // Git itself is the witness that the case is real: the file is in the index under a
        // directory the rules ignore, so editing it is a change the change set has to describe.
        git(&root, &["ls-files", "--error-unmatch", "vendored/kept.js"]);
        fs::write(root.join("vendored/kept.js"), "kept and edited\n").unwrap();
        assert!(
            status(&database, &GitWatcherManager::default(), &checkout_id)
                .unwrap()
                .files
                .iter()
                .any(|file| file.path == "vendored/kept.js")
        );

        let update = affected_checkouts(
            &plan,
            &event(root.join("vendored/kept.js").to_str().unwrap()),
        )
        .unwrap();
        let answered = without_ignored_only(&plan, update);

        assert_eq!(
            answered.status,
            [checkout_id],
            "a tracked file is part of the change set however its directory is ignored"
        );
    }

    /// The dominant rule: a stale row is worse than a slow one. Anything this cannot answer -- Git
    /// not being there, a deadline, a fatal error over a path -- is read as "not ignored", so the
    /// checkout is re-read rather than trusted.
    #[test]
    fn a_check_ignore_that_cannot_answer_invalidates_rather_than_trusting() {
        let temp = tempdir().unwrap();
        // Not a repository, so `check-ignore` has nothing to answer with.
        let root = temp.path().join("not-a-repo");
        fs::create_dir_all(&root).unwrap();
        let plan = RepoWatchPlan {
            repo_id: "repo".to_owned(),
            roots: [(root.clone(), "main".to_owned())].into_iter().collect(),
            all: vec!["main".to_owned()],
            ..RepoWatchPlan::default()
        };
        let update = WatchUpdate {
            status: vec!["main".to_owned()],
            activity: vec!["main".to_owned()],
            paths: BTreeMap::from([(
                "main".to_owned(),
                BTreeSet::from([PathBuf::from("build/out.js")]),
            )]),
            ..WatchUpdate::default()
        };

        assert_eq!(
            ignored_paths(&root, &[PathBuf::from("build/out.js")]),
            None,
            "Git answered a question about a directory that is not a repository"
        );
        assert_eq!(without_ignored_only(&plan, update).status, ["main"]);
    }

    /// The query has to be one call per checkout for the whole burst, not one per file: a build
    /// writes thousands of files and the cost this path exists to remove is a burst of `git`
    /// processes per file. One path per process would make it worse than the re-read it saves.
    #[test]
    fn a_burst_asks_git_once_for_the_whole_batch_and_not_once_per_file() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
        let files = 200;
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        for file in 0..files {
            fs::write(
                root.join(format!("node_modules/pkg/{file}.js")),
                format!("{file}\n"),
            )
            .unwrap();
        }
        let (database, checkout_id) = git_database(temp.path(), &root);
        let repo_id = database.load_workspace().unwrap().repos[0].id.clone();
        let plan = super::watch_plan(&database, &repo_id).unwrap();
        let root = root.canonicalize().unwrap();

        let mut update = WatchSlot::holding(WatchUpdate::default());
        for file in 0..files {
            let path = root
                .join(format!("node_modules/pkg/{file}.js"))
                .to_str()
                .unwrap()
                .to_owned();
            update.merge_update(
                affected_checkouts(&plan, &event(&path)).expect("the write is inside the checkout"),
            );
        }
        // The merged burst is the whole batch, deduped: one answer per path rather than one per
        // event, which is what keeps the set from growing with the burst.
        assert_eq!(update.update.paths[&checkout_id].len(), files);

        let before = git_reads(&root);
        let answered = without_ignored_only(&plan, update.take());

        assert!(answered.status.is_empty());
        assert_eq!(answered.activity, [checkout_id]);
        assert_eq!(
            git_reads(&root) - before,
            1,
            "{files} files of one burst must be one question to Git"
        );
    }

    /// A write in a checkout's own Git directory moves its index, and a write in the directory every
    /// worktree shares moves the merge base they all count against. Neither is a file, so neither can
    /// be argued out of the re-read by a file that Git ignores.
    #[test]
    fn a_write_in_git_metadata_is_re_read_whatever_the_files_beside_it_are() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        init_repo(&root);
        fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
        let (database, _) = git_database(temp.path(), &root);
        let repo_id = database.load_workspace().unwrap().repos[0].id.clone();
        let plan = super::watch_plan(&database, &repo_id).unwrap();
        let primary = plan.all[0].clone();
        let root = root.canonicalize().unwrap();

        for path in [
            root.join(".git/index").to_str().unwrap(),
            root.join(".git/refs/heads/trunk").to_str().unwrap(),
        ] {
            let mut update =
                affected_checkouts(&plan, &event(path)).expect("the write is metadata");
            update
                .paths
                .entry(primary.clone())
                .or_default()
                .insert(PathBuf::from("node_modules/pkg/index.js"));
            assert!(
                update.pinned.contains(&primary),
                "{path} is not marked as something a file cannot argue away"
            );
            assert_eq!(
                without_ignored_only(&plan, update).status,
                std::slice::from_ref(&primary)
            );
        }
    }
}
