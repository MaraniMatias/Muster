//! Safe, bounded access to files within a registered checkout.
//!
//! Four rules hold for everything here:
//!
//! * **Nothing is answered partially.** A directory with more than
//!   [`MAX_DIRECTORY_ENTRIES`] rows, a file larger than [`MAX_PREVIEW_BYTES`] and
//!   a resource larger than [`MAX_RESOURCE_BYTES`] are all errors, not a shorter
//!   list and a shorter text. A caller cannot tell a truncated answer from a
//!   complete one, so there is no truncation to offer.
//! * **Only what the workspace registered is reachable.** Every call re-checks the
//!   checkout against the [`Repo`] it claims to belong to: the stored checkout of
//!   that id, the same path in both, an id derived from that path, and a root that
//!   still canonicalizes to itself. A caller that hands over a real id with someone
//!   else's path gets an error rather than that directory.
//! * **Every refusal is explicit.** `.git` in any position, a link used as a
//!   directory, a `..` or absolute path, a binary or non-UTF-8 file, a socket or a
//!   FIFO: each is an error or an absent row, never a mangled one.
//! * **Every step is a handle, not a name.** Each component is opened under the
//!   descriptor of the one before it with `O_NOFOLLOW`, the file is opened under
//!   the directory handle its ancestors reached, and a listing is read from the
//!   directory handle the walk ended on rather than from its path. An ancestor
//!   exchanged for a link between two steps of a walk fails the step instead of
//!   redirecting the rest of it, and a directory that moves after it was opened
//!   keeps the handle that named it.
//!
//! Reading and writing share one boundary. A write compares the file against the
//! caller's expected text and revalidates identity and bytes just before rename;
//! POSIX still permits a narrow race with a non-cooperating writer between that
//! check and publication. A read-only file is refused rather than quietly made
//! writable.
//!
//! What is deliberately not offered yet, so a caller is not surprised by it:
//!
//! * Symlinks are neither listed nor followed, so a checkout containing them shows
//!   fewer rows than it has entries. Following one is how a checkout-relative path
//!   reaches outside the checkout without ever naming `..`, and deciding per link
//!   where it should point is a product decision this slice does not make.
//! * A name that is not UTF-8 has no spelling a `String`-shaped API could accept,
//!   so it is left out of a listing rather than shown as a mangled one.

use std::{
    cmp::Ordering,
    ffi::{CStr, CString, OsStr},
    fs::File,
    io::{self, Read, Write},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
};

use crate::review::ReviewStorage;
use crate::workspace::{checkout_id_for_path, repo_id_for_path, Checkout, Repo};

/// The largest file that can be previewed. Anything larger is refused outright.
pub const MAX_PREVIEW_BYTES: usize = 1024 * 1024;
/// The largest local resource read whole for a document to show. Anything larger
/// is refused outright rather than cut.
///
/// Larger than [`MAX_PREVIEW_BYTES`], because a screenshot a person wrote a
/// document around is not a text file, and equal to the largest diff content
/// this crate already holds in memory (`MAX_DIFF_BYTES`): the same order of
/// magnitude of bytes arriving from an untrusted checkout.
pub const MAX_RESOURCE_BYTES: usize = 16 * 1024 * 1024;
/// The largest directory that can be listed. Anything larger is refused outright.
pub const MAX_DIRECTORY_ENTRIES: usize = 2000;
const MAX_REVIEW_EXPORT_BYTES: usize = 512 * 1024;
const REVIEW_EXPORT_MARKER: &str = "<!-- muster-review-export -->\n";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    /// Checkout-relative, `/`-separated, and the spelling to hand back to a
    /// further listing or a preview.
    pub path: String,
    pub directory: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePreview {
    pub path: String,
    /// The whole file, or no preview at all: a text that stops mid-file looks like
    /// a file that ends there.
    pub text: String,
}

/// The checkout root as the filesystem currently reports it, or why it cannot be used.
///
/// The checkout handed in is a claim, not an authority: only the copy the workspace
/// registered is authority. Identity, both path spellings, and the repository it
/// claims to belong to have to agree with that copy, and the root has to still
/// canonicalize to the path it was registered under -- a checkout that has been
/// replaced by something else is refused rather than read.
fn root(repo: &Repo, checkout: &Checkout) -> Result<PathBuf, String> {
    let registered = repo
        .checkouts
        .iter()
        .find(|entry| entry.id == checkout.id)
        .ok_or("checkout is not registered in this repository")?;
    let forged = || "checkout is not the one this workspace registered".to_owned();
    if checkout.repo_id != repo.id
        || repo.id != repo_id_for_path(&repo.root)
        || checkout.is_missing
        || registered.is_missing
        || checkout.path != checkout.canonical_path
        || checkout.id != checkout_id_for_path(&checkout.canonical_path)
        || registered.repo_id != checkout.repo_id
        || registered.path != checkout.path
        || registered.canonical_path != checkout.canonical_path
        || registered.is_primary != checkout.is_primary
    {
        return Err(forged());
    }
    let root = Path::new(&registered.canonical_path);
    let resolved = root
        .canonicalize()
        .map_err(|error| format!("could not resolve checkout: {error}"))?;
    if resolved != root {
        return Err("checkout path changed; refresh before browsing its files".into());
    }
    if !root.is_dir() {
        return Err("checkout is not a directory".into());
    }
    Ok(resolved)
}

/// A checkout-relative path, as one the browser can walk.
///
/// `""` and `"."` are the checkout root on purpose, because a root that has to be
/// spelled differently from its children is a spelling someone gets wrong. Every
/// other path is a list of real names: absolute paths, `..`, and a `.` inside a
/// longer path are all refused, and so is `.git` as any part of the path, because
/// the browser is for the working tree and the object store is not browsable.
fn relative(path: &str) -> Result<PathBuf, String> {
    if path.is_empty() || path == "." {
        return Ok(PathBuf::new());
    }
    let candidate = Path::new(path);
    if candidate.is_absolute()
        || candidate
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("path must be checkout-relative".into());
    }
    if candidate
        .components()
        .any(|component| component.as_os_str() == ".git")
    {
        return Err(".git is not browsable".into());
    }
    Ok(candidate.to_path_buf())
}

/// Flags every open in the walk carries: `O_NOFOLLOW` refuses a component that has
/// been exchanged for a link, `O_NONBLOCK` keeps a FIFO from parking the open
/// forever, and `O_CLOEXEC` keeps a descriptor out of anything this app spawns.
const WALK_FLAGS: libc::c_int = libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;

/// `openat` handed back a fresh owned descriptor, or it handed back none.
fn descriptor(fd: libc::c_int) -> io::Result<OwnedFd> {
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: each caller passes a fresh, open descriptor from `openat` or `dup`; this is the
    // unique ownership transfer, and no caller closes or wraps the raw descriptor separately.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

pub(super) fn name_of(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
}

/// Walk the already canonical root from `/`, refusing replaced ancestors too.
/// Canonical paths resolve platform aliases such as macOS `/var` before this
/// walk; re-canonicalizing here could accept a link exchanged after validation.
pub(super) fn open_root(root: &Path) -> io::Result<OwnedFd> {
    let relative = root.strip_prefix("/").map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "checkout root must be absolute",
        )
    })?;
    let fd = unsafe {
        libc::openat(
            libc::AT_FDCWD,
            c"/".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | WALK_FLAGS,
        )
    };
    open_directory(&descriptor(fd)?, relative)
}

/// Walk `relative` under `parent`, one component at a time.
///
/// Each component is opened under the descriptor of the one before it, so the next
/// name is resolved by the kernel against the directory that was actually opened:
/// an ancestor exchanged after its step fails the next one with `ELOOP` instead of
/// redirecting it, and one that is not a directory fails with `ENOTDIR`.
pub(super) fn open_directory(parent: &OwnedFd, relative: &Path) -> io::Result<OwnedFd> {
    let mut current = parent.try_clone()?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path must be checkout-relative",
            ));
        };
        let name = name_of(name)?;
        let fd = unsafe {
            libc::openat(
                current.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | WALK_FLAGS,
            )
        };
        current = descriptor(fd)?;
    }
    Ok(current)
}

/// Open the file `name` inside a directory handle the walk already validated.
pub(super) fn open_file(directory: &OwnedFd, name: &CStr) -> io::Result<File> {
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | WALK_FLAGS,
        )
    };
    descriptor(fd).map(File::from)
}

/// What one entry of an open directory is: `Some(true)` a directory, `Some(false)`
/// a regular file, `None` anything a listing does not show.
///
/// Asked through the directory handle and without following it, so an entry
/// exchanged for a link between being read and being classified is seen as the
/// link it is.
pub(super) fn entry_kind(directory: &OwnedFd, name: &CStr) -> Result<Option<bool>, String> {
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    let asked = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            &mut info,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if asked != 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(format!("could not inspect entry: {error}"));
    }
    Ok(match info.st_mode & libc::S_IFMT {
        libc::S_IFDIR => Some(true),
        libc::S_IFREG => Some(false),
        _ => None,
    })
}

/// A directory stream over a duplicate of a validated directory handle.
///
/// `fdopendir` takes ownership of the descriptor it is handed and `closedir`
/// closes it, so the guard holds the stream and its descriptor together and the
/// caller's handle stays open for the entries it is asked about.
struct DirStream(*mut libc::DIR);

impl DirStream {
    fn open(directory: &OwnedFd) -> Result<Self, String> {
        let fd = unsafe { libc::dup(directory.as_raw_fd()) };
        if fd < 0 {
            return Err(format!(
                "could not list directory: {}",
                io::Error::last_os_error()
            ));
        }
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            let error = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(format!("could not list directory: {error}"));
        }
        Ok(Self(stream))
    }

    /// `None` is the end of the directory and nothing else. `readdir` answers "no
    /// more entries" and "failed" with the same null, so `errno` decides, and it
    /// is cleared first because a value left by an earlier call would report a
    /// complete listing as a failure.
    fn next(&mut self) -> Option<Result<CString, String>> {
        unsafe { *errno_cell() = 0 };
        let entry = unsafe { libc::readdir(self.0) };
        if entry.is_null() {
            let code = unsafe { *errno_cell() };
            return if code == 0 {
                None
            } else {
                Some(Err(format!(
                    "could not read directory entry: {}",
                    io::Error::from_raw_os_error(code)
                )))
            };
        }
        // Copied out before the next call: the record belongs to the stream.
        Some(Ok(
            unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_owned()
        ))
    }
}

impl Drop for DirStream {
    fn drop(&mut self) {
        unsafe { libc::closedir(self.0) };
    }
}

/// `readdir` is the one call here whose failure has no return value of its own.
fn errno_cell() -> *mut libc::c_int {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        unsafe { libc::__error() }
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    {
        unsafe { libc::__errno_location() }
    }
}

/// List one directory only; callers decide whether to expand child directories.
///
/// Rows come back alphabetically, compared without case and then exactly, so
/// `Apple` and `apple` keep a stable order next to each other. Directories are not
/// grouped ahead of files. A directory over [`MAX_DIRECTORY_ENTRIES`] rows is an
/// error rather than a short list, because a caller paging by offset cannot tell a
/// cut list from the whole one.
pub fn list_directory(
    repo: &Repo,
    checkout: &Checkout,
    directory: &str,
) -> Result<Vec<FileEntry>, String> {
    let root = root(repo, checkout)?;
    let relative = relative(directory)?;
    let handle = open_root(&root).map_err(|e| e.to_string())?;
    let handle = open_directory(&handle, &relative).map_err(|e| e.to_string())?;
    let prefix = if relative.as_os_str().is_empty() {
        String::new()
    } else {
        format!("{}/", relative.display())
    };
    let mut entries = Vec::new();
    let mut stream = DirStream::open(&handle)?;
    while let Some(item) = stream.next().transpose()? {
        // A name this API cannot spell is a name it cannot list; `to_string_lossy`
        // would hand back a path that resolves to nothing.
        let Ok(name) = item.to_str() else {
            continue;
        };
        if matches!(name, "." | ".." | ".git") {
            continue;
        }
        let Some(is_directory) = entry_kind(&handle, &item)? else {
            continue;
        };
        if entries.len() == MAX_DIRECTORY_ENTRIES {
            return Err(format!(
                "directory holds more than {MAX_DIRECTORY_ENTRIES} entries"
            ));
        }
        entries.push(FileEntry {
            name: name.to_owned(),
            path: format!("{prefix}{name}"),
            directory: is_directory,
        });
    }
    entries.sort_by(by_name);
    Ok(entries)
}

/// Alphabetical, ignoring case, then exactly, so two names that differ only in
/// case keep one fixed order instead of one decided by the filesystem's order.
fn by_name(left: &FileEntry, right: &FileEntry) -> Ordering {
    left.name
        .to_lowercase()
        .cmp(&right.name.to_lowercase())
        .then_with(|| left.name.cmp(&right.name))
}

/// Open one regular file inside a checkout and hold it by descriptor.
///
/// The whole of what decides whether a name may be read lives here, so a reader
/// that opened its own handle would be a second path policy: the checkout has to
/// be the one this workspace registered, the name has to be relative and inside
/// it, `.git` is not reachable, every component is opened under the descriptor
/// of the one before it with `O_NOFOLLOW`, and the entry that comes out has to
/// still be a regular file.
///
/// What comes back is the handle and the length the file claimed when it was
/// opened. Everything after this point reads *through the handle*, so a name
/// exchanged for a link once the file is open no longer describes the bytes.
fn open_regular(root: &Path, relative: &Path) -> Result<(File, u64), String> {
    let name = relative.file_name().ok_or(NOT_REGULAR)?;
    let root_handle = open_root(root).map_err(|e| e.to_string())?;
    let parent = open_directory(&root_handle, relative.parent().unwrap_or(Path::new("")))
        .map_err(|e| e.to_string())?;
    let name = name_of(name).map_err(|e| e.to_string())?;
    // The handle, not the name: `O_NOFOLLOW` refused a link exchanged for the file,
    // and what it opened is what decides whether there is a file here at all.
    let file = open_file(&parent, &name).map_err(|e| e.to_string())?;
    let opened = file
        .metadata()
        .map_err(|error| format!("could not inspect file: {error}"))?;
    if !opened.is_file() {
        return Err(NOT_REGULAR.into());
    }
    Ok((file, opened.len()))
}

/// What every read says about a name that is not a regular file here.
const NOT_REGULAR: &str = "only regular files can be read";

/// Read a file whole, or refuse it for being longer than `limit`.
///
/// The limit is asked twice because a file can grow between being measured and
/// being read: the read itself stops one byte past it, and anything that came to
/// that byte is the same refusal as a file that was already too large. Half a
/// file and the whole file are both "some bytes", and only one of them is the
/// file the caller asked for.
fn read_whole(file: impl Read, limit: usize, oversized: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read file: {error}"))?;
    if bytes.len() > limit {
        return Err(oversized.to_owned());
    }
    Ok(bytes)
}

/// Read a whole text file, or refuse it.
///
/// A file over [`MAX_PREVIEW_BYTES`] is refused rather than cut: half a file and
/// the whole file are both "some text", and only one of them is what the user
/// thinks they are reading. So is a file with a NUL in it, one that is not UTF-8,
/// anything that is not a regular file, and any path that leaves the checkout.
pub fn read_preview(repo: &Repo, checkout: &Checkout, path: &str) -> Result<FilePreview, String> {
    let root = root(repo, checkout)?;
    let relative = relative(path)?;
    let too_large = "file is larger than the 1 MiB preview limit";
    let (file, length) = open_regular(&root, &relative)?;
    if length > MAX_PREVIEW_BYTES as u64 {
        return Err(too_large.into());
    }
    let bytes = read_whole(file, MAX_PREVIEW_BYTES, too_large)?;
    if bytes.contains(&0) {
        return Err("binary files cannot be previewed".into());
    }
    let text = String::from_utf8(bytes).map_err(|_| "file is not valid UTF-8".to_owned())?;
    Ok(FilePreview {
        path: path.into(),
        text,
    })
}

/// The refusal a write offers when the bytes on disk are not the bytes the
/// caller read.
///
/// Not a conflict to resolve: it is the reason the save did not happen, and it
/// names the way out rather than the state.
const CHANGED_ON_DISK: &str = "file changed on disk; reload it before saving";

/// A write failure, including whether replacement was already published.
#[derive(Debug)]
pub enum WritePreviewError {
    BeforePublication(String),
    PublishedButNotDurable(String),
}

impl std::fmt::Display for WritePreviewError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BeforePublication(message) | Self::PublishedButNotDurable(message) => {
                formatter.write_str(message)
            }
        }
    }
}

/// Replace one text file inside a checkout, or refuse it.
///
/// The same walk a read is [(`read_preview`)] answered by — the registered
/// checkout, a relative name, no `.git`, every component opened under the
/// descriptor of the one before it with `O_NOFOLLOW`, and an entry that has to
/// still be a regular file — plus the two rules a writer owes on top of it:
///
/// - **The file still matches at the checks this operation can make.** `expected`
///   is compared with the current bytes and the original file identity is checked
///   again just before publication. A non-cooperating writer can still change the
///   name in the small interval between that check and `renameat`; POSIX does not
///   provide a portable compare-and-swap rename for this operation.
/// - **A read-only file stays read-only.** The replacement is a rename into the
///   directory, which needs write access to the *directory* rather than to the
///   file, so a save would quietly undo the bit somebody set. It is refused
///   instead.
///
/// The replacement is atomic: readers see the old name or the complete new
/// file, never a partial write. Failures before rename leave the original intact;
/// after rename, directory-sync failure is reported as
/// [`WritePreviewError::PublishedButNotDurable`], because the new bytes are visible
/// but persistence across a system failure was not confirmed.
pub fn write_preview(
    repo: &Repo,
    checkout: &Checkout,
    path: &str,
    expected: &str,
    content: &str,
) -> Result<(), WritePreviewError> {
    write_preview_with_hooks(
        repo,
        checkout,
        path,
        expected,
        content,
        || {},
        |fd| unsafe { libc::fsync(fd) },
    )
}

fn write_preview_with_hooks(
    repo: &Repo,
    checkout: &Checkout,
    path: &str,
    expected: &str,
    content: &str,
    before_publish: impl FnOnce(),
    sync_directory: impl FnOnce(std::os::fd::RawFd) -> i32,
) -> Result<(), WritePreviewError> {
    static SAVE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _save = SAVE_LOCK.lock().map_err(|_| {
        WritePreviewError::BeforePublication("could not serialize file saves".into())
    })?;
    let root = root(repo, checkout).map_err(WritePreviewError::BeforePublication)?;
    let relative = relative(path).map_err(WritePreviewError::BeforePublication)?;
    let too_large = "file is larger than the 1 MiB preview limit";
    if content.len() > MAX_PREVIEW_BYTES || content.contains('\0') {
        return Err(WritePreviewError::BeforePublication(
            if content.contains('\0') {
                "binary files cannot be previewed".into()
            } else {
                too_large.into()
            },
        ));
    }
    let name = relative
        .file_name()
        .ok_or_else(|| WritePreviewError::BeforePublication(NOT_REGULAR.into()))?;
    let root_handle = open_root(&root)
        .map_err(|error| WritePreviewError::BeforePublication(error.to_string()))?;
    let parent = open_directory(&root_handle, relative.parent().unwrap_or(Path::new("")))
        .map_err(|error| WritePreviewError::BeforePublication(error.to_string()))?;
    let name =
        name_of(name).map_err(|error| WritePreviewError::BeforePublication(error.to_string()))?;
    let file = open_file(&parent, &name)
        .map_err(|error| WritePreviewError::BeforePublication(error.to_string()))?;
    let opened = file.metadata().map_err(|error| {
        WritePreviewError::BeforePublication(format!("could not inspect file: {error}"))
    })?;
    if !opened.is_file() {
        return Err(WritePreviewError::BeforePublication(NOT_REGULAR.into()));
    }
    if opened.len() > MAX_PREVIEW_BYTES as u64 {
        return Err(WritePreviewError::BeforePublication(too_large.into()));
    }
    let mode = opened.permissions().mode() & 0o7777;
    if mode & 0o222 == 0 {
        return Err(WritePreviewError::BeforePublication(
            "file is read-only".into(),
        ));
    }
    let current = read_whole(file, MAX_PREVIEW_BYTES, too_large)
        .map_err(WritePreviewError::BeforePublication)?;
    if current.contains(&0) {
        return Err(WritePreviewError::BeforePublication(
            "binary files cannot be previewed".into(),
        ));
    }
    let current = String::from_utf8(current)
        .map_err(|_| WritePreviewError::BeforePublication("file is not valid UTF-8".into()))?;
    if current != expected {
        return Err(WritePreviewError::BeforePublication(CHANGED_ON_DISK.into()));
    }
    replace(
        &parent,
        &name,
        content.as_bytes(),
        mode,
        &opened,
        expected,
        (before_publish, sync_directory),
    )
    .map_err(|error| {
        if error.starts_with("published but directory sync failed:") {
            WritePreviewError::PublishedButNotDurable(error)
        } else {
            WritePreviewError::BeforePublication(error)
        }
    })
}

/// Create a new Markdown review export without following a path outside its owner.
///
/// Workdir exports are walked from the registered checkout root with the same
/// descriptor-based, no-symlink policy as previews. Default exports are walked
/// from the canonical home directory. Existing names are never overwritten.
pub fn export_review_markdown(
    repo: &Repo,
    checkout: &Checkout,
    storage: ReviewStorage,
    timestamp: &str,
    markdown: &str,
) -> Result<PathBuf, String> {
    if !valid_review_timestamp(timestamp) {
        return Err("review export timestamp is invalid".into());
    }
    if markdown.len() > MAX_REVIEW_EXPORT_BYTES || markdown.contains('\0') {
        return Err("review export is too large or contains binary text".into());
    }
    if !markdown.starts_with(REVIEW_EXPORT_MARKER) {
        return Err("review export is missing its ownership marker".into());
    }
    let (base, relative) = review_export_location(repo, checkout, storage)?;
    let parent = open_root(&base).map_err(|error| error.to_string())?;
    let directory = create_directories(&parent, &relative)?;
    let filename = create_review_file(&directory, timestamp, markdown.as_bytes())?;
    Ok(base.join(relative).join(filename))
}

/// The same destination shown in Settings and used by the exporter.
/// Resolving it never creates a directory or an export.
pub fn review_export_folder(
    repo: &Repo,
    checkout: &Checkout,
    storage: ReviewStorage,
) -> Result<PathBuf, String> {
    let (base, relative) = review_export_location(repo, checkout, storage)?;
    Ok(base.join(relative))
}

fn review_export_location(
    repo: &Repo,
    checkout: &Checkout,
    storage: ReviewStorage,
) -> Result<(PathBuf, PathBuf), String> {
    let checkout_root = root(repo, checkout)?;
    match storage {
        ReviewStorage::Workdir => Ok((checkout_root, PathBuf::from(".muster/reviews"))),
        ReviewStorage::Default => {
            let home = std::env::var_os("HOME").ok_or("HOME is unavailable")?;
            let home = PathBuf::from(home);
            if !home.is_absolute() {
                return Err("HOME must be an absolute directory".into());
            }
            let base = home
                .canonicalize()
                .map_err(|error| format!("could not resolve HOME: {error}"))?;
            Ok((base, PathBuf::from(".muster/tmp/reviews")))
        }
    }
}

/// Remove only this app's marked Markdown exports from its default review cache.
/// Workdir exports belong to the checkout and are never removed by this action.
pub fn clear_review_exports(
    repo: &Repo,
    checkout: &Checkout,
    storage: ReviewStorage,
) -> Result<usize, String> {
    if storage != ReviewStorage::Default {
        return Err("review exports inside a checkout are not cleared here".into());
    }
    let (base, relative) = review_export_location(repo, checkout, storage)?;
    let parent = open_root(&base).map_err(|error| error.to_string())?;
    let directory = match open_directory(&parent, &relative) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("could not open review export folder: {error}")),
    };
    clear_marked_review_exports(&directory)
}

fn clear_marked_review_exports(directory: &OwnedFd) -> Result<usize, String> {
    let mut stream = DirStream::open(directory)?;
    let mut names = Vec::new();
    let mut count = 0;
    while let Some(entry) = stream.next() {
        let name = entry?;
        if name.as_bytes() == b"." || name.as_bytes() == b".." {
            continue;
        }
        count += 1;
        if count > MAX_DIRECTORY_ENTRIES {
            return Err(format!(
                "review export folder has more than {MAX_DIRECTORY_ENTRIES} entries"
            ));
        }
        if !is_review_export_filename(&name) || entry_kind(directory, &name)? != Some(false) {
            continue;
        }
        let Ok(mut file) = open_file(directory, &name) else {
            continue;
        };
        let Ok(metadata) = file.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let mut marker = [0; REVIEW_EXPORT_MARKER.len()];
        if file.read_exact(&mut marker).is_err()
            || marker.as_slice() != REVIEW_EXPORT_MARKER.as_bytes()
        {
            continue;
        }
        names.push((name, metadata.dev(), metadata.ino()));
    }
    let mut removed = 0;
    for (name, device, inode) in names {
        let mut current: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                name.as_ptr(),
                &mut current,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                continue;
            }
            return Err(format!("could not inspect review export: {error}"));
        }
        if current.st_mode & libc::S_IFMT != libc::S_IFREG
            || current.st_dev as u64 != device
            || current.st_ino as u64 != inode
        {
            continue;
        }
        if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                continue;
            }
            return Err(format!("could not delete review export: {error}"));
        }
        removed += 1;
    }
    if removed > 0 && unsafe { libc::fsync(directory.as_raw_fd()) } != 0 {
        return Err(format!(
            "could not sync review export folder: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(removed)
}

fn is_review_export_filename(name: &CStr) -> bool {
    let Ok(name) = std::str::from_utf8(name.to_bytes()) else {
        return false;
    };
    let Some(stem) = name.strip_suffix(".md") else {
        return false;
    };
    let Some(timestamp) = stem.get(..15) else {
        return false;
    };
    if !valid_review_timestamp(timestamp) {
        return false;
    }
    match stem.get(15..) {
        None | Some("") => true,
        Some(suffix) => suffix
            .strip_prefix('-')
            .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())),
    }
}

fn valid_review_timestamp(value: &str) -> bool {
    value.len() == 15
        && value.as_bytes()[4] == b'-'
        && value.as_bytes()[7] == b'-'
        && value.as_bytes()[10] == b'-'
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7 | 10) || byte.is_ascii_digit())
}

fn create_directories(parent: &OwnedFd, relative: &Path) -> Result<OwnedFd, String> {
    let mut current = parent.try_clone().map_err(|error| error.to_string())?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err("review directory must be relative".into());
        };
        let name = name_of(name).map_err(|error| error.to_string())?;
        if unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(format!("could not create review export folder: {error}"));
            }
        }
        let fd = unsafe {
            libc::openat(
                current.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | WALK_FLAGS,
            )
        };
        current = descriptor(fd)
            .map_err(|error| format!("could not open review export folder: {error}"))?;
    }
    Ok(current)
}

fn create_review_file(
    directory: &OwnedFd,
    timestamp: &str,
    bytes: &[u8],
) -> Result<String, String> {
    let mut temporary_file = None;
    for _ in 0..32 {
        let temporary = temporary_name()?;
        let temporary_c = name_of(OsStr::new(&temporary)).map_err(|error| error.to_string())?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                temporary_c.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | WALK_FLAGS,
                0o600,
            )
        };
        if fd >= 0 {
            temporary_file = Some((temporary_c, fd));
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::AlreadyExists {
            return Err(format!(
                "could not create review export temporary file: {error}"
            ));
        }
    }
    let Some((temporary_c, fd)) = temporary_file else {
        return Err("could not allocate a temporary review export file".into());
    };
    let mut file = File::from(
        descriptor(fd)
            .map_err(|error| format!("could not create review export temporary file: {error}"))?,
    );
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        unsafe { libc::unlinkat(directory.as_raw_fd(), temporary_c.as_ptr(), 0) };
        return Err(format!("could not write review export: {error}"));
    }
    drop(file);
    for suffix in 1..=MAX_DIRECTORY_ENTRIES {
        let filename = if suffix == 1 {
            format!("{timestamp}.md")
        } else {
            format!("{timestamp}-{suffix}.md")
        };
        let target = name_of(OsStr::new(&filename)).map_err(|error| error.to_string())?;
        let linked = unsafe {
            libc::linkat(
                directory.as_raw_fd(),
                temporary_c.as_ptr(),
                directory.as_raw_fd(),
                target.as_ptr(),
                0,
            )
        };
        if linked == 0 {
            unsafe { libc::unlinkat(directory.as_raw_fd(), temporary_c.as_ptr(), 0) };
            if unsafe { libc::fsync(directory.as_raw_fd()) } != 0 {
                return Err(format!(
                    "could not sync review export folder: {}",
                    io::Error::last_os_error()
                ));
            }
            return Ok(filename);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::AlreadyExists {
            unsafe { libc::unlinkat(directory.as_raw_fd(), temporary_c.as_ptr(), 0) };
            return Err(format!("could not create review export: {error}"));
        }
    }
    unsafe { libc::unlinkat(directory.as_raw_fd(), temporary_c.as_ptr(), 0) };
    Err("too many review exports share this timestamp".into())
}

/// What a replacement leaves behind when it cannot be finished.
const ORPHANED: &str = "could not replace file";

/// Write `bytes` beside the file and rename it over the name.
///
/// The temporary file is created under the directory handle the walk already
/// validated, exclusively and without following anything, so a name that is
/// already there is a failure rather than a file somebody else's bytes land in.
fn replace(
    parent: &OwnedFd,
    name: &CStr,
    bytes: &[u8],
    mode: u32,
    original: &std::fs::Metadata,
    expected: &str,
    hooks: (impl FnOnce(), impl FnOnce(std::os::fd::RawFd) -> i32),
) -> Result<(), String> {
    let (before_publish, sync_directory) = hooks;
    let temporary = temporary_name()?;
    let held = name_of(OsStr::new(&temporary)).map_err(|error| error.to_string())?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            held.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | WALK_FLAGS,
            // Private while it is being written, and given the mode the file had
            // before the rename: `umask` may take bits off the mode a file is
            // created with, and a replacement must not quietly change the
            // permissions of the file it replaces.
            0o600,
        )
    };
    let mut file = match descriptor(fd) {
        Ok(fd) => File::from(fd),
        Err(error) => return Err(format!("could not write file: {error}")),
    };
    let written = (|| {
        file.set_permissions(std::fs::Permissions::from_mode(mode))
            .map_err(|error| format!("could not write file: {error}"))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("could not write file: {error}"))
    })();
    // Dropped before the rename: the descriptor has to be closed for the name
    // the file answers to to be the new one.
    drop(file);
    if let Err(error) = written {
        unsafe { libc::unlinkat(parent.as_raw_fd(), held.as_ptr(), 0) };
        return Err(error);
    }
    // Re-open and compare both inode identity and contents as late as possible.
    // A non-cooperating writer can still race the following rename; POSIX rename
    // has no portable compare-and-swap form on both supported platforms.
    before_publish();
    if let Err(error) = revalidate_target(parent, name, original, expected) {
        unsafe { libc::unlinkat(parent.as_raw_fd(), held.as_ptr(), 0) };
        return Err(error);
    }
    if unsafe {
        libc::renameat(
            parent.as_raw_fd(),
            held.as_ptr(),
            parent.as_raw_fd(),
            name.as_ptr(),
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        unsafe { libc::unlinkat(parent.as_raw_fd(), held.as_ptr(), 0) };
        return Err(format!("{ORPHANED}: {error}"));
    }
    if sync_directory(parent.as_raw_fd()) != 0 {
        return Err(format!(
            "published but directory sync failed: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Recheck the target name's identity and bytes immediately before publication.
fn revalidate_target(
    parent: &OwnedFd,
    name: &CStr,
    original: &std::fs::Metadata,
    expected: &str,
) -> Result<(), String> {
    let current = open_file(parent, name).map_err(|_| CHANGED_ON_DISK.to_owned())?;
    let metadata = current
        .metadata()
        .map_err(|error| format!("could not inspect file: {error}"))?;
    if metadata.dev() != original.dev() || metadata.ino() != original.ino() {
        return Err(CHANGED_ON_DISK.into());
    }
    let current = read_whole(
        current,
        MAX_PREVIEW_BYTES,
        "file is larger than the 1 MiB preview limit",
    )?;
    if current != expected.as_bytes() {
        return Err(CHANGED_ON_DISK.into());
    }
    Ok(())
}

/// A name no replacement in this process is using.
///
/// `O_EXCL` makes a collision a failure rather than a corruption, and this is
/// what keeps a collision from being somebody else's file: the pid separates two
/// Muster windows and the counter separates two saves inside one.
fn temporary_name() -> Result<String, String> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(format!(".muster-save-{}-{count}.tmp", std::process::id()))
}

/// Read a whole file out of a checkout as bytes, or refuse it.
///
/// The same boundary a preview is read through -- [`open_regular`] decides what
/// may be read and [`read_whole`] bounds how much of it comes back -- with the
/// two rules that only exist because a preview is text left out: a NUL and
/// invalid UTF-8 are ordinary bytes here, and the bound is
/// [`MAX_RESOURCE_BYTES`] rather than the preview's, because what comes back is
/// something to decode rather than something to show.
///
/// The answer is the bytes. A caller that wanted a path to reopen itself would
/// be asking for a name the walk has already stopped vouching for.
pub fn read_resource(repo: &Repo, checkout: &Checkout, path: &str) -> Result<Vec<u8>, String> {
    let root = root(repo, checkout)?;
    let relative = relative(path)?;
    let too_large = format!(
        "resource is larger than the {} MiB limit",
        MAX_RESOURCE_BYTES / (1024 * 1024)
    );
    let (file, length) = open_regular(&root, &relative)?;
    if length > MAX_RESOURCE_BYTES as u64 {
        return Err(too_large);
    }
    read_whole(file, MAX_RESOURCE_BYTES, &too_large)
}

/// The absolute path of something inside a checkout, proved to be there.
///
/// This is the whole of the chain that decides what may be handed to another
/// program, and it is the same one a preview reads through: the checkout has to
/// be the one this workspace registered, the name has to be relative and inside
/// it, and the last component has to still be a file or a directory under a walk
/// that refuses a link at every step. A symlink is refused rather than resolved,
/// which is stricter than resolving-and-checking and is the rule this module
/// already keeps: following a link is how a checkout-relative path reaches
/// outside the checkout without ever naming `..`.
///
/// What comes out is a path an application can be handed. It is not a promise
/// about what that application does with it; this is the last place that has a
/// say, and it has said yes to exactly this path in this checkout.
pub fn external_path(repo: &Repo, checkout: &Checkout, path: &str) -> Result<PathBuf, String> {
    let root = root(repo, checkout)?;
    let relative = relative(path)?;
    let name = match relative.file_name() {
        Some(name) => name,
        // The root itself, and the empty name that spells it: a folder is
        // something to open as well as something to walk into.
        None => return Ok(root),
    };
    let root_handle = open_root(&root).map_err(|error| error.to_string())?;
    let parent = open_directory(&root_handle, relative.parent().unwrap_or(Path::new("")))
        .map_err(|error| error.to_string())?;
    let name = name_of(name).map_err(|error| error.to_string())?;
    match entry_kind(&parent, &name)? {
        // Asked through the directory handle and without following it, so an
        // entry exchanged for a link between the walk and this question is seen
        // as the link it is rather than as whatever it points at.
        Some(_) => Ok(root.join(relative)),
        None => Err(format!("{path} is not a file or a folder here")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        io::Write,
        os::unix::{fs::symlink, fs::PermissionsExt, net::UnixListener},
    };
    use tempfile::{tempdir, TempDir};

    fn checkout_of(dir: &TempDir) -> (Repo, Checkout) {
        let repo = Repo::plain(dir.path(), "now").unwrap();
        let checkout = repo.checkouts[0].clone();
        (repo, checkout)
    }

    /// The two calls both readers are made of, up to the bound: what decides
    /// whether a name may be read, and the handle it decided it for. Only the
    /// step between them -- the size check and the read -- is what a test has to
    /// stand in the middle of, and standing there is the only way to reach the
    /// handle a real read uses.
    fn open_relative(repo: &Repo, checkout: &Checkout, path: &str) -> Result<(File, u64), String> {
        let root = root(repo, checkout)?;
        let relative = relative(path)?;
        open_regular(&root, &relative)
    }

    #[test]
    fn lists_the_root_alphabetically_and_never_reaches_git() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("zebra.txt"), "z").unwrap();
        fs::write(dir.path().join("Apple.txt"), "a").unwrap();
        fs::write(dir.path().join("Banana.txt"), "b").unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/config"), "[core]").unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        let outside = tempdir().unwrap();
        symlink(outside.path(), dir.path().join("escape")).unwrap();
        let (repo, checkout) = checkout_of(&dir);

        let names = |rows: Vec<FileEntry>| {
            rows.into_iter()
                .map(|row| row.name)
                .collect::<Vec<String>>()
        };
        let listed = names(list_directory(&repo, &checkout, "").unwrap());
        // Case-insensitive, and no directory grouped ahead of a file that sorts
        // earlier. The exact tiebreak needs two names that differ only in case,
        // which a case-insensitive filesystem cannot hold at once, so it is checked
        // on the comparator below.
        assert_eq!(listed, ["Apple.txt", "Banana.txt", "src", "zebra.txt"]);
        assert_eq!(
            list_directory(&repo, &checkout, ".").unwrap(),
            list_directory(&repo, &checkout, "").unwrap()
        );
        assert_eq!(
            list_directory(&repo, &checkout, "src").unwrap(),
            vec![FileEntry {
                name: "main.rs".into(),
                path: "src/main.rs".into(),
                directory: false,
            }]
        );

        for refused in [".git", ".git/config", "src/.git", "./src", "..", "../.."] {
            assert!(
                list_directory(&repo, &checkout, refused).is_err(),
                "{refused} was listed"
            );
        }

        let row = |name: &str| FileEntry {
            name: name.into(),
            path: name.into(),
            directory: false,
        };
        let mut rows = vec![row("apple"), row("Banana"), row("APPLE"), row("banana")];
        rows.sort_by(by_name);
        assert_eq!(rows, ["APPLE", "apple", "Banana", "banana"].map(row),);
    }

    #[test]
    fn listing_refuses_a_directory_over_the_limit_instead_of_truncating_it() {
        let dir = tempdir().unwrap();
        let over = format!("{:06}", MAX_DIRECTORY_ENTRIES);
        for index in 0..=MAX_DIRECTORY_ENTRIES {
            fs::write(dir.path().join(format!("{index:06}")), "").unwrap();
        }
        let (repo, checkout) = checkout_of(&dir);
        assert!(list_directory(&repo, &checkout, "")
            .unwrap_err()
            .contains("2000 entries"));

        // Exactly at the limit still lists in full: the refusal is the boundary,
        // not a shorter page.
        fs::remove_file(dir.path().join(&over)).unwrap();
        assert_eq!(
            list_directory(&repo, &checkout, "").unwrap().len(),
            MAX_DIRECTORY_ENTRIES
        );
    }

    #[test]
    fn preview_returns_a_whole_file_or_nothing() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("hello.txt"), "hello").unwrap();
        fs::write(dir.path().join("empty.txt"), "").unwrap();
        fs::write(
            dir.path().join("at-limit.txt"),
            vec![b'a'; MAX_PREVIEW_BYTES],
        )
        .unwrap();
        fs::write(
            dir.path().join("over.txt"),
            vec![b'a'; MAX_PREVIEW_BYTES + 1],
        )
        .unwrap();
        fs::write(dir.path().join("binary.bin"), [0x00, 0x01]).unwrap();
        fs::write(dir.path().join("latin1.txt"), [0xff, 0xfe]).unwrap();
        let (repo, checkout) = checkout_of(&dir);

        let hello = read_preview(&repo, &checkout, "hello.txt").unwrap();
        assert_eq!(hello.path, "hello.txt");
        assert_eq!(hello.text, "hello");
        assert_eq!(
            read_preview(&repo, &checkout, "empty.txt").unwrap().text,
            ""
        );
        assert_eq!(
            read_preview(&repo, &checkout, "at-limit.txt")
                .unwrap()
                .text
                .len(),
            MAX_PREVIEW_BYTES
        );
        for refused in ["over.txt", "binary.bin", "latin1.txt"] {
            assert!(
                read_preview(&repo, &checkout, refused).is_err(),
                "{refused}"
            );
        }
    }

    /// A picture a document names is read through the same boundary a preview
    /// is, and what comes back is bytes rather than a name: there is no path in
    /// the answer for a caller to reopen later.
    #[test]
    fn a_resource_is_read_whole_from_the_descriptor_or_not_at_all() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let png = [
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0xff, 0xfe, 0x00,
        ];
        fs::create_dir_all(dir.path().join("docs/assets")).unwrap();
        fs::write(dir.path().join("docs/assets/picture.png"), png).unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/config"), "[core]").unwrap();
        fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        symlink(
            outside.path().join("secret.txt"),
            dir.path().join("escape.png"),
        )
        .unwrap();
        UnixListener::bind(dir.path().join("socket.png")).unwrap();
        let fifo = dir.path().join("pipe.png");
        assert_eq!(
            unsafe {
                libc::mkfifo(
                    std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes())
                        .expect("a path with no NUL")
                        .as_ptr(),
                    0o600,
                )
            },
            0
        );
        // Two sparse files: what is refused here is the length, not the content.
        let sparse = |name: &str, length: u64| {
            let made = dir.path().join(name);
            fs::File::create(&made).unwrap().set_len(length).unwrap();
        };
        sparse("at-limit.png", MAX_RESOURCE_BYTES as u64);
        sparse("over-limit.png", MAX_RESOURCE_BYTES as u64 + 1);
        let (repo, checkout) = checkout_of(&dir);

        // A nested document's own picture, whole, bytes and all: the NUL and the
        // invalid UTF-8 that refuse a preview are ordinary content here.
        assert_eq!(
            read_resource(&repo, &checkout, "docs/assets/picture.png").unwrap(),
            png
        );
        assert_eq!(
            read_resource(&repo, &checkout, "at-limit.png")
                .unwrap()
                .len(),
            MAX_RESOURCE_BYTES
        );
        assert!(read_resource(&repo, &checkout, "over-limit.png")
            .unwrap_err()
            .contains("16 MiB"));

        for refused in [
            // A path that leaves, a path that is not relative, and the store.
            "../escape.png",
            "/etc/passwd",
            ".git/config",
            // Not a regular file, or nothing at all under that name.
            "escape.png",
            "socket.png",
            "pipe.png",
            "docs",
            "",
            "docs/assets/missing.png",
        ] {
            assert!(
                read_resource(&repo, &checkout, refused).is_err(),
                "{refused} was read"
            );
        }

        // The same identity rules a preview answers to, because it is the same
        // walk: a claim about a checkout is refused before the name is walked.
        let mut forged = checkout.clone();
        forged.id = "checkout:forged".into();
        assert!(read_resource(&repo, &forged, "docs/assets/picture.png").is_err());
        let mut elsewhere = checkout.clone();
        elsewhere.path = outside.path().canonicalize().unwrap().display().to_string();
        elsewhere.canonical_path = elsewhere.path.clone();
        assert!(read_resource(&repo, &elsewhere, "secret.txt").is_err());
        assert_eq!(
            read_resource(&repo, &checkout, "docs/assets/picture.png").unwrap(),
            png,
            "the genuine checkout still reads after all of that"
        );
    }

    /// A name exchanged for a link after the walk reached it must not redirect
    /// the read. Either the exchange is refused outright, or the bytes still come
    /// from the file that was validated -- never from the outside marker.
    ///
    /// Both sides of the boundary are checked, because a single-threaded test can
    /// only reach one answer to a race by standing on the other side of it. The
    /// first half exchanges the names before the walk and asks the whole
    /// production read, which has to refuse. The second half exchanges them after
    /// [`open_regular`] has opened and classified the file, and reads through the
    /// handle that walk handed back, which has to still hold the validated file.
    #[test]
    fn a_resource_read_stays_attached_to_the_file_it_validated() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        let picture = dir.path().join("sub/picture.png");
        fs::write(&picture, "inside").unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("picture.png"), "outside marker").unwrap();
        let (repo, checkout) = checkout_of(&dir);

        // Before the walk: the file itself is a link to a file outside the
        // checkout, and `O_NOFOLLOW` is what keeps the walk out of it.
        fs::write(dir.path().join("sub/decoy.png"), "inside").unwrap();
        fs::remove_file(dir.path().join("sub/decoy.png")).unwrap();
        symlink(
            outside.path().join("picture.png"),
            dir.path().join("sub/decoy.png"),
        )
        .unwrap();
        assert!(
            read_resource(&repo, &checkout, "sub/decoy.png").is_err(),
            "a link exchanged for the file is refused rather than followed"
        );

        // Before the walk: the ancestor is a link to a directory outside the
        // checkout, and the name itself now leads to the marker.
        fs::rename(dir.path().join("sub"), dir.path().join("moved")).unwrap();
        symlink(outside.path(), dir.path().join("sub")).unwrap();
        assert_eq!(
            fs::read_to_string(&picture).unwrap(),
            "outside marker",
            "the fixture is only an escape if the name leads out of the checkout"
        );
        assert!(
            read_resource(&repo, &checkout, "sub/picture.png").is_err(),
            "an ancestor exchanged for a link does not redirect the read out of the checkout"
        );

        // Across the boundary. The ancestor goes back to the directory it was, so
        // the next open is of a real file, and then everything the name describes
        // is exchanged while the descriptor is already in hand.
        fs::remove_file(dir.path().join("sub")).unwrap();
        fs::rename(dir.path().join("moved"), dir.path().join("sub")).unwrap();
        let (file, length) = open_relative(&repo, &checkout, "sub/picture.png").unwrap();
        assert_eq!(length, "inside".len() as u64);

        fs::remove_file(&picture).unwrap();
        symlink(outside.path().join("picture.png"), &picture).unwrap();
        fs::rename(dir.path().join("sub"), dir.path().join("moved")).unwrap();
        symlink(outside.path(), dir.path().join("sub")).unwrap();
        assert_eq!(
            fs::read_to_string(&picture).unwrap(),
            "outside marker",
            "reopening the name now would read the marker, which is the whole race"
        );

        // The read is through the handle, so what comes back is the file that was
        // validated and the bytes it had, not the marker that answers to its name.
        assert_eq!(
            read_whole(file, MAX_RESOURCE_BYTES, "over").unwrap(),
            b"inside"
        );
    }

    /// The bound is asked of the file and of the read, because a file that grows
    /// between the two is longer than the one that was measured. A reader that
    /// keeps only what fits is handing back a file that ends where it does not.
    ///
    /// Done to a real file rather than to a reader made to misbehave: the
    /// production [`open_regular`] returns a handle and the length the file
    /// claimed, the file is then grown past the bound behind that handle, and the
    /// production [`read_whole`] has to ask the read as well as the file. The
    /// length that was recorded no longer describes what the handle reads.
    #[test]
    fn a_file_that_grows_between_the_size_and_the_read_is_refused() {
        let dir = tempdir().unwrap();
        let made = dir.path().join("growing.txt");
        fs::write(&made, vec![b'a'; MAX_PREVIEW_BYTES]).unwrap();
        let (repo, checkout) = checkout_of(&dir);
        // The wording `read_preview` refuses an oversized file with.
        let too_large = "file is larger than the 1 MiB preview limit";

        // Exactly at the bound comes back whole: the boundary is the refusal, not
        // a shorter file.
        let (file, length) = open_relative(&repo, &checkout, "growing.txt").unwrap();
        assert_eq!(length, MAX_PREVIEW_BYTES as u64);
        assert_eq!(
            read_whole(file, MAX_PREVIEW_BYTES, too_large)
                .unwrap()
                .len(),
            MAX_PREVIEW_BYTES
        );

        // The same file, opened again at a length that fits, and one byte longer by
        // the time it is read -- a growth no single measurement can see, and the
        // one a size check alone would hand back as a complete file.
        let (file, length) = open_relative(&repo, &checkout, "growing.txt").unwrap();
        assert!(length <= MAX_PREVIEW_BYTES as u64);
        fs::OpenOptions::new()
            .append(true)
            .open(&made)
            .unwrap()
            .write_all(b"a")
            .unwrap();
        assert_eq!(made.metadata().unwrap().len(), MAX_PREVIEW_BYTES as u64 + 1);
        assert_eq!(
            read_whole(file, MAX_PREVIEW_BYTES, too_large).unwrap_err(),
            too_large,
            "one byte past the bound is the same refusal as a file that was already too long"
        );
    }

    #[test]
    fn preview_refuses_links_special_files_and_paths_that_leave_the_checkout() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        fs::write(dir.path().join("hello.txt"), "hello").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/inner.txt"), "inner").unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::create_dir(dir.path().join("sub/.git")).unwrap();
        fs::write(dir.path().join("sub/.git/config"), "[core]").unwrap();
        symlink(
            outside.path().join("secret.txt"),
            dir.path().join("secret-link"),
        )
        .unwrap();
        symlink(outside.path(), dir.path().join("escape")).unwrap();
        symlink(dir.path().join("sub"), dir.path().join("inside-link")).unwrap();
        UnixListener::bind(dir.path().join("socket")).unwrap();
        let (repo, checkout) = checkout_of(&dir);

        assert_eq!(
            read_preview(&repo, &checkout, "sub/inner.txt")
                .unwrap()
                .text,
            "inner"
        );
        for refused in [
            "secret-link",
            "escape/secret.txt",
            "inside-link/inner.txt",
            "socket",
            ".git/config",
            "sub/.git/config",
            "sub/../hello.txt",
            "../escape",
            "",
            "sub",
        ] {
            assert!(
                read_preview(&repo, &checkout, refused).is_err(),
                "{refused} was previewed"
            );
        }
        for refused in ["escape", "inside-link", "hello.txt", ".git"] {
            assert!(
                list_directory(&repo, &checkout, refused).is_err(),
                "{refused} was listed"
            );
        }
        // Only the two plain entries: the three links, `.git` and the socket are
        // all absent, and none of them is listed as something else.
        let listed = list_directory(&repo, &checkout, "").unwrap();
        assert_eq!(
            listed,
            vec![
                FileEntry {
                    name: "hello.txt".into(),
                    path: "hello.txt".into(),
                    directory: false,
                },
                FileEntry {
                    name: "sub".into(),
                    path: "sub".into(),
                    directory: true,
                },
            ]
        );
    }

    #[test]
    fn an_ancestor_exchanged_for_a_link_after_the_walk_keeps_the_child_inside() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/inner.txt"), "inner").unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("inner.txt"), "secret").unwrap();
        let (repo, checkout) = checkout_of(&dir);

        let root = open_root(Path::new(&checkout.canonical_path)).unwrap();
        let sub = open_directory(&root, Path::new("sub")).unwrap();

        // The ancestor is exchanged for a link to a directory outside the checkout
        // after the walk has already opened it.
        fs::rename(dir.path().join("sub"), dir.path().join("moved")).unwrap();
        symlink(outside.path(), dir.path().join("sub")).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("sub/inner.txt")).unwrap(),
            "secret",
            "the fixture is only an escape if the path itself leads out of the checkout"
        );

        // The handle still names the directory that was validated, so the child is
        // read from inside the checkout and never through the link.
        let name = CString::new("inner.txt").unwrap();
        let mut text = String::new();
        open_file(&sub, &name)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "inner");

        // A path that walks the same ancestor again is refused instead of followed
        // out to the file behind the link.
        assert!(read_preview(&repo, &checkout, "sub/inner.txt").is_err());
        assert!(list_directory(&repo, &checkout, "sub").is_err());
    }

    #[test]
    fn a_root_ancestor_replaced_after_validation_is_refused() {
        let holder = tempdir().unwrap();
        fs::create_dir_all(holder.path().join("parent/checkout")).unwrap();
        let validated = holder
            .path()
            .join("parent/checkout")
            .canonicalize()
            .unwrap();
        let outside = tempdir().unwrap();
        fs::create_dir(outside.path().join("checkout")).unwrap();
        fs::rename(holder.path().join("parent"), holder.path().join("saved")).unwrap();
        symlink(outside.path(), holder.path().join("parent")).unwrap();
        assert!(open_root(&validated).is_err());
    }

    #[test]
    fn an_entry_deleted_after_readdir_is_skipped() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("vanishing"), "text").unwrap();
        let handle = open_root(&dir.path().canonicalize().unwrap()).unwrap();
        let mut stream = DirStream::open(&handle).unwrap();
        let name = loop {
            let item = stream.next().unwrap().unwrap();
            if item.to_bytes() == b"vanishing" {
                break item;
            }
        };
        fs::remove_file(dir.path().join("vanishing")).unwrap();
        assert_eq!(entry_kind(&handle, &name).unwrap(), None);
        let (repo, checkout) = checkout_of(&dir);
        assert!(list_directory(&repo, &checkout, "").unwrap().is_empty());
    }

    #[test]
    fn a_checkout_is_only_the_one_the_workspace_registered() {
        let dir = tempdir().unwrap();
        let elsewhere = tempdir().unwrap();
        fs::write(dir.path().join("hello.txt"), "hello").unwrap();
        fs::write(elsewhere.path().join("secret.txt"), "secret").unwrap();
        let repo = Repo::plain(dir.path(), "now").unwrap();
        let checkout = repo.checkouts[0].clone();

        let mut unknown = checkout.clone();
        unknown.id = "checkout:forged".into();
        assert!(list_directory(&repo, &unknown, "").is_err());

        // The registered id, someone else's path.
        let mut forged = checkout.clone();
        forged.path = elsewhere
            .path()
            .canonicalize()
            .unwrap()
            .display()
            .to_string();
        assert!(list_directory(&repo, &forged, "").is_err());
        assert!(read_preview(&repo, &forged, "secret.txt").is_err());

        // The same, registered in the snapshot as well: now the id does not describe
        // the path it carries.
        let mut renamed = forged.clone();
        renamed.canonical_path = renamed.path.clone();
        let mut snapshot = repo.clone();
        snapshot.checkouts.push(renamed.clone());
        assert!(list_directory(&snapshot, &renamed, "").is_err());

        // A registered checkout whose root is behind a link: the path no longer
        // canonicalizes to itself.
        let holder = tempdir().unwrap();
        symlink(dir.path(), holder.path().join("alias")).unwrap();
        let mut aliased = checkout.clone();
        aliased.path = holder.path().join("alias").display().to_string();
        aliased.canonical_path = aliased.path.clone();
        aliased.id = checkout_id_for_path(&aliased.canonical_path);
        let mut snapshot = repo.clone();
        snapshot.checkouts.push(aliased.clone());
        assert!(list_directory(&snapshot, &aliased, "").is_err());

        let mut missing = checkout.clone();
        missing.is_missing = true;
        assert!(list_directory(&repo, &missing, "").is_err());

        // The genuine checkout still works after all of that.
        assert_eq!(
            read_preview(&repo, &checkout, "hello.txt").unwrap().text,
            "hello"
        );
    }

    /// What may be handed to another program is decided before anything is
    /// spawned, and the answer is one path: the canonical one, still inside this
    /// checkout, or a refusal.
    #[test]
    fn an_external_path_is_the_canonical_one_inside_the_checkout() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("docs")).unwrap();
        fs::write(dir.path().join("docs/notes.txt"), "x").unwrap();
        let (repo, checkout) = checkout_of(&dir);
        let root = Path::new(&checkout.canonical_path).canonicalize().unwrap();

        assert_eq!(
            external_path(&repo, &checkout, "docs/notes.txt").expect("a file inside it"),
            root.join("docs/notes.txt")
        );
        // A folder is a thing to open too, and so is the checkout itself.
        assert_eq!(
            external_path(&repo, &checkout, "docs").expect("a folder inside it"),
            root.join("docs")
        );
        assert_eq!(
            external_path(&repo, &checkout, "").expect("the root itself"),
            root
        );

        // Every refusal the browser already makes, made here too: this is the
        // step that decides, so it cannot lean on the opener.
        assert!(
            external_path(&repo, &checkout, "../secrets").is_err(),
            "a path that climbs out is not handed over"
        );
        assert!(
            external_path(&repo, &checkout, "/etc/passwd").is_err(),
            "an absolute path is not a name inside this checkout"
        );
        assert!(
            external_path(&repo, &checkout, ".git/config").is_err(),
            "the object store is not browsable and is not openable either"
        );
        assert!(
            external_path(&repo, &checkout, "docs/missing.txt").is_err(),
            "a file that is not there is not handed over"
        );

        // A link is refused rather than resolved: following it is how a
        // checkout-relative path reaches outside the checkout without a `..`.
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "x").unwrap();
        symlink(outside.path(), dir.path().join("escape")).unwrap();
        assert!(
            external_path(&repo, &checkout, "escape/secret.txt").is_err(),
            "a link out of the checkout is not followed"
        );

        // And a checkout that is not the registered one is refused before any of
        // that is asked.
        let mut forged = checkout.clone();
        forged.path = outside.path().display().to_string();
        forged.canonical_path = forged.path.clone();
        forged.id = checkout_id_for_path(&forged.canonical_path);
        assert!(external_path(&repo, &forged, "secret.txt").is_err());
    }

    /// A FIFO is not a file to hand a desktop application: opening one would
    /// park forever, and a refusal is a better answer than a hung window.
    #[cfg(unix)]
    #[test]
    fn a_fifo_is_not_something_to_open() {
        let dir = tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        let made = unsafe {
            libc::mkfifo(
                std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes())
                    .expect("a path with no NUL")
                    .as_ptr(),
                0o600,
            )
        };
        assert_eq!(made, 0, "a FIFO to refuse");
        let (repo, checkout) = checkout_of(&dir);
        assert!(external_path(&repo, &checkout, "pipe").is_err());
    }

    /// A save replaces the bytes the reader was shown, and nothing else.
    ///
    /// The compare is the whole of what stops two writers from each saving over
    /// the other, so it is asked in both directions: the bytes that are there
    /// are the ones this save expected, and bytes that are not are refused
    /// rather than overwritten.
    #[test]
    fn a_write_replaces_the_bytes_it_expected_and_refuses_any_others() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("hello.txt"), "hello").unwrap();
        fs::write(dir.path().join("other.txt"), "other").unwrap();
        let (repo, checkout) = checkout_of(&dir);

        write_preview(&repo, &checkout, "hello.txt", "hello", "hello there").unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("hello.txt")).unwrap(),
            "hello there"
        );

        // Somebody else wrote between the read and the save.
        assert_eq!(
            write_preview(&repo, &checkout, "other.txt", "what was read", "mine")
                .unwrap_err()
                .to_string(),
            CHANGED_ON_DISK
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("other.txt")).unwrap(),
            "other",
            "the refused save left the file as it was"
        );
        // Even the empty expected value is a claim about the bytes.
        assert_eq!(
            write_preview(&repo, &checkout, "other.txt", "", "mine")
                .unwrap_err()
                .to_string(),
            CHANGED_ON_DISK
        );
        // And the file that was saved can be saved again from what it now holds.
        write_preview(&repo, &checkout, "hello.txt", "hello there", "again").unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("hello.txt")).unwrap(),
            "again"
        );
    }

    #[test]
    fn replacement_interleaved_before_publication_is_refused_by_write_preview() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        fs::write(&path, "expected").unwrap();
        let (repo, checkout) = checkout_of(&dir);
        let result = write_preview_with_hooks(
            &repo,
            &checkout,
            "hello.txt",
            "expected",
            "mine",
            || {
                let replacement = dir.path().join("replacement");
                fs::write(&replacement, "external replacement").unwrap();
                fs::rename(replacement, &path).unwrap();
            },
            |fd| unsafe { libc::fsync(fd) },
        );
        assert!(matches!(
            result,
            Err(WritePreviewError::BeforePublication(ref error)) if error == CHANGED_ON_DISK
        ));
        assert_eq!(fs::read_to_string(path).unwrap(), "external replacement");
    }

    #[test]
    fn directory_sync_failure_reports_visible_published_bytes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        fs::write(&path, "expected").unwrap();
        let (repo, checkout) = checkout_of(&dir);
        let result = write_preview_with_hooks(
            &repo,
            &checkout,
            "hello.txt",
            "expected",
            "published",
            || {},
            |_| -1,
        );
        assert!(matches!(
            result,
            Err(WritePreviewError::PublishedButNotDurable(_))
        ));
        assert_eq!(fs::read_to_string(path).unwrap(), "published");
    }

    /// Everything a read refuses, a write refuses too, because the boundary that
    /// decides what may be touched is the same walk and the same name rules.
    #[test]
    fn a_write_is_refused_wherever_a_read_is() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        fs::write(dir.path().join("hello.txt"), "hello").unwrap();
        fs::write(dir.path().join("binary.bin"), [0x00, 0x01]).unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/inner.txt"), "inner").unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/config"), "[core]").unwrap();
        symlink(
            outside.path().join("secret.txt"),
            dir.path().join("link.txt"),
        )
        .unwrap();
        symlink(outside.path(), dir.path().join("escape")).unwrap();
        let (repo, checkout) = checkout_of(&dir);

        for refused in [
            // A link, an ancestor that is one, and the store.
            "link.txt",
            "escape/secret.txt",
            ".git/config",
            // Names a checkout cannot answer for at all.
            "../escape",
            "/etc/passwd",
            "sub",
            "docs",
            "missing.txt",
            "",
        ] {
            assert!(
                write_preview(&repo, &checkout, refused, "", "mine").is_err(),
                "{refused} was written"
            );
        }
        assert_eq!(
            fs::read_to_string(outside.path().join("secret.txt")).unwrap(),
            "secret",
            "nothing outside the checkout was written"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join(".git/config")).unwrap(),
            "[core]"
        );

        // A file that is not text, a write that is not text, and a write past the
        // bound the file it replaces is read through.
        assert!(write_preview(&repo, &checkout, "binary.bin", "", "mine").is_err());
        assert!(write_preview(&repo, &checkout, "hello.txt", "hello", "a\0b").is_err());
        assert!(write_preview(
            &repo,
            &checkout,
            "hello.txt",
            "hello",
            &"a".repeat(MAX_PREVIEW_BYTES + 1)
        )
        .is_err());
        assert_eq!(
            fs::read_to_string(dir.path().join("hello.txt")).unwrap(),
            "hello",
            "no refusal left anything behind"
        );
    }

    /// A replacement is the same file as far as everything else is concerned: the
    /// mode it had is the mode it keeps, a file somebody made read-only is
    /// refused rather than quietly made writable, and nothing is left beside it.
    #[test]
    fn a_write_keeps_the_file_it_replaces() {
        let dir = tempdir().unwrap();
        let made = dir.path().join("script.sh");
        fs::write(&made, "echo old").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&made, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (repo, checkout) = checkout_of(&dir);

        write_preview(&repo, &checkout, "script.sh", "echo old", "echo new").unwrap();
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&made).unwrap().permissions().mode() & 0o7777,
            0o755,
            "the replacement carries the mode the file had"
        );
        assert_eq!(
            fs::read_to_string(&made).unwrap(),
            "echo new",
            "an executable is not written as one that cannot be run"
        );

        // Read-only is a decision somebody made outside this app: a rename into
        // the directory would need the directory's write access rather than the
        // file's, and the save would succeed and the bit would be gone.
        #[cfg(unix)]
        {
            fs::set_permissions(&made, std::fs::Permissions::from_mode(0o444)).unwrap();
            assert_eq!(
                write_preview(&repo, &checkout, "script.sh", "echo new", "echo again")
                    .unwrap_err()
                    .to_string(),
                "file is read-only"
            );
            assert_eq!(fs::read_to_string(&made).unwrap(), "echo new");
        }

        // The temporary file is gone either way: the directory holds the file and
        // nothing this app left behind.
        let left = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            left,
            ["script.sh"],
            "a save left something behind: {left:?}"
        );
    }

    #[test]
    fn displayed_review_folder_is_the_export_folder_of_the_registered_checkout() {
        let primary = tempdir().unwrap();
        let worktree = tempdir().unwrap();
        let (mut repo, mut checkout) = checkout_of(&primary);
        checkout.path = worktree
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        checkout.canonical_path = checkout.path.clone();
        checkout.id = checkout_id_for_path(&checkout.path);
        checkout.is_primary = false;
        repo.checkouts.push(checkout.clone());
        let folder = review_export_folder(&repo, &checkout, ReviewStorage::Workdir).unwrap();
        assert_eq!(folder, Path::new(&checkout.path).join(".muster/reviews"));
        assert!(!folder.exists(), "showing the folder must not create it");
        let exported = export_review_markdown(
            &repo,
            &checkout,
            ReviewStorage::Workdir,
            "2026-06-02-1234",
            "<!-- muster-review-export -->\nreview\n",
        )
        .unwrap();
        assert_eq!(exported.parent(), Some(folder.as_path()));
        assert!(!primary.path().join(".muster/reviews").exists());
        let home = PathBuf::from(std::env::var_os("HOME").unwrap())
            .canonicalize()
            .unwrap();
        assert_eq!(
            review_export_folder(&repo, &checkout, ReviewStorage::Default).unwrap(),
            home.join(".muster/tmp/reviews"),
        );
        let mut foreign = checkout.clone();
        foreign.path = primary.path().to_string_lossy().into_owned();
        assert!(review_export_folder(&repo, &foreign, ReviewStorage::Workdir).is_err());
    }

    #[test]
    fn review_exports_are_exclusive_and_stay_inside_the_checkout() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("2026-06-02-1234.md"),
            "owned by the project",
        )
        .unwrap();
        let (repo, checkout) = checkout_of(&dir);
        let markdown = "<!-- muster-review-export -->\n# Code Review 2026-06-02\n";

        let first = export_review_markdown(
            &repo,
            &checkout,
            ReviewStorage::Workdir,
            "2026-06-02-1234",
            markdown,
        )
        .unwrap();
        let second = export_review_markdown(
            &repo,
            &checkout,
            ReviewStorage::Workdir,
            "2026-06-02-1234",
            markdown,
        )
        .unwrap();

        let checkout_root = dir.path().canonicalize().unwrap();
        assert_eq!(
            first,
            checkout_root.join(".muster/reviews/2026-06-02-1234.md")
        );
        assert_eq!(
            second,
            checkout_root.join(".muster/reviews/2026-06-02-1234-2.md")
        );
        assert_eq!(fs::read_to_string(&first).unwrap(), markdown);
        assert_eq!(fs::read_to_string(&second).unwrap(), markdown);
        assert_eq!(
            fs::read_to_string(dir.path().join("2026-06-02-1234.md")).unwrap(),
            "owned by the project"
        );
        assert!(export_review_markdown(
            &repo,
            &checkout,
            ReviewStorage::Workdir,
            "../escape",
            markdown,
        )
        .is_err());
        assert!(export_review_markdown(
            &repo,
            &checkout,
            ReviewStorage::Workdir,
            "2026-06-02-1235",
            "not marked",
        )
        .is_err());
    }

    #[test]
    fn review_export_does_not_overwrite_an_unmarked_file_with_the_same_timestamp() {
        let dir = tempdir().unwrap();
        let review_dir = dir.path().join(".muster/reviews");
        fs::create_dir_all(&review_dir).unwrap();
        let existing = review_dir.join("2026-06-02-1234.md");
        fs::write(&existing, "project-owned content").unwrap();
        let (repo, checkout) = checkout_of(&dir);
        let markdown = "<!-- muster-review-export -->\nreview\n";

        let exported = export_review_markdown(
            &repo,
            &checkout,
            ReviewStorage::Workdir,
            "2026-06-02-1234",
            markdown,
        )
        .unwrap();

        assert!(exported.ends_with("2026-06-02-1234-2.md"));
        assert_eq!(
            fs::read_to_string(existing).unwrap(),
            "project-owned content"
        );
        assert_eq!(fs::read_to_string(exported).unwrap(), markdown);
    }

    #[test]
    fn clearing_reviews_removes_only_marked_exports_and_refuses_workdir() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_export = outside.path().join("outside.md");
        fs::write(&outside_export, "<!-- muster-review-export -->\noutside\n").unwrap();
        let review_dir = dir.path().join(".muster/tmp/reviews");
        fs::create_dir_all(&review_dir).unwrap();
        let link = review_dir.join("2026-06-02-1237.md");
        symlink(&outside_export, &link).unwrap();
        let marked = review_dir.join("2026-06-02-1234.md");
        let marked_suffix = review_dir.join("2026-06-02-1234-2.md");
        let unmarked = review_dir.join("2026-06-02-1235.md");
        let wrong_name = review_dir.join("notes.md");
        let wrong_extension = review_dir.join("2026-06-02-1236.txt");
        fs::write(&marked, "<!-- muster-review-export -->\nreview\n").unwrap();
        fs::write(&marked_suffix, "<!-- muster-review-export -->\nsecond\n").unwrap();
        fs::write(&unmarked, "project content\n").unwrap();
        fs::write(
            &wrong_name,
            "<!-- muster-review-export -->\nnot an export\n",
        )
        .unwrap();
        fs::write(
            &wrong_extension,
            "<!-- muster-review-export -->\nnot markdown\n",
        )
        .unwrap();
        let directory = open_directory(
            &open_root(&dir.path().canonicalize().unwrap()).unwrap(),
            Path::new(".muster/tmp/reviews"),
        )
        .unwrap();

        assert_eq!(clear_marked_review_exports(&directory).unwrap(), 2);
        assert!(!marked.exists());
        assert!(!marked_suffix.exists());
        assert_eq!(fs::read_to_string(unmarked).unwrap(), "project content\n");
        assert!(wrong_name.exists());
        assert!(wrong_extension.exists());
        assert!(link.exists());
        assert_eq!(
            fs::read_to_string(outside_export).unwrap(),
            "<!-- muster-review-export -->\noutside\n"
        );

        let workdir_reviews = dir.path().join(".muster/reviews");
        fs::create_dir_all(&workdir_reviews).unwrap();
        let workdir_export = workdir_reviews.join("2026-06-02-1234.md");
        fs::write(&workdir_export, "<!-- muster-review-export -->\nkeep\n").unwrap();
        let (repo, checkout) = checkout_of(&dir);
        assert_eq!(
            clear_review_exports(&repo, &checkout, ReviewStorage::Workdir).unwrap_err(),
            "review exports inside a checkout are not cleared here"
        );
        assert!(workdir_export.exists());
    }

    #[test]
    fn review_export_refuses_a_symlinked_checkout_folder() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        symlink(outside.path(), dir.path().join(".muster")).unwrap();
        let (repo, checkout) = checkout_of(&dir);
        let markdown = "<!-- muster-review-export -->\nreview\n";
        assert!(export_review_markdown(
            &repo,
            &checkout,
            ReviewStorage::Workdir,
            "2026-06-02-1234",
            markdown,
        )
        .is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
