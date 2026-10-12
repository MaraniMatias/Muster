//! Starting interactive Pi terminals with private review prompts.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::services::executable::find_executable;
use crate::services::files::atomic_write;

const PROGRAM: &str = "pi";
const PROMPTS_DIR: &str = "pi/prompts";
const PROMPT_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
static NEXT_PROMPT_ID: AtomicU64 = AtomicU64::new(1);

/// The executable Pi is installed as, or None when this machine has none.
pub fn resolve_program() -> Option<PathBuf> {
    find_executable(PROGRAM)
}

/// Writes the review privately and returns the absolute path Pi can read as an initial @file prompt.
pub fn write_prompt(app_dir: &Path, prompt: &str) -> Result<PathBuf, String> {
    let directory = app_dir.join(PROMPTS_DIR);
    private_dir(app_dir, &directory)?;
    sweep_prompts(&directory)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = format!(
        "{timestamp}-{}-{}.md",
        std::process::id(),
        NEXT_PROMPT_ID.fetch_add(1, Ordering::Relaxed)
    );
    let path = directory.join(name);
    atomic_write(&path, prompt.as_bytes(), private_file())
        .map_err(|error| format!("the review could not be written for Pi: {}", error.message))?;
    Ok(path)
}

/// Removes a prompt when Pi could not be started with it.
pub fn remove_prompt(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Ends Pi option parsing and supplies the file as its initial interactive prompt.
pub fn arguments(prompt: &Path) -> Vec<String> {
    vec!["--".into(), format!("@{}", prompt.to_string_lossy())]
}

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

fn is_generated_prompt_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str().and_then(|name| name.strip_suffix(".md")) else {
        return false;
    };
    let mut parts = name.split('-');
    (0..3).all(|_| {
        parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    }) && parts.next().is_none()
}

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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("could not secure {}: {error}", directory.display()))?;
    }
    Ok(())
}

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
fn private_file() -> fs::Permissions {
    use std::os::unix::fs::PermissionsExt;
    fs::Permissions::from_mode(0o600)
}

#[cfg(not(unix))]
fn private_file() -> fs::Permissions {
    fs::Permissions::default()
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
    fn review_prompt_is_private_and_passed_as_an_at_file() {
        let app_dir = tempdir().unwrap();
        let prompt = write_prompt(app_dir.path(), "Review this change").unwrap();

        assert!(prompt.is_absolute());
        assert_eq!(
            std::fs::read_to_string(&prompt).unwrap(),
            "Review this change"
        );
        assert_eq!(
            arguments(&prompt),
            vec!["--".to_string(), format!("@{}", prompt.display())]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&prompt).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(prompt.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }

        remove_prompt(&prompt);
        assert!(!Path::new(&prompt).exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_prompt_ancestors_are_rejected_before_external_files_are_touched() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let app_dir = tempdir().unwrap();
        let external = tempdir().unwrap();
        let unrelated = external.path().join("unrelated.md");
        fs::write(&unrelated, "keep").unwrap();
        fs::set_permissions(external.path(), fs::Permissions::from_mode(0o755)).unwrap();
        symlink(external.path(), app_dir.path().join("pi")).unwrap();

        assert!(write_prompt(app_dir.path(), "secret").is_err());
        assert_eq!(fs::read_to_string(unrelated).unwrap(), "keep");
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
        assert_eq!(fs::read_to_string(external_prompt).unwrap(), "keep");
        assert_eq!(
            fs::metadata(external_prompts.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );

        let boundary = tempdir().unwrap();
        let external_app = tempdir().unwrap();
        symlink(external_app.path(), boundary.path().join("app")).unwrap();
        assert!(write_prompt(&boundary.path().join("app"), "secret").is_err());
        assert!(!external_app.path().join("pi").exists());
    }

    #[test]
    fn sweep_removes_only_expired_generated_regular_prompts() {
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
        assert_eq!(fs::read_to_string(unrelated).unwrap(), "keep");
        assert!(generated_directory.is_dir());
        #[cfg(unix)]
        assert!(fs::symlink_metadata(directory.join("7-8-9.md"))
            .unwrap()
            .file_type()
            .is_symlink());
    }
}
