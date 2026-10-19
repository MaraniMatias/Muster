//! Starting an interactive Pi with a review as its first prompt.
//!
//! Three things, and nothing that draws: where Pi is, where the prompt it reads
//! is written, and how the prompt is handed to it.
//!
//! The prompt is a *file* rather than an argument because a review is as long as
//! the notes in it, and a command line is the wrong place for that. It is
//! written into a private directory under the app's own folder, and Pi is
//! handed the path as one argument after `--`, so nothing in the path is ever
//! read as an option and nothing in it is ever handed to a shell.
//!
//! The prompt is owned from the moment it is written: a file that no live Pi
//! took is removed, because a review nobody read is not something to leave
//! lying in a private folder.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// The executable Pi is installed as.
const PROGRAM: &str = "pi";

/// Whether a program the operating system named is Pi.
///
/// The name is the OS's, not ours: a process is called `pi` whether it was
/// started as `pi`, as `node` with a changed title, or under any other name
/// that ends up saying `pi` in the same place.
pub fn is_pi(app: &str) -> bool {
    app.eq_ignore_ascii_case(PROGRAM)
}

/// Where the prompts live, under the app's own directory.
const PROMPTS_DIR: &str = "pi/prompts";

/// How long a prompt is kept before it is swept away.
///
/// A prompt is read once, when Pi starts, and is of no use afterwards. It is
/// swept rather than kept because it holds somebody's review, and a review is
/// not something to leave lying around in a file nobody will look at.
const PROMPT_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

/// The name of the file a prompt is written to.
///
/// The shape is fixed and the parts are generated, so a name can be recognised
/// as one of ours and swept without touching a file that is not.
const PROMPT_SUFFIX: &str = ".md";

static NEXT_PROMPT_ID: AtomicU64 = AtomicU64::new(1);

/// The app's own folder, where the private prompts are written.
///
/// `~/.muster`, the same folder the workspace, the preferences and the review
/// rounds live in: everything this app keeps for itself is under one roof.
pub fn app_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .map(|home| home.join(".muster"))
}

/// The executable Pi is installed as, or `None` when this machine has none.
///
/// The search is the shell's: every element of `PATH`, in order, and the first
/// one that holds an executable file called `pi`. A name that is not on the
/// path is not Pi, and a directory called `pi` is not a program.
pub fn resolve_program() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(PROGRAM))
        .find(|candidate| is_executable(candidate))
}

/// Writes the review privately and returns the prompt, which owns the file.
///
/// The directory is created with owner-only permissions and the file is written
/// with owner-only permissions, and a symlink anywhere on the way is refused
/// rather than followed: the prompt is somebody's review, and a link is how a
/// file this writes would end up somewhere it was not meant to be.
///
/// The prompt is returned rather than the path to it, because a path is a
/// claim that somebody else will clean it up, and nobody else knows whether
/// Pi ever read it.
pub fn write_prompt(app_dir: &Path, prompt: &str) -> Result<PromptFile, String> {
    let directory = app_dir.join(PROMPTS_DIR);
    private_dir(app_dir, &directory)?;
    sweep_prompts(&directory)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = format!(
        "{timestamp}-{}-{id}-{PROMPT_SUFFIX}",
        std::process::id(),
        id = NEXT_PROMPT_ID.fetch_add(1, Ordering::Relaxed)
    );
    let path = directory.join(name);
    // The file is created with its own permissions rather than written and
    // then tightened: a review that was briefly readable by every account on
    // the machine was readable, however briefly.
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .map_err(|error| format!("the review could not be written for Pi: {error}"))?;
    file.write_all(prompt.as_bytes())
        .map_err(|error| format!("the review could not be written for Pi: {error}"))?;
    set_private_file(&path)?;
    Ok(PromptFile::new(path))
}

/// A prompt that is on disk, and is removed unless a live Pi takes it.
///
/// The prompt is written before the program that reads it exists, and that
/// program may never exist: the write may finish after the window is gone,
/// the terminal may fail to start, the app may refuse the session. This owns
/// the file from the moment it is written, so a prompt nobody read is removed
/// rather than left lying in a private folder holding somebody's review.
pub struct PromptFile {
    path: PathBuf,
    taken: bool,
}

impl PromptFile {
    /// Takes ownership of a prompt that has been written.
    fn new(path: PathBuf) -> Self {
        Self { path, taken: false }
    }

    /// Where the prompt is, for the program that is about to read it.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Hands the prompt to a live Pi, which is about to read it.
    ///
    /// The file stays: Pi reads it when it starts, and a prompt removed
    /// before that is a review Pi never saw.
    pub fn hand_over(mut self) {
        self.taken = true;
    }
}

impl Drop for PromptFile {
    fn drop(&mut self) {
        if !self.taken {
            remove_prompt(&self.path);
        }
    }
}

/// Whether a Pi process working in `working_directory` is working in
/// `checkout`.
///
/// Fail-closed: a process whose directory the operating system will not say,
/// or a checkout that cannot be resolved, protects the checkout. A worktree
/// removed under a running Pi is a worktree somebody loses work in, and the
/// cost of refusing is one worktree that stays.
pub fn protects_checkout(working_directory: Option<&Path>, checkout: &Path) -> bool {
    let Some(working_directory) = working_directory.and_then(|path| path.canonicalize().ok())
    else {
        return true;
    };
    checkout
        .canonicalize()
        .map(|checkout| working_directory.starts_with(checkout))
        .unwrap_or(true)
}

/// Removes a prompt when Pi could not be started with it.
fn remove_prompt(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Ends Pi's option parsing and supplies the file as its initial interactive
/// prompt.
///
/// The two arguments, and not one string: `--` is what tells Pi that the next
/// argument is a prompt rather than an option, and the path is one argument so
/// that nothing in it is ever split, quoted or handed to a shell.
pub fn arguments(prompt: &Path) -> Vec<String> {
    vec!["--".into(), format!("@{}", prompt.to_string_lossy())]
}

/// Removes the prompts this build wrote more than a day ago.
///
/// Only files whose names are the shape this build generates, and only regular
/// files: a directory or a link that happens to carry such a name is left
/// alone, because removing it would be removing something that is not ours.
fn sweep_prompts(directory: &Path) -> Result<(), String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("could not read {}: {error}", directory.display()))?;
    let now = SystemTime::now();
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "could not read an entry in {}: {error}",
                directory.display()
            )
        })?;
        if !is_generated_prompt_name(&entry.file_name()) {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
        if !metadata.file_type().is_file() {
            continue;
        }
        let modified = metadata
            .modified()
            .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
        let expired = now
            .duration_since(modified)
            .is_ok_and(|age| age > PROMPT_RETENTION);
        if expired {
            fs::remove_file(&path).map_err(|error| {
                format!(
                    "could not remove expired prompt {}: {error}",
                    path.display()
                )
            })?;
        }
    }
    Ok(())
}

/// Whether a name is one this build writes.
///
/// The three numbers are the timestamp, the process and the counter, and the
/// extension is the one this build gives a prompt. A name that is not this
/// shape is somebody else's file.
fn is_generated_prompt_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name
        .to_str()
        .and_then(|name| name.strip_suffix(PROMPT_SUFFIX))
    else {
        return false;
    };
    let mut parts = name.split('-');
    (0..3).all(|_| {
        parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    }) && parts.next().is_none()
}

/// The private directory the prompts live in, created if it is not there.
fn private_dir(app_dir: &Path, directory: &Path) -> Result<(), String> {
    match fs::symlink_metadata(app_dir) {
        Ok(metadata) => validate_directory(app_dir, &metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(app_dir)
                .map_err(|error| format!("could not create {}: {error}", app_dir.display()))?;
            ensure_directory(app_dir)?;
        }
        Err(error) => return Err(format!("could not inspect {}: {error}", app_dir.display())),
    }
    ensure_directory(&app_dir.join("pi"))?;
    ensure_directory(directory)?;
    set_private_directory(directory)
}

/// Creates a directory that is not there yet, and refuses one that is a link.
fn ensure_directory(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => return validate_directory(path, &metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("could not inspect {}: {error}", path.display())),
    }
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("could not create {}: {error}", path.display())),
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
    validate_directory(path, &metadata)
}

/// A directory that is a link, or not a directory, is refused.
fn validate_directory(path: &Path, metadata: &fs::Metadata) -> Result<(), String> {
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "refusing symlink in private prompt path: {}",
            path.display()
        ));
    }
    if !metadata.is_dir() {
        return Err(format!(
            "private prompt path is not a directory: {}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_directory(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("could not secure {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_private_directory(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("could not secure {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_private_file(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Whether a path is a file that can be run.
fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::Path,
        time::{Duration, SystemTime},
    };

    use tempfile::tempdir;

    use super::{arguments, remove_prompt, write_prompt};

    #[test]
    fn a_review_is_private_and_handed_over_as_one_argument() {
        let app_dir = tempdir().unwrap();
        let prompt = write_prompt(app_dir.path(), "Review this change").unwrap();
        let path = prompt.path().to_path_buf();

        assert!(path.is_absolute());
        assert_eq!(fs::read_to_string(&path).unwrap(), "Review this change");
        assert_eq!(
            arguments(&path),
            vec!["--".to_string(), format!("@{}", path.display())]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }

        remove_prompt(&path);
        assert!(!Path::new(&path).exists());
    }

    /// A prompt is removed unless a live Pi takes it, and a Pi that is
    /// running is about to read the file.
    #[test]
    fn a_prompt_nobody_took_is_removed_and_one_a_live_pi_took_is_kept() {
        let app_dir = tempdir().unwrap();

        let taken = write_prompt(app_dir.path(), "A review Pi will read").unwrap();
        let taken_path = taken.path().to_path_buf();
        taken.hand_over();
        assert!(
            taken_path.exists(),
            "a prompt Pi is about to read is not removed before Pi reads it"
        );

        let abandoned = write_prompt(app_dir.path(), "A review nobody will read").unwrap();
        let abandoned_path = abandoned.path().to_path_buf();
        drop(abandoned);
        assert!(
            !abandoned_path.exists(),
            "a prompt nobody took is removed rather than left on disk"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_on_the_way_to_the_prompts_is_refused_before_anything_outside_is_touched() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let app_dir = tempdir().unwrap();
        let external = tempdir().unwrap();
        let unrelated = external.path().join("unrelated.md");
        fs::write(&unrelated, "keep").unwrap();
        fs::set_permissions(external.path(), fs::Permissions::from_mode(0o755)).unwrap();
        symlink(external.path(), app_dir.path().join("pi")).unwrap();

        assert!(write_prompt(app_dir.path(), "secret").is_err());
        assert_eq!(fs::read_to_string(&unrelated).unwrap(), "keep");
        assert_eq!(
            fs::metadata(external.path()).unwrap().permissions().mode() & 0o777,
            0o755
        );

        let prompt_app = tempdir().unwrap();
        let external_prompts = tempdir().unwrap();
        let external_prompt = external_prompts.path().join("1-2-3.md");
        fs::write(&external_prompt, "keep").unwrap();
        fs::set_permissions(external_prompts.path(), fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir(prompt_app.path().join("pi")).unwrap();
        symlink(
            external_prompts.path(),
            prompt_app.path().join("pi/prompts"),
        )
        .unwrap();
        assert!(write_prompt(prompt_app.path(), "secret").is_err());
        assert_eq!(fs::read_to_string(&external_prompt).unwrap(), "keep");
        assert_eq!(
            fs::metadata(external_prompts.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }

    #[test]
    fn the_sweep_removes_only_expired_prompts_this_build_wrote() {
        let app_dir = tempdir().unwrap();
        let directory = app_dir.path().join("pi/prompts");
        fs::create_dir_all(&directory).unwrap();
        let expired = directory.join("1-2-3.md");
        fs::write(&expired, "old prompt").unwrap();
        let old = SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
        fs::File::open(&expired)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(old))
            .unwrap();
        let unrelated = directory.join("notes.md");
        fs::write(&unrelated, "keep").unwrap();
        let generated_directory = directory.join("4-5-6.md");
        fs::create_dir(&generated_directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(&unrelated, directory.join("7-8-9.md")).unwrap();
        }

        write_prompt(app_dir.path(), "new prompt").unwrap();

        assert!(!expired.exists());
        assert_eq!(fs::read_to_string(&unrelated).unwrap(), "keep");
        assert!(generated_directory.is_dir());
        #[cfg(unix)]
        assert!(fs::symlink_metadata(directory.join("7-8-9.md"))
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn a_process_protects_the_checkout_it_is_working_in_and_fails_closed() {
        let temp = tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        let nested = checkout.join("nested");
        fs::create_dir_all(&nested).unwrap();

        // A process inside the checkout is working in it.
        assert!(super::protects_checkout(Some(&nested), &checkout));
        assert!(super::protects_checkout(Some(&checkout), &checkout));
        // A process somewhere else is not.
        assert!(!super::protects_checkout(Some(temp.path()), &checkout));
        // A process whose directory the operating system will not say is
        // treated as one that is: a worktree that stays is a nuisance, and a
        // worktree that goes under a running process is a loss.
        assert!(super::protects_checkout(None, &checkout));
        assert!(super::protects_checkout(
            Some(&checkout.join("gone")),
            &checkout
        ));
    }
}
