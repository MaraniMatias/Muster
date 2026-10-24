use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::VisualTestContext;
use gpui_kit::{
    AnyWindowHandle, AppContext, ClipboardItem, MouseButton, Pixels, Size, TestAppContext,
};

use super::*;
use crate::app::sidebar_tests::{close, open_git};
use crate::terminal::Palette;

/// How long a test waits for a child before deciding it is not coming.
///
/// Generous, because a test that fails on a busy machine is a test about
/// scheduling, and finite, because every test in this file is bounded.
const PATIENCE: Duration = Duration::from_secs(10);

/// How often a waiting test looks.
const POLL: Duration = Duration::from_millis(10);

static NEXT_ID: AtomicU32 = AtomicU32::new(0);

fn next_id() -> String {
    format!("muster-test-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

/// A session running `script` under `/bin/sh`.
///
/// Not the login shell: a suite must not depend on which shell the person
/// running it has, nor on what their startup files do to a PTY.
fn session(backend: &Arc<TerminalBackend>, script: &str) -> TerminalSession {
    session_in(backend, script, &std::env::temp_dir())
}

/// The same session, in a directory of its own.
///
/// The directory is the shell's own, and a bare name is read against it: a
/// terminal that prints `LICENSE` means the file where the shell is sitting.
fn session_in(backend: &Arc<TerminalBackend>, script: &str, cwd: &Path) -> TerminalSession {
    let mut config = TerminalSessionConfig::new(
        next_id(),
        cwd.to_owned(),
        INITIAL_COLUMNS,
        INITIAL_ROWS,
        Some(PathBuf::from("/bin/sh")),
    );
    config.args = Some(vec!["-c".to_owned(), script.to_owned()]);
    config.status_interval = Duration::from_millis(20);
    config.drain_deadline = Duration::from_millis(500);
    let session =
        TerminalSession::spawn(Arc::clone(backend), config).expect("the test session spawns");
    // The child starts once the size it will keep is known: a session with no
    // view is sized here. `start_at` applies that size and starts the child in
    // one step, so the size is settled before the view measures its own: a
    // queued resize would land after the view's size and put the grid back at
    // the older one.
    session
        .start_at(INITIAL_COLUMNS, INITIAL_ROWS)
        .expect("the session takes the size it was opened at");
    session
}

/// Open a window over an existing session, at a size.
fn open(
    cx: &mut TestAppContext,
    size: Size<Pixels>,
    backend: &Arc<TerminalBackend>,
    session: &TerminalSession,
) -> (AnyWindowHandle, Entity<TerminalWindow>) {
    cx.update(gpui_kit::init);
    cx.update(crate::terminal::install_keys);
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
                    TerminalWindow::with_session(session.clone(), Arc::clone(backend), window, cx)
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

#[test]
fn measured_width_cache_admits_a_bounded_warm_working_set() {
    let mut widths = WidthCache::new();
    let unique_names = (0..=MAX_WIDTH_CACHE_ENTRIES_PER_SIZE)
        .map(|index| format!("distinct-row-{index}"))
        .collect::<Vec<_>>();

    for name in &unique_names {
        if should_cache_width(&widths, 12, name) {
            widths
                .entry(12)
                .or_default()
                .insert(name.clone(), name.len() as f32);
        }
    }
    assert_eq!(widths[&12].len(), MAX_WIDTH_CACHE_ENTRIES_PER_SIZE);
    let warm_hits = unique_names
        .iter()
        .filter(|name| widths[&12].contains_key(name.as_str()))
        .count();
    assert_eq!(warm_hits, MAX_WIDTH_CACHE_ENTRIES_PER_SIZE);
    for _ in 0..2 {
        for name in &unique_names {
            assert_eq!(
                should_cache_width(&widths, 12, name),
                widths[&12].contains_key(name.as_str()),
                "repeated traversals retain admitted widths and reject new entries"
            );
        }
    }

    let mut sizes = WidthCache::new();
    for size in 0..MAX_WIDTH_CACHE_SIZES as u32 {
        sizes.entry(size).or_default();
    }
    assert!(!should_cache_width(&sizes, u32::MAX, "new-size"));
    assert_eq!(sizes.len(), MAX_WIDTH_CACHE_SIZES);
}

/// The window manager draws the frame, everywhere but Windows.
///
/// The toolkit's client-side decorations reserve a twenty-pixel band on every
/// side for the shadow they draw, and that band is a margin around the app:
/// the window's own controls sit twenty pixels away from the corner the
/// pointer goes to, and the app looks framed rather than filling the window.
/// The band is gone when the window is tiled, so the app would look different
/// maximized than it does not. The controls those decorations draw do not
/// answer the click either, so minimize, maximize and close were three buttons
/// that did nothing.
#[cfg(not(target_os = "windows"))]
#[test]
fn the_window_manager_draws_the_frame() {
    assert_eq!(
        window_frame(),
        None,
        "the window asks the window manager for its frame, not for one of its own"
    );
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
fn chrome_focus_shortcut_returns_to_the_terminal_without_using_tab(cx: &mut TestAppContext) {
    let backend = Arc::new(TerminalBackend::default());
    let session = session(&backend, "cat");
    let (handle, app_entity) = open(cx, size(px(800.), px(600.)), &backend, &session);
    cx.update_window(handle, |_, window, _| window.activate_window())
        .expect("the window activates");
    settle(cx, handle);

    cx.update_window(handle, |_, window, cx| {
        window.focus_next(cx);
        window.render_frame(cx);
        let terminal = app_entity.read_with(cx, |app, _| {
            app.terminal().expect("terminal is open").clone()
        });
        let terminal_focused = terminal.read(cx).focus_handle(cx).is_focused(window);
        assert!(!terminal_focused, "the test did not move focus into chrome");
        let shortcut = if cfg!(target_os = "macos") {
            "cmd-shift-escape"
        } else {
            "ctrl-shift-escape"
        };
        window.press(shortcut, cx);
        let terminal = app_entity.read_with(cx, |app, _| {
            app.terminal().expect("terminal is open").clone()
        });
        let terminal_focused = terminal.read(cx).focus_handle(cx).is_focused(window);
        assert!(
            terminal_focused,
            "the shortcut did not return focus to the PTY"
        );
    })
    .expect("the window remains open");
    let _ = session.close().wait();
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

/// How often the background reads run, and what that costs when nobody is
/// looking.
///
/// Hidden has to be slower than visible or the cadence is not answering the
/// question, and it has to be finite or the numbers would only move while
/// somebody watched them — a figure that is wrong the moment they look away
/// is not a cached figure.
#[test]
fn a_hidden_window_reads_less_often_but_still_reads() {
    let visible = cadence(true);
    let hidden = cadence(false);
    assert!(
        hidden.discover_every > visible.discover_every,
        "a hidden window asks Git for the worktree list as often as a visible one"
    );
    assert!(
        hidden.count_every > visible.count_every,
        "a hidden window counts lines as often as a visible one"
    );
    for ticks in [visible.count_every, hidden.count_every] {
        assert!(ticks > 0, "a cadence of zero never runs");
        assert!(
            TICK * ticks as u32 <= std::time::Duration::from_secs(5 * 60),
            "a cadence that waits minutes cannot catch up with an external change"
        );
    }
    // And the hidden cadence is a multiple of the tick, so a pass still lands
    // on a tick boundary instead of drifting against one.
    assert_eq!(hidden.count_every % TICK.as_secs(), 0);
}

/// A checkout that cannot be counted is unknown, never zero.
///
/// Zero is a claim: it says Git looked and the working tree is clean. A timed
/// out read has not looked, and drawing `+0 -0` for it would be a confident
/// answer to a question nobody asked.
#[test]
fn an_uncounted_checkout_draws_nothing_rather_than_zero() {
    let clean = DiffTotals::default();
    assert!(
        TerminalWindow::line_slot(Some(&clean)).is_none(),
        "a clean working tree draws nothing, which is not the same as unknown"
    );
    assert_eq!(
        TerminalWindow::line_slot(None),
        TerminalWindow::line_slot(Some(&clean)),
        "a checkout Git was not asked about must not be drawn as if it were clean"
    );
    assert_eq!(
        TerminalWindow::line_slot(Some(&DiffTotals {
            additions: 3,
            deletions: 1,
        })),
        Some((3, 1)),
    );
}
/// A path the terminal printed opens the file in the preview, when the file is
/// one the checkout holds.
///
/// The gesture is the terminal's own and it is pinned in the terminal's tests;
/// what is pinned here is the hop from the terminal's event to the window's
/// answer: the name is resolved through the checkout, and a name that is not a
/// file of this checkout opens nothing.
#[gpui_kit::test]
async fn a_path_the_terminal_printed_opens_the_file_in_the_preview(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let checkout = fixture.checkout.clone();
    std::fs::write(fixture.root.join("code.rs"), "fn main() {}\n").unwrap();
    let backend = Arc::new(TerminalBackend::default());
    let session = session_in(&backend, "cat", &fixture.root);
    // The terminal the path was printed by, wired the way the window wires one.
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let shell = Arc::new(crate::session_owner::OwnedShell {
                session: session.clone(),
                metrics: Arc::new(Mutex::new(crate::session_owner::UNMEASURED_CELL)),
            });
            let terminal =
                cx.new(|cx| TerminalView::new(session.clone(), Arc::clone(&shell.metrics), cx));
            app.terminals.insert(
                "link-test".to_owned(),
                LiveTerminal {
                    checkout_id: checkout.clone(),
                    view: terminal.clone(),
                    shell,
                    prompt: None,
                },
            );
            app.attach_links(&terminal, &checkout, window, cx);
            terminal.update(cx, |_, cx| {
                cx.emit(crate::terminal::LinkTarget::Path("code.rs".into()));
            });
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(id, preview)| id == &checkout && preview.path() == "code.rs")
    })
    .await;

    // A name that is not a file of this checkout opens nothing, and says
    // nothing: the pane stays on the file it was showing.
    cx.update_window(fixture.handle, |_, _, cx| {
        fixture.view.update(cx, |app, cx| {
            let terminal = app
                .terminals
                .get("link-test")
                .expect("the terminal the path was printed by")
                .view
                .clone();
            terminal.update(cx, |_, cx| {
                cx.emit(crate::terminal::LinkTarget::Path("../outside.rs".into()));
            });
        });
    })
    .unwrap();
    for _ in 0..8 {
        cx.run_until_parked();
    }
    let shown = fixture.view.read_with(cx, |app, _| {
        app.open_document
            .as_ref()
            .map(|(_, preview)| preview.path().to_owned())
    });
    assert_eq!(
        shown.as_deref(),
        Some("code.rs"),
        "a name outside the checkout took the pane off the file it was showing"
    );
    close(&fixture, cx);
}

/// A refusal reaches the window's toast stack.
///
/// Through the real window, because that is the whole claim: the terminal view
/// notices, the window hears, and the message lands in the stack the window
/// draws. A test that called the view alone would pass with the sink never wired.
///
/// The refusal is raised rather than provoked. That the view raises one for a
/// keystroke and for a paste, and never quotes what it refused, is proven in
/// the view's own tests; what only a window can prove is the last hop.
#[gpui_kit::test]
fn a_refused_input_reaches_the_window_toast(cx: &mut TestAppContext) {
    let backend = Arc::new(TerminalBackend::default());
    let session = session(&backend, "cat");
    let (handle, view) = open(cx, size(px(800.), px(600.)), &backend, &session);
    settle(cx, handle);

    let terminal = view
        .read_with(cx, |app, _| app.terminal().cloned())
        .expect("an injected window has one terminal");
    cx.update_window(handle, |_, window, cx| {
        terminal.update(cx, |terminal, cx| {
            terminal.refuse_for_test(
                "A keystroke was not sent",
                &crate::terminal::SessionError::Closing,
                cx,
            )
        });
        window.render_frame(cx);
    })
    .unwrap();

    let message = view.read_with(cx, |app, _| app.toasts.latest_error().map(str::to_owned));
    assert!(
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("toast-stack").is_some()
        })
        .expect("the test window remains open"),
        "the refusal was not drawn as a toast"
    );
    let _ = session.close().wait();
    cx.update_window(handle, |_, window, _| window.remove_window())
        .unwrap();
    let message = message.expect("a refused input said nothing in a toast");
    assert!(
        message.contains("keystroke") && message.contains("closing"),
        "the toast does not say what was refused and why: {message:?}"
    );
}

/// A shell that exits takes its session with it, the way a terminal closes.
///
/// `exit` is how a shell is ended on purpose, and the session that outlives it
/// shows a frozen frame that looks like a hung app — the only way out would be
/// the close button on a row somebody has to go looking for. Legacy closed the
/// session on the same status this window reads, and it did so through the
/// close the person would have pressed: the process is already gone, so the
/// question about stopping a live process never comes up.
///
/// Through the window rather than the session, because that is the whole claim:
/// the session noticing its own exit is already pinned in the session's tests.
#[gpui_kit::test]
fn a_session_whose_shell_exited_is_closed(cx: &mut TestAppContext) {
    let backend = Arc::new(TerminalBackend::default());
    let session = session(&backend, "exit 0");
    let (handle, view) = open(cx, size(px(800.), px(600.)), &backend, &session);
    settle(cx, handle);

    let deadline = Instant::now() + PATIENCE;
    while !matches!(session.status().shell, ShellState::Exited { .. }) {
        assert!(Instant::now() < deadline, "the shell never exited");
        std::thread::sleep(POLL);
    }

    // The one call the window's tick makes on every pass.
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        view.update(cx, |app, cx| app.close_exited_terminals(window, cx));
    })
    .expect("the window takes its tick");
    cx.run_until_parked();

    let left = view.read_with(cx, |app, _| {
        (
            app.terminals.len(),
            app.navigation.active_session.is_none(),
            app.closing_sessions.len(),
        )
    });
    let _ = session.close().wait();
    cx.update_window(handle, |_, window, _| window.remove_window())
        .expect("the window closes");
    cx.run_until_parked();

    assert_eq!(
        left,
        (0, true, 0),
        "a session whose shell exited is still in the window: {} terminals, \
         an active session: {}, {} still closing",
        left.0,
        !left.1,
        left.2
    );
}

/// The app menu's Quit item is offered wherever the focus is.
///
/// The platform draws a menu item as offered when the action is answerable
/// along the focused element's dispatch path, so a workspace with nothing
/// focused — no terminal open, or a window whose focus has gone — has no path
/// to answer from and the item is drawn greyed out, which is a menu that
/// cannot quit the app.
#[gpui_kit::test]
fn the_app_menu_offers_quit_with_nothing_focused(cx: &mut TestAppContext) {
    use super::sidebar_tests::{dispose, open};

    let fixture = open(cx);
    cx.update(crate::menus::install);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.blur(cx);
        window.render_frame(cx);
    })
    .unwrap();
    assert!(
        !cx.update_window(fixture.handle, |_, window, cx| window.focused(cx).is_some())
            .unwrap(),
        "nothing holds the focus, which is what a workspace with no terminal open is"
    );
    assert!(
        cx.update(|cx| cx.is_action_available(&crate::menus::Quit)),
        "Quit is what the app menu's last item asks about, and it must be answerable"
    );
    dispose(&fixture, cx);
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
    for index in 0..=255 {
        assert_eq!(
            (host.resolve_color)(index),
            crate::terminal::xterm_256(index as u8)
        );
    }
    let palette = Palette::default();
    assert_eq!((host.resolve_color)(256), palette.foreground);
    assert_eq!((host.resolve_color)(257), palette.background);
    assert_eq!((host.resolve_color)(258), palette.cursor);
    (host.store_clipboard)("ignored");
}

#[test]
fn the_working_directory_is_the_first_argument_that_is_one() {
    let dir = std::env::temp_dir();
    let args = vec![
        OsString::from("Muster"),
        OsString::from("not-a-directory"),
        dir.clone().into_os_string(),
        std::env::temp_dir().join("also-not-here").into_os_string(),
    ];
    assert_eq!(directory_from(args), Some(dir));
}

#[test]
fn no_argument_at_all_leaves_the_directory_undecided() {
    assert_eq!(
        directory_from([OsString::from("Muster")]),
        None,
        "the program's own name is not a working directory"
    );
    let working = working_directory();
    assert!(
        working.is_absolute() || working == Path::new("."),
        "the shell must start somewhere real, not in a guess"
    );
}

#[test]
fn the_filesystem_root_is_not_restored_as_a_workdir() {
    let root = std::env::current_dir()
        .unwrap()
        .ancestors()
        .last()
        .unwrap()
        .to_path_buf();
    let mut nav = Navigation::new();
    let mut errors = Vec::new();
    restore_roots(
        &mut nav,
        &[root.to_string_lossy().into_owned()],
        &mut errors,
    );
    assert!(
        nav.repos.is_empty(),
        "the filesystem root was restored as a workdir: {:?}",
        nav.repos
    );
    assert!(errors.is_empty(), "the root is not a failure: {errors:?}");
}

/// Finishing a selection copies its text automatically, as in Legacy.
///
/// Through the real window, not the view alone: the application opens windows
/// with `gpui_kit::open_window`, which wraps the view in the toolkit root. That
/// root owns the text-selection layer and a global `Cmd+C` binding, so a copy
/// verified without it is a copy nobody can perform.
#[gpui_kit::test]
fn a_drag_in_the_real_window_selects_text_and_copies_it(cx: &mut TestAppContext) {
    let backend = Arc::new(TerminalBackend::default());
    let session = session(&backend, "printf selection-text; cat");
    let (handle, _window) = open(cx, size(px(800.), px(600.)), &backend, &session);
    settle(cx, handle);
    assert!(
        wait_for_grid(&session, 't'),
        "the child never drew the text to select"
    );

    let mut cx = VisualTestContext::from_window(handle, cx);
    cx.update(|window, _| window.activate_window());
    cx.write_to_clipboard(ClipboardItem::new_string("previous clipboard".to_owned()));
    // Start on the first row beneath the title bar: the bar is 32px tall, and
    // a drag that starts on it moves the window rather than selecting.
    cx.simulate_mouse_down(
        gpui_kit::point(px(4.), px(40.)),
        MouseButton::Left,
        Default::default(),
    );
    cx.simulate_mouse_move(
        gpui_kit::point(px(300.), px(500.)),
        MouseButton::Left,
        Default::default(),
    );
    cx.simulate_mouse_up(
        gpui_kit::point(px(300.), px(500.)),
        MouseButton::Left,
        Default::default(),
    );

    let selected = session
        .snapshot()
        .selection
        .expect("the drag selected nothing");
    assert!(
        selected.contains("selection-text"),
        "selected: {selected:?}"
    );

    // No keyboard shortcut: releasing the mouse must copy the selection.
    let copied = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .expect("the clipboard holds the selection");
    assert_eq!(copied, selected, "the clipboard is the selection");

    cx.simulate_click(gpui_kit::point(px(100.), px(80.)), Default::default());
    assert!(
        session.snapshot().selection.is_none(),
        "a plain click clears selection"
    );
    assert_eq!(
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .as_deref(),
        Some(selected.as_str()),
        "clearing a selection must not erase the clipboard"
    );

    let _ = session.close().wait();
}

/// A file's header row is its collapse control, the way the chevron beside it is.
///
/// The pointer is placed on the path, which is the part of the row that is not a
/// control, so the gesture has to be the row's own. Clicking opens the file's
/// diff — the line it holds is on screen and the window has asked Git for it —
/// and clicking again closes it and forgets the read, which is what closing a
/// file means everywhere else in the change set.
#[gpui_kit::test]
fn the_whole_diff_header_row_opens_and_closes_its_file(cx: &mut TestAppContext) {
    use crate::app::sidebar_tests::{close, open_git};
    use std::process::Command;

    let fixture = open_git(cx);
    let tracked = fixture.root.join("main.rs");
    std::fs::write(&tracked, "fn main() {}\n").unwrap();
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .current_dir(&fixture.root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["add", "main.rs"]);
    git(&[
        "-c",
        "user.email=muster@example.test",
        "-c",
        "user.name=Muster",
        "commit",
        "--quiet",
        "-m",
        "add a file",
    ]);
    std::fs::write(&tracked, "fn main() {\n    let x = 1;\n}\n").unwrap();

    cx.update_window(fixture.handle, |_, _, cx| {
        fixture.view.update(cx, |app, cx| {
            app.navigation.active_checkout = Some(fixture.checkout.clone());
            app.open_changes(&fixture.checkout, None, cx);
        })
    })
    .unwrap();
    cx.run_until_parked();
    let key = (fixture.checkout.clone(), "main.rs".to_owned());

    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        window.click("diff-file-main.rs", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let opened = fixture.view.read_with(cx, |app, _| {
        app.expanded_diffs.contains(&key) && app.git_diff.contains_key(&key)
    });
    assert!(opened, "a click on the row reads the file's diff");
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.find("diff-file-main.rs").visible(),
            "the row is still there once it is open"
        );
        window.click("diff-file-main.rs", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let closed = fixture
        .view
        .read_with(cx, |app, _| !app.expanded_diffs.contains(&key));
    assert!(closed, "clicking the open row closes it again");
    close(&fixture, cx);
}

/// A diff's file header is Legacy's row: the letter that says what Git did,
/// then the path, then the counts, on one line.
///
/// The columns are read off the drawn bounds rather than off the markup. What
/// draws the type is pinned elsewhere and not here: the face is the theme's
/// (see `theme::tests`), and an absolute glyph width is not a stable thing to
/// assert here — the bundled faces are added by the test that needs them, so
/// the advance a run resolves to depends on the ambient font state.
#[gpui_kit::test]
fn a_diff_header_names_the_file_the_way_legacy_orders_its_columns(cx: &mut TestAppContext) {
    use crate::app::sidebar_tests::{close, open_git};
    use std::process::Command;

    let fixture = open_git(cx);
    let tracked = fixture.root.join("main.rs");
    std::fs::write(&tracked, "fn main() {}\n").unwrap();
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .current_dir(&fixture.root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["add", "main.rs"]);
    git(&[
        "-c",
        "user.email=muster@example.test",
        "-c",
        "user.name=Muster",
        "commit",
        "--quiet",
        "-m",
        "add a file",
    ]);
    std::fs::write(&tracked, "fn main() {\n    let x = 1;\n}\n").unwrap();

    cx.update_window(fixture.handle, |_, _, cx| {
        fixture.view.update(cx, |app, cx| {
            app.navigation.active_checkout = Some(fixture.checkout.clone());
            app.open_changes(&fixture.checkout, None, cx);
        })
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .unwrap();

    cx.update_window(fixture.handle, |_, window, _| {
        let letter = window.find("diff-status-main.rs").bounds();
        let path = window.find("diff-path-main.rs").bounds();
        assert!(
            (letter.center().y - path.center().y).abs() <= px(1.),
            "the letter and the path are one row"
        );
        assert!(
            letter.origin.x < path.origin.x,
            "the letter comes before the path"
        );
        assert!(
            letter.right() < path.right(),
            "the path is the row's long half and keeps its room"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// The diff pane is the pane's width, and its header is a row in it.
///
/// The toolkit lays a diff out at its longest line and scrolls sideways to reach
/// the rest, which takes the header with it: the letter, the path and the counts
/// are the answer to "what changed", and they leave with the code they name. A
/// line that wraps costs a wrapped row instead, and the header keeps its
/// columns — the path shortened to the room between the letter and the counts.
#[gpui_kit::test]
fn a_long_diff_line_leaves_the_header_in_the_pane(cx: &mut TestAppContext) {
    use crate::app::sidebar_tests::{close, open_git};
    use std::process::Command;

    let fixture = open_git(cx);
    let path = format!(
        "src/{}a-file-with-a-really-long-name-here.rs",
        "a-directory-with-a-long-name/".repeat(3)
    );
    let long_line = format!("fn main() {{ let a = \"{}\"; }}\n", "x".repeat(400));
    std::fs::create_dir_all(fixture.root.join(&path).parent().unwrap()).unwrap();
    std::fs::write(fixture.root.join(&path), "fn main() {}\n").unwrap();
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .current_dir(&fixture.root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["add", "."]);
    git(&[
        "-c",
        "user.email=muster@example.test",
        "-c",
        "user.name=Muster",
        "commit",
        "--quiet",
        "-m",
        "add a file",
    ]);
    std::fs::write(fixture.root.join(&path), long_line).unwrap();

    cx.update_window(fixture.handle, |_, _, cx| {
        fixture.view.update(cx, |app, cx| {
            app.navigation.active_checkout = Some(fixture.checkout.clone());
            app.open_changes(&fixture.checkout, None, cx);
        })
    })
    .unwrap();
    cx.run_until_parked();
    // The file is opened, because a collapsed file draws its header and nothing
    // else: the line that makes the pane wide has to be on screen for the pane
    // to be measured against it.
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        window.click(format!("diff-file-{path}"), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(
        fixture.view.read_with(cx, |app, _| app
            .git_diff
            .contains_key(&(fixture.checkout.clone(), path.clone()))),
        "the click read the file's diff"
    );
    cx.update_window(fixture.handle, |_, window, cx| {
        for _ in 0..4 {
            window.render_frame(cx);
        }
    })
    .unwrap();

    cx.update_window(fixture.handle, |_, window, _| {
        let row = window.find(format!("diff-file-{path}")).bounds();
        // The pane's own edge: its title row wears 12 pixels of padding, so the
        // close button's right edge plus that padding is where the pane ends.
        let pane = window.find("close-diff").bounds().right() + px(12.);
        assert!(
            row.right() <= pane,
            "a header row is the pane's width: {row:?} runs past {pane}"
        );
        let name = window.find(format!("diff-path-{path}")).bounds();
        let whole = crate::app::measured_width(
            &mut std::collections::HashMap::new(),
            window,
            &path,
            crate::diff::DIFF_CHROME_PX,
        );
        assert!(
            name.size.width < px(whole),
            "and a path too long for it is shortened to fit: {} of {whole}",
            name.size.width
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// A notice is read where the change is, so it is read at the size of the code.
///
/// The toolkit's diff refines its own root down to the chrome size, so a notice
/// that names no size of its own is drawn smaller than the code it stands in
/// for — which is the opposite of what a sentence about a change should do in a
/// pane full of 16px code. Both boxes are read off drawn rectangles in one
/// window: the notice, and the header's path, which is the chrome size on
/// purpose.
#[gpui_kit::test]
fn a_diff_notice_reads_at_the_size_of_the_code(cx: &mut TestAppContext) {
    use crate::app::sidebar_tests::{close, open_git};
    use std::process::Command;

    let fixture = open_git(cx);
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .current_dir(&fixture.root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    std::fs::write(fixture.root.join("main.rs"), "fn main() {}\n").unwrap();
    git(&["add", "main.rs"]);
    git(&[
        "-c",
        "user.email=muster@example.test",
        "-c",
        "user.name=Muster",
        "commit",
        "--quiet",
        "-m",
        "add a file",
    ]);
    // A symlink is a change with no text in it, so it opens as a notice.
    std::os::unix::fs::symlink("main.rs", fixture.root.join("link.rs")).unwrap();

    cx.update_window(fixture.handle, |_, _, cx| {
        fixture.view.update(cx, |app, cx| {
            app.navigation.active_checkout = Some(fixture.checkout.clone());
            app.open_changes(&fixture.checkout, None, cx);
        })
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("diff-file-link.rs", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        let notice = window.find("diff-notice-text-link.rs").bounds();
        let path = window.find("diff-path-link.rs").bounds();
        assert!(
            notice.size.height > path.size.height,
            "the notice reads at the code's size rather than the chrome's: \
             {} against {}",
            notice.size.height,
            path.size.height
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// The panels are the window's, at every size the window is.
///
/// A panel that draws itself at the width a drag last ended on leaves the rest
/// of its slot bare when the window is resized under it: the group rescales its
/// slots, the remembered width does not move, and what is left is a band of
/// background with a panel that stopped short of the edge. The same window is
/// measured at four sizes here, and the panels have to reach both edges at
/// every one of them.
#[gpui_kit::test]
fn the_workspace_panels_reach_both_edges_of_every_window_size(cx: &mut TestAppContext) {
    use crate::app::sidebar_tests::{close, open_git};

    let fixture = open_git(cx);
    for wanted in [
        size(px(1280.), px(800.)),
        size(px(1600.), px(1000.)),
        size(px(1000.), px(700.)),
        size(px(1400.), px(900.)),
    ] {
        cx.update_window(fixture.handle, |_, window, cx| {
            window.resize(wanted);
            window.bounds_changed(cx);
        })
        .expect("the window takes its new size");
        cx.update_window(fixture.handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            let viewport = window.viewport_size();
            let sidebar = window.find("navigation-panel").bounds();
            let inspector = window.find("inspector-panel").bounds();
            assert_eq!(
                sidebar.origin.x,
                px(0.),
                "at {wanted:?} the sidebar starts at {:?}",
                sidebar.origin.x
            );
            assert_eq!(
                inspector.right(),
                viewport.width,
                "at {wanted:?} the inspector ends at {:?} of a {viewport:?} window",
                inspector.right()
            );
        })
        .expect("the window lays out");
    }
    close(&fixture, cx);
}

/// The empty state belongs to the middle of the panel it is drawn in.
///
/// A checkout with no terminal open leaves the middle panel holding one
/// sentence and the action that fills it, and that pair belongs in the middle
/// of a panel rather than pinned under the title bar of a window 800px tall.
/// The state says so with `justify_center`, which centres nothing unless it is
/// as tall as the panel it centres itself in -- and `flex_1` was asking for that
/// height from a parent that lays its children out as blocks, where a flex
/// property grows nothing. Both rectangles are read off the drawn window: the
/// state, and the panel it has to cover, whose edges are the sidebar's and the
/// inspector's.
#[gpui_kit::test]
fn the_empty_state_sits_in_the_middle_of_the_main_panel(cx: &mut TestAppContext) {
    use crate::app::sidebar_tests::{close, open_git};

    let fixture = open_git(cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.resize(size(px(1280.), px(800.)));
        window.bounds_changed(cx);
        window.render_frame(cx);
        window.render_frame(cx);
        let sidebar = window.find("navigation-panel").bounds();
        let inspector = window.find("inspector-panel").bounds();
        let empty = window.find("empty-state").bounds();
        let action = window.find("empty-action").bounds();
        let panel = gpui_kit::point(
            px((sidebar.right().as_f32() + inspector.origin.x.as_f32()) / 2.),
            sidebar.center().y,
        );
        assert!(
            (empty.center().x - panel.x).abs() <= px(1.)
                && (empty.center().y - panel.y).abs() <= px(1.),
            "the empty state covers the panel between the sidebar and the \
             inspector, so its middle is the panel's: {empty:?} is centred at \
             {:?}, not {:?}",
            empty.center(),
            panel,
        );
        assert!(
            (action.center().x - panel.x).abs() <= px(1.),
            "and the action in it is centred on the panel's middle rather than \
             beside its left edge: {action:?} against {:?}",
            panel,
        );
    })
    .expect("the window lays out");
    close(&fixture, cx);
}

/// The grid is the middle panel, so it is what the middle panel is given.
///
/// A window that only grows is not a window the parser was told about: the PTY
/// keeps the columns and rows it was last asked for, and the grid is drawn at
/// that size into a body of another one.
#[gpui_kit::test]
fn resizing_the_window_resizes_the_terminal_grid(cx: &mut TestAppContext) {
    let backend = Arc::new(TerminalBackend::default());
    let session = session(&backend, "cat");
    let (handle, window) = open(cx, size(px(1200.), px(800.)), &backend, &session);
    settle(cx, handle);
    let before = window.read_with(cx, |app, cx| {
        app.terminal()
            .expect("a spawned shell has a grid")
            .read(cx)
            .applied_size()
    });

    cx.update_window(handle, |_, window, cx| {
        window.resize(size(px(1600.), px(1000.)));
        window.bounds_changed(cx);
    })
    .expect("the window takes its new size");
    settle(cx, handle);
    let after = window.read_with(cx, |app, cx| {
        app.terminal()
            .expect("a spawned shell has a grid")
            .read(cx)
            .applied_size()
    });

    assert!(
        after.0 > before.0 && after.1 > before.1,
        "a window that grew from {before:?} to 1600x1000 left the grid at {after:?}"
    );
    cx.update_window(handle, |_, window, _| window.remove_window())
        .expect("the window closes");
    cx.run_until_parked();
    let _ = session.close().wait();
}

/// The last line the shell writes is a line somebody can see.
///
/// The grid is one row per whole cell of the box it is given, and the child is
/// told about exactly those rows. A grid that reaches past the bottom of the
/// window does not have its last row clipped on screen -- the window cuts it
/// off, and the line the shell just wrote is a line nobody can read and no
/// scroll brings back. The title bar is what makes the difference: it is the
/// one part of this window the grid does not start at the top of.
#[gpui_kit::test]
fn the_grid_ends_inside_the_window(cx: &mut TestAppContext) {
    use crate::app::sidebar_tests::{dispose, open};

    let fixture = open(cx);
    // A window with room for the panels' own height, and one with less than
    // they ask for: the second is where a grid that measures what it was
    // given ends up with a row the window cannot show.
    for wanted in [size(px(1280.), px(800.)), size(px(1280.), px(190.))] {
        cx.update_window(fixture.handle, |_, window, cx| {
            window.resize(wanted);
            window.bounds_changed(cx);
            window.render_frame(cx);
            window.render_frame(cx);
        })
        .expect("the window takes its new size");
        cx.run_until_parked();

        let bottom = fixture.view.read_with(cx, |app, cx| {
            app.terminal()
                .expect("the fixture opens a shell")
                .read(cx)
                .drawn_bottom()
                .expect("the grid measured a cell")
        });
        let viewport = cx
            .update_window(fixture.handle, |_, window, _| window.viewport_size())
            .expect("the window has a viewport");
        assert!(
            bottom <= viewport.height,
            "at {wanted:?} the grid's last row ends at {bottom:?}, below a window that ends at \
             {:?}: the shell writes a line this window cannot show",
            viewport.height
        );
    }

    dispose(&fixture, cx);
}
