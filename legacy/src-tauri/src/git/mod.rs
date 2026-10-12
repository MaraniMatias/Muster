//! Trusted Rust services invoke Git with argument arrays; no Tauri command exposes this runner.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};

use crate::{
    domain::{
        ipc::{IpcError, IpcErrorCode},
        workspace::{checkout_id_for_path, repo_id_for_path, Checkout, Repo, RepoKind},
    },
    services,
};

/// Runs a specific Git operation without shell parsing, off the Tauri/UI thread.
/// The checkout is resolved from the supplied repo contract before Git is invoked.
///
/// The one runner here with no deadline, and deliberately so. It takes whatever arguments the caller
/// hands it, so it is also how a write happens -- a commit, a worktree added, a fetch -- and a
/// write stopped part way through is a repository in a state nobody asked for: a lock left behind,
/// an index caught mid-write, a ref moved for an object that was never finished. Ending it would
/// have to undo all of that, which is more dangerous than the wait. Reads cannot be in the middle of
/// anything, so those are the ones that get the deadline: see `git_output`.
pub async fn run_git(
    repo: Repo,
    checkout_id: String,
    args: Vec<OsString>,
) -> Result<Output, IpcError> {
    tauri::async_runtime::spawn_blocking(move || {
        let cwd = services::checkout::resolve_checkout_path(&repo, &checkout_id, Path::new("."))?;
        command_for(&cwd, &args).output().map_err(|error| {
            IpcError::new(
                IpcErrorCode::GitFailed,
                format!("could not start Git: {error}"),
            )
        })
    })
    .await
    .map_err(|error| {
        IpcError::new(
            IpcErrorCode::OperationFailed,
            format!("Git operation could not complete: {error}"),
        )
    })?
}

fn command_for(cwd: &Path, args: &[OsString]) -> Command {
    let mut command = Command::new("git");
    command.current_dir(cwd).args(args);
    command
}

#[derive(Default)]
struct Worktree {
    path: Option<PathBuf>,
    head: Option<String>,
    branch: Option<String>,
    bare: bool,
}

/// Every Git read this file makes, under the deadline the sidebar's reads run under.
///
/// Resolving a repository is up to eight reads of a repository nobody has opened yet, and any one of
/// them can be the one that never answers: an `index.lock` another process is holding, a config
/// include on a path that never opens, a helper waiting on a question with no terminal behind it.
/// `Command::output` waits on the process for as long as the process wants, and the `spawn_blocking`
/// slot this sits on comes back only when that wait is over, so a Git that never finishes was a
/// folder being opened that the app never finished answering.
///
/// The deadline is an argument rather than read from here so it stays the one answer to how long a
/// Git read may take, and so a test can watch a read end a Git that will not finish instead of
/// waiting `GIT_READ_TIMEOUT` out to learn that it does.
fn git_output(cwd: &Path, args: &[&str], within: Duration) -> Result<Output, IpcError> {
    services::git::run_git_read(cwd, args, within)
}

fn output_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout)
        .trim_end_matches(['\r', '\n'])
        .to_owned()
}

/// What a read of a ref Git may not have came back with.
///
/// "There is no such ref here" and "Git never answered" are two different answers, and they used to
/// arrive through the same door: `.ok()` dropped the failure to run at all and `.filter(success)`
/// dropped the exit code, so a Git stopped by the deadline, a spawn that failed, a pipe that could
/// not be read and a repository with an unborn HEAD all came out as one `None`. Only the first of
/// those is a fact about the repository, and it is the only one that is stored: a checkout with no
/// branch and no HEAD reads as one with no commits in it yet, and a repository whose
/// `default_branch` was never read has every branch in it compared against whatever is guessed
/// next time. A read that could not be made comes back as the `Err` instead, so each caller decides
/// what that means for its own field rather than the whole file deciding it once and wrong.
#[derive(Debug)]
enum OptionalRef {
    /// Git ran and there is nothing here: no HEAD yet, no remote, no `origin/HEAD`.
    Absent,
    /// Git ran and printed this.
    Present(String),
}

impl OptionalRef {
    /// The one conversion to a missing field in this module, and only `Absent` takes it.
    fn into_option(self) -> Option<String> {
        match self {
            OptionalRef::Absent => None,
            OptionalRef::Present(value) => Some(value),
        }
    }
}

/// `git_output` for the ref queries whose specific quiet failure means "there is no such ref here".
///
/// Only the expected status/output for each known operation is an absence. Any other non-success
/// status is a Git error; failures to spawn, read pipes, or meet the deadline remain `Err`s too.
fn optional_ref(cwd: &Path, args: &[&str], within: Duration) -> Result<OptionalRef, IpcError> {
    let output = git_output(cwd, args, within)?;
    if output.status.success() {
        Ok(OptionalRef::Present(output_text(&output)))
    } else if expected_ref_absence(args, &output) {
        Ok(OptionalRef::Absent)
    } else {
        Err(services::git::git_error("Git ref query failed", &output))
    }
}

fn expected_ref_absence(args: &[&str], output: &Output) -> bool {
    output.status.code() == Some(1)
        && output.stdout.is_empty()
        && output.stderr.is_empty()
        && matches!(
            args,
            ["symbolic-ref", "--quiet", "--short", "HEAD"]
                | [
                    "symbolic-ref",
                    "--quiet",
                    "--short",
                    "refs/remotes/origin/HEAD"
                ]
                | ["rev-parse", "--verify", "--quiet", "HEAD"]
                | ["show-ref", "--verify", "--quiet", _]
        )
}

fn parse_worktrees(output: &Output) -> Vec<Worktree> {
    let mut worktrees = Vec::new();
    let mut current = Worktree::default();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if line.is_empty() {
            if current.path.is_some() {
                worktrees.push(current);
            }
            current = Worktree::default();
            continue;
        }
        if line == "bare" {
            current.bare = true;
            continue;
        }
        if let Some((key, value)) = line.split_once(' ') {
            match key {
                "worktree" => current.path = Some(PathBuf::from(value)),
                "HEAD" => current.head = Some(value.to_owned()),
                "branch" => {
                    current.branch = Some(
                        value
                            .strip_prefix("refs/heads/")
                            .unwrap_or(value)
                            .to_owned(),
                    )
                }
                _ => {}
            }
        }
    }
    if current.path.is_some() {
        worktrees.push(current);
    }
    worktrees
}

fn canonical_or_original(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

/// Detects a Git checkout at an existing canonical directory. `None` means it is a plain folder.
/// For submodules, Git's worktree listing can expose the module gitdir instead of the checkout;
/// an explicitly opened submodule is therefore represented as its own standalone repo.
///
/// The reads that have a legitimate "there is no such thing here" answer carry it as `None` and only
/// as `None`; a read that could not be made at all is either an error the caller has to hear about
/// or a field left empty with the failure recorded, and the two decisions are made per field rather
/// than once for the whole resolution.
pub fn resolve_repository(
    requested_path: &Path,
    now: &str,
) -> Result<Option<(Repo, String)>, IpcError> {
    resolve_repository_within(requested_path, now, services::git::GIT_READ_TIMEOUT)
}

/// `resolve_repository` with the deadline as an argument, for the same reason `git_output` takes one:
/// the tests below need to watch one read of a repository end a Git that will not finish rather than
/// wait `GIT_READ_TIMEOUT` out to learn that it does.
fn resolve_repository_within(
    requested_path: &Path,
    now: &str,
    within: Duration,
) -> Result<Option<(Repo, String)>, IpcError> {
    let bare = git_output(
        requested_path,
        &["rev-parse", "--is-bare-repository"],
        within,
    )?;
    if bare.status.success() {
        if output_text(&bare) == "true" {
            return Err(IpcError::new(
                IpcErrorCode::NotRepository,
                "Bare repositories are not supported; open a working checkout instead.",
            ));
        }
    } else if is_plain_directory(requested_path, &bare)? {
        return Ok(None);
    } else {
        return Err(services::git::git_error(
            "could not inspect Git repository",
            &bare,
        ));
    }

    let top = git_output(requested_path, &["rev-parse", "--show-toplevel"], within)?;
    if !top.status.success() {
        return Err(services::git::git_error(
            "could not resolve Git checkout root",
            &top,
        ));
    }
    let checkout_root = PathBuf::from(output_text(&top))
        .canonicalize()
        .map_err(|error| {
            IpcError::new(
                IpcErrorCode::GitFailed,
                format!("could not resolve Git checkout root: {error}"),
            )
        })?;
    let superproject = git_output(
        &checkout_root,
        &["rev-parse", "--show-superproject-working-tree"],
        within,
    )?;
    let is_submodule = superproject.status.success() && !output_text(&superproject).is_empty();

    let listed = git_output(&checkout_root, &["worktree", "list", "--porcelain"], within)?;
    if !listed.status.success() {
        return Err(services::git::git_error(
            "could not list Git worktrees",
            &listed,
        ));
    }

    let mut worktrees: Vec<_> = parse_worktrees(&listed)
        .into_iter()
        .filter(|entry| !entry.bare)
        .collect();
    let opened_is_listed = worktrees.iter().any(|entry| {
        entry
            .path
            .as_deref()
            .map(|path| canonical_or_original(path.to_path_buf()) == checkout_root)
            .unwrap_or(false)
    });
    if !opened_is_listed && is_submodule {
        // A submodule Git does not list is the only checkout this repository has, so these two reads
        // are the whole of what the sidebar will ever say about it, and they are the last two of the
        // eight: by now the checkout root is known to be one Git could answer for. That decides what
        // a failure here means. Git answering "there is no HEAD here" is a repository with no commits
        // yet, and that stays empty and the folder still opens. Anything else is a Git that stopped
        // answering on the two reads that decide which branch this checkout is on, and a checkout
        // registered with neither a branch nor a HEAD is one whose changes get measured against
        // nothing -- so the error goes up to the caller, which is the same path the reads above
        // already take.
        let branch = optional_ref(
            &checkout_root,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            within,
        )
        .map_err(|error| read_failure("which branch this checkout is on", error))?
        .into_option();
        let head = optional_ref(
            &checkout_root,
            &["rev-parse", "--verify", "--quiet", "HEAD"],
            within,
        )
        .map_err(|error| read_failure("which commit this checkout is on", error))?
        .into_option();
        worktrees = vec![Worktree {
            path: Some(checkout_root.clone()),
            head,
            branch,
            bare: false,
        }];
    } else if !opened_is_listed || worktrees.is_empty() {
        return Err(IpcError::new(
            IpcErrorCode::GitFailed,
            format!(
                "Git did not list the containing checkout: {}",
                checkout_root.display()
            ),
        ));
    }

    let primary_root = worktrees
        .first()
        .and_then(|entry| entry.path.clone())
        .map(canonical_or_original)
        .ok_or_else(|| {
            IpcError::new(IpcErrorCode::GitFailed, "Git returned no primary checkout")
        })?;
    let repo_id = repo_id_for_path(&primary_root.to_string_lossy());
    let opened_checkout_id = checkout_id_for_path(&checkout_root.to_string_lossy());

    let checkouts = worktrees
        .into_iter()
        .filter_map(|entry| {
            let raw_path = entry.path?;
            let path = canonical_or_original(raw_path);
            let canonical_path = path.to_string_lossy().into_owned();
            Some(Checkout {
                id: checkout_id_for_path(&canonical_path),
                repo_id: repo_id.clone(),
                path: canonical_path.clone(),
                canonical_path: canonical_path.clone(),
                is_primary: path == primary_root,
                branch: entry.branch,
                head: entry.head,
                ahead_of_default: None,
                changed_files: 0,
                is_missing: !path.is_dir(),
                sessions: Vec::new(),
            })
        })
        .collect();

    let origin_head = resolve_default_branch(&checkout_root, within);
    let path = primary_root.to_string_lossy().into_owned();
    let name = primary_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.clone());
    let repo = Repo {
        id: repo_id,
        kind: RepoKind::Git,
        name,
        root: path,
        default_branch: origin_head,
        checkouts,
        created_at: now.to_owned(),
        last_opened_at: now.to_owned(),
    };
    Ok(Some((repo, opened_checkout_id)))
}

/// Whether the read Git answered with says there is no repository here at all.
///
/// Git's exact not-a-repository diagnostic is the only sentence treated as absence, and that is why
/// the read runner pins the language Git answers in: a translated Git would say the same thing in
/// another language, and a plain folder would arrive here as a repository whose config cannot be
/// read. The locale is pinned on the child, so what a user set for themselves still holds everywhere
/// else -- this is about the wording of one sentence, not about what they see.
///
/// The comparison is the whole sentence and not a substring, because a loose match would take
/// "detected dubious ownership in repository" and a corrupt config for a plain folder, and both are
/// errors the user has to hear about. Absence only stands when no checkout metadata marker exists in
/// the requested path or its parents; broken worktree/submodule markers fail closed.
fn is_plain_directory(path: &Path, output: &Output) -> Result<bool, IpcError> {
    if output.status.code() != Some(128)
        || String::from_utf8_lossy(&output.stderr).trim_end()
            != "fatal: not a git repository (or any of the parent directories): .git"
    {
        return Ok(false);
    }

    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor.join(".git")) {
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(IpcError::new(
                    IpcErrorCode::GitFailed,
                    format!("could not inspect Git metadata: {error}"),
                ));
            }
        }
    }
    Ok(true)
}

/// A read that could not be made, carrying what it was for.
///
/// This is the only place in this file where a failure becomes something the user reads rather than
/// something only the log has, so the message says what was being read and why it matters, because
/// from the outside "Git stopped reading after 30s" on a folder the user is opening says nothing
/// about which of the eight reads it was. The wrapper adds that context around the original error.
fn read_failure(what: &str, error: IpcError) -> IpcError {
    IpcError::new(
        error.code,
        format!(
            "could not read {what} for this Git repository: {}",
            error.message
        ),
    )
}

/// The remote branch a repository is measured against, or `None` when it has none.
///
/// Both reads here are allowed to come back with nothing, and neither of them is allowed to come
/// back with nothing *for the wrong reason*. A repository that was never cloned has no
/// `origin/HEAD`, which is a fact about it: the field is empty, the sidebar has no default branch
/// to show, and the user is asked for one the moment the folder opens. A Git that never answered is
/// not that fact, and the failure is written down here rather than left to look like one, because
/// this is the last place the two can still be told apart -- from here on the repository says "no
/// default branch" either way, and every branch in it is compared against whatever
/// `resolve_default_ref` can find until the user picks one.
///
/// Aborting the whole resolution over it would be worse than losing one field of it: one ref Git
/// was slow to answer would keep a real repository from opening at all, and would stop every other
/// repository from being reconciled behind this one.
fn resolve_default_branch(cwd: &Path, within: Duration) -> Option<String> {
    let named = optional_ref(
        cwd,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
        within,
    )
    .unwrap_or_else(|error| {
        log::warn!("Git could not read the default branch ({:?})", error.code);
        OptionalRef::Absent
    })
    .into_option()?;
    let branch = named.strip_prefix("origin/")?.to_owned();
    let reference = format!("refs/remotes/origin/{branch}");
    match optional_ref(
        cwd,
        &["show-ref", "--verify", "--quiet", &reference],
        within,
    ) {
        Ok(OptionalRef::Present(_)) => Some(branch),
        Ok(OptionalRef::Absent) => None,
        Err(error) => {
            log::warn!(
                "Git could not confirm the default branch ({:?})",
                error.code
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        fs::OpenOptions,
        io::Write,
        os::unix::{fs::OpenOptionsExt, process::ExitStatusExt},
        path::{Path, PathBuf},
        process::{Command, Output},
        time::{Duration, Instant},
    };

    use tempfile::tempdir;

    use super::{
        command_for, git_output, is_plain_directory, optional_ref, resolve_repository_within,
        OptionalRef,
    };
    use crate::{
        domain::ipc::IpcErrorCode,
        services::git::{run_git_read, GIT_READ_TIMEOUT},
    };

    #[test]
    fn metacharacter_filename_stays_one_literal_git_argument() {
        let filename = "$(touch must-not-run); *.txt";
        let args = [
            OsString::from("diff"),
            OsString::from("--"),
            OsString::from(filename),
        ];
        let command = command_for(Path::new("."), &args);
        let actual: Vec<_> = command.get_args().map(OsString::from).collect();

        assert_eq!(actual, args);
    }

    /// The read this module makes that never comes back, with the deadline that ends it.
    ///
    /// The repository is a real one, hung the way a real Git hangs rather than by a helper that
    /// sleeps: its config includes a named pipe nothing ever opens the writing end of, so Git
    /// blocks while reading the config it consults before it answers anything. That is the same
    /// shape as a credential helper waiting on a prompt with no terminal behind it, and it needs no
    /// timing to reproduce: the pipe is either openable or Git is already stuck in it.
    #[test]
    fn a_repository_read_that_never_finishes_fails_after_the_deadline_and_leaves_nothing_running() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        let hung = temp.path().join("hung-config");
        Command::new("git")
            .args(["init", "--quiet"])
            .arg(&repo)
            .output()
            .expect("a repository to read");
        Command::new("mkfifo")
            .arg(&hung)
            .output()
            .expect("a pipe Git cannot finish reading");
        OpenOptions::new()
            .append(true)
            .open(repo.join(".git/config"))
            .expect("the repository's own config")
            .write_all(format!("[include]\n\tpath = {}\n", hung.display()).as_bytes())
            .expect("the include Git cannot finish reading");

        let started = Instant::now();
        let error = git_output(
            &repo,
            &["rev-parse", "--is-bare-repository"],
            Duration::from_millis(300),
        )
        .expect_err("a Git that never finishes cannot answer a repository read");

        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the deadline did not end the read"
        );
        assert_eq!(error.code, IpcErrorCode::GitFailed);
        // This fixture's timed-out Git did not print its config path; the summary is not a redactor.
        assert!(
            error.message.starts_with("Git stopped reading after 0.3s"),
            "{error:?}"
        );

        // Nothing of the stopped Git is left holding the read end of the pipe, which is the one
        // process that could still be alive here. A Git still in there is a Git still reading the
        // config, so the write end opens; a read that was ended outright has no reader and the open
        // finds nothing. Non-blocking, so this cannot wait for the reader it is looking for.
        assert!(
            OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&hung)
                .is_err(),
            "the stopped Git left a process reading the pipe"
        );
    }

    /// A repository with nothing committed in it, so the reads that name a branch and a commit have
    /// Git's "there is no such thing here" for an answer and that is all they have.
    fn empty_repository(base: &Path) -> PathBuf {
        let repo = base.join("empty");
        git(base, &["init", "--quiet", &repo.to_string_lossy()]);
        repo
    }

    /// A repository with a commit on a branch, no remote, and so no `origin/HEAD` either.
    fn repository_without_a_remote(base: &Path) -> PathBuf {
        let repo = base.join("local");
        git(base, &["init", "--quiet", &repo.to_string_lossy()]);
        git(
            &repo,
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
        repo
    }

    /// A repository cloned from a real remote, so `origin/HEAD` exists and names a branch that does
    /// too. This is the shape every repository a user opens through a remote has.
    fn repository_with_a_remote(base: &Path) -> (PathBuf, String) {
        let origin = base.join("origin.git");
        git(
            base,
            &["init", "--quiet", "--bare", &origin.to_string_lossy()],
        );
        let clone = base.join("clone");
        git(
            base,
            &[
                "clone",
                "--quiet",
                &origin.to_string_lossy(),
                &clone.to_string_lossy(),
            ],
        );
        git(
            &clone,
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
        let branch = git_read(&clone, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
        git(&clone, &["push", "--quiet", "origin", &branch]);
        git(&clone, &["remote", "set-head", "origin", &branch]);
        (clone, branch)
    }

    /// A Git write used to prepare a real repository for a test; writes have no deadline.
    fn git(cwd: &Path, args: &[&str]) -> String {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        let output = command_for(cwd, &args).output().expect("Git is installed");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn git_read(cwd: &Path, args: &[&str]) -> String {
        let output = run_git_read(cwd, args, GIT_READ_TIMEOUT).expect("Git read completes");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// A repository nobody has committed anything to still opens, and the reads with nothing to find
    /// in it still come back empty.
    ///
    /// This is what `OptionalRef::Absent` is for. It is not a fallback and not a failure to read
    /// anything: Git ran, had no commit to name and no remote to name a default branch from, and the
    /// folder is registered anyway. A change that started reading every non-answering read as this
    /// one would have to keep reading this one as this one, which is what the assertions pin.
    #[test]
    fn a_repository_with_no_commits_still_resolves_with_nothing_to_name() {
        let temp = tempdir().unwrap();
        let repo = empty_repository(temp.path());

        let (resolved, _) = resolve_repository_within(&repo, "0", GIT_READ_TIMEOUT)
            .expect("a repository Git answers for resolves")
            .expect("a repository with no commits is still a repository");

        assert_eq!(resolved.checkouts.len(), 1);
        assert_eq!(resolved.default_branch, None);

        // The read this file makes for a commit, on that same repository: a HEAD with nothing
        // behind it is Git saying "there is no such commit here", and that is an answer.
        assert!(matches!(
            optional_ref(
                &repo,
                &["rev-parse", "--verify", "--quiet", "HEAD"],
                GIT_READ_TIMEOUT
            ),
            Ok(OptionalRef::Absent)
        ));

        // The read this file makes for the remote's default branch, on that same repository: nothing
        // was ever cloned here, so there is no `origin/HEAD` and that is a fact about the repository.
        assert!(matches!(
            optional_ref(
                &repo,
                &[
                    "symbolic-ref",
                    "--quiet",
                    "--short",
                    "refs/remotes/origin/HEAD"
                ],
                GIT_READ_TIMEOUT
            ),
            Ok(OptionalRef::Absent)
        ));
        assert!(
            optional_ref(
                &repo,
                &["symbolic-ref", "--quiet", "--short", "HEAD"],
                GIT_READ_TIMEOUT
            )
            .unwrap()
            .into_option()
            .is_some(),
            "an unborn branch still has a symbolic HEAD"
        );
    }

    #[test]
    fn a_detached_head_is_absent_but_its_commit_is_present() {
        let temp = tempdir().unwrap();
        let repo = repository_without_a_remote(temp.path());

        let (attached, _) = resolve_repository_within(&repo, "0", GIT_READ_TIMEOUT)
            .expect("a local repository resolves without a remote")
            .expect("a committed local repository is still a repository");
        assert_eq!(attached.default_branch, None);
        assert!(attached.checkouts[0].branch.is_some());
        assert!(attached.checkouts[0].head.is_some());

        git(&repo, &["checkout", "--quiet", "--detach"]);

        let (resolved, _) = resolve_repository_within(&repo, "0", GIT_READ_TIMEOUT)
            .expect("a detached repository resolves")
            .expect("a detached checkout is still a repository");

        assert_eq!(resolved.checkouts[0].branch, None);
        assert!(resolved.checkouts[0].head.is_some());
        assert_eq!(resolved.default_branch, None);
        assert!(matches!(
            optional_ref(
                &repo,
                &["symbolic-ref", "--quiet", "--short", "HEAD"],
                GIT_READ_TIMEOUT
            ),
            Ok(OptionalRef::Absent)
        ));
    }

    #[test]
    fn a_plain_directory_is_not_a_repository() {
        let temp = tempdir().unwrap();

        assert!(
            resolve_repository_within(temp.path(), "0", GIT_READ_TIMEOUT)
                .expect("a plain directory is not a Git error")
                .is_none()
        );
    }

    /// A diagnostic in another language is a Git that could not be read, not a plain folder.
    ///
    /// This is the half of the fix that can be pinned down without a translated Git installed: the
    /// comparison is this one sentence, so a Git that answered in the user's locale fails closed
    /// here -- and the read runner pins the locale on the child
    /// (`services::git::a_read_pins_the_language_git_answers_in_without_touching_the_process`), so
    /// the sentence compared is the one Git writes. Failing closed is the safe direction: a plain
    /// folder reported as a broken repository is a message the user can act on, and a broken
    /// repository reported as a plain folder is one they cannot.
    #[test]
    fn a_diagnostic_in_another_language_is_not_a_plain_directory() {
        let temp = tempdir().unwrap();
        let mut output = Output {
            status: std::process::ExitStatus::from_raw(128 << 8),
            stdout: Vec::new(),
            stderr:
                b"fatal: no es un repositorio de git (ni ninguno de los directorios padre): .git"
                    .to_vec(),
        };

        assert!(
            !is_plain_directory(temp.path(), &output).expect("a localized diagnostic is absence"),
            "a translated diagnostic was read as a plain directory"
        );

        // The same read on a real plain folder, on a real Git: still plain.
        assert!(
            resolve_repository_within(temp.path(), "0", GIT_READ_TIMEOUT)
                .expect("a plain directory is not a Git error")
                .is_none()
        );

        // And the sentence itself is absence again, so the comparison is exact rather than a shape.
        output.stderr =
            b"fatal: not a git repository (or any of the parent directories): .git".to_vec();
        assert!(is_plain_directory(temp.path(), &output).expect("absence is absence"));
    }

    #[test]
    fn a_corrupt_repository_config_is_not_an_absent_ref_or_plain_directory() {
        let temp = tempdir().unwrap();
        let repo = repository_without_a_remote(temp.path());
        std::fs::write(repo.join(".git/config"), "[core\n").unwrap();

        let error = optional_ref(
            &repo,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                "refs/remotes/origin/main",
            ],
            GIT_READ_TIMEOUT,
        )
        .expect_err("a broken repository config is not a missing ref");
        assert_eq!(error.code, IpcErrorCode::GitFailed);

        let error = resolve_repository_within(&repo, "0", GIT_READ_TIMEOUT)
            .expect_err("a broken repository config is not a plain directory");
        assert_eq!(error.code, IpcErrorCode::GitFailed);
    }

    /// The reads that do have something to answer, still answering it.
    #[test]
    fn a_repository_with_a_branch_and_a_remote_resolves_every_field() {
        let temp = tempdir().unwrap();
        let (clone, branch) = repository_with_a_remote(temp.path());

        let (resolved, _) = resolve_repository_within(&clone, "0", GIT_READ_TIMEOUT)
            .expect("a repository with a remote resolves")
            .expect("a cloned repository is a repository");

        assert_eq!(resolved.default_branch.as_deref(), Some(branch.as_str()));
        assert_eq!(resolved.checkouts.len(), 1);
        assert_eq!(
            resolved.checkouts[0].branch.as_deref(),
            Some(branch.as_str())
        );
        assert!(
            resolved.checkouts[0].head.is_some(),
            "a repository with a commit has a HEAD to name"
        );
    }

    /// A Git that will not answer is not a Git that said no.
    ///
    /// The repository here is one whose `origin/HEAD` is a named pipe nothing ever opens the
    /// writing end of, so every read Git would answer from it blocks exactly the way a credential
    /// helper waiting on a question with no terminal behind it does, while the reads that do not
    /// touch it go on answering normally. That is the case the whole distinction exists for: before
    /// it, `symbolic-ref` on that pipe and `symbolic-ref` on a repository that has never been
    /// cloned were the same `None`, and a slow Git was a repository with no default branch.
    #[test]
    fn a_default_branch_read_that_never_finishes_is_recorded_and_leaves_the_field_empty() {
        let temp = tempdir().unwrap();
        let (clone, branch) = repository_with_a_remote(temp.path());
        let origin_head = clone.join(".git/refs/remotes/origin/HEAD");
        std::fs::remove_file(&origin_head).expect("the origin HEAD this hangs");
        Command::new("mkfifo")
            .arg(&origin_head)
            .output()
            .expect("a pipe Git cannot finish reading");

        let started = Instant::now();
        let (resolved, _) = resolve_repository_within(&clone, "0", Duration::from_millis(300))
            .expect("one ref that will not answer does not fail the whole resolution")
            .expect("a cloned repository is a repository");

        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the deadline did not end the read"
        );
        assert_eq!(
            resolved.default_branch, None,
            "a default branch that was never read is not a default branch"
        );
        // The rest of the resolution answered, so the repository is still the repository it was:
        // the field is empty because nobody read it, not because the repository lost its branch.
        assert_eq!(
            resolved.checkouts[0].branch.as_deref(),
            Some(branch.as_str())
        );
        assert!(resolved.checkouts[0].head.is_some());
    }

    /// A read Git could not make at all is not a read that came back empty.
    ///
    /// This is the whole distinction, on the failure that needs no timing to produce: there is no
    /// directory here to run Git in, so Git never started and answered nothing whatsoever. Read as
    /// "there is no such ref", that is a Git that had nothing to say on a repository that had
    /// everything to say, and it is stored -- a checkout with no branch and no HEAD, a repository
    /// with no default branch -- which is a wrong answer about a working repository rather than a
    /// missing one. So it is the error, and only an exit code that is not success is `Absent`.
    #[test]
    fn a_read_that_could_not_run_git_at_all_is_not_an_absent_answer() {
        let temp = tempdir().unwrap();
        let missing = temp.path().join("not-a-directory");
        let arguments = ["symbolic-ref", "--quiet", "--short", "HEAD"];

        let error = optional_ref(&missing, &arguments, GIT_READ_TIMEOUT)
            .expect_err("a Git that never ran has not answered anything");

        assert_eq!(error.code, IpcErrorCode::GitFailed);
        // Git never started here; this assertion checks the spawn error for this missing cwd only.
        assert!(
            !error
                .message
                .contains(&temp.path().to_string_lossy().into_owned()),
            "this spawn failure unexpectedly includes the missing cwd: {error:?}"
        );

        // The other half of the same boundary: Git that ran and said there is no such ref is still
        // `Absent`, so the two never collapse into one another.
        assert!(matches!(
            optional_ref(
                &empty_repository(temp.path()),
                &["rev-parse", "--verify", "--quiet", "HEAD"],
                GIT_READ_TIMEOUT
            ),
            Ok(OptionalRef::Absent)
        ));
    }

    /// The same pipe one read earlier, where nothing has answered yet.
    ///
    /// `resolve_default_branch` records the failure and lets the repository through because a
    /// default branch is a field the app knows how to be without. The reads before it are not: they
    /// decide whether this is a repository at all, so a Git that stops answering one of them has to
    /// arrive at the user as a failure to read the repository, not as a plain folder.
    #[test]
    fn a_repository_whose_very_first_read_never_finishes_is_not_a_plain_folder() {
        let temp = tempdir().unwrap();
        let repo = repository_without_a_remote(temp.path());
        let hung = repo.join(".git/hung-config");
        Command::new("mkfifo")
            .arg(&hung)
            .output()
            .expect("a pipe Git cannot finish reading");
        OpenOptions::new()
            .append(true)
            .open(repo.join(".git/config"))
            .expect("the repository's own config")
            .write_all(format!("[include]\n\tpath = {}\n", hung.display()).as_bytes())
            .expect("the include Git cannot finish reading");

        let error = resolve_repository_within(&repo, "0", Duration::from_millis(300))
            .expect_err("a Git that never finishes cannot say this folder is not a repository");

        assert_eq!(error.code, IpcErrorCode::GitFailed);
        assert!(
            !error.message.contains(&repo.to_string_lossy().into_owned()),
            "this fixture's Git diagnostic unexpectedly contains the config path: {error:?}"
        );
    }
}
