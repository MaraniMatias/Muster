use super::*;
use crate::app::sidebar_tests::{
    add_worktree, close, commit_all, open_git, read_status, GitFixture,
};
use crate::inspector::{opens_elsewhere, sticky_ancestors, tree_window, within, Ancestor, TreeRow};
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{point, px, TestAppContext};
use muster_core::files::FileEntry;
use std::rc::Rc;
use std::sync::Arc;

/// The reading of the document on screen, as its own words.
///
/// Which is what a picture that cannot be drawn falls back to, and what a
/// selection of the reading copies.
fn reading(fixture: &GitFixture, cx: &mut TestAppContext) -> String {
    fixture.view.read_with(cx, |app, cx| {
        app.document_view
            .as_ref()
            .and_then(|view| view.preview.clone())
            .map(|preview| {
                preview.read_with(cx, |preview, _| preview.rendered_text().as_str().to_owned())
            })
            .expect("a rendered Markdown document")
    })
}

/// Draw the window until the element with this id is on screen, running the
/// workers each frame waits on. Whether it ever appears is the answer.
fn draw_until(fixture: &GitFixture, cx: &mut TestAppContext, id: &str) -> bool {
    let id = id.to_owned();
    for _ in 0..32 {
        cx.run_until_parked();
        let drawn = cx
            .update_window(fixture.handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find(id.clone()).is_some()
            })
            .unwrap();
        if drawn {
            return true;
        }
    }
    false
}

#[gpui_kit::test]
async fn collapsed_folders_hide_cached_children_loading_and_errors(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, _| {
            let key = |path: &str| (fixture.checkout.clone(), path.to_owned());
            let entries = std::collections::HashMap::from([
                (key(""), vec![FileEntry { name: "folder".into(), path: "folder".into(), directory: true }]),
                (key("folder"), vec![FileEntry { name: "child".into(), path: "folder/child".into(), directory: false }]),
            ]);
            let expanded = std::collections::HashSet::new();
            for state in 0..3 {
                let loading = if state == 1 { std::collections::HashSet::from([key("folder")]) } else { Default::default() };
                let errors = if state == 2 { std::collections::HashMap::from([(key("folder"), "Failed listing".into())]) } else { Default::default() };
                let tree = crate::inspector::prepare_tree(&crate::inspector::TreeSource {
                    checkout: &fixture.checkout, entries: &entries, loading: &loading,
                    errors: &errors, expanded: &expanded, statuses: None,
                }, &mut app.measured_widths, window);
                assert_eq!(tree.rows.len(), 1, "collapsed folder, cached state {state}");
                assert!(matches!(&tree.rows[0], TreeRow::Entry { entry, depth: 0, .. } if entry.path == "folder"));
            }
        });
    }).unwrap();
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn membership_removal_prunes_status_and_index_even_during_a_sweep(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    fixture
        .view
        .update(cx, |app, cx| app.request_git_status(&fixture.checkout, cx));
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .status_index
            .contains_key(&fixture.checkout)
    })
    .await;
    fixture.view.update(cx, |app, cx| {
        let status = app.git_status[&fixture.checkout].clone();
        let index = app.status_index[&fixture.checkout].clone();
        app.git_status.insert("removed".into(), status);
        app.status_index.insert("removed".into(), index);
        app.git_status_request.insert("removed".into(), 42);
        app.git_status_errors
            .insert("removed".into(), "failed".into());
        app.diff_refresh_running = true;
        app.refresh_diff_totals(cx);
        assert!(!app.git_status.contains_key("removed"));
        assert!(!app.status_index.contains_key("removed"));
        assert!(!app.git_status_request.contains_key("removed"));
        assert!(!app.git_status_errors.contains_key("removed"));
        assert!(app.git_status.contains_key(&fixture.checkout));
        assert!(app.status_index.contains_key(&fixture.checkout));
        app.navigation.repos.clear();
        app.refresh_diff_totals(cx);
        assert!(app.git_status.is_empty());
        assert!(app.status_index.is_empty());
    });
    close(&fixture, cx);
}

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
///
/// The entries are collected first and the rows borrow them, which is how the tree
/// itself is walked: a tree of ten thousand rows must not own ten thousand copies of
/// its listings to be asked what is stacked at some row of it.
fn legacy_tree<'a>(files: usize, entries: &'a mut Vec<FileEntry>) -> Vec<TreeRow<'a>> {
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
    entries.push(entry_at("src", 0, true));
    entries.push(entry_at("nested", 1, true));
    for index in 0..files {
        entries.push(entry_at(&format!("f{index}"), 2, false));
    }
    let entries: &'a [FileEntry] = entries;
    let mut rows = vec![
        TreeRow::Entry {
            entry: &entries[0],
            depth: 0,
            ancestors: vec![chain[0].clone()].into(),
        },
        TreeRow::Entry {
            entry: &entries[1],
            depth: 1,
            ancestors: chain.clone(),
        },
    ];
    rows.extend(entries[2..].iter().map(|entry| TreeRow::Entry {
        entry,
        depth: 2,
        ancestors: chain.clone(),
    }));
    rows
}

/// What the stack hides is measured from the row on screen on every pass, and not from the
/// row the last pass reached.
///
/// The defect this pins: measured from the row the last pass reached, the walk keeps
/// stepping down the tree and answers with the chain of whatever row it stops on — which
/// for a tree that ends in a file at its root, as most listings do, is no folders at all.
/// A tree deep enough to need a stack is walked all the way to that last row, so the stack
/// it needs is the one that never appeared.
#[test]
fn a_narrowing_chain_is_stacked_from_the_row_on_screen() {
    let mut entries = Vec::new();
    // The shape of a listing: a folder open at the root, one open inside it, files
    // under that, and then a file at the root again — which carries no folder.
    let root: Rc<[Ancestor]> = vec![Ancestor {
        path: "a".into(),
        name: "a".into(),
        depth: 0,
        index: 0,
    }]
    .into();
    let nested: Rc<[Ancestor]> = vec![
        Ancestor {
            path: "a".into(),
            name: "a".into(),
            depth: 0,
            index: 0,
        },
        Ancestor {
            path: "a/b".into(),
            name: "b".into(),
            depth: 1,
            index: 1,
        },
    ]
    .into();
    entries.push(entry_at("a", 0, true));
    entries.push(entry_at("b", 1, true));
    for index in 0..40 {
        entries.push(entry_at(&format!("f{index}"), 2, false));
    }
    entries.push(entry_at("loose.txt", 0, false));
    let entries: &[FileEntry] = &entries;
    let mut rows = vec![
        TreeRow::Entry {
            entry: &entries[0],
            depth: 0,
            ancestors: root.clone(),
        },
        TreeRow::Entry {
            entry: &entries[1],
            depth: 1,
            ancestors: nested.clone(),
        },
    ];
    rows.extend(entries[2..42].iter().map(|entry| TreeRow::Entry {
        entry,
        depth: 2,
        ancestors: nested.clone(),
    }));
    rows.push(TreeRow::Entry {
        entry: &entries[42],
        depth: 0,
        ancestors: Rc::from(Vec::new()),
    });
    // Every row under `b` names the two folders it is in: the answer is the chain
    // of the row on screen. The last few rows of the tree are left out because a
    // row whose own step lands on the last row answers with that row's chain —
    // Legacy reads `stacks[min(top, rows.length) - 1]`, and its arithmetic is
    // being kept whole.
    for top in 3..39 {
        assert_eq!(
            names(&rows, top, 8),
            ["a", "b"],
            "the stack at row {top} is the chain of the row on screen"
        );
    }
}

fn names(rows: &[TreeRow<'_>], top: usize, stack: usize) -> Vec<String> {
    sticky_ancestors(rows, top, stack)
        .into_iter()
        .map(|ancestor| ancestor.name)
        .collect()
}

/// The rule is Legacy's, so it is pinned against Legacy's own cases.
#[test]
fn the_folder_stack_is_the_rule_legacy_pins() {
    let mut entries = Vec::new();
    let tree = legacy_tree(80, &mut entries);
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
    let mut narrow_entries = Vec::new();
    let narrow = legacy_tree(2, &mut narrow_entries);
    let mut wide_entries = Vec::new();
    let wide = legacy_tree(10_000, &mut wide_entries);
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
    let entries = [entry_at("src", 0, true)];
    let rows = vec![
        TreeRow::Entry {
            entry: &entries[0],
            depth: 0,
            ancestors: chain.clone(),
        },
        TreeRow::Note {
            path: "src",
            message: "Loading folder…",
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
            window.find("file-stack-fold:src/nested").label(),
            Some("Collapse nested"),
            "a stacked folder closes from the stack too"
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
        // A stacked folder is the tree's own row, columns and all: its name
        // starting where the tree starts that folder's name, one 14px step in
        // for its depth. A stack drawn to any other shape is a legend above the
        // tree rather than the tree's rows carried.
        assert_eq!(
            window.find("file-stack-name-src/nested").bounds().origin.x - stack.bounds().origin.x,
            px(8. + 14. + 16. + 7. + 14. + 7.),
            "its name sits on the same column the tree draws it on"
        );
    })
    .unwrap();

    // Clicking one brings the tree back to where that folder's own row is. It is the
    // name that is clicked, not a separate button on the right of the row: the name is
    // what Legacy makes the way back, and it is the only part of a stacked row a
    // pointer is ever resting on.
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("file-stack-name-src/nested", cx);
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

/// A stacked folder answers for itself, and not for the row it is drawn over.
///
/// The defect this pins: the stack was drawn over the rows it names and had no hitbox
/// of its own, so a click on a stacked folder's name went through it to the tree row
/// hidden underneath. Measured, it opened `src/nested/f12.txt` — a file whose own row
/// sat at y = -207, above the top of the tree and nowhere near the pointer.
#[gpui_kit::test]
async fn a_stacked_folder_keeps_its_own_click(cx: &mut TestAppContext) {
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
    cx.update(|cx| {
        fixture
            .view
            .read(cx)
            .files_scroll
            .set_offset(gpui_kit::point(px(0.), px(-30. * 26.)))
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        // The row the stack is drawn over, and where it sits: a click on the
        // stacked name lands on this row's box, which is the whole defect.
        let stack = window.find("file-stack-name-src/nested").bounds();
        let hidden = window.find("file-slot-src/nested/f12.txt").bounds();
        assert!(
            stack.origin.y < hidden.bottom() && stack.bottom() > hidden.origin.y,
            "the stacked name is drawn over a real tree row"
        );
        window.click("file-stack-name-src/nested", cx);
    })
    .unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.read(|cx| {
        let app = fixture.view.read(cx);
        assert!(
            app.selected_file.is_empty(),
            "a stacked folder is not a file row: the row underneath it was never clicked"
        );
        assert_eq!(
            app.files_scroll.offset().y,
            px(0.),
            "and the click brought the folder to the top of the tree instead"
        );
    });

    // And its fold is the other gesture: it closes the folder, and it does not
    // also scroll, because a stacked row that answered both would close the
    // folder and then bring it straight back into view.
    cx.update(|cx| {
        fixture
            .view
            .read(cx)
            .files_scroll
            .set_offset(gpui_kit::point(px(0.), px(-30. * 26.)))
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        window.click("file-stack-fold:src", cx);
    })
    .unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.read(|cx| {
        let app = fixture.view.read(cx);
        assert!(
            !app.expanded_files.contains(&key("src")),
            "a stacked folder closes from the stack"
        );
        assert!(
            !app.file_entries.contains_key(&key("src/nested")),
            "and a closed folder forgets what was under it"
        );
    });
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

/// A row's marker is the answer the window indexed when it read that path, and the
/// answer after that is the one the row wears.
///
/// The index the tree reads is built once per status read rather than once per
/// render, so what this pins is the other half of that: a second read has to
/// replace it, and a path the second read no longer mentions has to lose the
/// marker rather than keep the one it was given.
#[gpui_kit::test]
async fn a_second_status_read_replaces_what_the_rows_wear(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    for name in ["one.txt", "two.txt"] {
        std::fs::write(fixture.root.join(name), "tracked\n").unwrap();
    }
    commit_all(&fixture.root, "tracked");
    let listing = |cx: &mut TestAppContext| {
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
    };
    // The window opened before these files existed, so its first listing is the one
    // it took then; ask again the way a reader opening a folder does.
    listing(cx);
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .file_entries
            .get(&(fixture.checkout.clone(), String::new()))
            .is_some_and(|entries| entries.len() == 2)
    })
    .await;

    std::fs::write(fixture.root.join("one.txt"), "changed\n").unwrap();
    fixture
        .view
        .update(cx, |app, cx| app.request_git_status(&fixture.checkout, cx));
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .status_index
            .get(&fixture.checkout)
            .is_some_and(|index| index.contains_key("one.txt"))
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(
            window.find("row-one.txt-status").visible(),
            "the changed file wears its marker"
        );
        assert!(
            window.try_find("row-two.txt-status").is_none(),
            "a file Git said nothing about wears none"
        );
    })
    .unwrap();

    // The other way round: the answer this read gives is not the answer the last
    // one gave, for either file.
    std::fs::write(fixture.root.join("one.txt"), "tracked\n").unwrap();
    std::fs::write(fixture.root.join("two.txt"), "changed\n").unwrap();
    fixture
        .view
        .update(cx, |app, cx| app.request_git_status(&fixture.checkout, cx));
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .status_index
            .get(&fixture.checkout)
            .is_some_and(|index| index.contains_key("two.txt") && !index.contains_key("one.txt"))
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(
            window.find("row-two.txt-status").visible(),
            "the file this read changed wears its marker"
        );
        assert!(
            window.try_find("row-one.txt-status").is_none(),
            "a file this read no longer mentions has lost the marker the last one gave it"
        );
        assert!(
            window.find("row-two.txt-add").visible(),
            "and its counts came with it"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// A checkout's failures are keyed by `(checkout, path)`, which is what keeps one
/// checkout's note out of another checkout's tree.
///
/// The tree reads the window's own map rather than a copy narrowed to this
/// checkout, so this is the check on that: the whole `(checkout, path)` is the key,
/// and a note that belongs to another checkout must not appear here.
#[gpui_kit::test]
async fn a_failure_in_another_checkout_is_not_a_note_in_this_tree(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::create_dir_all(fixture.root.join("docs")).unwrap();
    fixture.view.update(cx, |app, cx| {
        app.navigation.active_checkout = Some(fixture.checkout.clone());
        app.file_entries.insert(
            (fixture.checkout.clone(), String::new()),
            vec![FileEntry {
                name: "docs".into(),
                path: "docs".into(),
                directory: true,
            }],
        );
        app.expanded_files
            .insert((fixture.checkout.clone(), "docs".into()));
        // The same directory, in another checkout, failed to be read.
        app.file_errors.insert(
            ("another-checkout".to_owned(), "docs".into()),
            "Permission denied (os error 13)".into(),
        );
        cx.notify();
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(
            window.find("file-control:docs").visible(),
            "the folder is a row"
        );
        assert!(
            window.try_find("file-note-docs").is_none(),
            "another checkout's failure is not a note in this tree"
        );
        assert!(
            window.try_find("file-note-docs-text").is_none(),
            "and it is not a note drawn anywhere else either"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// A row wears the answer read for its own checkout, and a checkout read with
/// nothing changed wears no answer at all.
///
/// Two checkouts of one repository hold the same path, so an index keyed by path
/// rather than by `(checkout, path)` would give them the same row and the same
/// marker: this is the check on which of the two it is.
#[gpui_kit::test]
async fn a_status_read_for_one_checkout_does_not_reach_another_checkouts_rows(
    cx: &mut TestAppContext,
) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("shared.txt"), "tracked\n").unwrap();
    commit_all(&fixture.root, "tracked");
    let (worktree, _path) = add_worktree(&fixture, cx);
    // Changed in the checkout the window opened with, and nowhere else.
    std::fs::write(fixture.root.join("shared.txt"), "changed\n").unwrap();
    for checkout in [fixture.checkout.clone(), worktree.clone()] {
        fixture
            .view
            .update(cx, |app, cx| app.request_git_status(&checkout, cx));
        cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
            fixture.view.read(cx).status_index.contains_key(&checkout)
        })
        .await;
    }
    // The window opened before this file existed, so its first listing is the one
    // it took then; ask again the way a reader opening a folder does.
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
            .is_some_and(|entries| entries.iter().any(|entry| entry.path == "shared.txt"))
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(
            window.find("row-shared.txt-status").visible(),
            "the checkout that changed this path wears its marker"
        );
    })
    .unwrap();

    // The same path in the other checkout, read after this one, with nothing
    // changed in it.
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("select-checkout-{worktree}"), cx);
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .file_entries
            .get(&(
                fixture
                    .view
                    .read(cx)
                    .navigation
                    .active_checkout
                    .clone()
                    .unwrap(),
                String::new(),
            ))
            .is_some_and(|entries| entries.iter().any(|entry| entry.path == "shared.txt"))
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(
            window.find("file-control:shared.txt").visible(),
            "the other checkout's tree is on screen"
        );
        assert!(
            window.try_find("row-shared.txt-status").is_none(),
            "and the first checkout's marker is not on its row"
        );
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

#[gpui_kit::test]
async fn clearing_review_exports_requires_confirmation(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("change.txt"), "review\n").unwrap();
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

    fixture.view.update(cx, |app, cx| {
        app.navigation.review_storage = muster_core::review::ReviewStorage::Default;
        app.open_review_clear_confirmation(cx);
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("confirm-clear-review-exports").is_some());
        window.click("cancel-clear-review-exports", cx);
        window.render_frame(cx);
        assert!(window.try_find("confirm-clear-review-exports").is_none());
    })
    .unwrap();
    close(&fixture, cx);
}

/// Forgetting delivered reviews is a person's decision, so it is asked for before it
/// happens and it is not on offer where there is nothing to forget.
#[gpui_kit::test]
async fn forgetting_delivered_reviews_asks_first_and_says_what_it_costs(cx: &mut TestAppContext) {
    let fixture = open_git(cx);

    // Reachable with no diff open: the store is one file for the whole workspace, and a
    // person who has to open a diff to get its delivered reviews back is a person whose
    // store stays full.
    fixture
        .view
        .update(cx, |app, cx| app.open_opencode_forget_confirmation(cx));
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("confirm-forget-opencode-reviews").is_some(),
            "the decision is asked for without a diff open"
        );
        assert!(
            window.try_find("confirm-reset-opencode-reviews").is_some(),
            "the last-resort duplicate-scope reset is available"
        );
        window.click("cancel-forget-opencode-reviews", cx);
        window.render_frame(cx);
        assert!(
            window.try_find("confirm-forget-opencode-reviews").is_none(),
            "and cancelling removes it"
        );
    })
    .unwrap();

    std::fs::write(fixture.root.join("change.txt"), "review\n").unwrap();
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

    fixture.view.update(cx, |app, _| {
        app.review.opencode_rounds = vec![muster_core::opencode::ReviewRound {
            id: "r1".into(),
            checkout_id: fixture.checkout.clone(),
            session_id: "ses_review".into(),
            status: muster_core::opencode::RoundStatus::Dispatched,
            marker: "muster-review:r1".into(),
            prompt: "# Code Review\n".into(),
            note_ids: vec!["note-1".into()],
            claim: String::new(),
            notes: Vec::new(),
            created_at: "1".into(),
            updated_at: "1".into(),
        }];
    });

    // The route a menu item calls, with a diff open: the decision itself is a dialog
    // rather than an immediate deletion.
    fixture.view.update(cx, |app, cx| {
        app.open_opencode_forget_confirmation(cx);
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("confirm-forget-opencode-reviews").is_some(),
            "the decision is asked for"
        );
        window.click("cancel-forget-opencode-reviews", cx);
        window.render_frame(cx);
        assert!(
            window.try_find("confirm-forget-opencode-reviews").is_none(),
            "and cancelling removes it"
        );
    })
    .unwrap();
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

/// The open document draws its caret.
///
/// The toolkit's editor draws its caret only once it has been told it is
/// focused, and that answer comes with a frame's report of what holds the
/// keyboard. An editor built and focused in the same frame never hears about
/// it, and a document with no caret in it reads as a picture of a file rather
/// than as something a person can type in.
#[gpui_kit::test]
async fn the_open_document_draws_its_caret(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("main.rs"), "fn main() {}\n").unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "main.rs".into(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture.view.read(cx).open_document.is_some()
    })
    .await;
    // The caret wears the toolkit's own caret colour, and it is a narrow bar:
    // nothing else in this window is painted in that colour.
    let caret = cx.read(|cx| gpui_kit::component::Theme::global(cx).caret);
    let mut drawn = false;
    for _ in 0..8 {
        cx.update_window(fixture.handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
        drawn = cx
            .update_window(fixture.handle, |_, window, _| {
                window.painted_quads().iter().any(|quad| {
                    let Some(color) = quad.background.as_solid() else {
                        return false;
                    };
                    color.h == caret.h
                        && color.s == caret.s
                        && color.l == caret.l
                        && color.a == caret.a
                        && quad.bounds.size.height > quad.bounds.size.width
                })
            })
            .unwrap();
        if drawn {
            break;
        }
    }
    assert!(drawn, "the open document draws its caret");
    close(&fixture, cx);
}

/// The menu is one search field over one list, and `Auto` is its first row.
///
/// The search is drawn above the rows because it is the first thing a person
/// looks for, and `Auto` is a row rather than a heading because it is one of
/// the choices: a menu that pins two rows above its own search asks to be read
/// as two menus. The measurement is geometry, never a glyph advance: a row's
/// text is rounded to whole pixels while its shaped advance is not.
#[gpui_kit::test]
async fn the_grammar_menu_is_a_search_over_one_list(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let path = "notes-without-an-extension";
    std::fs::write(fixture.root.join(path), "name: muster\ncount: 42\n").unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, path.into(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, preview)| preview.path == path)
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("document-grammar", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        let search = window.find("document-grammar-search");
        let auto = window.find("document-grammar-auto");
        let plain = window.find("document-grammar-plaintext");
        assert_eq!(
            window.find("document-grammar-auto").label(),
            Some("Auto · no highlighting, selected"),
            "the first row is Auto and it is what this file wears"
        );
        assert_eq!(
            window.find("document-grammar-plaintext").label(),
            Some("Plain text"),
            "Plain text is the second row, not a row of its own above the search"
        );
        assert_eq!(
            (auto.bounds().size.height, plain.bounds().size.height),
            (px(28.), px(28.)),
            "every row is the height Legacy's menu row is"
        );
        assert_eq!(
            auto.bounds().origin.x,
            plain.bounds().origin.x,
            "rows share one column"
        );
        assert_eq!(
            window.find("document-grammar-plaintext").selected(),
            Some(false),
            "Plain text is not the choice this file wears"
        );
        // The search sits above the rows: its own field, not a row of the list.
        assert!(
            search.bounds().bottom() <= auto.bounds().origin.y,
            "the search is above the list it filters"
        );
    })
    .unwrap();

    // The keyboard walks the list, and the row it lands on is the one it wears.
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("document-grammar-search", cx);
        window.input("yaml", cx);
        assert_eq!(
            window.find("document-grammar-yaml").selected(),
            Some(true),
            "the filtered keyboard target is visibly selected"
        );
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(
            app.document_language_overrides
                .values()
                .next()
                .map(String::as_str),
            Some("yaml"),
            "the filtered keyboard target is the language that Enter confirms"
        );
    });
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn a_reader_can_force_the_grammar_a_file_wears(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let path = "notes-without-an-extension";
    let source = "name: muster\ncount: 42\nenabled: true\n";
    std::fs::write(fixture.root.join(path), source).unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, path.into(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, preview)| preview.path == path)
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("document-grammar").label(),
            Some("Choose syntax highlighting language, currently Auto (no highlighting)")
        );
        window.click("document-grammar", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(fixture.handle, |_, window, cx| window.input("yaml", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();

    let state = fixture.view.read_with(cx, |app, _| {
        app.document_view.as_ref().unwrap().state.clone()
    });
    assert_eq!(
        state.read_with(cx, |state, _| state.language_name().to_string()),
        "yaml"
    );
    cx.update_window(fixture.handle, |_, window, cx| {
        assert!(
            state.read(cx).focus_handle(cx).is_focused(window),
            "choosing a grammar returns focus to the editor"
        );
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(
            app.document_language_overrides
                .values()
                .next()
                .map(String::as_str),
            Some("yaml")
        );
        assert!(
            !app.document_grammar_open,
            "choosing a grammar closes its menu"
        );
    });
    let (yaml_colors, plain_colors) = cx.update(|cx| {
        let text = state.read(cx).text().clone();
        let theme = gpui_kit::component::Theme::global(cx);
        let colors = |language: &str| {
            let mut highlighter =
                gpui_kit::component::highlighter::SyntaxHighlighter::new(language);
            highlighter.update(None, &text, None);
            highlighter
                .styles(&(0..source.len()), theme.highlight_theme.as_ref())
                .iter()
                .filter_map(|(_, style)| style.color)
                .collect::<Vec<_>>()
        };
        (colors("yaml"), colors("plaintext"))
    });
    assert!(yaml_colors
        .iter()
        .any(|color| Some(color) != yaml_colors.first()));
    assert_ne!(
        yaml_colors, plain_colors,
        "forcing YAML changes token colours"
    );

    // A choice follows the file through another document instead of staying on the pane.
    let other_path = "other.txt";
    std::fs::write(fixture.root.join(other_path), "plain\n").unwrap();
    for selected_path in [other_path, path] {
        cx.update_window(fixture.handle, |_, window, cx| {
            fixture.view.update(cx, |app, cx| {
                let repo = app.navigation.repos[0].clone();
                let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
                app.request_document(repo, checkout, selected_path.into(), window, cx);
            });
        })
        .unwrap();
        cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
            fixture
                .view
                .read(cx)
                .open_document
                .as_ref()
                .is_some_and(|(_, preview)| preview.path == selected_path)
        })
        .await;
    }
    let restored = fixture.view.read_with(cx, |app, _| {
        app.document_view.as_ref().unwrap().state.clone()
    });
    assert_eq!(
        restored.read_with(cx, |state, _| state.language_name().to_string()),
        "yaml",
        "the file keeps its forced grammar when reopened"
    );

    // The search filters the whole list, Plain text among it: a row nobody can
    // reach is a row that does not exist, so it is typed for.
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("document-grammar", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(fixture.handle, |_, window, cx| window.input("plain", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("document-grammar-plaintext").label(),
            Some("Plain text"),
            "the search leaves the row it names"
        );
        window.click("document-grammar-plaintext", cx);
    })
    .unwrap();
    let plain_text = fixture.view.read_with(cx, |app, _| {
        assert_eq!(
            app.document_language_overrides
                .values()
                .next()
                .map(String::as_str),
            Some("plaintext")
        );
        app.document_view.as_ref().unwrap().state.clone()
    });
    assert_eq!(
        plain_text.read_with(cx, |state, _| state.language_name().to_string()),
        "plaintext"
    );

    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("document-grammar", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("document-grammar-auto", cx);
    })
    .unwrap();
    let restored = fixture.view.read_with(cx, |app, _| {
        assert!(app.document_language_overrides.is_empty());
        app.document_view.as_ref().unwrap().state.clone()
    });
    assert_eq!(
        restored.read_with(cx, |state, _| state.language_name().to_string()),
        "plaintext",
        "Auto restores the extension's choice"
    );
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

/// A Markdown document is read as a document, and its source is one click away.
///
/// Legacy rendered a `.md` file rather than showing it as code, with a rendered
/// reading and its source behind a control. The rendered reading is the toolkit's
/// own text view — which is what paints its fences with the `muster-content`
/// tokens this window installed — and both readings are one document's two views,
/// not two reads of the file.
#[gpui_kit::test]
async fn a_markdown_document_opens_rendered_and_reads_as_source_on_request(
    cx: &mut TestAppContext,
) {
    let fixture = open_git(cx);
    let source =
        "---\ntitle: Muster\n---\n\n# Heading\n\n- one\n- two\n\n```rust\nlet x = 1;\n```\n";
    std::fs::write(fixture.root.join("README.md"), source).unwrap();
    fixture.view.update(cx, |app, _| {
        app.navigation.active_checkout = Some(fixture.checkout.clone());
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "README.md".into(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, preview)| preview.path == "README.md")
    })
    .await;

    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(
            window.try_find("document-markdown").is_some(),
            "a Markdown file is read as a document"
        );
        assert!(
            window.try_find("document-grammar").is_none(),
            "rendered Markdown has no source grammar selector"
        );
        assert_eq!(
            window.find("document-toggle-reading").label(),
            Some("Read this document as its source"),
            "and the header offers the reading it is not on"
        );
        // The source is still there, whole, for the other reading.
        let state = fixture
            .view
            .read(cx)
            .document_view
            .as_ref()
            .expect("a view")
            .state
            .clone();
        assert_eq!(state.read(cx).text().to_string(), source);
    })
    .unwrap();

    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("document-toggle-reading", cx);
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update_window(fixture.handle, |_, window, _cx| {
        assert!(
            window.try_find("document-markdown").is_none(),
            "the source reading is the editor"
        );
        assert!(
            window.try_find("document-toggle-reading").is_some(),
            "and the header still offers the other reading"
        );
        assert!(
            window.try_find("document-grammar").is_some(),
            "the source reading offers a grammar selector"
        );
    })
    .unwrap();

    // And back again: the two readings are one document, not a re-read.
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("document-toggle-reading", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.try_find("document-markdown").is_some());
    })
    .unwrap();
    close(&fixture, cx);
}

/// The keyboard belongs to the reading that is on screen.
///
/// The defect this pins is the direction, not the missing feature. A Markdown
/// document opened with the editor focused while the pane drew its reading, and
/// the header's toggle focused that same editor on the way *back* to the reading
/// — the one direction where the editor is not mounted. So the reading was drawn
/// with nothing focused and its source needed a click in its body before a
/// keystroke went anywhere.
///
/// What is asserted is where the two presses land, not what a selection looks
/// like: `secondary-a` then `secondary-c` are dispatched to whichever handle the
/// window holds, so a clipboard that changed is a keystroke that arrived. The
/// blurred case at the end is the control — the same two presses with no focus
/// leave the clipboard alone, which is what makes the two above them mean
/// something.
#[gpui_kit::test]
async fn the_keyboard_follows_the_reading_that_is_on_screen(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let source = "# Heading\n\n- one\n- two\n";
    std::fs::write(fixture.root.join("README.md"), source).unwrap();
    fixture.view.update(cx, |app, _| {
        app.navigation.active_checkout = Some(fixture.checkout.clone());
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "README.md".into(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, preview)| preview.path == "README.md")
    })
    .await;
    let (state, preview) = fixture.view.read_with(cx, |app, _| {
        let view = app.document_view.as_ref().expect("a view");
        (view.state.clone(), view.preview.clone().expect("a reading"))
    });
    let editor_id = state.entity_id();
    let reading_id = preview.entity_id();

    // It opens as a document, so the reading is what holds the keyboard and the
    // editor behind it holds nothing. The keyboard is asked for at the end of
    // the frame that opened the document, so it is read in the next one.
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        assert!(
            preview.read(cx).focus_handle().is_focused(window),
            "a Markdown document opened with its reading on screen and the keyboard somewhere else"
        );
        assert!(
            !state.read(cx).focus_handle(cx).is_focused(window),
            "the editor behind the rendered reading kept the keyboard"
        );
    })
    .unwrap();

    // To the source: the keyboard follows it there, with no click in its body
    // first, and the reading that went away lets it go.
    let copied = cx
        .update_window(fixture.handle, |_, window, cx| {
            window.click("document-toggle-reading", cx);
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(
                state.read(cx).focus_handle(cx).is_focused(window),
                "the source is on screen and the keyboard is not"
            );
            assert!(
                !preview.read(cx).focus_handle().is_focused(window),
                "the reading that went away kept the keyboard"
            );
            window.press("secondary-a", cx);
            let selected = state.read(cx).selected_text().to_string();
            window.press("secondary-c", cx);
            selected
        })
        .unwrap();
    assert_eq!(
        copied, source,
        "selecting and copying the source needed a click in the editor first"
    );
    assert_eq!(
        cx.read_from_clipboard().map(|item| item.text()),
        Some(Some(source.to_owned())),
        "the source reading did not copy itself"
    );

    // And back: the reading takes the keyboard again and the editor gives it up,
    // and the same two presses answer from the reading.
    let copied = cx
        .update_window(fixture.handle, |_, window, cx| {
            window.click("document-toggle-reading", cx);
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(
                preview.read(cx).focus_handle().is_focused(window),
                "the reading is on screen and the keyboard is not"
            );
            assert!(
                !state.read(cx).focus_handle(cx).is_focused(window),
                "the hidden editor kept the keyboard behind the reading"
            );
            cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                "nothing was copied".to_owned(),
            ));
            window.press("secondary-a", cx);
            window.press("secondary-c", cx);
            cx.read_from_clipboard().map(|item| item.text())
        })
        .unwrap();
    assert_ne!(
        copied,
        Some(Some("nothing was copied".to_owned())),
        "the reading on screen did not answer the keyboard"
    );

    // Two readings, one document: neither entity was rebuilt to change focus, and
    // the text behind the reading is the text it was opened with.
    assert_eq!(state.entity_id(), editor_id, "the editor was replaced");
    assert_eq!(preview.entity_id(), reading_id, "the reading was replaced");
    cx.read(|cx| {
        assert_eq!(
            state.read(cx).text().to_string(),
            source,
            "the document changed while the keyboard moved between its readings"
        );
    });

    // The control: with nothing focused the same two presses copy nothing, which
    // is what makes the two copies above evidence of a focused handle rather
    // than of a window that copies whatever is asked.
    cx.update_window(fixture.handle, |_, window, cx| {
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
            "nothing was copied".to_owned(),
        ));
        window.blur(cx);
        window.render_frame(cx);
        window.press("secondary-a", cx);
        window.press("secondary-c", cx);
        assert_eq!(
            cx.read_from_clipboard().map(|item| item.text()),
            Some(Some("nothing was copied".to_owned())),
            "a copy with no focused handle on screen"
        );
    })
    .unwrap();

    // A file with no second reading is its editor, and the editor is what the
    // keyboard gets.
    std::fs::write(fixture.root.join("note.txt"), source).unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "note.txt".into(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, preview)| preview.path == "note.txt")
    })
    .await;
    let plain = fixture.view.read_with(cx, |app, _| {
        let view = app.document_view.as_ref().expect("a view");
        assert!(
            view.preview.is_none(),
            "a file that is not Markdown grew a second reading"
        );
        view.state.clone()
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        assert!(
            plain.read(cx).focus_handle(cx).is_focused(window),
            "a document with only a source did not take the keyboard"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// One real pixel, so a picture is bytes the toolkit's asset system can decode
/// rather than a file that merely exists.
const ONE_PIXEL_PNG: &[u8] = b"\
\x89PNG\r\n\x1a\n\
\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89\
\x00\x00\x00\rIDAT\x78\x9c\x63\xf8\xcf\xc0\xf0\x1f\x00\x05\x00\x01\xff\x89\x99\x3d\x1d\
\x00\x00\x00\x00IEND\xaeB\x60\x82";

/// What a document cannot draw is said in a box of its own.
///
/// A video, an audio file and a file are three words standing where a picture
/// stands, so they stand off the text around them by the same air rather than
/// running into it: the toolkit measures an inline element as a root box, so the
/// air is inside the box. How much air is a drawing decision and is not pinned
/// here; that the words are a box is what makes the two answers one answer.
#[gpui_kit::test]
async fn what_a_document_cannot_draw_is_said_in_a_box_of_its_own(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let source =
        "![The demo](assets/demo.mp4)\n\n![The notes](notes.pdf)\n\n![The tone](bell.mp3)\n";
    std::fs::write(fixture.root.join("README.md"), source).unwrap();
    fixture.view.update(cx, |app, _| {
        app.navigation.active_checkout = Some(fixture.checkout.clone());
    });
    open(cx, &fixture, "README.md").await;

    for said in ["The demo", "The notes", "The tone"] {
        let at = source.find(&format!("![{said}]")).unwrap();
        // Said, and drawn by this module rather than by the toolkit's own answer
        // for an inline object it cannot draw.
        assert!(
            draw_until(&fixture, cx, &format!("document-media-said-{at}")),
            "{said} is not said in a box"
        );
        assert!(
            !draw_until(&fixture, cx, &format!("document-media-{at}")),
            "{said} is drawn as a picture that is there"
        );
    }
    // The words are still the words: the box changed where they stand, not what
    // they say, and what they say is what a selection of the reading copies.
    let reading = reading(&fixture, cx);
    for said in ["The demo", "The notes", "The tone"] {
        assert!(reading.contains(said), "{said} is not said: {reading}");
    }
    close(&fixture, cx);
}

/// A one-pixel SVG: the one picture a document names that is markup rather than
/// bytes with a header, so the format is decided by the name and nothing else.
const ONE_PIXEL_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><rect width="1" height="1"/></svg>"#;

/// A document draws SVG beside every other picture.
///
/// The same path a PNG takes, and worth pinning because it is the one format
/// this window cannot decide from the bytes: an SVG has no header, so a name
/// that is not `.svg` for a file that is SVG is not a picture at all, and a name
/// that is `.svg` for a file that is not is still tried and still refused.
#[gpui_kit::test]
async fn a_rendered_document_draws_an_svg_beside_every_other_picture(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("diagram.png"), ONE_PIXEL_PNG).unwrap();
    std::fs::write(fixture.root.join("logo.svg"), ONE_PIXEL_SVG).unwrap();
    let source = "![The diagram](diagram.png)\n\n![The logo](logo.svg)\n";
    std::fs::write(fixture.root.join("README.md"), source).unwrap();
    fixture.view.update(cx, |app, _| {
        app.navigation.active_checkout = Some(fixture.checkout.clone());
    });
    open(cx, &fixture, "README.md").await;

    for (said, name) in [("The diagram", "diagram.png"), ("The logo", "logo.svg")] {
        let id = format!(
            "document-media-{}",
            source.find(&format!("![{said}]")).unwrap()
        );
        assert!(
            draw_until(&fixture, cx, &id),
            "{said} is not drawn as a picture"
        );
        assert!(
            fixture.view.read_with(cx, |app, _| {
                app.document_view
                    .as_ref()
                    .is_some_and(|view| view.media.image(name).is_some())
            }),
            "{said} is drawn without the bytes a read brought back"
        );
    }
    // What the toolkit made of the markup, which is the claim an element id
    // cannot make on its own: the SVG was rasterized rather than laid out as
    // something it cannot draw.
    cx.run_until_parked();
    cx.update_window(fixture.handle, |_, window, cx| {
        let svg = fixture.view.read_with(cx, |app, _| {
            app.document_view
                .as_ref()
                .and_then(|view| view.media.image("logo.svg"))
                .expect("the worker read the SVG into this document")
        });
        let decoded = svg
            .get_render_image(window, cx)
            .expect("the toolkit rasterized the SVG the read brought back");
        assert!(
            decoded.as_bytes(0).is_some_and(|pixels| !pixels.is_empty()),
            "the SVG was laid out rather than rasterized"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// A rendered document draws the picture it names and says everything else.
///
/// The toolkit's own image path resolves a URL against the process rather than
/// against the document, so a README's picture arrived as an empty box; this is
/// the reading that answers for it. The two lines with no alt text are the half
/// that had nothing at all: a video this window cannot play, and a `../` that
/// leaves the checkout, each of which has to say what the document put there
/// rather than leave a blank where it was.
///
/// The picture is proved by what the toolkit decoded out of the bytes the read
/// brought back. An element id only says the element was built, which is exactly
/// what a picture that never arrived would also produce.
#[gpui_kit::test]
async fn a_rendered_document_draws_its_picture_and_says_what_it_cannot(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("diagram.png"), ONE_PIXEL_PNG).unwrap();
    let source = "# Pictures\n\n![The diagram](diagram.png)\n\n![](assets/demo.mp4)\n\n![](../outside.png)\n";
    std::fs::write(fixture.root.join("README.md"), source).unwrap();
    fixture.view.update(cx, |app, _| {
        app.navigation.active_checkout = Some(fixture.checkout.clone());
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "README.md".into(), window, cx);
        });
    })
    .unwrap();
    // The window is driven to quiet without being drawn. Opening a document also
    // asks Git about the file it opened, so a frame drawn while that read is still
    // in flight is not the first frame — and the picture could have come back on
    // it. Parked first, the frame below is the first one there is.
    cx.run_until_parked();
    assert!(
        fixture.view.read_with(cx, |app, _| app
            .open_document
            .as_ref()
            .is_some_and(|(_, preview)| preview.path == "README.md")),
        "the document was not read"
    );

    // The picture is drawn from bytes the window holds, and those bytes arrive
    // from a worker rather than from the thread that draws.
    //
    // Which frame the picture turns up on is not something to pin: opening a
    // document also asks Git about the file it opened, so the scheduler may draw
    // a frame and run a read before this test asks it to. What holds either way
    // is the shape of the answer — the element and the store agree, so a picture
    // on screen is one this window read, and it was read once.
    let picture = format!("document-media-{}", source.find("![The diagram]").unwrap());
    assert!(
        draw_until(&fixture, cx, &picture),
        "a picture inside the checkout is drawn where the document names it"
    );
    assert!(
        fixture.view.read_with(cx, |app, _| app
            .document_view
            .as_ref()
            .is_some_and(|view| view.media.image("diagram.png").is_some())),
        "the picture on screen is bytes this window holds, not bytes a frame went and got"
    );
    assert!(
        reading(&fixture, cx).contains("The diagram"),
        "the line stands in the picture's place, and is what it fell back to"
    );
    assert_eq!(
        fixture.view.read_with(cx, |app, _| app
            .document_view
            .as_ref()
            .map_or(0, |view| view.media.reads())),
        1,
        "one picture named once is read once, however many frames were drawn"
    );

    // What the toolkit made of those bytes, which is the claim the element id
    // cannot make on its own: one pixel came out, four bytes of BGRA.
    cx.run_until_parked();
    let prepared = fixture.view.read_with(cx, |app, _| {
        app.document_view
            .as_ref()
            .and_then(|view| view.media.image("diagram.png"))
            .expect("the worker read the picture into this document")
    });
    assert_eq!(
        prepared.bytes.as_slice(),
        ONE_PIXEL_PNG,
        "what the document holds is the file it read, not a name it could reread"
    );
    cx.update_window(fixture.handle, |_, window, cx| {
        let decoded = prepared
            .get_render_image(window, cx)
            .expect("the toolkit decoded the bytes the read brought back");
        assert_eq!(
            decoded.as_bytes(0).map(<[u8]>::len),
            Some(4),
            "one pixel of the picture was decoded, not merely laid out"
        );
    })
    .unwrap();

    let reading = reading(&fixture, cx);
    assert!(
        reading.contains("The diagram"),
        "the alt text is the line that stands for a picture: {reading}"
    );
    assert!(
        reading.contains("Video: assets/demo.mp4"),
        "a video this window cannot play is named rather than left blank: {reading}"
    );
    assert!(
        reading.contains("Image: ../outside.png"),
        "a picture that reaches out of the checkout is named too: {reading}"
    );
    close(&fixture, cx);
}

/// Every picture the checkout will not hand over is said in words.
///
/// A name that climbs out of the checkout, an absolute one, one inside `.git`, a
/// link out of the checkout, a file that is not a picture whatever its name says,
/// one too large to hold whole and one that is not there: each of these was
/// answered in words before, because the drawing was handed a path and the
/// loader failed on it. They are refused here, before anything is read, and the
/// words are what the reading shows either way.
#[gpui_kit::test]
async fn a_picture_the_checkout_will_not_hand_over_is_said_in_words(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("elsewhere.png"), ONE_PIXEL_PNG).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("elsewhere.png"),
        fixture.root.join("linked.png"),
    )
    .unwrap();
    std::fs::write(
        fixture.root.join(".git/hooks/pre-commit.png"),
        ONE_PIXEL_PNG,
    )
    .unwrap();
    std::fs::write(fixture.root.join("notes.png"), "words, not pixels\n").unwrap();
    // Sparse, and one byte past the bound: the length is what is refused.
    std::fs::File::create(fixture.root.join("enormous.png"))
        .unwrap()
        .set_len((muster_core::files::MAX_RESOURCE_BYTES + 1) as u64)
        .unwrap();
    let said = [
        "The escaping one",
        "The absolute one",
        "The object store",
        "The link out",
        "The text file",
        "The enormous one",
        "The missing one",
    ];
    let urls = [
        "../outside.png",
        "/etc/passwd",
        ".git/hooks/pre-commit.png",
        "linked.png",
        "notes.png",
        "enormous.png",
        "absent.png",
    ];
    let mut source = String::new();
    for (said, url) in said.iter().zip(urls) {
        source.push_str(&format!("![{said}]({url})\n\n"));
    }
    std::fs::write(fixture.root.join("README.md"), &source).unwrap();

    fixture.view.update(cx, |app, _| {
        app.navigation.active_checkout = Some(fixture.checkout.clone());
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "README.md".into(), window, cx);
        })
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, preview)| preview.path == "README.md")
    })
    .await;
    // Every worker runs, so this is what the document looks like after the reads
    // that could have succeeded and did not.
    for _ in 0..4 {
        cx.run_until_parked();
        cx.update_window(fixture.handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
    }

    assert!(
        fixture.view.read_with(cx, |app, _| {
            [
                ".git/hooks/pre-commit.png",
                "linked.png",
                "notes.png",
                "enormous.png",
                "absent.png",
            ]
            .iter()
            .all(|name| {
                app.document_view
                    .as_ref()
                    .is_none_or(|view| view.media.image(name).is_none())
            })
        }),
        "not one of those names came back with a picture to draw"
    );
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        for said in said {
            let id = format!(
                "document-media-{}",
                source.find(&format!("![{said}]")).unwrap()
            );
            assert!(
                window.try_find(id.clone()).is_none(),
                "{said} is not drawn as a picture"
            );
        }
    })
    .unwrap();
    let words = reading(&fixture, cx);
    for said in said {
        assert!(
            words.contains(said),
            "{said} is said in words instead: {words}"
        );
    }
    close(&fixture, cx);
}

/// The document a person is editing is a draft, and the two controls answer for
/// the file rather than for the window.
///
/// Everything the bar does is a claim about bytes: a save writes what the editor
/// holds and the file on disk is asked to say so afterwards, and a Cancel leaves
/// both the file and the editor as they were. The keyboard is the same two
/// controls, so each is answered by the key as well.
#[gpui_kit::test]
async fn an_edited_document_saves_or_throws_itself_away(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let file = fixture.root.join("notes.md");
    std::fs::write(&file, "# Notes\n").unwrap();
    open(cx, &fixture, "notes.md").await;
    // A Markdown document opens as its reading, so the source is asked for.
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("document-toggle-reading", cx);
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .unwrap();
    assert!(
        draw_until(&fixture, cx, "format-document"),
        "Code mode does not keep its footer visible"
    );
    assert!(
        !draw_until(&fixture, cx, "save-document"),
        "a clean document offers to save"
    );

    // Typed text, and the bar that says so.
    cx.update_window(fixture.handle, |_, window, cx| {
        click_editor(&fixture, window, cx);
        window.input("edited ", cx);
    })
    .unwrap();
    assert!(
        draw_until(&fixture, cx, "save-document"),
        "an edited document does not offer to be saved"
    );
    assert!(
        draw_until(&fixture, cx, "cancel-document"),
        "an edited document offers no way to change one's mind"
    );

    // Escape is the way out, and it leaves the file alone.
    cx.update_window(fixture.handle, |_, window, cx| {
        window.dispatch_keystroke(gpui_kit::Keystroke::parse("escape").unwrap(), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        assert_eq!(
            editor_text(&fixture, cx),
            "# Notes\n",
            "the draft was not thrown away"
        );
    })
    .unwrap();
    assert!(
        draw_until(&fixture, cx, "format-document"),
        "Cancel hid the Code footer"
    );
    assert!(
        !draw_until(&fixture, cx, "save-document"),
        "a document nobody has edited still offers to be saved"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "# Notes\n",
        "Cancel wrote the file"
    );

    // The other way round: `⌘S` saves, and the file is what says so.
    cx.update_window(fixture.handle, |_, window, cx| {
        click_editor(&fixture, window, cx);
        window.input("edited ", cx);
        window.render_frame(cx);
        window.dispatch_keystroke(
            gpui_kit::Keystroke::parse(if cfg!(target_os = "macos") {
                "cmd-s"
            } else {
                "ctrl-s"
            })
            .unwrap(),
            cx,
        );
    })
    .unwrap();
    for _ in 0..8 {
        cx.run_until_parked();
        cx.update_window(fixture.handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
        if std::fs::read_to_string(&file).unwrap() == "# Notes\nedited " {
            break;
        }
    }
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "# Notes\nedited ",
        "the save did not write what the editor held"
    );
    assert!(
        draw_until(&fixture, cx, "format-document"),
        "saving hid the Code footer"
    );
    assert!(
        !draw_until(&fixture, cx, "save-document"),
        "a saved document is still a draft"
    );
    let saved = fixture.view.read_with(cx, |app, cx| {
        app.document_view
            .as_ref()
            .expect("the document is still open")
            .state
            .read(cx)
            .text()
            .to_string()
    });
    assert_eq!(saved, "# Notes\nedited ", "the editor lost what was saved");
    close(&fixture, cx);
}

/// A file with changes says so, and the button is what reads them.
///
/// The change list is Git's answer rather than the window's, so the button is
/// there exactly when Git has said this file is in it — and clicking it opens
/// that one file's diff rather than the whole change set. Nobody reads the
/// status before the file is opened: asking is what opening it does, so this
/// pins the button on a window where the ten-second tick has not come around.
#[gpui_kit::test]
async fn a_document_offers_the_changes_of_a_file_that_has_them(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("notes.md"), "# Notes\n").unwrap();
    std::fs::write(fixture.root.join("quiet.md"), "quiet\n").unwrap();
    commit_all(&fixture.root, "first");
    std::fs::write(fixture.root.join("notes.md"), "# Notes\nedited\n").unwrap();

    fixture.view.update(cx, |app, _| {
        app.navigation.active_checkout = Some(fixture.checkout.clone());
    });

    open(cx, &fixture, "notes.md").await;
    assert!(
        draw_until(&fixture, cx, "document-open-changes"),
        "a file with changes offers no way to read them"
    );
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("document-open-changes", cx);
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .unwrap();
    assert_eq!(
        fixture.view.read_with(cx, |app, _| app.open_diff.clone()),
        Some((fixture.checkout.clone(), Some("notes.md".into()))),
        "the button did not open this file's changes"
    );
    assert!(
        fixture
            .view
            .read_with(cx, |app, _| app.open_document.is_none()),
        "the document and its changes are one panel, and it shows the changes"
    );
    close(&fixture, cx);
}

/// A file's changes offer the source the other way round, unless Git removed it.
///
/// The same gesture read from the other pane: the changes of one file are the
/// document of that file, and the button is what says so. What decides it is
/// whether the file is still on disk, and that is Git's letter rather than the
/// window's guess — a change that removed the file has nothing behind the path
/// to open, so the pane says nothing rather than a read that can only fail.
#[gpui_kit::test]
async fn a_files_changes_offer_the_source_of_a_file_that_is_on_disk(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("notes.md"), "# Notes\n").unwrap();
    std::fs::write(fixture.root.join("gone.md"), "gone\n").unwrap();
    commit_all(&fixture.root, "first");
    std::fs::write(fixture.root.join("notes.md"), "# Notes\nedited\n").unwrap();
    std::fs::remove_file(fixture.root.join("gone.md")).unwrap();

    fixture.view.update(cx, |app, _| {
        app.navigation.active_checkout = Some(fixture.checkout.clone());
    });
    read_status(&fixture, &fixture.checkout, cx).await;

    fixture.view.update(cx, |app, cx| {
        app.open_changes(&fixture.checkout, Some("notes.md".into()), cx)
    });
    assert!(
        draw_until(&fixture, cx, "diff-open-source-notes.md"),
        "a file that is on disk offers no way to read it"
    );
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("diff-open-source-notes.md", cx);
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, preview)| preview.path == "notes.md")
    })
    .await;
    assert!(
        fixture.view.read_with(cx, |app, _| app.open_diff.is_none()),
        "the document and its changes are one panel, and it shows the document"
    );

    fixture.view.update(cx, |app, cx| {
        app.open_changes(&fixture.checkout, Some("gone.md".into()), cx)
    });
    assert!(
        !draw_until(&fixture, cx, "diff-open-source-gone.md"),
        "a removed file offers a source it does not have"
    );
    close(&fixture, cx);
}
/// Open one document of the fixture's checkout and wait for the file to be read.
async fn open(cx: &mut TestAppContext, fixture: &GitFixture, path: &str) {
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, path.into(), window, cx);
        })
    })
    .unwrap();
    let path = path.to_owned();
    cx.wait_for(fixture.handle, Duration::from_secs(20), move |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, preview)| preview.path == path)
    })
    .await;
}

#[gpui_kit::test]
async fn dirty_document_navigation_waits_for_cancel_or_discard(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("first.txt"), "original").unwrap();
    std::fs::write(fixture.root.join("second.txt"), "destination").unwrap();
    let open_path = |path: &'static str, cx: &mut TestAppContext| {
        cx.update_window(fixture.handle, |_, window, cx| {
            fixture.view.update(cx, |app, cx| {
                let repo = app.navigation.repos[0].clone();
                let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
                app.request_document(repo, checkout, path.to_owned(), window, cx);
            });
        })
        .unwrap();
    };
    open_path("first.txt", cx);
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, file)| file.path == "first.txt")
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        click_editor(&fixture, window, cx);
        window.input(" edited", cx);
    })
    .unwrap();
    open_path("second.txt", cx);
    assert!(confirmation_open(fixture.handle, cx));
    answer_confirmation(fixture.handle, "Cancel", cx);
    cx.run_until_parked();
    assert_eq!(
        fixture.view.read_with(cx, |app, cx| app
            .document_view
            .as_ref()
            .unwrap()
            .state
            .read(cx)
            .text()
            .to_string()),
        "original edited"
    );
    assert_eq!(
        fixture
            .view
            .read_with(cx, |app, _| app.document_target.as_ref().unwrap().1.clone()),
        "first.txt"
    );

    open_path("second.txt", cx);
    answer_confirmation(fixture.handle, "Discard", cx);
    cx.run_until_parked();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, file)| file.path == "second.txt")
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("first.txt")).unwrap(),
        "original"
    );
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn app_quit_waits_for_dirty_document_decisions(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("first.txt"), "original").unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "first.txt".into(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, file)| file.path == "first.txt")
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        click_editor(&fixture, window, cx);
        window.input(" draft", cx);
    })
    .unwrap();
    cx.update(crate::menus::install);
    cx.update_window(fixture.handle, |_, window, cx| window.press("cmd-q", cx))
        .unwrap();
    assert!(confirmation_open(fixture.handle, cx));
    answer_confirmation(fixture.handle, "Cancel", cx);
    cx.run_until_parked();
    assert!(fixture
        .view
        .read_with(cx, |app, _| app.document_view.as_ref().unwrap().is_dirty()));
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn dirty_document_window_close_can_be_cancelled(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("first.txt"), "original").unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "first.txt".into(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, file)| file.path == "first.txt")
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        click_editor(&fixture, window, cx);
        window.input(" draft", cx);
        fixture
            .view
            .update(cx, |app, cx| app.request_window_close(window, cx));
    })
    .unwrap();
    assert!(confirmation_open(fixture.handle, cx));
    answer_confirmation(fixture.handle, "Cancel", cx);
    cx.run_until_parked();
    assert!(fixture
        .view
        .read_with(cx, |app, _| app.document_view.as_ref().unwrap().is_dirty()));
    assert_eq!(
        fixture.view.read_with(cx, |app, cx| app
            .document_view
            .as_ref()
            .unwrap()
            .state
            .read(cx)
            .text()
            .to_string()),
        "original draft"
    );
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn dirty_document_save_conflict_keeps_draft_and_pending_destination(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("first.txt"), "original").unwrap();
    std::fs::write(fixture.root.join("second.txt"), "destination").unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "first.txt".into(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, file)| file.path == "first.txt")
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        click_editor(&fixture, window, cx);
        window.input(" draft", cx);
    })
    .unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "second.txt".into(), window, cx);
        });
    })
    .unwrap();
    std::fs::write(fixture.root.join("first.txt"), "external edit").unwrap();
    answer_confirmation(fixture.handle, "Save", cx);
    cx.run_until_parked();
    // The refusal is the window's toast rather than a line in the action bar: the bar
    // is Legacy's, and it says what the draft needs rather than what went wrong.
    assert_eq!(
        fixture
            .view
            .read_with(cx, |app, _| app.toasts.latest_error().map(str::to_owned)),
        Some("file changed on disk; reload it before saving".to_owned())
    );
    fixture.view.read_with(cx, |app, cx| {
        assert_eq!(app.document_target.as_ref().unwrap().1, "first.txt");
        assert!(app.document_view.as_ref().unwrap().is_dirty());
        assert!(app.pending_document_transition.is_some());
        assert_eq!(
            app.document_view
                .as_ref()
                .unwrap()
                .state
                .read(cx)
                .text()
                .to_string(),
            "original draft"
        );
    });
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("first.txt")).unwrap(),
        "external edit"
    );
    assert!(draw_until(&fixture, cx, "stay-in-document"));
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click(gpui_kit::ElementId::from("stay-in-document"), cx);
    })
    .unwrap();
    assert!(fixture
        .view
        .read_with(cx, |app, _| app.pending_document_transition.is_none()));
    cx.update_window(fixture.handle, |_, _, cx| {
        assert_eq!(editor_text(&fixture, cx), "original draft");
    })
    .unwrap();
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn uncertain_publication_blocks_navigation_until_durability_retry(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("first.txt"), "original").unwrap();
    std::fs::write(fixture.root.join("second.txt"), "destination").unwrap();
    let open = |path: &'static str, cx: &mut TestAppContext| {
        cx.update_window(fixture.handle, |_, window, cx| {
            fixture.view.update(cx, |app, cx| {
                let repo = app.navigation.repos[0].clone();
                let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
                app.request_document(repo, checkout, path.into(), window, cx);
            });
        })
        .unwrap();
    };
    open("first.txt", cx);
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, file)| file.path == "first.txt")
    })
    .await;
    fixture.view.update(cx, |app, _| {
        app.document_view.as_mut().unwrap().durability_uncertain = true;
    });
    open("second.txt", cx);
    assert!(!confirmation_open(fixture.handle, cx));
    assert_eq!(
        fixture
            .view
            .read_with(cx, |app, _| app.document_target.as_ref().unwrap().1.clone()),
        "first.txt"
    );
    assert!(draw_until(&fixture, cx, "save-document"));
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click(gpui_kit::ElementId::from("save-document"), cx);
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, file)| file.path == "second.txt")
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("first.txt")).unwrap(),
        "original"
    );
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn dirty_document_navigation_applies_only_after_successful_save(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("first.txt"), "original").unwrap();
    std::fs::write(fixture.root.join("second.txt"), "destination").unwrap();
    let open = |path: &'static str, cx: &mut TestAppContext| {
        cx.update_window(fixture.handle, |_, window, cx| {
            fixture.view.update(cx, |app, cx| {
                let repo = app.navigation.repos[0].clone();
                let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
                app.request_document(repo, checkout, path.to_owned(), window, cx);
            });
        })
        .unwrap();
    };
    open("first.txt", cx);
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, file)| file.path == "first.txt")
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        click_editor(&fixture, window, cx);
        window.input(" saved", cx);
    })
    .unwrap();
    open("second.txt", cx);
    answer_confirmation(fixture.handle, "Save", cx);
    cx.run_until_parked();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, file)| file.path == "second.txt")
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("first.txt")).unwrap(),
        "original saved"
    );
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn navigation_requested_while_saving_keeps_newer_edits_until_decided(
    cx: &mut TestAppContext,
) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("first.txt"), "original").unwrap();
    std::fs::write(fixture.root.join("second.txt"), "destination").unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "first.txt".into(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, file)| file.path == "first.txt")
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        click_editor(&fixture, window, cx);
        window.input(" first edit", cx);
        fixture.view.update(cx, |app, _| {
            app.document_view.as_mut().unwrap().saving = true
        });
        window.input(" newer edit", cx);
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "second.txt".into(), window, cx);
        });
    })
    .unwrap();
    assert!(!confirmation_open(fixture.handle, cx));
    fixture.view.update(cx, |app, _| {
        app.document_view.as_mut().unwrap().saving = false
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture
            .view
            .update(cx, |app, cx| app.continue_document_transition(window, cx));
    })
    .unwrap();
    assert!(confirmation_open(fixture.handle, cx));
    answer_confirmation(fixture.handle, "Cancel", cx);
    cx.run_until_parked();
    cx.update_window(fixture.handle, |_, _, cx| {
        assert_eq!(editor_text(&fixture, cx), "original first edit newer edit");
    })
    .unwrap();
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn navigation_requested_during_formatting_is_deferred_until_idle(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("first.txt"), "original").unwrap();
    std::fs::write(fixture.root.join("second.txt"), "destination").unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "first.txt".into(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, file)| file.path == "first.txt")
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        click_editor(&fixture, window, cx);
        window.input(" draft", cx);
        fixture.view.update(cx, |app, _| {
            app.document_view.as_mut().unwrap().formatting = true
        });
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "second.txt".into(), window, cx);
        });
    })
    .unwrap();
    assert!(!confirmation_open(fixture.handle, cx));
    fixture.view.update(cx, |app, _| {
        app.document_view.as_mut().unwrap().formatting = false
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture
            .view
            .update(cx, |app, cx| app.continue_document_transition(window, cx));
    })
    .unwrap();
    assert!(confirmation_open(fixture.handle, cx));
    assert_eq!(
        fixture
            .view
            .read_with(cx, |app, _| app.document_target.as_ref().unwrap().1.clone()),
        "first.txt"
    );
    answer_confirmation(fixture.handle, "Cancel", cx);
    cx.run_until_parked();
    cx.update_window(fixture.handle, |_, _, cx| {
        assert_eq!(editor_text(&fixture, cx), "original draft");
    })
    .unwrap();
    close(&fixture, cx);
}

/// A draft with nowhere to go is not a decision anybody has to make.
///
/// The helper is also how a format finishes, and a format navigates nowhere, so
/// without a pending destination it has nothing to apply and nothing to ask
/// about: the draft stays exactly where the person left it.
#[gpui_kit::test]
async fn a_document_transition_with_no_destination_asks_nothing(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("first.txt"), "original").unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "first.txt".into(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, file)| file.path == "first.txt")
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        click_editor(&fixture, window, cx);
        window.input(" draft", cx);
        window.render_frame(cx);
    })
    .unwrap();
    fixture.view.update(cx, |app, _| {
        assert!(app.document_view.as_ref().unwrap().is_dirty());
        assert!(app.pending_document_transition.is_none());
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture
            .view
            .update(cx, |app, cx| app.continue_document_transition(window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!confirmation_open(fixture.handle, cx));
    fixture.view.update(cx, |app, _| {
        assert!(app.document_view.as_ref().unwrap().is_dirty());
        assert!(app.pending_document_transition.is_none());
        assert_eq!(app.document_target.as_ref().unwrap().1, "first.txt");
    });
    cx.update_window(fixture.handle, |_, _, cx| {
        assert_eq!(editor_text(&fixture, cx), "original draft");
    })
    .unwrap();
    close(&fixture, cx);
}

/// Finishing a format is not leaving the document, however the format went.
///
/// Whether the formatter installed its text or refused to run, its completion
/// asks the same helper a pending navigation would, and a draft nobody was
/// navigating away from has to survive that without a prompt. The answer does
/// not depend on whether a Prettier happens to be installed, so the check is
/// the decision rather than the formatter's own outcome.
#[gpui_kit::test]
async fn a_finished_format_with_no_destination_asks_nothing(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("first.ts"), "const a = 1;\n").unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "first.ts".into(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, file)| file.path == "first.ts")
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        click_editor(&fixture, window, cx);
        window.input("const b = 2;", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture
            .view
            .update(cx, |app, cx| app.format_document(window, cx));
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(30), |_, cx| {
        !fixture
            .view
            .read(cx)
            .document_view
            .as_ref()
            .expect("a document is open")
            .formatting
    })
    .await;
    assert!(!confirmation_open(fixture.handle, cx));
    fixture.view.update(cx, |app, _| {
        assert!(app.document_view.as_ref().unwrap().is_dirty());
        assert!(app.pending_document_transition.is_none());
        assert_eq!(app.document_target.as_ref().unwrap().1, "first.ts");
    });
    close(&fixture, cx);
}

/// Put the caret in the open document's editor, which is where a person types.
fn click_editor(fixture: &GitFixture, window: &mut gpui_kit::Window, cx: &mut gpui_kit::App) {
    let state = fixture.view.read_with(cx, |app, _| {
        app.document_view
            .as_ref()
            .expect("a document is open")
            .state
            .clone()
    });
    window.click(("input", state.entity_id()), cx);
}

/// The text the open document's editor holds.
fn editor_text(fixture: &GitFixture, cx: &gpui_kit::App) -> String {
    fixture.view.read_with(cx, |app, cx| {
        app.document_view
            .as_ref()
            .expect("a document is open")
            .state
            .read(cx)
            .text()
            .to_string()
    })
}

/// A picture read for a document that has been closed updates nothing and keeps
/// nothing.
///
/// The store a read publishes into belongs to the document that named the
/// picture, so a read that lands after the document is gone has nowhere to put
/// its bytes and nothing to retain: the store went with the view.
#[gpui_kit::test]
async fn a_picture_read_after_its_document_is_closed_is_dropped(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("diagram.png"), ONE_PIXEL_PNG).unwrap();
    let source = "![The diagram](diagram.png)\n";
    std::fs::write(fixture.root.join("README.md"), source).unwrap();
    fixture.view.update(cx, |app, _| {
        app.navigation.active_checkout = Some(fixture.checkout.clone());
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "README.md".into(), window, cx);
        })
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, preview)| preview.path == "README.md")
    })
    .await;
    let picture = format!("document-media-{}", source.find("![The diagram]").unwrap());
    // One frame: the document is parsed and its picture is on a worker that has
    // not run, which is the window in which this read can go wrong.
    let store = cx
        .update_window(fixture.handle, |_, window, cx| {
            window.render_frame(cx);
            fixture.view.read_with(cx, |app, _| {
                app.document_view
                    .as_ref()
                    .expect("a document")
                    .media
                    .clone()
            })
        })
        .unwrap();
    let gone = Arc::downgrade(&store);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(gpui_kit::ElementId::from("close-document"), cx);
    })
    .unwrap();
    drop(store);
    for _ in 0..4 {
        cx.run_until_parked();
        cx.update_window(fixture.handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
    }

    assert!(
        gone.upgrade().is_none(),
        "the prepared pictures and the store that held them went with the document"
    );
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find(picture.clone()).is_none(),
            "nothing of a closed document is left on screen"
        );
        assert!(
            fixture
                .view
                .read_with(cx, |app, _| app.document_view.is_none()),
            "and the document pane is closed rather than showing a late arrival"
        );
    })
    .unwrap();

    // Opening it again reads it again into a store of its own: the late read
    // above published nothing that this document could have drawn.
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "README.md".into(), window, cx);
        })
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, preview)| preview.path == "README.md")
    })
    .await;
    assert!(
        draw_until(&fixture, cx, &picture),
        "the reopened document reads its picture afresh"
    );
    assert!(
        fixture.view.read_with(cx, |app, _| {
            app.document_view
                .as_ref()
                .is_some_and(|view| view.media.image("diagram.png").is_some())
        }),
        "into a store that belongs to this document alone"
    );
    close(&fixture, cx);
}

/// A picture whose bytes do not decode is said in words rather than left as the
/// blank box a loader that failed always was.
///
/// This is the one failure the bytes can still fail at after the read has
/// succeeded and the format has been decided: the header says PNG and the rest
/// is not one. The picture has arrived, so the node is claimed, and the element
/// that cannot paint falls back to the line that stands in for a picture.
#[gpui_kit::test]
async fn a_picture_that_does_not_decode_is_said_in_words(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    // A real PNG header, and then nothing that is an image.
    let mut broken = ONE_PIXEL_PNG[..8].to_vec();
    broken.extend_from_slice(b"not the rest of a picture");
    std::fs::write(fixture.root.join("broken.png"), &broken).unwrap();
    std::fs::write(fixture.root.join("diagram.png"), ONE_PIXEL_PNG).unwrap();
    let source = "![The broken one](broken.png)\n\n![The good one](diagram.png)\n";
    std::fs::write(fixture.root.join("README.md"), source).unwrap();
    fixture.view.update(cx, |app, _| {
        app.navigation.active_checkout = Some(fixture.checkout.clone());
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "README.md".into(), window, cx);
        })
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, preview)| preview.path == "README.md")
    })
    .await;
    let id = |said: &str| {
        format!(
            "document-media-{}",
            source.find(&format!("![{said}]")).unwrap()
        )
    };
    let said_id = |said: &str| {
        format!(
            "document-media-said-{}",
            source.find(&format!("![{said}]")).unwrap()
        )
    };
    assert!(
        draw_until(&fixture, cx, &id("The good one")),
        "the picture that does decode is drawn"
    );

    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find(said_id("The broken one")).is_some(),
            "a picture that will not decode is said in words where it was going to be"
        );
        assert!(
            window.try_find(said_id("The good one")).is_none(),
            "a picture that decoded is a picture, and says nothing in its place"
        );
    })
    .unwrap();
    let words = reading(&fixture, cx);
    assert!(
        words.contains("The broken one") && words.contains("The good one"),
        "and both are still what a selection of the reading copies: {words}"
    );
    close(&fixture, cx);
}

/// What is drawn is the bytes that were read.
///
/// A picture that is prepared is not read again: the name it was read from can
/// be gone, or can be pointing at something else entirely, and the reading keeps
/// the copy it took. That is the whole difference between handing the toolkit
/// bytes and handing it a path it will reopen whenever it likes.
///
/// The document is a nested one reaching a sibling directory, which is the name
/// a README most often spells: `../` walked, still inside the checkout.
#[gpui_kit::test]
async fn a_picture_drawn_is_the_bytes_that_were_read(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::create_dir_all(fixture.root.join("docs/guide")).unwrap();
    std::fs::create_dir_all(fixture.root.join("docs/assets")).unwrap();
    std::fs::write(fixture.root.join("docs/assets/diagram.png"), ONE_PIXEL_PNG).unwrap();
    let source = "![The diagram](../assets/diagram.png)\n";
    std::fs::write(fixture.root.join("docs/guide/README.md"), source).unwrap();
    fixture.view.update(cx, |app, _| {
        app.navigation.active_checkout = Some(fixture.checkout.clone());
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "docs/guide/README.md".into(), window, cx);
        })
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .open_document
            .as_ref()
            .is_some_and(|(_, preview)| preview.path == "docs/guide/README.md")
    })
    .await;
    let picture = format!("document-media-{}", source.find("![The diagram]").unwrap());
    assert!(draw_until(&fixture, cx, &picture));
    let prepared = fixture.view.read_with(cx, |app, _| {
        app.document_view
            .as_ref()
            .and_then(|view| view.media.image("docs/assets/diagram.png"))
            .expect("the worker read the picture this nested document names")
    });

    // The name now leads somewhere else, and would be a picture the reader never
    // asked for if anything reopened it.
    std::fs::write(
        fixture.root.join("docs/assets/diagram.png"),
        b"not a picture any more\n",
    )
    .unwrap();
    for _ in 0..4 {
        cx.run_until_parked();
        cx.update_window(fixture.handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
    }

    let again = fixture.view.read_with(cx, |app, _| {
        app.document_view
            .as_ref()
            .and_then(|view| view.media.image("docs/assets/diagram.png"))
    });
    assert!(
        again
            .as_ref()
            .is_some_and(|again| Arc::ptr_eq(again, &prepared)),
        "the picture is the one that was read, kept as it was read"
    );
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find(picture.clone()).is_some(),
            "and it is still drawn after the file behind its name changed"
        );
        let decoded = prepared
            .get_render_image(window, cx)
            .expect("the toolkit decoded the bytes the read brought back");
        assert_eq!(
            decoded.as_bytes(0).map(<[u8]>::len),
            Some(4),
            "the pixel is the one that was read"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// The modifier that means "open this somewhere else" is the platform's own.
///
/// Control on Linux and Windows, command on macOS, which is what
/// `Modifiers::secondary_key` is in the toolkit — this asks the same question of
/// a real click rather than naming a key. The gesture itself is not driven from a
/// test: the row's other answer spawns the opener, and a headless run would open
/// a real application on the machine running the suite. What the click *for*
/// is, and what the resolution of that path refuses, are both tested without one.
#[test]
fn a_modifier_is_what_separates_opening_a_row_from_acting_on_it() {
    use gpui_kit::{
        point, px, ClickEvent, Modifiers, MouseButton, MouseClickEvent, MouseDownEvent,
        MouseUpEvent,
    };
    let click = |modifiers| {
        let event = MouseDownEvent {
            button: MouseButton::Left,
            position: point(px(0.), px(0.)),
            modifiers,
            click_count: 1,
            first_mouse: false,
        };
        ClickEvent::Mouse(MouseClickEvent {
            up: MouseUpEvent {
                button: MouseButton::Left,
                position: point(px(0.), px(0.)),
                modifiers,
                click_count: 1,
            },
            down: event,
        })
    };
    let none = Modifiers::none();
    assert!(
        !opens_elsewhere(&click(none)),
        "a plain click is the row's own"
    );
    assert!(
        opens_elsewhere(&click(Modifiers {
            control: true,
            ..Modifiers::none()
        })),
        "ctrl opens it elsewhere"
    );
    assert!(
        opens_elsewhere(&click(Modifiers {
            platform: true,
            ..Modifiers::none()
        })),
        "cmd opens it elsewhere too, because this app runs on both"
    );
    assert!(
        !opens_elsewhere(&click(Modifiers {
            shift: true,
            ..Modifiers::none()
        })),
        "shift is a selection, not an opening"
    );
}

/// A name too long for the panel is reachable, not shortened.
///
/// Legacy's rows are `max-content` and the panel scrolls sideways; the native one
/// truncated at the panel's edge, so a deep tree's tail was unreadable with no
/// way to see it. The scrollbar is not asserted as pixels: what is asserted is the
/// handle's own maximum offset, which is the number the layout derived from the
/// widest row.
#[gpui_kit::test]
async fn a_name_too_long_for_the_panel_is_scrolled_to_not_cut(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let long = "a-widget-with-a-really-long-file-name-that-does-not-fit-inside-the-panel.tsx";
    std::fs::write(fixture.root.join(long), "x").unwrap();
    let key = |path: &str| (fixture.checkout.clone(), path.to_owned());
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.navigation.active_checkout = Some(fixture.checkout.clone());
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
            .is_some_and(|entries| entries.iter().any(|entry| entry.path == long))
    })
    .await;

    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        let scroll = fixture.view.read(cx).files_scroll.clone();
        let name = window.find(format!("file-name-{long}")).bounds();
        let slot = window.find(format!("file-slot-{long}")).bounds();
        assert!(
            scroll.max_offset().x > px(0.),
            "a name wider than the panel is reachable sideways: max {:?}",
            scroll.max_offset().x
        );
        assert_eq!(
            slot.origin.x
                - window
                    .find(format!("file-control:{long}"))
                    .bounds()
                    .origin
                    .x,
            px(8.),
            "and the row still starts at the panel's own gutter"
        );
        assert!(
            name.size.width > px(200.),
            "the name is drawn whole rather than shortened to fit"
        );
    })
    .unwrap();

    // A tree whose names all fit scrolls nowhere: the width is the panel's.
    std::fs::remove_file(fixture.root.join(long)).unwrap();
    std::fs::write(fixture.root.join("short.txt"), "x").unwrap();
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
            .is_some_and(|entries| entries.iter().any(|entry| entry.path == "short.txt"))
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        let scroll = fixture.view.read(cx).files_scroll.clone();
        assert_eq!(
            scroll.max_offset().x,
            px(0.),
            "a tree that fits does not scroll sideways"
        );
        let row = window.find("file-control:short.txt").bounds();
        assert_eq!(row.size.width, px(279.), "and its row fills the panel");
    })
    .unwrap();
    close(&fixture, cx);
}

/// The change set is read in a panel that does not scroll sideways.
///
/// Legacy's rows are `max-content` and the panel scrolled to follow a long path,
/// so the Git letter and the counts scrolled off with it and the name was only
/// readable by scrolling after it. A row is the panel's width now: the letter
/// and the counts keep their columns, and the name takes what is left of the row
/// and ends in an ellipsis.
#[gpui_kit::test]
async fn a_change_row_fits_the_panel_and_a_long_name_gives_up_its_room(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let long = "src/components/a-widget-with-a-really-long-file-name-here.tsx";
    std::fs::create_dir_all(fixture.root.join("src/components")).unwrap();
    std::fs::write(fixture.root.join(long), "first\n").unwrap();
    std::fs::write(fixture.root.join("short.txt"), "first\n").unwrap();
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
    std::fs::write(fixture.root.join(long), "second\n").unwrap();
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
            .is_some_and(|status| !status.files.is_empty())
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        window.render_frame(cx);
        window.render_frame(cx);
        // The rows are the panel's width and no wider, so nothing in the panel
        // is reachable by scrolling sideways.
        let panel = window.find("all-changes").bounds();
        let group = window.find("change-group-src/components").bounds();
        let row = window.find(format!("change-control-{long}")).bounds();
        assert!(
            row.right() <= panel.right(),
            "a row is the panel's width: {row:?} runs past {panel:?}"
        );
        assert_eq!(
            group.size.width, row.size.width,
            "and the heading above it is that width too"
        );
        let status = window.find(format!("row-{long}-status")).bounds();
        let name = window.find(format!("change-name-{long}")).bounds();
        let addition = window.find(format!("row-{long}-add")).bounds();
        assert!(
            status.right() <= name.origin.x,
            "the letter keeps its column"
        );
        assert!(name.right() <= addition.origin.x, "the counts keep theirs");
        assert!(
            addition.right() <= row.right(),
            "and the counts are inside the panel: {:?}",
            addition.right() - row.right()
        );
        assert!(
            name.size.width < px(200.),
            "a name too long for the panel is shortened to fit it: {:?}",
            name.size.width
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// A link the opener will refuse, in a document the fixture has actually read.
///
/// `file:` is refused by the opener's own scheme check before anything is
/// spawned, so the whole path is exercised and no browser is ever launched.
const REFUSED_LINK: &str = "file:///etc/passwd";

/// A document whose only content is one link the opener refuses.
fn write_a_refused_link(root: &Path) {
    std::fs::write(
        root.join("README.md"),
        format!("# Links\n\n[not a page]({REFUSED_LINK})\n"),
    )
    .unwrap();
}

/// The window's own work, run with no frame drawn.
///
/// A link used to be left in a queue that only the document pane drained while
/// rendering, so an error that arrives only once something else draws is
/// precisely what this pins. Nothing below renders a frame.
async fn until_the_failure_is_stated(
    cx: &mut TestAppContext,
    view: &Entity<TerminalWindow>,
) -> bool {
    for _ in 0..200 {
        if view.read_with(cx, |app, _| app.toasts.latest_error().is_some()) {
            return true;
        }
        cx.background_executor.timer(Duration::from_millis(5)).await;
        cx.run_until_parked();
    }
    view.read_with(cx, |app, _| app.toasts.latest_error().is_some())
}

/// A rendered link the opener refuses is said in the window, and dismissed.
///
/// The notice used to be filed under the file tree's opener key, and the only
/// renderer of that key drew file rows and skipped links, so a link that would
/// not open said nothing at all: a click that looked like it had opened a page.
/// It is the window's toast now, which is drawn whatever the inspector is
/// showing, and this reads it off the screen.
#[gpui_kit::test]
async fn a_refused_link_is_said_in_the_window_and_dismissed(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    write_a_refused_link(&fixture.root);
    fixture.view.update(cx, |app, _| {
        app.navigation.active_checkout = Some(fixture.checkout.clone());
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "README.md".into(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture.view.read(cx).open_document.is_some()
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .unwrap();

    // The click itself. Nothing renders between it and the error: a link that
    // waited for a later frame to be opened was a link that opened whenever the
    // window happened to draw next, which is the defect this pins.
    let clicked = cx.update_window(fixture.handle, |_, window, cx| {
        window.click_at("document-markdown", point(px(30.), px(60.)), cx)
    });
    clicked.expect("a click in an open window");
    assert!(
        until_the_failure_is_stated(cx, &fixture.view).await,
        "the click opened the link without anything else having to draw"
    );

    // The error is on screen, in this window's own toast stack.
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("toast-stack").is_some(),
            "what the opener refused is said in the window's toast stack"
        );
        fixture.view.read_with(cx, |app, _| {
            assert_eq!(
                app.toasts.latest_error(),
                Some("only an http or https link can be opened in the browser"),
                "the refusal is the opener's own words"
            );
        });
    })
    .unwrap();

    // A link is a document's, not the tree's: saying so opened nothing else.
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(
            app.document_target.as_ref().map(|(_, path)| path.as_str()),
            Some("README.md"),
            "the document that was being read is still the one on screen"
        );
        assert!(app.open_document.is_some());
        assert!(app.external_notices.is_empty());
        assert!(
            app.expanded_files.is_empty() && app.selected_file.is_empty(),
            "a link in a document is not a row in the tree"
        );
    });

    // And it goes away with its own dismiss button.
    let toast_id = fixture
        .view
        .read_with(cx, |app, _| app.toasts.newest_id())
        .expect("the toast has an id");
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click(format!("dismiss-toast-{toast_id}"), cx);
        window.render_frame(cx);
        assert!(
            window.try_find("toast-stack").is_none(),
            "dismissing takes the message away"
        );
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert!(app.toasts.latest_error().is_none());
        assert!(
            app.open_document.is_some(),
            "and the document is still open"
        );
    });
    close(&fixture, cx);
}

/// The same refusal is said with Files not selected, because the pane it was
/// clicked in is not the inspector.
///
/// Two of the three states the inspector can be in are checked here: the tab
/// set to Changes, and the panels hidden altogether. Neither is where a
/// document is read, and neither may swallow what the opener said.
#[gpui_kit::test]
async fn a_refused_link_is_said_whatever_the_inspector_is_showing(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    write_a_refused_link(&fixture.root);
    fixture.view.update(cx, |app, _| {
        app.navigation.active_checkout = Some(fixture.checkout.clone());
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "README.md".into(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture.view.read(cx).open_document.is_some()
    })
    .await;

    for panels_visible in [true, false] {
        fixture.view.update(cx, |app, cx| {
            app.inspector_tab = InspectorTab::Changes;
            app.navigation.panels_visible = panels_visible;
            cx.notify();
        });
        cx.update_window(fixture.handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            window.click_at("document-markdown", point(px(30.), px(60.)), cx);
        })
        .unwrap();
        assert!(
            until_the_failure_is_stated(cx, &fixture.view).await,
            "a link is refused the same way with panels_visible={panels_visible}"
        );
        cx.update_window(fixture.handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("toast-stack").is_some(),
                "the toast is the window's, not the inspector's"
            );
        })
        .unwrap();
        let toast_id = fixture
            .view
            .read_with(cx, |app, _| app.toasts.newest_id())
            .expect("the toast has an id");
        cx.update_window(fixture.handle, |_, window, cx| {
            window.click(format!("dismiss-toast-{toast_id}"), cx);
            window.render_frame(cx);
        })
        .unwrap();
    }
    close(&fixture, cx);
}

/// A link clicked after its window is gone opens nothing, and says nothing.
///
/// The handler holds the window weakly, which is what lets the toolkit's
/// `Send + Sync` callback carry it. The other half of that is what a click does
/// once the window is released: the upgrade fails, the click ends, and nothing
/// is spawned and nothing panics on the way.
#[gpui_kit::test]
async fn a_link_clicked_after_its_window_is_released_opens_nothing(cx: &mut TestAppContext) {
    use gpui_kit::{
        ClickEvent, Modifiers, MouseButton, MouseClickEvent, MouseDownEvent, MouseUpEvent,
    };

    let fixture = open_git(cx);
    let released = fixture.view.downgrade();
    close(&fixture, cx);
    drop(fixture);
    assert!(
        !released.is_upgradable(),
        "the window is gone, which is the state this case is about"
    );

    // A window that has nothing in it, so the handler can be called with the
    // `Window` and `App` the text view would have handed it.
    let (handle, _) = cx.update(|cx| {
        gpui_kit::open_window(crate::app::window_bounds(cx), cx, |_, cx| cx.new(|_| Empty))
            .expect("a window to click in")
    });
    let click = ClickEvent::Mouse(MouseClickEvent {
        down: MouseDownEvent {
            button: MouseButton::Left,
            position: point(px(0.), px(0.)),
            modifiers: Modifiers::none(),
            click_count: 1,
            first_mouse: false,
        },
        up: MouseUpEvent {
            button: MouseButton::Left,
            position: point(px(0.), px(0.)),
            modifiers: Modifiers::none(),
            click_count: 1,
        },
    });
    let handler = crate::document::link_handler(released.clone());
    // Reaching a released window is the failure this pins: the handler answers
    // `Err` and stops, rather than upgrading, spawning an opener, or panicking.
    cx.update_window(handle, |_, window, cx| {
        handler(
            &gpui_kit::SharedString::from(REFUSED_LINK),
            &click,
            window,
            cx,
        );
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!released.is_upgradable());
    cx.update_window(handle, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
async fn a_review_note_with_a_missing_line_stays_visible_on_its_file(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let path = "orphan.txt";
    std::fs::write(fixture.root.join(path), "original line\n").unwrap();
    commit_all(&fixture.root, "add review target");
    std::fs::remove_file(fixture.root.join(path)).unwrap();
    let note = muster_core::review::ReviewNote {
        id: "orphan-note".into(),
        checkout_id: fixture.checkout.clone(),
        path: path.into(),
        side: muster_core::review::ReviewSide::Old,
        line_start: 99,
        line_end: None,
        content: "This line disappeared".into(),
        code: "original line".into(),
        code_hash: muster_core::review::review_anchor_hash("original line"),
        outdated: false,
    };
    fixture.view.update(cx, |app, cx| {
        app.review
            .notes
            .insert(fixture.checkout.clone(), vec![note]);
        app.inspector_tab = InspectorTab::Changes;
        app.open_changes(&fixture.checkout, Some(path.into()), cx);
    });
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_diff
            .get(&(fixture.checkout.clone(), path.into()))
            .is_some_and(|diff| !diff.patch.is_empty())
    })
    .await;
    assert!(draw_until(&fixture, cx, "review-destination"));
    assert!(draw_until(&fixture, cx, "review-card-orphan-note"));
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn an_unknown_review_can_be_checked_again_without_authorizing_a_duplicate(
    cx: &mut TestAppContext,
) {
    use muster_core::opencode::{ReviewRound, RoundStatus};
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("change.txt"), "review\n").unwrap();
    fixture.view.update(cx, |app, cx| {
        app.open_changes(&fixture.checkout, Some("change.txt".into()), cx);
    });
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        let app = fixture.view.read(cx);
        !app.review.opencode_loading
            && app
                .git_diff
                .contains_key(&(fixture.checkout.clone(), "change.txt".into()))
    })
    .await;
    fixture.view.update(cx, |app, cx| {
        app.review.destination = Some(muster_core::opencode::Destination::Opencode {
            session_id: "ses_review".into(),
        });
        app.review.opencode_rounds = vec![ReviewRound {
            id: "uncertain-round".into(),
            checkout_id: fixture.checkout.clone(),
            session_id: "ses_review".into(),
            status: RoundStatus::Unknown,
            marker: "muster-review:uncertain-round".into(),
            prompt: "review".into(),
            note_ids: Vec::new(),
            claim: String::new(),
            notes: Vec::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }];
        cx.notify();
    });
    assert!(draw_until(&fixture, cx, "recheck-unknown-opencode-round"));
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("resolve-opencode-round").label(),
            Some("Allow retry (may duplicate)")
        );
        assert_eq!(
            window.find("recheck-unknown-opencode-round").label(),
            Some("Check delivery again")
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// A review sent to Pi opens a terminal in the checkout the review is about,
/// with the review written to a private file Pi reads as its first prompt.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[gpui_kit::test]
async fn a_review_sent_to_pi_opens_a_terminal_in_the_checkout_it_is_about(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let app_dir = tempfile::tempdir().unwrap();
    let program_dir = tempfile::tempdir().unwrap();
    // A link called `pi`, so the program is one the operating system calls Pi.
    let program = program_dir.path().join("pi");
    std::os::unix::fs::symlink("/bin/sh", &program).unwrap();
    std::fs::write(fixture.root.join("change.txt"), "one\ntwo\n").unwrap();
    fixture.view.update(cx, |app, cx| {
        app.review.notes.insert(
            fixture.checkout.clone(),
            vec![muster_core::review::ReviewNote {
                id: "pi-note".into(),
                checkout_id: fixture.checkout.clone(),
                path: "change.txt".into(),
                side: muster_core::review::ReviewSide::New,
                line_start: 1,
                line_end: None,
                content: "This line could be clearer".into(),
                code: "one".into(),
                code_hash: muster_core::review::review_anchor_hash("one"),
                outdated: false,
            }],
        );
        app.open_changes(&fixture.checkout, Some("change.txt".into()), cx);
    });

    // The destination and the rounds are read before a send may use them.
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        !fixture.view.read(cx).review.opencode_loading
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.send_review_to_pi_with(program.clone(), app_dir.path().to_owned(), window, cx);
        });
    })
    .unwrap();

    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .terminals
            .values()
            .any(|terminal| terminal.checkout_id == fixture.checkout)
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(
            app.terminals.len(),
            1,
            "one send opens one terminal, and no more"
        );
        assert!(app.review.destination_error.is_none());
    });

    // The prompt is a private file under the app's own folder, and it holds
    // the review: it is what Pi reads before it says anything.
    let prompts = app_dir.path().join("pi/prompts");
    let written = std::fs::read_dir(&prompts)
        .expect("the prompts folder")
        .map(|entry| entry.expect("an entry").path())
        .collect::<Vec<_>>();
    assert_eq!(written.len(), 1, "one send writes one prompt");
    let prompt = std::fs::read_to_string(&written[0]).expect("the prompt reads");
    assert!(
        prompt.contains("This line could be clearer"),
        "the prompt is the review: {prompt}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&written[0]).unwrap().permissions().mode() & 0o777,
            0o600,
            "the prompt is readable by its owner only"
        );
        assert_eq!(
            std::fs::metadata(&prompts).unwrap().permissions().mode() & 0o777,
            0o700,
            "the folder is reachable by its owner only"
        );
    }
    close(&fixture, cx);
}

/// A send to Pi that cannot write the prompt opens no terminal and says why.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[gpui_kit::test]
async fn a_review_sent_to_pi_without_a_writable_prompt_folder_opens_no_terminal(
    cx: &mut TestAppContext,
) {
    let fixture = open_git(cx);
    let app_dir = tempfile::tempdir().unwrap();
    let program_dir = tempfile::tempdir().unwrap();
    let program = program_dir.path().join("pi");
    std::os::unix::fs::symlink("/bin/sh", &program).unwrap();
    // A file where the app's folder should be: the prompt cannot be written.
    let blocked = app_dir.path().join("app");
    std::fs::write(&blocked, "not a folder").unwrap();
    std::fs::write(fixture.root.join("change.txt"), "one\ntwo\n").unwrap();
    fixture.view.update(cx, |app, cx| {
        app.review.notes.insert(
            fixture.checkout.clone(),
            vec![muster_core::review::ReviewNote {
                id: "pi-note".into(),
                checkout_id: fixture.checkout.clone(),
                path: "change.txt".into(),
                side: muster_core::review::ReviewSide::New,
                line_start: 1,
                line_end: None,
                content: "This line could be clearer".into(),
                code: "one".into(),
                code_hash: muster_core::review::review_anchor_hash("one"),
                outdated: false,
            }],
        );
        app.open_changes(&fixture.checkout, Some("change.txt".into()), cx);
    });

    // The destination and the rounds are read before a send may use them.
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        !fixture.view.read(cx).review.opencode_loading
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.send_review_to_pi_with(program.clone(), blocked, window, cx);
        });
    })
    .unwrap();

    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture.view.read(cx).review.destination_error.is_some()
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        assert!(
            app.terminals.is_empty(),
            "a terminal that opened with nothing to read is a terminal that opened wrong"
        );
        assert!(
            app.review.destination_error.is_some(),
            "the failure is said out loud"
        );
    });
    close(&fixture, cx);
}

/// The prompts a send wrote, counted.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn written_prompts(prompts: &Path) -> usize {
    std::fs::read_dir(prompts)
        .map(|entries| entries.count())
        .unwrap_or(0)
}

/// A review sent to Pi whose program cannot start leaves no prompt behind.
///
/// The program is a file that is not a program, so the PTY cannot be opened:
/// the spawn fails rather than the program being missing from the path, which
/// is the failure a send has to survive. The prompt was written and no Pi
/// will read it, so it is removed rather than left holding somebody's review.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[gpui_kit::test]
async fn a_review_sent_to_pi_removes_the_prompt_when_the_program_cannot_start(
    cx: &mut TestAppContext,
) {
    let fixture = open_git(cx);
    let app_dir = tempfile::tempdir().unwrap();
    let program_dir = tempfile::tempdir().unwrap();
    // A file that is not a program: the operating system will not run it.
    let program = program_dir.path().join("pi");
    std::fs::write(&program, "not a program").unwrap();
    std::fs::write(fixture.root.join("change.txt"), "one\ntwo\n").unwrap();
    fixture.view.update(cx, |app, cx| {
        app.review.notes.insert(
            fixture.checkout.clone(),
            vec![muster_core::review::ReviewNote {
                id: "pi-note".into(),
                checkout_id: fixture.checkout.clone(),
                path: "change.txt".into(),
                side: muster_core::review::ReviewSide::New,
                line_start: 1,
                line_end: None,
                content: "This line could be clearer".into(),
                code: "one".into(),
                code_hash: muster_core::review::review_anchor_hash("one"),
                outdated: false,
            }],
        );
        app.open_changes(&fixture.checkout, Some("change.txt".into()), cx);
    });

    // The destination and the rounds are read before a send may use them.
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        !fixture.view.read(cx).review.opencode_loading
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.send_review_to_pi_with(program.clone(), app_dir.path().to_owned(), window, cx);
        });
    })
    .unwrap();

    // The terminal that did not open is said out loud.
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture.view.read(cx).toasts.latest_error().is_some()
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        assert!(
            app.terminals.is_empty(),
            "a terminal that opened with nothing to read is a terminal that opened wrong"
        );
    });
    let prompts = app_dir.path().join("pi/prompts");
    assert_eq!(
        written_prompts(&prompts),
        0,
        "a prompt no Pi will read is removed rather than left on disk"
    );
    close(&fixture, cx);
}

/// A review sent to Pi whose window goes away leaves no prompt behind.
///
/// The window is gone before the callback that would open the terminal runs,
/// so there is no terminal and nothing will read the prompt: it is removed
/// rather than left holding somebody's review.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[gpui_kit::test]
async fn a_review_sent_to_pi_whose_window_goes_away_removes_the_prompt(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let app_dir = tempfile::tempdir().unwrap();
    let program_dir = tempfile::tempdir().unwrap();
    let program = program_dir.path().join("pi");
    std::os::unix::fs::symlink("/bin/sh", &program).unwrap();
    std::fs::write(fixture.root.join("change.txt"), "one\ntwo\n").unwrap();
    fixture.view.update(cx, |app, cx| {
        app.review.notes.insert(
            fixture.checkout.clone(),
            vec![muster_core::review::ReviewNote {
                id: "pi-note".into(),
                checkout_id: fixture.checkout.clone(),
                path: "change.txt".into(),
                side: muster_core::review::ReviewSide::New,
                line_start: 1,
                line_end: None,
                content: "This line could be clearer".into(),
                code: "one".into(),
                code_hash: muster_core::review::review_anchor_hash("one"),
                outdated: false,
            }],
        );
        app.open_changes(&fixture.checkout, Some("change.txt".into()), cx);
    });

    // The destination and the rounds are read before a send may use them.
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        !fixture.view.read(cx).review.opencode_loading
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.send_review_to_pi_with(program.clone(), app_dir.path().to_owned(), window, cx);
        });
    })
    .unwrap();
    // The window goes away while the prompt is on its way. The callback that
    // would open the terminal has nowhere to run.
    cx.update_window(fixture.handle, |_, window, _| window.remove_window())
        .unwrap();

    // The prompt is written and then removed: the write is what creates the
    // folder, and a prompt no terminal will read does not stay in it.
    let prompts = app_dir.path().join("pi/prompts");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut settled = false;
    while std::time::Instant::now() < deadline {
        cx.executor().timer(Duration::from_millis(10)).await;
        settled = prompts.exists() && written_prompts(&prompts) == 0;
        if settled {
            break;
        }
    }
    assert!(
        settled,
        "a prompt no terminal will read is removed rather than left on disk"
    );
    fixture.view.update(cx, |app, _| app.owner.stop());
}

#[gpui_kit::test]
async fn review_toolbar_separates_the_title_from_the_selected_action(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("change.txt"), "one\ntwo\nthree\n").unwrap();
    fixture.view.update(cx, |app, cx| {
        app.open_changes(&fixture.checkout, Some("change.txt".into()), cx)
    });
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        let app = fixture.view.read(cx);
        !app.review.opencode_loading
            && app
                .git_diff
                .contains_key(&(fixture.checkout.clone(), "change.txt".into()))
    })
    .await;
    fixture.view.update(cx, |app, cx| {
        app.navigation.panels_visible = false;
        cx.notify();
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.resize(gpui_kit::size(px(600.), px(800.)));
        window.bounds_changed(cx);
    })
    .unwrap();
    assert!(draw_until(&fixture, cx, "review-primary-action"));
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("review-primary-action").label(), Some("Save"));
        for id in [
            "review-storage",
            "clear-review-exports",
            "send-review-opencode",
            "export-review",
            "review-outdated",
        ] {
            assert!(
                window.try_find(id).is_none(),
                "{id} should not crowd the toolbar"
            );
        }
        let toolbar = window.find("review-title-row").bounds();
        let action = window.find("review-primary-action").bounds();
        let chooser = window.find("review-destination").bounds();
        let controls = [
            action,
            chooser,
            window.find("diff-open-source-change.txt").bounds(),
            window.find("close-diff").bounds(),
        ];
        for control in controls {
            assert!(control.top() >= toolbar.top() && control.bottom() <= toolbar.bottom());
            assert!(control.left() >= px(0.) && control.right() <= px(600.));
        }
        // The chooser is the right half of the same button, not a control beside
        // it: no gap between them, and both halves within one split's bounds.
        assert_eq!(action.right(), chooser.left());
        assert_eq!(action.top(), chooser.top());
        assert_eq!(action.bottom(), chooser.bottom());
        let split = window.find("review-export").bounds();
        assert_eq!(split.left(), action.left());
        assert_eq!(split.right(), chooser.right());
        // The chooser half carries no text of its own any more — it is the
        // caret, and it says what it is rather than which destination is chosen.
        assert_eq!(
            window.find("review-destination").label(),
            Some("Review destination")
        );
        assert!(action.left() > toolbar.left());
        window.click("review-primary-action", cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert!(!app.review.exporting && !app.review.destination_busy);
        assert!(app.review.opencode_rounds.is_empty());
    });
    // The caret half is the destination menu's trigger, so opening it is what
    // makes the split's other half a chooser at all.
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("review-destination", cx);
    })
    .unwrap();
    assert!(draw_until(&fixture, cx, "popup-menu"));
    fixture.view.update(cx, |app, cx| {
        app.review.destination = Some(muster_core::opencode::Destination::OpencodeNew);
        cx.notify();
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("review-primary-action").label(),
            Some("Send to OpenCode")
        );
        assert!(window.try_find("copy-opencode-session-id").is_none());
    })
    .unwrap();
    close(&fixture, cx);
}

/// Where the notes went is a row beside the button that put them there, not a
/// message that was true for a moment: the file is the answer to "did my Save do
/// anything", and it is the thing somebody hands to a teammate, so it is copyable.
#[gpui_kit::test]
async fn a_saved_review_names_the_file_it_wrote_and_copies_that_path(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("change.txt"), "one\ntwo\n").unwrap();
    fixture.view.update(cx, |app, cx| {
        app.open_changes(&fixture.checkout, Some("change.txt".into()), cx)
    });
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        let app = fixture.view.read(cx);
        !app.review.opencode_loading
            && app
                .git_diff
                .contains_key(&(fixture.checkout.clone(), "change.txt".into()))
    })
    .await;
    let path = fixture.root.join(".muster/reviews/2026-03-14-1532.md");
    let written = path.display().to_string();
    fixture.view.update(cx, |app, cx| {
        app.review.exported = Some(crate::review::ExportedReview {
            checkout_id: fixture.checkout.clone(),
            path,
        });
        cx.notify();
    });
    assert!(draw_until(&fixture, cx, "review-exported-path"));
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.find("review-exported-path").visible(),
            "the file the save wrote is on screen beside the button that saved it"
        );
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
            "something else".to_owned(),
        ));
        window.click("copy-review-export-path", cx);
        assert_eq!(
            cx.read_from_clipboard().map(|item| item.text()),
            Some(Some(written.clone())),
            "the icon beside the path is what puts it on the clipboard"
        );
    })
    .unwrap();
    // The file outlives the diff it came from, so a save for another checkout is
    // not this change set's file and is not named by this toolbar.
    fixture.view.update(cx, |app, cx| {
        app.review.exported = Some(crate::review::ExportedReview {
            checkout_id: "checkout:/somewhere/else".into(),
            path: std::path::PathBuf::from("/elsewhere/2026-03-14-1532.md"),
        });
        cx.notify();
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("review-exported-path").is_none());
    })
    .unwrap();
    close(&fixture, cx);
}

/// The path the diff's title names is a path somebody pastes somewhere else, so
/// the icon beside it puts that very path on the clipboard.
#[gpui_kit::test]
async fn the_diff_title_copies_the_path_it_names(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("change.txt"), "one\ntwo\n").unwrap();
    fixture.view.update(cx, |app, cx| {
        app.open_changes(&fixture.checkout, Some("change.txt".into()), cx)
    });
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_diff
            .contains_key(&(fixture.checkout.clone(), "change.txt".into()))
    })
    .await;
    assert!(draw_until(&fixture, cx, "copy-diff-path"));
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
            "something else".to_owned(),
        ));
        window.click("copy-diff-path", cx);
        assert_eq!(
            cx.read_from_clipboard().map(|item| item.text()),
            Some(Some("change.txt".to_owned())),
            "the icon beside the path is what puts it on the clipboard"
        );
    })
    .unwrap();
    // A whole change set names no file, so there is no path to copy.
    fixture
        .view
        .update(cx, |app, cx| app.open_changes(&fixture.checkout, None, cx));
    assert!(!draw_until(&fixture, cx, "copy-diff-path"));
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn review_composer_keeps_text_while_widening_and_escape_cancels(cx: &mut TestAppContext) {
    use gpui_kit::component::diff::{DiffLineRange, DiffSide};
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("change.txt"), "one\ntwo\nthree\n").unwrap();
    fixture.view.update(cx, |app, cx| {
        app.open_changes(&fixture.checkout, Some("change.txt".into()), cx)
    });
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_diff
            .contains_key(&(fixture.checkout.clone(), "change.txt".into()))
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        // The `+` is a control on a drawn diff, so the diff is on screen before a
        // note opens: the lines the note covers are washed in that view.
        window.render_frame(cx);
        fixture.view.update(cx, |app, cx| {
            app.begin_review_note(
                &DiffLineRange::new("change.txt", DiffSide::Modified, 3, 3),
                window,
                cx,
            )
        });
        window.render_frame(cx);
    })
    .unwrap();
    assert!(draw_until(&fixture, cx, "save-review-note"));
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        // The composer is one row of the diff: the field, the range row and
        // the footer, with Legacy's padding inside the box and none of it
        // around it. A card that grows past this has picked up margin again.
        let card = window.find("review-composer-card").bounds();
        assert!(card.size.height <= px(124.), "{card:?}");
        assert!(window.find("save-review-note").bounds().size.height <= px(24.));
        // Legacy's order: the range label, then the picker after it, and the
        // picker is the size of a line number rather than of a field.
        let picker = window.find("review-range-start").bounds();
        assert!(
            picker.size.height <= px(24.) && picker.size.width <= px(60.),
            "the picker is compact: {picker:?}"
        );
        // A range nobody can see is a range about the wrong lines, so the draft
        // marks the lines it covers with the diff's own selection.
        fixture.view.read_with(cx, |app, cx| {
            let state = &app.diff_view.as_ref().unwrap().state;
            assert_eq!(
                state.read(cx).selected_lines(),
                Some(DiffLineRange::new("change.txt", DiffSide::Modified, 3, 3))
            );
        });
        // An empty note is not a note: saving it is refused, and refusing it
        // leaves the draft and the lines it covers alone.
        window.click("save-review-note", cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert!(app.review.composer.is_some());
        assert!(app
            .review
            .notes
            .get(&fixture.checkout)
            .is_none_or(Vec::is_empty));
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        let input = fixture
            .view
            .read(cx)
            .review
            .composer
            .as_ref()
            .unwrap()
            .input
            .clone();
        input.update(cx, |input, cx| {
            input.set_value("Keep this comment", window, cx)
        });
        window.render_frame(cx);
        let card = window.find("review-composer-card").bounds();
        // A `+` clicked above the note being written says where its range starts
        // rather than opening another one: the draft is about the same change,
        // so what has been written stays written.
        fixture.view.update(cx, |app, cx| {
            app.begin_review_note(
                &DiffLineRange::new("change.txt", DiffSide::Modified, 1, 1),
                window,
                cx,
            )
        });
        fixture.view.read_with(cx, |app, cx| {
            let composer = app.review.composer.as_ref().unwrap();
            assert_eq!(
                composer.input.read(cx).value().as_str(),
                "Keep this comment"
            );
            assert_eq!(composer.code, "one\ntwo\nthree");
            assert_eq!((composer.line_start, composer.line_end), (1, 3));
        });
        window.render_frame(cx);
        // The box hangs on the line the `+` was clicked on, so choosing another
        // start moves the wash over the lines instead of dragging the box up the
        // diff and away from the caret. A range says one sentence more about
        // itself, so the card may grow downward; where it *is* cannot change.
        let widened = window.find("review-composer-card").bounds();
        assert_eq!(widened.origin, card.origin);
        assert!(
            widened.size.height >= card.size.height,
            "{widened:?} vs {card:?}"
        );
        fixture.view.read_with(cx, |app, cx| {
            let state = &app.diff_view.as_ref().unwrap().state;
            assert_eq!(
                state.read(cx).selected_lines(),
                Some(DiffLineRange::new("change.txt", DiffSide::Modified, 1, 3))
            );
        });
        window.click("save-review-note", cx);
        // A saved note leaves its lines washed on the diff: the mark is what
        // says which lines the comment is about.
        fixture.view.read_with(cx, |app, cx| {
            let state = &app.diff_view.as_ref().unwrap().state;
            assert_eq!(
                state.read(cx).selected_lines(),
                Some(DiffLineRange::new("change.txt", DiffSide::Modified, 1, 3))
            );
        });
        let note_id = fixture.view.update(cx, |app, cx| {
            let note = &mut app.review.notes.get_mut(&fixture.checkout).unwrap()[0];
            assert_eq!(note.code, "one\ntwo\nthree");
            assert_eq!((note.line_start, note.line_end), (1, Some(3)));
            note.outdated = true;
            note.code_hash = "accepted-anchor".into();
            let id = note.id.clone();
            app.edit_review_note(&id, window, cx);
            id
        });
        let input = fixture
            .view
            .read(cx)
            .review
            .composer
            .as_ref()
            .unwrap()
            .input
            .clone();
        input.update(cx, |input, cx| {
            input.set_value("Edited comment", window, cx)
        });
        // ⌘↵ finishes the note being edited, and is stopped before the input's own
        // Return, so the saved text carries no newline of its own.
        window.dispatch_keystroke(gpui_kit::Keystroke::parse("secondary-enter").unwrap(), cx);
        fixture.view.update(cx, |app, cx| {
            let note = &app.review.notes[&fixture.checkout][0];
            assert!(
                note.outdated,
                "editing prose must not accept a drifted anchor"
            );
            assert_eq!(note.code_hash, "accepted-anchor");
            assert_eq!(note.content, "Edited comment");
            app.edit_review_note(&note_id, window, cx);
        });
        // A note being edited marks the lines it was saved against.
        fixture.view.read_with(cx, |app, cx| {
            let state = &app.diff_view.as_ref().unwrap().state;
            assert_eq!(
                state.read(cx).selected_lines(),
                Some(DiffLineRange::new("change.txt", DiffSide::Modified, 1, 3))
            );
        });
        window.render_frame(cx);
        window.dispatch_keystroke(gpui_kit::Keystroke::parse("escape").unwrap(), cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, cx| {
        assert!(app.review.composer.is_none());
        // A cancelled draft gives the mark back to the note it came from: the
        // note that was applied last still says which lines it is about.
        let state = &app.diff_view.as_ref().unwrap().state;
        assert_eq!(
            state.read(cx).selected_lines(),
            Some(DiffLineRange::new("change.txt", DiffSide::Modified, 1, 3)),
            "a cancelled draft leaves the applied note's lines marked"
        );
    });
    close(&fixture, cx);
}

/// A saved note's actions are two icons on its own header row, and the cross asks.
///
/// The icons are named rather than labelled, so what is pinned here is the naming
/// and what the two of them do: the pencil opens the same composer a new note
/// does, with the saved text in it, and the cross keeps the note until the
/// question it asked has been answered.
#[gpui_kit::test]
async fn a_saved_note_is_edited_and_deleted_from_two_icons(cx: &mut TestAppContext) {
    use gpui_kit::component::diff::{DiffLineRange, DiffSide};
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("change.txt"), "one\ntwo\nthree\n").unwrap();
    fixture.view.update(cx, |app, cx| {
        app.open_changes(&fixture.checkout, Some("change.txt".into()), cx)
    });
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_diff
            .contains_key(&(fixture.checkout.clone(), "change.txt".into()))
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        fixture.view.update(cx, |app, cx| {
            app.begin_review_note(
                &DiffLineRange::new("change.txt", DiffSide::Modified, 3, 3),
                window,
                cx,
            )
        });
        let input = fixture
            .view
            .read(cx)
            .review
            .composer
            .as_ref()
            .unwrap()
            .input
            .clone();
        input.update(cx, |input, cx| {
            input.set_value("Keep this comment", window, cx)
        });
        window.render_frame(cx);
        window.click("save-review-note", cx);
    })
    .unwrap();
    // A saved note leaves its lines washed on the diff: the mark is what says
    // which lines the comment is about.
    fixture.view.read_with(cx, |app, cx| {
        let state = &app.diff_view.as_ref().unwrap().state;
        assert_eq!(
            state.read(cx).selected_lines(),
            Some(DiffLineRange::new("change.txt", DiffSide::Modified, 3, 3))
        );
    });
    let note_id = fixture.view.read_with(cx, |app, _| {
        app.review
            .notes
            .get(&fixture.checkout)
            .and_then(|notes| notes.first())
            .expect("a saved note")
            .id
            .clone()
    });
    let card = format!("review-card-{note_id}");
    let edit = format!("edit-review-{note_id}");
    let delete = format!("delete-review-{note_id}");
    assert!(draw_until(&fixture, cx, &edit));
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        // Both are icons: a name instead of a word, on the card's own header row
        // rather than in a row of their own below the note.
        let header = window.find(card.clone()).bounds();
        for (id, name) in [
            (edit.clone(), "Edit this note"),
            (delete.clone(), "Delete this note"),
        ] {
            let icon = window.find(id.clone());
            // An icon is named rather than worded: the name is what a screen reader
            // is given and what the tooltip says, and the control itself is a
            // small square rather than a word with padding around it.
            assert_eq!(icon.label(), Some(name));
            let bounds = icon.bounds();
            assert!(
                bounds.size.width <= px(28.) && bounds.size.height <= px(28.),
                "{id} is a square: {bounds:?}"
            );
            assert!(bounds.right() <= header.right() && bounds.top() >= header.top());
        }
        assert!(
            window.find(delete.clone()).bounds().left()
                > window.find(edit.clone()).bounds().right()
        );
        window.click(edit, cx);
        window.render_frame(cx);
        assert!(window.try_find("review-composer-card").is_some());
    })
    .unwrap();
    fixture.view.read_with(cx, |app, cx| {
        let composer = app.review.composer.as_ref().expect("the same composer");
        assert_eq!(composer.editing_id.as_deref(), Some(note_id.as_str()));
        assert_eq!(
            composer.input.read(cx).value().as_str(),
            "Keep this comment"
        );
        assert!(
            composer.start_picker.read(cx).selected_value().is_some(),
            "the range the note was saved against is still editable"
        );
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.dispatch_keystroke(gpui_kit::Keystroke::parse("escape").unwrap(), cx);
        window.render_frame(cx);
        window.click(delete.clone(), cx);
        window.render_frame(cx);
        assert!(window.try_find("confirm-delete-review-note").is_some());
        window.click("cancel-delete-review-note", cx);
        window.render_frame(cx);
        assert!(window.try_find("confirm-delete-review-note").is_none());
        window.click(delete, cx);
        window.render_frame(cx);
        window.click("confirm-delete-review-note", cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, cx| {
        assert!(
            app.review
                .notes
                .get(&fixture.checkout)
                .is_none_or(Vec::is_empty),
            "a cross that was not answered leaves the note alone, and an answered one removes it"
        );
        // A note that is gone takes its lines with it: the mark belongs to the
        // note, so a note that is not there owns nothing.
        assert!(
            app.diff_view
                .as_ref()
                .unwrap()
                .state
                .read(cx)
                .selected_lines()
                .is_none(),
            "a deleted note leaves no lines marked behind it"
        );
    });
    close(&fixture, cx);
}

/// The picker is the draft's own control for where its range starts.
///
/// It is the toolkit's select over the line numbers the range could start on, so
/// what it confirms is a line number and the window routes that straight back
/// into the draft: one state, rather than a control and a range that can
/// disagree. The confirmation is emitted rather than clicked, because a dropdown
/// has no row to hit in a headless window.
#[gpui_kit::test]
async fn review_range_picker_moves_the_start_line(cx: &mut TestAppContext) {
    use gpui_kit::component::diff::{DiffLineRange, DiffSide};
    use gpui_kit::component::select::SelectEvent;
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("change.txt"), "one\ntwo\nthree\n").unwrap();
    fixture.view.update(cx, |app, cx| {
        app.open_changes(&fixture.checkout, Some("change.txt".into()), cx)
    });
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_diff
            .contains_key(&(fixture.checkout.clone(), "change.txt".into()))
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        // The `+` is a control on a drawn diff, so the diff is on screen before
        // the note opens: the lines it covers are washed in that view.
        window.render_frame(cx);
        fixture.view.update(cx, |app, cx| {
            app.begin_review_note(
                &DiffLineRange::new("change.txt", DiffSide::Modified, 3, 3),
                window,
                cx,
            )
        });
        window.render_frame(cx);
    })
    .unwrap();
    assert!(draw_until(&fixture, cx, "review-range-start"));
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        // The picker names itself rather than being read as its current value,
        // and it follows the range label, which is the order Legacy draws them
        // in: what the range is, then the one thing about it that is a choice.
        let picker = window.find("review-range-start").bounds();
        let range = window.find("review-composer-range").bounds();
        assert_eq!(
            window.find("review-range-start").label(),
            Some("First line of the range")
        );
        assert!(
            range.right() <= picker.left(),
            "the range label comes before the picker: {range:?} then {picker:?}"
        );
        assert!(picker.left() - range.right() <= px(8.));
        let card = window.find("review-composer-card").bounds();
        let save = window.find("save-review-note").bounds();
        let cancel = window.find("cancel-review-note").bounds();
        assert!(cancel.right() <= save.left());
        assert!(card.right() - save.right() <= px(12.));
        let select = fixture
            .view
            .read(cx)
            .review
            .composer
            .as_ref()
            .unwrap()
            .start_picker
            .clone();
        select.update(cx, |_, cx| {
            cx.emit(SelectEvent::Confirm(Some(gpui_kit::SharedString::from(
                "+2",
            ))))
        });
        window.render_frame(cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, cx| {
        let composer = app.review.composer.as_ref().unwrap();
        assert_eq!((composer.line_start, composer.line_end), (2, 3));
        assert_eq!(composer.code, "two\nthree");
        // The picker is the draft's control for this line, so it is told what the
        // draft now says rather than left to agree with it by itself.
        assert_eq!(
            composer.start_picker.read(cx).selected_value().cloned(),
            Some(gpui_kit::SharedString::from("+2"))
        );
        // The lines the note now covers are the ones it will be saved against.
        let state = &app.diff_view.as_ref().unwrap().state;
        assert_eq!(
            state.read(cx).selected_lines(),
            Some(DiffLineRange::new("change.txt", DiffSide::Modified, 2, 3))
        );
    });
    close(&fixture, cx);
}

/// A note can be written about a line the change removed, not only about one it
/// added: the `−` side is a side, and the range moves there the same way.
#[gpui_kit::test]
async fn a_note_can_cover_lines_the_change_removed(cx: &mut TestAppContext) {
    use gpui_kit::component::diff::{DiffLineRange, DiffSide};
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("change.txt"), "one\ntwo\nthree\n").unwrap();
    commit_all(&fixture.root, "base");
    std::fs::write(fixture.root.join("change.txt"), "one\ntwo\nTHREE\n").unwrap();
    fixture.view.update(cx, |app, cx| {
        app.open_changes(&fixture.checkout, Some("change.txt".into()), cx)
    });
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_diff
            .contains_key(&(fixture.checkout.clone(), "change.txt".into()))
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        // The `+` is a control on a drawn diff, so the diff is on screen before
        // the note opens: the lines it covers are washed in that view.
        window.render_frame(cx);
        fixture.view.update(cx, |app, cx| {
            app.begin_review_note(
                &DiffLineRange::new("change.txt", DiffSide::Original, 3, 3),
                window,
                cx,
            )
        });
        window.render_frame(cx);
    })
    .unwrap();
    assert!(draw_until(&fixture, cx, "review-range-start"));
    let clicked_anchor_y = cx
        .update_window(fixture.handle, |_, window, cx| {
            let input = fixture
                .view
                .read(cx)
                .review
                .composer
                .as_ref()
                .unwrap()
                .input
                .clone();
            input.update(cx, |input, cx| {
                input.set_value("this line goes away", window, cx)
            });
            window.render_frame(cx);
            let clicked_anchor_y = window.find("review-composer-card").bounds().center().y;
            // The removed line above it is a start this note may be widened to, and
            // widening says the removed side rather than the added one.
            fixture.view.update(cx, |app, cx| {
                app.set_review_note_start(muster_core::review::ReviewSide::Old, 2, window, cx)
            });
            window.render_frame(cx);
            assert!(
                (window.find("review-composer-card").bounds().center().y - clicked_anchor_y).abs()
                    <= px(2.),
                "changing the range start must not move the composer off the clicked -3 line"
            );
            clicked_anchor_y
        })
        .unwrap();
    fixture.view.read_with(cx, |app, cx| {
        let composer = app.review.composer.as_ref().unwrap();
        assert_eq!(composer.side, muster_core::review::ReviewSide::Old);
        assert_eq!((composer.line_start, composer.line_end), (2, 3));
        assert_eq!(composer.code, "two\nthree");
        // The picker speaks the export's language: a removed line is a negative
        // number, which is what says the note is about the old side.
        assert_eq!(
            composer.start_picker.read(cx).selected_value().cloned(),
            Some(gpui_kit::SharedString::from("-2"))
        );
        let state = &app.diff_view.as_ref().unwrap().state;
        assert_eq!(
            state.read(cx).selected_lines(),
            Some(DiffLineRange::new("change.txt", DiffSide::Original, 2, 3))
        );
        // And the note is saved against the removed side, which is what the
        // export writes for it.
    });
    fixture.view.update(cx, |app, cx| app.save_review_note(cx));
    let saved_anchor_y = cx
        .update_window(fixture.handle, |_, window, cx| {
            window.render_frame(cx);
            let note_id = fixture.view.read_with(cx, |app, _| {
                app.review.notes[&fixture.checkout][0].id.clone()
            });
            window
                .find(format!("review-card-{note_id}"))
                .bounds()
                .center()
                .y
        })
        .unwrap();
    assert!(
        (saved_anchor_y - clicked_anchor_y).abs() <= px(2.),
        "the saved note stays at the clicked -3 line, not its selected -2 start"
    );
    fixture.view.read_with(cx, |app, _| {
        let note = &app.review.notes[&fixture.checkout][0];
        assert_eq!(note.side, muster_core::review::ReviewSide::Old);
        assert_eq!((note.line_start, note.line_end), (2, Some(3)));
        assert_eq!(note.code, "two\nthree");
        assert_eq!(note.content, "this line goes away");
    });
    close(&fixture, cx);
}

/// A note opened on a line the change added may still start on a line it removed
/// above it: the removal is part of the same change, so the range covers it and
/// the note is saved against the side the start is on.
#[gpui_kit::test]
async fn a_note_on_an_added_line_may_start_on_a_removed_one_above_it(cx: &mut TestAppContext) {
    use gpui_kit::component::diff::{DiffLineRange, DiffSide};
    use gpui_kit::component::select::SelectEvent;
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("change.txt"), "one\ntwo\nthree\n").unwrap();
    commit_all(&fixture.root, "base");
    std::fs::write(fixture.root.join("change.txt"), "one\nTWO\nTHREE\n").unwrap();
    fixture.view.update(cx, |app, cx| {
        app.open_changes(&fixture.checkout, Some("change.txt".into()), cx)
    });
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_diff
            .contains_key(&(fixture.checkout.clone(), "change.txt".into()))
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        // The `+` is a control on a drawn diff, so the diff is on screen before
        // the note opens: the lines it covers are washed in that view.
        window.render_frame(cx);
        fixture.view.update(cx, |app, cx| {
            app.begin_review_note(
                &DiffLineRange::new("change.txt", DiffSide::Modified, 3, 3),
                window,
                cx,
            )
        });
        window.render_frame(cx);
    })
    .unwrap();
    assert!(draw_until(&fixture, cx, "review-range-start"));
    cx.update_window(fixture.handle, |_, window, cx| {
        let input = fixture
            .view
            .read(cx)
            .review
            .composer
            .as_ref()
            .unwrap()
            .input
            .clone();
        input.update(cx, |input, cx| {
            input.set_value("this line replaces two", window, cx)
        });
        let picker = fixture
            .view
            .read(cx)
            .review
            .composer
            .as_ref()
            .unwrap()
            .start_picker
            .clone();
        // The removed line above the added one is a start this note may be
        // widened to, and the picker says so in the export's own language.
        picker.update(cx, |_, cx| {
            cx.emit(SelectEvent::Confirm(Some(gpui_kit::SharedString::from(
                "-2",
            ))))
        });
        window.render_frame(cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, cx| {
        let composer = app.review.composer.as_ref().unwrap();
        assert_eq!(composer.side, muster_core::review::ReviewSide::Old);
        assert_eq!((composer.line_start, composer.line_end), (2, 3));
        assert_eq!(composer.code, "two\nthree");
        // The card does not travel with the range: the comment is still being
        // written where it was opened.
        assert_eq!(composer.anchor, (muster_core::review::ReviewSide::New, 3));
        assert_eq!(
            composer.start_picker.read(cx).selected_value().cloned(),
            Some(gpui_kit::SharedString::from("-2"))
        );
        // The lines the note now covers are the lines it is saved against.
        let state = &app.diff_view.as_ref().unwrap().state;
        assert_eq!(
            state.read(cx).selected_lines(),
            Some(DiffLineRange::new("change.txt", DiffSide::Original, 2, 3))
        );
    });
    fixture.view.update(cx, |app, cx| app.save_review_note(cx));
    fixture.view.read_with(cx, |app, _| {
        let note = &app.review.notes[&fixture.checkout][0];
        assert_eq!(note.side, muster_core::review::ReviewSide::Old);
        assert_eq!((note.line_start, note.line_end), (2, Some(3)));
        assert_eq!(note.code, "two\nthree");
    });
    close(&fixture, cx);
}

/// A window with nothing in it, for a handler that only needs somewhere to be
/// called from.
struct Empty;

impl Render for Empty {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui_kit::IntoElement {
        div().size_full()
    }
}

/// A listed entry, for a tree built by hand rather than read from a filesystem.
fn entry_at(name: &str, _depth: usize, directory: bool) -> muster_core::files::FileEntry {
    muster_core::files::FileEntry {
        name: name.into(),
        path: name.into(),
        directory,
    }
}
