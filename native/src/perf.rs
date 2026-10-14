//! A finite, reproducible cost check for the terminal's hot paths.
//!
//! Every number this module prints is measured on the machine that ran it, at
//! the sizes named here, with the toolchain pinned. Nothing here asserts a
//! budget: a threshold written on one laptop is a lie on another, and a test
//! that fails on a slow CI box teaches people to ignore it. What it does is
//! make the measurement reproducible — same sizes, same loop counts, same
//! printed table — so two runs can be compared and a regression can be seen as
//! a number rather than as a feeling.
//!
//! ## Running it
//!
//! ```sh
//! scripts/perf-native.sh
//! ```
//!
//! or directly:
//!
//! ```sh
//! rustup run 1.97.1 cargo test --locked -p muster-native perf:: \\
//!   --release -- --ignored --nocapture --test-threads=1
//! ```
//!
//! ## What it does not measure
//!
//! It does not paint. Headless shaping has no GPU, no font rasteriser and no
//! compositor, so a frame here is a snapshot and nothing else. Real rendering,
//! real input and real GPU latency are not in these numbers and must not be
//! read into them.

use std::time::{Duration, Instant};

/// The loop counts, so two runs do the same work.
const ITERATIONS: usize = 2_000;

/// A common terminal grid and a large grid, identical across runs.
const SIZES: [(u16, u16); 2] = [(80, 24), (300, 100)];

/// One measured figure.
struct Figure {
    name: &'static str,
    total: Duration,
    each: Duration,
}

/// Time `work` `ITERATIONS` times and report it.
fn measure(name: &'static str, mut work: impl FnMut()) -> Figure {
    let start = Instant::now();
    for _ in 0..ITERATIONS {
        work();
    }
    let total = start.elapsed();
    Figure {
        name,
        total,
        each: total / ITERATIONS as u32,
    }
}

fn report(size: (u16, u16), figures: &[Figure]) {
    println!("\n== {}x{} ==", size.0, size.1);
    println!("{:<34} {:>12} {:>12}", "operation", "total", "each");
    for figure in figures {
        println!(
            "{:<34} {:>12} {:>12}",
            figure.name,
            format!("{:.3} ms", figure.total.as_secs_f64() * 1e3),
            format!("{:.3} us", figure.each.as_secs_f64() * 1e6)
        );
    }
}

/// The hot paths, at one size, against a live child.
#[gpui_kit::test]
#[ignore = "a measurement, not a test; run it with --ignored --nocapture"]
fn perf_snapshot_title_and_selection() {
    // Each case owns its own backend, so a closed session leaves nothing behind.
    for size in SIZES {
        // A child that fills the grid and then waits: the measurement is of a
        // grid with something in it, not of an empty one.
        let session = script_session(&format!(
            "i=0; while [ $i -lt {} ]; do printf 'line %s padded padding padding padding\\n' \"$i\"; i=$((i+1)); done; printf '\\033]2;perf-ready\\007'; sleep 30",
            size.1 * size.0 / 2
        ), size);
        wait_for_output(&session);
        session.select((0, 0), (usize::from(size.1) - 2, usize::from(size.0) - 1));
        assert!(!session
            .selection_text()
            .expect("a populated selection")
            .is_empty());

        let title = measure("session.title()", || {
            std::hint::black_box(session.title());
        });
        let selection = measure("session.selection_text()", || {
            std::hint::black_box(session.selection_text());
        });
        let snapshot = measure("session.snapshot()", || {
            std::hint::black_box(session.snapshot());
        });
        let painted = measure("session.painting_snapshot()", || {
            std::hint::black_box(session.painting_snapshot());
        });
        report(size, &[title, selection, snapshot, painted]);
        assert!(session.close().wait().expect("close completes").closed);
    }
}

/// What an idle session costs, which is what a hidden window wakes up for.
#[gpui_kit::test]
#[ignore = "a measurement, not a test; run it with --ignored --nocapture"]
fn perf_idle_session() {
    // Each case owns its own backend, so a closed session leaves nothing behind.
    let session = script_session("printf '\\033]2;perf-ready\\007'; sleep 30", (300, 100));
    wait_for_output(&session);
    // Nothing has been drawn, so nothing changes between iterations: this is
    // the floor, not a working session.
    let idle = measure("idle session.title()", || {
        std::hint::black_box(session.title());
    });
    let idle_snapshot = measure("idle session.snapshot()", || {
        std::hint::black_box(session.snapshot());
    });
    let start = Instant::now();
    assert!(session.close().wait().expect("close completes").closed);
    let elapsed = start.elapsed();
    report(
        (300, 100),
        &[
            idle,
            idle_snapshot,
            Figure {
                name: "close().wait()",
                total: elapsed,
                each: elapsed,
            },
        ],
    );
}

/// A child running `script` under `/bin/sh`, with a short drain deadline so a
/// measurement is not dominated by waiting for a child to be polite.
fn script_session(script: &str, size: (u16, u16)) -> crate::terminal::TerminalSession {
    use std::path::PathBuf;
    use std::time::Duration;
    let mut config = crate::terminal::TerminalSessionConfig::new(
        format!("muster-perf-{}", script.len()),
        std::env::temp_dir(),
        size.0,
        size.1,
        Some(PathBuf::from("/bin/sh")),
    );
    config.args = Some(vec!["-c".to_owned(), script.to_owned()]);
    config.status_interval = Duration::from_millis(50);
    config.drain_deadline = Duration::from_millis(200);
    crate::terminal::TerminalSession::spawn(backend(), config).expect("the session spawns")
}

fn backend() -> std::sync::Arc<crate::pty::TerminalBackend> {
    crate::pty::TerminalBackend::default().into()
}

/// The final OSC title is a barrier: preceding output has all been parsed.
fn wait_for_output(session: &crate::terminal::TerminalSession) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if session.title().as_deref() == Some("perf-ready") {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("child output did not finish within ten seconds");
}
