use super::*;
use crate::app::sidebar_tests::{close, open_git, GitFixture};
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::TestAppContext;

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
        window.click("close-diff", cx);
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
