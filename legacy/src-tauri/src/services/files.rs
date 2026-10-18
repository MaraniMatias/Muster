use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

use crate::{
    config::{AppSettings, ConfigFile, REVIEW_STORAGE_DEFAULT, REVIEW_STORAGE_WORKDIR},
    domain::review::MAX_ROUND_PROMPT_BYTES,
    domain::{
        files::{
            CheckoutImage, FileContent, FileEntry, FileEntryKind, FileProbe, FileTree,
            PrettierConfig, ReviewDocumentOrigin, ReviewExport, ReviewFolder, ReviewFolderCleared,
        },
        ipc::{IpcError, IpcErrorCode},
        workspace::{Checkout, Repo, RepoKind},
    },
    persistence::Database,
};

const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_MEDIA_HEADER_BYTES: u64 = 64 * 1024;
/// A Prettier config is a few hundred bytes. Anything past this is not one, and reading it would
/// mean handing the formatter a document it never asked for.
const MAX_CONFIG_BYTES: u64 = 256 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 2000;
/// Git's own bookkeeping, never shown in the tree. `check-ignore` reports it ignored on every
/// repository, which would otherwise put it in the list the moment ignores became visible.
const GIT_DIRECTORY: &str = ".git";

/// What a pinned review origin resolves against: the settings file location and its named storage mode.
///
/// The mode is captured per document; only the file location is used to find the app's home folder.
#[derive(Clone)]
pub struct ReviewPlaces {
    config_file: PathBuf,
    settings: AppSettings,
}

impl ReviewPlaces {
    pub fn new(config_file: PathBuf, settings: AppSettings) -> Self {
        Self {
            config_file,
            settings,
        }
    }

    /// Reads the settings file, which is the whole of what the other half of this is.
    pub fn load(config_file: &Path) -> Result<Self, IpcError> {
        Ok(Self::new(
            config_file.to_path_buf(),
            crate::config::load(config_file)?,
        ))
    }

    /// Checkout origins use no review settings; pinned review origins resolve their named place directly.
    pub fn load_for_origin(config_file: &Path, origin: &str) -> Result<Option<Self>, IpcError> {
        let storage = match origin {
            "checkout" => return Ok(None),
            "review-default" => REVIEW_STORAGE_DEFAULT,
            "review-workdir" => REVIEW_STORAGE_WORKDIR,
            _ => return Err(invalid_origin()),
        };
        let mut places = Self::new(config_file.to_path_buf(), AppSettings::default());
        places.set_storage(storage);
        Ok(Some(places))
    }

    /// Answers for a mode the settings file does not hold yet, for a row that is showing the
    /// folder a preference would write to before it is saved.
    pub fn set_storage(&mut self, storage: &str) {
        self.settings.reviews.storage = storage.into();
    }
}

/// Where an exported review goes when it is the app's own: `~/.muster/tmp/reviews`, under the
/// folder `config.yml` already lives in, so everything this app keeps in a home sits together.
const REVIEW_ROOT_FROM_HOME: &str = ".muster/tmp/reviews";
/// …and where it goes when it belongs to the code it is about: inside the checkout, under a folder
/// that is not the project's to name. The name is not a preference, because the only question a
/// person asks about it is which of the two places reviews go to.
const REVIEW_DIR_IN_CHECKOUT: &str = ".muster/reviews";
const EXPORTED_REVIEW_MARKER: &str = "<!-- muster-review-export -->\n";

fn checkout_root_for_review(database: &Database, checkout_id: &str) -> Result<PathBuf, IpcError> {
    let (repo, checkout) = registered_checkout(database, checkout_id)?;
    ensure_checkout_available(checkout.is_missing, &checkout.canonical_path)?;
    crate::services::checkout::resolve_checkout_path(&repo, checkout_id, Path::new("."))
}

fn workdir_review_paths(
    database: &Database,
    checkout_id: &str,
) -> Result<(PathBuf, PathBuf), IpcError> {
    let checkout_root = checkout_root_for_review(database, checkout_id)?;
    let review_root =
        contained_path_allow_missing(&checkout_root.join(REVIEW_DIR_IN_CHECKOUT), &checkout_root)?;
    Ok((checkout_root, review_root))
}

/// The folder exported reviews are written to. A workdir root may not exist yet, but every
/// existing ancestor must resolve inside the available checkout.
pub fn review_root_for(
    database: &Database,
    checkout_id: &str,
    places: &ReviewPlaces,
) -> Result<PathBuf, IpcError> {
    let config_file = ConfigFile(places.config_file.clone());
    match places.settings.reviews.storage.as_str() {
        REVIEW_STORAGE_WORKDIR => workdir_review_paths(database, checkout_id).map(|(_, root)| root),
        REVIEW_STORAGE_DEFAULT => {
            let home = crate::config::home_of(&config_file).ok_or_else(|| {
                IpcError::new(
                    IpcErrorCode::InvalidPath,
                    "the settings path has no home directory above it",
                )
            })?;
            Ok(home.join(REVIEW_ROOT_FROM_HOME))
        }
        _ => Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "unknown review storage mode",
        )),
    }
}

/// The review folder, and what it holds, for the settings dialog.
///
/// The path is the one the exports would be written to rather than one that had to exist: the
/// dialog asks where reviews go before any of them exist, and a folder that has not been written
/// yet is still the answer.
pub fn review_folder(
    database: &Database,
    checkout_id: &str,
    places: &ReviewPlaces,
) -> Result<ReviewFolder, IpcError> {
    let root = review_root_for(database, checkout_id, places)?;
    let (files, bytes) = review_folder_usage(&root)?;
    Ok(ReviewFolder {
        path: root.to_string_lossy().into_owned(),
        storage: places.settings.reviews.storage.clone(),
        files,
        bytes,
    })
}

/// How much this app's exports hold, using the same ownership test as clear. Exports are flat; a
/// missing folder holds nothing, which is the state it starts in rather than a failure.
fn review_folder_usage(root: &Path) -> Result<(usize, u64), IpcError> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(error) => {
            return Err(filesystem_error(
                "could not inspect review export folder",
                error,
            ))
        }
    };
    let mut files = 0;
    let mut bytes = 0;
    for entry in entries.flatten() {
        if let Some(metadata) = owned_review_export(&entry) {
            files += 1;
            bytes += metadata.len();
        }
    }
    Ok((files, bytes))
}

/// Deletes files bearing this app's export marker, and nothing else. `workdir` is refused here
/// rather than merely hidden in the dialog: those notes are the project's, not a cache of this app's.
pub fn clear_review_folder(
    database: &Database,
    checkout_id: &str,
    places: &ReviewPlaces,
) -> Result<ReviewFolderCleared, IpcError> {
    if places.settings.reviews.storage != REVIEW_STORAGE_DEFAULT {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "review notes inside a working directory are kept there, not cleared from here",
        ));
    }
    let root = review_root_for(database, checkout_id, places)?;
    let mut cleared = ReviewFolderCleared::default();
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(cleared),
        Err(error) => {
            return Err(filesystem_error(
                "could not inspect review export folder",
                error,
            ))
        }
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let Some(metadata) = owned_review_export(&entry) else {
            continue;
        };
        fs::remove_file(entry.path())
            .map_err(|error| filesystem_error("could not delete review export", error))?;
        cleared.files += 1;
        cleared.bytes += metadata.len();
    }
    Ok(cleared)
}

/// Whether a directory entry is one of this app's exports. A timestamp-shaped name alone is not
/// ownership; the marker is written by the exporter and checked without following symlinks.
fn owned_review_export(entry: &fs::DirEntry) -> Option<fs::Metadata> {
    let name = entry.file_name();
    if !is_exported_review(name.to_str()?) {
        return None;
    }
    let metadata = fs::symlink_metadata(entry.path()).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let mut file = File::open(entry.path()).ok()?;
    let mut marker = [0; EXPORTED_REVIEW_MARKER.len()];
    file.read_exact(&mut marker).ok()?;
    (marker.as_slice() == EXPORTED_REVIEW_MARKER.as_bytes()).then_some(metadata)
}

/// Whether a name is one `export_review_markdown` writes: the export timestamp, and the `-2` a
/// second export in the same minute gets.
fn is_exported_review(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".md") else {
        return false;
    };
    let Some(date) = stem.get(..10) else {
        return false;
    };
    let Some(clock) = stem.get(10..).and_then(|rest| rest.strip_prefix('-')) else {
        return false;
    };
    let (clock, suffix) = match clock.split_once('-') {
        Some((clock, suffix)) => (clock, Some(suffix)),
        None => (clock, None),
    };
    if validate_review_timestamp(date, &format!("{date}-{clock}")).is_err() {
        return false;
    }
    match suffix {
        Some(suffix) => !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()),
        None => true,
    }
}

pub fn list(
    database: &Database,
    checkout_id: &str,
    relative_path: &str,
) -> Result<FileTree, IpcError> {
    let (repo, checkout) = registered_checkout(database, checkout_id)?;
    ensure_checkout_available(checkout.is_missing, &checkout.canonical_path)?;
    let root = Path::new(&checkout.canonical_path);
    let requested = parse_relative_path(relative_path)?;
    let directory =
        crate::services::checkout::resolve_checkout_path(&repo, checkout_id, &requested)?;
    let metadata = fs::metadata(&directory)
        .map_err(|error| filesystem_error("could not inspect folder", error))?;
    if !metadata.is_dir() {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "requested path is not a directory",
        ));
    }

    let mut collected = Vec::new();
    let mut truncated = false;
    for item in fs::read_dir(&directory)
        .map_err(|error| filesystem_error("could not list folder", error))?
    {
        let item = item.map_err(|error| filesystem_error("could not read folder entry", error))?;
        let name = match item.file_name().into_string() {
            Ok(name) => name,
            Err(_) => continue,
        };
        if name == ".git" {
            continue;
        }
        let file_type = item
            .file_type()
            .map_err(|error| filesystem_error("could not inspect folder entry", error))?;
        let kind = if file_type.is_symlink() {
            FileEntryKind::Symlink
        } else if file_type.is_dir() {
            FileEntryKind::Directory
        } else if file_type.is_file() {
            FileEntryKind::File
        } else {
            continue;
        };
        let entry_path = item.path();
        let relative = entry_path.strip_prefix(root).map_err(|_| {
            IpcError::new(
                IpcErrorCode::PathOutsideCheckout,
                "entry is outside checkout",
            )
        })?;
        let Some(path) = relative.to_str() else {
            continue;
        };
        collected.push(FileEntry {
            name,
            path: path.replace(std::path::MAIN_SEPARATOR, "/"),
            kind,
            ignored: false,
        });
        if collected.len() > MAX_DIRECTORY_ENTRIES {
            truncated = true;
            collected.pop();
            break;
        }
    }

    if repo.kind == RepoKind::Git {
        // Gitignored files are listed and marked, not dropped: the tree shows them a step
        // quieter, so a build directory reads as present but uninteresting. `.git` is the
        // exception: `check-ignore` always reports it ignored because it is Git's own
        // bookkeeping, not something the project ignores, so it stays out of the tree
        // entirely, matched by name.
        let ignored = ignored_paths(root, &collected)?;
        for entry in &mut collected {
            entry.ignored = ignored.contains(entry.path.as_str());
        }
        collected.retain(|entry| entry.name != GIT_DIRECTORY);
    }
    collected.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(FileTree {
        entries: collected,
        truncated,
    })
}

/**
 * The bytes of one text file, under the limits the preview draws at: at most a mebibyte, no NUL,
 * and valid UTF-8. What this refuses is what the preview cannot show, which is the whole
 * definition of a file this app opens as text.
 */
fn read_preview_text(path: &Path) -> Result<String, IpcError> {
    let metadata =
        fs::metadata(path).map_err(|error| filesystem_error("could not inspect file", error))?;
    if !metadata.is_file() {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "selected path is not a file",
        ));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(too_large());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)
        .and_then(|file| file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| filesystem_error("could not read file", error))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(too_large());
    }
    if bytes.contains(&0) {
        return Err(binary_file());
    }
    String::from_utf8(bytes).map_err(|_| binary_file())
}

pub fn read(
    database: &Database,
    checkout_id: &str,
    origin: &str,
    relative_path: &Path,
    places: Option<&ReviewPlaces>,
) -> Result<FileContent, IpcError> {
    let (repo, checkout) = registered_checkout(database, checkout_id)?;
    let relative_path = relative_path
        .to_str()
        .ok_or_else(|| IpcError::new(IpcErrorCode::InvalidPath, "file path is not valid UTF-8"))?;
    let (relative_path, path) = match origin {
        "checkout" => {
            ensure_checkout_available(checkout.is_missing, &checkout.canonical_path)?;
            let relative_path = parse_relative_path(relative_path)?;
            let path = crate::services::checkout::resolve_checkout_path(
                &repo,
                checkout_id,
                &relative_path,
            )?;
            (relative_path, path)
        }
        "review-default" | "review-workdir" => {
            let relative_path = parse_review_relative_path(relative_path)?;
            let places = places.ok_or_else(missing_review_places)?;
            let root = review_root_for(database, checkout_id, places)?;
            let path = resolve_review_path(&root, &relative_path)?;
            (relative_path, path)
        }
        _ => return Err(invalid_origin()),
    };
    let content = read_preview_text(&path)?;
    let path = relative_path
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    Ok(FileContent { path, content })
}

/// Whether a path a terminal printed names a file the preview can open, and the checkout-relative
/// path to open it as.
///
/// This is `read` without the content, and it exists because the terminal asks the question on
/// hover: the answer has to be cheap and it has to agree with the reader, or a link would
/// underline itself for a file that then refuses to open. So it is the same limits, the same
/// containment, and text checks. Media classification reads at most 64 KiB, never the payload.
///
/// `working_directory` is where the terminal's shell is, and it is the whole difference between an
/// `ls` whose entries open and one that does nothing: a listing prints bare names, and a bare name
/// is relative to the directory the command ran in rather than to the root of the checkout. It is
/// only ever a base for a relative name — an absolute spelling is already what it says it is — and
/// it is never what decides containment, which is asked of the checkout below.
pub fn probe(
    database: &Database,
    checkout_id: &str,
    path: &str,
    working_directory: Option<&str>,
) -> Result<Option<FileProbe>, IpcError> {
    let Some(requested) = printed_path(path) else {
        return Ok(None);
    };
    // A terminal starts in its checkout and moves when its shell is told to, so the shell's own
    // directory is the base a relative name is read against. With no directory to read, the root
    // of the checkout is the only base there is, and it is a thing only the database can say —
    // which is why this is the one branch that asks before it looks.
    let base = match absolute_directory(working_directory) {
        Some(base) => base,
        None => {
            let (_, checkout) = registered_checkout(database, checkout_id)?;
            if checkout.is_missing {
                return Ok(None);
            }
            PathBuf::from(checkout.canonical_path)
        }
    };
    // The name as printed is what the extension is read from, and an absolute spelling is already
    // the whole of what it says, so the base only ever joins onto a relative one.
    let target = if requested.is_absolute() {
        requested.clone()
    } else {
        base.join(&requested)
    };
    // Canonicalization cheaply rejects absent names before the database lookup. Do not inspect file
    // contents until the registered checkout boundary has been established below.
    let Ok(target) = fs::canonicalize(&target) else {
        return Ok(None);
    };
    let (_, checkout) = registered_checkout(database, checkout_id)?;
    if checkout.is_missing {
        return Ok(None);
    }
    let Some(relative) = relative_to_checkout(&checkout.canonical_path, &target) else {
        return Ok(None);
    };
    // `..` cannot survive the canonicalization above, but `.git` can, and it is a directory this
    // tree never lists and so never a file it may hand to the preview.
    let relative_path = match parse_relative_path(&relative) {
        Ok(relative_path) => relative_path,
        Err(_) => return Ok(None),
    };
    if !preview_can_draw(&target, &requested) {
        return Ok(None);
    }
    Ok(Some(FileProbe {
        path: relative_path
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/"),
    }))
}

/// What a terminal printed, or `None` for the two things that name no file: an empty run of
/// characters, and one carrying a NUL, which is not a path any filesystem can answer for.
fn printed_path(path: &str) -> Option<PathBuf> {
    let path = path.trim();
    if path.is_empty() || path.contains('\0') {
        return None;
    }
    Some(PathBuf::from(path))
}

/// The directory a relative name is read against, when the terminal knows one.
///
/// A relative answer is refused rather than joined onto anything: resolving it here would resolve
/// it against whichever directory this process is in, which has nothing to do with the terminal
/// that printed the path.
fn absolute_directory(working_directory: Option<&str>) -> Option<PathBuf> {
    let directory = working_directory
        .map(str::trim)
        .filter(|directory| !directory.is_empty() && !directory.contains('\0'))?;
    let directory = Path::new(directory);
    directory.is_absolute().then(|| directory.to_path_buf())
}

/// The checkout-relative spelling of a file inside it, or `None` for one that is not.
///
/// `target` has to be the canonical file and the root is canonicalized beside it, because
/// containment is asked about the real thing and not about the spelling: a checkout reached through
/// a symlink, and a path printed through the same one, are one directory and have to be recognized
/// as it. A file that cannot be canonicalized does not exist, so there is nothing to place.
fn relative_to_checkout(root: &str, target: &Path) -> Option<String> {
    let root = fs::canonicalize(root).ok()?;
    Some(
        target
            .strip_prefix(root)
            .ok()?
            .to_string_lossy()
            .into_owned(),
    )
}

/// Whether the preview can draw this file, which is what a probe confirms and what `read` then
/// does. The limits are the reader's own, so a link can never underline itself for a file that
/// would refuse to open the moment it was clicked.
///
/// `spelling` is the name the line printed rather than the file's own, because the extension is
/// the only thing either of them is read for and both spellings of one file end in the same one.
fn preview_can_draw(target: &Path, spelling: &Path) -> bool {
    if media_limit(spelling).is_some() {
        return read_media_file(target, spelling, MAX_MEDIA_HEADER_BYTES, &mut Vec::new()).is_ok();
    }
    let Ok(text) = read_preview_text(target) else {
        return false;
    };
    extension(spelling) != "svg" || validate_svg(&text).is_ok()
}

/// Hands a file or a folder of this checkout to the application this machine has for it.
///
/// The tree spells a path relative to its checkout, so this is the same chain `probe` walks and in
/// the same order: the checkout has to be registered and present, the name has to be relative and
/// inside it, and what comes out the other side has to still be inside it once every symlink on
/// the way has been resolved. Only then does it reach the opener, which is handed a path and
/// decides nothing about what may be named.
///
/// A refusal here is not swallowed: the person who ctrl-clicked a row is told that the file did not
/// open, because a row that stayed where it was looks exactly like one that opened something.
pub fn open_externally(database: &Database, checkout_id: &str, path: &str) -> Result<(), IpcError> {
    let (repo, checkout) = registered_checkout(database, checkout_id)?;
    ensure_checkout_available(checkout.is_missing, &checkout.canonical_path)?;
    // The same refusals a preview read answers with, and for the same reasons: `..` and an
    // absolute spelling are not a name inside a checkout, and `.git` is not a file this tree lists
    // and therefore not one it may hand to another application either.
    let relative_path = parse_relative_path(path)?;
    let resolved =
        crate::services::checkout::resolve_checkout_path(&repo, checkout_id, &relative_path)?;
    crate::services::opener::open_path(&resolved)
        .map_err(|error| IpcError::new(IpcErrorCode::OperationFailed, error))
}

/// Returns a per-path lock shared by writes in this process. Dead entries are pruned on lookup.
/// This is not a cross-process compare-and-swap: another process can still change the file between
/// the content check and replacement.
fn file_write_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
    lock
}

pub fn write(
    database: &Database,
    checkout_id: &str,
    origin: &str,
    relative_path: &Path,
    content: &str,
    expected_content: &str,
    places: Option<&ReviewPlaces>,
) -> Result<(), IpcError> {
    let (repo, checkout) = registered_checkout(database, checkout_id)?;
    if content.len() as u64 > MAX_FILE_BYTES {
        return Err(too_large());
    }
    if content.contains('\0') {
        return Err(binary_file());
    }
    let relative_path = relative_path
        .to_str()
        .ok_or_else(|| IpcError::new(IpcErrorCode::InvalidPath, "file path is not valid UTF-8"))?;
    let path = match origin {
        "checkout" => {
            ensure_checkout_available(checkout.is_missing, &checkout.canonical_path)?;
            let relative_path = parse_relative_path(relative_path)?;
            crate::services::checkout::resolve_checkout_path(&repo, checkout_id, &relative_path)?
        }
        "review-default" | "review-workdir" => {
            let relative_path = parse_review_relative_path(relative_path)?;
            let places = places.ok_or_else(missing_review_places)?;
            let root = review_root_for(database, checkout_id, places)?;
            resolve_review_path(&root, &relative_path)?
        }
        _ => return Err(invalid_origin()),
    };
    let file_lock = file_write_lock(&path);
    let _guard = file_lock.lock().unwrap_or_else(|error| error.into_inner());

    // Validate current contents and source permissions while holding the same per-file lock as
    // replacement, so concurrent in-process writes cannot both accept the same expected value.
    let metadata =
        fs::metadata(&path).map_err(|error| filesystem_error("could not inspect file", error))?;
    if !metadata.is_file() {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "selected path is not a file",
        ));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(too_large());
    }
    // A read-only file is a decision someone made outside this app, and the temporary-file
    // replacement would quietly undo it: `rename` needs write access to the *directory*, not to
    // the file, so the save would succeed and the bit would be gone. Refuse instead.
    let permissions = metadata.permissions();
    if permissions.readonly() {
        return Err(IpcError::new(
            IpcErrorCode::PermissionDenied,
            "file is read-only",
        ));
    }

    let current =
        fs::read(&path).map_err(|error| filesystem_error("could not read file", error))?;
    if current.len() as u64 > MAX_FILE_BYTES {
        return Err(too_large());
    }
    if current.contains(&0) {
        return Err(binary_file());
    }
    let current = String::from_utf8(current).map_err(|_| binary_file())?;
    if current != expected_content {
        return Err(IpcError::new(
            IpcErrorCode::FileChanged,
            "file changed on disk; reload it before saving",
        ));
    }

    atomic_write(&path, content.as_bytes(), permissions)
}

/// The Prettier options that govern a file, found by walking up from the file's own
/// directory to the root of the checkout (or of the review folder) and stopping at the first
/// config that holds any. `None` is the answer for a checkout that configures nothing, and not a
/// failure: Prettier has defaults, and a file with no config is formatted with them.
///
/// Only the spellings a document can be read as are looked for. `.prettierrc.js`,
/// `prettier.config.mjs` and the rest are JavaScript, and there is no Node here to evaluate one.
/// A project that keeps its options in a script is formatted with the defaults rather than not
/// formatted at all, which is the only honest answer this process can give.
pub fn read_prettier_config(
    database: &Database,
    checkout_id: &str,
    origin: &str,
    relative_path: &str,
    places: Option<&ReviewPlaces>,
) -> Result<Option<PrettierConfig>, IpcError> {
    let (_repo, checkout) = registered_checkout(database, checkout_id)?;
    let (root, relative) = match origin {
        "checkout" => {
            ensure_checkout_available(checkout.is_missing, &checkout.canonical_path)?;
            (
                canonical_root(Path::new(&checkout.canonical_path))?,
                parse_relative_path(relative_path)?,
            )
        }
        "review-default" | "review-workdir" => {
            let places = places.ok_or_else(missing_review_places)?;
            (
                canonical_root(&review_root_for(database, checkout_id, places)?)?,
                parse_review_relative_path(relative_path)?,
            )
        }
        _ => return Err(invalid_origin()),
    };
    // The file's own directory first and the root last, so a config next to the file wins over the
    // one in the project, exactly as Prettier resolves it. `parent()` of a bare name is empty and
    // empty is what the root is, so the walk visits every directory once and stops there.
    let mut directory = relative.parent().map(Path::to_path_buf).unwrap_or_default();
    loop {
        let candidate = root.join(&directory);
        if let Some(options) = prettier_options_in(&candidate, &root)? {
            return Ok(Some(PrettierConfig {
                options,
                path: relative
                    .strip_prefix(&directory)
                    .unwrap_or(&relative)
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/"),
            }));
        }
        if directory.as_os_str().is_empty() {
            return Ok(None);
        }
        directory = directory
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
    }
}

/// Prettier's own search order within one directory, minus every spelling that would need a
/// JavaScript runtime. `package.json` is in it because a project may keep its options under a
/// `prettier` key, and one without that key is simply not a config.
const PRETTIER_CONFIG_FILES: &[(&str, ConfigSyntax)] = &[
    ("package.json", ConfigSyntax::Json),
    // Prettier reads `.prettierrc` as JSON first and falls back to YAML, and so does this.
    (".prettierrc", ConfigSyntax::JsonOrYaml),
    (".prettierrc.json", ConfigSyntax::Json),
    (".prettierrc.yml", ConfigSyntax::Yaml),
    (".prettierrc.yaml", ConfigSyntax::Yaml),
    ("prettier.config.json", ConfigSyntax::Json),
    ("prettier.config.yaml", ConfigSyntax::Yaml),
    ("prettier.config.yml", ConfigSyntax::Yaml),
];

#[derive(Clone, Copy)]
enum ConfigSyntax {
    Json,
    Yaml,
    /// `.prettierrc` and nothing else: the name carries no extension, so the syntax is whatever
    /// parses. JSON first because it is the stricter of the two and the one the CLI tries.
    JsonOrYaml,
}

/// A config's text as the options it holds.
///
/// Both spellings land on one error type because they do not have the same one, and the message is
/// the only part of either that ever reaches the caller. `JsonOrYaml` reports whichever failure
/// happened last, because a file that is neither was not going to be either of them.
fn parse_config(text: &str, syntax: ConfigSyntax) -> Result<serde_json::Value, String> {
    let as_json = || serde_json::from_str::<serde_json::Value>(text).map_err(|e| e.to_string());
    let as_yaml = || serde_yaml::from_str::<serde_json::Value>(text).map_err(|e| e.to_string());
    match syntax {
        ConfigSyntax::Json => as_json(),
        ConfigSyntax::Yaml => as_yaml(),
        ConfigSyntax::JsonOrYaml => {
            as_json().or_else(|json| as_yaml().map_err(|yaml| format!("{json}; as YAML: {yaml}")))
        }
    }
}

/// The root both the walk and every read are measured against, canonicalized once.
///
/// Containment is asked about the real directory and not about the spelling: a checkout reached
/// through a symlink and a file named inside it are the same place, and a root left uncanonicalized
/// would refuse its own files while accepting ones a symlink points at.
fn canonical_root(root: &Path) -> Result<PathBuf, IpcError> {
    fs::canonicalize(root).map_err(|error| {
        filesystem_error("could not resolve the root of the folder being read", error)
    })
}

/// Canonicalizes the nearest existing ancestor and appends missing components. Every existing
/// ancestor must remain inside the checkout; the final review directory may not exist yet.
fn contained_path_allow_missing(candidate: &Path, boundary: &Path) -> Result<PathBuf, IpcError> {
    let mut ancestor = candidate;
    let mut missing = Vec::new();
    loop {
        match ancestor.canonicalize() {
            Ok(resolved) => {
                if !resolved.starts_with(boundary) {
                    return Err(IpcError::new(
                        IpcErrorCode::PathOutsideCheckout,
                        "review folder resolves outside the checkout",
                    ));
                }
                return Ok(missing
                    .iter()
                    .rev()
                    .fold(resolved, |path, component| path.join(component)));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = ancestor.file_name() else {
                    return Err(filesystem_error(
                        "could not resolve review export folder",
                        error,
                    ));
                };
                missing.push(name.to_os_string());
                let Some(parent) = ancestor.parent() else {
                    return Err(filesystem_error(
                        "could not resolve review export folder",
                        error,
                    ));
                };
                ancestor = parent;
            }
            Err(error) => {
                return Err(filesystem_error(
                    "could not resolve review export folder",
                    error,
                ));
            }
        }
    }
}

/// The options in one directory, or `None` when it holds no config that counts.
///
/// Every path here is canonicalized and checked against `root` before it is opened. A directory
/// reached through a symlink and a config file that is itself one are both ways out of the folder
/// the reader asked about, and a name is not a containment check: `is_file` follows links.
fn prettier_options_in(
    directory: &Path,
    root: &Path,
) -> Result<Option<serde_json::Value>, IpcError> {
    for (name, syntax) in PRETTIER_CONFIG_FILES {
        let Some(path) = contained_path(&directory.join(name), root, name)? else {
            continue;
        };
        if !path.is_file() {
            continue;
        }
        let text = read_config_text(&path)?;
        let parsed = parse_config(&text, *syntax);
        if *name == "package.json" {
            // A package.json that will not parse, or that carries no `prettier` key, says
            // nothing about Prettier. It is not this function's error to report, and the walk
            // goes on to the next spelling in the same directory.
            let Ok(value) = parsed else { continue };
            let Some(options) = value.get("prettier") else {
                continue;
            };
            return checked_options(options, name);
        }
        return checked_options(
            &parsed.map_err(|error| unparseable_config(name, error))?,
            name,
        );
    }
    Ok(None)
}

/// The real path behind a candidate, once it is known to be inside `root`.
///
/// A candidate that does not exist is not an error: most of these names are absent in most
/// directories, and the walk is looking for the one that is there. Anything else that stops
/// `canonicalize` from answering is an error and not an absence, because a config this process
/// cannot open is one the reader believes it is formatting with and is not. `canonicalize` is what
/// makes the check safe: it resolves every link on the way, so a symlink out of the folder shows up
/// as a path that no longer starts with where it began.
fn contained_path(candidate: &Path, root: &Path, name: &str) -> Result<Option<PathBuf>, IpcError> {
    let resolved = match candidate.canonicalize() {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(IpcError::new(
                IpcErrorCode::OperationFailed,
                format!("{name} could not be resolved: {error}"),
            ));
        }
    };
    if !resolved.starts_with(root) {
        return Err(IpcError::new(
            IpcErrorCode::PathOutsideCheckout,
            format!("{name} resolves outside the folder being read"),
        ));
    }
    Ok(Some(resolved))
}

/// The options a config holds, with the two keys that are not the caller's to set.
///
/// `parser` is chosen from the file's own language, and `plugins` names modules that resolve
/// against this process' filesystem rather than the bundles the reader loads. A config carrying
/// either would fail every format instead of describing one, so they are dropped here rather than
/// left to fail once per click.
fn checked_options(
    value: &serde_json::Value,
    name: &str,
) -> Result<Option<serde_json::Value>, IpcError> {
    let Some(options) = value.as_object() else {
        return Err(IpcError::new(
            IpcErrorCode::OperationFailed,
            format!("{name} must hold a mapping of Prettier options"),
        ));
    };
    let mut options = options.clone();
    options.remove("parser");
    options.remove("plugins");
    Ok(Some(serde_json::Value::Object(options)))
}

fn read_config_text(path: &Path) -> Result<String, IpcError> {
    let metadata =
        fs::metadata(path).map_err(|error| filesystem_error("could not inspect config", error))?;
    // The read is bounded by `take` and the allocation by `min`, so a config that lies about its
    // size on disk costs the limit and not whatever it claims.
    let mut bytes = Vec::with_capacity(metadata.len().min(MAX_CONFIG_BYTES) as usize);
    File::open(path)
        .and_then(|file| file.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| filesystem_error("could not read config", error))?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(IpcError::new(
            IpcErrorCode::FileTooLarge,
            format!(
                "config file exceeds the {} KiB limit",
                MAX_CONFIG_BYTES / 1024
            ),
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        IpcError::new(
            IpcErrorCode::OperationFailed,
            "config file is not valid UTF-8",
        )
    })
}

fn unparseable_config(name: &str, error: impl std::fmt::Display) -> IpcError {
    IpcError::new(
        IpcErrorCode::OperationFailed,
        format!("{name} is not a readable Prettier config: {error}"),
    )
}

pub fn export_review_markdown(
    review_root: &Path,
    date: &str,
    timestamp: &str,
    markdown: &str,
) -> Result<String, IpcError> {
    export_review_markdown_inner(review_root, date, timestamp, markdown, None)
}

pub fn export_review_for_checkout(
    database: &Database,
    checkout_id: &str,
    places: &ReviewPlaces,
    date: &str,
    timestamp: &str,
    markdown: &str,
) -> Result<ReviewExport, IpcError> {
    let (boundary, root, origin) = match places.settings.reviews.storage.as_str() {
        REVIEW_STORAGE_WORKDIR => {
            let (boundary, root) = workdir_review_paths(database, checkout_id)?;
            (Some(boundary), root, ReviewDocumentOrigin::ReviewWorkdir)
        }
        REVIEW_STORAGE_DEFAULT => (
            None,
            review_root_for(database, checkout_id, places)?,
            ReviewDocumentOrigin::ReviewDefault,
        ),
        _ => {
            return Err(IpcError::new(
                IpcErrorCode::InvalidPath,
                "unknown review storage mode",
            ))
        }
    };
    let path = export_review_markdown_inner(&root, date, timestamp, markdown, boundary.as_deref())?;
    Ok(ReviewExport { path, origin })
}

fn export_review_markdown_inner(
    review_root: &Path,
    date: &str,
    timestamp: &str,
    markdown: &str,
    boundary: Option<&Path>,
) -> Result<String, IpcError> {
    validate_review_timestamp(date, timestamp)?;
    if markdown.len() > MAX_ROUND_PROMPT_BYTES {
        return Err(too_large());
    }
    if markdown.contains('\0') {
        return Err(binary_file());
    }

    let review_root = match boundary {
        Some(boundary) => contained_path_allow_missing(review_root, boundary)?,
        None => review_root.to_path_buf(),
    };
    fs::create_dir_all(&review_root)
        .map_err(|error| filesystem_error("could not create review export folder", error))?;
    let root = fs::canonicalize(&review_root)
        .map_err(|error| filesystem_error("could not resolve review export folder", error))?;
    if boundary.is_some_and(|boundary| !root.starts_with(boundary)) {
        return Err(IpcError::new(
            IpcErrorCode::PathOutsideCheckout,
            "review folder resolves outside the checkout",
        ));
    }
    let stem = timestamp.to_string();
    for suffix in 1usize.. {
        let filename = if suffix == 1 {
            format!("{stem}.md")
        } else {
            format!("{stem}-{suffix}.md")
        };
        let path = root.join(&filename);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let result = file
                    .write_all(EXPORTED_REVIEW_MARKER.as_bytes())
                    .and_then(|()| file.write_all(markdown.as_bytes()));
                if let Err(error) = result {
                    let _ = fs::remove_file(&path);
                    return Err(filesystem_error("could not write review export", error));
                }
                return Ok(filename);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(filesystem_error("could not create review export", error)),
        }
    }
    unreachable!("the filename suffix range is unbounded")
}

fn validate_review_timestamp(date: &str, timestamp: &str) -> Result<(), IpcError> {
    let valid_date = date.len() == 10
        && date.as_bytes()[4] == b'-'
        && date.as_bytes()[7] == b'-'
        && date
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit());
    let valid_timestamp = timestamp.len() == 15
        && timestamp.starts_with(date)
        && timestamp.as_bytes()[10] == b'-'
        && timestamp
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7 | 10) || byte.is_ascii_digit());
    if !valid_date || !valid_timestamp {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "review date and timestamp must use YYYY-MM-DD and YYYY-MM-DD-HHMM",
        ));
    }
    let year = date[0..4].parse::<u32>().unwrap();
    let month = date[5..7].parse::<u32>().unwrap();
    let day = date[8..10].parse::<u32>().unwrap();
    let hour = timestamp[11..13].parse::<u32>().unwrap();
    let minute = timestamp[13..15].parse::<u32>().unwrap();
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    if day == 0 || day > days_in_month || hour > 23 || minute > 59 {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "review date or local time is out of range",
        ));
    }
    Ok(())
}

/// Writes through a sibling temporary file so readers never observe a partial UTF-8 document.
///
/// The replacement arrives carrying the mode of the file it replaces, because a temporary file is
/// created with the default one and `rename` does not reconcile it: without this a saved script
/// loses its executable bit and a private file gains a group-read it never had. On Unix, syncing
/// the parent directory after the rename requests persistence of the directory entry as well as
/// the already-synced file contents; other platforms make no directory-sync guarantee here.
pub(crate) fn atomic_write(
    path: &Path,
    content: &[u8],
    permissions: fs::Permissions,
) -> Result<(), IpcError> {
    let parent = path.parent().ok_or_else(|| {
        IpcError::new(
            IpcErrorCode::InvalidPath,
            "selected file has no parent directory",
        )
    })?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            IpcError::new(
                IpcErrorCode::InvalidPath,
                "selected file name is not valid UTF-8",
            )
        })?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temporary = parent.join(format!(".{name}.muster-{nonce}-{}", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| filesystem_error("could not create temporary file", error))?;
        file.write_all(content)
            .and_then(|_| file.sync_all())
            .map_err(|error| filesystem_error("could not write file", error))?;
        drop(file);
        fs::set_permissions(&temporary, permissions)
            .map_err(|error| filesystem_error("could not apply file permissions", error))?;
        fs::rename(&temporary, path)
            .map_err(|error| filesystem_error("could not replace file", error))?;
        #[cfg(unix)]
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                filesystem_error(
                    "file was replaced, but syncing its parent directory failed; persistence of the rename is uncertain",
                    error,
                )
            })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn read_markdown_image(
    database: &Database,
    checkout_id: &str,
    markdown_path: &str,
    image_path: &str,
) -> Result<CheckoutImage, IpcError> {
    let (repo, checkout) = registered_checkout(database, checkout_id)?;
    ensure_checkout_available(checkout.is_missing, &checkout.canonical_path)?;
    let markdown_path = parse_relative_path(markdown_path)?;
    let markdown_file =
        crate::services::checkout::resolve_checkout_path(&repo, checkout_id, &markdown_path)?;
    if !fs::metadata(&markdown_file)
        .map_err(|error| filesystem_error("could not inspect Markdown file", error))?
        .is_file()
    {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "Markdown path is not a file",
        ));
    }

    let root = Path::new(&checkout.canonical_path);
    let markdown_directory = markdown_file.parent().ok_or_else(|| {
        IpcError::new(
            IpcErrorCode::InvalidPath,
            "Markdown file has no parent folder",
        )
    })?;
    let relative_image = normalize_markdown_image_path(root, markdown_directory, image_path)?;
    let image =
        crate::services::checkout::resolve_checkout_path(&repo, checkout_id, &relative_image)?;
    let metadata =
        fs::metadata(&image).map_err(|error| filesystem_error("could not inspect image", error))?;
    if !metadata.is_file() {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "Markdown image path is not a file",
        ));
    }
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err(image_too_large());
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(&image)
        .and_then(|file| file.take(MAX_IMAGE_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| filesystem_error("could not read image", error))?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(image_too_large());
    }
    let mime_type = image_mime_type(&bytes).ok_or_else(|| {
        IpcError::new(
            IpcErrorCode::BinaryFile,
            "Markdown image must be PNG, JPEG, GIF, or WebP",
        )
    })?;

    Ok(CheckoutImage {
        mime_type: mime_type.to_owned(),
        data_base64: BASE64.encode(&bytes),
        size_bytes: bytes.len(),
    })
}

fn normalize_markdown_image_path(
    root: &Path,
    markdown_directory: &Path,
    image_path: &str,
) -> Result<PathBuf, IpcError> {
    let image_path = image_path.split(['?', '#']).next().unwrap_or_default();
    let image_path = decode_markdown_image_url_path(image_path)?;
    if image_path.is_empty()
        || image_path.starts_with(['/', '\\'])
        || image_path.contains('\\')
        || image_path
            .split('/')
            .next()
            .is_some_and(|first_component| first_component.contains(':'))
    {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "Markdown image URL must be a relative checkout path",
        ));
    }

    let mut normalized = markdown_directory
        .strip_prefix(root)
        .map_err(|_| {
            IpcError::new(
                IpcErrorCode::PathOutsideCheckout,
                "Markdown file is outside the checkout",
            )
        })?
        .to_path_buf();
    for component in Path::new(&image_path).components() {
        match component {
            std::path::Component::Normal(name) if name == ".git" => {
                return Err(IpcError::new(
                    IpcErrorCode::InvalidPath,
                    "Markdown images cannot access .git",
                ));
            }
            std::path::Component::Normal(name) => normalized.push(name),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir if normalized.pop() => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => {
                return Err(IpcError::new(
                    IpcErrorCode::InvalidPath,
                    "Markdown image path escapes the checkout",
                ));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "Markdown image path is empty",
        ));
    }
    Ok(normalized)
}

fn decode_markdown_image_url_path(path: &str) -> Result<String, IpcError> {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let pair = bytes.get(index + 1..index + 3).ok_or_else(|| {
                IpcError::new(
                    IpcErrorCode::InvalidPath,
                    "Markdown image URL has invalid encoding",
                )
            })?;
            let hex = std::str::from_utf8(pair).map_err(|_| {
                IpcError::new(
                    IpcErrorCode::InvalidPath,
                    "Markdown image URL has invalid encoding",
                )
            })?;
            let value = u8::from_str_radix(hex, 16).map_err(|_| {
                IpcError::new(
                    IpcErrorCode::InvalidPath,
                    "Markdown image URL has invalid encoding",
                )
            })?;
            decoded.push(value);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    let decoded = String::from_utf8(decoded).map_err(|_| {
        IpcError::new(
            IpcErrorCode::InvalidPath,
            "Markdown image URL is not valid UTF-8",
        )
    })?;
    if decoded.contains('\0') {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "Markdown image URL contains an invalid character",
        ));
    }
    Ok(decoded)
}

fn image_mime_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn validate_svg(content: &str) -> Result<(), IpcError> {
    let doc = roxmltree::Document::parse(content).map_err(|_| invalid_media())?;
    let root = doc.root_element().tag_name();
    if root.name() != "svg" || root.namespace() != Some("http://www.w3.org/2000/svg") {
        return Err(invalid_media());
    }
    Ok(())
}

fn invalid_media() -> IpcError {
    IpcError::new(
        IpcErrorCode::InvalidPath,
        "file is not a supported media format matching its extension",
    )
}

fn media_limit(path: &Path) -> Option<u64> {
    match extension(path).as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "ico" | "bmp" => Some(16 * 1024 * 1024),
        "mp4" | "webm" | "mov" | "ogv" => Some(64 * 1024 * 1024),
        _ => None,
    }
}

/// The longest MIME `media_mime` returns plus its newline, reserved in front of the payload so the
/// response is one buffer. `media_mime` is the only thing that decides the header, and the media
/// tests assert every MIME it can return fits here.
const MEDIA_HEADER_HEADROOM: usize = 16;

/// MIME is assigned here, never by the caller. No asset protocol or filesystem permission.
pub fn read_media(database: &Database, checkout_id: &str, path: &str) -> Result<Vec<u8>, IpcError> {
    let (repo, checkout) = registered_checkout(database, checkout_id)?;
    ensure_checkout_available(checkout.is_missing, &checkout.canonical_path)?;
    let relative = parse_relative_path(path)?;
    let resolved = crate::services::checkout::resolve_checkout_path(&repo, checkout_id, &relative)?;
    // One buffer, not two, inside this reader: the payload is read behind the reserved header and
    // then moved to sit right behind it, so it is not held twice here. What the IPC transport and
    // the webview do with the response afterwards is outside what this measures.
    let mut response = vec![0u8; MEDIA_HEADER_HEADROOM];
    let (mime, offset) = read_media_file(&resolved, &relative, u64::MAX, &mut response)?;
    let header = mime.len() + 1;
    if header > offset {
        return Err(invalid_media());
    }
    let payload = response.len() - offset;
    response.copy_within(offset.., header);
    response.truncate(header + payload);
    response[..header - 1].copy_from_slice(mime.as_bytes());
    response[header - 1] = b'\n';
    Ok(response)
}

/// Reads one media file's payload into `out`, appended after whatever `out` already holds, and
/// answers with the MIME and the offset the payload starts at.
///
/// The payload is validated where it lands: `read_limit` caps how much of it is read (the probe
/// asks for a header and never the payload), the extension's cap still applies either way, and the
/// MIME is decided from the bytes actually read and returned.
fn read_media_file(
    path: &Path,
    relative: &Path,
    read_limit: u64,
    out: &mut Vec<u8>,
) -> Result<(&'static str, usize), IpcError> {
    let limit = media_limit(relative).ok_or_else(invalid_media)?;
    let metadata =
        fs::metadata(path).map_err(|e| filesystem_error("could not inspect media", e))?;
    if !metadata.is_file() {
        return Err(invalid_media());
    }
    if metadata.len() > limit {
        return Err(too_large());
    }
    let offset = out.len();
    // Reserved up front: `read_to_end` grows what it is given, and a buffer growing from nothing
    // to 64 MiB holds the old and the new one at once on the way there.
    out.reserve(metadata.len().min(read_limit) as usize);
    File::open(path)
        .and_then(|f| f.take((limit + 1).min(read_limit)).read_to_end(out))
        .map_err(|e| filesystem_error("could not read media", e))?;
    let bytes = &out[offset..];
    if bytes.len() as u64 > limit {
        return Err(too_large());
    }
    let mime = media_mime(&extension(relative), bytes).ok_or_else(invalid_media)?;
    Ok((mime, offset))
}

/// An ftyp atom's declared brands, not an arbitrary string elsewhere in the file.
fn bmff_brand(bytes: &[u8], brands: &[&[u8; 4]]) -> bool {
    if bytes.len() < 16 || &bytes[4..8] != b"ftyp" {
        return false;
    }
    let size = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
    if size < 16 || size > bytes.len() || !size.is_multiple_of(4) {
        return false;
    }
    brands.iter().any(|brand| {
        &bytes[8..12] == *brand
            || bytes[16..size]
                .as_chunks::<4>()
                .0
                .iter()
                .any(|b| b == *brand)
    })
}

fn ebml_vint(bytes: &[u8], offset: &mut usize, id: bool) -> Option<u64> {
    let first = *bytes.get(*offset)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 || (id && len > 4) {
        return None;
    }
    let value = bytes.get(*offset..*offset + len)?;
    *offset += len;
    let mut result = if id {
        first as u64
    } else {
        first as u64 & (0xffu64 >> len)
    };
    for b in &value[1..] {
        result = (result << 8) | *b as u64;
    }
    Some(result)
}

fn is_webm(bytes: &[u8]) -> bool {
    let mut offset = 0;
    if ebml_vint(bytes, &mut offset, true) != Some(0x1a45dfa3) {
        return false;
    }
    let Some(size) = ebml_vint(bytes, &mut offset, false).and_then(|s| usize::try_from(s).ok())
    else {
        return false;
    };
    let Some(end) = offset.checked_add(size).filter(|e| *e <= bytes.len()) else {
        return false;
    };
    let header = &bytes[..end];
    while offset < end {
        let Some(id) = ebml_vint(header, &mut offset, true) else {
            return false;
        };
        let Some(size) =
            ebml_vint(header, &mut offset, false).and_then(|s| usize::try_from(s).ok())
        else {
            return false;
        };
        let Some(next) = offset.checked_add(size).filter(|e| *e <= end) else {
            return false;
        };
        if id == 0x4282 {
            return &header[offset..next] == b"webm";
        }
        offset = next;
    }
    false
}

/// Ogg can carry audio. Require a BOS page with a Theora video identification packet.
fn is_ogg_video(bytes: &[u8]) -> bool {
    let mut offset = 0;
    while offset < bytes.len().min(64 * 1024) {
        let Some(header) = bytes.get(offset..offset + 27) else {
            return false;
        };
        if &header[..4] != b"OggS" || header[4] != 0 {
            return false;
        }
        let count = header[26] as usize;
        let Some(laces) = bytes.get(offset + 27..offset + 27 + count) else {
            return false;
        };
        let body = offset + 27 + count;
        let size: usize = laces.iter().map(|b| *b as usize).sum();
        let Some(packet) = bytes.get(body..body + size) else {
            return false;
        };
        if header[5] & 2 != 0
            && laces.first().is_some_and(|n| *n >= 7)
            && packet.starts_with(b"\x80theora")
        {
            return true;
        }
        offset = body + size;
    }
    false
}

fn media_mime(ext: &str, bytes: &[u8]) -> Option<&'static str> {
    match ext {
        "png" if image_mime_type(bytes) == Some("image/png") => Some("image/png"),
        "jpg" | "jpeg" if image_mime_type(bytes) == Some("image/jpeg") => Some("image/jpeg"),
        "gif" if image_mime_type(bytes) == Some("image/gif") => Some("image/gif"),
        "webp" if image_mime_type(bytes) == Some("image/webp") => Some("image/webp"),
        "bmp" if bytes.len() >= 26 && bytes.starts_with(b"BM") => Some("image/bmp"),
        "ico" if bytes.len() >= 22 && bytes.starts_with(b"\0\0\x01\0") && bytes[4..6] != [0, 0] => {
            Some("image/x-icon")
        }
        "avif" if bmff_brand(bytes, &[b"avif", b"avis"]) => Some("image/avif"),
        "mp4"
            if !bmff_brand(bytes, &[b"avif", b"avis", b"qt  "])
                && bmff_brand(
                    bytes,
                    &[
                        b"isom", b"iso2", b"iso3", b"iso4", b"iso5", b"iso6", b"mp41", b"mp42",
                        b"avc1", b"M4V ", b"dash",
                    ],
                ) =>
        {
            Some("video/mp4")
        }
        "mov" if bmff_brand(bytes, &[b"qt  "]) => Some("video/quicktime"),
        "webm" if is_webm(bytes) => Some("video/webm"),
        "ogv" if is_ogg_video(bytes) => Some("video/ogg"),
        _ => None,
    }
}

fn registered_checkout(
    database: &Database,
    checkout_id: &str,
) -> Result<(Repo, Checkout), IpcError> {
    database
        .load_registered_checkout(checkout_id)
        .map_err(|error| IpcError::new(IpcErrorCode::OperationFailed, error))?
        .ok_or_else(|| {
            IpcError::new(
                IpcErrorCode::InvalidCheckout,
                "checkout ID is not registered",
            )
        })
}

fn parse_relative_path(path: &str) -> Result<PathBuf, IpcError> {
    if path.is_empty() || path.contains('\0') {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "path must be relative to the checkout and cannot contain NUL",
        ));
    }
    let path = Path::new(path);
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        ) || matches!(component, std::path::Component::Normal(name) if name == ".git")
    }) {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "path must be relative, cannot contain '..', and cannot access .git",
        ));
    }
    Ok(path.to_path_buf())
}

fn parse_review_relative_path(path: &str) -> Result<PathBuf, IpcError> {
    let parsed = Path::new(path);
    if path.contains('\0')
        || path.contains('\\')
        || parsed.is_absolute()
        || parsed
            .components()
            .any(|component| matches!(component, std::path::Component::Prefix(_)))
    {
        return Err(IpcError::new(
            IpcErrorCode::PathOutsideCheckout,
            "review path must stay inside the review folder",
        ));
    }
    if path
        .split('/')
        .any(|part| part == ".." || part == "." || part.is_empty())
    {
        return Err(IpcError::new(
            IpcErrorCode::PathOutsideCheckout,
            "review path must stay inside the review folder",
        ));
    }
    if path.is_empty() || path.len() > 4096 || !path.ends_with(".md") {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "review path must be a relative Markdown file",
        ));
    }
    Ok(parsed.to_path_buf())
}

fn resolve_review_path(root: &Path, relative_path: &Path) -> Result<PathBuf, IpcError> {
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| filesystem_error("could not resolve review export folder", error))?;
    let canonical_path = fs::canonicalize(root.join(relative_path))
        .map_err(|error| filesystem_error("could not resolve review file", error))?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err(IpcError::new(
            IpcErrorCode::PathOutsideCheckout,
            "selected path resolves outside the review folder",
        ));
    }
    Ok(canonical_path)
}

fn invalid_origin() -> IpcError {
    IpcError::new(IpcErrorCode::InvalidPath, "file origin is not supported")
}

fn missing_review_places() -> IpcError {
    IpcError::new(
        IpcErrorCode::OperationFailed,
        "review settings were not loaded for this operation",
    )
}

fn ignored_paths(root: &Path, entries: &[FileEntry]) -> Result<HashSet<String>, IpcError> {
    if entries.is_empty() {
        return Ok(HashSet::new());
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "--no-index", "--stdin", "-z"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            IpcError::new(
                IpcErrorCode::GitFailed,
                format!("could not apply Git ignore rules: {error}"),
            )
        })?;
    {
        let stdin = child.stdin.as_mut().expect("piped stdin");
        for entry in entries {
            stdin
                .write_all(entry.path.as_bytes())
                .and_then(|_| stdin.write_all(&[0]))
                .map_err(|error| {
                    IpcError::new(
                        IpcErrorCode::GitFailed,
                        format!("could not apply Git ignore rules: {error}"),
                    )
                })?;
        }
    }
    let output = child.wait_with_output().map_err(|error| {
        IpcError::new(
            IpcErrorCode::GitFailed,
            format!("could not apply Git ignore rules: {error}"),
        )
    })?;
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(IpcError::new(
            IpcErrorCode::GitFailed,
            "Git could not apply ignore rules",
        ));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .filter_map(|path| std::str::from_utf8(path).ok().map(str::to_owned))
        .collect())
}

fn filesystem_error(action: &str, error: std::io::Error) -> IpcError {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => IpcErrorCode::FolderMissing,
        std::io::ErrorKind::PermissionDenied => IpcErrorCode::PermissionDenied,
        _ => IpcErrorCode::OperationFailed,
    };
    IpcError::new(code, format!("{action}: {error}"))
}

fn missing(message: &str) -> IpcError {
    IpcError::new(IpcErrorCode::FolderMissing, message)
}

fn ensure_checkout_available(is_missing: bool, path: &str) -> Result<(), IpcError> {
    if is_missing {
        return Err(missing("checkout is no longer available"));
    }
    let metadata =
        fs::metadata(path).map_err(|error| filesystem_error("checkout is unavailable", error))?;
    if !metadata.is_dir() {
        return Err(missing("checkout is no longer available"));
    }
    Ok(())
}

fn too_large() -> IpcError {
    IpcError::new(
        IpcErrorCode::FileTooLarge,
        "file exceeds the 1 MiB text preview limit",
    )
}

fn image_too_large() -> IpcError {
    IpcError::new(
        IpcErrorCode::FileTooLarge,
        "image exceeds the 2 MiB Markdown preview limit",
    )
}

fn binary_file() -> IpcError {
    IpcError::new(
        IpcErrorCode::BinaryFile,
        "file is binary or is not valid UTF-8",
    )
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::Path,
        process::Command,
        sync::{Arc, Barrier},
    };

    use tempfile::tempdir;

    use crate::{
        config::{AppSettings, ReviewSettings, REVIEW_STORAGE_DEFAULT, REVIEW_STORAGE_WORKDIR},
        domain::{files::FileEntry, ipc::IpcErrorCode},
        persistence::Database,
        services::workspace,
    };

    use super::{
        clear_review_folder, export_review_for_checkout, export_review_markdown, list,
        open_externally, probe, read, read_markdown_image, read_prettier_config, review_folder,
        review_root_for, write, FileContent, IpcError, ReviewDocumentOrigin, ReviewPlaces,
        EXPORTED_REVIEW_MARKER, MAX_ROUND_PROMPT_BYTES, REVIEW_DIR_IN_CHECKOUT,
    };

    fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn db(temp: &Path) -> Database {
        Database::open(temp.join("workspace.sqlite3")).unwrap()
    }

    /// A place for direct service tests, with the settings file in a temporary home.
    fn places_in(home: &Path, storage: &str) -> ReviewPlaces {
        ReviewPlaces::new(
            home.join(".muster").join("config.yml"),
            AppSettings {
                reviews: ReviewSettings {
                    storage: storage.into(),
                },
                ..AppSettings::default()
            },
        )
    }

    /// The pair that puts reviews inside the checkout, which is what the `review` origin tests
    /// read through.
    fn reviews_in_the_checkout() -> ReviewPlaces {
        places_in(Path::new("/"), REVIEW_STORAGE_WORKDIR)
    }

    fn read_checkout(
        database: &Database,
        checkout_id: &str,
        path: &str,
    ) -> Result<FileContent, IpcError> {
        read(database, checkout_id, "checkout", Path::new(path), None)
    }

    fn write_checkout(
        database: &Database,
        checkout_id: &str,
        path: &str,
        content: &str,
        expected_content: &str,
    ) -> Result<(), IpcError> {
        write(
            database,
            checkout_id,
            "checkout",
            Path::new(path),
            content,
            expected_content,
            None,
        )
    }

    #[test]
    fn checkout_and_pinned_review_origins_ignore_invalid_review_settings() {
        let temp = tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        let config_file = temp.path().join(".muster/config.yml");
        fs::create_dir_all(&checkout).unwrap();
        fs::create_dir_all(config_file.parent().unwrap()).unwrap();
        fs::write(&config_file, "reviews: [\n").unwrap();
        fs::write(checkout.join("notes.txt"), "before").unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &checkout).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;
        let places = ReviewPlaces::load_for_origin(&config_file, "checkout").unwrap();

        assert!(places.is_none());
        assert_eq!(
            read(
                &database,
                checkout_id,
                "checkout",
                Path::new("notes.txt"),
                places.as_ref(),
            )
            .unwrap()
            .content,
            "before"
        );
        assert!(read_prettier_config(
            &database,
            checkout_id,
            "checkout",
            "notes.txt",
            places.as_ref(),
        )
        .unwrap()
        .is_none());
        write(
            &database,
            checkout_id,
            "checkout",
            Path::new("notes.txt"),
            "after",
            "before",
            places.as_ref(),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(checkout.join("notes.txt")).unwrap(),
            "after"
        );

        assert!(
            ReviewPlaces::load_for_origin(&config_file, "review-default")
                .unwrap()
                .is_some()
        );
        assert!(
            ReviewPlaces::load_for_origin(&config_file, "review-workdir")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn pinned_review_origins_read_write_format_and_clear_their_own_roots() {
        let temp = tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        let config_file = temp.path().join(".muster/config.yml");
        let default_root = temp.path().join(".muster/tmp/reviews");
        fs::create_dir_all(&checkout).unwrap();
        fs::create_dir_all(&default_root).unwrap();
        fs::create_dir_all(config_file.parent().unwrap()).unwrap();
        fs::write(&config_file, "reviews:\n  storage: workdir\n").unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &checkout).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;
        let default_places = ReviewPlaces::load_for_origin(&config_file, "review-default")
            .unwrap()
            .unwrap();
        let workdir_places = ReviewPlaces::load_for_origin(&config_file, "review-workdir")
            .unwrap()
            .unwrap();
        let workdir_root = checkout.join(REVIEW_DIR_IN_CHECKOUT);
        fs::create_dir_all(&workdir_root).unwrap();
        fs::write(default_root.join("same.md"), "default").unwrap();
        fs::write(workdir_root.join("same.md"), "workdir").unwrap();
        fs::write(default_root.join(".prettierrc"), r#"{"tabWidth": 4}"#).unwrap();
        fs::write(workdir_root.join(".prettierrc"), r#"{"tabWidth": 7}"#).unwrap();

        assert_eq!(
            read(
                &database,
                checkout_id,
                "review-default",
                Path::new("same.md"),
                Some(&default_places)
            )
            .unwrap()
            .content,
            "default"
        );
        assert_eq!(
            read(
                &database,
                checkout_id,
                "review-workdir",
                Path::new("same.md"),
                Some(&workdir_places)
            )
            .unwrap()
            .content,
            "workdir"
        );
        for (origin, places, tab_width) in [
            ("review-default", &default_places, 4),
            ("review-workdir", &workdir_places, 7),
        ] {
            let config =
                read_prettier_config(&database, checkout_id, origin, "same.md", Some(places))
                    .unwrap()
                    .unwrap();
            assert_eq!(config.options["tabWidth"], tab_width);
        }
        write(
            &database,
            checkout_id,
            "review-workdir",
            Path::new("same.md"),
            "saved workdir",
            "workdir",
            Some(&workdir_places),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(default_root.join("same.md")).unwrap(),
            "default"
        );
        assert_eq!(
            fs::read_to_string(workdir_root.join("same.md")).unwrap(),
            "saved workdir"
        );

        let default_export = export_review_for_checkout(
            &database,
            checkout_id,
            &default_places,
            "2026-03-14",
            "2026-03-14-1532",
            "default export",
        )
        .unwrap();
        let workdir_export = export_review_for_checkout(
            &database,
            checkout_id,
            &workdir_places,
            "2026-03-14",
            "2026-03-14-1532",
            "workdir export",
        )
        .unwrap();
        assert_eq!(default_export.origin, ReviewDocumentOrigin::ReviewDefault);
        assert_eq!(workdir_export.origin, ReviewDocumentOrigin::ReviewWorkdir);
        assert_eq!(default_export.path, workdir_export.path);
        let cleared = clear_review_folder(&database, checkout_id, &default_places).unwrap();
        assert_eq!(cleared.files, 1);
        assert!(!default_root.join(&default_export.path).exists());
        assert!(workdir_root.join(&workdir_export.path).exists());
    }

    #[test]
    fn media_formats_match_extensions_and_container_identifiers() {
        use super::media_mime;
        let bmff = |brand: &[u8]| {
            let mut b = vec![0, 0, 0, 16];
            b.extend_from_slice(b"ftyp");
            b.extend_from_slice(brand);
            b.extend_from_slice(&[0; 4]);
            b
        };
        let mut bmp = vec![0; 26];
        bmp[..2].copy_from_slice(b"BM");
        let mut ico = vec![0; 22];
        ico[..6].copy_from_slice(&[0, 0, 1, 0, 1, 0]);
        let mut ogg = vec![0; 27];
        ogg[..4].copy_from_slice(b"OggS");
        ogg[5] = 2;
        ogg[26] = 1;
        ogg.push(7);
        ogg.extend_from_slice(b"\x80theora");
        let webm = b"\x1a\x45\xdf\xa3\x87\x42\x82\x84webm".to_vec();
        for (ext, bytes, mime) in [
            ("png", b"\x89PNG\r\n\x1a\n".to_vec(), "image/png"),
            ("jpg", b"\xff\xd8\xff".to_vec(), "image/jpeg"),
            ("jpeg", b"\xff\xd8\xff".to_vec(), "image/jpeg"),
            ("gif", b"GIF89a".to_vec(), "image/gif"),
            ("webp", b"RIFF\0\0\0\0WEBP".to_vec(), "image/webp"),
            ("bmp", bmp, "image/bmp"),
            ("ico", ico, "image/x-icon"),
            ("avif", bmff(b"avif"), "image/avif"),
            ("mp4", bmff(b"isom"), "video/mp4"),
            ("mov", bmff(b"qt  "), "video/quicktime"),
            ("webm", webm, "video/webm"),
            ("ogv", ogg.clone(), "video/ogg"),
        ] {
            assert_eq!(media_mime(ext, &bytes), Some(mime), "{ext}");
            // The response reserves this much in front of the payload, and moves the payload to
            // fit: a MIME that outgrew it would be refused rather than served.
            assert!(mime.len() < super::MEDIA_HEADER_HEADROOM, "{mime}");
            assert_eq!(media_mime("txt", &bytes), None);
            for length in 0..bytes.len() {
                assert_eq!(
                    media_mime(ext, &bytes[..length]),
                    None,
                    "truncated {ext} at {length}"
                );
            }
        }
        assert_eq!(media_mime("png", b"GIF89a"), None);
        assert_eq!(media_mime("mp4", &bmff(b"avif")), None);
        assert_eq!(media_mime("mp4", &bmff(b"qt  ")), None);
        assert_eq!(media_mime("mp4", &bmff(b"zzzz")), None);
        assert_eq!(media_mime("mov", &bmff(b"isom")), None);
        assert_eq!(
            media_mime("webm", b"\x1a\x45\xdf\xa3\x8b\x42\x82\x88matroska"),
            None
        );
        ogg[28..].copy_from_slice(b"vorbis!");
        assert_eq!(media_mime("ogv", &ogg), None);
        assert_eq!(media_mime("ogv", b"OggS"), None);
        // Eight-byte sizes must not panic, and must stay bounded by the input.
        assert_eq!(
            media_mime("webm", b"\x1a\x45\xdf\xa3\x01\xff\xff\xff\xff\xff\xff\xff"),
            None
        );
    }

    #[test]
    fn sec_media_reads_and_probe_reuse_containment_and_caps() {
        use super::read_media;
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir(&root).unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let id = &state.repos[0].checkouts[0].id;
        let png = b"\x89PNG\r\n\x1a\n";
        fs::write(root.join("image.PNG"), png).unwrap();
        assert_eq!(
            read_media(&database, id, "image.PNG").unwrap(),
            [b"image/png\n".as_slice(), png].concat()
        );
        assert!(probe(&database, id, "image.PNG", None).unwrap().is_some());
        // The longest MIME the reader can assign, whose header fills the space reserved in front of the
        // payload exactly. image/png above is the other end of the same move: a shorter header
        // pulls the payload up behind it.
        let mov = b"\0\0\0\x10ftypqt  \0\0\0\0";
        fs::write(root.join("clip.MOV"), mov).unwrap();
        assert_eq!(
            read_media(&database, id, "clip.MOV").unwrap(),
            [b"video/quicktime\n".as_slice(), mov].concat()
        );
        assert!(read_media(&database, "unknown", "image.PNG").is_err());
        for path in ["../image.PNG", ".git/image.PNG", "/image.PNG"] {
            assert!(read_media(&database, id, path).is_err());
        }
        fs::write(root.join("mismatch.jpg"), png).unwrap();
        assert!(read_media(&database, id, "mismatch.jpg").is_err());
        assert!(probe(&database, id, "mismatch.jpg", None)
            .unwrap()
            .is_none());
        #[cfg(unix)]
        {
            let outside = temp.path().join("outside.png");
            fs::write(&outside, png).unwrap();
            std::os::unix::fs::symlink(outside, root.join("escape.png")).unwrap();
            assert!(read_media(&database, id, "escape.png").is_err());
            assert!(probe(&database, id, "escape.png", None).unwrap().is_none());
        }
        for (name, cap) in [
            ("large.png", 16 * 1024 * 1024),
            ("large.mp4", 64 * 1024 * 1024),
        ] {
            let file = fs::File::create(root.join(name)).unwrap();
            file.set_len(cap + 1).unwrap();
            assert!(matches!(
                read_media(&database, id, name).unwrap_err().code,
                IpcErrorCode::FileTooLarge
            ));
            assert!(probe(&database, id, name, None).unwrap().is_none());
        }
        assert!(read_media(&database, id, ".").is_err());
    }

    /// The chain that decides what may leave the app, in the order it decides it, with the opener
    /// itself stubbed by the fact that nothing here reaches it: every case below is refused before
    /// the spawn, which is the property worth pinning, because a case that got past the boundary
    /// would put a real `open` on the real machine rather than fail a test.
    #[test]
    fn opening_a_file_outside_refuses_what_a_read_also_refuses() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir(&root).unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let id = &state.repos[0].checkouts[0].id;
        fs::write(root.join("notes.txt"), "hello\n").unwrap();
        fs::create_dir(root.join("src")).unwrap();

        // A name that is not this checkout's to name. `.git` is here because it is the one name
        // `parse_relative_path` refuses that a tree could otherwise have listed.
        for path in ["../notes.txt", ".git/config", "/etc/passwd", ""] {
            assert!(
                open_externally(&database, id, path).is_err(),
                "{path} was opened"
            );
        }
        // A checkout this database does not have, and one whose directory is gone.
        assert!(open_externally(&database, "unknown", "notes.txt").is_err());

        // A link inside the checkout that leaves it is a way out of the checkout, and this is the
        // last place that is still knowable: the opener would be handed the real path behind it.
        #[cfg(unix)]
        {
            let outside = temp.path().join("outside.txt");
            fs::write(&outside, "secret\n").unwrap();
            std::os::unix::fs::symlink(&outside, root.join("escape.txt")).unwrap();
            let error = open_externally(&database, id, "escape.txt").unwrap_err();
            assert_eq!(error.code, IpcErrorCode::PathOutsideCheckout);
        }

        // A name that is inside the checkout and is not there. It never reaches the opener either,
        // and it does not have to: the resolution canonicalizes the path, so a name with nothing
        // behind it is already an answer about the checkout rather than one about the machine.
        let missing = open_externally(&database, id, "gone.txt").unwrap_err();
        assert_eq!(missing.code, IpcErrorCode::FolderMissing);
    }

    #[test]
    fn media_probe_reads_only_a_bounded_header_of_large_videos() {
        use super::{read_media_file, MAX_MEDIA_HEADER_BYTES};
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir(&root).unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let id = &state.repos[0].checkouts[0].id;
        let path = root.join("large.mp4");
        fs::write(&path, b"\0\0\0\x10ftypisom\0\0\0\0").unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(64 * 1024 * 1024)
            .unwrap();
        let mut header = Vec::new();
        let (mime, offset) = read_media_file(
            &path,
            Path::new("large.mp4"),
            MAX_MEDIA_HEADER_BYTES,
            &mut header,
        )
        .unwrap();
        assert_eq!(mime, "video/mp4");
        assert_eq!((header.len() - offset) as u64, MAX_MEDIA_HEADER_BYTES);
        assert!(probe(&database, id, "large.mp4", None).unwrap().is_some());
    }

    #[test]
    fn svg_source_remains_editable_while_probe_validates_the_xml_root() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir(&root).unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let id = &state.repos[0].checkouts[0].id;
        let svg = "<svg xmlns='http://www.w3.org/2000/svg'/>";
        fs::write(root.join("icon.svg"), svg).unwrap();
        assert_eq!(
            read_checkout(&database, id, "icon.svg").unwrap().content,
            svg
        );
        assert!(probe(&database, id, "icon.svg", None).unwrap().is_some());
        let next = "<s:svg xmlns:s='http://www.w3.org/2000/svg'><s:rect/></s:svg>";
        write_checkout(&database, id, "icon.svg", next, svg).unwrap();
        write_checkout(&database, id, "icon.svg", "<svg", next).unwrap();
        assert_eq!(fs::read_to_string(root.join("icon.svg")).unwrap(), "<svg");
        write_checkout(&database, id, "icon.svg", svg, "<svg").unwrap();
        for invalid in ["<html><!-- <svg> --></html>", "<svg>", "<svg/>", "<svg xmlns='wrong'/>", "<!DOCTYPE svg [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><svg xmlns='http://www.w3.org/2000/svg'>&x;</svg>"] {
            fs::write(root.join("bad.svg"), invalid).unwrap();
            assert_eq!(read_checkout(&database, id, "bad.svg").unwrap().content, invalid);
            assert!(probe(&database, id, "bad.svg", None).unwrap().is_none());
            write_checkout(&database, id, "bad.svg", "<svg", invalid).unwrap();
        }
        let file = fs::File::create(root.join("large.svg")).unwrap();
        file.set_len(1024 * 1024 + 1).unwrap();
        assert!(matches!(
            read_checkout(&database, id, "large.svg").unwrap_err().code,
            IpcErrorCode::FileTooLarge
        ));
    }

    #[test]
    fn checkout_id_selects_only_its_registered_tree_and_rejects_unknown_ids() {
        let temp = tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::write(first.join("first.txt"), "one").unwrap();
        fs::write(second.join("second.txt"), "two").unwrap();
        let database = db(temp.path());
        let first_state = workspace::register_folder(&database, &first).unwrap();
        let second_state = workspace::register_folder(&database, &second).unwrap();

        let first_files = list(&database, &first_state.repos[0].checkouts[0].id, ".").unwrap();
        let second_files = list(&database, &second_state.repos[1].checkouts[0].id, ".").unwrap();

        assert_eq!(
            first_files
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["first.txt"]
        );
        assert_eq!(
            second_files
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["second.txt"]
        );
        assert!(list(&database, "checkout:unregistered", ".").is_err());
    }

    fn prettier_config(
        database: &Database,
        checkout_id: &str,
        origin: &str,
        path: &str,
        places: Option<&ReviewPlaces>,
    ) -> Result<Option<serde_json::Value>, IpcError> {
        read_prettier_config(database, checkout_id, origin, path, places)
            .map(|config| config.map(|config| config.options))
    }

    #[test]
    fn prettier_options_come_from_the_nearest_config_above_the_file() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir_all(root.join("src/nested")).unwrap();
        fs::write(
            root.join(".prettierrc"),
            r#"{ "printWidth": 120, "parser": "yaml", "plugins": ["./plugin.js"] }"#,
        )
        .unwrap();
        // The nearest config wins, and the two keys the reader owns are gone by the time it sees
        // any of this: the parser is the file's own language and the plugins are the bundles the
        // reader loads, and neither is anything a config on disk may name.
        fs::write(root.join("src/.prettierrc.yaml"), "semi: false\n").unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;

        let nested = prettier_config(
            &database,
            checkout_id,
            "checkout",
            "src/nested/app.ts",
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(nested, serde_json::json!({ "semi": false }));

        let at_root = prettier_config(&database, checkout_id, "checkout", "README.md", None)
            .unwrap()
            .unwrap();
        assert_eq!(at_root, serde_json::json!({ "printWidth": 120 }));
    }

    #[test]
    fn a_prettierrc_with_no_extension_is_read_as_yaml_when_it_is_not_json() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir_all(root.join("src")).unwrap();
        // The name carries no extension, and this is the shape people actually write in it.
        fs::write(
            root.join(".prettierrc"),
            "printWidth: 120\nsingleQuote: true\n",
        )
        .unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;

        assert_eq!(
            prettier_config(&database, checkout_id, "checkout", "src/app.ts", None,)
                .unwrap()
                .unwrap(),
            serde_json::json!({ "printWidth": 120, "singleQuote": true })
        );
    }

    #[test]
    fn config_overrides_are_carried_to_the_worker_without_partial_matching() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir_all(root.join("src")).unwrap();
        let options = serde_json::json!({ "printWidth": 120, "overrides": [
            { "files": "*.{ts,tsx}", "options": { "semi": false, "parser": "css" } }
        ] });
        fs::write(root.join(".prettierrc"), options.to_string()).unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let config = read_prettier_config(
            &database,
            &state.repos[0].checkouts[0].id,
            "checkout",
            "src/école.ts",
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(config.options, options);
        assert_eq!(config.path, "src/école.ts");
    }

    #[test]
    fn an_override_pattern_is_read_from_the_directory_of_the_config_that_holds_it() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir_all(root.join("packages/app/src")).unwrap();
        fs::create_dir_all(root.join("packages/other/src")).unwrap();
        fs::write(root.join("packages/app/src/a.ts"), "const a = 1;").unwrap();
        fs::write(root.join("packages/other/src/a.ts"), "const a = 1;").unwrap();
        // Both packages keep the same file name, and only the pattern written next to one of them
        // names it. Matched against the checkout root this names neither.
        fs::write(
            root.join("packages/app/.prettierrc"),
            r#"{ "printWidth": 100, "overrides": [ { "files": "src/*.ts", "options": { "semi": false } } ] }"#,
        )
        .unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;

        assert_eq!(
            prettier_config(
                &database,
                checkout_id,
                "checkout",
                "packages/app/src/a.ts",
                None,
            )
            .unwrap()
            .unwrap(),
            serde_json::json!({ "printWidth": 100, "overrides": [ { "files": "src/*.ts", "options": { "semi": false } } ] })
        );
        let config = read_prettier_config(
            &database,
            checkout_id,
            "checkout",
            "packages/app/src/a.ts",
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(config.path, "src/a.ts");
        // The other package has no config at all, so its file keeps Prettier's defaults.
        assert_eq!(
            prettier_config(
                &database,
                checkout_id,
                "checkout",
                "packages/other/src/a.ts",
                None,
            )
            .unwrap(),
            None
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_config_this_process_cannot_open_is_an_error_and_not_an_absence() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join(".prettierrc"), "{}").unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;
        let config = root.join(".prettierrc");

        // A config nobody may read is not a checkout that configures nothing: answering `None`
        // would format the file with defaults and tell the reader nothing about why.
        fs::set_permissions(&config, fs::Permissions::from_mode(0o000)).unwrap();
        let outcome = prettier_config(&database, checkout_id, "checkout", "src/app.ts", None);
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();

        assert!(
            outcome.is_err(),
            "an unreadable config must not read as no config"
        );
    }

    #[test]
    fn sec_prettier_options_never_read_through_a_symlink_out_of_the_checkout() {
        #[cfg(unix)]
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        let outside = temp.path().join("outside");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join(".prettierrc"), r#"{ "printWidth": 1 }"#).unwrap();
        #[cfg(unix)]
        {
            // A directory reached through a link, and a config that is itself one, are the two
            // ways out of the folder the reader asked about.
            symlink(&outside, root.join("src/linked")).unwrap();
            symlink(outside.join(".prettierrc"), root.join(".prettierrc")).unwrap();
        }
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;

        #[cfg(unix)]
        {
            let through_a_link = prettier_config(
                &database,
                checkout_id,
                "checkout",
                "src/linked/app.ts",
                None,
            )
            .unwrap_err();
            assert!(matches!(
                through_a_link.code,
                IpcErrorCode::PathOutsideCheckout
            ));

            let config_is_a_link =
                prettier_config(&database, checkout_id, "checkout", "README.md", None).unwrap_err();
            assert!(matches!(
                config_is_a_link.code,
                IpcErrorCode::PathOutsideCheckout
            ));
        }
    }

    #[test]
    fn a_checkout_that_configures_nothing_answers_with_no_prettier_options() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir_all(root.join("src")).unwrap();
        // A package.json without a `prettier` key and a config this process cannot read are both
        // the same answer: not a config, and the walk keeps going.
        fs::write(root.join("package.json"), "{ \"name\": \"repo\" }").unwrap();
        fs::write(root.join("prettier.config.js"), "module.exports = {}").unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;

        assert!(
            prettier_config(&database, checkout_id, "checkout", "src/app.ts", None,)
                .unwrap()
                .is_none()
        );
        assert!(
            prettier_config(&database, checkout_id, "review-default", "review.md", None,).is_err()
        );
        assert!(prettier_config(
            &database,
            "checkout:unregistered",
            "checkout",
            "src/app.ts",
            None,
        )
        .is_err());
    }

    #[test]
    fn a_package_json_prettier_key_is_a_config_and_a_broken_config_says_so() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("package.json"),
            r#"{ "prettier": { "useTabs": true } }"#,
        )
        .unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;

        assert_eq!(
            prettier_config(&database, checkout_id, "checkout", "src/app.ts", None,)
                .unwrap()
                .unwrap(),
            serde_json::json!({ "useTabs": true })
        );

        // A config that will not parse stops the walk instead of being skipped: formatting with
        // the defaults while the project says otherwise is worse than saying the config is broken.
        fs::create_dir_all(root.join("src/broken")).unwrap();
        fs::write(root.join("src/broken/.prettierrc"), "{ not json").unwrap();
        let broken = prettier_config(
            &database,
            checkout_id,
            "checkout",
            "src/broken/other.ts",
            None,
        )
        .unwrap_err();
        assert!(matches!(broken.code, IpcErrorCode::OperationFailed));
    }

    #[test]
    fn sec_01_02_rejects_path_traversal_and_symlinks_that_escape_the_checkout() {
        #[cfg(unix)]
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "secret").unwrap();
        #[cfg(unix)]
        symlink(&outside, root.join("escape")).unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;

        let traversal = read_checkout(&database, checkout_id, "../outside/secret.txt").unwrap_err();
        assert!(matches!(traversal.code, IpcErrorCode::InvalidPath));
        let git_metadata = read_checkout(&database, checkout_id, ".git/config").unwrap_err();
        assert!(matches!(git_metadata.code, IpcErrorCode::InvalidPath));
        #[cfg(unix)]
        {
            let escape = read_checkout(&database, checkout_id, "escape/secret.txt").unwrap_err();
            assert!(matches!(escape.code, IpcErrorCode::PathOutsideCheckout));
        }
    }

    #[test]
    fn sec_11_a_review_origin_reads_only_the_review_root() {
        #[cfg(unix)]
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        let review_root = checkout.join(REVIEW_DIR_IN_CHECKOUT);
        let outside = temp.path().join("outside");
        fs::create_dir_all(&review_root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(checkout.join("checkout-only.md"), "checkout").unwrap();
        fs::write(outside.join("secret.md"), "secret").unwrap();
        fs::write(review_root.join("inside.md"), "inside").unwrap();
        #[cfg(unix)]
        {
            symlink(outside.join("secret.md"), review_root.join("escape.md")).unwrap();
            symlink(
                review_root.join("inside.md"),
                review_root.join("inside-link.md"),
            )
            .unwrap();
        }
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &checkout).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;
        let places = reviews_in_the_checkout();

        let checkout_file = read(
            &database,
            checkout_id,
            "review-workdir",
            Path::new("checkout-only.md"),
            Some(&places),
        )
        .unwrap_err();
        assert!(matches!(checkout_file.code, IpcErrorCode::FolderMissing));
        let traversal = read(
            &database,
            checkout_id,
            "review-workdir",
            Path::new("../checkout/checkout-only.md"),
            Some(&places),
        )
        .unwrap_err();
        assert!(matches!(traversal.code, IpcErrorCode::PathOutsideCheckout));
        #[cfg(unix)]
        {
            let escape = read(
                &database,
                checkout_id,
                "review-workdir",
                Path::new("escape.md"),
                Some(&places),
            )
            .unwrap_err();
            assert!(matches!(escape.code, IpcErrorCode::PathOutsideCheckout));
            assert_eq!(
                read(
                    &database,
                    checkout_id,
                    "review-workdir",
                    Path::new("inside-link.md"),
                    Some(&places),
                )
                .unwrap()
                .content,
                "inside"
            );
        }
    }

    #[test]
    fn sec_12_a_review_origin_refuses_to_write_anything_but_markdown() {
        let temp = tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        let review_root = checkout.join(REVIEW_DIR_IN_CHECKOUT);
        fs::create_dir_all(&review_root).unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &checkout).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;
        let places = reviews_in_the_checkout();

        for path in ["notes.txt", "notes"] {
            let error = write(
                &database,
                checkout_id,
                "review-workdir",
                Path::new(path),
                "changed",
                "before",
                Some(&places),
            )
            .unwrap_err();
            assert!(matches!(error.code, IpcErrorCode::InvalidPath));
        }
    }

    #[test]
    fn a_review_origin_never_reaches_outside_a_registered_checkout() {
        let temp = tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        fs::create_dir_all(&checkout).unwrap();
        fs::write(checkout.join("notes.txt"), "safe").unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &checkout).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;

        let error = write_checkout(
            &database,
            checkout_id,
            "../outside/secret.txt",
            "changed",
            "secret",
        )
        .unwrap_err();
        assert!(matches!(error.code, IpcErrorCode::InvalidPath));
        assert_eq!(
            fs::read_to_string(checkout.join("notes.txt")).unwrap(),
            "safe"
        );
    }

    /// The one preference that decides this: the app's own folder in the home, or a folder inside
    /// the checkout the review was made in. Both are named here, because the dialog names both.
    #[test]
    fn a_review_folder_is_the_app_folder_or_the_one_inside_the_checkout() {
        let temp = tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        let home = temp.path().join("home");
        fs::create_dir_all(&checkout).unwrap();
        fs::create_dir_all(&home).unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &checkout).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;

        assert_eq!(
            review_root_for(
                &database,
                checkout_id,
                &places_in(&home, REVIEW_STORAGE_DEFAULT)
            )
            .unwrap(),
            home.join(".muster/tmp/reviews"),
            "the default answer must be the folder reviews have always gone to",
        );
        assert_eq!(
            review_root_for(&database, checkout_id, &reviews_in_the_checkout()).unwrap(),
            Path::new(&state.repos[0].checkouts[0].canonical_path).join(REVIEW_DIR_IN_CHECKOUT),
        );
        let folder = review_folder(&database, checkout_id, &reviews_in_the_checkout()).unwrap();
        assert_eq!((folder.files, folder.bytes), (0, 0));
    }

    #[test]
    fn workdir_export_does_not_recreate_a_missing_checkout() {
        let temp = tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        fs::create_dir_all(&checkout).unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &checkout).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;
        fs::remove_dir_all(&checkout).unwrap();

        let error = export_review_for_checkout(
            &database,
            checkout_id,
            &reviews_in_the_checkout(),
            "2026-03-14",
            "2026-03-14-1532",
            "review",
        )
        .unwrap_err();

        assert_eq!(error.code, IpcErrorCode::FolderMissing);
        assert!(!checkout.exists());
    }

    #[test]
    #[cfg(unix)]
    fn workdir_export_rejects_escaping_ancestor_symlinks_and_allows_internal_ones() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        let outside = temp.path().join("outside");
        let internal = checkout.join("internal");
        fs::create_dir_all(&checkout).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(&internal).unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &checkout).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;
        let places = reviews_in_the_checkout();

        symlink(&outside, checkout.join(".muster")).unwrap();
        let error = export_review_for_checkout(
            &database,
            checkout_id,
            &places,
            "2026-03-14",
            "2026-03-14-1532",
            "review",
        )
        .unwrap_err();
        assert_eq!(error.code, IpcErrorCode::PathOutsideCheckout);
        assert!(!outside.join("reviews").exists());

        fs::remove_file(checkout.join(".muster")).unwrap();
        fs::create_dir(checkout.join(".muster")).unwrap();
        symlink(&outside, checkout.join(".muster/reviews")).unwrap();
        let error = export_review_for_checkout(
            &database,
            checkout_id,
            &places,
            "2026-03-14",
            "2026-03-14-1532",
            "review",
        )
        .unwrap_err();
        assert_eq!(error.code, IpcErrorCode::PathOutsideCheckout);
        assert!(fs::read_dir(&outside).unwrap().next().is_none());

        fs::remove_file(checkout.join(".muster/reviews")).unwrap();
        fs::remove_dir(checkout.join(".muster")).unwrap();
        symlink(&internal, checkout.join(".muster")).unwrap();
        let filename = export_review_for_checkout(
            &database,
            checkout_id,
            &places,
            "2026-03-14",
            "2026-03-14-1532",
            "review",
        )
        .unwrap();
        assert_eq!(filename.origin, ReviewDocumentOrigin::ReviewWorkdir);
        assert_eq!(
            fs::read_to_string(internal.join("reviews").join(filename.path)).unwrap(),
            format!("{EXPORTED_REVIEW_MARKER}review")
        );
    }

    /// What the dialog reports about a folder is the folder before anything is in it: it asks
    /// where reviews go in order to change where they go.
    #[test]
    fn a_review_folder_that_is_not_there_yet_holds_nothing_rather_than_failing() {
        let temp = tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        fs::create_dir_all(&checkout).unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &checkout).unwrap();

        let folder = review_folder(
            &database,
            &state.repos[0].checkouts[0].id,
            &places_in(temp.path(), REVIEW_STORAGE_DEFAULT),
        )
        .unwrap();
        assert_eq!(folder.files, 0);
        assert_eq!(folder.bytes, 0);
        assert_eq!(folder.storage, REVIEW_STORAGE_DEFAULT);
        assert!(folder.path.ends_with(".muster/tmp/reviews"));
    }

    /// Clearing removes only marked app exports: an ordinary note or a timestamp-shaped file is
    /// not evidence of ownership. Count and delete use the same selector.
    #[test]
    fn clearing_a_review_folder_removes_the_exports_and_leaves_the_rest() {
        let temp = tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&checkout).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &checkout).unwrap();
        let places = places_in(temp.path(), REVIEW_STORAGE_DEFAULT);
        let checkout_id = &state.repos[0].checkouts[0].id;
        let root = review_root_for(&database, checkout_id, &places).unwrap();

        let first =
            export_review_markdown(&root, "2026-03-14", "2026-03-14-1532", "first").unwrap();
        export_review_markdown(&root, "2026-03-14", "2026-03-14-1532", "second").unwrap();
        let original = format!("{EXPORTED_REVIEW_MARKER}first");
        let edited = format!("{EXPORTED_REVIEW_MARKER}edited");
        write(
            &database,
            checkout_id,
            "review-default",
            Path::new(&first),
            &edited,
            &original,
            Some(&places),
        )
        .unwrap();
        // Valid export-like names, a directory and a symlink are not owned without the marker.
        fs::write(root.join("2026-03-14-9999.md"), "not an hour").unwrap();
        fs::write(root.join("2026-03-14-1540-4.md"), "manual").unwrap();
        fs::write(root.join("notes.txt"), "mine").unwrap();
        fs::create_dir_all(root.join("2026-03-14-1600.md")).unwrap();
        fs::write(
            outside.join("export.md"),
            format!("{EXPORTED_REVIEW_MARKER}linked"),
        )
        .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.join("export.md"), root.join("2026-03-14-1601.md"))
            .unwrap();

        let counted = review_folder(&database, checkout_id, &places).unwrap();
        assert_eq!(counted.files, 2);
        assert_eq!(
            counted.bytes,
            edited.len() as u64 + (EXPORTED_REVIEW_MARKER.len() + "second".len()) as u64
        );
        let cleared = clear_review_folder(&database, checkout_id, &places).unwrap();
        assert_eq!(cleared.files, 2);
        assert_eq!(cleared.bytes, counted.bytes);

        let left: Vec<String> = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        for name in [
            "notes.txt",
            "2026-03-14-9999.md",
            "2026-03-14-1540-4.md",
            "2026-03-14-1600.md",
        ] {
            assert!(left.iter().any(|left| left == name), "{name} is gone");
        }
        #[cfg(unix)]
        assert!(left.iter().any(|left| left == "2026-03-14-1601.md"));
        let empty = review_folder(&database, checkout_id, &places).unwrap();
        assert_eq!((empty.files, empty.bytes), (0, 0));
    }

    /// Notes inside a working directory are the project's, not a cache of this app's, so the
    /// command refuses rather than trusting the button that is not drawn.
    #[test]
    fn reviews_inside_a_working_directory_are_refused_by_the_clear() {
        let temp = tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        fs::create_dir_all(&checkout).unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &checkout).unwrap();
        let places = reviews_in_the_checkout();
        let checkout_id = &state.repos[0].checkouts[0].id;
        let root = review_root_for(&database, checkout_id, &places).unwrap();
        export_review_markdown(&root, "2026-03-14", "2026-03-14-1532", "first").unwrap();

        assert!(clear_review_folder(&database, checkout_id, &places).is_err());
        assert!(root.join("2026-03-14-1532.md").is_file());
    }

    #[test]
    fn two_exports_in_the_same_minute_do_not_overwrite_each_other() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("reviews");
        let first =
            export_review_markdown(&root, "2026-03-14", "2026-03-14-1532", "first").unwrap();
        let first_bytes = fs::read(root.join(&first)).unwrap();
        let second =
            export_review_markdown(&root, "2026-03-14", "2026-03-14-1532", "second").unwrap();

        assert_eq!(first, "2026-03-14-1532.md");
        assert_eq!(second, "2026-03-14-1532-2.md");
        assert_eq!(
            fs::read(root.join(&first)).unwrap(),
            [EXPORTED_REVIEW_MARKER.as_bytes(), b"first"].concat()
        );
        assert_eq!(
            fs::read_to_string(root.join(second)).unwrap(),
            format!("{EXPORTED_REVIEW_MARKER}second")
        );
        assert_eq!(fs::read(root.join(first)).unwrap(), first_bytes);
    }

    #[test]
    fn a_review_export_with_a_malformed_date_or_timestamp_is_refused() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("reviews");
        let too_long = "x".repeat(4097);
        let invalid = [
            ("2026-3-14", "2026-03-14-1532"),
            ("2026-03-14", "2026-03-14-999"),
            ("../etc/passwd", "2026-03-14-1532"),
            (too_long.as_str(), "2026-03-14-1532"),
        ];
        for (date, timestamp) in invalid {
            let error = export_review_markdown(&root, date, timestamp, "review").unwrap_err();
            assert!(matches!(error.code, IpcErrorCode::InvalidPath));
            assert!(!root.exists());
        }
        assert!(
            export_review_markdown(
                &root,
                "2026-03-14",
                "2026-03-14-1532",
                &"x".repeat(MAX_ROUND_PROMPT_BYTES + 1),
            )
            .unwrap_err()
            .code
                == IpcErrorCode::FileTooLarge
        );
        assert!(!root.exists());
    }

    #[test]
    fn gitignored_files_are_listed_and_marked_in_git_checkouts_only() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        git(&root, &["init"]);
        fs::write(root.join(".gitignore"), "ignored.txt\nignored-dir/\n").unwrap();
        fs::write(root.join("visible.txt"), "visible").unwrap();
        fs::write(root.join("ignored.txt"), "ignored").unwrap();
        fs::create_dir(root.join("ignored-dir")).unwrap();
        fs::write(root.join("ignored-dir/hidden.txt"), "ignored").unwrap();
        let database = db(temp.path());

        let state = workspace::register_folder(&database, &root).unwrap();
        let entries = list(&database, &state.repos[0].checkouts[0].id, ".")
            .unwrap()
            .entries;
        let ignored = |name: &str| {
            entries
                .iter()
                .find(|entry| entry.name == name)
                .map(|entry| entry.ignored)
        };

        // Ignored files stay in the tree so it can show them a step quieter, and the flag says
        // which ones those are. `.git` is the one thing never listed.
        assert!(names(&entries).contains(&"visible.txt"));
        assert!(!names(&entries).contains(&".git"));
        assert_eq!(ignored("visible.txt"), Some(false));
        assert_eq!(ignored("ignored.txt"), Some(true));
        assert_eq!(ignored("ignored-dir"), Some(true));
    }

    #[test]
    fn a_plain_folder_marks_nothing_as_ignored() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("plain");
        fs::create_dir_all(&root).unwrap();
        // A .gitignore in a folder that is not a checkout is just a file: nothing honors it.
        fs::write(root.join(".gitignore"), "*.log\n").unwrap();
        fs::write(root.join("keep.log"), "kept").unwrap();
        let database = db(temp.path());

        let state = workspace::register_folder(&database, &root).unwrap();
        let entries = list(&database, &state.repos[0].checkouts[0].id, ".")
            .unwrap()
            .entries;

        assert!(entries.iter().all(|entry| !entry.ignored));
        assert!(names(&entries).contains(&"keep.log"));
        // And `.git` is a plain directory here, with nothing to hide it.
        assert!(!names(&entries).contains(&".git"));
    }

    fn names(entries: &[FileEntry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.name.as_str()).collect()
    }

    #[test]
    fn limits_text_reads_and_rejects_binary_content() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("plain");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("large.txt"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
        fs::write(root.join("binary.bin"), [0, 1, 2]).unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;

        let large = read_checkout(&database, checkout_id, "large.txt").unwrap_err();
        let binary = read_checkout(&database, checkout_id, "binary.bin").unwrap_err();

        assert!(matches!(large.code, IpcErrorCode::FileTooLarge));
        assert!(matches!(binary.code, IpcErrorCode::BinaryFile));
    }

    #[test]
    fn probe_validates_checkout_before_reading_external_preview_content() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("plain");
        let external = temp.path().join("outside.txt");
        fs::create_dir_all(&root).unwrap();
        fs::write(&external, [0, 1, 2]).unwrap();
        let database = db(temp.path());
        workspace::register_folder(&database, &root).unwrap();

        let error = probe(
            &database,
            "unknown-checkout",
            external.to_str().unwrap(),
            Some(root.to_str().unwrap()),
        )
        .unwrap_err();

        assert_eq!(error.code, IpcErrorCode::InvalidCheckout);
    }

    #[test]
    fn probe_confirms_only_text_files_this_checkout_holds() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("plain");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("notes.txt"), "hello").unwrap();
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("nested/deep.rs"), "fn main() {}").unwrap();
        fs::write(root.join("binary.bin"), [0, 1, 2]).unwrap();
        fs::write(root.join("large.txt"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
        git(&root, &["init", "--quiet"]);
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;

        assert_eq!(
            probe(&database, checkout_id, "notes.txt", None)
                .unwrap()
                .map(|hit| hit.path),
            Some("notes.txt".to_string())
        );

        // A terminal prints the same file three ways and all three name it.
        for printed in [
            "./notes.txt",
            "nested/deep.rs",
            root.join("notes.txt").to_string_lossy().as_ref(),
        ] {
            assert!(
                probe(&database, checkout_id, printed, None)
                    .unwrap()
                    .is_some(),
                "{printed} should name a file the preview can open"
            );
        }

        // Everything the preview would refuse to draw, the probe refuses to underline.
        for printed in [
            "missing.txt",
            "binary.bin",
            "large.txt",
            "nested",
            "../escape.txt",
            "/etc/hosts",
            ".git/config",
            "~/notes.txt",
            "",
        ] {
            assert!(
                probe(&database, checkout_id, printed, None)
                    .unwrap()
                    .is_none(),
                "{printed} should not be offered as a file"
            );
        }
    }

    /// The bug this exists for: `ls` prints bare names, and a bare name is relative to the
    /// directory the command ran in. Without the terminal's own directory they were read against
    /// the root of the checkout, so nothing below the root could ever open.
    #[test]
    fn probe_reads_a_bare_name_against_the_directory_the_terminal_is_in() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("plain");
        fs::create_dir_all(root.join("src/lib")).unwrap();
        fs::write(root.join("LICENSE"), "terms").unwrap();
        fs::write(root.join("src/lib/muster-terminal.ts"), "export {};").unwrap();
        fs::write(root.join("src/lib/LICENSE"), "terms").unwrap();
        fs::write(root.join("notes.txt"), "hello").unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;
        let below = root.join("src/lib");

        for (printed, expected) in [
            ("muster-terminal.ts", "src/lib/muster-terminal.ts"),
            ("./muster-terminal.ts", "src/lib/muster-terminal.ts"),
            ("../lib/muster-terminal.ts", "src/lib/muster-terminal.ts"),
            // A name with no extension and no directory on it, which is what half of an `ls` is.
            ("LICENSE", "src/lib/LICENSE"),
        ] {
            assert_eq!(
                probe(
                    &database,
                    checkout_id,
                    printed,
                    Some(below.to_string_lossy().as_ref())
                )
                .unwrap()
                .map(|hit| hit.path),
                Some(expected.to_string()),
                "{printed} should be read from the terminal's own directory"
            );
        }

        // A terminal in the root resolves the same bare names against the root, which is where it
        // starts, so an `ls` there opens too — including the one name with no extension in it,
        // which is what an `ls` of a checkout is full of.
        assert_eq!(
            probe(
                &database,
                checkout_id,
                "LICENSE",
                Some(root.to_string_lossy().as_ref())
            )
            .unwrap()
            .map(|hit| hit.path),
            Some("LICENSE".to_string())
        );

        // The directory is a hint about where to look and nothing else: `..` out of it, a file
        // beside it, Git's own directory and a directory rather than a file are all still refused.
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), "[core]").unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "not yours").unwrap();
        for (directory, printed) in [
            (root.join("src"), "../outside/secret.txt"),
            (below.clone(), "secret.txt"),
            (below.clone(), ".."),
            (root.clone(), ".git/config"),
        ] {
            assert!(
                probe(
                    &database,
                    checkout_id,
                    printed,
                    Some(directory.to_string_lossy().as_ref())
                )
                .unwrap()
                .is_none(),
                "{printed} should not be offered as a file of this checkout"
            );
        }

        // A directory this side cannot join onto is not a base, so the bare name falls back to the
        // root rather than to something guessed. A terminal that reported nothing at all is the
        // same case and answers the same way.
        for reported in [Some("relative/"), Some(""), None] {
            assert_eq!(
                probe(&database, checkout_id, "notes.txt", reported)
                    .unwrap()
                    .map(|hit| hit.path),
                Some("notes.txt".to_string()),
                "{reported:?} is no base, so the name is read from the root"
            );
        }
    }

    #[test]
    fn writes_text_only_when_the_expected_content_still_matches() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("plain");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("notes.txt"), "before").unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;

        write_checkout(&database, checkout_id, "notes.txt", "after", "before").unwrap();
        assert_eq!(fs::read_to_string(root.join("notes.txt")).unwrap(), "after");

        fs::write(root.join("notes.txt"), "external change").unwrap();
        let changed = write_checkout(&database, checkout_id, "notes.txt", "another edit", "after")
            .unwrap_err();
        assert!(matches!(changed.code, IpcErrorCode::FileChanged));
        assert_eq!(
            fs::read_to_string(root.join("notes.txt")).unwrap(),
            "external change"
        );
    }

    #[test]
    fn concurrent_writes_with_same_expected_content_only_one_wins() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("plain");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("notes.txt"), "before").unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let checkout_id = state.repos[0].checkouts[0].id.clone();
        let barrier = Arc::new(Barrier::new(3));

        let writers: Vec<_> = ["first edit", "second edit"]
            .into_iter()
            .map(|content| {
                let database = database.clone();
                let checkout_id = checkout_id.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let result =
                        write_checkout(&database, &checkout_id, "notes.txt", content, "before");
                    (content, result)
                })
            })
            .collect();
        barrier.wait();

        let mut winners = Vec::new();
        let mut conflicts = 0;
        for writer in writers {
            let (content, result) = writer.join().unwrap();
            match result {
                Ok(()) => winners.push(content),
                Err(error) => {
                    assert!(matches!(error.code, IpcErrorCode::FileChanged));
                    conflicts += 1;
                }
            }
        }
        assert_eq!(winners.len(), 1);
        assert_eq!(conflicts, 1);
        assert_eq!(
            fs::read_to_string(root.join("notes.txt")).unwrap(),
            winners[0]
        );
    }

    #[test]
    fn rejects_unsafe_file_writes() {
        #[cfg(unix)]
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(root.join("notes.txt"), "safe").unwrap();
        fs::write(outside.join("secret.txt"), "secret").unwrap();
        #[cfg(unix)]
        symlink(&outside, root.join("escape")).unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;

        assert!(matches!(
            write_checkout(
                &database,
                checkout_id,
                "../outside/secret.txt",
                "x",
                "secret"
            )
            .unwrap_err()
            .code,
            IpcErrorCode::InvalidPath
        ));
        assert!(matches!(
            write_checkout(&database, checkout_id, "notes.txt", "bad\0text", "safe")
                .unwrap_err()
                .code,
            IpcErrorCode::BinaryFile
        ));
        #[cfg(unix)]
        assert!(matches!(
            write_checkout(
                &database,
                checkout_id,
                "escape/secret.txt",
                "changed",
                "secret"
            )
            .unwrap_err()
            .code,
            IpcErrorCode::PathOutsideCheckout
        ));
    }

    #[test]
    fn a_save_keeps_the_files_permissions_and_refuses_a_read_only_one() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("script.sh"), "echo one\n").unwrap();
        fs::write(root.join("locked.txt"), "locked\n").unwrap();
        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;

        // A read-only file is refused rather than quietly made writable by the replacement, and
        // refusing it leaves the bytes on disk untouched.
        let mut locked = fs::metadata(root.join("locked.txt")).unwrap().permissions();
        locked.set_readonly(true);
        fs::set_permissions(root.join("locked.txt"), locked).unwrap();
        assert!(matches!(
            write_checkout(
                &database,
                checkout_id,
                "locked.txt",
                "changed\n",
                "locked\n"
            )
            .unwrap_err()
            .code,
            IpcErrorCode::PermissionDenied
        ));
        assert_eq!(
            fs::read_to_string(root.join("locked.txt")).unwrap(),
            "locked\n"
        );

        // And a file that is writable keeps the mode it arrived with, so saving a script does not
        // quietly make it unrunnable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(root.join("script.sh"), fs::Permissions::from_mode(0o755)).unwrap();
            write_checkout(
                &database,
                checkout_id,
                "script.sh",
                "echo two\n",
                "echo one\n",
            )
            .unwrap();
            let mode = fs::metadata(root.join("script.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
            assert_eq!(
                fs::read_to_string(root.join("script.sh")).unwrap(),
                "echo two\n"
            );
        }
    }

    #[test]
    fn markdown_images_are_contained_raster_files_with_strict_limits() {
        #[cfg(unix)]
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let root = temp.path().join("repo");
        let outside = temp.path().join("outside");
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::create_dir_all(root.join("images")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(root.join("docs/readme.md"), "![image](../images/pixel.png)").unwrap();
        fs::write(
            root.join("images/pixel.png"),
            b"\x89PNG\r\n\x1a\nimage-data",
        )
        .unwrap();
        fs::write(
            root.join("images/pixel photo.png"),
            b"\x89PNG\r\n\x1a\nimage-data",
        )
        .unwrap();
        fs::write(
            root.join("images/vector.svg"),
            "<svg onload='alert(1)'></svg>",
        )
        .unwrap();
        fs::write(
            root.join("images/large.png"),
            [
                b"\x89PNG\r\n\x1a\n".as_slice(),
                &vec![0; 2 * 1024 * 1024][..],
            ]
            .concat(),
        )
        .unwrap();
        fs::write(outside.join("secret.png"), b"\x89PNG\r\n\x1a\nsecret").unwrap();
        #[cfg(unix)]
        symlink(outside.join("secret.png"), root.join("images/escape.png")).unwrap();

        let database = db(temp.path());
        let state = workspace::register_folder(&database, &root).unwrap();
        let checkout_id = &state.repos[0].checkouts[0].id;
        let image = read_markdown_image(
            &database,
            checkout_id,
            "docs/readme.md",
            "../images/pixel.png",
        )
        .unwrap();
        assert_eq!(image.mime_type, "image/png");
        assert_eq!(image.size_bytes, 18);
        assert!(!image.data_base64.is_empty());
        assert!(read_markdown_image(
            &database,
            checkout_id,
            "docs/readme.md",
            "../images/pixel%20photo.png",
        )
        .is_ok());

        let script_url = read_markdown_image(
            &database,
            checkout_id,
            "docs/readme.md",
            "javascript:alert(1)",
        )
        .unwrap_err();
        assert!(matches!(script_url.code, IpcErrorCode::InvalidPath));

        let unsupported = read_markdown_image(
            &database,
            checkout_id,
            "docs/readme.md",
            "../images/vector.svg",
        )
        .unwrap_err();
        assert!(matches!(unsupported.code, IpcErrorCode::BinaryFile));

        let too_large = read_markdown_image(
            &database,
            checkout_id,
            "docs/readme.md",
            "../images/large.png",
        )
        .unwrap_err();
        assert!(matches!(too_large.code, IpcErrorCode::FileTooLarge));

        #[cfg(unix)]
        {
            let escaped = read_markdown_image(
                &database,
                checkout_id,
                "docs/readme.md",
                "../images/escape.png",
            )
            .unwrap_err();
            assert!(matches!(escaped.code, IpcErrorCode::PathOutsideCheckout));
        }
    }
}
