//! The window's own geometry, as it is left and found again.
//!
//! It is the application's state and not the workspace's: the window's frame
//! is the window's, and the workdirs on the panel are the workspace's. So it
//! lives in its own file, beside the workspace file, and the workspace file's
//! shape does not change because the window was resized.
//!
//! The file carries no version: one shape, changed in place, so the shape
//! itself is the whole contract and a file this build cannot read is refused
//! rather than read through or migrated.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// The cap on the file: a geometry is a handful of numbers.
const STATE_CAP: u64 = 64 * 1024;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

/// The window's own size, and whether it was maximized.
///
/// The size is the **windowed** size: the size the window had while it was
/// not maximized. A maximized window's own bounds are the screen's, so the
/// size it had before it was maximized is the one kept, and a window that is
/// left maximized comes back maximized and comes back to that size when it is
/// un-maximized.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WindowGeometry {
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

impl Default for WindowGeometry {
    fn default() -> Self {
        Self {
            width: 1280.0,
            height: 800.0,
            maximized: false,
        }
    }
}

impl WindowGeometry {
    /// The size has to be one a window can be drawn at.
    pub fn validate(&self) -> Result<(), String> {
        if !self.width.is_finite() || !(100.0..=20_000.0).contains(&self.width) {
            return Err("window width must be between 100 and 20000".into());
        }
        if !self.height.is_finite() || !(100.0..=20_000.0).contains(&self.height) {
            return Err("window height must be between 100 and 20000".into());
        }
        Ok(())
    }
}

/// Where the window's own geometry lives.
pub fn state_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is unavailable for the window state")?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        return Err("HOME must be an absolute directory".into());
    }
    Ok(home.join(".muster/native-window.json"))
}

/// Read the geometry the window was left at.
///
/// A file that is not there is a window that was never sized: the default.
/// A file this build cannot read, a symlink, a file that is not a regular file
/// and a file past the cap are refused.
pub fn load(path: &Path) -> Result<WindowGeometry, String> {
    let Some(bytes) = read(path)? else {
        return Ok(WindowGeometry::default());
    };
    let geometry: WindowGeometry = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid window state {}: {error}", path.display()))?;
    geometry.validate()?;
    Ok(geometry)
}

/// Keep the geometry, so the next launch opens the window at it.
///
/// The bytes are written to a private temporary file in the same directory,
/// synced, and renamed into place: a reader never sees half a file, and a
/// crash never leaves one behind. A file this build cannot read is never
/// written over.
pub fn save(path: &Path, geometry: &WindowGeometry) -> Result<(), String> {
    geometry.validate()?;
    let bytes = serde_json::to_vec_pretty(geometry)
        .map_err(|error| format!("could not encode the window state: {error}"))?;
    if bytes.len() as u64 > STATE_CAP {
        return Err("the window state exceeds the 64 KiB limit".into());
    }
    // A file this build cannot read is not this build's file, and it is not
    // written over.
    load(path)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(parent)
        .map_err(|error| format!("could not create the window state directory: {error}"))?;
    let name = path
        .file_name()
        .ok_or("the window state path has no filename")?;
    let temp = parent.join(format!(
        ".{}.{}-{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp)
        .map_err(|error| format!("could not create the window state temporary file: {error}"))?;
    let written = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        File::open(parent)?.sync_all()
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temp);
        return Err(format!(
            "could not save the window state {}: {error}",
            path.display()
        ));
    }
    Ok(())
}

/// Read the file, within the cap.
///
/// `None` is a file that is not there at all. A symlink, a file that is not a
/// regular file and a file past the cap are refused: the file is the app's
/// own, and something else in its place is not the app's file.
fn read(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "could not inspect the window state {}: {error}",
                path.display()
            ))
        }
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err("the window state must be a regular file, not a symlink".into())
        }
        Ok(_) => {}
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|error| {
        format!(
            "could not read the window state {}: {error}",
            path.display()
        )
    })?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect the window state: {error}"))?;
    if !metadata.is_file() || metadata.len() > STATE_CAP {
        return Err("the window state is not a regular file within the 64 KiB limit".into());
    }
    let mut bytes = Vec::new();
    file.take(STATE_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read the window state: {error}"))?;
    if bytes.len() as u64 > STATE_CAP {
        return Err("the window state exceeds the 64 KiB limit".into());
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn the_window_state_round_trips() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("native-window.json");
        assert_eq!(load(&path).unwrap(), WindowGeometry::default());
        let geometry = WindowGeometry {
            width: 1440.,
            height: 900.,
            maximized: true,
        };
        save(&path, &geometry).unwrap();
        assert_eq!(load(&path).unwrap(), geometry);
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn a_window_state_this_build_cannot_read_is_refused_and_never_written_over() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("native-window.json");
        let old = br#"{"width":1000.0,"height":700.0,"maximized":false,"somethingElse":1}"#;
        fs::write(&path, old).unwrap();
        assert!(load(&path).is_err());
        assert!(save(&path, &WindowGeometry::default()).is_err());
        assert_eq!(fs::read(&path).unwrap(), old);
    }

    #[test]
    fn a_window_state_that_is_a_symlink_is_refused() {
        let temp = tempdir().unwrap();
        let target = temp.path().join("target");
        fs::write(&target, "{}").unwrap();
        let path = temp.path().join("native-window.json");
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(&target, &path).unwrap();
            assert!(load(&path).is_err());
            assert!(save(&path, &WindowGeometry::default()).is_err());
        }
        #[cfg(not(unix))]
        {
            let _ = (target, path);
        }
    }

    #[test]
    fn a_size_the_window_cannot_be_drawn_at_is_refused() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("native-window.json");
        for width in [f32::NAN, f32::INFINITY, 0., 20_001.] {
            let geometry = WindowGeometry {
                width,
                height: 800.,
                maximized: false,
            };
            assert!(save(&path, &geometry).is_err(), "width {width} is refused");
        }
        assert!(!path.exists(), "nothing was written");
    }
}
