use super::*;

/// Net line changes from the default branch's merge base, as in Legacy's
/// numstat_counts. Staged and unstaged changes are compared together, not summed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DiffTotals {
    pub additions: u64,
    pub deletions: u64,
}

pub fn diff_totals(path: &Path) -> Result<DiffTotals, String> {
    let within = GitReadBudget::default();
    let opened = open_directory_with_budget(path, within)?;
    let checkout = opened
        .repo
        .checkouts
        .iter()
        .find(|checkout| checkout.id == opened.checkout_id)
        .ok_or("opened checkout is unavailable")?;
    diff_totals_for_checkout(&opened.repo, checkout, within)
}

/// Count an already discovered checkout without listing every worktree again.
/// The snapshot is not authority: revalidate paths, checkout root and shared
/// Git metadata on every call. Errors never become a clean zero.
pub fn diff_totals_for_checkout(
    repo: &Repo,
    checkout: &Checkout,
    within: GitReadBudget,
) -> Result<DiffTotals, String> {
    within.remaining()?;
    if repo.kind != RepoKind::Git
        || repo.id != repo_id_for_path(&repo.root)
        || checkout.repo_id != repo.id
        || checkout.is_missing
        || checkout.id != checkout_id_for_path(&checkout.canonical_path)
        || checkout.path != checkout.canonical_path
        || checkout.is_primary != (checkout.path == repo.root)
        || !repo
            .checkouts
            .iter()
            .any(|entry| entry.id == checkout.id && entry.canonical_path == checkout.canonical_path)
    {
        return Err("line totals require this repository's available Git checkout".into());
    }
    let paths = [&repo.root, &checkout.canonical_path];
    for path in paths
        .into_iter()
        .take(if checkout.is_primary { 1 } else { 2 })
    {
        validate_path(path)?;
        let path = Path::new(path);
        if path
            .canonicalize()
            .map_err(|error| format!("could not resolve Git checkout: {error}"))?
            != path
        {
            return Err("Git checkout path changed; refresh before counting changes".into());
        }
        let top = git_success(path, &["rev-parse", "--show-toplevel"], within)?;
        if Path::new(output_text(&top)?) != path {
            return Err("Git checkout identity changed; refresh before counting changes".into());
        }
    }
    let root = Path::new(&checkout.canonical_path);
    let common = |path: &Path| -> Result<PathBuf, String> {
        let output = git_success(path, &["rev-parse", "--git-common-dir"], within)?;
        path.join(output_text(&output)?)
            .canonicalize()
            .map_err(|error| format!("could not resolve shared Git metadata: {error}"))
    };
    if !checkout.is_primary && common(root)? != common(Path::new(&repo.root))? {
        return Err("Git checkout repository changed; refresh before counting changes".into());
    }
    let primary_branch = repo
        .checkouts
        .iter()
        .find(|checkout| checkout.is_primary)
        .and_then(|checkout| checkout.branch.as_deref());
    let (_, reference) = default_ref_with_budget(
        root,
        repo.default_branch.as_deref().or(primary_branch),
        within,
    )?;
    let base = git_success(root, &["merge-base", &reference, "HEAD"], within)?;
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
        within,
    )?;
    let mut totals = parse_numstat_totals(&output.stdout)?;
    let untracked = git_success(
        root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        within,
    )?;
    if !untracked.stdout.is_empty() && !untracked.stdout.ends_with(&[0]) {
        return Err("Git returned an incomplete untracked listing".into());
    }
    let deadline = within.deadline();
    let mut budget = GIT_OUTPUT_CAP;
    for name in untracked
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        within.remaining()?;
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
    within.remaining()?;
    Ok(totals)
}

// NUL records and rename pairs follow Legacy's parse_numstat, without a map:
// this API only folds the unique records into a checkout total.
pub(super) fn parse_numstat_totals(bytes: &[u8]) -> Result<DiffTotals, String> {
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
pub(super) fn untracked_lines(
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
