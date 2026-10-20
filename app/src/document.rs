//! The central document: read as a reading, edited as its source.
use crate::app::TerminalWindow;
use crate::theme::{zed_button, Roles};
use crate::tooltip::RestingTooltip;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::text::{FrontmatterPlugin, MarkdownExtensions, TextView, TextViewState};
use gpui_kit::component::{Disableable, Icon, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    div, point, px, AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Role, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window,
};
use muster_core::workspace::{Checkout, Repo};
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

mod media;

/// The document's editor, kept between renders.
///
/// The state is one entity created when the document is opened and read from
/// every frame after: a text selection, a scroll offset and a parse are then
/// things the reader did rather than things the last frame threw away. The text
/// is written back, and only when somebody says so.
pub(crate) struct DocumentView {
    pub(crate) state: Entity<EditorState>,
    /// The reading view of a Markdown file, built when the document is opened and
    /// kept beside the editor so the two views are one toggle rather than a second
    /// read of the file.
    pub(crate) preview: Option<Entity<TextViewState>>,
    /// The replacement was published, but its directory entry was not confirmed durable.
    pub(crate) durability_uncertain: bool,
    /// Whether this document is being read as a document or as its source. True
    /// for a Markdown file that opens rendered, false for everything else.
    rendered: bool,
    /// The document this state was built for. A different path replaces it: a
    /// selection or an offset carried over from another file would describe a
    /// position in a file that is not on screen.
    path: String,
    /// The text as the file held it when it was read: what a draft is measured
    /// against, what Cancel puts back, and what a save says the file must still
    /// hold.
    loaded: String,
    /// Whether the editor holds another text than the one above, decided as the
    /// text is asked for rather than as it changes: an undo back to what the file
    /// holds is the end of a draft, and only a comparison says so.
    dirty: bool,
    /// A save is on its way, so the controls wait for its answer.
    pub(crate) saving: bool,
    /// Prettier is working on a captured copy; its answer may only replace that same text.
    pub(crate) formatting: bool,
    /// Cancelling or replacing this document invalidates an in-flight formatter result.
    format_generation: u64,
    /// The pictures this document's reading has named, and the bytes that came
    /// back for them. Owned by the document rather than by the window, so a
    /// different document — or closing this one — drops them with the view, and
    /// a read still in flight has nothing to publish into afterwards.
    pub(crate) media: Arc<media::MediaStore>,
}

impl DocumentView {
    /// The editor for one document, already holding its whole text.
    ///
    /// The text is set once, whole, from what `read_preview` returned: a
    /// preview that stops mid-file would look like a file that ends there.
    fn new(
        identity: &str,
        path: &str,
        text: &str,
        language: &str,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language)
                .line_number(true)
                // No reflow: a document is shown as it is written, and the
                // editor scrolls both ways instead of rewrapping long lines.
                .soft_wrap(false)
                .default_value(text)
        });
        let markdown = is_markdown(path);
        let preview = markdown.then(|| cx.new(|cx| TextViewState::markdown(text, cx)));
        Self {
            state,
            preview,
            durability_uncertain: false,
            rendered: markdown,
            path: identity.to_owned(),
            loaded: text.to_owned(),
            dirty: false,
            saving: false,
            formatting: false,
            format_generation: 0,
            media: media::MediaStore::new(),
        }
    }

    /// Whether this document is being read as a document rather than as source.
    ///
    /// A file that is not Markdown has no second reading, so the question only has
    /// two answers where there are two views to choose between.
    pub(crate) fn is_dirty(&self) -> bool {
        self.dirty
    }

    fn is_rendered(&self) -> bool {
        self.rendered && self.preview.is_some()
    }

    /// The reading that is on screen, and the keyboard that goes with it.
    ///
    /// A document has two readings and mounts one of them, so the focus belongs to
    /// the one mounted: on the editor while the source is what the pane draws, and
    /// on the text view while the reading is. Focusing the editor behind a rendered
    /// document is what made a Markdown file look like the keyboard had stopped, and
    /// leaving focus on the reading that just went away is the other half of it. Both
    /// handles are the toolkit's own — `EditorState` and `TextViewState` each carry
    /// one, and the toolkit tracks them on the elements it draws.
    fn focus_visible(&self, window: &mut Window, cx: &mut App) {
        if let Some(handle) = self.visible_handle(cx) {
            handle.focus(window, cx);
        }
    }

    /// The same keyboard, asked for at the end of this cycle rather than now.
    ///
    /// The toolkit's editor draws its caret only once it has been told it is
    /// focused, and that answer comes with a frame's report of what holds the
    /// keyboard. A reading built in this very frame never hears about it: the
    /// report this frame makes is made before the reading's own listener is
    /// live, so the caret is never drawn and the document reads as a picture of
    /// a file rather than as something a person can type in. Asked for at the
    /// end of the cycle, the focus is where the next frame's report can carry
    /// it.
    fn focus_visible_after_this_frame(&self, window: &mut Window, cx: &mut App) {
        let Some(handle) = self.visible_handle(cx) else {
            return;
        };
        window.defer(cx, move |window, cx| handle.focus(window, cx));
    }

    /// The handle of the reading that is on screen.
    fn visible_handle(&self, cx: &App) -> Option<FocusHandle> {
        if self.is_rendered() {
            self.preview
                .as_ref()
                .map(|preview| preview.read(cx).focus_handle().clone())
        } else {
            Some(self.state.read(cx).focus_handle(cx))
        }
    }
}

/// What a draft is saved with, on this platform's keyboard.
fn save_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘S"
    } else {
        "Ctrl+S"
    }
}

/// The editor for `path`: the retained one, or a new one for a new document.
fn document_state(
    retained: &mut Option<DocumentView>,
    identity: &str,
    path: &str,
    text: &str,
    language: &str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<EditorState> {
    if retained.as_ref().is_none_or(|view| view.path != identity) {
        *retained = Some(DocumentView::new(
            identity, path, text, language, window, cx,
        ));
    }
    retained
        .as_ref()
        .map(|view| view.state.clone())
        .expect("just installed")
}

/// Whether a path names a Markdown file.
///
/// The old app's own set, which is the same four names `syntax_language`
/// answers `markdown` for: a file whose whole point
/// is to be read, rather than one whose point is that something runs it.
pub(crate) fn is_markdown(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        matches!(
            extension.to_ascii_lowercase().as_str(),
            "md" | "markdown" | "mdown" | "mkd"
        )
    })
}

/// The toolkit language name a path asks for.
///
/// The language a path asks the toolkit to highlight with.
///
/// Known dotfiles and their variants follow Legacy's filename mappings; unknown
/// files still open, just uncoloured.
pub(crate) fn syntax_language(path: &str) -> &'static str {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase();
    if name == ".env" || name.starts_with(".env.") {
        return "dotenv";
    }
    if let Some(language) = filename_language(&name) {
        return language;
    }
    if let Some(dotfile) = name.strip_prefix('.') {
        if let Some(dot) = dotfile.find('.').map(|dot| dot + 1) {
            if let Some(language) = filename_language(&name[..dot]) {
                return language;
            }
        }
    }
    match name.rsplit_once('.').map_or("", |(_, extension)| extension) {
        "json" | "jsonc" => "json",
        "rs" => "rust",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "yaml" | "yml" => "yaml",
        "html" | "htm" => "html",
        "css" | "scss" => "css",
        "ini" => "ini",
        "md" | "mdown" | "markdown" => "markdown",
        "bash" | "sh" | "zsh" => "bash",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => "cpp",
        "cs" => "csharp",
        "cmake" => "cmake",
        "diff" | "patch" => "diff",
        "ejs" => "ejs",
        "erb" => "erb",
        "ex" | "exs" => "elixir",
        "go" => "go",
        "gql" | "graphql" => "graphql",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "lua" => "lua",
        "make" | "makefile" | "mk" => "make",
        "php" => "php",
        "proto" => "proto",
        "py" | "pyw" | "pyi" => "python",
        "rb" => "ruby",
        "scala" => "scala",
        "sql" => "sql",
        "svelte" => "svelte",
        "swift" => "swift",
        "toml" => "toml",
        "vue" => "vue",
        "zig" => "zig",
        _ => "plaintext",
    }
}

fn prettier_parser(path: &str, language: &str) -> Option<&'static str> {
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match language {
        "javascript" => Some("babel"),
        "typescript" | "tsx" => Some("typescript"),
        "json" => Some(match extension.as_str() {
            "jsonc" => "jsonc",
            "json5" => "json5",
            _ => "json",
        }),
        "css" => Some(match extension.as_str() {
            "scss" => "scss",
            "less" => "less",
            _ => "css",
        }),
        "scss" => Some("scss"),
        "less" => Some("less"),
        "html" => Some("html"),
        "vue" => Some("vue"),
        "markdown" => Some("markdown"),
        "yaml" => Some("yaml"),
        "graphql" => Some("graphql"),
        _ => None,
    }
}

fn formatter_navigation_is_current(current_request: u64, expected_request: u64) -> bool {
    current_request == expected_request
}

fn formatter_result_is_current(
    current_text: &str,
    expected_text: &str,
    current_language: &str,
    expected_language: &str,
) -> bool {
    current_text == expected_text && current_language == expected_language
}

fn formatter_suffix(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "js" | "jsx" | "mjs" | "cjs" => ".js",
        "ts" | "mts" | "cts" => ".ts",
        "tsx" => ".tsx",
        "json" => ".json",
        "jsonc" => ".jsonc",
        "json5" => ".json5",
        "css" => ".css",
        "scss" => ".scss",
        "less" => ".less",
        "html" | "htm" => ".html",
        "vue" => ".vue",
        "md" | "markdown" | "mdown" | "mkd" => ".md",
        "yaml" | "yml" => ".yaml",
        "gql" | "graphql" => ".graphql",
        _ => ".txt",
    }
}

/// The formatters this app runs, in the order it prefers them.
///
/// `prettierd` first because it keeps Prettier loaded between formats, and the
/// plain `prettier` when the daemon is not installed. Both are asked for by
/// name on PATH, outside the checkout, in the order they are written here.
const FORMATTERS: [&str; 2] = ["prettierd", "prettier"];

/// A user-installed Prettier executable, never a command from the checkout.
fn prettier_executable(checkout_root: &Path) -> Result<(PathBuf, OsString), String> {
    let path = std::env::var_os("PATH").ok_or_else(|| {
        "Prettier is unavailable because PATH is not set. Install it outside the checkout."
            .to_owned()
    })?;
    prettier_executable_from_path(checkout_root, &path)
}

fn prettier_executable_from_path(
    checkout_root: &Path,
    path: &OsStr,
) -> Result<(PathBuf, OsString), String> {
    let checkout_root = checkout_root
        .canonicalize()
        .map_err(|error| format!("could not resolve checkout for formatting: {error}"))?;
    let mut directories = Vec::new();
    for directory in std::env::split_paths(path) {
        let Ok(absolute) = std::path::absolute(&directory) else {
            continue;
        };
        if absolute.starts_with(&checkout_root) {
            continue;
        }
        let Ok(directory) = directory.canonicalize() else {
            continue;
        };
        if !directory.starts_with(&checkout_root) && !directories.contains(&directory) {
            directories.push(directory);
        }
    }
    let safe_path = std::env::join_paths(&directories)
        .map_err(|error| format!("could not prepare formatter PATH: {error}"))?;
    // The name is the outer loop, so a daemon installed late on PATH still wins over
    // a plain `prettier` installed early: that is the preference, not the order the
    // directories happen to be listed in.
    for name in FORMATTERS {
        for directory in &directories {
            let Ok(executable) = directory.join(name).canonicalize() else {
                continue;
            };
            if executable.starts_with(&checkout_root) {
                continue;
            }
            let Ok(metadata) = std::fs::metadata(&executable) else {
                continue;
            };
            if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
                return Ok((executable, safe_path));
            }
        }
    }
    Err("Neither prettierd nor prettier is installed as a tool of your own. `npm install -g @fsouza/prettierd` or `npm install -g prettier` puts one on PATH; a copy inside the checkout is deliberately not run.".into())
}

/// Run only the user's Prettier, with project config, editorconfig, plugins, and checkout-local
/// PATH entries excluded. Formatting is explicit and its output remains a draft in the editor.
fn run_prettier(
    checkout_root: &Path,
    path: &str,
    parser: &str,
    source: &str,
) -> Result<String, String> {
    if source.len() > muster_core::files::MAX_PREVIEW_BYTES {
        return Err(
            "Prettier cannot format a document larger than the editor's size limit.".into(),
        );
    }
    let (executable, safe_path) = prettier_executable(checkout_root)?;
    run_prettier_with(&executable, &safe_path, path, parser, source)
}

/// Whether this formatter is the daemon rather than the plain command line.
///
/// It is a question about the name because the daemon is chosen by name and the two
/// differ in one place only: `prettierd` is given the document on stdin and takes its
/// options as `--option=value`, because a bare word after a flag is a second file path
/// to it. Everything else — the captured copy, the PATH, the timeout — is one call.
fn is_daemon_formatter(executable: &Path) -> bool {
    executable.file_name() == Some(OsStr::new(FORMATTERS[0]))
}

/// A working directory for the formatter that is neither the checkout nor the copy's
/// own temporary one.
///
/// A daemon keys the process it keeps alive by the directory it was started in, so a
/// directory that changes with every format would start a daemon per click and leave
/// each one behind. This one is empty of anything a formatter could read a config from.
fn formatter_working_dir() -> Result<PathBuf, String> {
    let directory = std::env::temp_dir().join("muster-formatter");
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("could not prepare the formatter's directory: {error}"))?;
    Ok(directory)
}

fn run_prettier_with(
    executable: &Path,
    safe_path: &OsStr,
    path: &str,
    parser: &str,
    source: &str,
) -> Result<String, String> {
    let temp_dir = tempfile::tempdir()
        .map_err(|error| format!("could not prepare formatter input: {error}"))?;
    let mut input = tempfile::Builder::new()
        .prefix("muster-format-")
        .suffix(formatter_suffix(path))
        .tempfile_in(temp_dir.path())
        .map_err(|error| format!("could not prepare formatter input: {error}"))?;
    input
        .write_all(source.as_bytes())
        .map_err(|error| format!("could not write formatter input: {error}"))?;
    let daemon = is_daemon_formatter(executable);
    let parser_argument = format!("--parser={parser}");
    let mut command = Command::new(executable);
    command
        .current_dir(formatter_working_dir()?)
        .env("PATH", safe_path)
        .env_remove("NODE_OPTIONS")
        .env_remove("NODE_PATH");
    if daemon {
        command
            .args(["--no-config", "--no-editorconfig", &parser_argument])
            .arg(input.path())
            .stdin(Stdio::piped());
    } else {
        command
            .args(["--no-config", "--no-editorconfig", "--parser", parser])
            .arg(input.path())
            .stdin(Stdio::null());
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    use std::os::unix::process::CommandExt;
    command.process_group(0);
    let output = run_formatter_process(command, daemon.then_some(source))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if message.is_empty() {
            format!("Prettier exited with {}.", output.status)
        } else {
            message
        });
    }
    String::from_utf8(output.stdout).map_err(|_| "Prettier returned non-UTF-8 output.".into())
}

const FORMATTER_TIMEOUT: Duration = Duration::from_secs(20);
const FORMATTER_STDERR_LIMIT: usize = 64 * 1024;

#[derive(Debug)]
struct FormatterOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Run the formatter, handing it `stdin_source` when it reads the document that way.
///
/// The write happens before the streams are read and the daemon reads its input before
/// it answers, so the two cannot block each other; a formatter that died first takes the
/// write as its own failure rather than as a document it refused.
fn run_formatter_process(
    command: Command,
    stdin_source: Option<&str>,
) -> Result<FormatterOutput, String> {
    run_formatter_process_with_timeout(command, stdin_source, FORMATTER_TIMEOUT)
}

fn run_formatter_process_with_timeout(
    mut command: Command,
    stdin_source: Option<&str>,
    timeout: Duration,
) -> Result<FormatterOutput, String> {
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start Prettier: {error}"))?;
    if let Some(source) = stdin_source {
        let mut stdin = child.stdin.take().ok_or("Prettier stdin was not piped.")?;
        if let Err(error) = stdin.write_all(source.as_bytes()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("could not hand the document to Prettier: {error}"));
        }
    }
    let stdout = child
        .stdout
        .take()
        .ok_or("Prettier stdout was not piped.")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Prettier stderr was not piped.")?;
    let (sender, receiver) = mpsc::channel();
    let stdout_thread = read_formatter_stream(
        stdout,
        true,
        muster_core::files::MAX_PREVIEW_BYTES,
        sender.clone(),
    );
    let stderr_thread = read_formatter_stream(stderr, false, FORMATTER_STDERR_LIMIT, sender);
    let deadline = Instant::now() + timeout;
    let mut streams = [None, None];
    let mut status = None;
    let mut exited_pipe_deadline = None;
    let mut failure = None;

    while status.is_none() || streams.iter().any(Option::is_none) {
        while let Ok((is_stdout, result)) = receiver.try_recv() {
            let slot = if is_stdout { 0 } else { 1 };
            if result
                .as_ref()
                .is_err_and(|error| error.kind() == std::io::ErrorKind::FileTooLarge)
            {
                failure = Some(if is_stdout {
                    "Prettier produced output larger than the editor's size limit.".to_owned()
                } else {
                    "Prettier diagnostics exceeded the output limit.".to_owned()
                });
                break;
            }
            streams[slot] = Some(result);
        }
        if failure.is_some() {
            break;
        }
        match child.try_wait() {
            Ok(exit) => status = exit.or(status),
            Err(error) => {
                failure = Some(format!("could not wait for Prettier: {error}"));
                break;
            }
        }
        if Instant::now() >= deadline {
            failure = Some(format!(
                "Prettier timed out after {} seconds.",
                timeout.as_secs()
            ));
            break;
        }
        if status.is_some() && streams.iter().any(Option::is_none) {
            // Give the readers a short drain window before killing a descendant
            // that inherited either pipe from the formatter parent.
            let deadline = *exited_pipe_deadline
                .get_or_insert_with(|| Instant::now() + Duration::from_millis(100));
            if Instant::now() >= deadline {
                terminate_formatter_group(child.id());
            }
        }
        thread::sleep(Duration::from_millis(5));
    }

    if failure.is_some() {
        terminate_formatter_group(child.id());
        let _ = child.kill();
    }
    let final_status = child
        .wait()
        .map_err(|error| format!("could not reap Prettier: {error}"))?;
    let _ = stdout_thread.join();
    let _ = stderr_thread.join();
    if let Some(error) = failure {
        return Err(error);
    }
    let stdout = streams[0]
        .take()
        .ok_or("Prettier stdout was not collected.")?
        .map_err(|error| format!("could not read Prettier stdout: {error}"))?;
    let stderr = streams[1]
        .take()
        .ok_or("Prettier stderr was not collected.")?
        .map_err(|error| format!("could not read Prettier stderr: {error}"))?;
    Ok(FormatterOutput {
        status: status.unwrap_or(final_status),
        stdout,
        stderr,
    })
}

fn read_formatter_stream<R: Read + Send + 'static>(
    stream: R,
    is_stdout: bool,
    limit: usize,
    sender: mpsc::Sender<(bool, std::io::Result<Vec<u8>>)>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut bytes = Vec::with_capacity(limit.min(8192));
        let result = stream
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .and_then(|_| {
                if bytes.len() > limit {
                    Err(std::io::Error::from(std::io::ErrorKind::FileTooLarge))
                } else {
                    Ok(bytes)
                }
            });
        let _ = sender.send((is_stdout, result));
    })
}

fn terminate_formatter_group(process_id: u32) {
    if let Ok(process_id) = libc::pid_t::try_from(process_id) {
        // SAFETY: a negative PID targets only the dedicated formatter process group.
        unsafe { libc::kill(-process_id, libc::SIGKILL) };
    }
}

fn filename_language(name: &str) -> Option<&'static str> {
    Some(match name {
        "makefile" | "gnumakefile" => "make",
        "cmakelists.txt" => "cmake",
        ".bash_login" | ".bash_logout" | ".bash_profile" | ".bashrc" | ".profile" | ".shrc"
        | ".cshrc" | ".kshrc" | ".login" | ".tcshrc" | ".zlogin" | ".zlogout" | ".zprofile"
        | ".zshenv" | ".zshrc" => "bash",
        ".babelrc" | ".eslintrc" | ".jshintrc" | ".prettierrc" | ".swcrc" | ".watchmanconfig" => {
            "json"
        }
        ".curlrc" | ".dircolors" | ".editorconfig" | ".gitattributes" | ".gitconfig"
        | ".gitmodules" | ".inputrc" | ".npmrc" | ".selected_editor" | ".tmux.conf" | ".yarnrc" => {
            "ini"
        }
        ".cvsignore" | ".dockerignore" | ".eslintignore" | ".gitignore" | ".hgignore"
        | ".npmignore" | ".prettierignore" => "gitignore",
        ".psqlrc" => "sql",
        _ => return None,
    })
}

/// Identity for a language choice; native documents currently come from checkouts only.
fn document_language_key(checkout: &str, path: &str) -> String {
    format!("{checkout}\0checkout\0{path}")
}

fn language_label(language: &str) -> String {
    match language {
        "bash" => "Shell".into(),
        "cmake" => "CMake".into(),
        "cpp" => "C++".into(),
        "csharp" => "C#".into(),
        "css" => "CSS".into(),
        "dotenv" => "Dotenv".into(),
        "ejs" => "EJS".into(),
        "erb" => "ERB".into(),
        "graphql" => "GraphQL".into(),
        "gitignore" => "Gitignore".into(),
        "html" => "HTML".into(),
        "ini" => "INI".into(),
        "javascript" => "JavaScript".into(),
        "json" => "JSON".into(),
        "php" => "PHP".into(),
        "proto" => "Protocol Buffers".into(),
        "sql" => "SQL".into(),
        "toml" => "TOML".into(),
        "tsx" => "TSX".into(),
        "typescript" => "TypeScript".into(),
        "vue" => "Vue".into(),
        "yaml" => "YAML".into(),
        "zig" => "Zig".into(),
        _ => language
            .split(['-', '_'])
            .map(|part| {
                let mut chars = part.chars();
                chars
                    .next()
                    .map(|first| first.to_uppercase().chain(chars).collect::<String>())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn available_grammars() -> Vec<String> {
    let mut languages = gpui_kit::component::highlighter::LanguageRegistry::singleton()
        .languages()
        .into_iter()
        .map(|language| language.to_string())
        .filter(|language| language != "plaintext")
        .collect::<Vec<_>>();
    languages.sort_by_key(|language| language_label(language).to_lowercase());
    languages
}

fn document_language_label(path: &str, override_language: Option<&str>) -> String {
    match override_language {
        Some("plaintext") => "Plain text".into(),
        Some(language) => language_label(language),
        None => {
            let detected = syntax_language(path);
            if detected == "plaintext" {
                "Auto (no highlighting)".into()
            } else {
                language_label(detected)
            }
        }
    }
}

/// One row's height, and the number that scrolls the list.
///
/// Legacy's menu row is 5px of padding over an 18px line, and this menu draws
/// the same row, so the height is a constant rather than something measured:
/// the keyboard's highlight has to be scrolled into view without a frame to
/// measure it in.
const GRAMMAR_ROW_PX: f32 = 28.;

/// Every grammar a file can wear, in the order the menu lists them.
///
/// `Auto` is first because it is what the file already wears, and it is a row
/// like any other: the search above the list filters all of it, so somebody
/// who wants plain text types for it instead of reaching past the list for a
/// row that is not in it.
fn grammar_choices(path: &str) -> Vec<(String, Option<String>)> {
    let detected = syntax_language(path);
    let auto = if detected == "plaintext" {
        "Auto · no highlighting".to_owned()
    } else {
        format!("Auto · {}", language_label(detected))
    };
    std::iter::once((auto, None))
        .chain(std::iter::once((
            "Plain text".to_owned(),
            Some("plaintext".to_owned()),
        )))
        .chain(
            available_grammars()
                .into_iter()
                .map(|language| (language_label(&language), Some(language))),
        )
        .collect()
}

/// The rows of `choices` a query leaves, by index.
///
/// A row answers to its written name and to the name the registry holds it
/// under, so `yml` finds YAML and `plain` finds Plain text. A query that
/// matches nothing leaves nothing: an empty list is the answer, not every row.
fn matching_grammar_rows(choices: &[(String, Option<String>)], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    choices
        .iter()
        .enumerate()
        .filter(|(_, (label, language))| {
            query.is_empty()
                || label.to_lowercase().contains(&query)
                || language.as_ref().is_some_and(|name| name.contains(&query))
        })
        .map(|(index, _)| index)
        .collect()
}

/// What a row in the grammar menu acts on: the window that owns it, the
/// checkout and the path of the file it is a grammar for.
struct GrammarTarget {
    view: gpui_kit::WeakEntity<TerminalWindow>,
    checkout: String,
    path: String,
}

/// The grammar picker: a search field over the grammars, drawn here.
///
/// The toolkit's command palette answers a query of its own, and it draws its
/// search field above whatever a header holds, which put `Auto` and `Plain
/// text` above the field that filters them. A menu whose search is not its first
/// thing is not a menu anybody reads the way they expect, so the list, the
/// rows and the search are drawn here: the search on top, `Auto` as the first
/// row, and every row carrying the pointer, the hover and the keyboard that the
/// rest of this window's rows carry.
fn grammar_menu(
    query: &Entity<InputState>,
    scroll: &ScrollHandle,
    override_language: Option<&str>,
    highlighted: usize,
    roles: Roles,
    target: GrammarTarget,
    cx: &mut App,
) -> AnyElement {
    let choices = grammar_choices(&target.path);
    let GrammarTarget {
        view,
        checkout,
        path: selected_path,
    } = target;
    let matched = matching_grammar_rows(&choices, &query.read(cx).value());
    // The keyboard cannot be past what the query left, and an empty list has
    // nothing to point at.
    let highlighted = highlighted.min(matched.len().saturating_sub(1));
    let chosen = override_language.map(str::to_owned);
    let rows = matched
        .iter()
        .enumerate()
        .map(|(position, index)| {
            let (label, language) = &choices[*index];
            let id = language.as_deref().unwrap_or("auto");
            let selected = position == highlighted;
            let is_choice = language.as_deref() == chosen.as_deref();
            let language = language.clone();
            let label = label.clone();
            let accessibility_label =
                format!("{}{}", label, if is_choice { ", selected" } else { "" });
            div()
                .id(SharedString::from(format!("document-grammar-{id}")))
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .h(px(GRAMMAR_ROW_PX))
                .px_2()
                .rounded(px(4.))
                .font_family(crate::app::UI_FONT_FAMILY)
                .text_size(px(13.))
                .line_height(px(18.))
                .text_color(roles.text)
                .role(Role::ListBoxOption)
                .aria_label(accessibility_label)
                .aria_selected(selected)
                .when(selected, |row| row.bg(roles.selected))
                .hover(|style| {
                    style.bg(if selected {
                        roles.selected_hover
                    } else {
                        roles.hover
                    })
                })
                .child(div().flex_1().min_w_0().text_ellipsis().child(label))
                .when(is_choice, |row| {
                    row.child(Icon::new(IconName::Check).size_3().text_color(roles.accent))
                })
                .on_click({
                    let view = view.clone();
                    let checkout = checkout.clone();
                    let selected_path = selected_path.clone();
                    move |_, window, cx| {
                        let _ = view.update(cx, |app, cx| {
                            app.set_document_language(
                                &checkout,
                                &selected_path,
                                language.clone(),
                                window,
                                cx,
                            );
                        });
                    }
                })
                .test_support()
                .into_any_element()
        })
        .collect::<Vec<_>>();
    div()
        .flex()
        .flex_col()
        .bg(roles.surface_raised)
        .on_key_down({
            let view = view.clone();
            let query = query.clone();
            let scroll = scroll.clone();
            let choices = choices.clone();
            move |event, _window, cx| {
                let query = query.clone();
                let scroll = scroll.clone();
                let choices = choices.clone();
                let _ = view.update(cx, move |app, cx| {
                    let rows = matching_grammar_rows(&choices, &query.read(cx).value());
                    let mut position = app
                        .document_grammar_highlight
                        .min(rows.len().saturating_sub(1));
                    match event.keystroke.key.as_ref() {
                        "down" => position = (position + 1).min(rows.len().saturating_sub(1)),
                        "up" => position = position.saturating_sub(1),
                        _ => {}
                    }
                    app.document_grammar_highlight = position;
                    scroll_grammar_row_into_view(&scroll, position);
                    cx.notify();
                });
            }
        })
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .px_3()
                .py(px(6.))
                .border_b_1()
                .border_color(roles.border)
                .child(
                    Icon::new(IconName::Search)
                        .size_3()
                        .text_color(roles.text_muted),
                )
                .child(
                    Input::new(query)
                        .id("document-grammar-search")
                        .appearance(false)
                        .p_0()
                        .w_full()
                        .text_size(px(13.))
                        .aria_label("Search grammars"),
                ),
        )
        .child(
            div()
                .id("document-grammar-list")
                .flex()
                .flex_col()
                .max_h(px(240.))
                .overflow_y_scroll()
                .track_scroll(scroll)
                .px_1()
                .children(rows)
                .test_support(),
        )
        .into_any_element()
}

/// Keep the highlighted row inside the visible part of the list.
///
/// Rows are a fixed height and the list is a plain scroll container, so where
/// the keyboard is inside the content is its row times that height, and no
/// layout has to be read back to know it.
fn scroll_grammar_row_into_view(scroll: &ScrollHandle, row: usize) {
    let offset = scroll.offset();
    let top = px(GRAMMAR_ROW_PX * row as f32);
    let bottom = top + px(GRAMMAR_ROW_PX);
    let visible = offset.y..offset.y + scroll.bounds().size.height;
    let wanted = if top < visible.start {
        top
    } else if bottom > visible.end {
        bottom - scroll.bounds().size.height
    } else {
        offset.y
    };
    if wanted != offset.y {
        scroll.set_offset(point(offset.x, wanted));
    }
}

/// The document's body: the reading, or the source.
///
/// Both fill the pane and scroll themselves, because a document taller than the
/// pane has to be readable from the middle of it. A scrollable text view keeps
/// its own scrollbar; the editor keeps the one it draws over its own viewport.
/// Neither scrolls inside a container here: a container whose only child fills
/// it has nothing to scroll, which is what a Markdown preview without a
/// `scrollable` view is.
///
/// The margin is the pane's, not either reading's, so a document is read the
/// same way whichever of the two is on screen.
fn document_body(
    state: &Entity<EditorState>,
    rendered: Option<Entity<TextViewState>>,
    media: Option<media::DocumentMedia>,
    link_handler: impl Fn(&gpui_kit::SharedString, &gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App)
        + Send
        + Sync
        + 'static,
) -> AnyElement {
    let reading = match rendered {
        Some(preview) => {
            let mut extensions = MarkdownExtensions::default()
                .frontmatter()
                .plugin(FrontmatterPlugin::new());
            // A document's own pictures are answered through the checkout it was
            // read from, and everything it names that is not a picture is said in
            // words instead: see `document::media`.
            if let Some(media) = media {
                extensions = extensions.plugin(media);
            }
            div()
                .id("document-markdown")
                .test_support()
                .size_full()
                .child(
                    TextView::new(&preview)
                        // Scrolling its own content is the text view's, and the
                        // only thing that has the height of the document to
                        // measure it against.
                        .scrollable(true)
                        .size_full()
                        // Legacy's front matter was a collapsible tree above
                        // the document, drawn from the YAML rather than shown
                        // as a wall of keys. This is the toolkit's rendering
                        // of the same node.
                        .markdown_extensions(extensions)
                        .on_link_click(link_handler),
                )
                .into_any_element()
        }
        None => Editor::new(state)
            .readonly(false)
            .bordered(false)
            .size_full()
            .into_any_element(),
    };
    div()
        .size_full()
        .min_w_0()
        .min_h_0()
        .px_6()
        .py_5()
        .child(reading)
        .into_any_element()
}

impl TerminalWindow {
    /// The repository and checkout one document is read from and written to.
    ///
    /// The pair a read or a save is walked against, so it is asked once and in one
    /// place: a document and its pictures have to reach the same checkout, and a
    /// document that could be read through one checkout and written through another
    /// would be a file nobody can reason about.
    pub(crate) fn document_source(&self, checkout: &str) -> Option<(Repo, Checkout)> {
        let repo = self
            .navigation
            .repos
            .iter()
            .find(|repo| repo.checkouts.iter().any(|one| one.id == checkout))?
            .clone();
        Some((repo, self.navigation.checkout(checkout)?.clone()))
    }

    /// Somebody typed in the open document: whether it is now a draft.
    ///
    /// Asked of the editor's text rather than of a change event, so an undo back
    /// to what the file holds is the end of a draft rather than a state a draft
    /// can be stuck in.
    pub(crate) fn refresh_document_draft(&mut self, cx: &App) {
        let Some(view) = self.document_view.as_mut() else {
            return;
        };
        view.dirty = *view.state.read(cx).text() != view.loaded.as_str();
    }

    /// Keep one destructive destination until its Save / Discard / Cancel decision resolves.
    pub(crate) fn request_document_transition(
        &mut self,
        transition: crate::app::PendingDocumentTransition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_document_transition.is_none() {
            self.pending_document_transition = Some(transition);
        }
        self.continue_document_transition(window, cx);
    }

    /// Apply a clean destination or ask for a decision once background work is idle.
    ///
    /// The decision is about leaving this document, so it belongs to a
    /// destination: a completion that only finishes work where the window already
    /// is — a format, or a save nobody queued a destination for — reaches this
    /// helper with nothing pending, and asking there would put a draft in front of
    /// a Save / Discard / Cancel choice about a navigation nobody asked for.
    pub(crate) fn continue_document_transition(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_document_transition.is_none() {
            return;
        }
        self.refresh_document_draft(cx);
        let Some(document) = self.document_view.as_ref() else {
            if let Some(transition) = self.pending_document_transition.take() {
                transition(self, window, cx);
            }
            return;
        };
        if document.saving
            || document.formatting
            || document.durability_uncertain
            || self.document_transition_prompted
        {
            return;
        }
        if !document.dirty {
            let transition = self.pending_document_transition.take();
            if let Some(transition) = transition {
                transition(self, window, cx);
            }
            return;
        }
        self.document_transition_prompted = true;
        let prompt = window.prompt(
            gpui_kit::PromptLevel::Warning,
            "Save changes to this document?",
            Some("Your edits will be lost if you discard them."),
            &["Cancel", "Save", "Discard"],
            cx,
        );
        cx.spawn_in(window, async move |view, cx| {
            let choice = prompt.await;
            let _ = view.update_in(cx, |app, window, cx| {
                app.document_transition_prompted = false;
                match choice {
                    Ok(1) => app.save_document(window, cx),
                    Ok(2) => {
                        let transition = app.pending_document_transition.take();
                        app.clear_document();
                        if let Some(transition) = transition {
                            transition(app, window, cx);
                        }
                    }
                    _ => app.pending_document_transition = None,
                }
            });
        })
        .detach();
    }

    /// Write the open document back, or say why it cannot be written.
    ///
    /// What is written is the editor's own text and the opened text is compared
    /// against the current target, then its identity and bytes are checked again
    /// immediately before publication. POSIX rename still leaves a narrow race
    /// with a non-cooperating writer, and a post-rename sync failure means the
    /// bytes are visible but durability is uncertain. The answer is waited for
    /// off the UI thread, and lands on the document only
    /// if that document is still the one on screen.
    pub(crate) fn save_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_document_draft(cx);
        let Some((checkout_id, path)) = self.document_target.clone() else {
            return;
        };
        let Some((repo, checkout)) = self.document_source(&checkout_id) else {
            return;
        };
        let (expected, content) = match self.document_view.as_ref().filter(|view| {
            (view.dirty || view.durability_uncertain) && !view.saving && !view.formatting
        }) {
            Some(view) => (view.loaded.clone(), view.state.read(cx).text().to_string()),
            None => return,
        };
        if let Some(view) = self.document_view.as_mut() {
            view.saving = true;
        }
        let token = self.document_request;
        let written = (checkout_id.clone(), path.clone(), content.clone());
        let worker = cx.background_executor().spawn(async move {
            muster_core::files::write_preview(&repo, &checkout, &path, &expected, &content)
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let (checkout_id, path, content) = written;
            let _ = view.update_in(cx, |app, window, cx| {
                // The document this save was for is not the one on screen.
                if app.document_request != token {
                    return;
                }
                let Some(document) = app.document_view.as_mut() else {
                    return;
                };
                document.saving = false;
                let mut transition = None;
                let mut reconsider_transition = false;
                let mut refresh_git_status = false;
                let refusal;
                match result {
                    Ok(()) => {
                        let current = document.state.read(cx).text().to_string();
                        document.loaded = content.clone();
                        document.durability_uncertain = false;
                        document.dirty = current != content;
                        let saved_cleanly = !document.dirty;
                        refusal = document.dirty.then(|| {
                            "The document changed while saving; the newer edits remain unsaved."
                                .to_owned()
                        });
                        // The reading is a parse of the text as saved, and the
                        // saved text is the only one it can be right about.
                        if is_markdown(&path) {
                            document.preview =
                                Some(cx.new(|cx| TextViewState::markdown(&content, cx)));
                        }
                        // What was loaded is what the pane draws, so a document
                        // reopened from here is the one that was saved.
                        if let Some(preview) = app
                            .open_document
                            .as_mut()
                            .filter(|(id, preview)| *id == checkout_id && preview.path == path)
                            .map(|(_, preview)| preview)
                        {
                            preview.text = content;
                        }
                        // The file is not what it was: what the Changes view will
                        // show about it has to be asked again.
                        app.git_diff.remove(&(checkout_id.clone(), path.clone()));
                        app.git_diff_request
                            .remove(&(checkout_id.clone(), path.clone()));
                        app.request_git_status(&checkout_id, cx);
                        if saved_cleanly {
                            transition = app.pending_document_transition.take();
                        } else {
                            reconsider_transition = app.pending_document_transition.is_some();
                        }
                        cx.notify();
                    }
                    Err(muster_core::files::WritePreviewError::PublishedButNotDurable(error)) => {
                        // The rename is visible, but durability was not confirmed. Treat
                        // those bytes as the new baseline so Retry cannot overwrite a
                        // newer external edit using the stale pre-save expectation.
                        document.loaded = content.clone();
                        document.durability_uncertain = true;
                        document.dirty = *document.state.read(cx).text() != content.as_str();
                        if is_markdown(&path) {
                            document.preview =
                                Some(cx.new(|cx| TextViewState::markdown(&content, cx)));
                        }
                        if let Some(preview) = app
                            .open_document
                            .as_mut()
                            .filter(|(id, preview)| *id == checkout_id && preview.path == path)
                            .map(|(_, preview)| preview)
                        {
                            preview.text = content.clone();
                        }
                        app.git_diff.remove(&(checkout_id.clone(), path.clone()));
                        app.git_diff_request
                            .remove(&(checkout_id.clone(), path.clone()));
                        refresh_git_status = true;
                        refusal = Some(format!(
                            "Save is visible but may not survive a system failure: {error}"
                        ));
                        cx.notify();
                    }
                    Err(muster_core::files::WritePreviewError::BeforePublication(error)) => {
                        refusal = Some(error);
                        cx.notify();
                    }
                }
                if let Some(message) = refusal {
                    app.push_error(message, cx);
                }
                if refresh_git_status {
                    app.request_git_status(&checkout_id, cx);
                }
                if let Some(transition) = transition {
                    transition(app, window, cx);
                } else if reconsider_transition {
                    app.continue_document_transition(window, cx);
                }
            });
        })
        .detach();
    }

    /// Format the editor's captured text off the UI thread, and accept the answer
    /// only if the same document, language and text are still on screen.
    pub(crate) fn format_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_document_draft(cx);
        let Some((checkout_id, path)) = self.document_target.clone() else {
            return;
        };
        let Some((_, checkout)) = self.document_source(&checkout_id) else {
            return;
        };
        let language_key = document_language_key(&checkout_id, &path);
        let language = self
            .document_language_overrides
            .get(&language_key)
            .map(String::as_str)
            .unwrap_or_else(|| syntax_language(&path))
            .to_owned();
        let Some(parser) = prettier_parser(&path, &language) else {
            return;
        };
        let request = self.document_request;
        let identity = format!("{checkout_id}\0{path}\0{request}");
        let Some(document) = self
            .document_view
            .as_mut()
            .filter(|view| view.path == identity)
        else {
            return;
        };
        if document.saving || document.formatting {
            return;
        }
        let source = document.state.read(cx).text().to_string();
        document.formatting = true;
        document.format_generation = document.format_generation.wrapping_add(1);
        let generation = document.format_generation;
        let checkout_root = PathBuf::from(checkout.canonical_path);
        let path_for_format = path.clone();
        let language_key = language_key.clone();
        let language_for_check = language.clone();
        let source_for_format = source.clone();
        let source_for_check = source.clone();
        let worker = cx.background_executor().spawn(async move {
            run_prettier(&checkout_root, &path_for_format, parser, &source_for_format)
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = worker.await;
            let mut install = None;
            let mut refusal = None;
            let _ = view.update_in(cx, |app, _, cx| {
                if !formatter_navigation_is_current(app.document_request, request) {
                    return;
                }
                let current_language = app
                    .document_language_overrides
                    .get(&language_key)
                    .map(String::as_str)
                    .unwrap_or_else(|| syntax_language(&path))
                    .to_owned();
                let Some(document) = app
                    .document_view
                    .as_mut()
                    .filter(|document| document.path == identity)
                else {
                    return;
                };
                if document.format_generation != generation {
                    return;
                }
                let current = document.state.read(cx).text().to_string();
                if !formatter_result_is_current(
                    &current,
                    &source_for_check,
                    &current_language,
                    &language_for_check,
                ) {
                    document.formatting = false;
                    document.dirty = current != document.loaded;
                    refusal = Some(
                        "The document changed while formatting; the result was discarded."
                            .to_owned(),
                    );
                    cx.notify();
                    return;
                }
                match result {
                    Ok(formatted) if formatted == current => {
                        document.formatting = false;
                        document.dirty = formatted != document.loaded;
                        cx.notify();
                    }
                    Ok(formatted) => {
                        install = Some((document.state.clone(), formatted));
                    }
                    Err(error) => {
                        document.formatting = false;
                        refusal = Some(error);
                        cx.notify();
                    }
                }
            });
            if let Some(message) = refusal {
                let _ = view.update(cx, |app, cx| app.push_error(message, cx));
            }
            if let Some((state, formatted)) = install {
                let expected = source_for_check;
                let next = formatted.clone();
                let installed = state
                    .update_in(cx, move |state, window, cx| {
                        if *state.text() != expected.as_str() {
                            return false;
                        }
                        state.replace_all(next, window, cx);
                        true
                    })
                    .unwrap_or(false);
                let _ = view.update(cx, |app, cx| {
                    if app.document_request != request {
                        return;
                    }
                    let Some(document) = app.document_view.as_mut().filter(|document| {
                        document.path == identity && document.format_generation == generation
                    }) else {
                        return;
                    };
                    document.formatting = false;
                    let mut discarded = None;
                    if installed {
                        document.dirty = formatted != document.loaded;
                    } else {
                        let current = document.state.read(cx).text().to_string();
                        document.dirty = current != document.loaded;
                        discarded = Some(
                            "The document changed while formatting; the result was discarded."
                                .to_owned(),
                        );
                    }
                    cx.notify();
                    if let Some(message) = discarded {
                        app.push_error(message, cx);
                    }
                });
            }
            let _ = view.update_in(cx, |app, window, cx| {
                if app.document_request == request {
                    app.continue_document_transition(window, cx);
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Watch the grammar menu's search field for as long as the menu is open.
    ///
    /// A typed character re-filters the list, so the row the keyboard was on
    /// is no longer a row: the highlight goes back to the top of what is left.
    /// Return is not answered here: the window's keystroke interceptor takes it
    /// before the field and the popover both see it, because both of those
    /// close the menu rather than choose a row.
    pub(crate) fn watch_document_grammar_query(
        &mut self,
        query: &Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.document_grammar_query = Some(query.clone());
        self.document_grammar_query_seen = query.read(cx).value().to_string();
        self.document_grammar_query_watch =
            Some(
                cx.subscribe_in(query, window, |app, _, event, window, cx| match event {
                    InputEvent::Change => {
                        app.document_grammar_highlight = 0;
                        cx.notify();
                    }
                    InputEvent::PressEnter {
                        secondary: false, ..
                    } => {
                        app.confirm_document_grammar(window, cx);
                    }
                    _ => {}
                }),
            );
    }

    /// Wear the grammar the keyboard is on, and close the menu.
    ///
    /// The rows are the ones the search left, in the same order the menu drew
    /// them, so the row the highlight counts is the row the person sees.
    pub(crate) fn confirm_document_grammar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A closed menu has no row to confirm, and the popover closes before
        // its own Return reaches here when the field is not the focused one.
        if !self.document_grammar_open {
            return;
        }
        let Some((checkout, path)) = self.document_target.clone() else {
            return;
        };
        let Some(query) = self.document_grammar_query.as_ref() else {
            return;
        };
        let query = query.read(cx).value().to_string();
        let choices = grammar_choices(&path);
        let rows = matching_grammar_rows(&choices, &query);
        let Some(language) = rows
            .get(
                self.document_grammar_highlight
                    .min(rows.len().saturating_sub(1)),
            )
            .map(|row| choices[*row].1.clone())
        else {
            return;
        };
        self.set_document_language(&checkout, &path, language, window, cx);
    }

    fn set_document_language(
        &mut self,
        checkout: &str,
        path: &str,
        language: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = document_language_key(checkout, path);
        if let Some(language) = language.as_ref() {
            self.document_language_overrides
                .insert(key, language.clone());
        } else {
            self.document_language_overrides.remove(&key);
        }
        if self
            .document_target
            .as_ref()
            .is_some_and(|(active_checkout, active_path)| {
                active_checkout == checkout && active_path == path
            })
        {
            if let Some(view) = self.document_view.as_ref() {
                let language = language.as_deref().unwrap_or_else(|| syntax_language(path));
                view.state
                    .update(cx, |state, cx| state.set_highlighter(language, cx));
                view.state.read(cx).focus_handle(cx).focus(window, cx);
            }
        }
        self.document_grammar_open = false;
        cx.notify();
    }

    /// Throw the draft away and put the text the file was read with back.
    ///
    /// The editor, not the file: this is what a person means by Cancel, and the
    /// file has not been written yet, so there is nothing on disk to restore.
    pub(crate) fn cancel_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_document_draft(cx);
        let Some(view) = self
            .document_view
            .as_ref()
            .filter(|view| view.dirty && !view.saving)
        else {
            return;
        };
        let (state, loaded) = (view.state.clone(), view.loaded.clone());
        let Some(document) = self.document_view.as_mut() else {
            return;
        };
        document.dirty = false;
        document.formatting = false;
        document.format_generation = document.format_generation.wrapping_add(1);
        // Putting the text back is an edit the toolkit answers through the
        // window the editor is drawn in, so that is where it is asked for.
        cx.spawn_in(window, async move |_, cx| {
            let _ = state.update_in(cx, |state, window, cx| {
                state.replace_all(loaded, window, cx);
                state.focus_handle(cx).focus(window, cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// The two keys a draft answers, and nothing else.
    ///
    /// `⌘S` saves it and Escape throws it away, both only while there is a draft:
    /// a document nobody has edited keeps the Escape that closes the pane and the
    /// key that nobody asked for.
    pub(crate) fn document_edit_key(
        &mut self,
        key: &gpui_kit::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.document_view.as_ref().is_none_or(|view| view.saving) {
            return false;
        }
        self.refresh_document_draft(cx);
        if self.document_view.as_ref().is_none_or(|view| !view.dirty) {
            return false;
        }
        let app_modifier = if cfg!(target_os = "macos") {
            key.modifiers.platform && !key.modifiers.control
        } else {
            key.modifiers.control && !key.modifiers.platform
        };
        if app_modifier && !key.modifiers.alt && key.key.eq_ignore_ascii_case("s") {
            self.save_document(window, cx);
            return true;
        }
        if key.key == "escape" {
            self.cancel_document(window, cx);
            return true;
        }
        false
    }

    pub(crate) fn clear_document(&mut self) {
        // Every gesture that replaces what this window shows comes through here
        // first, so this is where a newer view selection is noted. A decision
        // waiting on Git may only act while nothing newer has been asked for.
        self.view_intent = self.view_intent.wrapping_add(1);
        if let Some(key) = self.document_target.take() {
            self.file_loading.remove(&key);
            self.document_errors.remove(&key);
        }
        self.open_document = None;
        self.document_view = None;
        self.pending_document_transition = None;
        self.document_transition_prompted = false;
        self.document_grammar_open = false;
        self.document_grammar_query = None;
        self.document_grammar_query_watch = None;
        self.document_grammar_query_seen.clear();
        self.document_grammar_highlight = 0;
        self.document_request = self.document_request.wrapping_add(1);
    }

    pub(crate) fn document_element(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let target = self.document_target.clone().or_else(|| {
            self.open_document
                .as_ref()
                .map(|(id, preview)| (id.clone(), preview.path.clone()))
        });
        let Some((checkout_id, path)) = target else {
            self.document_view = None;
            return None;
        };
        if self.navigation.active_checkout.as_ref() != Some(&checkout_id) {
            return None;
        }
        let roles = Roles::of(cx);
        let key = (checkout_id.clone(), path.clone());
        let grammar_key = document_language_key(&checkout_id, &path);
        // What a frame draws is what the editor holds, so the draft is settled
        // before anything asks about it.
        self.refresh_document_draft(cx);
        if let Some(view) = self.document_view.as_mut().filter(|view| view.dirty) {
            // A draft is edited in its source. The reading is a parse of the text
            // as it was read, so drawing it now would show a document the editor
            // no longer holds.
            view.rendered = false;
        }
        // Opened where it was clicked: the handler carries this window weakly
        // into the toolkit's text view and dispatches from the click itself.
        let link_handler = link_handler(cx.entity().downgrade());
        let body = if let Some((_, preview)) = self
            .open_document
            .as_ref()
            .filter(|(id, preview)| id == &checkout_id && preview.path == path)
        {
            let identity = format!("{checkout_id}\0{path}\0{}", self.document_request);
            let new_view = self
                .document_view
                .as_ref()
                .is_none_or(|view| view.path != identity);
            let language = self
                .document_language_overrides
                .get(&grammar_key)
                .cloned()
                .unwrap_or_else(|| syntax_language(&path).to_owned());
            let state = document_state(
                &mut self.document_view,
                &identity,
                &path,
                &preview.text,
                &language,
                window,
                cx,
            );
            if new_view {
                self.document_grammar_open = false;
                // The header asks whether this file has changes, and the answer is
                // Git's rather than the maintenance tick's: the file this pane is
                // drawing is the one that has to be asked about, and a status read
                // from before it was edited says nothing about it. The tick stays
                // what it is — the one that refreshes.
                self.request_git_status(&checkout_id, cx);
                // Only the reading that is on screen takes the keyboard: a
                // Markdown document opens as its reading, so the editor behind
                // it stays unfocused until the header asks for the source.
                //
                // The keyboard is asked for at the end of this cycle, and not
                // now: the reading was built in this very frame, and the
                // toolkit only tells an editor it is focused once a frame has
                // reported it, which is a report this editor's own listener is
                // not yet around to hear.
                self.document_view
                    .as_ref()
                    .expect("just installed")
                    .focus_visible_after_this_frame(window, cx);
            }
            // A Markdown file is read as a document and can be read as its source;
            // everything else is its source and only its source.
            let rendered = self
                .document_view
                .as_ref()
                .filter(|view| view.is_rendered())
                .and_then(|view| view.preview.clone());
            // The pictures a rendered document names are reached through the same
            // checkout the document itself was read from, so a `../` that would
            // leave it is refused where every other path out of it is refused.
            let media = rendered.as_ref().and_then(|_| {
                let (repo, checkout) = self.document_source(&checkout_id)?;
                Some(media::DocumentMedia::new(
                    repo,
                    checkout,
                    &path,
                    Arc::downgrade(&self.document_view.as_ref()?.media),
                ))
            });
            if let Some(media) = media.as_ref() {
                // This frame says what the document already holds and starts the
                // reads its last parse asked for; the bytes arrive later, off the
                // UI thread, and ask for a frame of their own when they do.
                media.prepare(cx.entity().downgrade(), cx);
            }
            document_body(&state, rendered, media, link_handler)
        } else if let Some(error) = self.document_errors.get(&key) {
            let retry_path = path.clone();
            let retry_checkout = checkout_id.clone();
            // A file the pane cannot draw is not a file nobody can open: this is the one
            // place a refusal has something to offer instead. What it cannot be
            // is a file outside the checkout — the opener resolves the path
            // through the checkout again, and refuses it there if it is not there.
            let open_repo = self.document_source(&checkout_id).map(|(repo, _)| repo);
            let open_checkout = self.navigation.checkout(&checkout_id).cloned();
            div()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_color(roles.status.warning).child(error.clone()))
                .child(
                    zed_button(format!("retry-read-{path}"))
                        .ghost()
                        .small()
                        .label("Retry")
                        .accessibility_label(format!("Retry reading {path}"))
                        .on_click(cx.listener(move |app, _, window, cx| {
                            let checkout = app.navigation.checkout(&retry_checkout).cloned();
                            let repo = app
                                .navigation
                                .repos
                                .iter()
                                .find(|repo| repo.checkouts.iter().any(|c| c.id == retry_checkout))
                                .cloned();
                            if let (Some(repo), Some(checkout)) = (repo, checkout) {
                                app.request_document(
                                    repo,
                                    checkout,
                                    retry_path.clone(),
                                    window,
                                    cx,
                                );
                            }
                        })),
                )
                .when(open_repo.is_some() && open_checkout.is_some(), |pane| {
                    let repo = open_repo.clone().expect("asked for");
                    let checkout = open_checkout.clone().expect("asked for");
                    let opened = path.clone();
                    pane.child(
                        zed_button(format!("open-externally-{path}"))
                            .ghost()
                            .small()
                            .label("Open externally")
                            .accessibility_label(format!("Open {path} in another application"))
                            .on_click(cx.listener(move |app, _, window, cx| {
                                app.open_externally(
                                    repo.clone(),
                                    checkout.clone(),
                                    opened.clone(),
                                    window,
                                    cx,
                                );
                            })),
                    )
                })
                .into_any_element()
        } else {
            div()
                .id("document-loading")
                .p_3()
                .text_color(roles.text_muted)
                .child("Loading file…")
                .into_any_element()
        };
        // A Markdown document is rendered by the toolkit's own text view and not
        // by the editor: the toolkit's view draws headings, lists, tables and
        // fences the way Legacy's preview did, and its fences are coloured by the
        // `muster-content` theme this window already installed. The header says
        // which reading is on the screen and offers the other one.
        let markdown = self
            .document_view
            .as_ref()
            .is_some_and(|view| view.preview.is_some());
        let rendered = self
            .document_view
            .as_ref()
            .is_some_and(DocumentView::is_rendered);
        // The changes this file has, in Git's own words: a file with none offers
        // nothing to see, and one whose change set has not been read is not a file
        // that can say it has none.
        let changes = self
            .git_status
            .get(&checkout_id)
            .filter(|status| status.files.iter().any(|file| file.path == path))
            .map(|status| {
                let file = status
                    .files
                    .iter()
                    .find(|file| file.path == path)
                    .expect("asked for");
                (file.additions, file.deletions)
            });
        // Code always has its Format footer. Save and Cancel appear for a draft;
        // Cancel is only for edits, while an uncertain published save offers retry.
        let identity = format!("{checkout_id}\0{path}\0{}", self.document_request);
        let code_footer = self
            .document_view
            .as_ref()
            .filter(|view| view.path == identity && !rendered)
            .map(|view| {
                (
                    view.dirty,
                    view.dirty || view.durability_uncertain,
                    view.saving,
                    view.formatting,
                    self.pending_document_transition.is_some(),
                )
            });
        let format_language = self
            .document_language_overrides
            .get(&grammar_key)
            .map(String::as_str)
            .unwrap_or_else(|| syntax_language(&path))
            .to_owned();
        let format_supported = prettier_parser(&path, &format_language).is_some();
        let grammar_selector = if !rendered && self.document_view.is_some() {
            let override_language = self.document_language_overrides.get(&grammar_key).cloned();
            let trigger_label = document_language_label(&path, override_language.as_deref());
            let query = window.use_keyed_state(
                format!("document-grammar-query-{grammar_key}"),
                cx,
                InputState::new,
            );
            let scroll = window.use_keyed_state(
                format!("document-grammar-scroll-{grammar_key}"),
                cx,
                |_, _| ScrollHandle::new(),
            );
            let focus = query.read(cx).focus_handle(cx);
            let view = cx.weak_entity();
            Some(
                Popover::new("popover:document-grammar")
                    .appearance(false)
                    .open(self.document_grammar_open)
                    .on_open_change({
                        let view = view.clone();
                        let query = query.clone();
                        let focus = focus.clone();
                        move |open, window, cx| {
                            let _ = view.update(cx, |app, cx| {
                                app.document_grammar_open = *open;
                                app.document_grammar_highlight = 0;
                                if *open {
                                    query.update(cx, |query, cx| query.set_value("", window, cx));
                                    app.watch_document_grammar_query(&query, window, cx);
                                }
                                cx.notify();
                            });
                            if *open {
                                focus.focus(window, cx);
                            }
                        }
                    })
                    .track_focus(&focus)
                    .w(px(232.))
                    // Zed's overlay: rounded 8px with a 4px inset, which is
                    // what keeps the last row from painting over the corner
                    // the panel rounds off.
                    .p(px(4.))
                    .bg(roles.surface_raised)
                    .border_1()
                    .border_color(roles.border_strong)
                    .rounded(px(8.))
                    .trigger(
                        zed_button("document-grammar")
                            .text()
                            .small()
                            .compact()
                            .label(trigger_label.clone())
                            .accessibility_label(format!(
                                "Choose syntax highlighting language, currently {trigger_label}"
                            ))
                            .dropdown_caret(true)
                            .resting_tooltip("Choose syntax highlighting language"),
                    )
                    .content({
                        let view = view.clone();
                        let query = query.clone();
                        let scroll = scroll.read(cx).clone();
                        let checkout = checkout_id.clone();
                        let selected_path = path.clone();
                        let override_language = override_language.clone();
                        let highlight = self.document_grammar_highlight;
                        move |_, _, cx| {
                            grammar_menu(
                                &query,
                                &scroll,
                                override_language.as_deref(),
                                highlight,
                                roles,
                                GrammarTarget {
                                    view: view.clone(),
                                    checkout: checkout.clone(),
                                    path: selected_path.clone(),
                                },
                                cx,
                            )
                        }
                    })
                    .into_any_element(),
            )
        } else {
            None
        };
        Some(
            div()
                .id("document-pane")
                .size_full()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .bg(roles.surface_base)
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_3()
                        .px_3()
                        .h(px(40.))
                        .border_b_1()
                        .border_color(roles.border)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(roles.text_muted)
                                .child(path.clone()),
                        )
                        .when_some(grammar_selector, |header, selector| header.child(selector))
                        .when(markdown, |header| {
                            header.child(
                                zed_button("document-toggle-reading")
                                    .text()
                                    .small()
                                    .compact()
                                    .rounded(px(0.))
                                    .label(if rendered { "Source" } else { "Rendered" })
                                    .accessibility_label(if rendered {
                                        "Read this document as its source"
                                    } else {
                                        "Read this document rendered"
                                    })
                                    .resting_tooltip(if rendered {
                                        "Read the Markdown source"
                                    } else {
                                        "Read the rendered document"
                                    })
                                    .on_click(cx.listener(|app, _, window, cx| {
                                        let Some(view) = app.document_view.as_mut() else {
                                            return;
                                        };
                                        if view.preview.is_some() {
                                            app.document_grammar_open = false;
                                            view.rendered = !view.rendered;
                                            // The reading that just went away takes
                                            // the focus with it, and the one now on
                                            // screen has to have it: focus left on
                                            // nothing reads as a window that stopped
                                            // listening, and focus left on the hidden
                                            // editor is what the rendered reading
                                            // used to do.
                                            view.focus_visible(window, cx);
                                            cx.notify();
                                        }
                                    })),
                            )
                        })
                        .when_some(changes, |header, (added, removed)| {
                            header
                                .child(
                                    zed_button("document-open-changes")
                                        .text()
                                        .small()
                                        .compact()
                                        .rounded(px(0.))
                                        .label("Changes")
                                        .accessibility_label(format!("Show the changes to {path}"))
                                        .resting_tooltip("Show this file's changes")
                                        .on_click(cx.listener(move |app, _, window, cx| {
                                            app.request_open_changes(
                                                &checkout_id,
                                                Some(path.clone()),
                                                window,
                                                cx,
                                            );
                                            app.focus_terminal(window, cx);
                                        })),
                                )
                                .when(added.unwrap_or(0) + removed.unwrap_or(0) > 0, |row| {
                                    row.when_some(
                                        added.filter(|count| *count != 0),
                                        |row, added| {
                                            row.child(
                                                div()
                                                    .flex_shrink_0()
                                                    .text_size(px(11.))
                                                    .text_color(roles.status.success)
                                                    .child(format!("+{added}")),
                                            )
                                        },
                                    )
                                    .when_some(
                                        removed.filter(|count| *count != 0),
                                        |row, removed| {
                                            row.child(
                                                div()
                                                    .flex_shrink_0()
                                                    .text_size(px(11.))
                                                    .text_color(roles.status.danger_text)
                                                    .child(format!("−{removed}")),
                                            )
                                        },
                                    )
                                })
                        })
                        .child(
                            zed_button("close-document")
                                .custom(crate::app::sidebar_action_style(cx))
                                .small()
                                .compact()
                                .icon(IconName::X)
                                .accessibility_label("Close preview")
                                .resting_tooltip("Close preview")
                                .on_click(cx.listener(|app, _, window, cx| {
                                    app.request_document_transition(
                                        Box::new(|app, window, cx| {
                                            app.clear_document();
                                            // A close is a close: the row that
                                            // named this view goes with it.
                                            app.view_pin = None;
                                            app.focus_terminal(window, cx);
                                            cx.notify();
                                        }),
                                        window,
                                        cx,
                                    );
                                })),
                        ),
                )
                .child(div().flex_1().min_w_0().min_h_0().child(body))
                .when_some(code_footer, |pane, (dirty, save_needed, saving, formatting, transition_pending)| {
                    pane.child(
                        div()
                            .id("document-actions")
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .px_3()
                            .py_1()
                            .border_t_1()
                            .border_color(roles.border)
                            // Legacy's format control: the braces it draws, on the flat
                            // hover wash its `document-action` wears rather than a button box.
                            .child(
                                zed_button("format-document")
                                    .custom(crate::app::sidebar_action_style(cx))
                                    .small()
                                    .compact()
                                    .icon(IconName::Braces)
                                    .accessibility_label("Format with Prettier")
                                    .resting_tooltip(if !format_supported {
                                        format!("Prettier does not format {format_language}.")
                                    } else if formatting {
                                        "Formatting…".to_owned()
                                    } else {
                                        "Format with Prettier; the result stays an unsaved draft."
                                            .to_owned()
                                    })
                                    .disabled(!format_supported || saving || formatting)
                                    .on_click(cx.listener(|app, _, window, cx| {
                                        app.format_document(window, cx);
                                    })),
                            )
                            .when(transition_pending, |footer| {
                                footer.child(
                                    zed_button("stay-in-document")
                                        .ghost()
                                        .small()
                                        .compact()
                                        .label("Stay here")
                                        .accessibility_label("Cancel the pending navigation and stay in this document")
                                        .on_click(cx.listener(|app, _, _, cx| {
                                            app.pending_document_transition = None;
                                            cx.notify();
                                        })),
                                )
                            })
                            .when(save_needed, |footer| {
                                footer.child(
                                    div()
                                        .id("document-draft-actions")
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .when(dirty, |actions| {
                                            actions.child(
                                                zed_button("cancel-document")
                                                    .ghost()
                                                    .small()
                                                    .compact()
                                                    .label("Cancel")
                                                    .accessibility_label("Discard the unsaved changes")
                                                    .resting_tooltip("Discard the unsaved changes (Esc)")
                                                    .disabled(saving)
                                                    .on_click(cx.listener(|app, _, window, cx| {
                                                        app.cancel_document(window, cx);
                                                    })),
                                            )
                                        })
                                        .child(
                                            zed_button("save-document")
                                                .primary()
                                                .small()
                                                .compact()
                                                .label(if saving {
                                                    "Saving…"
                                                } else if save_needed && !dirty {
                                                    "Retry"
                                                } else {
                                                    "Save"
                                                })
                                                .accessibility_label(if save_needed && !dirty {
                                                    "Retry confirming the save is durable"
                                                } else {
                                                    "Save this file"
                                                })
                                                .resting_tooltip(if saving || formatting {
                                                    "Save unavailable while an operation is running"
                                                        .to_owned()
                                                } else if save_needed && !dirty {
                                                    "Retry syncing the published file to confirm durability"
                                                        .to_owned()
                                                } else {
                                                    format!("Save this file ({})", save_key())
                                                })
                                                .disabled(saving || formatting)
                                                .on_click(cx.listener(|app, _, window, cx| {
                                                    app.save_document(window, cx);
                                                })),
                                        ),
                                )
                            }),
                    )
                })
                .into_any_element(),
        )
    }
}

/// A link clicked in a rendered document, opened where it was clicked.
///
/// The toolkit's text view hands this the `Window` and `App` of the click, and
/// its handler is `Send + Sync`; a weak handle to this window is both of those
/// things. So the click is dispatched from the click rather than left for a
/// later frame to notice, and a window that has been released answers `Err` —
/// answering nothing is what a click on a closed window should do.
pub(crate) fn link_handler(
    view: gpui_kit::WeakEntity<TerminalWindow>,
) -> impl Fn(&gpui_kit::SharedString, &gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App)
       + Send
       + Sync
       + 'static {
    move |url, _, window, cx| {
        let _ = view.update(cx, |app, cx| {
            app.open_link(url.to_string(), window, cx);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{Render, TestAppContext};

    /// The production body: one retained view, drawn as the window draws it.
    struct Harness {
        view: Option<DocumentView>,
        document: Option<(String, String)>,
    }

    impl Render for Harness {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let Some((path, text)) = self.document.clone() else {
                return div().size_full().into_any_element();
            };
            let state = document_state(
                &mut self.view,
                &format!("{path}\0{text}"),
                &path,
                &text,
                syntax_language(&path),
                window,
                cx,
            );
            let rendered = self
                .view
                .as_ref()
                .filter(|view| view.is_rendered())
                .and_then(|view| view.preview.clone());
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(div().flex_1().min_w_0().min_h_0().child(document_body(
                    &state,
                    rendered,
                    None,
                    |_, _, _, _| {},
                )))
                .into_any_element()
        }
    }

    fn open(cx: &mut TestAppContext) -> (gpui_kit::AnyWindowHandle, Entity<Harness>) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::install(cx);
        });
        cx.update(|cx| {
            let bounds = gpui_kit::Bounds {
                origin: gpui_kit::Point::default(),
                size: gpui_kit::size(gpui_kit::px(800.), gpui_kit::px(480.)),
            };
            let options = gpui_kit::WindowOptions {
                window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)),
                ..Default::default()
            };
            let (window, view) = gpui_kit::open_window(options, cx, |_, cx| {
                cx.new(|_| Harness {
                    view: None,
                    document: None,
                })
            })
            .expect("open test window");
            (window, view)
        })
    }

    /// The editor the harness is currently drawing.
    fn editor(cx: &mut TestAppContext, harness: &Entity<Harness>) -> Entity<EditorState> {
        harness.read_with(cx, |harness, _| {
            harness
                .view
                .as_ref()
                .expect("a document is open")
                .state
                .clone()
        })
    }

    /// The editor's own element id, which is what a pointer event addresses.
    fn at(state: &Entity<EditorState>) -> (&'static str, gpui_kit::EntityId) {
        ("input", state.entity_id())
    }

    /// Frames until a Markdown parse has landed and the view measured itself.
    fn settle(cx: &mut TestAppContext, window: gpui_kit::AnyWindowHandle) {
        for _ in 0..8 {
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| window.render_frame(cx))
                .unwrap();
        }
    }

    /// A document taller than the pane is read from the middle of it, whichever
    /// reading is on screen.
    #[gpui_kit::test]
    fn a_document_taller_than_the_pane_scrolls_in_both_readings(cx: &mut TestAppContext) {
        let (window, harness) = open(cx);
        // Its source: the editor keeps a viewport onto a file taller than the pane.
        let source = (0..400)
            .map(|line| format!("let line_{line} = {line};\n"))
            .collect::<String>();
        harness.update(cx, |harness, cx| {
            harness.document = Some(("src/main.rs".into(), source));
            cx.notify();
        });
        settle(cx, window);
        let state = editor(cx, &harness);
        let id = at(&state);
        cx.update_window(window, |_, window, cx| {
            window.scroll(
                id,
                // A negative delta is down: the offset is the content's own
                // displacement, so scrolling down moves it away from zero.
                gpui_kit::ScrollDelta::Lines(gpui_kit::point(0., -5.)),
                cx,
            );
        })
        .unwrap();
        assert!(
            state.read_with(cx, |state, _| state.scroll_offset().y) < gpui_kit::px(0.),
            "the source did not scroll"
        );

        // Its reading: the same content as a document, which scrolls itself.
        let rendered = (0..60)
            .map(|block| format!("Block {block}: {}\n\n", "lorem ipsum dolor ".repeat(20)))
            .collect::<String>();
        harness.update(cx, |harness, cx| {
            harness.document = Some(("README.md".into(), rendered));
            cx.notify();
        });
        settle(cx, window);
        let preview = harness.read_with(cx, |harness, _| {
            harness
                .view
                .as_ref()
                .expect("a document is open")
                .preview
                .clone()
                .expect("a Markdown file has a reading")
        });
        assert!(
            preview.read_with(cx, |preview, _| {
                preview.list_state().max_offset_for_scrollbar().y
            }) > gpui_kit::px(0.),
            "the reading has nothing to scroll"
        );
        cx.update_window(window, |_, window, cx| {
            window.scroll(
                "document-markdown",
                gpui_kit::ScrollDelta::Lines(gpui_kit::point(0., -5.)),
                cx,
            );
        })
        .unwrap();
        assert!(
            preview.read_with(cx, |preview, _| {
                preview.list_state().scroll_px_offset_for_scrollbar().y
            }) < gpui_kit::px(0.),
            "the reading did not scroll"
        );
    }

    #[test]
    fn the_language_comes_from_the_path_and_falls_back_to_plain_text() {
        for (path, language) in [
            ("package.json", "json"),
            ("src/main.rs", "rust"),
            ("app.js", "javascript"),
            ("app.mjs", "javascript"),
            ("types.d.ts", "typescript"),
            ("view.tsx", "tsx"),
            ("ci.yaml", "yaml"),
            ("ci.yml", "yaml"),
            ("index.html", "html"),
            ("site.css", "css"),
            ("README.md", "markdown"),
            ("LICENSE", "plaintext"),
            ("Makefile", "make"),
            ("Cargo.toml", "toml"),
            ("src/MAIN.PY", "python"),
            (r"src\main.go", "go"),
            ("CMakeLists.txt", "cmake"),
            ("scripts/build.sh", "bash"),
            ("home/.bashrc", "bash"),
            ("home/.kshrc", "bash"),
            ("home/.login", "bash"),
            ("home/.bashrc.local", "bash"),
            (".gitignore", "gitignore"),
            ("nested/.dockerignore", "gitignore"),
            (".gitattributes", "ini"),
            ("home/.gitconfig", "ini"),
            (".gitmodules", "ini"),
            ("settings.ini", "ini"),
            ("home/.env", "dotenv"),
            (".env.production.local", "dotenv"),
            (".envrc", "plaintext"),
            (".eslintrc.json", "json"),
            ("src/main.cpp", "cpp"),
            ("query.sql", "sql"),
            ("api.proto", "proto"),
            ("schema.graphql", "graphql"),
            ("types.gql", "graphql"),
            ("component.vue", "vue"),
            ("diagram.mmd", "plaintext"),
            ("document.xml", "plaintext"),
            ("notes.rst", "plaintext"),
            ("src/main.rs.bak", "plaintext"),
        ] {
            assert_eq!(syntax_language(path), language, "{path}");
        }
    }

    #[gpui_kit::test]
    fn one_editor_serves_the_document_and_a_new_file_replaces_it(cx: &mut TestAppContext) {
        let (window, harness) = open(cx);
        harness.update(cx, |harness, cx| {
            harness.document = Some(("src/main.rs".into(), "fn main() {}\n".into()));
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();
        let first = editor(cx, &harness);
        let first_id = first.entity_id();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert_eq!(
            editor(cx, &harness).entity_id(),
            first_id,
            "a redraw keeps the editor"
        );

        harness.update(cx, |harness, cx| {
            harness.document = Some(("Cargo.toml".into(), "[package]\n".into()));
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();
        assert_ne!(
            editor(cx, &harness).entity_id(),
            first_id,
            "another file is another editor"
        );
    }

    #[gpui_kit::test]
    fn a_document_is_readable_whole_and_editable(cx: &mut TestAppContext) {
        let (window, harness) = open(cx);
        let text = "{\n  \"name\": \"muster\",\n  \"empty\": \"\"\n}\n";
        harness.update(cx, |harness, cx| {
            harness.document = Some(("Cargo.toml".into(), text.into()));
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();
        let state = editor(cx, &harness);
        let id = at(&state);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find(id).value(), Some(text));
            // The whole file is there to be edited, and the trailing newline is a
            // line of the file rather than a wrapping detail: typing lands after
            // it, not inside the last line.
            window.click(id, cx);
            window.input("!", cx);
        })
        .unwrap();
        cx.read(|cx| {
            let state = state.read(cx);
            assert_eq!(state.text().to_string(), format!("{text}!"));
            assert!(!state.presentation().is_readonly());
            assert!(state.presentation().is_code_editor());
        });

        // Selectable and copyable: selection is how a reader takes a name out.
        let (selected, copied) = cx
            .update_window(window, |_, window, cx| {
                window.click(id, cx);
                window.press("secondary-a", cx);
                let selected = state.read(cx).selected_text().to_string();
                window.press("secondary-c", cx);
                let copied = cx
                    .read_from_clipboard()
                    .expect("copy wrote the clipboard")
                    .text();
                (selected, copied)
            })
            .unwrap();
        let edited = format!("{text}!");
        assert_eq!(selected, edited);
        assert_eq!(copied.as_deref(), Some(edited.as_str()));
    }

    /// Tab is two spaces, and the shifted one takes them back: an editor that
    /// answers Tab with focus traversal reads as a pane the keyboard skipped.
    #[gpui_kit::test]
    fn a_tab_in_the_source_is_two_spaces_and_shift_tab_is_the_other_way(cx: &mut TestAppContext) {
        let (window, harness) = open(cx);
        let text = "fn main() {}\n";
        harness.update(cx, |harness, cx| {
            harness.document = Some(("src/main.rs".into(), text.into()));
            cx.notify();
        });
        settle(cx, window);
        let state = editor(cx, &harness);
        let id = at(&state);
        cx.update_window(window, |_, window, cx| {
            window.click(id, cx);
            window.press("tab", cx);
        })
        .unwrap();
        cx.read(|cx| {
            assert_eq!(
                state.read(cx).text().to_string(),
                format!("{text}  "),
                "Tab writes the indent rather than moving the focus"
            )
        });
        cx.update_window(window, |_, window, cx| {
            window.press("shift-tab", cx);
        })
        .unwrap();
        cx.read(|cx| assert_eq!(state.read(cx).text().to_string(), text));
    }

    #[gpui_kit::test]
    fn an_empty_file_is_an_empty_editor_and_not_a_missing_one(cx: &mut TestAppContext) {
        let (window, harness) = open(cx);
        harness.update(cx, |harness, cx| {
            harness.document = Some(("LICENSE".into(), String::new()));
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();
        let state = editor(cx, &harness);
        let id = at(&state);
        cx.update_window(window, |_, window, _cx| {
            assert_eq!(window.find(id).value(), Some(""));
        })
        .unwrap();
        cx.read(|cx| assert_eq!(state.read(cx).text().to_string(), ""));
    }
    #[gpui_kit::test]
    fn additional_grammars_produce_legacy_token_colors(cx: &mut TestAppContext) {
        let (window, harness) = open(cx);
        for (path, source) in [
            ("Cargo.toml", "[package]\nname = \"muster\"\nversion = 42\n"),
            ("Makefile", "# comment\nall:\n\techo hello\n"),
            (
                "change.patch",
                "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n",
            ),
            (
                "view.svelte",
                "<script>const name = 'hello';</script><p>{name}</p>",
            ),
            ("view.ejs", "<p><%= name %></p>"),
            ("view.erb", "<p><%= name %></p>"),
            (
                "component.vue",
                "<template><p>{{ name }}</p></template>\n<script>const n = 1;</script>\n",
            ),
            (
                "api.proto",
                "// a service\nsyntax = \"proto3\";\n\nmessage User {\n  string name = 1;\n}\n",
            ),
            (
                "schema.graphql",
                "# a query\nquery Hello($id: ID!) {\n  user(id: $id) { name }\n}\n",
            ),
            ("main.py", "# comment\ndef run():\n    return \"hello\"\n"),
            ("run.sh", "# comment\nif true; then echo \"hello\"; fi\n"),
            (
                ".env.local",
                "# local settings\nAPI_URL=\"https://example.test\"\nPORT=3000\n",
            ),
            (
                ".gitignore",
                "# dependency cache\nnode_modules/\n!.env.example\n*.log\n",
            ),
            (
                ".gitconfig",
                "[user]\nname = \"Ada\"\nemail = ada@example.test\n",
            ),
            (
                "main.go",
                "package main\nfunc main() { println(\"hello\") }\n",
            ),
            ("main.c", "// comment\nint main() { return 42; }\n"),
            ("main.cpp", "// comment\nint main() { return 42; }\n"),
            (
                "Main.java",
                "// comment\nclass Main { String name = \"hello\"; }\n",
            ),
            (
                "main.cs",
                "// comment\nclass Main { string name = \"hello\"; }\n",
            ),
            (
                "query.sql",
                "-- comment\nSELECT 'hello' FROM users WHERE id = 42;\n",
            ),
            ("main.rb", "# comment\ndef run\n  puts \"hello\"\nend\n"),
            ("main.lua", "-- comment\nlocal name = \"hello\"\n"),
            ("main.swift", "// comment\nlet name = \"hello\"\n"),
            ("main.kt", "// comment\nval name = \"hello\"\n"),
            ("main.scala", "// comment\nval name = \"hello\"\n"),
            ("main.php", "<?php // comment\n$name = \"hello\";\n"),
            (
                "main.ex",
                "# comment\ndefmodule Main do\n def name, do: \"hello\"\nend\n",
            ),
            ("main.zig", "// comment\nconst name = \"hello\";\n"),
            ("CMakeLists.txt", "# comment\nset(NAME \"hello\")\n"),
        ] {
            harness.update(cx, |h, cx| {
                h.document = Some((path.into(), source.into()));
                cx.notify();
            });
            cx.update_window(window, |_, window, cx| window.render_frame(cx))
                .unwrap();
            let state = editor(cx, &harness);
            cx.update(|cx| {
                let mut highlighter =
                    gpui_kit::component::highlighter::SyntaxHighlighter::new(syntax_language(path));
                let text = state.read(cx).text().clone();
                assert!(highlighter.update(None, &text, None), "{path}");
                let theme = gpui_kit::component::Theme::global(cx);
                let styles = highlighter.styles(&(0..source.len()), theme.highlight_theme.as_ref());
                let colors: Vec<_> = styles.iter().filter_map(|(_, style)| style.color).collect();
                assert!(
                    colors.iter().any(|color| Some(color) != colors.first()),
                    "{path} is monochrome"
                );
            });
        }
    }

    /// A single-file component is coloured in all three of its blocks.
    ///
    /// The Vue grammar's own query is the markup one — a tag is a tag and an
    /// attribute is an attribute — so `<template>` reads as what it is, and the
    /// two blocks that are *not* markup are reached through the grammar's
    /// injections. Each is asked for separately, because a component whose
    /// `<style>` opens in the editor's own ink still passes a check that only
    /// asks whether the file is coloured somewhere.
    #[gpui_kit::test]
    fn a_vue_component_is_coloured_in_its_script_its_template_and_its_style(
        cx: &mut TestAppContext,
    ) {
        let (window, harness) = open(cx);
        let source = "<script setup lang=\"ts\">\nconst name = 'muster';\n</script>\n\n\
             <template>\n  <p :title=\"name\">{{ name }}</p>\n</template>\n\n\
             <style scoped>\n.name {\n  color: red;\n}\n</style>\n";
        harness.update(cx, |h, cx| {
            h.document = Some(("component.vue".into(), source.into()));
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .unwrap();
        let state = editor(cx, &harness);
        cx.update(|cx| {
            let mut highlighter = gpui_kit::component::highlighter::SyntaxHighlighter::new("vue");
            let text = state.read(cx).text().clone();
            assert!(highlighter.update(None, &text, None), "vue");
            let theme = gpui_kit::component::Theme::global(cx);
            let styles = highlighter.styles(&(0..source.len()), theme.highlight_theme.as_ref());
            let coloured = |block: &str| {
                let range = source
                    .find(block)
                    .map(|at| at..at + block.len())
                    .unwrap_or_else(|| panic!("the component has a {block} block"));
                let mut runs = styles
                    .iter()
                    .filter(|(span, _)| span.start < range.end && span.end > range.start)
                    .filter_map(|(_, style)| style.color)
                    .collect::<Vec<_>>();
                runs.sort_by_key(|color| format!("{color:?}"));
                runs.dedup();
                assert!(runs.len() > 1, "the {block} block is one colour: {runs:?}");
            };
            coloured("const name = 'muster';");
            coloured("<p :title=\"name\">");
            coloured("color: red;");
        });
    }

    #[gpui_kit::test]
    fn json_grammar_really_produces_different_token_styles(cx: &mut TestAppContext) {
        let (window, harness) = open(cx);
        let source = "{\"name\": \"muster\", \"count\": 42, \"ready\": true}\n";
        harness.update(cx, |h, cx| {
            h.document = Some(("package.json".into(), source.into()));
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .unwrap();
        let state = editor(cx, &harness);
        cx.update(|cx| {
            let mut highlighter = gpui_kit::component::highlighter::SyntaxHighlighter::new("json");
            let text = state.read(cx).text().clone();
            assert!(highlighter.update(None, &text, None));
            let theme = gpui_kit::component::Theme::global(cx);
            let styles = highlighter.styles(&(0..source.len()), theme.highlight_theme.as_ref());
            let colors = styles
                .iter()
                .filter_map(|(_, style)| style.color)
                .collect::<Vec<_>>();
            assert!(colors.len() > 2);
            assert!(
                colors.iter().any(|color| color != &colors[0]),
                "JSON is not monochrome"
            );
        });
        let old = state.entity_id();
        harness.update(cx, |h, cx| {
            h.document = Some(("package.json".into(), "{}\n".into()));
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .unwrap();
        let updated = editor(cx, &harness);
        assert_ne!(
            updated.entity_id(),
            old,
            "same path with refreshed content is not stale"
        );
        updated.read_with(cx, |state, _| assert_eq!(state.text().to_string(), "{}\n"));
    }

    /// Template text is prose, and Legacy painted it as prose.
    ///
    /// The Vue grammar's own query opens by capturing `(template_element)`, which
    /// is the whole `<template>` element, as `@tag`. A span that wide reaches
    /// every byte the rest of the query does not name — the whitespace, the
    /// indentation, and the text between the tags — so a label's own text came
    /// out the colour of a tag. `(text) @none` wears no colour, so nothing
    /// painted over it. One capture says a container wears nothing, and the text
    /// inside it falls to the editor's ink.
    #[gpui_kit::test]
    fn vue_template_text_is_not_painted_as_a_string(cx: &mut TestAppContext) {
        let (window, harness) = open(cx);
        let source =
            "<template>\n  <x-label for=\"previewType\">Preview Type</x-label>\n</template>\n";
        harness.update(cx, |h, cx| {
            h.document = Some(("component.vue".into(), source.into()));
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .unwrap();
        let state = editor(cx, &harness);
        cx.update(|cx| {
            let mut highlighter = gpui_kit::component::highlighter::SyntaxHighlighter::new("vue");
            let text = state.read(cx).text().clone();
            assert!(highlighter.update(None, &text, None), "vue");
            let theme = gpui_kit::component::Theme::global(cx);
            let styles = highlighter.styles(&(0..source.len()), theme.highlight_theme.as_ref());
            let paint = |needle: &str| {
                let at = source.find(needle).expect("the label has the needle");
                styles
                    .iter()
                    .filter(|(range, _)| range.start < at + needle.len() && range.end > at)
                    .filter_map(|(_, style)| style.color)
                    .collect::<Vec<_>>()
            };
            assert!(
                paint("Preview Type").is_empty(),
                "the text between the tags wears {:?}",
                paint("Preview Type")
            );
            // And the tag around it still wears one, so this is not a file that
            // failed to parse or a highlighter that found nothing in it.
            assert!(
                !paint("x-label").is_empty(),
                "the tag around the text is not coloured"
            );
        });
    }

    #[test]
    fn prettier_parser_matches_legacy_supported_languages() {
        for (path, language, parser) in [
            ("src/main.ts", "typescript", Some("typescript")),
            ("src/view.tsx", "tsx", Some("typescript")),
            ("src/data.jsonc", "json", Some("jsonc")),
            ("src/theme.scss", "css", Some("scss")),
            ("src/page.vue", "vue", Some("vue")),
            ("README.md", "markdown", Some("markdown")),
            ("src/main.rs", "rust", None),
        ] {
            assert_eq!(prettier_parser(path, language), parser, "{path}");
        }
    }

    #[test]
    fn formatter_result_is_discarded_after_navigation() {
        assert!(formatter_navigation_is_current(4, 4));
        assert!(!formatter_navigation_is_current(5, 4));
    }

    #[test]
    fn formatter_result_is_discarded_after_text_or_language_changes() {
        assert!(formatter_result_is_current(
            "draft", "draft", "json", "json"
        ));
        assert!(!formatter_result_is_current(
            "new draft",
            "draft",
            "json",
            "json"
        ));
        assert!(!formatter_result_is_current(
            "draft",
            "draft",
            "json",
            "typescript"
        ));
    }

    fn formatter_test_command(script: &str) -> Command {
        use std::os::unix::process::CommandExt;
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", script])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        command
    }

    #[test]
    fn formatter_process_bounds_both_output_streams() {
        let stdout =
            run_formatter_process(formatter_test_command("head -c 1048577 /dev/zero"), None)
                .unwrap_err();
        assert!(stdout.contains("output larger than"));

        let stderr =
            run_formatter_process(formatter_test_command("head -c 65537 /dev/zero >&2"), None)
                .unwrap_err();
        assert!(stderr.contains("diagnostics exceeded"));
    }

    #[test]
    fn formatter_process_times_out_and_reaps_descendants() {
        let started = Instant::now();
        let result = run_formatter_process_with_timeout(
            formatter_test_command("sleep 30 & wait"),
            None,
            Duration::from_millis(50),
        );
        assert!(result.unwrap_err().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(3));

        let started = Instant::now();
        let result = run_formatter_process(formatter_test_command("sleep 30 & exit 0"), None)
            .expect("the formatter parent succeeded");
        assert!(result.status.success());
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn formatter_process_returns_success_output_and_failure_diagnostics() {
        let output = run_formatter_process(formatter_test_command("printf formatted"), None)
            .expect("formatter succeeds");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"formatted");

        let failed = run_formatter_process(
            formatter_test_command("printf diagnostic >&2; exit 7"),
            None,
        )
        .expect("process result is collected");
        assert!(!failed.status.success());
        assert_eq!(failed.stderr, b"diagnostic");
    }

    #[test]
    fn formatter_skips_checkout_tools_and_uses_an_isolated_input() {
        use std::{fs, os::unix::fs::PermissionsExt};

        fn install(path: &Path, script: &str) {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, script).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let checkout = tempfile::tempdir().unwrap();
        let local_bin = checkout.path().join(".tools");
        let local_prettier = local_bin.join("prettier");
        install(&local_prettier, "#!/bin/sh\nprintf unsafe\n");

        let user_bin = tempfile::tempdir().unwrap();
        let user_prettier = user_bin.path().join("prettier");
        install(
            &user_prettier,
            "#!/bin/sh\n[ \"$1\" = \"--no-config\" ] || exit 21\n[ \"$2\" = \"--no-editorconfig\" ] || exit 22\n[ \"$3\" = \"--parser\" ] || exit 23\n[ \"$4\" = \"json\" ] || exit 24\nexec /bin/cat \"$5\"\n",
        );
        let path =
            std::env::join_paths([local_bin.as_os_str(), user_bin.path().as_os_str()]).unwrap();
        let (executable, safe_path) =
            prettier_executable_from_path(checkout.path(), &path).unwrap();
        assert_eq!(executable, user_prettier.canonicalize().unwrap());
        assert!(std::env::split_paths(&safe_path)
            .all(|directory| !directory.starts_with(checkout.path())));

        let source = "{\"name\":\"muster\"}\n";
        assert_eq!(
            run_prettier_with(&executable, &safe_path, "config.json", "json", source).unwrap(),
            source,
            "the formatter reads a temp copy and cannot run the checkout's executable"
        );
    }

    /// The daemon is what the app reaches for first, and it is a different call: it is
    /// given the document on stdin and takes its parser as one `--parser=<name>`
    /// argument, because a bare word after a flag is a second file path to it.
    #[test]
    fn a_daemon_formatter_is_preferred_and_is_given_the_document_on_stdin() {
        use std::{fs, os::unix::fs::PermissionsExt};

        fn install(path: &Path, script: &str) {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, script).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let checkout = tempfile::tempdir().unwrap();
        let user_bin = tempfile::tempdir().unwrap();
        install(
            &user_bin.path().join("prettierd"),
            "#!/bin/sh\n[ \"$3\" = \"--parser=json\" ] || exit 31\ndocument=$(/bin/cat) || exit 32\n[ \"$document\" = '{\"name\":\"muster\"}' ] || exit 33\nprintf formatted\n",
        );
        // Installed and never asked for: a daemon on PATH outranks it.
        install(&user_bin.path().join("prettier"), "#!/bin/sh\nexit 1\n");

        let path = user_bin.path().as_os_str();
        let (executable, safe_path) = prettier_executable_from_path(checkout.path(), path).unwrap();
        assert_eq!(executable.file_name(), Some(OsStr::new("prettierd")));
        assert_eq!(
            run_prettier_with(
                &executable,
                &safe_path,
                "config.json",
                "json",
                "{\"name\":\"muster\"}"
            )
            .unwrap(),
            "formatted"
        );
    }
}
