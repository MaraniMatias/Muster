//! Safe, bounded, read-only access to files within a registered checkout.
//!
//! Four rules hold for everything here:
//!
//! * **Nothing is answered partially.** A directory with more than
//!   [`MAX_DIRECTORY_ENTRIES`] rows and a file larger than [`MAX_PREVIEW_BYTES`]
//!   are both errors, not a shorter list and a shorter text. A caller cannot tell
//!   a truncated answer from a complete one, so there is no truncation to offer.
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
    io::{self, Read},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
};

use crate::workspace::{checkout_id_for_path, repo_id_for_path, Checkout, Repo};

/// The largest file that can be previewed. Anything larger is refused outright.
pub const MAX_PREVIEW_BYTES: usize = 1024 * 1024;
/// The largest directory that can be listed. Anything larger is refused outright.
pub const MAX_DIRECTORY_ENTRIES: usize = 2000;

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
    /// Always false. Kept for callers that already read it; a preview this
    /// crate cannot return whole is an error, not a partial one.
    pub truncated: bool,
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

/// Read a whole text file, or refuse it.
///
/// A file over [`MAX_PREVIEW_BYTES`] is refused rather than cut: half a file and
/// the whole file are both "some text", and only one of them is what the user
/// thinks they are reading. So is a file with a NUL in it, one that is not UTF-8,
/// anything that is not a regular file, and any path that leaves the checkout.
pub fn read_preview(repo: &Repo, checkout: &Checkout, path: &str) -> Result<FilePreview, String> {
    let root = root(repo, checkout)?;
    let relative = relative(path)?;
    let name = relative
        .file_name()
        .ok_or("only regular files can be previewed")?;
    let root_handle = open_root(&root).map_err(|e| e.to_string())?;
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
        return Err("only regular files can be previewed".into());
    }
    let too_large = || "file is larger than the 1 MiB preview limit".to_owned();
    if opened.len() > MAX_PREVIEW_BYTES as u64 {
        return Err(too_large());
    }
    let mut bytes = Vec::with_capacity(MAX_PREVIEW_BYTES + 1);
    file.take((MAX_PREVIEW_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read file: {error}"))?;
    if bytes.len() > MAX_PREVIEW_BYTES {
        // It grew between the size and the read. Cutting here is the one thing a
        // caller cannot see, so it is refused like any other oversized file.
        return Err(too_large());
    }
    if bytes.contains(&0) {
        return Err("binary files cannot be previewed".into());
    }
    let text = String::from_utf8(bytes).map_err(|_| "file is not valid UTF-8".to_owned())?;
    Ok(FilePreview {
        path: path.into(),
        text,
        truncated: false,
    })
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
        os::unix::{fs::symlink, net::UnixListener},
    };
    use tempfile::{tempdir, TempDir};

    fn checkout_of(dir: &TempDir) -> (Repo, Checkout) {
        let repo = Repo::plain(dir.path(), "now").unwrap();
        let checkout = repo.checkouts[0].clone();
        (repo, checkout)
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
        assert!(!hello.truncated);
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
}
