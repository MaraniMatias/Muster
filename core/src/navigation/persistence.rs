use super::*;
use crate::review::ReviewStorage;

/// What the workspace file holds. Every field is required and no field is unknown,
/// and there is no version in the file: one shape, changed in place, so the shape
/// itself is the whole contract and a file this build cannot read is refused rather
/// than read through or migrated.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NavigationState {
    pub roots: Vec<String>,
    pub archived_checkout_ids: Vec<String>,
    #[serde(deserialize_with = "required_selection")]
    pub active_checkout_id: Option<String>,
    pub sidebar_width: f32,
    pub review_storage: ReviewStorage,
    pub panel_layout: PanelLayout,
}

/// Where the Files and Changes panel sits.
///
/// Two shapes, and the file keeps which one the window was left in: the panel
/// is a column of its own at the right of the window, or it sits under the
/// sidebar in the sidebar's own column.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum PanelLayout {
    /// The panel is a column of its own, at the right of the window.
    #[default]
    Side,
    /// The panel sits under the sidebar, in one column with it.
    Below,
}

// An absent field is not the current schema's explicit null selection.
pub(super) fn required_selection<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::deserialize(deserializer)
}

impl Default for NavigationState {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            archived_checkout_ids: Vec::new(),
            active_checkout_id: None,
            sidebar_width: 240.0,
            review_storage: ReviewStorage::Workdir,
            panel_layout: PanelLayout::default(),
        }
    }
}

impl NavigationState {
    pub fn validate(&self) -> Result<(), String> {
        if !self.sidebar_width.is_finite() || !(240.0..=500.0).contains(&self.sidebar_width) {
            return Err("native sidebar width must be between 240 and 500".into());
        }
        if self.roots.len() > 1024 || self.archived_checkout_ids.len() > 8192 {
            return Err("native workspace contains too many directories".into());
        }
        let mut roots = HashSet::new();
        for root in &self.roots {
            validate_path(root)?;
            if !roots.insert(root) {
                return Err("native workspace contains duplicate roots".into());
            }
        }
        let mut archived = HashSet::new();
        for id in &self.archived_checkout_ids {
            validate_checkout_id(id)?;
            if !archived.insert(id)
                || self
                    .roots
                    .iter()
                    .any(|root| checkout_id_for_path(root) == *id)
            {
                return Err(
                    "native workspace contains duplicate or primary archived checkouts".into(),
                );
            }
        }
        if let Some(id) = &self.active_checkout_id {
            validate_checkout_id(id)?;
            if archived.contains(id) {
                return Err("the active checkout cannot be archived".into());
            }
            if self.roots.is_empty() {
                return Err("an active checkout requires a registered root".into());
            }
        }
        Ok(())
    }
}

pub(super) fn validate_path(path: &str) -> Result<(), String> {
    let parsed = Path::new(path);
    if path.len() > 4096
        || path.contains('\0')
        || !parsed.is_absolute()
        || parsed.components().collect::<PathBuf>().as_os_str() != parsed.as_os_str()
        || parsed
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err("native workspace paths must be absolute normalized directory paths".into());
    }
    Ok(())
}

pub(super) fn validate_checkout_id(id: &str) -> Result<(), String> {
    validate_path(
        id.strip_prefix("checkout:")
            .ok_or("invalid native checkout ID")?,
    )
}

/// Suggested isolated state location; the caller may instead use a test/profile path.
/// No fallback to the Legacy database or another format is attempted.
pub fn state_path() -> Result<PathBuf, String> {
    let home =
        std::env::var_os("HOME").ok_or("HOME is unavailable for native workspace storage")?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        return Err("HOME must be an absolute directory".into());
    }
    Ok(home.join(".muster/native-workspace.json"))
}

/// Missing files start an empty workspace. A file this build cannot read, symlinks,
/// non-regular files and oversized files are explicit errors.
pub fn load(path: &Path) -> Result<NavigationState, String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(NavigationState::default())
        }
        Err(error) => {
            return Err(format!(
                "could not inspect native workspace {}: {error}",
                path.display()
            ))
        }
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err("native workspace must be a regular file, not a symlink".into())
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
            "could not read native workspace {}: {error}",
            path.display()
        )
    })?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect native workspace: {error}"))?;
    if !metadata.is_file() || metadata.len() > STATE_CAP {
        return Err("native workspace is not a regular file within the 1 MiB limit".into());
    }
    let mut bytes = Vec::new();
    file.take(STATE_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read native workspace: {error}"))?;
    if bytes.len() as u64 > STATE_CAP {
        return Err("native workspace exceeds the 1 MiB limit".into());
    }
    let state: NavigationState = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid native workspace {}: {error}", path.display()))?;
    state.validate()?;
    Ok(state)
}

/// Validate, write privately in the same directory, sync, then atomically replace.
/// Invalid existing files are never silently overwritten. Hosts must serialize
/// writes and surface errors; a directory-sync failure can occur after replacement.
pub fn save(path: &Path, state: &NavigationState) -> Result<(), String> {
    state.validate()?;
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("could not encode native workspace: {error}"))?;
    if bytes.len() as u64 > STATE_CAP {
        return Err("native workspace exceeds the 1 MiB limit".into());
    }
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
        .map_err(|error| format!("could not create native workspace directory: {error}"))?;
    let name = path
        .file_name()
        .ok_or("native workspace path has no filename")?;
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
        .map_err(|error| format!("could not create native workspace temporary file: {error}"))?;
    let written = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        File::open(parent)?.sync_all()
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temp);
        return Err(format!(
            "could not save native workspace {}: {error}",
            path.display()
        ));
    }
    Ok(())
}
