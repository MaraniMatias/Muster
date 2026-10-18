use super::*;

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
    open_directory_with_budget(path, GitReadBudget::default())
}

#[cfg(test)]
pub(super) fn open_directory_within(
    path: &Path,
    within: Duration,
) -> Result<OpenedDirectory, String> {
    open_directory_with_budget(path, GitReadBudget::new(within))
}

pub fn open_directory_with_budget(
    path: &Path,
    within: GitReadBudget,
) -> Result<OpenedDirectory, String> {
    within.remaining()?;
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
        within.remaining()?;
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
    within.remaining()?;
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
pub(super) struct Worktree {
    pub(super) path: PathBuf,
    pub(super) head: Option<String>,
    pub(super) branch: Option<String>,
}

pub(super) fn parse_worktrees(bytes: &[u8]) -> Result<Vec<Worktree>, String> {
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

pub(super) fn path_text(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| "non-UTF-8 directory paths are not supported".into())
}

pub(super) fn canonical_or_original(path: &Path) -> Result<PathBuf, String> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(error) => Err(format!(
            "could not resolve worktree {}: {error}",
            path.display()
        )),
    }
}
