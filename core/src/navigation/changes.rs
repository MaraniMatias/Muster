use super::diffs::{
    parse_numstat_by_path, untracked_lines, validated_checkout_root, NumstatCounts,
};
use super::worktrees::porcelain_path;
use super::*;

/// What a checkout changed, against the merge base with its default branch.
///
/// Two change sets are one answer, not two: a commit made in the worktree moves every
/// sibling's base, so the panel lists the worktree's own commits as well as its dirty
/// files. Reading only `git diff HEAD` would hide the commits an agent pushed before the
/// terminal was opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatus {
    /// `None` while HEAD is detached, which is a state and not a missing answer.
    pub branch: Option<String>,
    /// `None` only in a repository with no commit at all.
    pub head: Option<String>,
    pub default_branch: String,
    pub ahead_count: u64,
    pub files: Vec<ChangedFile>,
}

/// One changed file, as Git names it and as it counts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    /// The path a rename came from, which is not in the record of a status read.
    pub old_path: Option<String>,
    /// Git's own two-column code with its blank half dropped: `M`, `A`, `MM`, `U`, `??`,
    /// `R`. Deliberately not an enum, because a column this app does not have a case for
    /// is a code to show, and an unknown code a panel must still be able to draw.
    pub status: String,
    /// Absent rather than zero when Git has no count: `--numstat` prints `-` for both
    /// columns of a binary file, and a zero here would be a number nobody measured.
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
}

/// One `@@` header's extent, so a reader can fold what it is not looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffHunk {
    pub start_line: u64,
    pub end_line: u64,
}

/// One file's change, as far as this build is willing to carry it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    /// Complete patch under the hard caps. Binary and refused patches are empty;
    /// large text patches remain available to a virtualized reader.
    pub patch: String,
    /// Hash of every line Git printed, including the ones a limit threw away. Two large
    /// diffs both answer with an empty patch, and a reader that compares patches alone
    /// would keep serving the first one's lines for the second.
    pub revision: String,
    pub hunks: Vec<DiffHunk>,
    pub total_lines: usize,
    pub is_binary: bool,
    /// The image this file is, when its bytes say so.
    ///
    /// A binary diff carries no text, so the only way to say *what* changed rather
    /// than that something did is to look at the bytes Git refused to print. The
    /// file has already been copied into the private snapshot this diff reads, so
    /// the answer costs a header's worth of reading and nothing else.
    pub image: Option<ImageFormat>,
    /// A large, complete patch requiring virtualized presentation, not a refusal.
    pub large: bool,
    /// Refused outright: over a line, hunk, line-length or output cap. A refusal is an
    /// explicit answer, because the alternative is a change set that looks complete.
    pub too_large: bool,
    /// The link this file is, for a diff that has no text on either side.
    pub symlink_target: Option<String>,
}

/// An image format, named as the reader would name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    WebP,
    Bmp,
    Ico,
}

impl ImageFormat {
    /// The format's own name, lower case and without a dot.
    pub fn name(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Gif => "gif",
            Self::WebP => "webp",
            Self::Bmp => "bmp",
            Self::Ico => "ico",
        }
    }
}

/// What the first bytes of a changed file say it is, if they say anything.
///
/// `bytes` is `None` only for a file that is no longer there, which has nothing to ask
/// and whose name is therefore the only answer available; it is believed only when it is
/// one of the names this function could have named from bytes. A file that *is* there is
/// answered by its bytes alone: a `.png` that is a Git LFS pointer is text, an arbitrary
/// blob named `.png` is a blob, and an empty file is not a picture however it is spelled.
/// An empty slice is a header read from a present empty file, not an absence.
fn image_format(bytes: Option<&[u8]>, path: &str) -> Option<ImageFormat> {
    let Some(bytes) = bytes else {
        let name = path.rsplit('.').next()?.to_ascii_lowercase();
        return match name.as_str() {
            "png" => Some(ImageFormat::Png),
            "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
            "gif" => Some(ImageFormat::Gif),
            "webp" => Some(ImageFormat::WebP),
            "bmp" => Some(ImageFormat::Bmp),
            "ico" => Some(ImageFormat::Ico),
            _ => None,
        };
    };
    match bytes {
        [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, ..] => Some(ImageFormat::Png),
        [0xff, 0xd8, 0xff, ..] => Some(ImageFormat::Jpeg),
        [b'G', b'I', b'F', b'8', b'7' | b'9', b'a', ..] => Some(ImageFormat::Gif),
        // RIFF container, the four-byte size field, then the form at its fixed offset:
        // a `WEBP` anywhere else is a suffix coincidence, not a format.
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some(ImageFormat::WebP),
        [b'B', b'M', ..] => Some(ImageFormat::Bmp),
        [0x00, 0x00, 0x01, 0x00, ..] => Some(ImageFormat::Ico),
        _ => None,
    }
}

/// Threshold for large-patch metadata. The native viewer virtualizes complete
/// patches under the hard caps; crossing this threshold never discards content.
const SMALL_DIFF_LINES: usize = 1_000;
const SMALL_DIFF_BYTES: usize = 512 * 1024;
const MAX_DIFF_LINES: usize = 100_000;
const MAX_DIFF_HUNKS: usize = 10_000;
const MAX_DIFF_LINE_BYTES: usize = 64 * 1024;
const MAX_DIFF_BYTES: usize = 16 * 1024 * 1024;

/// Every changed file of a checkout, with the lines each one adds and removes.
///
/// The counts come from the same base reference as the list, so a row's number can never
/// describe a different change set than the row it sits in.
pub fn status(
    repo: &Repo,
    checkout: &Checkout,
    within: GitReadBudget,
) -> Result<GitStatus, String> {
    let root = validated_checkout_root(repo, checkout, within)?;
    let (base, default_branch) = merge_base(repo, root.as_path(), within)?;
    let mut files = changed_files(root.as_path(), &base, within)?;
    let counts = numstat_counts(root.as_path(), &base, within)?;
    let untracked = untracked_counts(&root, &files, within)?;
    for file in &mut files {
        match counts.get(file.path.as_str()) {
            Some((additions, deletions)) => {
                file.additions = *additions;
                file.deletions = *deletions;
            }
            None => {
                file.additions = untracked.get(file.path.as_str()).copied().flatten();
                file.deletions = None;
            }
        }
    }
    let branch = optional_ref(
        root.as_path(),
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        within,
    )?;
    let head = optional_ref(root.as_path(), &["rev-parse", "--short", "HEAD"], within)?;
    let ahead = git_success(
        root.as_path(),
        &["rev-list", "--count", &format!("{base}..HEAD")],
        within,
    )?;
    let ahead_count = output_text(&ahead)?
        .parse()
        .map_err(|error| format!("Git returned an invalid ahead count: {error}"))?;
    // The branch a row reads, without the `refs/` a ref carries: the ref is what Git is
    // asked with and the name is what a person is shown.
    Ok(GitStatus {
        branch,
        head,
        default_branch,
        ahead_count,
        files,
    })
}

/// One file's change against the same merge base the status listed it under.
///
/// `path` is checkout-relative as the status spelled it; anything else is refused before
/// Git is asked, because a diff is a read of one file and a path from anywhere else is a
/// read of something nobody selected.
pub fn file_diff(
    repo: &Repo,
    checkout: &Checkout,
    path: &str,
    within: GitReadBudget,
) -> Result<FileDiff, String> {
    file_diff_before_git(repo, checkout, path, within, || {})
}

// The callback allows tests to exchange the working directory at exactly the old
// check/use boundary. Git must read only the private snapshot after this point.
pub(super) fn file_diff_before_git(
    repo: &Repo,
    checkout: &Checkout,
    path: &str,
    within: GitReadBudget,
    before_git: impl FnOnce(),
) -> Result<FileDiff, String> {
    let root = validated_checkout_root(repo, checkout, within)?;
    let relative = validated_relative_path(path)?;
    let (base, _) = merge_base(repo, &root, within)?;
    let changed = changed_files(&root, &base, within)?
        .into_iter()
        .find(|file| file.path == path)
        .ok_or("Git does not report this file as changed in this checkout")?;
    let snapshot = tempfile::Builder::new()
        .prefix("muster-diff-")
        .tempdir()
        .map_err(|error| format!("could not create diff snapshot: {error}"))?;
    // `None` is a file that is gone; `Some(empty)` is a present file with no bytes to
    // read, and the two are told apart because only the first may be named by its path.
    let mut header: Option<Vec<u8>> = None;
    match snapshot_file(&root, &relative, snapshot.path(), within)? {
        Snapshot::TooLarge => return Ok(too_large(path)),
        Snapshot::Link(target) => {
            return Ok(FileDiff {
                path: path.to_owned(),
                patch: String::new(),
                revision: digest(target.as_bytes()),
                hunks: Vec::new(),
                total_lines: 0,
                is_binary: false,
                image: None,
                large: false,
                too_large: false,
                symlink_target: Some(target),
            })
        }
        Snapshot::Missing if changed.status == "??" => {
            return Err("changed file disappeared while opening it".into())
        }
        Snapshot::Missing => {}
        Snapshot::File(read) => {
            // Git is about to be asked for a diff of this file and will refuse to
            // print one, because its bytes are not text. Reading the header it is
            // about to refuse is what lets the reader be told *what* changed.
            header = Some(read);
        }
    }
    let untracked = changed.status == "??";
    // Keep the real repository/index for tracked comparisons, but never let Git
    // resolve a working-tree input from a mutable checkout pathname.
    let mut args = Vec::new();
    if !untracked {
        args.push(format!("--work-tree={}", snapshot.path().display()));
    }
    args.extend(
        [
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--find-renames",
            "--unified=3",
        ]
        .map(str::to_owned),
    );
    if untracked {
        args.extend(["--no-index", "--", "/dev/null", path].map(str::to_owned));
    } else {
        args.push(base);
        args.push("--".into());
        if let Some(old) = &changed.old_path {
            args.push(literal_pathspec(old));
        }
        args.push(literal_pathspec(path));
    }
    before_git();
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    let cwd = if untracked {
        snapshot.path()
    } else {
        root.as_path()
    };
    let output = match git_output(cwd, &borrowed, within) {
        Ok(output) => output,
        Err(error) if is_output_overflow(&error) => return Ok(too_large(path)),
        Err(error) => return Err(error),
    };
    if !(output.status.success() || untracked && output.status.code() == Some(1)) {
        return Err(git_error("Git could not diff this file", &output));
    }
    let mut diff = scan(path.to_owned(), &output.stdout)?;
    diff.image = image_format(header.as_deref(), path);
    Ok(diff)
}

enum Snapshot {
    /// The file, and the first bytes of it: enough to say what it is to a reader
    /// who is about to be told there is no text diff of it.
    File(Vec<u8>),
    Missing,
    Link(String),
    TooLarge,
}

fn snapshot_file(
    root: &Path,
    relative: &Path,
    destination: &Path,
    within: GitReadBudget,
) -> Result<Snapshot, String> {
    use std::os::{fd::AsRawFd, unix::fs::PermissionsExt};
    let handle = crate::files::open_root(root).map_err(|e| e.to_string())?;
    let parent =
        match crate::files::open_directory(&handle, relative.parent().unwrap_or(Path::new(""))) {
            Ok(parent) => parent,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Snapshot::Missing)
            }
            Err(error) => return Err(format!("could not open changed directory: {error}")),
        };
    let name = crate::files::name_of(relative.file_name().ok_or("changed path is not a file")?)
        .map_err(|e| e.to_string())?;
    let mut target = [0u8; 4096];
    let length = unsafe {
        libc::readlinkat(
            parent.as_raw_fd(),
            name.as_ptr(),
            target.as_mut_ptr().cast(),
            target.len(),
        )
    };
    if length >= 0 {
        if length as usize == target.len() {
            return Err("symlink target exceeds preview limit".into());
        }
        return Ok(Snapshot::Link(
            String::from_utf8_lossy(&target[..length as usize]).into_owned(),
        ));
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::NotFound {
        return Ok(Snapshot::Missing);
    }
    if error.raw_os_error() != Some(libc::EINVAL) {
        return Err(error.to_string());
    }
    let mut file = match crate::files::open_file(&parent, &name) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Snapshot::Missing),
        Err(error) => return Err(format!("could not open changed file: {error}")),
    };
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("changed path is not a regular file".into());
    }
    if metadata.len() > MAX_DIFF_BYTES as u64 {
        return Ok(Snapshot::TooLarge);
    }
    let target = destination.join(relative);
    fs::create_dir_all(target.parent().ok_or("changed path has no parent")?)
        .map_err(|e| e.to_string())?;
    let mut copy = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .map_err(|e| e.to_string())?;
    let mut total = 0;
    let mut header = Vec::with_capacity(HEADER_BYTES);
    let mut block = [0; 64 * 1024];
    loop {
        within.remaining()?;
        let count = file.read(&mut block).map_err(|e| e.to_string())?;
        total += count;
        if total > MAX_DIFF_BYTES {
            return Ok(Snapshot::TooLarge);
        }
        if header.len() < HEADER_BYTES {
            let take = block[..count]
                .iter()
                .take(HEADER_BYTES - header.len())
                .copied()
                .collect::<Vec<u8>>();
            header.extend_from_slice(&take);
        }
        if count == 0 {
            break;
        }
        copy.write_all(&block[..count]).map_err(|e| e.to_string())?;
    }
    copy.set_permissions(fs::Permissions::from_mode(metadata.permissions().mode()))
        .map_err(|e| e.to_string())?;
    Ok(Snapshot::File(header))
}

/// How much of a changed file is read to answer "what is this".
///
/// Every signature [`image_format`] knows is longer than a signature and shorter
/// than this, so the question is answered by a header rather than by a file.
const HEADER_BYTES: usize = 16;

/// A path Git is told about, spelled so Git cannot read it as a pattern.
///
/// A pathspec is a pattern by default: `a[1].txt` also selects `a1.txt`, and a path
/// beginning with `:` is pathspec magic rather than a filename at all. Either one turns
/// a request for one file into a request for several, so the file's own name is asked for
/// as a literal.
fn literal_pathspec(path: &str) -> String {
    format!(":(literal){path}")
}

/// Whether every part of `path` that exists is inside `root`.
///
/// The nearest existing ancestor is the part that carries the containment: a name that is
/// absent contributes nothing to escape through, and a deleted file's own entry is absent
/// on purpose. Walking up also refuses a link used as a parent directory, which is the one
/// way a checkout-relative path reaches outside the checkout without ever naming `..`.
#[cfg(test)]
pub(crate) fn contained_existing_ancestor(root: &Path, path: &Path) -> Result<(), String> {
    let outside = || "this path resolves outside the active checkout".to_owned();
    for candidate in path.ancestors() {
        match fs::canonicalize(candidate) {
            Ok(resolved) if resolved.starts_with(root) => return Ok(()),
            Ok(_) => return Err(outside()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("could not resolve changed path: {error}")),
        }
    }
    Err(outside())
}

fn too_large(path: &str) -> FileDiff {
    FileDiff {
        path: path.to_owned(),
        patch: String::new(),
        revision: String::new(),
        hunks: Vec::new(),
        total_lines: 0,
        is_binary: false,
        image: None,
        large: false,
        too_large: true,
        symlink_target: None,
    }
}

fn default_ref(
    repo: &Repo,
    root: &Path,
    within: GitReadBudget,
) -> Result<(String, String), String> {
    let primary = repo
        .checkouts
        .iter()
        .find(|checkout| checkout.is_primary)
        .and_then(|checkout| checkout.branch.as_deref());
    default_ref_with_budget(root, repo.default_branch.as_deref().or(primary), within)
}

/// The commit every change in this checkout is measured from, and the branch that
/// commit belongs to. Both come from one resolution, because resolving the base twice
/// is two answers that could disagree.
fn merge_base(repo: &Repo, root: &Path, within: GitReadBudget) -> Result<(String, String), String> {
    let (branch, reference) = default_ref(repo, root, within)?;
    let output = git_success(root, &["merge-base", &reference, "HEAD"], within)?;
    Ok((output_text(&output)?.to_owned(), branch))
}

/// The worktree's dirty files and the commits it holds above the base, keyed by path so a
/// commit and a dirty edit of one file are one row and keep one old path.
fn changed_files(
    root: &Path,
    base: &str,
    within: GitReadBudget,
) -> Result<Vec<ChangedFile>, String> {
    let status = git_success(
        root,
        &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
        within,
    )?;
    let mut files = parse_porcelain_v2(&status.stdout)?;
    let committed = git_success(
        root,
        &[
            "diff",
            "--name-status",
            "-z",
            "--find-renames",
            base,
            "HEAD",
        ],
        within,
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
    Ok(files.into_values().collect())
}

fn numstat_counts(root: &Path, base: &str, within: GitReadBudget) -> Result<NumstatCounts, String> {
    let output = git_success(
        root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--numstat",
            "-z",
            "--find-renames",
            base,
            "--",
        ],
        within,
    )?;
    parse_numstat_by_path(&output.stdout)
}

/// Untracked files are not in `--numstat`, so their added lines are counted from disk
/// under the same shared read budget as everything else. A link is not counted: its lines
/// are not added lines, and counting them would answer for a file Git does not diff.
fn untracked_counts(
    root: &Path,
    files: &[ChangedFile],
    within: GitReadBudget,
) -> Result<BTreeMap<String, Option<u64>>, String> {
    let mut counts = BTreeMap::new();
    let untracked: Vec<&ChangedFile> = files.iter().filter(|file| file.status == "??").collect();
    if untracked.is_empty() {
        return Ok(counts);
    }
    let listed_bytes = git_success(
        root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        within,
    )?;
    if !listed_bytes.stdout.is_empty() && !listed_bytes.stdout.ends_with(&[0]) {
        return Err("Git returned an incomplete untracked listing".into());
    }
    let mut listed = HashSet::new();
    for name in listed_bytes
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        listed.insert(
            String::from_utf8(name.to_vec())
                .map_err(|_| "Git returned a non-UTF-8 untracked path".to_owned())?,
        );
    }
    let deadline = within.deadline();
    let mut budget = GIT_OUTPUT_CAP;
    for file in untracked {
        if !listed.contains(&file.path) {
            return Err("Git listed an untracked file it does not report".into());
        }
        let relative = validated_relative_path(&file.path)?;
        // Counts are optional per path; an unsupported, unreadable or oversized
        // entry must not hide the other Git rows or turn into a measured zero.
        counts.insert(
            file.path.clone(),
            untracked_lines(root, &relative, &mut budget, deadline).unwrap_or(None),
        );
    }
    Ok(counts)
}

/// A checkout-relative path with nothing above it and nothing Git owns in it.
///
/// `..` and `.` are not `Normal` components and absolutes are not relative at all, so one
/// component check refuses all three; `.git` is a normal component and is named here,
/// because the directory this app diffs is not Git's own bookkeeping.
fn validated_relative_path(path: &str) -> Result<PathBuf, String> {
    let relative = Path::new(path);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || relative.components().any(|part| part.as_os_str() == ".git")
    {
        return Err("changed paths must be relative to the checkout and outside .git".into());
    }
    Ok(relative.to_path_buf())
}

fn parse_porcelain_v2(output: &[u8]) -> Result<BTreeMap<String, ChangedFile>, String> {
    let mut records = output.split(|byte| *byte == 0).peekable();
    let mut files = BTreeMap::new();
    while let Some(record) = records.next() {
        if record.is_empty() || record.starts_with(b"#") {
            continue;
        }
        let (path, old_path, status) = match record.first() {
            Some(b'?') => (porcelain_path(record, 1)?, None, "??".to_owned()),
            Some(b'1') => (
                porcelain_path(record, 8)?,
                None,
                xy_status(record.get(2..4)),
            ),
            Some(b'2') => {
                let path = porcelain_path(record, 9)?;
                let old_path = records
                    .next()
                    .filter(|path| !path.is_empty())
                    .ok_or("Git returned an incomplete rename status")?;
                (
                    path,
                    Some(
                        String::from_utf8(old_path.to_vec())
                            .map_err(|_| "Git returned a non-UTF-8 changed filename".to_owned())?,
                    ),
                    xy_status(record.get(2..4)),
                )
            }
            Some(b'u') => (
                porcelain_path(record, 10)?,
                None,
                xy_status(record.get(2..4)),
            ),
            _ => return Err("Git returned an unknown worktree status record".into()),
        };
        files.insert(
            path.clone(),
            ChangedFile {
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

/// Both columns, minus the half Git says is not there.
///
/// Porcelain v2 writes a period where the other implementations write a blank: a change
/// only in the worktree is `.M` and one only in the index is `M.`. Both are `M` to a
/// reader, and both halves are kept when both are real, so a file staged and then edited
/// again is the `MM` it is and a conflict is the `UU` it is.
fn xy_status(xy: Option<&[u8]>) -> String {
    let status = xy
        .unwrap_or_default()
        .iter()
        .copied()
        .filter(|byte| !matches!(byte, b' ' | b'.' | 0))
        .map(|byte| byte as char)
        .collect::<String>();
    if status.is_empty() {
        "M".to_owned()
    } else {
        status
    }
}

fn parse_name_status(output: &[u8]) -> Result<Vec<ChangedFile>, String> {
    let mut records = output.split(|byte| *byte == 0);
    let mut files = Vec::new();
    while let Some(record) = records.next() {
        if record.is_empty() {
            continue;
        }
        let status = String::from_utf8(record[..1].to_vec())
            .map_err(|_| "Git returned an invalid change status".to_owned())?;
        let renamed = matches!(status.as_str(), "R" | "C");
        let first = records
            .next()
            .filter(|path| !path.is_empty())
            .ok_or("Git returned an incomplete change status")?;
        let (old_path, path) = if renamed {
            let second = records
                .next()
                .filter(|path| !path.is_empty())
                .ok_or("Git returned an incomplete rename status")?;
            (Some(first), second)
        } else {
            (None, first)
        };
        let name = |bytes: &[u8]| {
            String::from_utf8(bytes.to_vec())
                .map_err(|_| "Git returned a non-UTF-8 changed filename".to_owned())
        };
        files.push(ChangedFile {
            path: name(path)?,
            old_path: old_path.map(name).transpose()?,
            status,
            additions: None,
            deletions: None,
        });
    }
    Ok(files)
}

/// Read one patch under the caps, deciding from the lines themselves what it is.
///
/// The revision is hashed from every line read, including the ones a cap then threw away,
/// because those are the lines a reader is shown and an identity that stopped looking at
/// them would be blind to exactly the changes that matter most.
pub(crate) fn scan(path: String, bytes: &[u8]) -> Result<FileDiff, String> {
    let mut patch = String::new();
    let mut hunks = Vec::new();
    let mut total_lines = 0usize;
    let mut is_binary = false;
    let mut too_large = bytes.len() > MAX_DIFF_BYTES;
    if too_large {
        return Ok(FileDiff {
            path,
            patch,
            revision: digest(bytes),
            hunks,
            total_lines: 0,
            is_binary: false,
            image: None,
            large: false,
            too_large: true,
            symlink_target: None,
        });
    }
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        total_lines += 1;
        if line.len() > MAX_DIFF_LINE_BYTES || total_lines > MAX_DIFF_LINES {
            too_large = true;
        }
        if line.starts_with(b"@@") {
            if hunks.len() >= MAX_DIFF_HUNKS {
                too_large = true;
            } else {
                hunks.push(parse_hunk(line).ok_or_else(|| {
                    format!("Git printed an unreadable hunk header in the diff of {path}")
                })?);
            }
        }
        if line.starts_with(b"Binary files ") || line.starts_with(b"GIT binary patch") {
            is_binary = true;
        }
        if !too_large && !is_binary {
            patch.push_str(&String::from_utf8_lossy(line));
            patch.push('\n');
            if patch.len() > MAX_DIFF_BYTES {
                too_large = true;
                patch.clear();
            }
        } else {
            patch.clear();
        }
    }
    let large = !is_binary
        && !too_large
        && (total_lines > SMALL_DIFF_LINES || patch.len() > SMALL_DIFF_BYTES);
    if is_binary || too_large {
        patch.clear();
    }
    Ok(FileDiff {
        path,
        patch,
        revision: digest(bytes),
        hunks: if is_binary || too_large {
            Vec::new()
        } else {
            hunks
        },
        total_lines,
        is_binary,
        image: None,
        large,
        too_large,
        symlink_target: None,
    })
}

/// `@@ -old[,count] +new[,count] @@`: the new side's start, and its end by its length.
fn parse_hunk(header: &[u8]) -> Option<DiffHunk> {
    let text = std::str::from_utf8(header).ok()?;
    let plus = text.split_once(" +")?.1;
    let end = plus
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(plus.len());
    let start: u64 = plus[..end].parse().ok()?;
    let rest = plus[end..].strip_prefix(',').unwrap_or(&plus[end..]);
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let count: u64 = if digits.is_empty() {
        1
    } else {
        digits.parse().ok()?
    };
    Some(DiffHunk {
        start_line: start,
        end_line: start + count,
    })
}

/// FNV-1a, written out rather than taken from a hasher whose output is not a contract.
///
/// This value is compared against another value from the same build, so it has to be the
/// same number every time and across versions; `DefaultHasher` promises neither.
fn digest(bytes: &[u8]) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash = hash.wrapping_mul(0x100_0000_01b3) ^ u64::from(*byte);
    }
    format!("{hash:016x}")
}
