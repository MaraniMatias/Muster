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
//! rustup run 1.97.1 cargo test --locked -p muster perf:: \\
//!   --release -- --ignored --nocapture --test-threads=1
//! ```
//!
//! ## What it does not measure
//!
//! It does not paint. Headless shaping has no GPU, no font rasteriser and no
//! compositor, so a frame here is a snapshot and nothing else. Real rendering,
//! real input and real GPU latency are not in these numbers and must not be
//! read into them.

use crate::inspector::{prepare_tree, status_index, StatusIndex, TreeSource};
use gpui_kit::AppContext;
use gpui_kit::{div, Context, IntoElement, Render, TestAppContext, Window};
use muster_core::files::FileEntry;
use muster_core::navigation::{ChangedFile, GitStatus};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// How a grid is named in the report, so every case reads the same way.
fn grid(size: (u16, u16)) -> String {
    format!("{}x{}", size.0, size.1)
}

/// The loop counts, so two runs do the same work.
const ITERATIONS: usize = 2_000;

/// A common terminal grid and a large grid, identical across runs.
const SIZES: [(u16, u16); 2] = [(80, 24), (300, 100)];

/// One checkout's tree, as `(folders, files per folder)`, identical across runs.
///
/// Every folder is open, because a tree that has been walked is the tree this
/// measurement is about: preparation walks every visible row, not the sixty-odd
/// rows that happen to be on screen.
const TREES: [(usize, usize); 2] = [(10, 10), (40, 24)];

/// How many of the tree's rows Git had something to say about, as a share of them.
const CHANGED_SHARE: usize = 3;

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

fn report(label: &str, figures: &[Figure]) {
    println!("\n== {label} ==");
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
        let range = measure("session.selection_range()", || {
            std::hint::black_box(session.selection_range());
        });
        let snapshot = measure("session.snapshot()", || {
            std::hint::black_box(session.snapshot());
        });
        let painted = measure("session.painting_snapshot()", || {
            std::hint::black_box(session.painting_snapshot());
        });
        report(&grid(size), &[title, selection, range, snapshot, painted]);
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
        &grid((300, 100)),
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

/// A window with nothing in it, which is all this case needs: preparation
/// measures text and builds no element.
struct Empty;

impl Render for Empty {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// What a checkout's tree looks like to the window, held in memory.
///
/// The listings, the folders that are open, the ones still answering and the
/// ones that failed, and the Git status a share of the rows carry. It is built
/// once per case and never changes, so every measured iteration prepares exactly
/// the same tree, which is what makes two runs comparable.
struct TreeFixture {
    checkout: String,
    entries: HashMap<(String, String), Vec<FileEntry>>,
    loading: HashSet<(String, String)>,
    errors: HashMap<(String, String), String>,
    expanded: HashSet<(String, String)>,
    /// The checkout's changed files as the window holds them: the status itself and
    /// the index the tree reads out of it, both built when the read was accepted.
    status: GitStatus,
    statuses: StatusIndex,
    rows: usize,
}

/// A tree of `folders` open folders holding `per_folder` files each.
fn tree_fixture(folders: usize, per_folder: usize) -> TreeFixture {
    let checkout = "perf-checkout".to_owned();
    let mut entries = HashMap::new();
    let mut loading = HashSet::new();
    let mut errors = HashMap::new();
    let mut expanded = HashSet::new();
    let mut rows = folders;
    let root: Vec<FileEntry> = (0..folders)
        .map(|folder| FileEntry {
            name: format!("folder-{folder:03}"),
            path: format!("folder-{folder:03}"),
            directory: true,
        })
        .collect();
    entries.insert((checkout.clone(), String::new()), root);
    for folder in 0..folders {
        let directory = format!("folder-{folder:03}");
        let children: Vec<FileEntry> = (0..per_folder)
            .map(|file| FileEntry {
                name: format!("module_{file:03}.rs"),
                path: format!("{directory}/module_{file:03}.rs"),
                directory: false,
            })
            .collect();
        entries.insert((checkout.clone(), directory.clone()), children);
        expanded.insert((checkout.clone(), directory.clone()));
        // A folder still answering and one that failed, so the notes that stand in
        // for children are part of what is measured rather than an accident of a
        // fixture that never fails to read anything. Either one contributes a note
        // where its children would be, rather than the children themselves.
        let answered = match folder % (folders / 2).max(1) {
            0 => {
                rows += 1;
                loading.insert((checkout.clone(), directory));
                false
            }
            1 => {
                rows += 1;
                errors.insert(
                    (checkout.clone(), directory),
                    "Permission denied (os error 13)".to_owned(),
                );
                false
            }
            _ => true,
        };
        if answered {
            rows += per_folder;
        }
    }
    // A third of the rows carry a status, which is a working checkout rather than
    // one with a whole tree changed.
    let mut files = Vec::new();
    for folder in 0..folders {
        for file in (0..per_folder).step_by(CHANGED_SHARE) {
            files.push(ChangedFile {
                path: format!("folder-{folder:03}/module_{file:03}.rs"),
                old_path: None,
                status: "M".to_owned(),
                additions: Some(file as u64),
                deletions: Some(1),
            });
        }
    }
    let status = GitStatus {
        branch: Some("main".to_owned()),
        head: Some("0f1e2d3".to_owned()),
        default_branch: "main".to_owned(),
        ahead_count: 0,
        files,
    };
    let statuses = status_index(&status);
    TreeFixture {
        checkout,
        entries,
        loading,
        errors,
        expanded,
        status,
        statuses,
        rows,
    }
}

/// The file tree's per-render preparation, on a tree held in memory.
///
/// Preparation is what a parent render does before it builds a single element: it
/// walks every visible row of the checkout's tree, reads what Git did to each one
/// and measures how wide the widest of them wants to be. Building the rows on
/// screen, laying them out and painting them are not in these numbers, and neither
/// is the fixture, which is timed and printed on its own so the two are never
/// added together into a figure that describes neither.
///
/// The widths are warm, because a name's width does not change between frames
/// and the window measures each one once. A first frame pays for shaping every
/// name once; the frames after it are what this measures.
#[gpui_kit::test]
#[ignore = "a measurement, not a test; run it with --ignored --nocapture"]
fn perf_files_tree_preparation(cx: &mut TestAppContext) {
    for (folders, per_folder) in TREES {
        let started = Instant::now();
        let fixture = tree_fixture(folders, per_folder);
        let built = started.elapsed();
        let (handle, _view) = cx
            .update(|cx| {
                gpui_kit::init(cx);
                crate::app::load_fonts(cx);
                crate::theme::install(cx);
                gpui_kit::open_window(crate::app::window_bounds(cx), cx, |_, cx| cx.new(|_| Empty))
            })
            .unwrap();
        cx.update_window(handle, |_, window, _| {
            let mut widths = HashMap::new();
            let source = TreeSource {
                checkout: &fixture.checkout,
                entries: &fixture.entries,
                loading: &fixture.loading,
                errors: &fixture.errors,
                expanded: &fixture.expanded,
                statuses: Some(&fixture.statuses),
            };
            // The first preparation measures every name in the tree, which is a
            // cost of the first frame and not of the frames after it.
            let prepared = prepare_tree(&source, &mut widths, window);
            assert_eq!(
                prepared.rows.len(),
                fixture.rows,
                "the fixture is the tree the measurement claims to walk"
            );
            assert_eq!(
                prepared.statuses.map(|index| index.len()),
                Some(fixture.status.files.len()),
                "the index the tree reads is the accepted status, by path"
            );
            let mut widest = 0.;
            let prepared = measure("tree preparation (warm widths)", || {
                widest = prepare_tree(&source, &mut widths, window).widest;
            });
            assert!(widest > 0., "the tree is measured, not skipped");
            report(
                &format!(
                    "tree: {folders} folders x {per_folder} files = {} rows",
                    fixture.rows
                ),
                &[
                    Figure {
                        name: "fixture construction",
                        total: built,
                        each: built,
                    },
                    prepared,
                ],
            );
        })
        .unwrap();
    }
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
