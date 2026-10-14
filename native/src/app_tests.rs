use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::test::TestWindowExt;
use gpui_kit::VisualTestContext;
use gpui_kit::{
    AnyWindowHandle, AppContext, ClipboardItem, MouseButton, Pixels, Size, TestAppContext,
};

use super::*;
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
/// A refusal reaches the banner the window already draws.
///
/// Through the real window, because that is the whole claim: the terminal view
/// notices, the window hears, and the message lands in the one place this
/// window says something went wrong. A test that called the view alone would
/// pass with the sink never wired at all.
///
/// The refusal is raised rather than provoked. That the view raises one for a
/// keystroke and for a paste, and never quotes what it refused, is proven in
/// the view's own tests; what only a window can prove is the last hop.
#[gpui_kit::test]
fn a_refused_input_reaches_the_window_banner(cx: &mut TestAppContext) {
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

    let message = view.read_with(cx, |app, _| app.failure.clone());
    let _ = session.close().wait();
    cx.update_window(handle, |_, window, _| window.remove_window())
        .unwrap();
    let message = message.expect("a refused input said nothing in the banner");
    assert!(
        message.contains("keystroke") && message.contains("closing"),
        "the banner does not say what was refused and why: {message:?}"
    );
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
