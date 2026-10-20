//! The preferences a person sets once and forgets they made: `~/.muster/settings.json`.
//!
//! They are preferences rather than workspace state, and the two are kept apart on purpose.
//! [`crate::navigation`] holds what a *workspace* is -- which checkouts exist, how wide the
//! sidebar is, where reviews are exported -- and this holds the choices about how things look.
//! A window remembering the mode it is in is neither, and stays in window memory.
//!
//! ## Why no version
//!
//! The two stores this crate keeps beside it refuse a file this build cannot read,
//! because each holds someone's work: checkouts they registered, rounds that owe a
//! service a decision. So each is one shape with every field required, and a shape
//! that changes is changed in place -- there is no number to move. A preference
//! holds no work: re-picking a font size costs nothing, and no preference can be
//! half-read into a wrong state, since every field here is either a number inside a
//! documented range or a closed set of spellings. So every field defaults, an
//! unknown key is a key this build ignores, and this file is the one document in
//! `~/.muster` a person may open and edit: the file is deliberately readable text.
//!
//! ## What is not here
//!
//! * No theme, no accent, no editor background: one appearance, dark, chosen in the host before
//!   the first window opens, and the palette lives in the host's `theme` module. A colour field
//!   here would be a second authority for a decision that has one.
//! * No interface zoom: the host scales nothing as a whole.
//! * No review storage: [`crate::navigation::NavigationState`] already persists it, and one
//!   preference with two homes is one preference that can disagree with itself.
//! * No line height and no scrollback size: those are the terminal's own constants, in both
//!   this app and the one it replaced.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};

const SETTINGS_FILE: &str = "settings.json";
/// This file is a few dozen lines of numbers. Anything past this is not a preference file.
const STATE_CAP: u64 = 64 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

/// A range a size is clamped into, published so a control cannot offer a value the file refuses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SizeLimits {
    pub min: f64,
    pub max: f64,
}

pub const UI_FONT_SIZE_LIMITS: SizeLimits = SizeLimits {
    min: 11.0,
    max: 20.0,
};
pub const TERMINAL_FONT_SIZE_LIMITS: SizeLimits = SizeLimits {
    min: 9.0,
    max: 32.0,
};
pub const EDITOR_FONT_SIZE_LIMITS: SizeLimits = SizeLimits {
    min: 9.0,
    max: 32.0,
};
pub const INDENTATION_SIZE_LIMITS: SizeLimits = SizeLimits { min: 1.0, max: 8.0 };

/// The shape of a terminal cursor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalCursorStyle {
    /// The cell it fills, with the glyph under it drawn as the surface: a caret is the one
    /// thing on screen drawn the other way round.
    #[default]
    Block,
    /// A line beside the text, for a reader who would rather not have a cell filled in.
    Bar,
    /// A line under the text.
    Underline,
}

/// When the terminal's own scrollbar is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalScrollbarMode {
    #[default]
    Hidden,
    /// While there is somewhere to scroll to.
    Auto,
    Always,
}

/// What a terminal follows when the work it is doing moves to another worktree.
///
/// A row in the panel moves; the shell inside it does not. Nothing here changes a
/// process's working directory or feeds a directory to a program in front of the
/// shell: a row is a row, and where it is filed says which worktree the person is
/// looking at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FollowWorktree {
    /// Follow neither a shell's working directory nor an agent's work.
    #[default]
    Off,
    /// Follow the directory the shell is in, when that is another registered worktree.
    Cd,
    /// Follow the work an agent session is doing, when that is another registered
    /// worktree.
    Agent,
    /// Follow both.
    Both,
}

impl FollowWorktree {
    /// Whether a shell's working directory is followed.
    pub fn follows_cd(self) -> bool {
        matches!(self, Self::Cd | Self::Both)
    }

    /// Whether an agent session's work is followed.
    pub fn follows_agent(self) -> bool {
        matches!(self, Self::Agent | Self::Both)
    }
}

/// The interface's own sizes and one tree behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiSettings {
    /// The one number every size the interface draws itself in is a ratio of.
    pub font_size: f64,
    /// Whether parent folders stay visible while the file tree scrolls.
    pub tree_sticky_scroll: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            tree_sticky_scroll: true,
        }
    }
}

/// How the terminal is drawn and how it reacts.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TerminalSettings {
    pub font_size: f64,
    pub ligatures: bool,
    pub cursor_style: TerminalCursorStyle,
    pub cursor_blink: bool,
    /// Whether a selection gesture in the terminal puts what it selected on the clipboard, the
    /// way a text field does. Read when the copy runs rather than when a terminal is created,
    /// so it applies to the terminals already open.
    pub selection_copy: bool,
    pub scrollbar: TerminalScrollbarMode,
    /// Whether a terminal follows the work it is doing into another worktree.
    pub follow_worktree: FollowWorktree,
    /// Whether a new terminal is told to report how its commands ended, which is what turns a
    /// failed command's row red. Read when a terminal is created, so it decides only the
    /// terminals opened after the change; off costs the red row, because nothing else in the app
    /// can see a command that failed without ending the shell.
    pub shell_integration: bool,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_size: 16.0,
            ligatures: true,
            cursor_style: TerminalCursorStyle::Block,
            cursor_blink: true,
            selection_copy: true,
            scrollbar: TerminalScrollbarMode::Hidden,
            shell_integration: true,
            follow_worktree: FollowWorktree::Off,
        }
    }
}

/// One level of indent, in spaces or in tab stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IndentationSettings {
    pub use_spaces: bool,
    /// A whole number of columns. A fractional one is refused by name rather than rounded:
    /// it can only be a typo, and rounding it would store a size nobody asked for.
    pub size: u32,
}

impl Default for IndentationSettings {
    fn default() -> Self {
        Self {
            use_spaces: true,
            size: 2,
        }
    }
}

/// How the document editor is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EditorSettings {
    pub font_size: f64,
    pub ligatures: bool,
    /// The same answer the terminal's own blink is, for the same reason: a caret that blinks
    /// is a distraction.
    pub cursor_blink: bool,
    pub indentation: IndentationSettings,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            font_size: 13.0,
            ligatures: true,
            cursor_blink: true,
            indentation: IndentationSettings::default(),
        }
    }
}

/// Every preference this build keeps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    pub ui: UiSettings,
    pub terminal: TerminalSettings,
    pub editor: EditorSettings,
}

impl AppSettings {
    /// What a hand-edited file asked for, brought inside the ranges above.
    ///
    /// A size outside its range is not drawn at that size, so it is clamped rather than
    /// refused: this file exists to be edited, and a 400px font is a typo rather than a reason
    /// to lose the rest of it. A value no font size can be comes back as the default instead,
    /// because the default is a size.
    ///
    /// Clamping on the way out as well as on the way in is what keeps the file readable: JSON
    /// has no infinity, so an unclamped one would be written as `null` and the next read would
    /// refuse the file this app had just written itself.
    ///
    /// The closed sets need no clamping: an unknown spelling is refused by the parse, named
    /// with the variant it did not have, rather than quietly becoming one of them.
    pub fn clamped(self) -> Self {
        Self {
            ui: UiSettings {
                font_size: bounded(self.ui.font_size, UI_FONT_SIZE_LIMITS, 14.0),
                tree_sticky_scroll: self.ui.tree_sticky_scroll,
            },
            terminal: TerminalSettings {
                font_size: bounded(self.terminal.font_size, TERMINAL_FONT_SIZE_LIMITS, 16.0),
                ligatures: self.terminal.ligatures,
                cursor_style: self.terminal.cursor_style,
                cursor_blink: self.terminal.cursor_blink,
                selection_copy: self.terminal.selection_copy,
                scrollbar: self.terminal.scrollbar,
                shell_integration: self.terminal.shell_integration,
                follow_worktree: self.terminal.follow_worktree,
            },
            editor: EditorSettings {
                font_size: bounded(self.editor.font_size, EDITOR_FONT_SIZE_LIMITS, 13.0),
                ligatures: self.editor.ligatures,
                cursor_blink: self.editor.cursor_blink,
                indentation: IndentationSettings {
                    use_spaces: self.editor.indentation.use_spaces,
                    size: self.editor.indentation.size.clamp(
                        INDENTATION_SIZE_LIMITS.min as u32,
                        INDENTATION_SIZE_LIMITS.max as u32,
                    ),
                },
            },
        }
    }
}

/// Suggested location; the caller may instead use a test or profile path.
pub fn settings_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is unavailable for settings storage")?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        return Err("HOME must be an absolute directory".into());
    }
    Ok(home.join(".muster").join(SETTINGS_FILE))
}

/// Missing files read as the defaults. A symlink, a non-regular file, an oversized file and a
/// document that does not parse are each an error naming the path: a settings file is edited by
/// hand, so a mistake in it is something to report rather than to paper over with defaults.
pub fn load(path: &Path) -> Result<AppSettings, String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AppSettings::default())
        }
        Err(error) => {
            return Err(format!(
                "could not inspect settings {}: {error}",
                path.display()
            ))
        }
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err("settings must be a regular file, not a symlink".into())
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
    let file = options
        .open(path)
        .map_err(|error| format!("could not read settings {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect settings: {error}"))?;
    if !metadata.is_file() || metadata.len() > STATE_CAP {
        return Err("settings are not a regular file within the 64 KiB limit".into());
    }
    let mut bytes = Vec::new();
    file.take(STATE_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read settings: {error}"))?;
    if bytes.len() as u64 > STATE_CAP {
        return Err("settings exceed the 64 KiB limit".into());
    }
    // Empty is a person who opened the file, deleted everything and saved: nothing is set yet,
    // which is what the defaults say. Not a file that is broken.
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(AppSettings::default());
    }
    let settings: AppSettings = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid settings {}: {error}", path.display()))?;
    Ok(settings.clamped())
}

/// Clamp, then write privately in the same directory, sync, and replace atomically.
///
/// An existing file that does not parse is never written over: it is the only copy of the
/// preferences somebody had, and replacing a typo they could have found with the defaults they
/// never asked for loses the rest of the file too. Callers serialize their own writes.
pub fn save(path: &Path, settings: &AppSettings) -> Result<(), String> {
    load(path)?;
    let bytes = serde_json::to_vec_pretty(&settings.clamped())
        .map_err(|error| format!("could not encode settings: {error}"))?;
    if bytes.len() as u64 > STATE_CAP {
        return Err("settings exceed the 64 KiB limit".into());
    }
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
        .map_err(|error| format!("could not create the settings folder: {error}"))?;
    let name = path.file_name().ok_or("settings path has no filename")?;
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
        .map_err(|error| format!("could not create the settings temporary file: {error}"))?;
    let written = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        File::open(parent)?.sync_all()
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temp);
        return Err(format!(
            "could not save settings {}: {error}",
            path.display()
        ));
    }
    Ok(())
}

fn bounded(value: f64, limits: SizeLimits, default: f64) -> f64 {
    if !value.is_finite() {
        return default;
    }
    value.clamp(limits.min, limits.max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path_in(home: &tempfile::TempDir) -> PathBuf {
        home.path().join(".muster").join(SETTINGS_FILE)
    }

    #[test]
    fn a_missing_file_reads_as_the_defaults_and_a_save_creates_the_folder() {
        let home = tempfile::tempdir().unwrap();
        let path = path_in(&home);
        assert_eq!(load(&path).unwrap(), AppSettings::default());
        save(&path, &AppSettings::default()).unwrap();
        assert_eq!(load(&path).unwrap(), AppSettings::default());
        assert!(path.is_file());
    }

    #[test]
    fn what_is_saved_is_what_is_read_back() {
        let home = tempfile::tempdir().unwrap();
        let path = path_in(&home);
        let written = AppSettings {
            ui: UiSettings {
                font_size: 16.0,
                tree_sticky_scroll: false,
            },
            terminal: TerminalSettings {
                font_size: 18.0,
                ligatures: false,
                cursor_style: TerminalCursorStyle::Bar,
                cursor_blink: false,
                selection_copy: false,
                scrollbar: TerminalScrollbarMode::Always,
                shell_integration: false,
                follow_worktree: FollowWorktree::Both,
            },
            editor: EditorSettings {
                font_size: 15.0,
                ligatures: false,
                cursor_blink: false,
                indentation: IndentationSettings {
                    use_spaces: false,
                    size: 4,
                },
            },
        };
        save(&path, &written).unwrap();
        assert_eq!(load(&path).unwrap(), written);
    }

    /// The file is a document a person edits, so a key this build does not have is a key it
    /// ignores and a value out of range is one it clamps: neither costs the rest of the file.
    #[test]
    fn a_partial_or_future_file_keeps_the_defaults_it_did_not_set() {
        let home = tempfile::tempdir().unwrap();
        let path = path_in(&home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"terminal":{"fontSize":18,"cursorStyle":"underline"},"future":{"x":1}}"#,
        )
        .unwrap();
        let settings = load(&path).unwrap();
        assert_eq!(settings.terminal.font_size, 18.0);
        assert_eq!(
            settings.terminal.cursor_style,
            TerminalCursorStyle::Underline
        );
        assert_eq!(settings.ui.font_size, 14.0);
        assert_eq!(settings.editor.indentation.size, 2);
    }

    #[test]
    fn a_size_outside_its_range_is_clamped_and_an_unusable_one_is_the_default() {
        let home = tempfile::tempdir().unwrap();
        let path = path_in(&home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"ui":{"fontSize":400},"terminal":{"fontSize":1}}"#,
        )
        .unwrap();
        let settings = load(&path).unwrap();
        assert_eq!(settings.ui.font_size, UI_FONT_SIZE_LIMITS.max);
        assert_eq!(settings.terminal.font_size, TERMINAL_FONT_SIZE_LIMITS.min);

        // A number too large to be a float is refused by the parse, and a caller holding one
        // in memory gets the default rather than a `null` this file could not be read back from.
        fs::write(&path, r#"{"editor":{"fontSize":1e999}}"#).unwrap();
        assert!(load(&path).is_err());
        assert_eq!(
            AppSettings {
                editor: EditorSettings {
                    font_size: f64::INFINITY,
                    ..EditorSettings::default()
                },
                ..AppSettings::default()
            }
            .clamped()
            .editor
            .font_size,
            13.0
        );

        fs::write(&path, r#"{"editor":{"indentation":{"size":40}}}"#).unwrap();
        assert_eq!(load(&path).unwrap().editor.indentation.size, 8);
    }

    /// A misspelled enum is not a preference this build can act on, so it is named rather than
    /// quietly becoming one of the settings there are.
    #[test]
    fn a_cursor_this_build_does_not_have_is_refused_by_name() {
        let home = tempfile::tempdir().unwrap();
        let path = path_in(&home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"terminal":{"cursorStyle":"wedge"}}"#).unwrap();
        assert!(
            load(&path).unwrap_err().contains("wedge"),
            "the error names what it was not"
        );
    }

    /// A person's only copy of their preferences is the file, so an Apply is not allowed to
    /// replace a typo with a page of defaults.
    #[test]
    fn a_file_that_does_not_parse_is_never_written_over() {
        let home = tempfile::tempdir().unwrap();
        let path = path_in(&home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let broken = "{ not json";
        fs::write(&path, broken).unwrap();
        assert!(load(&path).is_err());
        assert!(save(&path, &AppSettings::default()).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), broken);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_not_the_file_and_a_saved_one_is_private() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let home = tempfile::tempdir().unwrap();
        let path = path_in(&home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let target = home.path().join("elsewhere.json");
        symlink(&target, &path).unwrap();
        assert!(load(&path).unwrap_err().contains("symlink"));
        assert!(save(&path, &AppSettings::default()).is_err());

        fs::remove_file(&path).unwrap();
        save(&path, &AppSettings::default()).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "a settings file is private to the person who owns it"
        );
    }
}
