use super::*;

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
    let within = GitReadBudget::default();
    let root = primary_root(repo, within)?;
    let (default_branch, start_ref) =
        default_ref_with_budget(&root, repo.default_branch.as_deref(), within)?;
    Ok(WorktreeDefaults {
        location: root.join(".worktrees"),
        default_branch,
        start_ref,
    })
}

/// A branch the person named, resolved to a commit or refused.
pub(super) fn named_ref(root: &Path, name: &str, within: GitReadBudget) -> Result<String, String> {
    if name.is_empty() || name.starts_with('-') {
        return Err("enter a valid Git branch name".into());
    }
    let valid = git_output(
        root,
        &["check-ref-format", &format!("refs/heads/{name}")],
        within,
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
            within,
        )?
        .is_some()
        {
            return Ok(reference);
        }
    }
    Err(format!("branch '{name}' does not exist in this repository"))
}

pub(super) fn primary_root(repo: &Repo, within: GitReadBudget) -> Result<PathBuf, String> {
    if repo.kind != RepoKind::Git {
        return Err("worktrees require an available Git repository".into());
    }
    let root = Path::new(&repo.root);
    validate_path(&repo.root)?;
    let opened = open_directory_with_budget(root, within)?;
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

pub(super) fn checked_worktree(
    repo: &Repo,
    checkout: &Checkout,
    within: GitReadBudget,
) -> Result<(PathBuf, Worktree), String> {
    let root = primary_root(repo, within)?;
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

    let listed = git_success(&root, &["worktree", "list", "--porcelain", "-z"], within)?;
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

pub(super) fn porcelain_path(record: &[u8], spaces: usize) -> Result<String, String> {
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

pub(super) fn parse_worktree_dirty_files(bytes: &[u8]) -> Result<Vec<String>, String> {
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
    worktree_removal_info_with_budget(repo, checkout, GitReadBudget::default())
}

fn worktree_removal_info_with_budget(
    repo: &Repo,
    checkout: &Checkout,
    within: GitReadBudget,
) -> Result<WorktreeRemovalInfo, String> {
    let (root, worktree) = checked_worktree(repo, checkout, within)?;
    let status = git_success(
        &worktree.path,
        &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
        within,
    )?;
    let dirty_files = parse_worktree_dirty_files(&status.stdout)?;
    let (default_branch, default_ref, unmerged_commits) =
        if let Some(branch) = worktree.branch.as_deref() {
            let (default_branch, default_ref) =
                default_ref_with_budget(&root, repo.default_branch.as_deref(), within)?;
            let branch_ref = format!("refs/heads/{branch}");
            let valid = git_output(&root, &["check-ref-format", &branch_ref], within)?;
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
                within,
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
                within,
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
    let within = GitReadBudget::default();
    let info = worktree_removal_info_with_budget(repo, checkout, within)?;
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
    let (root, worktree) = checked_worktree(repo, checkout, within)?;
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
    within.remaining()?;
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
pub(super) fn default_ref_with_budget(
    root: &Path,
    persisted: Option<&str>,
    within: GitReadBudget,
) -> Result<(String, String), String> {
    let named = optional_ref(
        root,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        within,
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
            within,
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
                within,
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
    let within = GitReadBudget::default();
    let root = primary_root(repo, within)?;
    let root = root.as_path();
    let start_ref = match base_ref.trim() {
        "" => default_ref_with_budget(root, repo.default_branch.as_deref(), within)?.1,
        reference if reference.starts_with("refs/") => {
            if optional_ref(
                root,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{reference}^{{commit}}"),
                ],
                within,
            )?
            .is_some()
            {
                reference.to_owned()
            } else {
                return Err(format!("{reference} does not reach a commit"));
            }
        }
        name => named_ref(root, name, within)?,
    };
    let location = root.join(".worktrees");
    let task = sanitize_component(task_name)?;
    let branch = branch.trim();
    if branch.is_empty() || branch.starts_with('-') {
        return Err("enter a valid Git branch name".into());
    }
    let checked = git_output(root, &["check-ref-format", "--branch", branch], within)?;
    // --branch expands @{-1}. A selector is not a literal new branch name.
    if !checked.status.success() || output_text(&checked)? != branch {
        return Err("enter a valid literal Git branch name".into());
    }
    let listed = git_success(root, &["worktree", "list", "--porcelain", "-z"], within)?;
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
        within,
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
    ensure_worktrees_ignored(root, within)?;
    match fs::create_dir(&location) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("could not create worktree location: {error}")),
    }
    require_worktree_parent(&location)?;
    if location.canonicalize().map_err(|error| error.to_string())? != location {
        return Err("the repository's .worktrees location cannot be a symbolic link".into());
    }
    within.remaining()?;
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
pub(super) fn sanitize_component(value: &str) -> Result<String, String> {
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

pub(super) fn require_worktree_parent(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(
            "the repository's .worktrees location must be a directory, not a symbolic link".into(),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("could not inspect worktree location: {error}")),
    }
}

pub(super) fn git_path(
    root: &Path,
    args: &[&str],
    within: GitReadBudget,
) -> Result<PathBuf, String> {
    let output = git_success(root, args, within)?;
    let raw = PathBuf::from(output_text(&output)?);
    Ok(if raw.is_absolute() {
        raw
    } else {
        root.join(raw)
    })
}

// Adapted from Legacy's ensure_worktrees_ignored. Open without following a
// symlink even if it already contains the pattern, and preserve other entries.
pub(super) fn ensure_worktrees_ignored(root: &Path, within: GitReadBudget) -> Result<(), String> {
    let path = git_path(root, &["rev-parse", "--git-path", "info/exclude"], within)?;
    let git_dir = git_path(root, &["rev-parse", "--absolute-git-dir"], within)?
        .canonicalize()
        .map_err(|error| format!("could not resolve Git metadata: {error}"))?;
    let common = git_path(root, &["rev-parse", "--git-common-dir"], within)?
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
