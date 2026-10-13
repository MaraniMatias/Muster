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
    collections::HashSet,
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
const SCHEMA_VERSION: u32 = 1;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Both the shared repository and the particular checkout the user opened.
#[derive(Debug)]
pub struct OpenedDirectory {
    pub repo: Repo,
    pub checkout_id: String,
}

/// Open a canonical folder, discovering all registered worktrees if it is Git.
///
/// This does not register anything, start a shell, run Git mutations, or merge
/// existing sessions. The host retains its live sessions when merging the result.
pub fn open_directory(path: &Path) -> Result<OpenedDirectory, String> {
    open_directory_within(path, GIT_READ_TIMEOUT)
}

fn open_directory_within(path: &Path, within: Duration) -> Result<OpenedDirectory, String> {
    let requested = path
        .canonicalize()
        .map_err(|error| format!("could not open folder {}: {error}", path.display()))?;
    if !requested.is_dir() {
        return Err("selected path is not a folder".into());
    }
    path_text(&requested)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("could not read the current time: {error}"))?
        .as_secs()
        .to_string();
    let bare = git_output(&requested, &["rev-parse", "--is-bare-repository"], within)?;
    if !bare.status.success() {
        if is_plain_directory(&requested, &bare)? {
            let repo = Repo::plain(&requested, now)?;
            return Ok(OpenedDirectory {
                checkout_id: repo.checkouts[0].id.clone(),
                repo,
            });
        }
        return Err(git_error("could not inspect Git repository", &bare));
    }
    if output_text(&bare)? == "true" {
        return Err("bare repositories are not supported; open a working checkout instead".into());
    }
    let root = git_success(&requested, &["rev-parse", "--show-toplevel"], within)?;
    let checkout_root = PathBuf::from(output_text(&root)?)
        .canonicalize()
        .map_err(|error| format!("could not resolve Git checkout root: {error}"))?;
    let listed = git_success(
        &checkout_root,
        &["worktree", "list", "--porcelain", "-z"],
        within,
    )?;
    let mut worktrees = parse_worktrees(&listed.stdout)?;
    let opened_is_listed = worktrees
        .iter()
        .any(|entry| canonical_or_original(&entry.path).is_ok_and(|path| path == checkout_root));
    if !opened_is_listed {
        let superproject = git_success(
            &checkout_root,
            &["rev-parse", "--show-superproject-working-tree"],
            within,
        )?;
        if output_text(&superproject)?.is_empty() {
            return Err(format!(
                "Git did not list the containing checkout: {}",
                checkout_root.display()
            ));
        }
        // Git sometimes lists a submodule's gitdir instead of its checkout.
        worktrees = vec![Worktree {
            path: checkout_root.clone(),
            branch: optional_ref(
                &checkout_root,
                &["symbolic-ref", "--quiet", "--short", "HEAD"],
                within,
            )?,
            head: optional_ref(
                &checkout_root,
                &["rev-parse", "--verify", "--quiet", "HEAD"],
                within,
            )?,
        }];
    }
    let primary_root = canonical_or_original(
        &worktrees
            .first()
            .ok_or("Git returned no primary checkout")?
            .path,
    )?;
    let root = path_text(&primary_root)?.to_owned();
    let repo_id = repo_id_for_path(&root);
    let opened_checkout_id = checkout_id_for_path(path_text(&checkout_root)?);
    let mut seen = HashSet::new();
    let mut checkouts = Vec::with_capacity(worktrees.len());
    for entry in worktrees {
        let path = canonical_or_original(&entry.path)?;
        let text = path_text(&path)?.to_owned();
        let id = checkout_id_for_path(&text);
        if !seen.insert(id.clone()) {
            return Err("Git returned duplicate worktree identities".into());
        }
        checkouts.push(Checkout {
            id,
            repo_id: repo_id.clone(),
            path: text.clone(),
            canonical_path: text,
            is_primary: path == primary_root,
            branch: entry.branch,
            head: entry.head,
            ahead_of_default: None,
            changed_files: 0,
            is_missing: !path.is_dir(),
            sessions: Vec::new(),
        });
    }
    let default_branch = optional_ref(
        &checkout_root,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
        within,
    )?
    .and_then(|name| name.strip_prefix("origin/").map(str::to_owned));
    let default_branch = match default_branch {
        Some(branch) => optional_ref(
            &checkout_root,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/remotes/origin/{branch}"),
            ],
            within,
        )?
        .map(|_| branch),
        None => None,
    };
    let name = primary_root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(&root)
        .to_owned();
    Ok(OpenedDirectory {
        repo: Repo {
            id: repo_id,
            kind: RepoKind::Git,
            name,
            root,
            default_branch,
            checkouts,
            created_at: now.clone(),
            last_opened_at: now,
        },
        checkout_id: opened_checkout_id,
    })
}

#[derive(Default)]
struct Worktree {
    path: PathBuf,
    head: Option<String>,
    branch: Option<String>,
}

fn parse_worktrees(bytes: &[u8]) -> Result<Vec<Worktree>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "Git returned a non-UTF-8 worktree path")?;
    if !text.ends_with("\0\0") {
        return Err("Git returned an incomplete worktree listing".into());
    }
    let mut result = Vec::new();
    let mut current = Worktree::default();
    for field in text.split('\0') {
        if field.is_empty() {
            if !current.path.as_os_str().is_empty() {
                result.push(std::mem::take(&mut current));
            }
        } else if field == "bare" {
            return Err(
                "bare repositories are not supported; open a working checkout instead".into(),
            );
        } else if let Some(path) = field.strip_prefix("worktree ") {
            if !current.path.as_os_str().is_empty() || !Path::new(path).is_absolute() {
                return Err("Git returned an invalid worktree record".into());
            }
            current.path = PathBuf::from(path);
        } else if let Some(head) = field.strip_prefix("HEAD ") {
            if !head.bytes().all(|byte| byte.is_ascii_hexdigit()) || !matches!(head.len(), 40 | 64)
            {
                return Err("Git returned an invalid worktree HEAD".into());
            }
            if !head.bytes().all(|byte| byte == b'0') {
                current.head = Some(head.to_owned());
            }
        } else if let Some(branch) = field.strip_prefix("branch refs/heads/") {
            if branch.is_empty() || branch.chars().any(char::is_control) {
                return Err("Git returned an invalid worktree branch".into());
            }
            current.branch = Some(branch.to_owned());
        }
        // detached, locked and prunable fields do not change membership.
    }
    Ok(result)
}

fn path_text(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| "non-UTF-8 directory paths are not supported".into())
}

fn canonical_or_original(path: &Path) -> Result<PathBuf, String> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(error) => Err(format!(
            "could not resolve worktree {}: {error}",
            path.display()
        )),
    }
}

fn output_text(output: &Output) -> Result<&str, String> {
    let text = std::str::from_utf8(&output.stdout).map_err(|_| "Git returned non-UTF-8 output")?;
    Ok(text.strip_suffix('\n').unwrap_or(text))
}

fn is_plain_directory(path: &Path, output: &Output) -> Result<bool, String> {
    if output.status.code() != Some(128)
        || String::from_utf8_lossy(&output.stderr).trim_end()
            != "fatal: not a git repository (or any of the parent directories): .git"
    {
        return Ok(false);
    }
    for ancestor in path.ancestors() {
        let marker = ancestor.join(".git");
        match fs::symlink_metadata(&marker) {
            // A `.git` file is a worktree or submodule link, and a broken one
            // still fails closed. A `.git` directory only counts when Git would
            // have called it a checkout: a stub holding nothing but `info/`,
            // which some tool leaves in a home directory, is not a repository
            // Git refused, and reading it as one would refuse every plain
            // folder underneath it.
            Ok(metadata) if !metadata.is_dir() || is_git_directory(&marker)? => return Ok(false),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("could not inspect Git metadata: {error}")),
        }
    }
    Ok(true)
}

/// `HEAD` with `objects` and `refs` is what makes a `.git` directory a
/// repository to Git; without them there was never one to break.
fn is_git_directory(marker: &Path) -> Result<bool, String> {
    for entry in ["HEAD", "objects", "refs"] {
        match fs::symlink_metadata(marker.join(entry)) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(format!("could not inspect Git metadata: {error}")),
        }
    }
    Ok(true)
}

fn optional_ref(cwd: &Path, args: &[&str], within: Duration) -> Result<Option<String>, String> {
    let output = git_output(cwd, args, within)?;
    if output.status.success() {
        return Ok(Some(output_text(&output)?.to_owned()));
    }
    if output.status.code() == Some(1) && output.stdout.is_empty() && output.stderr.is_empty() {
        return Ok(None);
    }
    Err(git_error("could not read Git reference", &output))
}

fn git_success(cwd: &Path, args: &[&str], within: Duration) -> Result<Output, String> {
    let output = git_output(cwd, args, within)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(git_error("Git read failed", &output))
    }
}

fn git_error(context: &str, output: &Output) -> String {
    format!(
        "{context} ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim_end()
    )
}

fn git_command(cwd: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(cwd)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env_remove("LANGUAGE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The opened path, not a parent process's Git invocation, owns the operation.
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
    ] {
        command.env_remove(name);
    }
    command
}

/// Bounded, nonblocking pipe collection on the supported macOS/Linux targets.
/// The child stays unreaped until its pipes close, reserving the process-group
/// identity so timeout/overflow cleanup cannot signal a recycled PID.
#[cfg(unix)]
fn git_output(cwd: &Path, args: &[&str], within: Duration) -> Result<Output, String> {
    use std::{
        os::{fd::AsRawFd, unix::process::CommandExt},
        process::Child,
        thread,
        time::Instant,
    };

    struct ReadChild(Child, bool);
    impl Drop for ReadChild {
        fn drop(&mut self) {
            if !self.1 {
                // process_group(0) gave this still-unreaped child its own group.
                unsafe { libc::kill(-(self.0.id() as i32), libc::SIGKILL) };
                let _ = self.0.wait();
            }
        }
    }
    fn nonblocking(pipe: &impl AsRawFd) -> Result<(), String> {
        let fd = pipe.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(format!(
                "could not read Git's pipes: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
    fn drain(pipe: &mut impl Read, bytes: &mut Vec<u8>, cap: usize) -> Result<bool, String> {
        let mut buffer = [0; 8192];
        // Yield to the deadline even when a process produces output continuously.
        for _ in 0..32 {
            match pipe.read(&mut buffer) {
                Ok(0) => return Ok(true),
                Ok(count) if bytes.len() + count > cap => {
                    return Err(format!("Git output exceeded the {cap}-byte limit"));
                }
                Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(format!("could not read Git output: {error}")),
            }
        }
        Ok(false)
    }

    let mut command = git_command(cwd, args);
    command.process_group(0);
    let mut child = ReadChild(
        command
            .spawn()
            .map_err(|error| format!("could not start Git: {error}"))?,
        false,
    );
    let mut stdout = child
        .0
        .stdout
        .take()
        .ok_or("Git stdout pipe is unavailable")?;
    let mut stderr = child
        .0
        .stderr
        .take()
        .ok_or("Git stderr pipe is unavailable")?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let deadline = Instant::now() + within;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (mut out_closed, mut err_closed) = (false, false);
    loop {
        if !out_closed {
            out_closed = drain(&mut stdout, &mut out, GIT_OUTPUT_CAP)?;
        }
        if !err_closed {
            err_closed = drain(&mut stderr, &mut err, GIT_DIAGNOSTIC_CAP)?;
        }
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let waited = unsafe {
            libc::waitid(
                libc::P_PID,
                child.0.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if waited < 0 {
            return Err(format!(
                "could not inspect Git's exit: {}",
                std::io::Error::last_os_error()
            ));
        }
        if unsafe { info.si_pid() } != 0 && out_closed && err_closed {
            let status = child
                .0
                .wait()
                .map_err(|error| format!("could not reap Git: {error}"))?;
            child.1 = true;
            return Ok(Output {
                status,
                stdout: out,
                stderr: err,
            });
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "Git read timed out after {:.1}s: {}",
                within.as_secs_f64(),
                String::from_utf8_lossy(&err).trim_end()
            ));
        }
        thread::sleep(remaining.min(Duration::from_millis(5)));
    }
}

#[cfg(not(unix))]
fn git_output(_cwd: &Path, _args: &[&str], _within: Duration) -> Result<Output, String> {
    Err("native Git discovery is supported on macOS and Linux only".into())
}

/// Creation stays in the primary repository's `.worktrees` directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeDefaults {
    pub location: PathBuf,
    pub default_branch: String,
    /// Fully qualified ref; the label above never pretends a remote is local.
    pub start_ref: String,
}

/// Fresh Git state shown before removing one selected linked worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRemovalInfo {
    pub checkout_id: String,
    pub path: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub dirty_files: Vec<String>,
    pub default_branch: Option<String>,
    pub default_ref: Option<String>,
    pub unmerged_commits: u32,
}

/// The exact preview the person acknowledged before a destructive worktree removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRemovalConfirmation {
    pub checkout_id: String,
    pub path: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub dirty_files: Vec<String>,
    pub default_ref: Option<String>,
    pub unmerged_commits: u32,
    pub confirm_dirty: bool,
    pub confirm_unmerged: bool,
    pub delete_branch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedWorktree {
    /// Git may remove the worktree but fail to delete the optional branch.
    pub warning: Option<String>,
}

/// Revalidate the repository identity before resolving any mutation inputs.
pub fn worktree_defaults(repo: &Repo) -> Result<WorktreeDefaults, String> {
    let root = primary_root(repo)?;
    let (default_branch, start_ref) = default_ref(&root, repo.default_branch.as_deref())?;
    Ok(WorktreeDefaults {
        location: root.join(".worktrees"),
        default_branch,
        start_ref,
    })
}

/// A branch the person named, resolved to a commit or refused.
fn named_ref(root: &Path, name: &str) -> Result<String, String> {
    if name.is_empty() || name.starts_with('-') {
        return Err("enter a valid Git branch name".into());
    }
    let valid = git_output(
        root,
        &["check-ref-format", &format!("refs/heads/{name}")],
        GIT_READ_TIMEOUT,
    )?;
    if !valid.status.success() {
        return Err(format!("'{name}' is not a valid Git branch name"));
    }
    for prefix in ["refs/remotes/origin/", "refs/heads/"] {
        let reference = format!("{prefix}{name}");
        if optional_ref(
            root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{reference}^{{commit}}"),
            ],
            GIT_READ_TIMEOUT,
        )?
        .is_some()
        {
            return Ok(reference);
        }
    }
    Err(format!("branch '{name}' does not exist in this repository"))
}

fn primary_root(repo: &Repo) -> Result<PathBuf, String> {
    if repo.kind != RepoKind::Git {
        return Err("worktrees require an available Git repository".into());
    }
    let root = Path::new(&repo.root);
    validate_path(&repo.root)?;
    let opened = open_directory(root)?;
    if opened.repo.kind != RepoKind::Git
        || opened.repo.id != repo.id
        || opened.repo.root != repo.root
        || opened.checkout_id != checkout_id_for_path(&repo.root)
        || !opened.repo.checkouts.iter().any(|checkout| {
            checkout.id == opened.checkout_id && checkout.is_primary && !checkout.is_missing
        })
    {
        return Err("repository identity changed; refresh before creating a worktree".into());
    }
    Ok(root.to_path_buf())
}

fn checked_worktree(repo: &Repo, checkout: &Checkout) -> Result<(PathBuf, Worktree), String> {
    let root = primary_root(repo)?;
    if checkout.repo_id != repo.id
        || checkout.is_primary
        || checkout.is_missing
        || checkout.id != checkout_id_for_path(&checkout.path)
        || checkout.path != checkout.canonical_path
    {
        return Err("worktree identity changed; refresh before removing it".into());
    }
    validate_path(&checkout.path)?;
    let path = Path::new(&checkout.path);
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("could not verify selected worktree: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("selected worktree is not a real directory".into());
    }
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("could not resolve selected worktree: {error}"))?;
    if canonical == root || canonical.to_str() != Some(checkout.canonical_path.as_str()) {
        return Err("selected path is not this non-primary worktree; refresh and retry".into());
    }

    let listed = git_success(
        &root,
        &["worktree", "list", "--porcelain", "-z"],
        GIT_READ_TIMEOUT,
    )?;
    let mut selected = None;
    for entry in parse_worktrees(&listed.stdout)? {
        if canonical_or_original(&entry.path)? == canonical {
            if selected.is_some() {
                return Err(
                    "Git listed this worktree more than once; refresh before removing it".into(),
                );
            }
            let metadata = fs::symlink_metadata(&entry.path)
                .map_err(|error| format!("could not verify Git's worktree path: {error}"))?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err("Git's selected worktree path is not a real directory".into());
            }
            selected = Some(entry);
        }
    }
    let selected = selected.ok_or_else(|| {
        "Git no longer lists this selected worktree; refresh before removing it".to_owned()
    })?;
    Ok((root, selected))
}

fn porcelain_path(record: &[u8], spaces: usize) -> Result<String, String> {
    let mut found = 0;
    for (index, byte) in record.iter().enumerate() {
        if *byte == b' ' {
            found += 1;
            if found == spaces {
                let path = &record[index + 1..];
                if path.is_empty() {
                    break;
                }
                return String::from_utf8(path.to_vec())
                    .map_err(|_| "Git returned a non-UTF-8 changed filename".into());
            }
        }
    }
    Err("Git returned an invalid worktree status record".into())
}

fn parse_worktree_dirty_files(bytes: &[u8]) -> Result<Vec<String>, String> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if bytes.last() != Some(&0) {
        return Err("Git returned an incomplete worktree status".into());
    }
    let records = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    let mut files = Vec::new();
    let mut index = 0;
    while index + 1 < records.len() {
        let record = records[index];
        let (path, renamed) = if record.starts_with(b"? ") {
            (porcelain_path(record, 1)?, false)
        } else if record.starts_with(b"1 ") {
            (porcelain_path(record, 8)?, false)
        } else if record.starts_with(b"2 ") {
            (porcelain_path(record, 9)?, true)
        } else if record.starts_with(b"u ") {
            (porcelain_path(record, 10)?, false)
        } else {
            return Err("Git returned an unknown worktree status record".into());
        };
        files.push(path);
        index += 1;
        if renamed {
            let old_path = records
                .get(index)
                .filter(|path| !path.is_empty())
                .ok_or("Git returned an incomplete rename status")?;
            files.push(
                String::from_utf8(old_path.to_vec())
                    .map_err(|_| "Git returned a non-UTF-8 changed filename")?,
            );
            index += 1;
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

/// Read a fresh Git preview for this exact, selected linked worktree.
pub fn worktree_removal_info(
    repo: &Repo,
    checkout: &Checkout,
) -> Result<WorktreeRemovalInfo, String> {
    let (root, worktree) = checked_worktree(repo, checkout)?;
    let status = git_success(
        &worktree.path,
        &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
        GIT_READ_TIMEOUT,
    )?;
    let dirty_files = parse_worktree_dirty_files(&status.stdout)?;
    let (default_branch, default_ref, unmerged_commits) =
        if let Some(branch) = worktree.branch.as_deref() {
            let (default_branch, default_ref) = default_ref(&root, repo.default_branch.as_deref())?;
            let branch_ref = format!("refs/heads/{branch}");
            let valid = git_output(&root, &["check-ref-format", &branch_ref], GIT_READ_TIMEOUT)?;
            if !valid.status.success() {
                return Err(git_error("could not validate worktree branch", &valid));
            }
            let output = git_success(
                &root,
                &[
                    "rev-list",
                    "--count",
                    &format!("{default_ref}..{branch_ref}"),
                ],
                GIT_READ_TIMEOUT,
            )?;
            let count = output_text(&output)?.parse::<u32>().map_err(|error| {
                format!("Git returned an invalid unmerged commit count: {error}")
            })?;
            (Some(default_branch), Some(default_ref), count)
        } else {
            // `--all` includes detached worktree HEADs, so count against durable branch, remote, and tag refs.
            let output = git_success(
                Path::new(&worktree.path),
                &[
                    "rev-list",
                    "--count",
                    "HEAD",
                    "--not",
                    "--branches",
                    "--remotes",
                    "--tags",
                ],
                GIT_READ_TIMEOUT,
            )?;
            let count = output_text(&output)?.parse::<u32>().map_err(|error| {
                format!("Git returned an invalid detached commit count: {error}")
            })?;
            (None, None, count)
        };
    Ok(WorktreeRemovalInfo {
        checkout_id: checkout.id.clone(),
        path: checkout.canonical_path.clone(),
        branch: worktree.branch,
        head: worktree.head,
        dirty_files,
        default_branch,
        default_ref,
        unmerged_commits,
    })
}

/// Remove only the worktree represented by a fresh preview and its explicit confirmation.
pub fn remove_worktree(
    repo: &Repo,
    checkout: &Checkout,
    confirmation: &WorktreeRemovalConfirmation,
) -> Result<RemovedWorktree, String> {
    let info = worktree_removal_info(repo, checkout)?;
    if confirmation.checkout_id != info.checkout_id
        || confirmation.path != info.path
        || confirmation.branch != info.branch
        || confirmation.head != info.head
        || confirmation.dirty_files != info.dirty_files
        || confirmation.default_ref != info.default_ref
        || confirmation.unmerged_commits != info.unmerged_commits
    {
        return Err(
            "the worktree changed since the preview; review the safeguards and retry".into(),
        );
    }
    if !info.dirty_files.is_empty() && !confirmation.confirm_dirty {
        return Err(
            "uncommitted files are present; explicitly confirm their loss before deleting".into(),
        );
    }
    if info.branch.is_none() && info.unmerged_commits > 0 && !confirmation.confirm_unmerged {
        return Err(
            "detached HEAD has commits unreachable from branches, remotes, or tags; explicitly confirm their loss before deleting".into(),
        );
    }
    if confirmation.delete_branch && info.branch.is_none() {
        return Err("this worktree has no local branch to delete".into());
    }

    // Recheck membership and the registered path immediately before the destructive Git call.
    let (root, worktree) = checked_worktree(repo, checkout)?;
    if worktree.branch != info.branch || worktree.head != info.head {
        return Err("the worktree branch changed; refresh the preview and retry".into());
    }
    let path = worktree
        .path
        .to_str()
        .ok_or("non-UTF-8 worktree paths are not supported")?;
    let mut args = vec!["worktree", "remove"];
    if !info.dirty_files.is_empty() {
        args.push("--force");
    }
    args.push(path);
    let output = git_write(&root, &args)?;
    if !output.status.success() {
        return Err(git_error("could not remove worktree", &output));
    }

    let warning = if confirmation.delete_branch {
        let branch = info.branch.as_deref().expect("validated branch");
        match git_write(&root, &["branch", "-D", "--", branch]) {
            Ok(output) if output.status.success() => None,
            Ok(output) => Some(git_error(
                "worktree was removed, but branch deletion failed",
                &output,
            )),
            Err(error) => Some(format!(
                "worktree was removed, but branch deletion failed; inspect the repository: {error}"
            )),
        }
    } else {
        None
    };
    Ok(RemovedWorktree { warning })
}

// From Legacy's resolve_default_ref/resolve_branch_ref: remote refs remain
// preferred, and only refs resolving to commits are usable. Unlike Legacy's
// main-only creation dialog, native creation also accepts the requested master fallback.
fn default_ref(root: &Path, persisted: Option<&str>) -> Result<(String, String), String> {
    let named = optional_ref(
        root,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        GIT_READ_TIMEOUT,
    )?;
    let candidates = persisted
        .map(|branch| branch.strip_prefix("origin/").unwrap_or(branch))
        .into_iter()
        .chain(
            named
                .as_deref()
                .and_then(|name| name.strip_prefix("refs/remotes/origin/")),
        )
        .chain(["main", "master"]);
    for branch in candidates {
        let valid = git_output(
            root,
            &["check-ref-format", &format!("refs/heads/{branch}")],
            GIT_READ_TIMEOUT,
        )?;
        if !valid.status.success() {
            if valid.status.code() == Some(1) && valid.stderr.is_empty() {
                continue;
            }
            return Err(git_error("could not validate default branch", &valid));
        }
        for prefix in ["refs/remotes/origin/", "refs/heads/"] {
            let reference = format!("{prefix}{branch}");
            if optional_ref(
                root,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{reference}^{{commit}}"),
                ],
                GIT_READ_TIMEOUT,
            )?
            .is_some()
            {
                return Ok((branch.to_owned(), reference));
            }
        }
    }
    Err("Git could not determine a valid default branch; choose one before creating a worktree or viewing changes".into())
}

/// Create on a worker, with no deadline on the Git mutation. Never deletes a
/// created branch and directory to roll back a subsequent discovery failure.
///
/// `base_ref` is where the new branch starts: empty asks this module to resolve
/// the repository's default, a short name resolves that branch, and a fully
/// qualified `refs/...` is taken as given once it is shown to reach a commit.
/// The qualified form is what lets a host reuse the answer it already has
/// rather than asking Git to find the same branch a second time.
pub fn create_worktree(
    repo: &Repo,
    task_name: &str,
    branch: &str,
    base_ref: &str,
) -> Result<OpenedDirectory, String> {
    let root = primary_root(repo)?;
    let root = root.as_path();
    let start_ref = match base_ref.trim() {
        "" => default_ref(root, repo.default_branch.as_deref())?.1,
        reference if reference.starts_with("refs/") => {
            if optional_ref(
                root,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{reference}^{{commit}}"),
                ],
                GIT_READ_TIMEOUT,
            )?
            .is_some()
            {
                reference.to_owned()
            } else {
                return Err(format!("{reference} does not reach a commit"));
            }
        }
        name => named_ref(root, name)?,
    };
    let location = root.join(".worktrees");
    let task = sanitize_component(task_name)?;
    let branch = branch.trim();
    if branch.is_empty() || branch.starts_with('-') {
        return Err("enter a valid Git branch name".into());
    }
    let checked = git_output(
        root,
        &["check-ref-format", "--branch", branch],
        GIT_READ_TIMEOUT,
    )?;
    // --branch expands @{-1}. A selector is not a literal new branch name.
    if !checked.status.success() || output_text(&checked)? != branch {
        return Err("enter a valid literal Git branch name".into());
    }
    let listed = git_success(
        root,
        &["worktree", "list", "--porcelain", "-z"],
        GIT_READ_TIMEOUT,
    )?;
    if let Some(holder) = parse_worktrees(&listed.stdout)?
        .iter()
        .find(|entry| entry.branch.as_deref() == Some(branch))
    {
        return Err(format!(
            "branch '{branch}' is already checked out at {}",
            holder.path.display()
        ));
    }
    if optional_ref(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
        GIT_READ_TIMEOUT,
    )?
    .is_some()
    {
        return Err(format!(
            "branch '{branch}' already exists; choose a new branch"
        ));
    }
    require_worktree_parent(&location)?;
    let destination = location.join(task);
    match fs::symlink_metadata(&destination) {
        Ok(_) => {
            return Err(format!(
                "worktree destination already exists: {}",
                destination.display()
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("could not inspect worktree destination: {error}")),
    }
    ensure_worktrees_ignored(root)?;
    match fs::create_dir(&location) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("could not create worktree location: {error}")),
    }
    require_worktree_parent(&location)?;
    if location.canonicalize().map_err(|error| error.to_string())? != location {
        return Err("the repository's .worktrees location cannot be a symbolic link".into());
    }
    let result = git_write(
        root,
        &[
            "worktree",
            "add",
            "-b",
            branch,
            path_text(&destination)?,
            &start_ref,
        ],
    )?;
    if !result.status.success() {
        return Err(git_error(
            "could not create worktree; inspect Git's state before retrying",
            &result,
        ));
    }
    let discovered = open_directory(&destination).and_then(|opened| {
        let expected = checkout_id_for_path(path_text(&destination)?);
        if opened.repo.kind != RepoKind::Git
            || opened.repo.id != repo.id
            || opened.checkout_id != expected
            || !opened.repo.checkouts.iter().any(|checkout| {
                checkout.id == expected
                    && !checkout.is_missing
                    && checkout.branch.as_deref() == Some(branch)
            })
        {
            return Err("the created checkout's Git identity or branch changed".into());
        }
        Ok(opened)
    });
    discovered.map_err(|error| format!(
        "Git created the worktree at {} but discovery failed: {error}. The worktree and branch were left untouched.",
        destination.display()
    ))
}

// The same name normalization and limits as Legacy's worktree service.
fn sanitize_component(value: &str) -> Result<String, String> {
    let mut sanitized = String::new();
    for character in value.trim().chars() {
        if character.is_alphanumeric() || matches!(character, '-' | '_' | '.' | ' ') {
            sanitized.push(character);
        } else if !sanitized.ends_with('-') {
            sanitized.push('-');
        }
    }
    let mut sanitized = sanitized
        .trim_matches([' ', '.'])
        .trim_start_matches('.')
        .chars()
        .take(80)
        .collect::<String>();
    while sanitized.len() > 180 {
        sanitized.pop();
    }
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        return Err("worktree name must contain a letter or number".into());
    }
    Ok(sanitized)
}

fn require_worktree_parent(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(
            "the repository's .worktrees location must be a directory, not a symbolic link".into(),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("could not inspect worktree location: {error}")),
    }
}

fn git_path(root: &Path, args: &[&str]) -> Result<PathBuf, String> {
    let output = git_success(root, args, GIT_READ_TIMEOUT)?;
    let raw = PathBuf::from(output_text(&output)?);
    Ok(if raw.is_absolute() {
        raw
    } else {
        root.join(raw)
    })
}

// Adapted from Legacy's ensure_worktrees_ignored. Open without following a
// symlink even if it already contains the pattern, and preserve other entries.
fn ensure_worktrees_ignored(root: &Path) -> Result<(), String> {
    let path = git_path(root, &["rev-parse", "--git-path", "info/exclude"])?;
    let git_dir = git_path(root, &["rev-parse", "--absolute-git-dir"])?
        .canonicalize()
        .map_err(|error| format!("could not resolve Git metadata: {error}"))?;
    let common = git_path(root, &["rev-parse", "--git-common-dir"])?
        .canonicalize()
        .map_err(|error| format!("could not resolve shared Git metadata: {error}"))?;
    let parent = path
        .parent()
        .ok_or("Git exclude file has no parent")?
        .canonicalize()
        .map_err(|error| format!("could not resolve Git info directory: {error}"))?;
    if !parent.starts_with(&git_dir) && !parent.starts_with(&common) {
        return Err("Git's local exclude file is outside the repository metadata directory".into());
    }
    let path = parent.join(path.file_name().ok_or("Git exclude file has no name")?);
    let mut options = OpenOptions::new();
    options.read(true).append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options
        .open(&path)
        .map_err(|error| format!("could not safely open Git's local exclude file: {error}"))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > GIT_OUTPUT_CAP as u64 {
        return Err("Git exclude must be a regular file within the 16 MiB limit".into());
    }
    let mut contents = Vec::new();
    (&mut file)
        .take(GIT_OUTPUT_CAP as u64 + 1)
        .read_to_end(&mut contents)
        .map_err(|error| error.to_string())?;
    if contents.len() > GIT_OUTPUT_CAP {
        return Err("Git exclude exceeds the 16 MiB limit".into());
    }
    if contents
        .split(|byte| *byte == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .any(|line| {
            matches!(
                line,
                b"/.worktrees/" | b"/.worktrees" | b".worktrees/" | b".worktrees"
            )
        })
    {
        return Ok(());
    }
    if !contents.is_empty() && !contents.ends_with(b"\n") {
        file.write_all(b"\n").map_err(|error| error.to_string())?;
    }
    file.write_all(b"/.worktrees/\n")
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("could not update Git exclude: {error}"))
}

/// Mutations have no deadline and are never killed for overflowing diagnostics.
/// Drain both pipes fully, retaining only a bounded diagnostic prefix.
fn git_write(root: &Path, args: &[&str]) -> Result<Output, String> {
    fn collect(mut pipe: impl Read, cap: usize) -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            let read = match pipe.read(&mut buffer) {
                Ok(read) => read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            if read == 0 {
                return Ok(bytes);
            }
            let keep = read.min(cap.saturating_sub(bytes.len()));
            bytes.extend_from_slice(&buffer[..keep]);
        }
    }
    let mut child = git_command(root, args)
        .spawn()
        .map_err(|error| format!("could not start Git mutation: {error}"))?;
    let out = child.stdout.take().ok_or("Git stdout unavailable")?;
    let err = child.stderr.take().ok_or("Git stderr unavailable")?;
    std::thread::scope(|scope| {
        let stdout = scope.spawn(move || collect(out, GIT_OUTPUT_CAP));
        let stderr = scope.spawn(move || collect(err, GIT_DIAGNOSTIC_CAP));
        let status = child.wait().map_err(|error| {
            format!("Git mutation exit is unknown; inspect repository before retrying: {error}")
        })?;
        let stdout = stdout
            .join()
            .map_err(|_| "Git mutation stdout reader failed")?
            .map_err(|error| error.to_string())?;
        let stderr = stderr
            .join()
            .map_err(|_| "Git mutation stderr reader failed")?
            .map_err(|error| error.to_string())?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    })
}

/// Net line changes from the default branch's merge base, as in Legacy's
/// numstat_counts. Staged and unstaged changes are compared together, not summed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DiffTotals {
    pub additions: u64,
    pub deletions: u64,
}

pub fn diff_totals(path: &Path) -> Result<DiffTotals, String> {
    let opened = open_directory(path)?;
    if opened.repo.kind != RepoKind::Git {
        return Err("line totals require a Git checkout".into());
    }
    let root = Path::new(
        &opened
            .repo
            .checkouts
            .iter()
            .find(|checkout| checkout.id == opened.checkout_id)
            .ok_or("opened checkout is unavailable")?
            .canonical_path,
    );
    let primary_branch = opened
        .repo
        .checkouts
        .iter()
        .find(|checkout| checkout.is_primary)
        .and_then(|checkout| checkout.branch.as_deref());
    let (_, reference) = default_ref(
        root,
        opened.repo.default_branch.as_deref().or(primary_branch),
    )?;
    let base = git_success(root, &["merge-base", &reference, "HEAD"], GIT_READ_TIMEOUT)?;
    let output = git_success(
        root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--numstat",
            "-z",
            "--find-renames",
            output_text(&base)?,
            "--",
        ],
        GIT_READ_TIMEOUT,
    )?;
    let mut totals = parse_numstat_totals(&output.stdout)?;
    let untracked = git_success(
        root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        GIT_READ_TIMEOUT,
    )?;
    if !untracked.stdout.is_empty() && !untracked.stdout.ends_with(&[0]) {
        return Err("Git returned an incomplete untracked listing".into());
    }
    let deadline = std::time::Instant::now() + GIT_READ_TIMEOUT;
    let mut budget = GIT_OUTPUT_CAP;
    for name in untracked
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let relative = Path::new(
            std::str::from_utf8(name).map_err(|_| "Git returned a non-UTF-8 untracked path")?,
        );
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err("Git returned an unsafe untracked path".into());
        }
        let path = root.join(relative);
        // Like Legacy, an untracked path escaping the checkout is not counted.
        if !path
            .canonicalize()
            .map_err(|error| format!("could not inspect untracked file: {error}"))?
            .starts_with(root)
        {
            continue;
        }
        if fs::symlink_metadata(&path)
            .map_err(|error| error.to_string())?
            .file_type()
            .is_symlink()
        {
            continue;
        }
        if let Some(lines) = untracked_lines(&path, &mut budget, deadline)? {
            totals.additions = totals
                .additions
                .checked_add(lines)
                .ok_or("line totals overflow")?;
        }
    }
    Ok(totals)
}

// NUL records and rename pairs follow Legacy's parse_numstat, without a map:
// this API only folds the unique records into a checkout total.
fn parse_numstat_totals(bytes: &[u8]) -> Result<DiffTotals, String> {
    if !bytes.is_empty() && !bytes.ends_with(&[0]) {
        return Err("Git returned an incomplete line count record".into());
    }
    let mut fields = bytes.split(|byte| *byte == 0);
    let mut total = DiffTotals::default();
    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        let mut columns = record.splitn(3, |byte| *byte == b'\t');
        let additions = columns.next().ok_or("invalid line count record")?;
        let deletions = columns.next().ok_or("invalid line count record")?;
        let path = columns.next().ok_or("invalid line count record")?;
        if path.is_empty() {
            for _ in 0..2 {
                if fields.next().is_none_or(|path| path.is_empty()) {
                    return Err("invalid rename line count record".into());
                }
            }
        }
        match (additions, deletions) {
            (b"-", b"-") => {}
            (a, d) => {
                let count = |bytes: &[u8]| -> Result<u64, String> {
                    let text = std::str::from_utf8(bytes).map_err(|_| "invalid line count")?;
                    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
                        return Err("invalid line count".into());
                    }
                    text.parse().map_err(|_| "invalid line count".into())
                };
                total.additions = total
                    .additions
                    .checked_add(count(a)?)
                    .ok_or("line totals overflow")?;
                total.deletions = total
                    .deletions
                    .checked_add(count(d)?)
                    .ok_or("line totals overflow")?;
            }
        }
    }
    Ok(total)
}

// Legacy's 8,000-byte binary sniff and newline/trailing-line rule, with a shared
// 16 MiB budget and deadline. Unreadable/oversized files are errors, not zero.
fn untracked_lines(
    path: &Path,
    budget: &mut usize,
    deadline: std::time::Instant,
) -> Result<Option<u64>, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("could not safely read untracked file: {error}"))?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("untracked file is not a regular file".into());
    }
    let mut head = [0u8; 8000];
    let mut length = 0;
    while length < head.len() {
        if std::time::Instant::now() >= deadline {
            return Err("untracked line count timed out".into());
        }
        let read = file
            .read(&mut head[length..])
            .map_err(|error| error.to_string())?;
        *budget = budget
            .checked_sub(read)
            .ok_or("untracked reads exceed the shared 16 MiB limit")?;
        if read == 0 {
            break;
        }
        length += read;
    }
    if head[..length].contains(&0) {
        return Ok(None);
    }
    let mut count = head[..length].iter().filter(|byte| **byte == b'\n').count() as u64;
    let mut last = head[..length].last().copied();
    let mut block = [0; 64 * 1024];
    loop {
        if std::time::Instant::now() >= deadline {
            return Err("untracked line count timed out".into());
        }
        let read = file.read(&mut block).map_err(|error| error.to_string())?;
        *budget = budget
            .checked_sub(read)
            .ok_or("untracked reads exceed the shared 16 MiB limit")?;
        if read == 0 {
            break;
        }
        count += block[..read].iter().filter(|byte| **byte == b'\n').count() as u64;
        last = Some(block[read - 1]);
    }
    Ok(Some(
        count + u64::from(last.is_some_and(|byte| byte != b'\n')),
    ))
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

/// Only native navigation preferences. All fields are required in the current
/// schema; incompatible files are refused, not read through compatibility defaults.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NavigationState {
    pub schema_version: u32,
    pub roots: Vec<String>,
    pub archived_checkout_ids: Vec<String>,
    #[serde(deserialize_with = "required_selection")]
    pub active_checkout_id: Option<String>,
    pub sidebar_width: f32,
}

// An absent field is not the current schema's explicit null selection.
fn required_selection<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::deserialize(deserializer)
}

impl Default for NavigationState {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            roots: Vec::new(),
            archived_checkout_ids: Vec::new(),
            active_checkout_id: None,
            sidebar_width: 240.0,
        }
    }
}

impl NavigationState {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "native workspace schema {} is unsupported; expected {SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        if !self.sidebar_width.is_finite() || !(240.0..=500.0).contains(&self.sidebar_width) {
            return Err("native sidebar width must be between 240 and 500".into());
        }
        if self.roots.len() > 1024 || self.archived_checkout_ids.len() > 8192 {
            return Err("native workspace contains too many directories".into());
        }
        let mut roots = HashSet::new();
        for root in &self.roots {
            validate_path(root)?;
            if !roots.insert(root) {
                return Err("native workspace contains duplicate roots".into());
            }
        }
        let mut archived = HashSet::new();
        for id in &self.archived_checkout_ids {
            validate_checkout_id(id)?;
            if !archived.insert(id)
                || self
                    .roots
                    .iter()
                    .any(|root| checkout_id_for_path(root) == *id)
            {
                return Err(
                    "native workspace contains duplicate or primary archived checkouts".into(),
                );
            }
        }
        if let Some(id) = &self.active_checkout_id {
            validate_checkout_id(id)?;
            if archived.contains(id) {
                return Err("the active checkout cannot be archived".into());
            }
            if self.roots.is_empty() {
                return Err("an active checkout requires a registered root".into());
            }
        }
        Ok(())
    }
}

fn validate_path(path: &str) -> Result<(), String> {
    let parsed = Path::new(path);
    if path.len() > 4096
        || path.contains('\0')
        || !parsed.is_absolute()
        || parsed.components().collect::<PathBuf>().as_os_str() != parsed.as_os_str()
        || parsed
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err("native workspace paths must be absolute normalized directory paths".into());
    }
    Ok(())
}

fn validate_checkout_id(id: &str) -> Result<(), String> {
    validate_path(
        id.strip_prefix("checkout:")
            .ok_or("invalid native checkout ID")?,
    )
}

/// Suggested isolated state location; the caller may instead use a test/profile path.
/// No fallback to the Legacy database or another format is attempted.
pub fn state_path() -> Result<PathBuf, String> {
    let home =
        std::env::var_os("HOME").ok_or("HOME is unavailable for native workspace storage")?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        return Err("HOME must be an absolute directory".into());
    }
    Ok(home.join(".muster/native-workspace.json"))
}

/// Missing files start an empty workspace. Corruption, unsupported schemas,
/// symlinks, non-regular files and oversized files are explicit errors.
pub fn load(path: &Path) -> Result<NavigationState, String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(NavigationState::default())
        }
        Err(error) => {
            return Err(format!(
                "could not inspect native workspace {}: {error}",
                path.display()
            ))
        }
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err("native workspace must be a regular file, not a symlink".into())
        }
        Ok(_) => {}
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|error| {
        format!(
            "could not read native workspace {}: {error}",
            path.display()
        )
    })?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect native workspace: {error}"))?;
    if !metadata.is_file() || metadata.len() > STATE_CAP {
        return Err("native workspace is not a regular file within the 1 MiB limit".into());
    }
    let mut bytes = Vec::new();
    file.take(STATE_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read native workspace: {error}"))?;
    if bytes.len() as u64 > STATE_CAP {
        return Err("native workspace exceeds the 1 MiB limit".into());
    }
    let state: NavigationState = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid native workspace {}: {error}", path.display()))?;
    state.validate()?;
    Ok(state)
}

/// Validate, write privately in the same directory, sync, then atomically replace.
/// Invalid existing files are never silently overwritten. Hosts must serialize
/// writes and surface errors; a directory-sync failure can occur after replacement.
pub fn save(path: &Path, state: &NavigationState) -> Result<(), String> {
    state.validate()?;
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("could not encode native workspace: {error}"))?;
    if bytes.len() as u64 > STATE_CAP {
        return Err("native workspace exceeds the 1 MiB limit".into());
    }
    load(path)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(parent)
        .map_err(|error| format!("could not create native workspace directory: {error}"))?;
    let name = path
        .file_name()
        .ok_or("native workspace path has no filename")?;
    let temp = parent.join(format!(
        ".{}.{}-{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp)
        .map_err(|error| format!("could not create native workspace temporary file: {error}"))?;
    let written = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        File::open(parent)?.sync_all()
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temp);
        return Err(format!(
            "could not save native workspace {}: {error}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repository(base: &Path) -> PathBuf {
        let root = base.join("repo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "--quiet", "--initial-branch=trunk"]);
        git(
            &root,
            &[
                "-c",
                "user.email=muster@example.test",
                "-c",
                "user.name=Muster",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "first",
            ],
        );
        root
    }

    #[test]
    fn plain_directory_has_canonical_identity_and_no_sessions() {
        let temp = tempdir().unwrap();
        let opened = open_directory(temp.path()).unwrap();
        assert_eq!(opened.repo.kind, RepoKind::Plain);
        assert_eq!(
            opened.checkout_id,
            checkout_id_for_path(path_text(&temp.path().canonicalize().unwrap()).unwrap())
        );
        assert_eq!(opened.repo.checkouts.len(), 1);
        assert!(opened.repo.checkouts[0].sessions.is_empty());
        let file = temp.path().join("file");
        fs::write(&file, "not a directory").unwrap();
        assert!(open_directory(&file).is_err());
        assert!(open_directory(&temp.path().join("missing")).is_err());
    }

    #[test]
    fn linked_worktree_subdirectory_opens_shared_repo_and_correct_checkout() {
        let temp = tempdir().unwrap();
        let root = repository(temp.path());
        let worktree = temp.path().join("tree with spaces");
        git(
            &root,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature",
                worktree.to_str().unwrap(),
            ],
        );
        fs::create_dir(worktree.join("src")).unwrap();
        let primary = open_directory(&root).unwrap();
        let opened = open_directory(&worktree.join("src")).unwrap();
        assert_eq!(opened.repo.id, primary.repo.id);
        assert_eq!(opened.repo.checkouts.len(), 2);
        assert_eq!(
            opened.checkout_id,
            checkout_id_for_path(worktree.canonicalize().unwrap().to_str().unwrap())
        );
        assert_eq!(opened.repo.checkouts[0].branch.as_deref(), Some("trunk"));
        assert_eq!(opened.repo.checkouts[1].branch.as_deref(), Some("feature"));
        assert!(!opened.repo.checkouts[1].is_primary);
        assert!(opened
            .repo
            .checkouts
            .iter()
            .all(|checkout| checkout.sessions.is_empty()));
    }

    #[test]
    fn detached_and_missing_worktrees_remain_registered() {
        let temp = tempdir().unwrap();
        let root = repository(temp.path());
        let tree = temp.path().join("detached");
        git(
            &root,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                tree.to_str().unwrap(),
            ],
        );
        let opened = open_directory(&tree).unwrap();
        assert_eq!(opened.repo.checkouts[1].branch, None);
        assert!(opened.repo.checkouts[1].head.is_some());
        fs::remove_dir_all(&tree).unwrap();
        let opened = open_directory(&root).unwrap();
        assert_eq!(opened.repo.checkouts.len(), 2);
        assert!(opened.repo.checkouts[1].is_missing);
        assert_eq!(checkout_for_directory(&[opened.repo], &tree), None);
    }

    #[cfg(unix)]
    #[test]
    fn nul_listing_and_aliases_preserve_path_identity() {
        use std::os::unix::fs::symlink;
        let temp = tempdir().unwrap();
        let root = repository(temp.path());
        let tree = temp.path().join("tree\n\"quoted\"");
        git(
            &root,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature",
                tree.to_str().unwrap(),
            ],
        );
        let alias = temp.path().join("alias");
        symlink(&tree, &alias).unwrap();
        let direct = open_directory(&tree).unwrap();
        let other = open_directory(&alias).unwrap();
        assert_eq!(direct.checkout_id, other.checkout_id);
        assert_eq!(direct.repo.id, other.repo.id);
        assert_eq!(
            direct.repo.checkouts[1].path,
            tree.canonicalize().unwrap().to_str().unwrap()
        );
    }

    #[test]
    fn broken_git_metadata_and_corrupt_config_are_not_plain_folders() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join(".git"), "gitdir: /does-not-exist\n").unwrap();
        assert!(open_directory(temp.path()).is_err());
        fs::remove_file(temp.path().join(".git")).unwrap();
        let root = repository(temp.path());
        fs::write(root.join(".git/config"), "[broken\n").unwrap();
        assert!(open_directory(&root).is_err());
    }

    #[test]
    fn a_git_stub_above_a_plain_folder_is_not_a_checkout_marker() {
        let temp = tempdir().unwrap();
        // A home directory holding nothing but a global excludes file: Git
        // never called it a repository, so neither does the folder under it.
        let home = temp.path().join("home");
        fs::create_dir_all(home.join(".git/info")).unwrap();
        fs::write(home.join(".git/info/exclude"), ".worktrees/\n").unwrap();
        let folder = home.join("notes");
        fs::create_dir(&folder).unwrap();

        let opened = open_directory(&folder).unwrap();
        assert_eq!(opened.repo.kind, RepoKind::Plain);
        assert_eq!(
            opened.repo.root,
            folder.canonicalize().unwrap().display().to_string()
        );
    }

    #[test]
    fn unborn_head_is_not_a_fabricated_commit() {
        let temp = tempdir().unwrap();
        git(temp.path(), &["init", "--quiet", "--initial-branch=trunk"]);
        let opened = open_directory(temp.path()).unwrap();
        assert_eq!(opened.repo.checkouts[0].branch.as_deref(), Some("trunk"));
        assert_eq!(opened.repo.checkouts[0].head, None);
    }

    #[test]
    fn cwd_matching_is_segment_aware_deepest_and_unambiguous() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        let nested = root.join("nested");
        let neighbour = temp.path().join("root-other");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir(&neighbour).unwrap();
        let mut repo = Repo::plain(&root, "now").unwrap();
        let checkout = Checkout::new(repo.id.clone(), &nested, false).unwrap();
        let id = checkout.id.clone();
        repo.checkouts.push(checkout);
        assert_eq!(checkout_for_directory(&[repo.clone()], &nested), Some(id));
        assert_eq!(checkout_for_directory(&[repo.clone()], &neighbour), None);
        let mut duplicate = repo.checkouts[1].clone();
        duplicate.id = "different".into();
        repo.checkouts.push(duplicate);
        assert_eq!(checkout_for_directory(&[repo], &nested), None);
    }

    #[test]
    fn state_roundtrips_without_sessions_and_replaces_atomically() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("profile/native-workspace.json");
        assert_eq!(load(&path).unwrap(), NavigationState::default());
        let root = temp
            .path()
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let mut state = NavigationState {
            roots: vec![root.clone()],
            active_checkout_id: Some(checkout_id_for_path(&root)),
            ..NavigationState::default()
        };
        save(&path, &state).unwrap();
        assert_eq!(load(&path).unwrap(), state);
        state.sidebar_width = 400.0;
        save(&path, &state).unwrap();
        assert_eq!(load(&path).unwrap(), state);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("sessions"));
        assert!(!text.contains("sessionOrder"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn invalid_state_cannot_overwrite_last_good_file() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("native-workspace.json");
        save(&path, &NavigationState::default()).unwrap();
        let before = fs::read(&path).unwrap();
        for width in [0.0, 239.0, 501.0, f32::NAN, f32::INFINITY] {
            let invalid = NavigationState {
                sidebar_width: width,
                ..NavigationState::default()
            };
            assert!(save(&path, &invalid).is_err());
            assert_eq!(fs::read(&path).unwrap(), before);
        }
        let invalid = NavigationState {
            roots: vec!["relative".into()],
            ..NavigationState::default()
        };
        assert!(save(&path, &invalid).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn unsupported_corrupt_and_oversized_files_are_refused_without_rewrite() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("native-workspace.json");
        let unsupported = NavigationState {
            schema_version: 999,
            ..NavigationState::default()
        };
        fs::write(&path, serde_json::to_vec(&unsupported).unwrap()).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(load(&path).unwrap_err().contains("schema 999"));
        assert!(save(&path, &NavigationState::default()).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        fs::write(&path, b"{}").unwrap();
        assert!(load(&path).is_err());
        fs::write(&path, vec![b' '; STATE_CAP as usize + 1]).unwrap();
        assert!(load(&path).is_err());
    }

    #[test]
    fn malformed_checkout_references_duplicates_and_archived_selection_are_refused() {
        let root = "/work/repo".to_owned();
        let mut state = NavigationState {
            roots: vec![root.clone()],
            ..NavigationState::default()
        };
        state.active_checkout_id = Some("session:123".into());
        assert!(state.validate().is_err());
        state.active_checkout_id = Some("checkout:/work/../repo".into());
        assert!(state.validate().is_err());
        state.active_checkout_id = Some("checkout:/work/tree".into());
        state
            .archived_checkout_ids
            .push("checkout:/work/tree".into());
        assert!(state.validate().is_err());
        state.active_checkout_id = None;
        state.archived_checkout_ids.clear();
        state
            .archived_checkout_ids
            .push(checkout_id_for_path(&root));
        assert!(state.validate().is_err());
        state.archived_checkout_ids.clear();
        state.roots.push(root);
        assert!(state.validate().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn state_symlinks_are_refused_without_touching_the_target() {
        use std::os::unix::fs::symlink;
        let temp = tempdir().unwrap();
        let target = temp.path().join("legacy-data");
        fs::write(&target, "do not touch").unwrap();
        let path = temp.path().join("native-workspace.json");
        symlink(&target, &path).unwrap();
        assert!(load(&path).is_err());
        assert!(save(&path, &NavigationState::default()).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "do not touch");
    }

    #[cfg(unix)]
    #[test]
    fn stuck_git_read_is_timed_out_and_its_process_is_reaped() {
        use std::os::unix::fs::OpenOptionsExt;
        let temp = tempdir().unwrap();
        let root = repository(temp.path());
        let pipe = temp.path().join("config-pipe");
        assert!(Command::new("mkfifo")
            .arg(&pipe)
            .status()
            .unwrap()
            .success());
        let mut config = OpenOptions::new()
            .append(true)
            .open(root.join(".git/config"))
            .unwrap();
        writeln!(config, "[include]\n\tpath = {}", pipe.display()).unwrap();
        let began = std::time::Instant::now();
        let error = open_directory_within(&root, Duration::from_millis(100)).unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        assert!(began.elapsed() < Duration::from_secs(5));
        assert!(OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&pipe)
            .is_err());
    }

    #[test]
    fn bare_repositories_are_refused() {
        let temp = tempdir().unwrap();
        git(temp.path(), &["init", "--quiet", "--bare"]);
        assert!(open_directory(temp.path()).unwrap_err().contains("bare"));
    }

    #[test]
    fn explicitly_opened_submodule_has_its_own_identity() {
        let temp = tempdir().unwrap();
        let source = repository(temp.path());
        let parent = temp.path().join("parent");
        fs::create_dir(&parent).unwrap();
        git(&parent, &["init", "--quiet"]);
        git(
            &parent,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "--quiet",
                source.to_str().unwrap(),
                "module",
            ],
        );
        let module = parent.join("module");
        let opened = open_directory(&module).unwrap();
        assert_eq!(
            opened.repo.root,
            module.canonicalize().unwrap().to_str().unwrap()
        );
        assert_eq!(opened.repo.checkouts.len(), 1);
        assert_eq!(opened.repo.id, repo_id_for_path(&opened.repo.root));
        assert_eq!(opened.repo.checkouts[0].branch.as_deref(), Some("trunk"));
    }

    #[test]
    fn schema_requires_explicit_selection_and_refuses_unknown_fields() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("native-workspace.json");
        let mut json = serde_json::to_value(NavigationState::default()).unwrap();
        json.as_object_mut().unwrap().remove("activeCheckoutId");
        fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        assert!(load(&path).is_err());
        json["activeCheckoutId"] = serde_json::Value::Null;
        json["sessions"] = serde_json::json!([]);
        fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        assert!(load(&path).is_err());
        for id in [
            "checkout:/work/./tree",
            "checkout:/work//tree",
            "checkout:/work/tree/",
        ] {
            let state = NavigationState {
                roots: vec!["/work/repo".into()],
                active_checkout_id: Some(id.into()),
                ..NavigationState::default()
            };
            assert!(state.validate().is_err(), "{id}");
        }
    }

    #[test]
    fn oversized_save_preserves_last_good_file() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("native-workspace.json");
        save(&path, &NavigationState::default()).unwrap();
        let before = fs::read(&path).unwrap();
        let state = NavigationState {
            roots: (0..1024)
                .map(|index| format!("/{}-{index}", "a".repeat(3000)))
                .collect(),
            ..NavigationState::default()
        };
        assert!(state.validate().is_ok());
        assert!(save(&path, &state).unwrap_err().contains("1 MiB"));
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[cfg(unix)]
    #[test]
    fn overflowing_git_output_is_refused_instead_of_truncated() {
        let temp = tempdir().unwrap();
        let began = std::time::Instant::now();
        let error = git_output(
            temp.path(),
            &["-c", "alias.flood=!yes x", "flood"],
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(error.contains("exceeded"), "{error}");
        assert!(began.elapsed() < Duration::from_secs(5));
    }

    fn main_repo(base: &Path) -> Repo {
        let root = repository(base);
        git(&root, &["branch", "-m", "main"]);
        open_directory(&root).unwrap().repo
    }

    #[test]
    fn defaults_resolve_local_remote_master_and_explicit_branches() {
        let temp = tempdir().unwrap();
        let mut repo = main_repo(temp.path());
        let root = Path::new(&repo.root);
        let defaults = worktree_defaults(&repo).unwrap();
        assert_eq!(defaults.default_branch, "main");
        assert_eq!(defaults.start_ref, "refs/heads/main");
        assert_eq!(defaults.location, root.join(".worktrees"));
        git(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        assert_eq!(
            worktree_defaults(&repo).unwrap().start_ref,
            "refs/remotes/origin/main"
        );
        git(root, &["update-ref", "-d", "refs/remotes/origin/main"]);
        git(root, &["branch", "-m", "master"]);
        assert_eq!(worktree_defaults(&repo).unwrap().default_branch, "master");
        git(root, &["branch", "-m", "trunk"]);
        assert!(worktree_defaults(&repo).is_err());
        repo.default_branch = Some("trunk".into());
        assert_eq!(
            worktree_defaults(&repo).unwrap().start_ref,
            "refs/heads/trunk"
        );
        git(root, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
        git(
            root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/trunk",
            ],
        );
        repo.default_branch = None;
        assert_eq!(
            worktree_defaults(&repo).unwrap().start_ref,
            "refs/remotes/origin/trunk"
        );
    }

    #[test]
    fn creates_sanitized_worktree_and_preserves_external_worktree_and_exclude() {
        let temp = tempdir().unwrap();
        let repo = main_repo(temp.path());
        let root = Path::new(&repo.root);
        let external = temp.path().join("external");
        git(
            root,
            &[
                "worktree",
                "add",
                "-b",
                "external",
                external.to_str().unwrap(),
            ],
        );
        fs::write(root.join(".git/info/exclude"), "existing-rule").unwrap();
        let opened = create_worktree(&repo, "../new/task", "feature/task", "").unwrap();
        let checkout = opened
            .repo
            .checkouts
            .iter()
            .find(|checkout| checkout.id == opened.checkout_id)
            .unwrap();
        assert_eq!(
            checkout.path,
            root.join(".worktrees/-new-task").to_str().unwrap()
        );
        assert_eq!(checkout.branch.as_deref(), Some("feature/task"));
        assert_eq!(opened.repo.checkouts.len(), 3);
        assert_eq!(
            fs::read_to_string(root.join(".git/info/exclude")).unwrap(),
            "existing-rule\n/.worktrees/\n"
        );
        assert!(external.is_dir());
        create_worktree(&repo, "another", "feature/another", "").unwrap();
        assert_eq!(
            fs::read_to_string(root.join(".git/info/exclude"))
                .unwrap()
                .matches("/.worktrees/")
                .count(),
            1
        );
        let status = git_success(root, &["status", "--porcelain"], GIT_READ_TIMEOUT).unwrap();
        assert!(status.stdout.is_empty());
    }

    #[test]
    fn creation_accepts_a_named_or_qualified_base_and_refuses_one_that_is_not_there() {
        let temp = tempdir().unwrap();
        let repo = main_repo(temp.path());
        let root = Path::new(&repo.root);
        // A repository Git will not guess a default for is the ordinary case
        // here, so the host has to be able to hand the base in both ways.
        git(root, &["branch", "-m", "trunk"]);
        assert!(worktree_defaults(&repo).is_err());

        let opened = create_worktree(&repo, "named", "feature/named", "trunk").unwrap();
        assert_eq!(opened.repo.checkouts.len(), 2);
        assert_eq!(
            opened
                .repo
                .checkouts
                .iter()
                .find(|checkout| checkout.id == opened.checkout_id)
                .and_then(|checkout| checkout.branch.clone())
                .as_deref(),
            Some("feature/named")
        );

        let qualified =
            create_worktree(&repo, "qualified", "feature/qualified", "refs/heads/trunk").unwrap();
        assert!(qualified
            .repo
            .checkouts
            .iter()
            .any(|checkout| checkout.branch.as_deref() == Some("feature/qualified")));

        for base in ["nope", "refs/heads/nope", "--upload-pack=evil"] {
            assert!(
                create_worktree(&repo, "refused", "feature/refused", base).is_err(),
                "{base} was accepted as a base"
            );
        }
        assert!(!root.join(".worktrees/refused").exists());
    }

    #[test]
    fn creation_rejects_taken_existing_and_nonliteral_branches_and_occupied_paths() {
        let temp = tempdir().unwrap();
        let repo = main_repo(temp.path());
        assert!(create_worktree(&repo, "task", "main", "")
            .unwrap_err()
            .contains("already checked out"));
        git(Path::new(&repo.root), &["branch", "existing"]);
        assert!(create_worktree(&repo, "task", "existing", "")
            .unwrap_err()
            .contains("already exists"));
        for branch in ["--detach", "bad name", "bad..ref", "@{-1}", "HEAD", ""] {
            assert!(
                create_worktree(&repo, "task", branch, "").is_err(),
                "{branch}"
            );
        }
        for task in ["", "...", "   "] {
            assert!(
                create_worktree(&repo, task, "feature/task", "").is_err(),
                "{task}"
            );
        }
        fs::create_dir(Path::new(&repo.root).join(".worktrees")).unwrap();
        fs::create_dir(Path::new(&repo.root).join(".worktrees/occupied")).unwrap();
        assert!(create_worktree(&repo, "occupied", "feature/task", "")
            .unwrap_err()
            .contains("already exists"));
        assert!(optional_ref(
            Path::new(&repo.root),
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/heads/feature/task"
            ],
            GIT_READ_TIMEOUT
        )
        .unwrap()
        .is_none());
        let unicode = sanitize_component(&"界".repeat(100)).unwrap();
        assert!(unicode.chars().count() <= 80 && unicode.len() <= 180);
    }

    #[test]
    fn mutation_revalidates_primary_and_refuses_plain_bare_missing_or_changed_identity() {
        let temp = tempdir().unwrap();
        let mut repo = main_repo(temp.path());
        repo.id = "repo:incorrect".into();
        assert!(worktree_defaults(&repo).is_err());
        assert!(!Path::new(&repo.root).join(".worktrees").exists());
        let plain = temp.path().join("plain");
        fs::create_dir(&plain).unwrap();
        assert!(worktree_defaults(&Repo::plain(&plain, "now").unwrap()).is_err());
        let bare = temp.path().join("bare");
        git(temp.path(), &["init", "--bare", bare.to_str().unwrap()]);
        repo.root = bare.to_str().unwrap().into();
        assert!(worktree_defaults(&repo).is_err());
        repo.root = temp.path().join("missing").to_str().unwrap().into();
        assert!(worktree_defaults(&repo).is_err());
        let real = open_directory(&temp.path().join("repo")).unwrap().repo;
        let linked = temp.path().join("linked");
        git(
            Path::new(&real.root),
            &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
        );
        repo = real;
        repo.root = linked.canonicalize().unwrap().to_str().unwrap().into();
        repo.id = repo_id_for_path(&repo.root);
        assert!(worktree_defaults(&repo).is_err());
    }

    fn removal_confirmation(
        info: &WorktreeRemovalInfo,
        confirm_dirty: bool,
        delete_branch: bool,
    ) -> WorktreeRemovalConfirmation {
        WorktreeRemovalConfirmation {
            checkout_id: info.checkout_id.clone(),
            path: info.path.clone(),
            branch: info.branch.clone(),
            head: info.head.clone(),
            dirty_files: info.dirty_files.clone(),
            default_ref: info.default_ref.clone(),
            unmerged_commits: info.unmerged_commits,
            confirm_dirty,
            confirm_unmerged: false,
            delete_branch,
        }
    }

    #[test]
    fn detached_worktree_commits_require_explicit_confirmation_before_removal() {
        let temp = tempdir().unwrap();
        let repo = main_repo(temp.path());
        let path = temp.path().join("detached");
        git(
            Path::new(&repo.root),
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                path.to_str().unwrap(),
                "HEAD",
            ],
        );
        fs::write(path.join("detached.txt"), "unique detached commit\n").unwrap();
        git(&path, &["add", "detached.txt"]);
        commit(&path, "detached-only commit");

        let opened = open_directory(&path).unwrap();
        let checkout = opened
            .repo
            .checkouts
            .iter()
            .find(|checkout| checkout.id == opened.checkout_id)
            .unwrap()
            .clone();
        let info = worktree_removal_info(&opened.repo, &checkout).unwrap();
        assert_eq!(info.branch, None);
        assert_eq!(info.unmerged_commits, 1);

        let mut confirmation = removal_confirmation(&info, false, false);
        assert!(remove_worktree(&opened.repo, &checkout, &confirmation)
            .unwrap_err()
            .contains("explicitly confirm"));
        assert!(path.is_dir());

        confirmation.confirm_unmerged = true;
        remove_worktree(&opened.repo, &checkout, &confirmation).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn removal_previews_dirty_files_and_unmerged_commits_and_keeps_branch_by_default() {
        let temp = tempdir().unwrap();
        let repo = main_repo(temp.path());
        let opened = create_worktree(&repo, "task", "feature/task", "").unwrap();
        let checkout = opened
            .repo
            .checkouts
            .iter()
            .find(|checkout| checkout.id == opened.checkout_id)
            .unwrap()
            .clone();
        let path = Path::new(&checkout.path);
        fs::write(path.join("committed.txt"), "branch commit\n").unwrap();
        git(path, &["add", "committed.txt"]);
        commit(path, "unmerged commit");
        fs::write(path.join("uncommitted file.txt"), "keep or lose\n").unwrap();

        let info = worktree_removal_info(&opened.repo, &checkout).unwrap();
        assert_eq!(info.branch.as_deref(), Some("feature/task"));
        assert_eq!(info.unmerged_commits, 1);
        assert_eq!(info.default_branch.as_deref(), Some("main"));
        assert!(info
            .dirty_files
            .iter()
            .any(|file| file == "uncommitted file.txt"));

        let mut confirmation = removal_confirmation(&info, false, false);
        assert!(remove_worktree(&opened.repo, &checkout, &confirmation)
            .unwrap_err()
            .contains("explicitly confirm"));
        assert!(path.is_dir());
        confirmation.confirm_dirty = true;
        let removed = remove_worktree(&opened.repo, &checkout, &confirmation).unwrap();
        assert!(removed.warning.is_none());
        assert!(!path.exists());
        assert!(git_output(
            Path::new(&repo.root),
            &["show-ref", "--verify", "--quiet", "refs/heads/feature/task"],
            GIT_READ_TIMEOUT,
        )
        .unwrap()
        .status
        .success());
        assert!(Path::new(&repo.root).join(".git").exists());
    }

    #[test]
    fn removal_deletes_a_branch_only_when_the_confirmation_asks_for_it() {
        let temp = tempdir().unwrap();
        let repo = main_repo(temp.path());
        let opened = create_worktree(&repo, "task", "feature/task", "").unwrap();
        let checkout = opened
            .repo
            .checkouts
            .iter()
            .find(|checkout| checkout.id == opened.checkout_id)
            .unwrap()
            .clone();
        let info = worktree_removal_info(&opened.repo, &checkout).unwrap();
        let confirmation = removal_confirmation(&info, false, true);

        let removed = remove_worktree(&opened.repo, &checkout, &confirmation).unwrap();
        assert!(removed.warning.is_none());
        assert!(!Path::new(&checkout.path).exists());
        assert!(!git_output(
            Path::new(&repo.root),
            &["show-ref", "--verify", "--quiet", "refs/heads/feature/task"],
            GIT_READ_TIMEOUT,
        )
        .unwrap()
        .status
        .success());
    }

    #[test]
    fn removal_refuses_primary_missing_and_unregistered_paths_without_deleting_them() {
        let temp = tempdir().unwrap();
        let repo = main_repo(temp.path());
        let opened = create_worktree(&repo, "task", "feature/task", "").unwrap();
        let checkout = opened
            .repo
            .checkouts
            .iter()
            .find(|checkout| checkout.id == opened.checkout_id)
            .unwrap()
            .clone();
        let primary = opened
            .repo
            .checkouts
            .iter()
            .find(|checkout| checkout.is_primary)
            .unwrap();
        assert!(worktree_removal_info(&opened.repo, primary).is_err());

        let info = worktree_removal_info(&opened.repo, &checkout).unwrap();
        let confirmation = removal_confirmation(&info, false, false);
        git(
            Path::new(&repo.root),
            &["worktree", "remove", "--force", &checkout.path],
        );
        fs::create_dir(&checkout.path).unwrap();
        let sentinel = Path::new(&checkout.path).join("not-a-worktree.txt");
        fs::write(&sentinel, "leave me").unwrap();
        assert!(remove_worktree(&opened.repo, &checkout, &confirmation).is_err());
        assert_eq!(fs::read_to_string(sentinel).unwrap(), "leave me");

        fs::remove_dir_all(&checkout.path).unwrap();
        git(
            Path::new(&repo.root),
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature/missing",
                &checkout.path,
            ],
        );
        fs::remove_dir_all(&checkout.path).unwrap();
        let refreshed = open_directory(Path::new(&repo.root)).unwrap();
        let missing = refreshed
            .repo
            .checkouts
            .iter()
            .find(|item| item.id == checkout.id)
            .unwrap();
        assert!(missing.is_missing);
        assert!(worktree_removal_info(&refreshed.repo, missing).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn worktree_creation_refuses_symlink_locations_destinations_and_exclude() {
        use std::os::unix::fs::symlink;
        let temp = tempdir().unwrap();
        let repo = main_repo(temp.path());
        let root = Path::new(&repo.root);
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        for target in [&outside, &temp.path().join("missing")] {
            symlink(target, root.join(".worktrees")).unwrap();
            assert!(create_worktree(&repo, "task", "feature/task", "").is_err());
            fs::remove_file(root.join(".worktrees")).unwrap();
        }
        fs::create_dir(root.join(".worktrees")).unwrap();
        symlink(temp.path().join("missing"), root.join(".worktrees/task")).unwrap();
        assert!(create_worktree(&repo, "task", "feature/task", "").is_err());
        fs::remove_file(root.join(".worktrees/task")).unwrap();
        let exclude = root.join(".git/info/exclude");
        fs::remove_file(&exclude).unwrap();
        let victim = outside.join("victim");
        fs::write(&victim, "/.worktrees/\n").unwrap();
        symlink(&victim, &exclude).unwrap();
        assert!(create_worktree(&repo, "task", "feature/task", "").is_err());
        assert_eq!(fs::read_to_string(&victim).unwrap(), "/.worktrees/\n");
        fs::remove_file(&exclude).unwrap();
        fs::remove_dir(root.join(".git/info")).unwrap();
        symlink(&outside, root.join(".git/info")).unwrap();
        assert!(create_worktree(&repo, "task", "feature/task", "")
            .unwrap_err()
            .contains("outside"));
        assert!(!outside.join("exclude").exists());
    }

    fn commit(root: &Path, message: &str) {
        git(
            root,
            &[
                "-c",
                "user.email=muster@example.test",
                "-c",
                "user.name=Muster",
                "commit",
                "--quiet",
                "-m",
                message,
            ],
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_discovery_after_creation_reports_partial_success_without_rollback() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempdir().unwrap();
        let repo = main_repo(temp.path());
        let root = Path::new(&repo.root);
        // A real post-checkout hook makes the new directory stop identifying as
        // its own checkout after Git creates it, exercising the production path.
        let hook = root.join(".git/hooks/post-checkout");
        fs::write(&hook, "#!/bin/sh\nrm -f .git\n").unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).unwrap();
        let error = create_worktree(&repo, "partial", "feature/partial", "").unwrap_err();
        assert!(
            error.contains("Git created the worktree") && error.contains("left untouched"),
            "{error}"
        );
        assert!(root.join(".worktrees/partial").is_dir());
        assert!(optional_ref(
            root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/heads/feature/partial"
            ],
            GIT_READ_TIMEOUT
        )
        .unwrap()
        .is_some());
    }

    #[test]
    fn totals_count_staged_unstaged_and_untracked_once_and_skip_ignored_binary_files() {
        let temp = tempdir().unwrap();
        let repo = main_repo(temp.path());
        let root = Path::new(&repo.root);
        fs::write(root.join("tracked"), "old\nkeep\n").unwrap();
        git(root, &["add", "tracked"]);
        commit(root, "tracked");
        fs::write(root.join("tracked"), "staged\nkeep\n").unwrap();
        git(root, &["add", "tracked"]);
        fs::write(root.join("tracked"), "final\nkeep\n").unwrap();
        fs::write(root.join("untracked\tfile\n"), "one\ntwo").unwrap();
        fs::write(root.join("binary"), b"binary\0\n").unwrap();
        fs::write(root.join(".git/info/exclude"), "ignored\n").unwrap();
        fs::write(root.join("ignored"), "ignore\n").unwrap();
        assert_eq!(
            diff_totals(root).unwrap(),
            DiffTotals {
                additions: 3,
                deletions: 1
            }
        );
        fs::remove_file(root.join("untracked\tfile\n")).unwrap();
        fs::write(root.join("tracked"), "old\nkeep\n").unwrap();
        // Staged change undone in the worktree is a net zero, not two diffs.
        assert_eq!(diff_totals(root).unwrap(), DiffTotals::default());
        assert!(diff_totals(temp.path()).is_err());
    }

    #[test]
    fn totals_include_feature_commits_from_merge_base_and_renames() {
        let temp = tempdir().unwrap();
        let repo = main_repo(temp.path());
        let root = Path::new(&repo.root);
        fs::write(root.join("original"), "one\ntwo\n").unwrap();
        git(root, &["add", "original"]);
        commit(root, "base file");
        let tree = temp.path().join("feature");
        git(
            root,
            &["worktree", "add", "-b", "feature", tree.to_str().unwrap()],
        );
        git(&tree, &["mv", "original", "renamed\tfile\n"]);
        fs::write(tree.join("added"), "three\n").unwrap();
        git(&tree, &["add", "."]);
        commit(&tree, "feature changes");
        assert_eq!(
            diff_totals(&tree).unwrap(),
            DiffTotals {
                additions: 1,
                deletions: 0
            }
        );
        assert_eq!(diff_totals(root).unwrap(), DiffTotals::default());
    }

    #[test]
    fn unresolved_base_and_unborn_head_are_errors_not_clean_totals() {
        let temp = tempdir().unwrap();
        let root = repository(temp.path());
        git(&root, &["checkout", "--detach", "HEAD"]);
        assert!(diff_totals(&root).is_err());
        let unborn = temp.path().join("unborn");
        fs::create_dir(&unborn).unwrap();
        git(&unborn, &["init", "--quiet", "--initial-branch=main"]);
        assert!(diff_totals(&unborn).is_err());
        assert!(worktree_defaults(&open_directory(&unborn).unwrap().repo).is_err());
    }

    #[test]
    fn invalid_numstat_and_count_overflow_are_not_reported_as_clean() {
        for bytes in [
            b"1\t1\tfile".as_slice(),
            b"-\t1\tfile\0",
            b"1\t2\t\0old\0",
            b"x\t2\tfile\0",
            b"18446744073709551615\t0\ta\0\x31\t0\tb\0",
        ] {
            assert!(parse_numstat_totals(bytes).is_err(), "{bytes:?}");
        }
        assert_eq!(
            parse_numstat_totals(b"2\t1\t\0old\0new\0-\t-\tbinary\0").unwrap(),
            DiffTotals {
                additions: 2,
                deletions: 1
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn untracked_count_is_contained_bounded_and_refuses_nonregular_files() {
        use std::os::unix::fs::symlink;
        let temp = tempdir().unwrap();
        let repo = main_repo(temp.path());
        let root = Path::new(&repo.root);
        let outside = temp.path().join("secret");
        fs::write(&outside, "not counted\n").unwrap();
        symlink(&outside, root.join("link")).unwrap();
        assert_eq!(diff_totals(root).unwrap(), DiffTotals::default());
        let file = root.join("text");
        fs::write(&file, "one\ntwo").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        assert!(untracked_lines(&file, &mut 3, deadline).is_err());
        assert!(untracked_lines(&file, &mut 100, std::time::Instant::now()).is_err());
        assert!(untracked_lines(root, &mut 100, deadline).is_err());
        fs::write(&file, b"\0binary").unwrap();
        assert!(untracked_lines(&file, &mut 3, deadline).is_err());
        assert!(untracked_lines(&file, &mut 100, std::time::Instant::now()).is_err());
        let mut late_binary = vec![b'a'; 8000];
        late_binary.extend_from_slice(b"\0\n");
        fs::write(&file, late_binary).unwrap();
        assert_eq!(
            untracked_lines(&file, &mut 9000, deadline).unwrap(),
            Some(1)
        );
    }

    #[test]
    fn mutation_drains_large_diagnostics_without_killing_or_timing_out() {
        let temp = tempdir().unwrap();
        let output = git_write(
            temp.path(),
            &[
                "-c",
                "alias.noisy=!dd if=/dev/zero bs=1024 count=80 1>&2",
                "noisy",
            ],
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stderr.len(), GIT_DIAGNOSTIC_CAP);
    }

    #[test]
    fn incomplete_and_invalid_git_listings_fail_closed() {
        assert!(parse_worktrees(b"worktree /work/repo\0").is_err());
        assert!(parse_worktrees(b"worktree relative\0\0").is_err());
        assert!(parse_worktrees(b"worktree /work/repo\0HEAD invalid\0\0").is_err());
    }
}
