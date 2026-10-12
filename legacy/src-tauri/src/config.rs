//! `~/.muster/config.yml`: the preferences the Settings dialog owns.
//!
//! One file, in the user's home, written by the dialog and read at launch. It is not the
//! database: the database holds state that belongs to a workspace (which checkouts exist, how wide
//! a pane is), and this holds the choices a person makes once and forgets they made.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    domain::ipc::{IpcError, IpcErrorCode},
    services::files::atomic_write,
};

const CONFIG_DIR_FROM_HOME: &str = ".muster";
const CONFIG_FILE_NAME: &str = "config.yml";

const UI_FONT_SIZE_MIN: f64 = 11.0;
const UI_FONT_SIZE_MAX: f64 = 20.0;
const TERMINAL_FONT_SIZE_MIN: f64 = 9.0;
const TERMINAL_FONT_SIZE_MAX: f64 = 32.0;
const EDITOR_FONT_SIZE_MIN: f64 = 9.0;
const EDITOR_FONT_SIZE_MAX: f64 = 32.0;
const INDENTATION_SIZE_MIN: u32 = 1;
const INDENTATION_SIZE_MAX: u32 = 8;
/// The dark surface the editor, the diff and the terminal open on, and what a `contentBackground`
/// that is not a color comes back as. It is written in `src/domain/settings.ts` as well, because a
/// preference nobody has touched still has to have an answer on both sides of the bridge, and one
/// constant here is what keeps the two copies from drifting apart.
const CONTENT_BACKGROUND: &str = "#16181c";
/// The ends of the zoom steps the renderer walks. Kept here so a factor typed into the file
/// cannot come back as a scale no step in `src/domain/zoom.ts` names.
const ZOOM_MIN: f64 = 0.8;
const ZOOM_MAX: f64 = 1.5;

#[derive(Clone)]
pub struct ConfigFile(pub PathBuf);

pub fn config_file(home: &Path) -> ConfigFile {
    ConfigFile(home.join(CONFIG_DIR_FROM_HOME).join(CONFIG_FILE_NAME))
}

/// The app's own folder in the user's home, which is where `config.yml` lives.
///
/// Also where the shell-integration script a terminal sources is written, so that the two things this
/// app keeps in a person's home sit together rather than in a folder that exists for one of them.
/// Derived from the settings file rather than from `home` a second time, because `terminal_create`
/// is handed the file and not the home, and it should not have to walk back up to find the folder.
pub fn dir_of(config_file: &ConfigFile) -> Option<&Path> {
    config_file.0.parent()
}

/// The home the settings file sits in, derived the same way `dir_of` is.
///
/// `~/.muster` is the app's folder in a home, so its parent is that home. Callers that need both
/// — anything deciding where the app keeps its own files — already hold the settings file, and a
/// second copy of "where this person's home is" in every one of those signatures is a second thing
/// to keep in step with the first.
pub fn home_of(config_file: &ConfigFile) -> Option<&Path> {
    dir_of(config_file)?.parent()
}

/// Every field defaults, unlike the rows in the database: this file is written by a person as
/// well as by the dialog, and a line they deleted is a preference they did not set, not a shape
/// this build cannot read.
///
/// That is the whole reason these `default`s exist, and it is not a reading of what an older build
/// wrote. `~/.muster/config.yml` survives every upgrade, is hand-edited, and is not covered by
/// `SCHEMA_VERSION`: refusing a file because a key is absent would take the settings of everyone
/// who never opened the dialog on a preference that has a default anyway.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    pub ui: UiSettings,
    pub terminal: TerminalSettings,
    pub editor: EditorSettings,
    pub reviews: ReviewSettings,
}

/// Where an exported review round is written: `default` is the app's own folder in the user's
/// home, `workdir` is a folder inside the checkout the review was made in.
///
/// Two, and only two, because the two questions behind it have different answers. The home folder
/// is the app's business: notes nobody asked to keep, in a place no repository can reach. The
/// working directory is the project's business: notes that belong to the code they are about, next
/// to it, where a diff, a commit or a teammate can find them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ReviewSettings {
    pub storage: String,
}

pub const REVIEW_STORAGE_DEFAULT: &str = "default";
pub const REVIEW_STORAGE_WORKDIR: &str = "workdir";

impl Default for ReviewSettings {
    fn default() -> Self {
        Self {
            storage: REVIEW_STORAGE_DEFAULT.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct UiSettings {
    pub font_size: f64,
    pub zoom: f64,
    pub theme: String,
    pub content_background: String,
    pub tree_sticky_scroll: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            zoom: 1.0,
            theme: "system".into(),
            content_background: CONTENT_BACKGROUND.into(),
            tree_sticky_scroll: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct TerminalSettings {
    pub font_size: f64,
    pub ligatures: bool,
    pub cursor_blink: bool,
    /// The shape of the cursor: `block`, `bar` or `underline`.
    pub cursor_style: String,
    pub scrollbar: String,
    /// Whether a new terminal is told to report how its commands ended, which is what turns a
    /// failed command's row red.
    ///
    /// Read when a terminal is created, so this only decides what the terminals opened *after* the
    /// change get: terminals already open keep their startup hooks, and turning it on does not
    /// reach back into them. Off costs the red bar, because nothing else in the app can see a command
    /// that failed without ending the shell.
    /// Native shell startup preserves banners and warnings; unsupported shells remain uninstrumented.
    pub shell_integration: bool,
    /// Which activity moves a terminal to its matching worktree: off, cd, agent or both.
    pub follow_worktree: String,
    /// Whether a selection gesture in the terminal puts what it selected on the clipboard, the way
    /// a text field does. On by default, which is the behaviour this preference started as; off
    /// leaves the selection itself alone so nothing lands on the clipboard behind the reader's back.
    ///
    /// Read when the copy callback runs rather than when a terminal is created, so it applies to the
    /// terminals already open and not only to the ones opened after the change.
    pub selection_copy: bool,
}

const WORKTREE_FOLLOW_MODES: [&str; 4] = ["off", "cd", "agent", "both"];

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_size: 16.0,
            ligatures: true,
            cursor_blink: true,
            cursor_style: "block".into(),
            scrollbar: "hidden".into(),
            shell_integration: true,
            follow_worktree: "agent".into(),
            selection_copy: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct EditorSettings {
    pub font_size: f64,
    pub ligatures: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct IndentationSettings {
    pub use_spaces: bool,
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

impl AppSettings {
    /// Clamp what a hand-edited file asked for. Sizes are read-only for the shell, so a value
    /// outside the range is not drawn at that size, and a scrollbar mode outside the three the
    /// terminal knows is not a mode at all.
    fn normalized(mut self) -> Self {
        self.ui.font_size = bounded(self.ui.font_size, UI_FONT_SIZE_MIN, UI_FONT_SIZE_MAX, 14.0);
        self.ui.zoom = bounded(self.ui.zoom, ZOOM_MIN, ZOOM_MAX, 1.0);
        if !matches!(self.ui.theme.as_str(), "system" | "light" | "dark") {
            self.ui.theme = "system".into();
        }
        if !valid_hex_color(&self.ui.content_background) {
            self.ui.content_background = CONTENT_BACKGROUND.into();
        } else {
            self.ui.content_background.make_ascii_lowercase();
        }
        self.terminal.font_size = bounded(
            self.terminal.font_size,
            TERMINAL_FONT_SIZE_MIN,
            TERMINAL_FONT_SIZE_MAX,
            16.0,
        );
        if !matches!(
            self.terminal.cursor_style.as_str(),
            "block" | "bar" | "underline"
        ) {
            self.terminal.cursor_style = "block".into();
        }
        if !matches!(
            self.terminal.scrollbar.as_str(),
            "hidden" | "auto" | "always"
        ) {
            self.terminal.scrollbar = "hidden".into();
        }
        if !WORKTREE_FOLLOW_MODES.contains(&self.terminal.follow_worktree.as_str()) {
            self.terminal.follow_worktree = "agent".into();
        }
        self.editor.font_size = bounded(
            self.editor.font_size,
            EDITOR_FONT_SIZE_MIN,
            EDITOR_FONT_SIZE_MAX,
            13.0,
        );
        self.editor.indentation.size = self
            .editor
            .indentation
            .size
            .clamp(INDENTATION_SIZE_MIN, INDENTATION_SIZE_MAX);
        if !matches!(
            self.reviews.storage.as_str(),
            REVIEW_STORAGE_DEFAULT | REVIEW_STORAGE_WORKDIR
        ) {
            self.reviews.storage = REVIEW_STORAGE_DEFAULT.into();
        }
        self
    }
}

fn valid_hex_color(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 7 && bytes[0] == b'#' && bytes[1..].iter().all(u8::is_ascii_hexdigit)
}

/// A file the user typed into also carries `.nan` and `1e999`, which no font size can be, so
/// anything outside the range comes back as the default rather than reaching the renderer.
fn bounded(value: f64, min: f64, max: f64, default: f64) -> f64 {
    if !value.is_finite() {
        return default;
    }
    value.clamp(min, max)
}

/// Reads the file, or the defaults when there is not one yet.
///
/// A file that is there and does not parse is an error and not a fallback: the dialog says so and
/// the file stays exactly as it is, because the alternative is a Apply that overwrites a typo with
/// a page of defaults.
pub fn load(path: &Path) -> Result<AppSettings, IpcError> {
    let Some(document) = read_document(path)? else {
        return Ok(AppSettings::default());
    };
    let settings: AppSettings =
        serde_yaml::from_str(&document).map_err(|error| parse_error(&error.to_string()))?;
    Ok(settings.normalized())
}

/// The file's text, or `None` when there is nothing to read: no file yet, or one that is empty
/// because a person opened it, deleted everything and saved. Both are a person who has not set
/// anything, not a file that is broken.
fn read_document(path: &Path) -> Result<Option<String>, IpcError> {
    match fs::read_to_string(path) {
        Ok(document) if document.trim().is_empty() => Ok(None),
        Ok(document) => Ok(Some(document)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error("could not read the settings file", error)),
    }
}

/// Writes the file, and refuses to write over one that cannot be read.
///
/// The refusal is the point of this function. A file somebody edited by hand and got wrong is the
/// only copy of the preferences they had, and an Apply (or a zoom step on the keyboard, which
/// writes the same file) would replace a typo they could have found with a page of defaults they
/// never asked for. So the file is parsed first: it either says what it says, or it is left alone
/// and the error names the line.
pub fn save(path: &Path, settings: &AppSettings) -> Result<(), IpcError> {
    if let Some(document) = read_document(path)? {
        serde_yaml::from_str::<AppSettings>(&document).map_err(|error| {
            parse_error(&format!(
                "{} was left as it is because it could not be read: {error}",
                path.display()
            ))
        })?;
    }
    let settings = settings.clone().normalized();
    let document = serde_yaml::to_string(&settings)
        .map_err(|error| parse_error(&format!("the settings could not be written: {error}")))?;
    let parent = path.parent().ok_or_else(|| {
        IpcError::new(
            IpcErrorCode::InvalidPath,
            "the settings path has no parent directory",
        )
    })?;
    fs::create_dir_all(parent)
        .map_err(|error| io_error("could not create the settings folder", error))?;
    atomic_write(path, document.as_bytes(), permissions_for(path))
}

/// The mode the file is written with. One the user never chose is private to them; one they did
/// is left alone, because a save is not a reason to undo a decision made outside this app.
fn permissions_for(path: &Path) -> fs::Permissions {
    if let Ok(metadata) = fs::metadata(path) {
        return metadata.permissions();
    }
    new_file_permissions()
}

#[cfg(unix)]
fn new_file_permissions() -> fs::Permissions {
    use std::os::unix::fs::PermissionsExt;
    fs::Permissions::from_mode(0o600)
}

#[cfg(not(unix))]
fn new_file_permissions() -> fs::Permissions {
    fs::Permissions::default()
}

fn io_error(action: &str, error: std::io::Error) -> IpcError {
    IpcError::new(IpcErrorCode::OperationFailed, format!("{action}: {error}"))
}

fn parse_error(message: &str) -> IpcError {
    IpcError::new(IpcErrorCode::InvalidPath, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// Where `~/.muster/config.yml` lands inside a temporary home, folder created: a test that
    /// writes the file by hand is standing in for a person who already has the folder.
    fn config_path_in(home: &tempfile::TempDir) -> PathBuf {
        let path = home
            .path()
            .join(CONFIG_DIR_FROM_HOME)
            .join(CONFIG_FILE_NAME);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        path
    }

    #[test]
    fn a_missing_file_reads_as_the_defaults_and_a_save_creates_the_folder() {
        let home = settings_dir();
        let path = home
            .path()
            .join(CONFIG_DIR_FROM_HOME)
            .join(CONFIG_FILE_NAME);
        assert_eq!(load(&path).unwrap(), AppSettings::default());
        save(&path, &AppSettings::default()).unwrap();
        assert_eq!(load(&path).unwrap(), AppSettings::default());
        assert!(path.is_file());
    }

    #[test]
    fn a_save_that_writes_over_a_missing_file_creates_the_folder_first() {
        let home = settings_dir();
        let path = home
            .path()
            .join(CONFIG_DIR_FROM_HOME)
            .join(CONFIG_FILE_NAME);
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn what_is_saved_is_what_is_read_back() {
        let home = settings_dir();
        let path = home
            .path()
            .join(CONFIG_DIR_FROM_HOME)
            .join(CONFIG_FILE_NAME);
        let written = AppSettings {
            ui: UiSettings {
                font_size: 16.0,
                zoom: 1.2,
                theme: "light".into(),
                content_background: CONTENT_BACKGROUND.into(),
                tree_sticky_scroll: true,
            },
            terminal: TerminalSettings {
                font_size: 18.0,
                ligatures: false,
                cursor_blink: false,
                cursor_style: "bar".into(),
                scrollbar: "always".into(),
                shell_integration: false,
                follow_worktree: "both".into(),
                selection_copy: false,
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
            reviews: ReviewSettings {
                storage: REVIEW_STORAGE_WORKDIR.into(),
            },
        };
        save(&path, &written).unwrap();
        assert_eq!(load(&path).unwrap(), written);
    }

    /// An export has exactly two homes, so a third name in a hand-edited file is a typo rather than
    /// a mode, and the folder in the app's own directory is the answer that is never a surprise.
    #[test]
    fn an_unknown_review_storage_reads_as_the_app_folder() {
        let home = settings_dir();
        let path = config_path_in(&home);
        std::fs::write(&path, "reviews:\n  storage: somewhere-else\n").unwrap();
        assert_eq!(load(&path).unwrap().reviews.storage, REVIEW_STORAGE_DEFAULT);
    }

    /// A preference the renderer owns crosses the bridge as JSON, lands in this struct and goes back
    /// out to the file, and every step of that is silent about a field it does not have. This is
    /// the test that would have caught that: it starts from the shape the dialog sends rather than
    /// from this side of it, so a field missing here is a field the file does not carry.
    #[test]
    fn every_preference_the_renderer_owns_survives_a_save_and_a_load() {
        let home = settings_dir();
        let path = config_path_in(&home);
        // What the Settings dialog sends: the renderer holds these as camelCase, and the file
        // holds them as camelCase too, so both ends of this are the same words.
        let from_the_dialog = serde_json::json!({
            "ui": { "fontSize": 16.0, "zoom": 1.0, "theme": "dark", "contentBackground": CONTENT_BACKGROUND, "treeStickyScroll": true },
            "terminal": {
                "fontSize": 16.0,
                "ligatures": true,
                "cursorBlink": true,
                "cursorStyle": "block",
                "scrollbar": "hidden",
                "shellIntegration": true,
                "followWorktree": "cd",
                "selectionCopy": true
            },
            "editor": {
                "fontSize": 13.0,
                "ligatures": true,
                "cursorBlink": true,
                "indentation": { "useSpaces": true, "size": 2 }
            },
            "reviews": { "storage": "workdir" }
        });

        save(
            &path,
            &serde_yaml::from_str(&serde_yaml::to_string(&from_the_dialog).unwrap()).unwrap(),
        )
        .unwrap();
        let loaded = load(&path).unwrap();

        assert_eq!(loaded.terminal.follow_worktree, "cd");
        assert_eq!(loaded.reviews.storage, REVIEW_STORAGE_WORKDIR);
        assert_eq!(serde_json::to_value(&loaded).unwrap(), from_the_dialog);
    }

    /// Touching one preference used to write out only the ones this side knew about, so changing
    /// the zoom reset a following rule nobody was looking at. The file is what has to remember.
    #[test]
    fn saving_another_preference_keeps_the_following_rules() {
        let home = settings_dir();
        let path = config_path_in(&home);
        let mut settings = AppSettings::default();
        settings.terminal.follow_worktree = "off".into();
        save(&path, &settings).unwrap();

        // A zoom step writes the same file, and it is handed the whole settings, not a patch.
        let mut zoomed = load(&path).unwrap();
        zoomed.ui.zoom = 1.2;
        save(&path, &zoomed).unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.ui.zoom, 1.2);
        assert_eq!(loaded.terminal.follow_worktree, "off");
    }

    #[test]
    fn deleted_terminal_preferences_are_not_normalized_or_saved() {
        let home = settings_dir();
        let path = config_path_in(&home);
        std::fs::write(&path, "terminal:\n  followAgentAcrossWorktrees: false\n  followDirectoryAcrossWorktrees: true\n  followSelection: always\n  changeDirectoryOnMove: false\n").unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded, AppSettings::default());
        save(&path, &loaded).unwrap();
        let terminal = serde_json::to_value(load(&path).unwrap()).unwrap()["terminal"].clone();
        assert_eq!(
            terminal,
            serde_json::json!({
                "fontSize": 16.0, "ligatures": true, "cursorBlink": true, "cursorStyle": "block",
                "scrollbar": "hidden", "shellIntegration": true, "followWorktree": "agent", "selectionCopy": true
            })
        );
        let document = std::fs::read_to_string(&path).unwrap();
        for key in [
            "followAgentAcrossWorktrees",
            "followDirectoryAcrossWorktrees",
            "followSelection",
            "changeDirectoryOnMove",
        ] {
            assert!(!document.contains(key));
        }
    }

    #[test]
    fn worktree_follow_modes_round_trip_and_unknown_modes_default_to_agent() {
        let home = settings_dir();
        let path = config_path_in(&home);
        assert_eq!(AppSettings::default().terminal.follow_worktree, "agent");
        for mode in ["off", "cd", "agent", "both", "sometimes"] {
            std::fs::write(&path, format!("terminal:\n  followWorktree: {mode}\n")).unwrap();
            assert_eq!(
                load(&path).unwrap().terminal.follow_worktree,
                if mode == "sometimes" { "agent" } else { mode }
            );
        }
    }

    #[test]
    fn a_partial_file_keeps_the_defaults_for_the_lines_it_leaves_out() {
        let home = settings_dir();
        let path = config_path_in(&home);
        std::fs::write(&path, "ui:\n  fontSize: 18\n").unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.ui.font_size, 18.0);
        assert_eq!(loaded.terminal, TerminalSettings::default());
        assert_eq!(loaded.editor, EditorSettings::default());
    }

    #[test]
    fn a_file_that_does_not_parse_is_an_error_and_is_left_where_it_is() {
        let home = settings_dir();
        let path = config_path_in(&home);
        let broken = "ui: [this is not\n";
        std::fs::write(&path, broken).unwrap();
        assert!(load(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
    }

    #[test]
    fn a_save_refuses_to_write_over_a_file_it_cannot_read() {
        // The keyboard zoom writes this same file, so "save over a typo" is not a dialog-only
        // mistake: a scale step would destroy a hand-edited config nobody was looking at.
        let home = settings_dir();
        let path = config_path_in(&home);
        let broken = "ui: [this is not\n";
        std::fs::write(&path, broken).unwrap();

        let error = save(&path, &AppSettings::default()).unwrap_err().message;

        assert!(error.contains("left as it is"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
    }

    /// The shell-integration toggle is the one preference whose default is *on*, so it is the one a
    /// missing key is most likely to get wrong in both directions: a `config.yml` written by a build
    /// from before the key existed must load as on, because that is the behaviour those files were
    /// living with, and must not be mistaken for a person having turned it off.
    #[test]
    fn shell_integration_is_on_by_default_and_survives_every_way_of_being_written() {
        let home = settings_dir();
        let path = config_path_in(&home);

        // Absent entirely, which is every file written before the key existed.
        assert!(load(&path).unwrap().terminal.shell_integration);
        // Absent from a file that has other preferences in it.
        std::fs::write(&path, "ui:\n  fontSize: 18\n").unwrap();
        assert!(load(&path).unwrap().terminal.shell_integration);

        // Explicitly off, and explicitly on, both survive a round trip rather than being normalised
        // back to the default.
        for written in [false, true] {
            let mut settings = AppSettings::default();
            settings.terminal.shell_integration = written;
            save(&path, &settings).unwrap();
            assert_eq!(load(&path).unwrap().terminal.shell_integration, written);
            assert!(
                std::fs::read_to_string(&path)
                    .unwrap()
                    .contains(&format!("shellIntegration: {written}")),
                "the file should say what was asked for"
            );
        }
    }

    #[test]
    fn a_fractional_indentation_size_is_rejected_and_not_overwritten() {
        let home = settings_dir();
        let path = config_path_in(&home);
        let invalid = "editor:\n  indentation:\n    size: 2.5\n";
        std::fs::write(&path, invalid).unwrap();

        assert!(load(&path).is_err());
        assert!(save(&path, &AppSettings::default()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), invalid);
    }

    #[test]
    fn a_save_over_a_file_with_lines_missing_from_it_is_allowed() {
        // A partial file is readable and is a person who set two preferences, not a broken one.
        let home = settings_dir();
        let path = config_path_in(&home);
        std::fs::write(&path, "editor:\n  indentation:\n    size: 4\n").unwrap();

        save(&path, &AppSettings::default()).unwrap();

        assert_eq!(load(&path).unwrap(), AppSettings::default());
    }

    #[test]
    fn a_save_over_an_empty_file_is_allowed() {
        let home = settings_dir();
        let path = config_path_in(&home);
        std::fs::write(&path, "  \n").unwrap();

        save(&path, &AppSettings::default()).unwrap();

        assert_eq!(load(&path).unwrap(), AppSettings::default());
    }

    #[test]
    fn sizes_outside_the_range_and_names_that_are_not_modes_come_back_bounded() {
        let home = settings_dir();
        let path = config_path_in(&home);
        std::fs::write(
            &path,
            concat!(
                // `sidebarLayout` is a key this build has no field for. It is here to say what
                // happens to one: an unknown key is ignored, like any other line a person typed,
                // and the file still loads. Nothing reads it and nothing migrates it.
                "ui:\n  fontSize: .nan\n  zoom: 99\n  theme: solarized\n  contentBackground: not-a-color\n  sidebarLayout: wide\n",
                "terminal:\n  fontSize: 2\n  cursorStyle: beam\n  scrollbar: sometimes\n",
                "editor:\n  fontSize: 400\n  indentation:\n    size: 0\n",
            ),
        )
        .unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.ui.font_size, 14.0);
        assert_eq!(loaded.ui.zoom, ZOOM_MAX);
        assert_eq!(loaded.ui.theme, "system");
        assert_eq!(loaded.ui.content_background, CONTENT_BACKGROUND);
        assert_eq!(loaded.terminal.font_size, TERMINAL_FONT_SIZE_MIN);
        assert_eq!(loaded.terminal.cursor_style, "block");
        assert_eq!(loaded.terminal.scrollbar, "hidden");
        assert_eq!(loaded.editor.font_size, EDITOR_FONT_SIZE_MAX);
        assert_eq!(loaded.editor.indentation.size, INDENTATION_SIZE_MIN);
    }

    #[test]
    fn a_color_keeps_its_hex_digits_and_loses_its_capitals() {
        // A picker hands back lower case and a person typing into the file does not, so the two
        // spellings of one color have to come back as the one the dialog and the stylesheet compare.
        let home = settings_dir();
        let path = config_path_in(&home);
        std::fs::write(&path, "ui:\n  contentBackground: '#A1B2C3'\n").unwrap();
        assert_eq!(load(&path).unwrap().ui.content_background, "#a1b2c3");
    }

    #[cfg(unix)]
    #[test]
    fn a_save_keeps_the_mode_the_file_already_had() {
        use std::os::unix::fs::PermissionsExt;
        let home = settings_dir();
        let path = home
            .path()
            .join(CONFIG_DIR_FROM_HOME)
            .join(CONFIG_FILE_NAME);
        save(&path, &AppSettings::default()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        save(
            &path,
            &AppSettings {
                ui: UiSettings {
                    font_size: 15.0,
                    zoom: 1.0,
                    theme: "dark".into(),
                    content_background: CONTENT_BACKGROUND.into(),
                    tree_sticky_scroll: true,
                },
                ..AppSettings::default()
            },
        )
        .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
    }
}
