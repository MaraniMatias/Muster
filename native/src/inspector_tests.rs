use super::*;
use crate::app::sidebar_tests::{close, open_git, GitFixture};
use crate::inspector::{sticky_ancestors, tree_window, within, Ancestor, TreeRow};
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{px, TestAppContext};
use std::rc::Rc;

#[test]
fn change_status_keeps_uncommon_git_codes_visible() {
    for status in ["R", "C", "T", "??", "!!", "X"] {
        assert_eq!(crate::inspector::change_marker(status), status);
    }
    assert_eq!(crate::inspector::file_marker("R "), "");
    assert_eq!(crate::inspector::file_marker("??"), "");
}

/// A row wears the icon its own name and kind resolve to, in a box the row's metrics already
/// name: 14 pixels, the size the whole inspector draws its icons at.
///
/// This is the row that used to wear one folder glyph for every directory and one page glyph for
/// every file, so the assertion is that the two names resolve to two different drawings rather
/// than that an icon is present.
#[gpui_kit::test]
async fn a_file_row_wears_the_icon_its_own_name_resolves_to(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::create_dir_all(fixture.root.join("docs")).unwrap();
    std::fs::write(fixture.root.join("main.rs"), "fn main() {}\n").unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.file_entries
                .remove(&(fixture.checkout.clone(), String::new()));
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_directory(repo, checkout, String::new(), window, cx);
        })
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .file_entries
            .get(&(fixture.checkout.clone(), String::new()))
            .is_some_and(|entries| entries.iter().any(|entry| entry.path == "main.rs"))
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        let file = window.find("file-icon-main.rs");
        let folder = window.find("file-icon-docs");
        assert!(file.visible(), "a file row wears an icon");
        assert!(folder.visible(), "a directory row wears an icon too");
        assert_eq!(file.bounds().size.width, px(14.));
        assert_eq!(folder.bounds().size.height, px(14.));
    })
    .unwrap();
    close(&fixture, cx);
}

/// Opening a folder opens it inside the tree that is already open, and not instead of it.
///
/// The defect this pins: a listing arriving cleared every other expansion in the same
/// checkout, so opening a folder two levels down collapsed everything above it and the
/// subtree being walked vanished the instant its child finished loading. One level was
/// never enough to show it, which is why it survived a click-through.
#[gpui_kit::test]
async fn opening_a_nested_folder_keeps_the_folders_above_it_open(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::create_dir_all(fixture.root.join("outer/inner")).unwrap();
    std::fs::write(fixture.root.join("outer/inner/deep.txt"), "content").unwrap();
    let key = |path: &str| (fixture.checkout.clone(), path.to_owned());

    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.file_entries.remove(&key(""));
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_directory(repo, checkout, String::new(), window, cx);
        })
    })
    .unwrap();
    for path in ["outer", "outer/inner"] {
        cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
            let view = fixture.view.read(cx);
            view.file_entries.contains_key(&key("")) || view.file_entries.contains_key(&key(path))
        })
        .await;
        let control = gpui_kit::ElementId::from(format!("file-control:{path}"));
        cx.update_window(fixture.handle, |_, window, cx| {
            window.render_frame(cx);
            window.click(control, cx);
        })
        .unwrap();
        cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
            fixture.view.read(cx).file_entries.contains_key(&key(path))
        })
        .await;
        cx.update_window(fixture.handle, |_, window, cx| {
            window.render_frame(cx);
            let (outer, opened) = fixture.view.read_with(cx, |app, _| {
                (
                    app.expanded_files.contains(&key("outer")),
                    app.expanded_files.contains(&key(path)),
                )
            });
            assert!(outer, "outer stays open whatever happens below it");
            assert!(opened, "{path} is the folder just opened");
        })
        .unwrap();
    }
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .try_find("file-control:outer/inner/deep.txt")
                .is_some(),
            "the grandchild is on screen: the tree above it did not collapse"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// Collapsing a directory forgets the whole subtree, not just the directory.
///
/// Everything under a closed folder is unreachable by definition: leaving the
/// descendants expanded and their listings resident is what made a tree walked
/// wide and closed again stay in memory, and what made it come back open. The
/// prefix has to carry the separator, so `src-tauri` survives closing `src`.
#[gpui_kit::test]
async fn collapsing_a_folder_forgets_everything_under_it(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::create_dir_all(fixture.root.join("outer/inner")).unwrap();
    std::fs::write(fixture.root.join("outer/inner/deep.txt"), "content").unwrap();
    std::fs::create_dir_all(fixture.root.join("src-tauri")).unwrap();
    std::fs::write(fixture.root.join("src-tauri/kept.txt"), "content").unwrap();
    let key = |path: &str| (fixture.checkout.clone(), path.to_owned());

    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.file_entries.remove(&key(""));
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_directory(repo, checkout, String::new(), window, cx);
        })
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .file_entries
            .get(&key(""))
            .is_some_and(|entries| entries.len() == 2)
    })
    .await;
    for path in ["outer", "outer/inner", "src-tauri"] {
        let control = gpui_kit::ElementId::from(format!("file-control:{path}"));
        cx.update_window(fixture.handle, |_, window, cx| {
            // Two frames: the first lands the listing that made this row exist, the
            // second is the one that has it.
            window.render_frame(cx);
            window.render_frame(cx);
            window.click(control, cx);
        })
        .unwrap();
        cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
            fixture.view.read(cx).file_entries.contains_key(&key(path))
        })
        .await;
    }

    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        let close = gpui_kit::ElementId::from("file-control:outer");
        window.click(close, cx);
    })
    .unwrap();
    let checkout = fixture.checkout.clone();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        fixture.view.read_with(cx, |app, _| {
            let under =
                |(owner, path): &(String, String)| owner == &checkout && within(path, "outer");
            let open: Vec<&String> = app
                .expanded_files
                .iter()
                .filter(|e| under(e))
                .map(|(_, p)| p)
                .collect();
            let resident: Vec<&String> = app
                .file_entries
                .keys()
                .filter(|e| under(e))
                .map(|(_, p)| p)
                .collect();
            assert!(open.is_empty(), "nothing under it stays open: {open:?}");
            assert!(
                resident.is_empty(),
                "nothing under it stays resident: {resident:?}"
            );
            assert!(
                app.expanded_files
                    .contains(&(checkout.clone(), "src-tauri".into())),
                "a sibling whose name merely starts the same is not under it"
            );
            assert!(app
                .file_entries
                .contains_key(&(checkout.clone(), "src-tauri".into())));
        });
    })
    .unwrap();
    close(&fixture, cx);
}

/// A folder that is still answering says so inside the tree, and the rows around it
/// stay where they were.
///
/// The note used to be drawn below the list, where its height came out of the list's
/// box: opening a folder shrank the tree by one row and moved everything under it.
/// A note is now a row like any other, at the depth its children would have been.
#[gpui_kit::test]
async fn a_folder_that_is_still_loading_says_so_in_place(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::create_dir_all(fixture.root.join("outer")).unwrap();
    std::fs::write(fixture.root.join("twin.txt"), "x").unwrap();
    let key = |path: &str| (fixture.checkout.clone(), path.to_owned());

    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.file_entries.remove(&key(""));
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_directory(repo, checkout, String::new(), window, cx);
        })
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture.view.read(cx).file_entries.contains_key(&key(""))
    })
    .await;

    // Mark the folder as answering without letting it answer: this is the instant a
    // person spends looking at a folder, which is when a moving tree is noticed.
    fixture.view.update(cx, |app, cx| {
        app.expanded_files.insert(key("outer"));
        app.file_loading.insert(key("outer"));
        cx.notify();
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        let before = window.find("file-icon-outer").bounds().origin.y;
        assert!(
            window.find("file-note-outer-text").visible(),
            "the folder says so in the tree"
        );
        assert_eq!(
            window.find("retry-list-outer").label(),
            Some("Retry listing outer"),
            "a note offers the one thing that can be done about it, and names what it reads"
        );
        assert_eq!(
            before,
            window.find("file-icon-outer").bounds().origin.y,
            "the folder's own row did not move"
        );
        assert!(
            window.try_find("file-icon-outer/anything").is_none(),
            "a folder that has not answered has no children yet"
        );
    })
    .unwrap();

    // The rows after it keep their place: nothing below the list changed height.
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("file-icon-twin.txt").visible());
        assert!(
            window.find("file-icon-twin.txt").bounds().origin.y
                > window.find("file-note-outer-text").bounds().origin.y,
            "the note is in the order of the tree, not appended under it"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// Legacy's fixture: two folders open over eighty files, which is the shape its own
/// sticky test builds.
fn legacy_tree(files: usize) -> Vec<TreeRow> {
    let chain: Rc<[Ancestor]> = vec![
        Ancestor {
            path: "src".into(),
            name: "src".into(),
            depth: 0,
            index: 0,
        },
        Ancestor {
            path: "src/nested".into(),
            name: "nested".into(),
            depth: 1,
            index: 1,
        },
    ]
    .into();
    let mut rows = vec![
        TreeRow::Entry {
            entry: entry_at("src", 0, true),
            depth: 0,
            ancestors: vec![chain[0].clone()].into(),
        },
        TreeRow::Entry {
            entry: entry_at("nested", 1, true),
            depth: 1,
            ancestors: chain.clone(),
        },
    ];
    rows.extend((0..files).map(|index| TreeRow::Entry {
        entry: entry_at(&format!("f{index}"), 2, false),
        depth: 2,
        ancestors: chain.clone(),
    }));
    rows
}

fn names(rows: &[TreeRow], top: usize, stack: usize) -> Vec<String> {
    sticky_ancestors(rows, top, stack)
        .into_iter()
        .map(|ancestor| ancestor.name)
        .collect()
}

/// The rule is Legacy's, so it is pinned against Legacy's own cases.
#[test]
fn the_folder_stack_is_the_rule_legacy_pins() {
    let tree = legacy_tree(80);
    // At the top of the tree the row on screen is the folder itself: nothing to stack.
    assert_eq!(names(&tree, 0, 8), Vec::<String>::new());
    // Everywhere else the chain above the row is the stack, and the folders the stack
    // covers are not repeated in it — the row they are drawn on is still on screen.
    assert_eq!(names(&tree, 1, 8), ["src", "nested"]);
    assert_eq!(names(&tree, 40, 8), ["src", "nested"]);
    // Past the end of the tree, the chain of the last row is what is left.
    assert_eq!(names(&tree, 10_000, 8), ["src", "nested"]);
    // Trimmed to what the panel holds, keeping the folders nearest the row.
    assert_eq!(names(&tree, 40, 1), ["nested"]);
}

/// A tree of ten thousand rows must not cost a scan to stack. The chain is carried on the
/// row, so the answer is the same whatever the tree's size — which is the whole reason it
/// is carried rather than looked up.
#[test]
fn a_wide_tree_stacks_the_same_way_a_narrow_one_does() {
    let narrow = legacy_tree(2);
    let wide = legacy_tree(10_000);
    for top in [0, 1, 2, 3, 9, 500, 10_000] {
        assert_eq!(
            names(&wide, top, 8),
            names(&narrow, top.min(narrow.len()), 8),
            "the size of the tree must not change what is stacked at row {top}"
        );
    }
}

/// A folder that has not answered has no chain: a note stands where its children would be
/// and must not put its parent folders into the stack a second time.
#[test]
fn a_note_does_not_carry_a_chain() {
    let chain: Rc<[Ancestor]> = vec![Ancestor {
        path: "src".into(),
        name: "src".into(),
        depth: 0,
        index: 0,
    }]
    .into();
    let rows = vec![
        TreeRow::Entry {
            entry: entry_at("src", 0, true),
            depth: 0,
            ancestors: chain.clone(),
        },
        TreeRow::Note {
            path: "src".into(),
            message: "Loading folder…".into(),
            depth: 1,
            ancestors: chain,
        },
    ];
    assert_eq!(names(&rows, 2, 8), ["src"]);
    assert_eq!(names(&rows, 1, 8), ["src"]);
}

/// The stack is drawn over the tree when the tree is scrolled, and clicking a folder in
/// it brings that folder back into view.
///
/// The stack is read from where the tree is scrolled to, so the only way to see it is to
/// scroll: `UniformListScrollHandle::scroll_to_item` is what a click on a stacked folder
/// uses, so the test uses it too.
#[gpui_kit::test]
async fn the_folder_stack_appears_over_a_scrolled_tree(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::create_dir_all(fixture.root.join("src/nested")).unwrap();
    for index in 0..40 {
        std::fs::write(
            fixture.root.join(format!("src/nested/f{index:02}.txt")),
            "x",
        )
        .unwrap();
    }
    let key = |path: &str| (fixture.checkout.clone(), path.to_owned());

    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.file_entries.remove(&key(""));
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_directory(repo, checkout, String::new(), window, cx);
        })
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture.view.read(cx).file_entries.contains_key(&key(""))
    })
    .await;
    for path in ["src", "src/nested"] {
        let control = gpui_kit::ElementId::from(format!("file-control:{path}"));
        cx.update_window(fixture.handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            window.click(control, cx);
        })
        .unwrap();
        cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
            fixture.view.read(cx).file_entries.contains_key(&key(path))
        })
        .await;
    }

    // At the top of the tree there is nothing stacked: the row on screen is the folder.
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("file-stack").is_none(),
            "a tree at the top of itself has nothing to stack"
        );
    })
    .unwrap();

    cx.update(|cx| {
        fixture.view.update(cx, |app, _| {
            app.files_scroll
                .set_offset(gpui_kit::point(px(0.), px(-30. * 26.)))
        })
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        let stack = window.find("file-stack");
        assert!(
            stack.visible(),
            "a scrolled tree names the folders it is inside"
        );
        assert!(window.try_find("file-stack-name-src").is_some());
        assert!(window.try_find("file-stack-name-src/nested").is_some());
        assert_eq!(
            window.find("file-stack:src/nested").label(),
            Some("Show nested in tree"),
            "a stacked folder is a way back to it"
        );
        assert_eq!(
            stack.bounds().size.height,
            px(26. * 2.),
            "the stack is as tall as the folders in it, and no taller"
        );
        assert!(
            window.find("file-icon-src").bounds().origin.y < stack.bounds().origin.y,
            "the folders it names have scrolled above the tree: that is why it is there"
        );
    })
    .unwrap();

    // Clicking one brings the tree back to where that folder's own row is.
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("file-stack:src/nested", cx);
    })
    .unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        assert_eq!(
            fixture.view.read(cx).files_scroll.offset().y,
            px(0.),
            "clicking a stacked folder brings it back to the top of the tree"
        );
        assert!(
            window.try_find("file-stack").is_none(),
            "back at the top, and the stack has gone with it"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// The window is Legacy's: a fixed number of rows with a few spare, and the tree's real
/// height accounted for as space above and below so the scrollbar is the tree's.
#[test]
fn the_window_covers_the_tree_and_leaves_the_rest_as_space() {
    let (drawn, above, below) = tree_window(40, 0);
    assert_eq!(
        drawn,
        0..40,
        "a tree smaller than the window is built whole"
    );
    assert_eq!((above, below), (0., 0.), "and it needs no space around it");

    let (drawn, above, below) = tree_window(1_000, 500);
    assert_eq!(drawn.len(), 64);
    assert_eq!(
        above,
        490. * 26.,
        "the rows above the window are still there to scroll"
    );
    assert_eq!(
        below,
        (1_000 - 554) as f32 * 26.,
        "and so are the ones below"
    );
    assert_eq!(above + below + drawn.len() as f32 * 26., 1_000. * 26.);

    // Near the top the spare rows are spent on what is above, and near the bottom the
    // window stops short rather than running past the end of the tree.
    let (drawn, above, _) = tree_window(1_000, 3);
    assert_eq!(drawn, 0..64);
    assert_eq!(above, 0.);
    let (drawn, _, below) = tree_window(1_000, 999);
    assert_eq!(drawn, 936..1_000);
    assert_eq!(below, 0.);
}

/// Every row of the tree is the height the tree says it is, whatever the tree's size.
///
/// The rows live in a flex column inside a scroller, and a flex column will shrink its
/// children to fit if it can. It could: the rows then stood 21 pixels apart instead of
/// 26, and the tree scrolled by less than it claimed.
#[gpui_kit::test]
async fn rows_keep_the_height_the_tree_scrolls_by(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    for index in 0..60 {
        std::fs::write(fixture.root.join(format!("f{index:03}.txt")), "x").unwrap();
    }
    let key = |path: &str| (fixture.checkout.clone(), path.to_owned());
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.file_entries.remove(&key(""));
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_directory(repo, checkout, String::new(), window, cx);
        })
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .file_entries
            .get(&key(""))
            .is_some_and(|entries| entries.len() == 60)
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        let first = window.find("file-icon-f000.txt").bounds().origin.y;
        let tenth = window.find("file-icon-f009.txt").bounds().origin.y;
        assert_eq!(
            tenth - first,
            px(9. * 26.),
            "nine rows apart is nine rows of the tree's own height"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// Two rows that agree about Git keep their own identity.
///
/// An element id names an element for the whole window, so an id built from the
/// status or a count alone would be worn by every row that agrees with it. Two
/// files modified the same way are the ordinary case, so this is the case.
#[gpui_kit::test]
async fn rows_agreeing_about_git_keep_distinct_identities(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    for name in ["one.txt", "two.txt"] {
        std::fs::write(fixture.root.join(name), "tracked\n").unwrap();
    }
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
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
        "add files",
    ]);
    for name in ["one.txt", "two.txt"] {
        std::fs::write(fixture.root.join(name), "changed\n").unwrap();
    }
    fixture.view.update(cx, |app, cx| {
        app.navigation.active_checkout = Some(fixture.checkout.clone());
        app.inspector_tab = InspectorTab::Changes;
        app.request_git_status(&fixture.checkout, cx);
        cx.notify();
    });
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_status
            .get(&fixture.checkout)
            .is_some_and(|status| status.files.len() == 2)
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        window.render_frame(cx);
        for name in ["one.txt", "two.txt"] {
            assert!(
                window.find(format!("row-{name}-status")).visible(),
                "{name} keeps its own status"
            );
            assert!(
                window.find(format!("row-{name}-add")).visible(),
                "{name} keeps its own counts"
            );
        }
    })
    .unwrap();
    close(&fixture, cx);
}

#[gpui_kit::test]
fn files_root_load_and_simultaneous_directory_expansions_keep_independent_results(
    cx: &mut TestAppContext,
) {
    let fixture = open_git(cx);
    std::fs::create_dir_all(fixture.root.join("left")).unwrap();
    std::fs::create_dir_all(fixture.root.join("right")).unwrap();
    std::fs::write(fixture.root.join("left/a.txt"), "a").unwrap();
    std::fs::write(fixture.root.join("right/b.txt"), "b").unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.file_entries
                .remove(&(fixture.checkout.clone(), String::new()));
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            // Request through render, which supplies the active window context.
            app.request_directory(repo, checkout, String::new(), window, cx);
            app.inspector_tab = InspectorTab::Files;
        })
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            for path in ["left", "right"] {
                app.expanded_files
                    .insert((fixture.checkout.clone(), path.into()));
                app.request_directory(repo.clone(), checkout.clone(), path.into(), window, cx);
            }
        })
    })
    .unwrap();
    cx.run_until_parked();
    fixture.view.read_with(cx, |app, _| {
        assert!(app.file_entries[&(fixture.checkout.clone(), "left".into())]
            .iter()
            .any(|entry| entry.name == "a.txt"));
        assert!(
            app.file_entries[&(fixture.checkout.clone(), "right".into())]
                .iter()
                .any(|entry| entry.name == "b.txt")
        );
    });
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn folder_activation_keeps_expanded_accessibility_state_in_sync(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::create_dir_all(fixture.root.join("folder")).unwrap();
    std::fs::write(fixture.root.join("folder/file.txt"), "content").unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.file_entries
                .remove(&(fixture.checkout.clone(), String::new()));
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_directory(repo, checkout, String::new(), window, cx);
        })
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .file_entries
            .get(&(fixture.checkout.clone(), String::new()))
            .is_some_and(|entries| entries.iter().any(|entry| entry.path == "folder"))
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("file-control:folder").expanded(), Some(false));
        window.click("file-control:folder", cx);
        assert_eq!(window.find("file-control:folder").expanded(), Some(true));
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .file_entries
            .get(&(fixture.checkout.clone(), "folder".into()))
            .is_some_and(|entries| entries.iter().any(|entry| entry.name == "file.txt"))
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("file-control:folder/file.txt").is_some());
        window.click("file-control:folder", cx);
        assert_eq!(window.find("file-control:folder").expanded(), Some(false));
        assert!(window.try_find("file-control:folder/file.txt").is_none());
    })
    .unwrap();
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn document_request_has_independent_token_and_changes_status_is_available(
    cx: &mut TestAppContext,
) {
    let fixture: GitFixture = open_git(cx);
    fixture.view.update(cx, |app, cx| {
        app.inspector_tab = InspectorTab::Changes;
        app.request_git_status(&fixture.checkout, cx);
    });
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_status
            .contains_key(&fixture.checkout)
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(app.inspector_tab, InspectorTab::Changes);
        assert_eq!(app.git_status[&fixture.checkout].default_branch, "main");
    });
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn changes_open_in_the_center_and_refresh_without_replacing_the_terminal(
    cx: &mut TestAppContext,
) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("change.txt"), "first\n").unwrap();
    let terminal_ids = fixture.view.read_with(cx, |app, _| {
        app.terminals.keys().cloned().collect::<Vec<_>>()
    });
    fixture.view.update(cx, |app, cx| {
        app.open_changes(&fixture.checkout, Some("change.txt".into()), cx)
    });
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_diff
            .contains_key(&(fixture.checkout.clone(), "change.txt".into()))
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(
            app.open_diff,
            Some((fixture.checkout.clone(), Some("change.txt".into())))
        );
        assert!(app.open_document.is_none());
        assert!(
            app.git_diff[&(fixture.checkout.clone(), "change.txt".into())]
                .patch
                .contains("+first")
        );
        for id in &terminal_ids {
            assert!(app.terminals.contains_key(id));
        }
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("refresh-changes").is_none(),
            "refresh must not be a permanent extra button"
        );
        assert!(
            window.try_find("all-changes").is_none(),
            "All changes is a view, not a second toolbar action"
        );
        assert_eq!(window.find("item-crumb").label(), Some("change.txt"));
        // The crumb naming a file wears that file's icon, at the crumb's own 12 pixels. A whole
        // change set names no file, so it wears the terminal glyph and has no icon to find.
        let icon = window.find("item-crumb-icon");
        assert!(icon.visible(), "the crumb names a file and wears its icon");
        assert_eq!(icon.bounds().size.width, px(12.));
        window.click("close-diff", cx);
    })
    .unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("item-crumb-icon").is_none(),
            "a terminal names no file and wears the glyph it always wore"
        );
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert!(app.open_diff.is_none());
        for id in &terminal_ids {
            assert!(app.terminals.contains_key(id));
        }
    });
    std::fs::write(fixture.root.join("change.txt"), "second\n").unwrap();
    fixture.view.update(cx, |app, cx| {
        app.open_changes(&fixture.checkout, Some("change.txt".into()), cx);
        app.request_git_status(&fixture.checkout, cx);
    });
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_diff
            .get(&(fixture.checkout.clone(), "change.txt".into()))
            .is_some_and(|diff| diff.patch.contains("+second"))
    })
    .await;
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_status
            .contains_key(&fixture.checkout)
    })
    .await;
    fixture
        .view
        .update(cx, |app, cx| app.open_changes(&fixture.checkout, None, cx));
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("item-crumb").label(), Some("All changes"));
        window.click("collapse-file", cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert!(app
            .expanded_diffs
            .contains(&(fixture.checkout.clone(), "change.txt".into())));
    });
    close(&fixture, cx);
}

/// Retry answers the failure it is showing.
///
/// A directory that could not be listed and a file that could not be read are
/// two different operations on one path, and a retry that repeats the wrong one
/// fails the same way forever: listing a file answers `ENOTDIR` every time.
#[gpui_kit::test]
async fn retry_after_a_failed_read_reads_the_file_again(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let path = "late.txt".to_owned();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, path.clone(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .document_errors
            .contains_key(&(fixture.checkout.clone(), "late.txt".into()))
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        assert!(
            !app.file_errors
                .contains_key(&(fixture.checkout.clone(), "late.txt".into())),
            "a failed read was filed as a failed listing"
        );
        assert!(app.open_document.is_none());
        assert_eq!(
            app.document_target.as_ref().map(|(_, path)| path.as_str()),
            Some("late.txt")
        );
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("document-loading").is_none());
        assert!(window.try_find("retry-read-late.txt").is_some());
        assert!(window.try_find("item-crumb").is_some());
    })
    .unwrap();
    std::fs::write(fixture.root.join(&path), "here now\n").unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("retry-read-{path}"), cx);
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, preview)| preview.text == "here now\n")
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        assert!(
            app.document_errors.is_empty(),
            "the failure outlived its retry"
        );
        assert!(app
            .file_errors
            .values()
            .all(|error| !error.contains("late.txt")));
    });
    close(&fixture, cx);
}

/// A refresh reads an expanded change's diff only where that diff is drawn.
#[gpui_kit::test]
async fn a_refresh_reads_expanded_diffs_only_while_the_change_set_is_open(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("change.txt"), "one\n").unwrap();
    let key = (fixture.checkout.clone(), "change.txt".to_owned());
    fixture.view.update(cx, |app, cx| {
        app.inspector_tab = InspectorTab::Changes;
        app.expanded_diffs.insert(key.clone());
        app.request_git_status(&fixture.checkout, cx);
    });
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_status
            .contains_key(&fixture.checkout)
    })
    .await;
    cx.run_until_parked();
    fixture.view.read_with(cx, |app, _| {
        assert!(
            app.git_diff_request.is_empty() && app.diff_queue.waiting() == 0,
            "a refresh read diffs that nothing was drawing"
        );
    });
    fixture.view.update(cx, |app, cx| {
        app.open_diff = Some((fixture.checkout.clone(), None));
        app.request_git_status(&fixture.checkout, cx);
    });
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        let app = fixture.view.read(cx);
        !app.git_status_running.contains(&fixture.checkout)
            && app
                .git_diff
                .get(&key)
                .is_some_and(|diff| diff.path == "change.txt")
    })
    .await;
    close(&fixture, cx);
}

/// A burst of expansions is answered: every one of them ends in a diff or in a
/// reported failure, and none of them is left waiting on a read that is not
/// coming.
#[gpui_kit::test]
async fn a_burst_of_expansions_answers_every_one_of_them(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    // Twice the four reads this window allows to exist at once, so the queue
    // has to hand out the rest as slots free.
    let count = 8;
    for index in 0..count {
        std::fs::write(fixture.root.join(format!("c{index}.txt")), "one\n").unwrap();
    }
    fixture.view.update(cx, |app, cx| {
        app.open_changes(&fixture.checkout, None, cx);
    });
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_status
            .get(&fixture.checkout)
            .is_some_and(|status| status.files.len() >= count)
    })
    .await;
    let paths: Vec<String> = fixture.view.read_with(cx, |app, _| {
        app.git_status[&fixture.checkout]
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect()
    });
    fixture.view.update(cx, |app, cx| {
        for path in &paths {
            app.expanded_diffs
                .insert((fixture.checkout.clone(), path.clone()));
            app.request_file_diff(&fixture.checkout, path, cx);
        }
    });
    let expected = paths.len();
    cx.wait_for(fixture.handle, Duration::from_secs(30), |_, cx| {
        let app = fixture.view.read(cx);
        app.diff_queue.running() == 0
            && app.diff_queue.waiting() == 0
            && app.git_diff_request.is_empty()
            && paths
                .iter()
                .filter(|path| {
                    app.git_diff
                        .contains_key(&(fixture.checkout.clone(), (*path).clone()))
                        || app
                            .git_diff_errors
                            .contains_key(&(fixture.checkout.clone(), (*path).clone()))
                })
                .count()
                == expected
    })
    .await;
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn large_json_uses_one_retained_unified_view_with_real_source_coordinates(
    cx: &mut TestAppContext,
) {
    use gpui_kit::component::diff::{DiffLineRange, DiffMode, DiffSide};
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("package.json"), "{}\n").unwrap();
    super::run_git(&fixture.root, &["add", "package.json"]);
    super::run_git(
        &fixture.root,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.test",
            "commit",
            "-qm",
            "json",
        ],
    );
    let body = format!(
        "{{\n{}  \"last\": true\n}}\n",
        (0..1500)
            .map(|i| format!("  \"key{i}\": {i},\n"))
            .collect::<String>()
    );
    std::fs::write(fixture.root.join("package.json"), &body).unwrap();
    let key = (fixture.checkout.clone(), "package.json".to_owned());
    fixture.view.update(cx, |app, cx| {
        app.open_changes(&fixture.checkout, Some("package.json".into()), cx)
    });
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture.view.read(cx).git_diff.contains_key(&key)
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    let first = fixture.view.read_with(cx, |app, cx| {
        let diff = &app.git_diff[&key];
        assert!(diff.large && !diff.too_large && diff.patch.contains("key1499"));
        let state = app.diff_view.as_ref().unwrap().state.clone();
        assert_eq!(state.read(cx).mode(), DiffMode::Unified);
        assert_eq!(state.read(cx).files()[0].language().as_ref(), "json");
        state
    });
    first.update(cx, |state, cx| {
        state.set_selected_lines(
            Some(DiffLineRange::new("package.json", DiffSide::Modified, 2, 3)),
            cx,
        );
        let selected = state.selected_text(cx);
        assert!(
            selected.contains("\"key0\"") && selected.contains("\"key1\""),
            "{selected}"
        );
        assert!(!selected.contains("@@") && !selected.starts_with('+'));
        state.set_selected_lines(
            Some(DiffLineRange::new("package.json", DiffSide::Original, 1, 1)),
            cx,
        );
        assert_eq!(state.selected_text(cx).trim(), "{}");
    });
    cx.update_window(fixture.handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(
            app.diff_view.as_ref().unwrap().state.entity_id(),
            first.entity_id()
        )
    });

    // All files share one native virtual list, not a nested viewport per file.
    std::fs::write(fixture.root.join("other.json"), "{\"ready\": true}\n").unwrap();
    fixture.view.update(cx, |app, cx| {
        app.open_changes(&fixture.checkout, None, cx);
        app.request_git_status(&fixture.checkout, cx);
    });
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_status
            .get(&fixture.checkout)
            .is_some_and(|s| s.files.len() == 2)
    })
    .await;
    fixture.view.update(cx, |app, cx| {
        for path in ["package.json", "other.json"] {
            app.expanded_diffs
                .insert((fixture.checkout.clone(), path.into()));
            app.request_file_diff(&fixture.checkout, path, cx);
        }
    });
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        let app = fixture.view.read(cx);
        app.git_diff.contains_key(&key)
            && app
                .git_diff
                .contains_key(&(fixture.checkout.clone(), "other.json".into()))
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    let all = fixture.view.read_with(cx, |app, cx| {
        let state = app.diff_view.as_ref().unwrap().state.clone();
        assert_eq!(state.read(cx).files().len(), 2);
        assert!(!state.read(cx).is_file_collapsed("package.json"));
        state
    });
    assert_ne!(all.entity_id(), first.entity_id());

    // Same-length content changes still replace parsed data, not the viewer entity.
    std::fs::write(fixture.root.join("other.json"), "{\"ready\": null}\n").unwrap();
    fixture
        .view
        .update(cx, |app, cx| app.request_git_status(&fixture.checkout, cx));
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_diff
            .get(&(fixture.checkout.clone(), "other.json".into()))
            .is_some_and(|d| d.patch.contains("null"))
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(
            app.diff_view.as_ref().unwrap().state.entity_id(),
            all.entity_id()
        )
    });
    // Parser refusals survive subsequent frames rather than becoming a clean diff.
    fixture.view.update(cx, |app, cx| {
        let diff = app.git_diff.get_mut(&key).unwrap();
        diff.patch = "not a unified patch".into();
        diff.revision.push_str("-invalid");
        cx.notify();
    });
    for _ in 0..2 {
        cx.update_window(fixture.handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
        fixture.view.read_with(cx, |app, _| {
            assert!(app
                .diff_view
                .as_ref()
                .unwrap()
                .parse_errors
                .contains_key("package.json"));
        });
    }
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn a_json_file_is_visible_in_the_actual_workspace_editor(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let source = "{\n  \"project\": \"Muster\",\n  \"enabled\": true\n}\n";
    std::fs::write(fixture.root.join("package.json"), source).unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "package.json".into(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture.view.read(cx).open_document.is_some()
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    let state = fixture.view.read_with(cx, |app, _| {
        app.document_view.as_ref().unwrap().state.clone()
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        let input = ("input", state.entity_id());
        let editor = window.find(input);
        assert_eq!(editor.value(), Some(source));
        assert!(
            editor.bounds().size.height.as_f32() > 200.,
            "editor is actually visible"
        );
        assert!(editor.bounds().size.width.as_f32() > 100.);
        window.click("close-document", cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert!(app.open_document.is_none());
        assert!(app.document_target.is_none());
    });
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn a_refresh_during_a_status_read_is_coalesced_not_lost(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let before = fixture.view.update(cx, |app, cx| {
        app.request_git_status(&fixture.checkout, cx);
        let before = app.git_status_request[&fixture.checkout];
        app.request_git_status(&fixture.checkout, cx);
        app.request_git_status(&fixture.checkout, cx);
        assert_eq!(
            app.git_status_request[&fixture.checkout],
            before.wrapping_add(2)
        );
        assert!(app.git_status_running.contains(&fixture.checkout));
        before
    });
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        let app = fixture.view.read(cx);
        !app.git_status_running.contains(&fixture.checkout)
            && app.git_status.contains_key(&fixture.checkout)
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(
            app.git_status_request[&fixture.checkout],
            before.wrapping_add(3),
            "two pending refreshes launch one fresh snapshot after the old read"
        );
    });
    close(&fixture, cx);
}

/// The two lists stand on Legacy's columns.
///
/// Every distance here is a difference between two drawn bounds, so the check
/// is about where a column starts and not about how wide a glyph is: the file
/// tree's fold slot at the panel's own 8px and one 14px step per level, a
/// change row one step in from the heading above it, and every row exactly as
/// tall as the next.
#[gpui_kit::test]
async fn the_file_and_change_lists_line_up_on_legacy_columns(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::create_dir_all(fixture.root.join("src")).unwrap();
    std::fs::write(fixture.root.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(fixture.root.join("top.txt"), "tracked\n").unwrap();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
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
        "add files",
    ]);
    std::fs::write(fixture.root.join("top.txt"), "changed\n").unwrap();

    fixture.view.update(cx, |app, cx| {
        app.navigation.active_checkout = Some(fixture.checkout.clone());
        app.inspector_tab = InspectorTab::Files;
        cx.notify();
    });
    // The window opened before these files existed, so its first listing is
    // already the empty one; ask again the way a reader opening a folder does.
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.file_entries
                .remove(&(fixture.checkout.clone(), String::new()));
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_directory(repo, checkout, String::new(), window, cx);
        })
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .file_entries
            .get(&(fixture.checkout.clone(), String::new()))
            .is_some_and(|entries| !entries.is_empty())
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        window.render_frame(cx);
        // The control is laid over its row, so its bounds are the row's box:
        // every distance below is measured from the row's own left edge.
        let row = window.find("file-control:src").bounds();
        let slot = window.find("file-slot-src").bounds();
        assert_eq!(row.size.height, px(26.), "a row is one row tall");
        assert_eq!(
            slot.origin.x - row.origin.x,
            px(8.),
            "the fold slot starts at the panel's own gutter"
        );
    })
    .unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.expanded_files
                .insert((fixture.checkout.clone(), "src".into()));
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_directory(repo, checkout, "src".into(), window, cx);
        })
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .file_entries
            .get(&(fixture.checkout.clone(), "src".into()))
            .is_some_and(|entries| !entries.is_empty())
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        window.render_frame(cx);
        let row = window.find("file-control:src/main.rs").bounds();
        let slot = window.find("file-slot-src/main.rs").bounds();
        assert_eq!(
            slot.origin.x - row.origin.x,
            px(22.),
            "a level in is one 14px step"
        );
    })
    .unwrap();

    fixture.view.update(cx, |app, cx| {
        app.inspector_tab = InspectorTab::Changes;
        app.request_git_status(&fixture.checkout, cx);
        cx.notify();
    });
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_status
            .get(&fixture.checkout)
            .is_some_and(|status| !status.files.is_empty())
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        window.render_frame(cx);
        let group = window.find("change-group-/").bounds();
        let row = window.find("change-control-top.txt").bounds();
        let status = window.find("row-top.txt-status").bounds();
        let name = window.find("change-name-top.txt").bounds();
        let addition = window.find("row-top.txt-add").bounds();
        assert_eq!(row.size.height, px(26.), "a row is one row tall");
        assert_eq!(
            group.origin.x - row.origin.x,
            px(0.),
            "the heading sits on the panel's own edge"
        );
        assert_eq!(
            status.origin.x - row.origin.x,
            px(22.),
            "status starts one step in from the heading above it"
        );
        assert!(status.right() <= name.origin.x, "status precedes the name");
        assert!(name.right() <= addition.origin.x, "counts follow the name");
    })
    .unwrap();
    for status in ["R", "C", "T", "??"] {
        fixture.view.update(cx, |app, cx| {
            app.git_status.get_mut(&fixture.checkout).unwrap().files[0].status = status.into();
            app.open_diff = Some((fixture.checkout.clone(), None));
            cx.notify();
        });
        cx.update_window(fixture.handle, |_, window, cx| {
            window.render_frame(cx);
            // The chip is still there: a status the file tree's marker filter
            // drops must not leave the change list or the header blank.
            assert!(
                window.find("row-top.txt-status").visible(),
                "{status} keeps its chip in the change list"
            );
            assert!(
                window.find("diff-status-top.txt").visible(),
                "{status} keeps its letter in the diff header"
            );
        })
        .unwrap();
    }
    close(&fixture, cx);
}

/// A listed entry, for a tree built by hand rather than read from a filesystem.
fn entry_at(name: &str, _depth: usize, directory: bool) -> muster_core::files::FileEntry {
    muster_core::files::FileEntry {
        name: name.into(),
        path: name.into(),
        directory,
    }
}
