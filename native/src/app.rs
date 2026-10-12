//! The window, and the two ways this app ends.
//!
//! One grid, one status line, one shell. Everything with a lifetime in it lives
//! here: the shell is spawned and claimed *before* the view that draws it, so a
//! window that never mounts still has a shell somebody is responsible for
//! ending, and the quit path is registered before the window opens, so a quit
//! that arrives in the first frame still ends it.
//!
//! Two endings, and they differ on purpose:
//!
//! * A **window close** can be refused. A `vim` in front of the shell is the
//!   person's work, and only they decide whether it is worth closing.
//! * An **app quit** cannot. GPUI runs [`install_quit_path`]'s handler from
//!   `App::shutdown`, and the platform hands the process to `exit` rather than
//!   unwinding, so a window's `Drop` is not somewhere a shell can be relied on
//!   to end from. That path is also where the free [`shutdown`] lives: the
//!   session's close sequence respects the drain deadline and the process
//!   group, and `shutdown` is the only thing that reaches a child the sequence
//!   never got to.
//!
//! Nothing else is read from the machine: no database is opened, nothing under
//! `~/.muster` is touched, and no shell startup file is parsed by this file.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use gpui_kit::{
    div, px, size, AnyElement, App, AppContext, Bounds, Context, Entity, Focusable, IntoElement,
    ParentElement, Render, Styled, TextAlign, Window, WindowBounds, WindowOptions,
};

use crate::pty::TerminalBackend;
use crate::terminal::{
    shutdown, xterm_256, CellMetrics, CloseReport, ColorSnapshot, Palette, SessionError,
    ShellState, TerminalHost, TerminalSession, TerminalSessionConfig, TerminalView,
};

/// The shell this process owns, taken by whichever path ends it first.
///
/// A `static` rather than a field because the quit callback has no window to
/// borrow from, and `shutdown` wants a `&TerminalBackend` that outlives every
/// entity in the process. Taking the value is what makes ending idempotent: the
/// second caller finds nothing and does nothing, whichever order a window close
/// and an app quit arrive in.
static OWNED: Mutex<Option<Arc<OwnedShell>>> = Mutex::new(None);

/// One shell, and everything needed to end it deliberately.
///
/// Cloned into the view rather than moved into it, because the quit path has to
/// own the shell before the view exists: a shell only the view owns outlives the
/// app whenever the view is never mounted.
struct OwnedShell {
    backend: Arc<TerminalBackend>,
    session: TerminalSession,
    /// Where the view publishes its measured cell. Built here rather than in the
    /// view because the session's host callbacks are made before the view that
    /// answers them exists.
    metrics: Arc<Mutex<CellMetrics>>,
}

/// What the person is looking at.
///
/// One grid and one line of chrome. Everything below the line belongs to the
/// child, and the line itself is the only thing in this window that is not
/// either this file or the parser.
pub struct TerminalWindow {
    /// The grid. `None` only when no shell could be spawned, which is the one
    /// state in which this window has something of its own to draw.
    terminal: Option<Entity<TerminalView>>,
    /// Why there is no grid, in the window's own words.
    failure: Option<String>,
    /// The shell, for a window that was disposed of before the quit path ran.
    shell: Option<Arc<OwnedShell>>,
    /// Whether this window has taken focus for its terminal yet.
    ///
    /// A field rather than a call on every frame because focusing is a window
    /// mutation, and doing it on each paint fights whatever else in the window
    /// is asking for focus.
    focused: bool,
    /// The name to show until the child sets a title of its own.
    fallback: String,
    palette: Palette,
}

impl TerminalWindow {
    /// Open the terminal: one login shell, in the directory this process was
    /// pointed at.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        match start_shell() {
            Ok(shell) => Self::over(shell, window, cx),
            Err(error) => Self::refused(error.to_string()),
        }
    }

    /// The same window over a session somebody else built.
    ///
    /// A test needs this because [`Self::new`] picks the shell out of the
    /// running person's environment, and a suite must not depend on which shell
    /// that is. The caller keeps ownership: the quit path ends exactly one
    /// shell, and a test opens more than one.
    pub fn with_session(
        session: TerminalSession,
        backend: Arc<TerminalBackend>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let shell = Arc::new(OwnedShell {
            backend,
            session,
            metrics: Arc::new(Mutex::new(UNMEASURED_CELL)),
        });
        Self::over(shell, window, cx)
    }

    /// The window with no shell behind it.
    ///
    /// A terminal that refused to spawn is not a terminal that closed: somebody
    /// has to be told, and this is the only surface that can tell them.
    pub fn refused(failure: String) -> Self {
        Self {
            terminal: None,
            failure: Some(failure),
            shell: None,
            focused: false,
            fallback: default_title(),
            palette: Palette::default(),
        }
    }

    /// Mount the grid over `shell` and register how this window ends.
    fn over(shell: Arc<OwnedShell>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let view =
            cx.new(|cx| TerminalView::new(shell.session.clone(), Arc::clone(&shell.metrics), cx));
        refuse_close_while_busy(&shell, window, cx);

        Self {
            terminal: Some(view),
            failure: None,
            shell: Some(shell),
            focused: false,
            fallback: default_title(),
            palette: Palette::default(),
        }
    }

    /// The grid this window draws, when it has one.
    pub fn terminal(&self) -> Option<&Entity<TerminalView>> {
        self.terminal.as_ref()
    }

    /// What the status line shows right now.
    ///
    /// The title is whatever the child last set, already sanitised and bounded
    /// by the parser: untrusted text with a fixed-size destination.
    fn line(&self, cx: &App) -> Line {
        let Some(terminal) = &self.terminal else {
            return Line {
                title: self.fallback.clone(),
                busy: false,
                failure: self.failure.clone(),
            };
        };

        let session = terminal.read(cx).session();
        let snapshot = session.snapshot();
        let status = session.status();

        Line {
            title: snapshot
                .title
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| self.fallback.clone()),
            busy: status.foreground.running,
            // Three failures with three different fixes, so none of them is
            // allowed to be phrased like the others.
            failure: status.poll_error.map_or_else(
                || match status.shell {
                    ShellState::Exited { code: Some(code) } => {
                        Some(format!("shell exited ({code})"))
                    }
                    ShellState::Exited { code: None } => Some("shell exited".to_owned()),
                    ShellState::Unknown | ShellState::Running => None,
                },
                |error| Some(format!("the shell's status could not be read: {error}")),
            ),
        }
    }
}

impl Render for TerminalWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Focus once, on the first frame. Nothing else in this window takes
        // keys, so a terminal nobody focused is a terminal nobody can type into.
        if !self.focused {
            self.focused = true;
            if let Some(terminal) = &self.terminal {
                terminal.read(cx).focus_handle(cx).focus(window, cx);
            }
        }

        let line = self.line(cx);
        let background = self.palette.background(ColorSnapshot::Default);
        let text = self.palette.foreground(ColorSnapshot::Default);
        let alarm = self.palette.foreground(ColorSnapshot::Indexed(1));

        // A shell that never started has no grid to draw, and a half-drawn
        // grid under an error reads as a shell that opened and then stopped.
        let body: AnyElement = match &self.terminal {
            Some(terminal) => terminal.clone().into_any_element(),
            None => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .px_6()
                .child(line.failure.clone().unwrap_or_default())
                .into_any_element(),
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(background)
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_shrink_0()
                    .h(px(22.))
                    .px_2()
                    .overflow_hidden()
                    .text_xs()
                    .text_color(text)
                    .child(line.title.clone())
                    .child(div().ml_2().child(line.detail()))
                    // One fixed trailing slot rather than a conditional child:
                    // a line that grows and shrinks its children is a line that
                    // relayouts every time the shell's state changes. `flex_1`
                    // rather than `ml_auto` so the message takes the space the
                    // title did not and the two cannot overlap when narrow.
                    .child(
                        div()
                            .flex_1()
                            .text_align(TextAlign::Right)
                            .text_color(alarm)
                            .child(line.failure.clone().unwrap_or_default()),
                    ),
            )
            .child(body)
    }
}

impl Drop for TerminalWindow {
    fn drop(&mut self) {
        // Not every exit runs the quit path: a window closed through the
        // platform never reaches `on_app_quit`, and an injected session is not
        // this process's to end. `Drop for Inner` stops a session's threads and
        // deliberately leaves the PTY, so skipping this is the difference
        // between a closed terminal and an invisible one.
        let _ = self.shell;
        let _ = end_shell();
    }
}

// ---------------------------------------------------------------------------------------------
// Ending the shell
// ---------------------------------------------------------------------------------------------

/// Register the process-wide path that ends the shell when the app quits.
///
/// `App::on_app_quit` is the hook GPUI runs from `App::shutdown`, which is what
/// the platform's quit reaches instead of an unwind. Registered before the
/// window is built so a quit in the first frame still ends the shell.
pub fn install_quit_path(cx: &mut App) {
    cx.on_app_quit(|_| async move {
        let Some(shell) = claim() else { return };
        // On its own thread, because the quit callback has `SHUTDOWN_TIMEOUT`
        // (200ms) and a close is allowed a grace period before it kills. The
        // close thread is detached either way, so this is explicit rather than
        // fire-and-forget only to keep the handle alive.
        thread::Builder::new()
            .name("muster-native-quit".to_owned())
            .spawn(move || {
                let _ = end(shell);
            })
            .expect("muster-native: the quit thread could not be spawned");
    })
    .detach();
}

/// End this process's shell, on the caller's thread.
///
/// The test seam: [`install_quit_path`] runs exactly this body on a thread of
/// its own. Here it is synchronous, so a test can wait for the answer instead
/// of asserting that a thread it cannot see finished.
pub fn end_shell() -> Option<Result<CloseReport, SessionError>> {
    claim().map(end)
}

/// Take the process's shell, if it still has one.
fn claim() -> Option<Arc<OwnedShell>> {
    OWNED.lock().ok()?.take()
}

/// End `shell`: the session's own close sequence, waited for, and then the free
/// [`shutdown`] for any child that sequence never reached.
fn end(shell: Arc<OwnedShell>) -> Result<CloseReport, SessionError> {
    let report = shell.session.close().wait();
    shutdown(&shell.backend);
    report
}

/// Refuse a window close while something is in front of the shell.
///
/// The gate is the last status poll rather than the fresh read inside
/// `begin_close`, and the difference is the whole point: `begin_close` sets the
/// session's closing flag and nothing sets it back, so asking it in order to
/// answer "no" answers a close with a terminal that no longer accepts keys. A
/// poll is at most one status interval old, which is exactly how old the answer
/// already displayed above the grid is.
fn refuse_close_while_busy(shell: &Arc<OwnedShell>, window: &mut Window, cx: &App) {
    let shell = Arc::clone(shell);
    window.on_window_should_close(cx, move |_window, _cx| {
        let status = shell.session.status();

        if let Some(error) = status.poll_error {
            log::warn!("muster-native: close refused, the shell's state is unknown: {error}");
            return false;
        }
        if status.foreground.running {
            let app = status.foreground.app.as_deref().unwrap_or("a program");
            log::warn!("muster-native: close refused, {app} is still running");
            return false;
        }

        // Nothing is killed here: the close only starts once the window is on
        // its way out and the refusal can no longer be taken back.
        shell.session.close();
        true
    });
}

// ---------------------------------------------------------------------------------------------
// Starting the shell
// ---------------------------------------------------------------------------------------------

/// The shell this milestone opens.
///
/// The person's login shell in the directory this process was pointed at, with
/// no database, no `~/.muster`, and no startup file of its own.
fn start_shell() -> Result<Arc<OwnedShell>, SessionError> {
    let config = TerminalSessionConfig::new(
        SESSION_ID.to_owned(),
        working_directory(),
        INITIAL_COLUMNS,
        INITIAL_ROWS,
        None,
    );
    spawn_and_claim(Arc::new(TerminalBackend::default()), config)
}

/// Spawn `config`'s shell and take ownership of it before returning.
///
/// Separate from the window on purpose: a session only the view owns leaks a
/// shell whenever the view is never mounted, and the mount happens after the
/// spawn.
fn spawn_and_claim(
    backend: Arc<TerminalBackend>,
    mut config: TerminalSessionConfig,
) -> Result<Arc<OwnedShell>, SessionError> {
    let metrics = Arc::new(Mutex::new(UNMEASURED_CELL));
    config.host = host(Arc::clone(&metrics));
    let session = TerminalSession::spawn(Arc::clone(&backend), config)?;

    let shell = Arc::new(OwnedShell {
        backend,
        session,
        metrics,
    });
    *OWNED.lock().expect("owned shell slot poisoned") = Some(Arc::clone(&shell));
    Ok(shell)
}

/// The three things only this window can answer.
///
/// Injected rather than reached for, so `TerminalSession` keeps no opinion about
/// fonts, palettes or clipboards, and a headless test can answer all three.
fn host(metrics: Arc<Mutex<CellMetrics>>) -> TerminalHost {
    TerminalHost {
        cell_metrics: Box::new(move || metrics.lock().map(|cell| *cell).unwrap_or(UNMEASURED_CELL)),
        // The view's own palette: a child that indexed 231 asked for the
        // brightest cube corner and gets it whether or not this window is open.
        resolve_color: Box::new(xterm_256),
        // No clipboard this milestone. Nothing here owns one, so an OSC 52
        // write is dropped rather than put anywhere a shell could read it back
        // out of.
        store_clipboard: Box::new(|_| {}),
    }
}

/// Where the shell starts.
///
/// The directory this process was pointed at, or the one it was launched from.
/// Never the home directory: an app that does not know where it was opened from
/// should say so rather than guess, and `$HOME` is a guess that looks exactly
/// like working.
pub fn working_directory() -> PathBuf {
    directory_from(std::env::args_os()).unwrap_or_else(|| {
        std::env::current_dir().ok().unwrap_or_else(|| {
            log::warn!("muster-native: no working directory is known, the shell starts in `.`");
            PathBuf::from(".")
        })
    })
}

/// The first argument that names a directory, if there is one.
///
/// Split out of [`working_directory`] because `std::env::args` cannot be handed
/// a value in a test, and this is the half that decides.
fn directory_from(args: impl IntoIterator<Item = std::ffi::OsString>) -> Option<PathBuf> {
    args.into_iter()
        .skip(1)
        .map(PathBuf::from)
        .find(|path| path.is_dir())
}

/// The size the window opens at.
///
/// Chosen rather than left to the platform so the first grid is a real one:
/// `open_window` with no bounds opens whatever the desktop last remembered, and
/// a remembered postage stamp is not a terminal anybody can use.
pub fn window_bounds(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(960.), px(640.)),
            cx,
        ))),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------------------------
// The status line
// ---------------------------------------------------------------------------------------------

/// The line above the grid.
struct Line {
    title: String,
    /// Whether something is in front of the shell, which is what a close asks
    /// about and what a person needs to see before they close it themselves.
    busy: bool,
    failure: Option<String>,
}

impl Line {
    /// The middle of the line, shown only when there is something to say.
    ///
    /// A `String` rather than an `Option` so the element tree has the same
    /// shape whatever the shell is doing: a line that grows and shrinks its
    /// children is a line that relayouts on every frame the title changes.
    fn detail(&self) -> String {
        if self.busy {
            "a program is running".to_owned()
        } else {
            String::new()
        }
    }
}

/// The directory's own name: what a shell calls itself before a program renames
/// it. Computed once per window because it reads the filesystem.
fn default_title() -> String {
    working_directory()
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| DEFAULT_TITLE.to_owned())
}

/// The backend key for this milestone's one session.
const SESSION_ID: &str = "muster-native";

/// The grid the PTY starts at, before anything has been measured.
///
/// Eighty by twenty-four is what every terminal has opened at since 1971; the
/// first prepaint replaces it with whatever the window really is.
const INITIAL_COLUMNS: u16 = 80;
const INITIAL_ROWS: u16 = 24;

/// What the line shows when no program has set a title and the directory has no
/// name to lend.
const DEFAULT_TITLE: &str = "muster-native";

/// The cell a child is told about before anything has been measured.
///
/// Eight by sixteen, which is what `TerminalHost::default` claims and what every
/// non-GPUI caller of this crate gets. A child that asks within its first
/// millisecond deserves the conventional answer rather than a zero.
const UNMEASURED_CELL: CellMetrics = CellMetrics {
    width: 8,
    height: 16,
};

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, AppContext, Pixels, Size, TestAppContext};

    use super::*;

    /// How long a test waits for a child before deciding it is not coming.
    ///
    /// Generous, because a test that fails on a busy machine is a test about
    /// scheduling, and finite, because every test in this file is bounded.
    const PATIENCE: Duration = Duration::from_secs(10);

    /// How often a waiting test looks.
    const POLL: Duration = Duration::from_millis(10);

    static NEXT_ID: AtomicU32 = AtomicU32::new(0);

    fn next_id() -> String {
        format!(
            "muster-native-test-{}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// A session running `script` under `/bin/sh`.
    ///
    /// Not the login shell: a suite must not depend on which shell the person
    /// running it has, nor on what their startup files do to a PTY.
    fn session(backend: &Arc<TerminalBackend>, script: &str) -> TerminalSession {
        let mut config = TerminalSessionConfig::new(
            next_id(),
            std::env::temp_dir(),
            INITIAL_COLUMNS,
            INITIAL_ROWS,
            Some(PathBuf::from("/bin/sh")),
        );
        config.args = Some(vec!["-c".to_owned(), script.to_owned()]);
        config.status_interval = Duration::from_millis(20);
        config.drain_deadline = Duration::from_millis(500);
        TerminalSession::spawn(Arc::clone(backend), config).expect("the test session spawns")
    }

    /// Open a window over an existing session, at a size.
    fn open(
        cx: &mut TestAppContext,
        size: Size<Pixels>,
        backend: &Arc<TerminalBackend>,
        session: &TerminalSession,
    ) -> (AnyWindowHandle, Entity<TerminalWindow>) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        gpui_kit::point(px(0.), px(0.)),
                        size,
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    cx.new(|cx| {
                        TerminalWindow::with_session(
                            session.clone(),
                            Arc::clone(backend),
                            window,
                            cx,
                        )
                    })
                },
            )
            .expect("the test window opens")
        })
    }

    /// Wait, bounded, for the grid to hold `wanted`.
    ///
    /// Polls the session rather than the window on purpose: the snapshot is one
    /// short lock and is readable from this thread, where a rendered frame
    /// would need the event loop this test is deliberately not running.
    fn wait_for_grid(session: &TerminalSession, wanted: char) -> bool {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if session
                .snapshot()
                .rows
                .iter()
                .any(|row| row.cells.iter().any(|cell| cell.c == wanted))
            {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(POLL);
        }
    }

    /// Draw a few frames of `handle`.
    ///
    /// More than one because the view measures its cell while it is painted and
    /// the canvas that publishes the measurement is laid out in the next frame's
    /// prepaint. A single frame therefore measures nothing, which is a property
    /// of the frame order and not of the window.
    fn settle(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        for _ in 0..3 {
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .expect("the window renders a frame");
            cx.run_until_parked();
        }
    }

    #[gpui_kit::test]
    fn the_terminal_renders_at_every_window_size(cx: &mut TestAppContext) {
        let backend = Arc::new(TerminalBackend::default());
        // One session behind several windows: this is about drawing, and a
        // shell per size would be a handful of orphans nobody asked for.
        let session = session(&backend, "sleep 30");
        let big = size(px(1200.), px(800.));

        for size in [
            big,
            size(px(640.), px(480.)),
            size(px(120.), px(60.)),
            size(px(1.), px(1.)),
        ] {
            let (handle, window) = open(cx, size, &backend, &session);
            settle(cx, handle);

            let grid = window.read_with(cx, |window, cx| {
                window
                    .terminal()
                    .expect("a spawned shell has a grid")
                    .read(cx)
                    .applied_size()
            });
            if size == big {
                assert!(
                    grid.0 > 80 && grid.1 > 24,
                    "a 1200x800 window measured {grid:?}"
                );
            } else if size.width < px(8.) || size.height < px(22.) {
                // Too small for even one cell under the line: a background and
                // nothing else. Painting half a glyph here is how a terminal
                // ends up with a column of clipped characters at its edge.
                assert_eq!(grid, (0, 0), "{size:?} measured a grid anyway");
            }

            cx.update_window(handle, |_, window, _| window.remove_window())
                .expect("the window closes");
            cx.run_until_parked();
        }

        session.close().wait().expect("the test session closes");
    }

    #[gpui_kit::test]
    fn a_focused_terminal_sends_a_keystroke_to_the_shell(cx: &mut TestAppContext) {
        // `cat` echoes exactly what it is sent, so what lands back in the grid
        // is the child's answer rather than a copy of what the test wrote.
        let backend = Arc::new(TerminalBackend::default());
        let session = session(&backend, "cat");
        let (handle, _window) = open(cx, size(px(800.), px(600.)), &backend, &session);

        // Active as well as focused: `FocusState` asks both, and a press is
        // refused when either half is missing.
        cx.update_window(handle, |_, window, _| window.activate_window())
            .expect("the window activates");
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            window.press("q", cx);
        })
        .expect("the window takes a key press");
        cx.run_until_parked();

        let typed = wait_for_grid(&session, 'q');
        let _ = session.close().wait();

        assert!(typed, "the keystroke never reached the child's write path");
    }

    #[test]
    fn an_app_quit_ends_the_shell_and_leaves_nothing_to_end_twice() {
        let backend = Arc::new(TerminalBackend::default());
        let mut config = TerminalSessionConfig::new(
            next_id(),
            std::env::temp_dir(),
            INITIAL_COLUMNS,
            INITIAL_ROWS,
            Some(PathBuf::from("/bin/sh")),
        );
        config.args = Some(vec!["-c".to_owned(), "sleep 30".to_owned()]);
        config.drain_deadline = Duration::from_millis(500);
        let id = config.id.clone();

        spawn_and_claim(Arc::clone(&backend), config).expect("the shell spawns");

        // `end_shell` is the whole body of the quit handler, so asserting on it
        // asserts that a quit ends the shell, without quitting the test process
        // to find out.
        let report = end_shell()
            .expect("the quit path found a shell")
            .expect("the close sequence reported an error");
        assert_eq!(report.id, id, "the close reported about another shell");

        assert!(
            end_shell().is_none(),
            "a second quit must find nothing left to end"
        );
    }

    #[test]
    fn the_host_answers_from_the_measured_cell_and_the_views_palette() {
        let metrics = Arc::new(Mutex::new(UNMEASURED_CELL));
        let host = host(Arc::clone(&metrics));

        assert_eq!(
            (host.cell_metrics)(),
            UNMEASURED_CELL,
            "a child that asks before the first frame gets the conventional cell"
        );

        *metrics.lock().expect("the measured cell") = CellMetrics {
            width: 8,
            height: 17,
        };
        assert_eq!(
            (host.cell_metrics)(),
            CellMetrics {
                width: 8,
                height: 17
            },
            "the view publishes into the cell the host reads"
        );
        assert_eq!((host.resolve_color)(196), (255, 0, 0));
        (host.store_clipboard)("ignored");
    }

    #[test]
    fn the_working_directory_is_the_first_argument_that_is_one() {
        let dir = std::env::temp_dir();
        let args = vec![
            OsString::from("muster-native"),
            OsString::from("not-a-directory"),
            dir.clone().into_os_string(),
            std::env::temp_dir().join("also-not-here").into_os_string(),
        ];
        assert_eq!(directory_from(args), Some(dir));
    }

    #[test]
    fn no_argument_at_all_leaves_the_directory_undecided() {
        assert_eq!(
            directory_from([OsString::from("muster-native")]),
            None,
            "the program's own name is not a working directory"
        );
        let working = working_directory();
        assert!(
            working.is_absolute() || working == Path::new("."),
            "the shell must start somewhere real, not in a guess"
        );
    }
}
