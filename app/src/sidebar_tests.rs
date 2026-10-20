//! Integration tests draw the production sidebar and dispatch real pointer/key events.

use std::process::Command;

use super::*;
use crate::sidebar::{HEADING_NAME_SIZE, HEADING_PATH_SIZE};
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{
    AnyWindowHandle, App, InputEvent, MouseButton, MouseDownEvent, MouseExitEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, TestAppContext, Window,
};
use tempfile::TempDir;
#[path = "inspector_tests.rs"]
mod inspector_tests;

fn confirmation_open(handle: AnyWindowHandle, cx: &mut TestAppContext) -> bool {
    use gpui_kit::component::WindowExt;
    assert!(
        !cx.has_pending_prompt(),
        "confirmations must not use platform prompts"
    );
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let open = window.has_active_dialog(cx);
        assert_eq!(open, window.try_find("dialog").is_some());
        open
    })
    .unwrap()
}

fn answer_confirmation(handle: AnyWindowHandle, label: &str, cx: &mut TestAppContext) {
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let choice = (0..3usize)
            .find(|choice| {
                window
                    .within("dialog")
                    .try_find(("confirmation-choice", *choice))
                    .is_some_and(|button| button.label() == Some(label))
            })
            .expect("the confirmation offers this action");
        window
            .within("dialog")
            .click(("confirmation-choice", choice), cx);
    })
    .unwrap();
}

fn run_git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A real Git repository with one commit, so a worktree can actually be made.
///
/// The worktree dialog's whole subject is Git's answer, and a fixture made of
/// a plain directory would answer nothing: there is no default branch to
/// resolve, no `.worktrees` to write, and no branch to take.
fn git_repository(base: &Path) -> PathBuf {
    let root = base.join("repo");
    std::fs::create_dir(&root).unwrap();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .current_dir(&root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "--quiet", "--initial-branch=main"]);
    git(&[
        "-c",
        "user.email=muster@example.test",
        "-c",
        "user.name=Muster",
        "commit",
        "--quiet",
        "--allow-empty",
        "-m",
        "first",
    ]);
    root
}

/// A session the service reported protects a worktree that has no terminal of its own.
///
/// The session may have been started outside this app: the service is the only thing
/// that knows about it, and where a foreground program was started is not where an
/// agent is working.
#[gpui_kit::test]
fn a_session_the_service_reported_protects_a_worktree_with_no_terminal_of_its_own(
    cx: &mut TestAppContext,
) {
    use muster_core::opencode::observe::AgentSession;
    let fixture = open_git(cx);
    let (id, _path) = add_worktree(&fixture, cx);
    fixture.view.update(cx, |app, _| {
        let directory = app
            .navigation
            .checkout(&id)
            .expect("the worktree is registered")
            .canonical_path
            .clone();
        app.agent.sessions = vec![AgentSession {
            id: "ses_a".into(),
            title: "work".into(),
            running: true,
            awaiting_reply: Some(false),
            directory: Some(PathBuf::from(directory)),
            ..AgentSession::default()
        }];
    });
    fixture.view.read_with(cx, |app, _| {
        assert!(
            app.knows_active_opencode_session(&id),
            "a session started outside this app protects the worktree"
        );
    });
    close(&fixture, cx);
}

/// A terminal running OpenCode says what its session is doing, and who is doing it.
///
/// The row is a read of what the service says, and the three states it can be in are
/// the ones the panel has a shape for: working, waiting for an answer, and failed.
#[gpui_kit::test]
fn an_opencode_row_says_what_its_session_is_doing(cx: &mut TestAppContext) {
    use muster_core::opencode::observe::{AgentAgent, AgentSession};
    let fixture = open_git(cx);
    add_terminal(&fixture, cx);
    fixture.view.update(cx, |app, _| {
        app.agent.sessions = vec![AgentSession {
            id: "ses_a".into(),
            title: "work".into(),
            agent: Some("build".into()),
            running: true,
            awaiting_reply: Some(false),
            updated_at: 1_000,
            ..AgentSession::default()
        }];
        app.agent.catalogs.insert(
            fixture.checkout.clone(),
            vec![AgentAgent {
                id: "build".into(),
                name: "Build".into(),
                mode: "primary".into(),
                color: Some("#5c7cfa".into()),
                hidden: false,
            }],
        );
    });
    fixture.view.read_with(cx, |app, _| {
        let state = app.opencode_row_state(&fixture.checkout, Some("OC | work"));
        assert!(
            state.working,
            "a turn that is running is the state that moves"
        );
        assert_eq!(state.label.as_deref(), Some("Build"));
        assert_eq!(state.tint.as_deref(), Some("#5c7cfa"));
        assert_eq!(state.note, "working");
        assert_eq!(
            state.elapsed,
            Some(((muster_core::opencode::now_millis() - 1_000).max(0) / 1000) as u64)
        );
    });

    // A title that names two sessions names neither of them.
    fixture.view.update(cx, |app, _| {
        app.agent
            .sessions
            .push(muster_core::opencode::observe::AgentSession {
                id: "ses_b".into(),
                title: "work".into(),
                awaiting_reply: Some(false),
                ..Default::default()
            });
    });
    fixture.view.read_with(cx, |app, _| {
        let state = app.opencode_row_state(&fixture.checkout, Some("OC | work"));
        assert!(state.unavailable);
        assert_eq!(state.note, "several sessions with this name");
        assert!(!state.working);
    });
    close(&fixture, cx);
}

/// A terminal running OpenCode says what its session is doing, and who is doing it.
#[test]
fn a_task_name_suggests_a_branch_only_while_the_person_has_not_edited_it() {
    assert_eq!(TerminalWindow::slug_for("Fix login"), "Fix-login");
    assert_eq!(TerminalWindow::slug_for("  a//b  "), "a-b");
    assert_eq!(TerminalWindow::slug_for("..."), "new-task");
    assert_eq!(TerminalWindow::slug_for(".hidden/x"), "hidden-x");
}

#[gpui_kit::test]
fn the_sidebar_bundles_truetype_faces_and_shapes_fitted_labels(cx: &mut TestAppContext) {
    for bytes in [
        include_bytes!("../assets/fonts/FiraCode-Regular.ttf").as_slice(),
        include_bytes!("../assets/fonts/FiraCode-Bold.ttf").as_slice(),
    ] {
        assert_eq!(&bytes[..4], b"\x00\x01\x00\x00", "expected a TrueType face");
    }

    // The measurement the repo path's fit runs on, in the family the sidebar
    // draws with: shaped, not a character count, and growing with the size.
    let fixture = open(cx);
    cx.update_window(fixture.handle, |_, window, _| {
        assert!(sidebar_text_width(window, "MMMM", 11.) > sidebar_text_width(window, "MM", 11.));
        let root = "/home/person/workspace/deeply/nested/checkout";
        assert!(sidebar_text_width(window, root, 11.) > sidebar_text_width(window, root, 10.));
        assert_eq!(
            sidebar_repo_path(window, root, "checkout", 500., false),
            root
        );
        let branch = "feature/sidebar/weigh-a-label";
        assert_eq!(sidebar_name(window, branch, 1000.), branch);
        let tail = "…/weigh-a-label";
        assert_eq!(
            sidebar_name(
                window,
                branch,
                sidebar_text_width(window, tail, 13.).as_f32()
            ),
            tail
        );
        assert_eq!(sidebar_name(window, branch, 0.), tail);
        assert_eq!(sidebar_name(window, "shell", 0.), "shell");
        assert_eq!(
            sidebar_name(window, "feature//unicode/日本語", 0.),
            "…/日本語"
        );
        let elided = sidebar_repo_path(window, root, "checkout", 200., false);
        assert!(
            elided.starts_with('…') && elided.len() < root.len(),
            "a path wider than the panel kept every segment: {elided}"
        );
    })
    .unwrap();
    dispose(&fixture, cx);
}

#[test]
fn sidebar_drag_scroll_speed_grows_toward_the_edge_and_is_bounded() {
    assert_eq!(sidebar_scroll_step(1.), 2.);
    assert_eq!(sidebar_scroll_step(12.), 6.);
    assert_eq!(sidebar_scroll_step(36.), 14.);
    assert_eq!(sidebar_scroll_step(-1.), -2.);
    assert_eq!(sidebar_scroll_step(-36.), -14.);
}

#[gpui_kit::test]
fn rename_limits_typed_names_and_preserves_unicode(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let id = fixture.sessions[0].clone();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.double_click(format!("select-session-{id}"), cx);
        window.render_frame(cx);
        assert!(window.try_find(format!("select-session-{id}")).is_none());
        window.click(format!("rename-session-{id}"), cx);
        assert!(fixture.view.read_with(cx, |app, _| app.editing.is_some()));
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-a"
            } else {
                "ctrl-a"
            },
            cx,
        );
        window.input(&"a".repeat(61), cx);
        fixture.view.read_with(cx, |app, cx| {
            assert_eq!(
                app.editing
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .value()
                    .as_ref(),
                "a".repeat(60)
            );
        });
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-a"
            } else {
                "ctrl-a"
            },
            cx,
        );
        window.input(&"é".repeat(60), cx);
        window.press("enter", cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(app.navigation.session(&id).unwrap().name, "é".repeat(60));
    });
    dispose(&fixture, cx);
}

#[gpui_kit::test]
fn keyboard_focus_on_a_session_reveals_its_sibling_close_button(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let id = &fixture.sessions[0];
    cx.update_window(fixture.handle, |_, window, cx| {
        pointer_move(window, gpui_kit::point(px(600.), px(400.)), None, cx);
        window.blur(cx);
        window.focus_next(cx);
        window.render_frame(cx);
        for _ in 0..20 {
            window.press("tab", cx);
            if window.find(format!("select-session-{id}")).focused() == Some(true) {
                window.render_frame(cx);
                assert!(window.find(format!("close-session-{id}")).visible());
                return;
            }
        }
        panic!("the terminal row was not reachable in the tab order");
    })
    .unwrap();
    dispose(&fixture, cx);
}

#[gpui_kit::test]
fn repo_menu_trigger_stays_visible_until_escape_dismisses_its_menu(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        let actions = format!("actions-{}", fixture.repo_id);
        window.hover(format!("fold-repo-{}", fixture.repo_id), cx);
        window.click(actions.clone(), cx);
        fixture.view.read_with(cx, |app, _| {
            assert_eq!(
                app.open_repo_menu.as_deref(),
                Some(fixture.repo_id.as_str())
            );
            assert!(!app.navigation.folded_repos.contains(&fixture.repo_id));
        });
        pointer_move(window, gpui_kit::point(px(800.), px(500.)), None, cx);
        window.render_frame(cx);
        assert!(window.find(actions).visible());
        window.press("escape", cx);
    })
    .unwrap();
    assert!(fixture
        .view
        .read_with(cx, |app, _| app.open_repo_menu.is_none()));
    close(&fixture, cx);
}

#[gpui_kit::test]
fn repo_fold_target_covers_the_heading_height(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    for click_top_edge in [true, false] {
        let was_folded = fixture.view.read_with(cx, |app, _| {
            app.navigation.folded_repos.contains(&fixture.repo_id)
        });
        cx.update_window(fixture.handle, |_, window, cx| {
            let fold = window
                .find(format!("fold-repo-{}", fixture.repo_id))
                .bounds();
            assert!(
                fold.size.height >= px(26.),
                "fold hitbox spans the heading: {fold:?}"
            );
            let y = if click_top_edge {
                fold.top() + px(1.)
            } else {
                fold.bottom() - px(1.)
            };
            let position = gpui_kit::point(fold.left() + px(12.), y);
            pointer_move(window, position, None, cx);
            pointer_down(window, position, cx);
            pointer_up(window, position, cx);
            window.render_frame(cx);
        })
        .unwrap();
        let is_folded = fixture.view.read_with(cx, |app, _| {
            app.navigation.folded_repos.contains(&fixture.repo_id)
        });
        assert_ne!(
            is_folded,
            was_folded,
            "clicking the {} edge should toggle the repository",
            if click_top_edge { "top" } else { "bottom" }
        );
    }
    close(&fixture, cx);
}

/// The repository heading's name and path sit in the middle of the row.
///
/// The heading is as wide as the panel and the pair is what it says, so the
/// pair is centred in the room the heading has rather than held against its
/// left edge.
#[gpui_kit::test]
fn the_repository_heading_centres_its_name_and_path(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let repo = fixture
        .view
        .read_with(cx, |app, _| app.navigation.repos[0].id.clone());
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        let content = window.find(format!("heading-content-{repo}")).bounds();
        let name = window.find(format!("repo-name-{repo}")).bounds();
        let path = window.find(format!("repo-path-{repo}")).bounds();
        assert!(
            path.size.width > px(0.),
            "the fixture's repository has no path to centre"
        );
        let left = name.left().as_f32() - content.left().as_f32();
        let right = content.right().as_f32() - path.right().as_f32();
        assert!(
            (left - right).abs() <= 1.,
            "the heading's texts sit {left}px from the left and {right}px from the right"
        );
    })
    .unwrap();
    dispose(&fixture, cx);
}

/// The heading's name and path are one line of text, and one line of text has
/// one baseline.
///
/// The path is drawn smaller than the name, and a smaller text's baseline sits
/// nearer the foot of its own box: aligning the two boxes at the foot would
/// leave the path hanging below the name. The path is lifted by the
/// difference, so the two share the name's baseline.
#[gpui_kit::test]
fn the_repository_heading_shares_one_baseline(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let repo = fixture
        .view
        .read_with(cx, |app, _| app.navigation.repos[0].id.clone());
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        let name = window.find(format!("repo-name-{repo}")).bounds();
        let path = window.find(format!("repo-path-{repo}")).bounds();
        let name_baseline = name.bottom()
            - px(baseline_from_bottom(
                window,
                HEADING_NAME_SIZE,
                gpui_kit::FontWeight::MEDIUM,
            ));
        let path_baseline = path.bottom()
            - px(baseline_from_bottom(
                window,
                HEADING_PATH_SIZE,
                gpui_kit::FontWeight::NORMAL,
            ));
        let gap = (name_baseline - path_baseline).as_f32();
        assert!(
            gap.abs() <= 0.25,
            "the path hangs {gap}px below the name's baseline"
        );
    })
    .unwrap();
    dispose(&fixture, cx);
}

/// A worktree's name is one line, whatever the row has to make room for.
///
/// The pointer on the row gives the room to the figures and to the close
/// button, so a name that fitted a moment ago no longer does. It is cut with
/// an ellipsis; it never takes a second line, which would spill out of the
/// row and over the one below it.
#[gpui_kit::test]
fn a_worktree_name_that_does_not_fit_stays_on_one_line(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let repo = fixture.view.read_with(cx, |app, _| {
        app.navigation
            .repos
            .iter()
            .find(|repo| repo.id == fixture.repo_id)
            .unwrap()
            .clone()
    });
    let opened =
        navigation::create_worktree(&repo, "native-opencode", "feat/native-opencode", "").unwrap();
    let worktree = opened.checkout_id.clone();
    fixture
        .view
        .update(cx, |app, _| app.navigation.merge(opened, false));
    fixture.view.update(cx, |app, cx| {
        app.navigation.sidebar_width = 240.;
        app.diff_totals.insert(
            worktree.clone(),
            DiffTotals {
                additions: 7674,
                deletions: 265,
            },
        );
        cx.notify();
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        let row = window.find(format!("select-checkout-{worktree}")).bounds();
        pointer_move(window, row.center(), None, cx);
        window.render_frame(cx);
        let name = window.find(format!("checkout-name-{worktree}")).bounds();
        assert!(
            name.size.height <= px(22.),
            "the name took a second line: {name:?}"
        );
        assert!(
            name.right() <= row.right(),
            "the name runs out of the row: {name:?} {row:?}"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// A row that says a terminal is starting keeps its words inside the panel.
///
/// The row is as wide as the panel and the sentence is longer than the room
/// left of it, so the sentence is cut with an ellipsis rather than running
/// under the panel's edge.
#[gpui_kit::test]
fn a_pending_terminal_row_stays_inside_the_panel(cx: &mut TestAppContext) {
    let fixture = open(cx);
    fixture.view.update(cx, |app, cx| {
        app.navigation.repos[0].name =
            "a-directory-name-longer-than-the-panel-it-is-drawn-in".into();
        app.navigation
            .pending
            .insert("pending-1".into(), fixture.checkout.clone());
        cx.notify();
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        let panel = window.find("navigation-panel").bounds();
        let text = window
            .find(format!("pending-{}", fixture.checkout))
            .bounds();
        assert!(
            text.right() <= panel.right(),
            "the row runs out of the panel: {text:?} {panel:?}"
        );
        assert!(
            text.size.height <= px(22.),
            "the sentence took a second line: {text:?}"
        );
    })
    .unwrap();
    dispose(&fixture, cx);
}

#[gpui_kit::test]
fn protected_checkouts_have_no_lifecycle_controls(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let home = "checkout:home-worktree";
    fixture.view.update(cx, |app, cx| {
        let repo = &mut app.navigation.repos[0];
        repo.kind = RepoKind::Git;
        repo.checkouts[0].is_missing = true;
        let mut checkout = repo.checkouts[0].clone();
        checkout.id = home.into();
        checkout.is_primary = false;
        checkout.is_missing = false;
        checkout.sessions.clear();
        repo.checkouts.push(checkout);
        app.navigation.home = Some(home.into());
        cx.notify();
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window
            .try_find(format!("dismiss-missing-{}", fixture.checkout))
            .is_none());
        assert!(window
            .try_find(format!("worktree-actions-{home}"))
            .is_none());
    })
    .unwrap();
    dispose(&fixture, cx);
}

/// Theming a row must not move it, nor take its controls with it.
///
/// The row's own geometry and the position of the control that acts on it are
/// the hitbox contract the drag gesture and the keyboard order both rely on.
/// A wash that changes the row's padding, or a font that changes its measure,
/// breaks both silently, so this is asserted against the drawn frame rather
/// than against the style that produced it.
#[gpui_kit::test]
fn the_pointer_states_do_not_move_a_row_or_its_controls(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let open_row = format!("select-session-{}", fixture.sessions[0]);
    let other_row = format!("select-session-{}", fixture.sessions[1]);
    let close = format!("close-session-{}", fixture.sessions[0]);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        pointer_move(window, gpui_kit::point(px(800.), px(500.)), None, cx);
        let (rest_row, rest_close) = (
            window.find(open_row.clone()).bounds(),
            window.find(close.clone()).bounds(),
        );
        for row in [&open_row, &other_row] {
            window.hover(row.clone(), cx);
            window.render_frame(cx);
            assert_eq!(
                window.find(open_row.clone()).bounds(),
                rest_row,
                "hovering {row} moved a row"
            );
            assert_eq!(
                window.find(close.clone()).bounds(),
                rest_close,
                "hovering {row} moved its close control"
            );
            assert!(
                window.find(open_row.clone()).visible(),
                "hovering {row} took the open row off the panel"
            );
        }
    })
    .unwrap();
    dispose(&fixture, cx);
}

/// The open row stays the open row while the pointer is on it.
///
/// Selection is announced and is what the keyboard order follows, so the one
/// thing a pointer must not do is talk the window out of it.
#[gpui_kit::test]
fn the_open_row_stays_selected_while_the_pointer_is_on_it(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let open_row = format!("select-session-{}", fixture.sessions[0]);
    let other_row = format!("select-session-{}", fixture.sessions[1]);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(open_row.clone(), cx);
        window.render_frame(cx);
        assert_eq!(
            window.find(open_row.clone()).selected(),
            Some(true),
            "the row was clicked, so it is the open one"
        );
        for row in [&open_row, &other_row] {
            window.hover(row.clone(), cx);
            window.render_frame(cx);
            assert_eq!(
                window.find(open_row.clone()).selected(),
                Some(true),
                "hovering {row} stopped the open row reading as open"
            );
            assert_eq!(
                window.find(other_row.clone()).selected(),
                Some(false),
                "hovering {row} opened the row it is not on"
            );
        }
    })
    .unwrap();
    dispose(&fixture, cx);
}
/// Reading a file is not unfinished work, so clicking through files adds no
/// row of unfinished work — and a file somebody types in is, without being
/// asked for.
#[gpui_kit::test]
async fn only_the_files_being_edited_are_unfinished_work_rows(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let checkout = fixture.checkout.clone();
    for path in ["one.txt", "two.txt", "three.txt"] {
        std::fs::write(fixture.root.join(path), "one line\n").unwrap();
        cx.update_window(fixture.handle, |_, window, cx| {
            fixture.view.update(cx, |app, cx| {
                let repo = app.navigation.repos[0].clone();
                let checkout = app.navigation.checkout(&checkout).unwrap().clone();
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
                .is_some_and(|(id, preview)| id == &checkout && preview.path == path)
        })
        .await;
        cx.update_window(fixture.handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
    }
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("item-row-three.txt (edited)").is_none(),
            "three files read and none of them typed in is not unfinished work"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// A draft is a row from the first keystroke, and it stops being one when the
/// draft is settled — saved, discarded, or undone back to what the file holds.
#[gpui_kit::test]
async fn a_row_is_a_draft_and_not_a_file_you_are_reading(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let checkout = fixture.checkout.clone();
    std::fs::write(fixture.root.join("code.rs"), "fn main() {}\n").unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&checkout).unwrap().clone();
            app.request_document(repo, checkout, "code.rs".into(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture.view.read(cx).open_document.is_some()
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("item-row-0").is_none(),
            "an open file nobody has typed in is being read, which is not a row"
        );
        let editor = fixture.view.read_with(cx, |app, _| {
            app.document_view.as_ref().unwrap().state.clone()
        });
        window.click(("input", editor.entity_id()), cx);
        window.input(" typed", cx);
        window.render_frame(cx);
        assert_eq!(
            window.find("select-item-code.rs (edited)").label(),
            Some("unsaved edits in main: code.rs (edited)"),
            "a draft is what the person is in the middle of, and the row says so"
        );
        assert_eq!(
            window.find("select-item-code.rs (edited)").selected(),
            Some(true),
            "the draft is what the pane is showing"
        );
        window.press("esc", cx);
        window.render_frame(cx);
        assert!(
            window.try_find("item-row-0").is_none(),
            "discarding the draft is what settles it, and the row goes with it"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// A note is a row from the moment it is written, it says how many, and it goes
/// when the review that holds it has nothing left in it.
#[gpui_kit::test]
fn a_note_is_a_row_that_says_how_many(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let checkout = fixture.checkout.clone();
    let note = |id: &str, path: &str, outdated: bool| muster_core::review::ReviewNote {
        id: id.into(),
        checkout_id: checkout.clone(),
        path: path.into(),
        side: muster_core::review::ReviewSide::New,
        line_start: 1,
        line_end: None,
        content: "look at this".into(),
        code: "a line".into(),
        code_hash: muster_core::review::review_anchor_hash("a line"),
        outdated,
    };
    fixture.view.update(cx, |app, cx| {
        app.open_changes(&checkout, None, cx);
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("item-row-0").is_none(),
            "a change set nobody has written a note on is not unfinished work"
        );
    })
    .unwrap();
    fixture.view.update(cx, |app, _| {
        app.review
            .notes
            .insert(checkout.clone(), vec![note("one", "notes.txt", false)]);
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("select-item-notes.txt (note)").label(),
            Some("review notes in main: notes.txt (note)"),
            "a note is work the pane is not showing, and the row names it"
        );
    })
    .unwrap();
    fixture.view.update(cx, |app, _| {
        app.review.notes.insert(
            checkout.clone(),
            vec![
                note("one", "notes.txt", false),
                note("two", "notes.txt", false),
                note("three", "other.txt", false),
            ],
        );
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("select-item-notes.txt (2 notes)").label(),
            Some("review notes in main: notes.txt (2 notes)"),
            "a row that said one note when there were two would be a claim nobody made"
        );
        assert!(
            window.find("select-item-other.txt (note)").visible(),
            "each file a review has notes on is a row of its own"
        );
    })
    .unwrap();
    // An outdated note is one the person has said is not part of this review, so
    // it is not one the row counts — the same rule the diff's own count uses, or
    // the panel and the pane would disagree about how many notes there are.
    fixture.view.update(cx, |app, _| {
        app.review.notes.insert(
            checkout.clone(),
            vec![
                note("one", "notes.txt", true),
                note("two", "notes.txt", true),
            ],
        );
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("item-row-0").is_none(),
            "a review with nothing left in it is not a row either"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// A file on screen is a row of its own, and the terminal it replaced is not
/// marked.
///
/// A terminal left marked while a file is being read says the pane is
/// somewhere else, and it is the one thing in the panel that is plainly wrong.
#[gpui_kit::test]
async fn a_file_on_screen_is_a_row_and_the_terminal_is_not(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let session = fixture.sessions[1].clone();
    std::fs::write(fixture.directory.path().join("code.rs"), "fn main() {}\n").unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "code.rs".into(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture.view.read(cx).open_document.is_some()
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        let view = window.find("select-view-code.rs");
        let label = view.label().unwrap();
        assert!(
            label.starts_with("Preview of code.rs in "),
            "the row says what the pane is on, and what it is: {label}"
        );
        assert_eq!(
            window.find("select-view-code.rs").selected(),
            Some(true),
            "the row that names the file on screen is the row that is open"
        );
        assert_eq!(
            window.find(format!("select-session-{session}")).selected(),
            Some(false),
            "a terminal left marked while a file is read says the pane is somewhere else"
        );
    })
    .unwrap();
    dispose(&fixture, cx);
}

/// Reading through files leaves one row, and it is the one on screen.
///
/// The row is the pane's, not the file's: ten files read in a row are one
/// thing being looked at, and ten rows would be ten claims about work that
/// nobody is doing.
#[gpui_kit::test]
async fn reading_through_files_leaves_one_row(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let checkout = fixture.checkout.clone();
    for path in ["one.txt", "two.txt", "three.txt"] {
        std::fs::write(fixture.root.join(path), "one line\n").unwrap();
        cx.update_window(fixture.handle, |_, window, cx| {
            fixture.view.update(cx, |app, cx| {
                let repo = app.navigation.repos[0].clone();
                let checkout = app.navigation.checkout(&checkout).unwrap().clone();
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
                .is_some_and(|(id, preview)| id == &checkout && preview.path == path)
        })
        .await;
    }
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("select-view-one.txt").is_none(),
            "the row follows the file on screen rather than growing one row per file"
        );
        assert_eq!(
            window.find("select-view-three.txt").label(),
            Some("Preview of three.txt in main"),
            "the row says what the pane is on"
        );
        assert!(
            window.try_find("item-row-three.txt (edited)").is_none(),
            "a file somebody only read is not unfinished work"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// The row outlives the terminal that replaced it.
///
/// A click on a terminal is a navigation and not a close, so the file that was
/// open is still one click away: the row is the way back to it.
#[gpui_kit::test]
async fn the_view_row_outlives_the_terminal_that_replaced_it(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let session = fixture.sessions[1].clone();
    std::fs::write(fixture.directory.path().join("code.rs"), "fn main() {}\n").unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "code.rs".into(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture.view.read(cx).open_document.is_some()
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("select-session-{session}"), cx);
        window.render_frame(cx);
        assert!(
            window.try_find("select-view-code.rs").is_some(),
            "the row is the way back to the file that was open"
        );
        assert_eq!(
            window.find("select-view-code.rs").selected(),
            Some(false),
            "the pane is on the terminal now, so the row is not the open one"
        );
        assert_eq!(
            window.find(format!("select-session-{session}")).selected(),
            Some(true),
            "the terminal is what the pane is showing"
        );
        window.click("select-view-code.rs", cx);
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture.view.read(cx).open_document.is_some()
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("select-view-code.rs").selected(),
            Some(true),
            "the row is the way back, and it brought the file with it"
        );
    })
    .unwrap();
    dispose(&fixture, cx);
}

/// The row closes what it names.
///
/// A close is a close: the view goes and the row goes with it, and the pane
/// is back on the terminal it came from.
#[gpui_kit::test]
async fn the_view_row_closes_what_it_names(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let session = fixture.sessions[1].clone();
    std::fs::write(fixture.directory.path().join("code.rs"), "fn main() {}\n").unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app.navigation.repos[0].clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_document(repo, checkout, "code.rs".into(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture.view.read(cx).open_document.is_some()
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.hover("select-view-code.rs", cx);
        window.click("close-view-code.rs", cx);
        window.render_frame(cx);
        assert!(
            window.try_find("select-view-code.rs").is_none(),
            "a close is a close: the row goes with the view it named"
        );
        assert_eq!(
            window.find(format!("select-session-{session}")).selected(),
            Some(true),
            "the pane is back on the terminal"
        );
    })
    .unwrap();
    assert!(fixture
        .view
        .read_with(cx, |app, _| app.open_document.is_none()));
    dispose(&fixture, cx);
}

/// A change set is a diff, and it says so.
///
/// The change set is not a terminal either, and the row that says which it is
/// is the one that wears the selection.
#[gpui_kit::test]
fn a_change_set_is_a_diff_row(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let checkout = fixture.checkout.clone();
    std::fs::write(fixture.root.join("change.txt"), "first\n").unwrap();
    fixture.view.update(cx, |app, cx| {
        app.open_changes(&checkout, None, cx);
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("select-view-changes").label(),
            Some("Diff of the changes in main"),
            "a change set is a diff, and the row says so"
        );
        assert_eq!(
            window.find("select-view-changes").selected(),
            Some(true),
            "the row that names what is on screen is the row that is open"
        );
        assert_eq!(
            window
                .find(format!("select-checkout-{checkout}"))
                .selected(),
            Some(false),
            "the workdir row gives the selection up to what is inside it"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

#[gpui_kit::test]
fn insertion_row_does_not_change_a_stationary_drag_target(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let first = &fixture.sessions[0];
    let dragged = &fixture.sessions[1];
    cx.update_window(fixture.handle, |_, window, cx| {
        let from = window
            .find(format!("select-session-{dragged}"))
            .bounds()
            .center();
        let first_bounds = window.find(format!("select-session-{first}")).bounds();
        let to = gpui_kit::point(first_bounds.center().x, first_bounds.top() + px(3.));
        pointer_down(window, from, cx);
        pointer_move(window, to, Some(MouseButton::Left), cx);
        window.render_frame(cx);
        for _ in 0..3 {
            pointer_move(window, to, Some(MouseButton::Left), cx);
            window.render_frame(cx);
            fixture.view.read_with(cx, |app, _| {
                assert_eq!(
                    app.sidebar_drag
                        .as_ref()
                        .unwrap()
                        .target
                        .as_ref()
                        .unwrap()
                        .index,
                    0
                );
            });
        }
        pointer_up(window, to, cx);
        window.render_frame(cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(
            app.navigation.checkout(&fixture.checkout).unwrap().sessions[0].id,
            *dragged
        );
    });
    dispose(&fixture, cx);
}

#[gpui_kit::test]
async fn a_count_sweep_answers_every_batch_and_never_turns_a_failure_into_zero(
    cx: &mut TestAppContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let root = git_repository(directory.path());
    let opened = navigation::open_directory(&root).unwrap();
    std::fs::write(root.join("untracked"), "one\ntwo\n").unwrap();
    let checkout = opened.repo.checkouts[0].clone();
    let mut invalid = checkout.clone();
    invalid.is_missing = true;
    let mut work = vec![("invalid".into(), opened.repo.clone(), invalid)];
    for index in 0..SWEEP_CONCURRENCY + 1 {
        work.push((index.to_string(), opened.repo.clone(), checkout.clone()));
    }
    let executor = cx.update(|cx| cx.background_executor().clone());
    executor.allow_parking();
    let results = count_sweep(work, executor).await;
    assert_eq!(results.len(), SWEEP_CONCURRENCY + 2);
    assert!(results[0].2.is_err());
    for (index, (_, path, total)) in results.into_iter().skip(1).enumerate() {
        assert_eq!(path, checkout.path);
        assert_eq!(
            total.unwrap(),
            DiffTotals {
                additions: 2,
                deletions: 0
            },
            "{index}"
        );
    }
}

#[gpui_kit::test]
async fn diff_refresh_is_immediate_coalesced_and_skips_plain_folders(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let plain = tempfile::tempdir().unwrap();
    let opened = navigation::open_directory(plain.path()).unwrap();
    let plain_id = opened.checkout_id.clone();
    let path = fixture.root.join("new-file");
    std::fs::write(&path, "one\n").unwrap();
    fixture.view.update(cx, |app, cx| {
        app.navigation.merge(opened, false);
        app.refresh_diff_totals(cx);
        assert!(app.diff_refresh_running);
        app.refresh_diff_totals(cx);
        assert!(app.diff_refresh_again);
    });
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        let app = fixture.view.read(cx);
        !app.diff_refresh_running
            && app
                .diff_totals
                .get(&fixture.checkout)
                .is_some_and(|counts| counts.additions == 1)
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        assert!(!app.diff_refresh_again);
        assert!(!app.diff_totals.contains_key(&plain_id));
    });
    std::fs::write(&path, "one\ntwo\n").unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("select-checkout-{}", fixture.checkout), cx);
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .diff_totals
            .get(&fixture.checkout)
            .is_some_and(|counts| counts.additions == 2)
    })
    .await;
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn hovering_a_checkout_opens_its_line_counts(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("changed"), "one\ntwo\n").unwrap();
    fixture
        .view
        .update(cx, |app, cx| app.refresh_diff_totals(cx));
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .diff_totals
            .get(&fixture.checkout)
            .is_some_and(|counts| counts.additions == 2)
    })
    .await;

    let width = |cx: &mut TestAppContext, id: &str| {
        let mut found = None;
        cx.update_window(fixture.handle, |_, window, _| {
            found = window
                .try_find(SharedString::from(id.to_owned()))
                .map(|snapshot| snapshot.bounds().size.width.as_f32());
        })
        .unwrap();
        found
    };

    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
    })
    .unwrap();
    // At rest the row says `+2` in the width it has always reserved.
    assert_eq!(
        width(cx, &format!("diff-figures-{}", fixture.checkout)),
        Some(0.),
        "the figures were open before the pointer arrived"
    );
    assert!(width(cx, &format!("diff-mark-add-{}", fixture.checkout)).unwrap_or(0.) > 0.);

    cx.update_window(fixture.handle, |_, window, cx| {
        window.hover(format!("select-checkout-{}", fixture.checkout), cx);
        window.render_frame(cx);
    })
    .unwrap();
    let figures = width(cx, &format!("diff-figures-{}", fixture.checkout)).unwrap();
    assert!(figures > 0., "hover left the line counts closed: {figures}");
    assert_eq!(
        width(cx, &format!("diff-mark-add-{}", fixture.checkout)),
        Some(0.),
        "the mark did not give way to the figures"
    );
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn the_pointer_over_a_row_reveals_its_own_controls(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let (worktree, _) = add_worktree(&fixture, cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
    })
    .unwrap();

    // The real pointer, not an element flag: hover has to survive hit-testing
    // through the overlays the row draws on top of itself.
    let cross = format!("worktree-actions-{worktree}");
    cx.update_window(fixture.handle, |_, window, cx| {
        let row = window.find(format!("select-checkout-{worktree}")).bounds();
        pointer_move(window, row.center(), None, cx);
        window.render_frame(cx);
        assert!(
            window.find(cross.clone()).visible(),
            "the row's own action stayed hidden under the pointer"
        );
    })
    .unwrap();

    // Moving away takes the affordance back, so the panel is not left lit.
    cx.update_window(fixture.handle, |_, window, cx| {
        pointer_move(window, gpui_kit::point(px(900.), px(600.)), None, cx);
        window.render_frame(cx);
        assert!(!window.find(cross).visible());
    })
    .unwrap();
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn the_background_sweep_finds_counts_with_only_the_pointer_over_a_row(
    cx: &mut TestAppContext,
) {
    let fixture = open_git(cx);
    fixture.view.update(cx, |app, cx| app.start_maintenance(cx));
    std::fs::write(fixture.root.join("changed"), "one\ntwo\n").unwrap();

    // Nothing asks for these counts: the sweep does, the way it does in the app.
    cx.wait_for(fixture.handle, Duration::from_secs(30), |_, cx| {
        fixture
            .view
            .read(cx)
            .diff_totals
            .get(&fixture.checkout)
            .is_some_and(|counts| counts.additions == 2)
    })
    .await;

    let figures = format!("diff-figures-{}", fixture.checkout);
    let mark = format!("diff-mark-add-{}", fixture.checkout);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        pointer_move(
            window,
            window
                .find(format!("select-checkout-{}", fixture.checkout))
                .bounds()
                .center(),
            None,
            cx,
        );
        window.render_frame(cx);
        let open = window.find(figures.clone()).bounds().size.width.as_f32();
        assert!(
            open > 0.,
            "the sweep counted the file but hover left the figures closed"
        );
        assert_eq!(window.find(mark).bounds().size.width.as_f32(), 0.);
    })
    .unwrap();
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn hover_opens_counts_wide_enough_to_need_the_strip(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    // The shape a real worktree produces: five digits on each side, which is
    // wider than the strip a bare mark reserves and has to open into.
    let (worktree, _) = add_worktree(&fixture, cx);
    fixture.view.update(cx, |app, cx| {
        app.diff_totals.insert(
            worktree.clone(),
            DiffTotals {
                additions: 43_084,
                deletions: 354,
            },
        );
        cx.notify();
    });

    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window
                .try_find(format!("diff-figures-{worktree}"))
                .map(|snapshot| snapshot.bounds().size.width.as_f32()),
            Some(0.),
            "wide figures were open before the pointer arrived"
        );
        let row = window.find(format!("select-checkout-{worktree}")).bounds();
        pointer_move(window, row.center(), None, cx);
        window.render_frame(cx);
        let open = window
            .find(format!("diff-figures-{worktree}"))
            .bounds()
            .size
            .width
            .as_f32();
        assert!(open > 40., "the figures did not open wide enough: {open}");
        // And the cross keeps its own strip, so the figures never run under it.
        let cross = window.find(format!("worktree-actions-{worktree}")).bounds();
        assert!(
            figures_right_of(window, &worktree) <= cross.left().as_f32() + 0.5,
            "the figures reached into the cross's slot"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

fn figures_right_of(window: &mut Window, checkout: &str) -> f32 {
    window
        .find(format!("diff-figures-{checkout}"))
        .bounds()
        .right()
        .as_f32()
}

#[gpui_kit::test]
fn the_row_that_adds_a_terminal_is_centred_across_the_panel(cx: &mut TestAppContext) {
    let fixture = open(cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        let action = window
            .find(format!("new-terminal-content-{}", fixture.checkout))
            .bounds();
        let row = window
            .find(format!("new-terminal-{}", fixture.checkout))
            .bounds();
        // A session row is the panel's own width, indented on the left only,
        // so it is what says how wide the panel is from here.
        let terminal = window
            .find(format!("select-session-{}", fixture.sessions[0]))
            .bounds();
        assert!(
            (row.right().as_f32() - terminal.right().as_f32()).abs() <= 1.,
            "the row is {}px narrower than the panel's rows",
            terminal.right().as_f32() - row.right().as_f32()
        );
        let offset = action.center().x.as_f32() - row.center().x.as_f32();
        assert!(
            offset.abs() <= 1.,
            "New terminal content sits {offset}px off the row's middle"
        );
    })
    .unwrap();
    dispose(&fixture, cx);
}

#[gpui_kit::test]
fn worktree_creation_cannot_submit_while_defaults_are_loading(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.open_worktree_dialog(&fixture.repo_id, window, cx);
            assert!(app.worktree.as_ref().unwrap().loading_defaults);
            app.submit_worktree(window, cx);
            assert!(!app.worktree.as_ref().unwrap().busy);
        });
    })
    .unwrap();
    close(&fixture, cx);
}

/// A plain directory with two live terminals, as the sidebar tests open one.
///
/// `pub(super)` because the title bar's own tests draw over the same window
/// rather than standing up a second fixture for it.
pub(super) struct Fixture {
    pub(super) directory: TempDir,
    pub(super) handle: AnyWindowHandle,
    pub(super) view: Entity<TerminalWindow>,
    backend: Arc<TerminalBackend>,
    pub(super) checkout: String,
    pub(super) sessions: Vec<String>,
}

pub(super) fn open(cx: &mut TestAppContext) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repo::plain(directory.path(), "test").unwrap();
    let checkout = repo.checkouts[0].id.clone();
    let backend = Arc::new(TerminalBackend::default());
    let mut sessions = Vec::new();
    cx.update(|cx| {
        gpui_kit::init(cx);
        load_fonts(cx);
        crate::theme::install(cx);
    });
    let (handle, view) = cx.update(|cx| {
        gpui_kit::open_window(window_bounds(cx), cx, |window, cx| {
            cx.new(|cx| {
                let mut app = TerminalWindow::empty(backend.clone(), true, cx);
                app.navigation.merge(
                    navigation::OpenedDirectory {
                        repo,
                        checkout_id: checkout.clone(),
                    },
                    true,
                );
                for _ in 0..2 {
                    let id = format!(
                        "sidebar-test-{}-{}",
                        std::process::id(),
                        NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
                    );
                    let mut config = TerminalSessionConfig::new(
                        id.clone(),
                        directory.path().to_owned(),
                        80,
                        24,
                        Some(PathBuf::from("/bin/sh")),
                    );
                    // Keep a shell parent and enable job control, like the application's
                    // interactive login shell. An exec-optimized `sh -c cat` has
                    // no separate foreground process group to observe.
                    config.args = Some(vec!["-c".into(), "set -m; cat; :".into()]);
                    config.status_interval = Duration::from_millis(20);
                    let shell = app.owner.spawn(config).unwrap();
                    let terminal = cx.new(|cx| {
                        TerminalView::new(shell.session.clone(), shell.metrics.clone(), cx)
                    });
                    app.terminals.insert(
                        id.clone(),
                        LiveTerminal {
                            checkout_id: checkout.clone(),
                            view: terminal,
                            shell,
                        },
                    );
                    app.navigation.add_session(
                        Session {
                            id: id.clone(),
                            checkout_id: checkout.clone(),
                            name: "sh".into(),
                            created_at: "test".into(),
                            session_type: SessionType::Shell,
                            status: SavedSessionStatus::Active,
                        },
                        Some(app.navigation.revision),
                    );
                    sessions.push(id);
                }
                app.focus_terminal(window, cx);
                app
            })
        })
        .unwrap()
    });
    cx.update_window(handle, |_, window, cx| {
        window.activate_window();
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    Fixture {
        directory,
        handle,
        view,
        backend,
        checkout,
        sessions,
    }
}

pub(super) fn dispose(fixture: &Fixture, cx: &mut TestAppContext) {
    cx.update_window(fixture.handle, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    fixture.view.update(cx, |app, _| app.owner.stop());
}

pub(super) fn pointer_move(
    window: &mut Window,
    position: Point<Pixels>,
    pressed_button: Option<MouseButton>,
    cx: &mut App,
) {
    window.dispatch_event(
        MouseMoveEvent {
            position,
            pressed_button,
            modifiers: Default::default(),
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
}

fn pointer_down(window: &mut Window, position: Point<Pixels>, cx: &mut App) {
    window.dispatch_event(
        MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
}

fn pointer_up(window: &mut Window, position: Point<Pixels>, cx: &mut App) {
    window.dispatch_event(
        MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Default::default(),
            click_count: 1,
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
}

fn pointer_exit(window: &mut Window, position: Point<Pixels>, cx: &mut App) {
    window.dispatch_event(
        MouseExitEvent {
            position,
            pressed_button: Some(MouseButton::Left),
            modifiers: Default::default(),
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
}

fn drag_pointer(window: &mut Window, from: Point<Pixels>, to: Point<Pixels>, cx: &mut App) {
    pointer_move(window, from, None, cx);
    pointer_down(window, from, cx);
    pointer_move(window, to, Some(MouseButton::Left), cx);
    pointer_up(window, to, cx);
}

#[gpui_kit::test]
fn real_sidebar_selection_retains_both_terminal_entities(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let before = fixture.view.read_with(cx, |app, _| {
        app.terminals
            .iter()
            .map(|(id, terminal)| (id.clone(), terminal.view.entity_id()))
            .collect::<HashMap<_, _>>()
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        let select = window
            .find(SharedString::from(format!(
                "select-session-{}",
                fixture.sessions[0]
            )))
            .bounds();
        let close = window
            .find(SharedString::from(format!(
                "close-session-{}",
                fixture.sessions[0]
            )))
            .bounds();
        assert!(select.origin.x + select.size.width <= px(240.));
        assert!(close.origin.x + close.size.width <= px(240.));
        window.click(
            SharedString::from(format!("select-session-{}", fixture.sessions[0])),
            cx,
        );
        assert_eq!(
            window
                .find(SharedString::from(format!(
                    "select-session-{}",
                    fixture.sessions[0]
                )))
                .selected(),
            Some(true)
        );
        assert_eq!(
            window
                .find(SharedString::from(format!(
                    "select-session-{}",
                    fixture.sessions[1]
                )))
                .selected(),
            Some(false)
        );
    })
    .unwrap();
    fixture.view.read_with(cx, |app, cx| {
        assert_eq!(
            app.navigation.active_session.as_deref(),
            Some(fixture.sessions[0].as_str())
        );
        assert_eq!(app.terminals.len(), 2);
        for (id, terminal) in &app.terminals {
            assert_eq!(before[id], terminal.view.entity_id());
        }
        assert_eq!(
            app.terminal().unwrap().read(cx).session().id(),
            fixture.sessions[0]
        );
    });
    assert!(fixture.backend.status(&fixture.sessions[1]).is_ok());
    dispose(&fixture, cx);
}

#[gpui_kit::test]
fn checkout_fold_hides_controls_but_keeps_live_sessions_and_selection(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let active = fixture.sessions[1].clone();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click(
            SharedString::from(format!("fold-checkout-{}", fixture.checkout)),
            cx,
        );
        assert!(window
            .try_find(SharedString::from(format!("select-session-{active}")))
            .is_none());
        assert_eq!(
            window
                .find(SharedString::from(format!(
                    "fold-checkout-{}",
                    fixture.checkout
                )))
                .expanded(),
            Some(false)
        );
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert!(app.navigation.folded_checkouts.contains(&fixture.checkout));
        assert_eq!(
            app.navigation.active_session.as_deref(),
            Some(active.as_str())
        );
        assert_eq!(app.terminals.len(), 2);
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click(
            SharedString::from(format!("fold-checkout-{}", fixture.checkout)),
            cx,
        );
        assert!(window
            .try_find(SharedString::from(format!("select-session-{active}")))
            .is_some());
        assert_eq!(
            window
                .find(SharedString::from(format!(
                    "fold-checkout-{}",
                    fixture.checkout
                )))
                .expanded(),
            Some(true)
        );
    })
    .unwrap();
    dispose(&fixture, cx);
}

#[gpui_kit::test]
fn sidebar_rows_keep_close_controls_out_of_flow(cx: &mut TestAppContext) {
    // The action stays in the tab order but is an absolute overlay, so the name
    // has the same measured space at rest and while the pointer is over the row.
    let fixture = open(cx);
    let id = fixture.sessions[0].clone();
    let name = SharedString::from(format!("select-session-{id}"));
    let action = SharedString::from(format!("close-session-{id}"));
    cx.update_window(fixture.handle, |_, window, cx| {
        let before = window.find(name.clone()).bounds();
        let close_before = window.find(action.clone()).bounds();
        assert_eq!(close_before.size.width, px(20.));
        window.hover(name.clone(), cx);
        let after = window.find(name).bounds();
        let close_after = window.find(action).bounds();
        assert_eq!(before.size.width, after.size.width);
        assert_eq!(close_before.origin.x, close_after.origin.x);
        assert_eq!(close_before.size.width, close_after.size.width);
    })
    .unwrap();
    dispose(&fixture, cx);
}

#[gpui_kit::test]
fn panels_shortcut_captures_the_focused_terminal_without_losing_it(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let selected = fixture.sessions[1].clone();
    let shortcut = if cfg!(target_os = "macos") {
        "cmd-/"
    } else {
        "ctrl-/"
    };
    cx.update_window(fixture.handle, |_, window, cx| window.press(shortcut, cx))
        .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert!(!app.navigation.panels_visible);
        assert_eq!(app.navigation.sidebar_width, 240.);
        assert_eq!(
            app.navigation.active_session.as_deref(),
            Some(selected.as_str())
        );
        assert_eq!(app.terminals.len(), 2);
    });
    cx.update_window(fixture.handle, |_, window, cx| window.press(shortcut, cx))
        .unwrap();
    assert!(fixture
        .view
        .read_with(cx, |app, _| app.navigation.panels_visible));
    assert!(fixture.backend.status(&selected).is_ok());
    dispose(&fixture, cx);
}

/// Git's own answer for a checkout whose change set is `files`.
///
/// The row's decision is a question about this list, so a test that pins the
/// decision does not need a repository behind it. The reads themselves are
/// pinned against real ones below.
fn status_of(files: &[(&str, &str)]) -> navigation::GitStatus {
    navigation::GitStatus {
        branch: Some("main".into()),
        head: Some("abc1234".into()),
        default_branch: "main".into(),
        ahead_count: 0,
        files: files
            .iter()
            .map(|(path, status)| navigation::ChangedFile {
                path: (*path).into(),
                old_path: None,
                status: (*status).into(),
                additions: None,
                deletions: None,
            })
            .collect(),
    }
}

/// A live shell in a Git fixture's checkout, so a row that should show a
/// terminal has one to show and one to give the keyboard to.
fn add_terminal(fixture: &GitFixture, cx: &mut TestAppContext) -> String {
    let id = format!(
        "git-fixture-shell-{}-{}",
        std::process::id(),
        NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
    );
    let checkout = fixture.checkout.clone();
    fixture.view.update(cx, |app, cx| {
        let mut config = TerminalSessionConfig::new(
            id.clone(),
            fixture.root.clone(),
            80,
            24,
            Some(PathBuf::from("/bin/sh")),
        );
        config.args = Some(vec!["-c".into(), "set -m; cat; :".into()]);
        config.status_interval = Duration::from_millis(20);
        let shell = app.owner.spawn(config).unwrap();
        let terminal =
            cx.new(|cx| TerminalView::new(shell.session.clone(), shell.metrics.clone(), cx));
        app.terminals.insert(
            id.clone(),
            LiveTerminal {
                checkout_id: checkout.clone(),
                view: terminal,
                shell,
            },
        );
        app.navigation.add_session(
            Session {
                id: id.clone(),
                checkout_id: checkout.clone(),
                name: "sh".into(),
                created_at: "test".into(),
                session_type: SessionType::Shell,
                status: SavedSessionStatus::Active,
            },
            Some(app.navigation.revision),
        );
    });
    id
}

/// A PNG header and a NUL: bytes Git will not measure as lines.
const PICTURE: [u8; 10] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];

/// Commit everything the fixture writes, under an identity of its own so the
/// machine running the suite is not asked for one.
pub(super) fn commit_all(root: &Path, message: &str) {
    run_git(root, &["add", "-A"]);
    run_git(
        root,
        &[
            "-c",
            "user.email=muster@example.test",
            "-c",
            "user.name=Muster",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
    );
}

/// Read a checkout's change list the way the Changes tab does, and wait for it.
///
/// The tree asks for this read as it draws, so by the time a test has changed
/// anything the window is holding an answer from before. Asking again is what
/// the tab's button does, and it is what makes the row's decision a question
/// about the repository as it is now rather than as it was.
pub(super) async fn read_status(fixture: &GitFixture, checkout: &str, cx: &mut TestAppContext) {
    fixture
        .view
        .update(cx, |app, cx| app.request_git_status(checkout, cx));
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_status
            .get(checkout)
            .is_some_and(|status| !status.files.is_empty())
    })
    .await;
}

/// Forget what the window knows about a checkout's changes.
///
/// This is the state a row click meets for a checkout the window has not read
/// yet: the Files tree reads the active checkout as it draws, so without this a
/// fixture would answer every click from a read taken before the test wrote
/// anything.
fn forget_status(fixture: &GitFixture, checkouts: &[&str], cx: &mut TestAppContext) {
    fixture.view.update(cx, |app, _| {
        for checkout in checkouts {
            app.git_status.remove(*checkout);
            // The tree's index is built with the status it indexes, so a window
            // that has forgotten the status has forgotten the index with it.
            app.status_index.remove(*checkout);
            app.git_status_errors.remove(*checkout);
        }
    });
}

/// One binary file, one empty file and one mode change: three changes that
/// count no lines at all, against a row that draws `+0 −0` for every one of
/// them. The change list is what the row's click reads, so it opens the change
/// set with the counts still at zero.
#[gpui_kit::test]
async fn a_row_with_no_line_counts_still_opens_its_change_set(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("tracked.txt"), "hello\n").unwrap();
    commit_all(&fixture.root, "tracked");
    run_git(
        &fixture.root,
        &["update-index", "--chmod=+x", "tracked.txt"],
    );
    std::fs::write(fixture.root.join("picture.png"), PICTURE).unwrap();
    std::fs::write(fixture.root.join("empty.txt"), "").unwrap();
    read_status(&fixture, &fixture.checkout, cx).await;

    // The counts are settled first, so the click below meets a row honestly
    // advertising `+0 −0` and nothing else.
    fixture
        .view
        .update(cx, |app, cx| app.refresh_diff_totals(cx));
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .diff_totals
            .contains_key(&fixture.checkout)
    })
    .await;
    assert_eq!(
        fixture
            .view
            .read_with(cx, |app, _| app.diff_totals[&fixture.checkout]),
        DiffTotals {
            additions: 0,
            deletions: 0
        },
        "Git can count none of these three, which is not the same as there being none"
    );

    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("select-checkout-{}", fixture.checkout), cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(
            app.open_diff,
            Some((fixture.checkout.clone(), None)),
            "a row whose counts are zero opened the terminal instead of its changes"
        );
        assert_eq!(app.inspector_tab, InspectorTab::Changes);
        assert!(app.pending_row_choice.is_none());
        assert_eq!(
            app.diff_totals[&fixture.checkout],
            DiffTotals {
                additions: 0,
                deletions: 0
            },
            "the change set was opened on the strength of a count the row does not have"
        );
        let status = &app.git_status[&fixture.checkout];
        assert_eq!(status.files.len(), 3, "{status:?}");
        assert!(
            status
                .files
                .iter()
                .find(|file| file.path == "picture.png")
                .is_some_and(|file| file.additions.is_none()),
            "a binary file was given a line count nobody measured: {status:?}"
        );
    });
    close(&fixture, cx);
}

/// A click on a checkout the window has not read yet waits for Git rather than
/// calling it clean, and answers from what the read says.
#[gpui_kit::test]
async fn a_click_before_the_change_list_is_read_waits_for_it(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("picture.png"), PICTURE).unwrap();
    forget_status(&fixture, &[&fixture.checkout], cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("select-checkout-{}", fixture.checkout), cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert!(
            app.pending_row_choice.is_some(),
            "the click decided without asking Git"
        );
        assert!(
            app.open_diff.is_none() && !app.focus_pending,
            "the click answered for a checkout nobody had read"
        );
    });
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture.view.read(cx).pending_row_choice.is_none()
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(app.open_diff, Some((fixture.checkout.clone(), None)));
        assert_eq!(app.inspector_tab, InspectorTab::Changes);
    });
    close(&fixture, cx);
}

/// A commit the worktree is holding above the merge base is a change even
/// though its working tree is clean and its line counts are zero.
#[gpui_kit::test]
async fn a_committed_change_ahead_of_the_merge_base_opens_the_change_set(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let (worktree, path) = add_worktree(&fixture, cx);
    std::fs::write(path.join("picture.png"), PICTURE).unwrap();
    commit_all(&path, "picture");
    read_status(&fixture, &worktree, cx).await;
    fixture
        .view
        .update(cx, |app, cx| app.refresh_diff_totals(cx));
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture.view.read(cx).diff_totals.contains_key(&worktree)
    })
    .await;
    assert_eq!(
        fixture
            .view
            .read_with(cx, |app, _| app.diff_totals[&worktree]),
        DiffTotals {
            additions: 0,
            deletions: 0
        }
    );
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("select-checkout-{worktree}"), cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(
            app.open_diff,
            Some((worktree.clone(), None)),
            "a worktree whose change is committed but uncounted showed its terminal"
        );
        let status = &app.git_status[&worktree];
        assert_eq!(status.ahead_count, 1, "{status:?}");
        assert_eq!(
            status.files.len(),
            1,
            "the commit this worktree holds is not in its change set: {status:?}"
        );
    });
    close(&fixture, cx);
}

/// Read the root listing again, for a fixture whose tree was drawn before the
/// test wrote anything into the checkout.
async fn read_root_listing(fixture: &GitFixture, path: &str, cx: &mut TestAppContext) {
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.file_entries
                .remove(&(fixture.checkout.clone(), String::new()));
            let repo = app
                .navigation
                .repos
                .iter()
                .find(|repo| repo.id == fixture.repo_id)
                .unwrap()
                .clone();
            let checkout = app.navigation.checkout(&fixture.checkout).unwrap().clone();
            app.request_directory(repo, checkout, String::new(), window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .file_entries
            .get(&(fixture.checkout.clone(), String::new()))
            .is_some_and(|entries| entries.iter().any(|entry| entry.path == path))
    })
    .await;
}

/// A checkout Git says is clean shows its terminal: once the read that says so
/// has answered, and while it has not, by waiting rather than by guessing.
#[gpui_kit::test]
async fn a_known_clean_checkout_shows_its_terminal(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    add_terminal(&fixture, cx);
    // The click arrives before the window knows, and is answered by the read.
    forget_status(&fixture, &[&fixture.checkout], cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("select-checkout-{}", fixture.checkout), cx);
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture.view.read(cx).pending_row_choice.is_none()
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        fixture.view.read_with(cx, |app, cx| {
            assert!(
                app.open_diff.is_none(),
                "a clean checkout opened a change set"
            );
            assert!(
                app.terminal()
                    .is_some_and(|terminal| terminal.read(cx).focus_handle(cx).is_focused(window)),
                "a clean checkout kept the keyboard away from its terminal"
            );
        });
    })
    .unwrap();
    // The second click is answered from what the window already knows, and asks
    // Git nothing.
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("select-checkout-{}", fixture.checkout), cx);
        fixture.view.read_with(cx, |app, cx| {
            assert!(app.open_diff.is_none());
            assert!(app.pending_row_choice.is_none());
            assert!(!app.git_status_running.contains(&fixture.checkout));
            assert!(app
                .terminal()
                .is_some_and(|terminal| terminal.read(cx).focus_handle(cx).is_focused(window)));
        });
    })
    .unwrap();
    close(&fixture, cx);
}

/// A read that could not be made is said in the window rather than read as a
/// checkout with nothing in it.
#[gpui_kit::test]
async fn a_failed_change_read_is_said_rather_than_called_clean(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("picture.png"), PICTURE).unwrap();
    // Without its Git metadata this checkout cannot be read at all, and the
    // change in it is one the read would otherwise have listed.
    std::fs::remove_dir_all(fixture.root.join(".git")).unwrap();
    forget_status(&fixture, &[&fixture.checkout], cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("select-checkout-{}", fixture.checkout), cx);
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture.view.read(cx).toasts.latest_error().is_some()
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        assert!(
            app.git_status_errors.contains_key(&fixture.checkout),
            "the read did not fail, so this is not the case it was written for"
        );
        assert!(
            app.pending_row_choice.is_none(),
            "the click is still waiting on a read that already failed"
        );
        assert!(app.open_diff.is_none());
        assert!(!app.focus_pending, "a failed read took the keyboard");
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.find("toast-stack").visible(),
            "the failure is in the state and not on screen"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// A change list the window is still holding is not an answer to a question
/// asked after the read that would have confirmed it failed. A clean list is
/// not a clean checkout.
#[gpui_kit::test]
async fn a_clean_list_cached_before_a_failed_read_is_not_a_clean_checkout(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    add_terminal(&fixture, cx);
    // A real read, and the answer the window keeps: this checkout is clean.
    fixture
        .view
        .update(cx, |app, cx| app.request_git_status(&fixture.checkout, cx));
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_status
            .contains_key(&fixture.checkout)
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        assert!(
            app.git_status[&fixture.checkout].files.is_empty(),
            "the list this test starts from is not clean, so the rest is not this case"
        );
    });
    // Git stops answering. The list above is what the panel still draws, and it
    // is older than the failure.
    std::fs::remove_dir_all(fixture.root.join(".git")).unwrap();
    fixture
        .view
        .update(cx, |app, cx| app.request_git_status(&fixture.checkout, cx));
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_status_errors
            .contains_key(&fixture.checkout)
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("select-checkout-{}", fixture.checkout), cx);
        fixture.view.read_with(cx, |app, cx| {
            assert!(
                app.git_status.contains_key(&fixture.checkout),
                "the failed read dropped the list the decision is being taken against"
            );
            assert!(
                app.pending_row_choice.is_some(),
                "the click read the cached clean list as the newest answer and took it"
            );
            let terminal = app.terminal().expect("the checkout's own shell");
            assert!(
                !terminal.read(cx).focus_handle(cx).is_focused(window),
                "a read that failed put the keyboard where a clean answer would have put it"
            );
            assert!(app.open_diff.is_none());
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture.view.read(cx).toasts.latest_error().is_some()
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        assert!(app.pending_row_choice.is_none());
        assert!(app.open_diff.is_none());
        assert!(!app.focus_pending, "a failed read took the keyboard");
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.find("toast-stack").visible(),
            "the failure is in the state and not on screen"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// The same for a list that has changes in it: a failed read does not say the
/// change set is still there, so the row does not open one it cannot vouch for.
#[gpui_kit::test]
async fn a_cached_change_list_is_not_opened_after_the_read_behind_it_failed(
    cx: &mut TestAppContext,
) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("picture.png"), PICTURE).unwrap();
    read_status(&fixture, &fixture.checkout, cx).await;
    std::fs::remove_dir_all(fixture.root.join(".git")).unwrap();
    fixture
        .view
        .update(cx, |app, cx| app.request_git_status(&fixture.checkout, cx));
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_status_errors
            .contains_key(&fixture.checkout)
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("select-checkout-{}", fixture.checkout), cx);
        fixture.view.read_with(cx, |app, _| {
            assert!(
                !app.git_status[&fixture.checkout].files.is_empty(),
                "the list this test starts from is empty, so the rest is not this case"
            );
            assert!(
                app.open_diff.is_none(),
                "a read that failed still opened the change set it left behind"
            );
            assert!(
                app.pending_row_choice.is_some(),
                "the click took the cached list as an answer no read has confirmed"
            );
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture.view.read(cx).toasts.latest_error().is_some()
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        assert!(app.pending_row_choice.is_none());
        assert!(app.open_diff.is_none());
        assert!(!app.focus_pending);
    });
    close(&fixture, cx);
}

/// Two clicks, one turn: the read the first one started must not navigate back
/// to the checkout the second one left behind.
#[gpui_kit::test]
async fn a_late_read_does_not_navigate_back_to_the_checkout_left_behind(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let (worktree, path) = add_worktree(&fixture, cx);
    // Both checkouts have changes, so the two answers disagree about the surface.
    std::fs::write(fixture.root.join("picture.png"), PICTURE).unwrap();
    std::fs::write(path.join("picture.png"), PICTURE).unwrap();
    forget_status(&fixture, &[&fixture.checkout, &worktree], cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("select-checkout-{}", fixture.checkout), cx);
        // Nothing has had a chance to answer yet: both clicks are in one turn.
        window.click(format!("select-checkout-{worktree}"), cx);
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture.view.read(cx).open_diff == Some((worktree.clone(), None))
    })
    .await;
    // Every read has landed by now, including the one for the checkout the
    // person left behind, and none of them navigated back to it.
    cx.run_until_parked();
    fixture.view.read_with(cx, |app, _| {
        assert!(app.git_status.contains_key(&fixture.checkout));
        assert!(app.pending_row_choice.is_none());
        assert_eq!(app.open_diff, Some((worktree.clone(), None)));
        assert_eq!(
            app.navigation.active_checkout.as_deref(),
            Some(worktree.as_str())
        );
    });
    close(&fixture, cx);
}

/// The person naming a terminal while the read is on its way gets that terminal.
#[gpui_kit::test]
async fn a_late_read_does_not_take_the_terminal_the_person_asked_for(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let session = add_terminal(&fixture, cx);
    std::fs::write(fixture.root.join("picture.png"), PICTURE).unwrap();
    forget_status(&fixture, &[&fixture.checkout], cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("select-checkout-{}", fixture.checkout), cx);
        window.click(format!("select-session-{session}"), cx);
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_status
            .get(&fixture.checkout)
            .is_some_and(|status| !status.files.is_empty())
    })
    .await;
    cx.run_until_parked();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        fixture.view.read_with(cx, |app, cx| {
            assert!(app.pending_row_choice.is_none());
            assert!(
                app.open_diff.is_none(),
                "a late read put a change set over the terminal the person chose"
            );
            assert_eq!(
                app.navigation.active_session.as_deref(),
                Some(session.as_str())
            );
            assert!(
                app.terminal()
                    .is_some_and(|terminal| terminal.read(cx).focus_handle(cx).is_focused(window)),
                "the terminal the person asked for did not keep the keyboard"
            );
        });
    })
    .unwrap();
    close(&fixture, cx);
}

/// The same for a document: a file opened in the tree while the read is on its
/// way stays the surface.
#[gpui_kit::test]
async fn a_late_read_does_not_take_the_document_the_person_asked_for(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("note.txt"), "hello\n").unwrap();
    std::fs::write(fixture.root.join("picture.png"), PICTURE).unwrap();
    // The tree's own listing, so the file row below is a real target rather
    // than a call into the window.
    read_root_listing(&fixture, "note.txt", cx).await;
    forget_status(&fixture, &[&fixture.checkout], cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        window.click(format!("select-checkout-{}", fixture.checkout), cx);
        window.click("file-control:note.txt", cx);
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .git_status
            .get(&fixture.checkout)
            .is_some_and(|status| !status.files.is_empty())
    })
    .await;
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture.view.read(cx).open_document.is_some()
    })
    .await;
    cx.run_until_parked();
    fixture.view.read_with(cx, |app, _| {
        assert!(app.pending_row_choice.is_none());
        assert!(
            app.open_diff.is_none(),
            "a late read put a change set over the document being read"
        );
    });
    close(&fixture, cx);
}

/// A plain directory has no change set to have. That is an answer, and the row
/// does not wait for a Git read that would never come.
#[gpui_kit::test]
fn a_plain_directory_row_still_shows_its_terminal(cx: &mut TestAppContext) {
    let fixture = open(cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("select-checkout-{}", fixture.checkout), cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert!(
            app.pending_row_choice.is_none(),
            "a plain folder is waiting for a Git read that will never answer"
        );
        assert!(app.open_diff.is_none());
        assert!(app.open_document.is_none());
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.read_with(cx, |app, cx| {
            assert!(
                app.terminal()
                    .is_some_and(|terminal| terminal.read(cx).focus_handle(cx).is_focused(window)),
                "a plain folder's row did not give the keyboard to its terminal"
            );
        });
    })
    .unwrap();
    dispose(&fixture, cx);
}

#[gpui_kit::test]
fn a_worktree_row_and_the_label_on_it_do_the_same_thing(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let label = format!("select-checkout-{}", fixture.checkout);
    // A checkout with a change set opens it rather than whatever the window was
    // showing. The line counts beside the row are presentation and play no part
    // in that: this row's own are zero.
    fixture.view.update(cx, |app, _| {
        app.diff_totals.insert(
            fixture.checkout.clone(),
            DiffTotals {
                additions: 0,
                deletions: 0,
            },
        );
        app.git_status
            .insert(fixture.checkout.clone(), status_of(&[("change.txt", "M")]));
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(label.clone(), cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(
            app.open_diff,
            Some((fixture.checkout.clone(), None)),
            "a row with a change set opened the terminal instead of them"
        );
        assert_eq!(
            app.navigation.active_checkout.as_deref(),
            Some(&*fixture.checkout)
        );
        assert_eq!(app.inspector_tab, InspectorTab::Changes);
    });
    // A checkout Git says is clean keeps the view it had: the terminal.
    fixture.view.update(cx, |app, _| {
        app.git_status
            .insert(fixture.checkout.clone(), status_of(&[]));
        app.open_diff = Some((fixture.checkout.clone(), None));
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click(label, cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert!(app.open_diff.is_none(), "a clean row invented a change set");
        assert!(app.open_document.is_none());
    });
    close(&fixture, cx);
}

/// The count drawn on the row is a target of its own, and it is the same click
/// as the name beside it.
#[gpui_kit::test]
async fn the_row_and_the_figures_on_it_make_the_same_decision(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    std::fs::write(fixture.root.join("changed.txt"), "changed\n").unwrap();
    read_status(&fixture, &fixture.checkout, cx).await;
    fixture
        .view
        .update(cx, |app, cx| app.refresh_diff_totals(cx));
    cx.wait_for(fixture.handle, Duration::from_secs(20), |_, cx| {
        fixture
            .view
            .read(cx)
            .diff_totals
            .get(&fixture.checkout)
            .is_some_and(|totals| totals.additions > 0)
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        // The figures answer on the pointer going down, which is the row's own
        // gesture rather than the toolkit's click, and they are open under the
        // pointer, which is where a person finds them.
        window.hover(format!("select-checkout-{}", fixture.checkout), cx);
        window.render_frame(cx);
        let figures = window
            .find(format!("diff-figures-{}", fixture.checkout))
            .bounds();
        pointer_down(window, figures.center(), cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(app.open_diff, Some((fixture.checkout.clone(), None)));
    });
    close(&fixture, cx);
}

/// A terminal that was asked for is the view, even over a preview, and a preview
/// opened while its shell was spawning is not taken back when that shell lands.
#[gpui_kit::test]
async fn a_new_terminal_takes_the_preview_and_a_later_preview_keeps_the_keyboard(
    cx: &mut TestAppContext,
) {
    let fixture = open(cx);
    let preview = |path: &str| FilePreview {
        path: path.to_owned(),
        text: "preview\n".into(),
    };
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.open_document = Some((fixture.checkout.clone(), preview("first.txt")));
            app.request_terminal(&fixture.checkout, window, cx);
        });
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert!(
            app.open_document.is_none(),
            "the request left the preview it was meant to replace on screen"
        );
        assert_eq!(app.navigation.pending.len(), 1);
    });
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture.view.read(cx).terminals.len() == 3
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        let active = app.navigation.active_session.clone().unwrap();
        assert!(app.terminals.contains_key(&active));
        assert!(app.open_document.is_none());
    });
    // Now the other half of the race: the preview is opened while that shell is
    // still spawning, and it is still there when the shell lands.
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.request_terminal(&fixture.checkout, window, cx)
        });
        fixture.view.update(cx, |app, cx| {
            app.open_document = Some((fixture.checkout.clone(), preview("second.txt")));
            app.document_request = app.document_request.wrapping_add(1);
            cx.notify();
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture.view.read(cx).terminals.len() == 4
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(
            app.open_document
                .as_ref()
                .map(|(_, preview)| preview.path.as_str()),
            Some("second.txt"),
            "a terminal that finished spawning took a preview opened after it"
        );
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        assert!(
            !fixture
                .view
                .read_with(cx, |app, cx| app.terminal().is_some_and(|terminal| {
                    terminal.read(cx).focus_handle(cx).is_focused(window)
                })),
            "a late terminal took the keyboard from the preview the person is reading"
        );
    })
    .unwrap();
    dispose(&fixture, cx);
}

#[gpui_kit::test]
fn double_click_rename_accepts_enter_and_escape_discards_edit(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let id = fixture.sessions[1].clone();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.double_click(SharedString::from(format!("select-session-{id}")), cx);
        window.input("renamed", cx);
        window.press("enter", cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert!(app.editing.is_none());
        assert_eq!(app.navigation.session(&id).unwrap().name, "renamed");
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.double_click(SharedString::from(format!("select-session-{id}")), cx);
        window.input("discarded", cx);
        window.press("escape", cx);
    })
    .unwrap();
    assert_eq!(
        fixture.view.read_with(cx, |app, _| app
            .navigation
            .session(&id)
            .unwrap()
            .name
            .clone()),
        "renamed"
    );
    dispose(&fixture, cx);
}

#[gpui_kit::test]
fn moving_a_row_preserves_pty_and_view_identity(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let target_dir = tempfile::tempdir().unwrap();
    let repo = Repo::plain(target_dir.path(), "target").unwrap();
    let target = repo.checkouts[0].id.clone();
    let id = fixture.sessions[0].clone();
    let entity = fixture
        .view
        .read_with(cx, |app, _| app.terminals[&id].view.entity_id());
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.navigation.merge(
                navigation::OpenedDirectory {
                    repo,
                    checkout_id: target.clone(),
                },
                false,
            );
            app.move_terminal(&id, &target, 0, window, cx);
        });
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(app.terminals[&id].view.entity_id(), entity);
        assert_eq!(app.navigation.session(&id).unwrap().checkout_id, target);
        assert_eq!(app.navigation.active_session.as_deref(), Some(id.as_str()));
        assert!(app.toasts.latest_info().unwrap().contains("no cd was sent"));
    });
    assert!(fixture.backend.status(&id).is_ok());
    dispose(&fixture, cx);
}

#[gpui_kit::test]
fn sidebar_drag_reorders_a_terminal_and_retains_its_pty(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let id = fixture.sessions[0].clone();
    let other = fixture.sessions[1].clone();
    let entity = fixture
        .view
        .read_with(cx, |app, _| app.terminals[&id].view.entity_id());
    cx.update_window(fixture.handle, |_, window, cx| {
        let from_bounds = window
            .find(SharedString::from(format!("select-session-{id}")))
            .bounds();
        let to_bounds = window
            .find(SharedString::from(format!("select-session-{other}")))
            .bounds();
        drag_pointer(
            window,
            from_bounds.center(),
            gpui_kit::point(to_bounds.center().x, to_bounds.bottom() - px(3.)),
            cx,
        );
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        let order = &app.navigation.checkout(&fixture.checkout).unwrap().sessions;
        assert_eq!(order[0].id, other);
        assert_eq!(order[1].id, id);
        assert_eq!(
            app.navigation.active_session.as_deref(),
            Some(other.as_str())
        );
        assert_eq!(app.terminals[&id].view.entity_id(), entity);
    });
    assert!(fixture.backend.status(&id).is_ok());
    assert!(fixture.backend.status(&other).is_ok());
    dispose(&fixture, cx);
}

#[gpui_kit::test]
fn sidebar_drag_moves_a_terminal_to_another_checkout_without_restarting_it(
    cx: &mut TestAppContext,
) {
    let fixture = open(cx);
    let target_dir = tempfile::tempdir().unwrap();
    let repo = Repo::plain(target_dir.path(), "target").unwrap();
    let target = repo.checkouts[0].id.clone();
    let id = fixture.sessions[0].clone();
    let entity = fixture
        .view
        .read_with(cx, |app, _| app.terminals[&id].view.entity_id());
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.navigation.merge(
                navigation::OpenedDirectory {
                    repo,
                    checkout_id: target.clone(),
                },
                false,
            );
            cx.notify();
        });
        window.render_frame(cx);
        let from = window
            .find(SharedString::from(format!("select-session-{id}")))
            .bounds()
            .center();
        let to = window
            .find(SharedString::from(format!("new-terminal-{target}")))
            .bounds()
            .center();
        pointer_move(window, from, None, cx);
        pointer_down(window, from, cx);
        pointer_move(window, to, Some(MouseButton::Left), cx);
        assert_eq!(
            fixture
                .view
                .read(cx)
                .sidebar_drag
                .as_ref()
                .and_then(|drag| drag.target.as_ref())
                .map(|target| (target.checkout_id.as_str(), target.index)),
            Some((target.as_str(), 0))
        );
        pointer_up(window, to, cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(app.navigation.session(&id).unwrap().checkout_id, target);
        assert_eq!(
            app.navigation.checkout(&fixture.checkout).unwrap().sessions[0].id,
            fixture.sessions[1]
        );
        assert_eq!(app.navigation.checkout(&target).unwrap().sessions[0].id, id);
        assert_eq!(app.terminals[&id].checkout_id, target);
        assert_eq!(app.terminals[&id].view.entity_id(), entity);
        assert_eq!(app.navigation.active_session.as_deref(), Some(id.as_str()));
    });
    assert!(fixture.backend.status(&id).is_ok());
    assert!(fixture.backend.status(&fixture.sessions[1]).is_ok());
    dispose(&fixture, cx);
}

#[gpui_kit::test]
fn sidebar_click_stays_a_click_below_the_five_pixel_drag_threshold(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let id = fixture.sessions[0].clone();
    let other = fixture.sessions[1].clone();
    cx.update_window(fixture.handle, |_, window, cx| {
        let bounds = window
            .find(SharedString::from(format!("select-session-{id}")))
            .bounds();
        let from = bounds.center();
        let near = gpui_kit::point(from.x + px(4.), from.y);
        pointer_move(window, from, None, cx);
        pointer_down(window, from, cx);
        pointer_move(window, near, Some(MouseButton::Left), cx);
        assert!(!fixture.view.read(cx).sidebar_drag.as_ref().unwrap().started);
        pointer_up(window, near, cx);
        assert!(fixture.view.read(cx).sidebar_drag.is_none());
        assert_eq!(
            fixture.view.read(cx).navigation.active_session.as_deref(),
            Some(id.as_str())
        );

        let from = window
            .find(SharedString::from(format!("select-session-{id}")))
            .bounds()
            .center();
        let threshold = gpui_kit::point(from.x + px(5.), from.y);
        pointer_move(window, from, None, cx);
        pointer_down(window, from, cx);
        pointer_move(window, threshold, Some(MouseButton::Left), cx);
        assert!(fixture.view.read(cx).sidebar_drag.as_ref().unwrap().started);
        window.press("escape", cx);
        assert!(fixture.view.read(cx).sidebar_drag.is_none());
        pointer_up(window, threshold, cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        let order = &app.navigation.checkout(&fixture.checkout).unwrap().sessions;
        assert_eq!(order[0].id, id);
        assert_eq!(order[1].id, other);
        assert_eq!(app.navigation.active_session.as_deref(), Some(id.as_str()));
    });
    dispose(&fixture, cx);
}

#[gpui_kit::test]
fn sidebar_drag_autoscrolls_near_the_scroll_viewport_edge(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let id = fixture.sessions[0].clone();
    let directory = tempfile::tempdir().unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            for index in 0..16 {
                let path = directory.path().join(format!("root-{index}"));
                std::fs::create_dir(&path).unwrap();
                let repo = Repo::plain(&path, format!("extra-{index}")).unwrap();
                let checkout_id = repo.checkouts[0].id.clone();
                app.navigation
                    .merge(navigation::OpenedDirectory { repo, checkout_id }, false);
            }
            cx.notify();
        });
        window.render_frame(cx);
        assert!(fixture.view.read(cx).scroll.max_offset().y > px(0.));
        let from = window
            .find(SharedString::from(format!("select-session-{id}")))
            .bounds()
            .center();
        let viewport = fixture.view.read(cx).scroll.bounds();
        let edge = gpui_kit::point(from.x, viewport.bottom() - px(4.));
        pointer_move(window, from, None, cx);
        pointer_down(window, from, cx);
        pointer_move(window, edge, Some(MouseButton::Left), cx);
        for _ in 0..3 {
            window.simulate_next_frame(cx);
            window.render_frame(cx);
        }
        assert!(fixture.view.read(cx).scroll.offset().y < px(0.));
        window.press("escape", cx);
        pointer_up(window, edge, cx);
    })
    .unwrap();
    assert_eq!(
        fixture.view.read_with(cx, |app, _| app
            .navigation
            .session(&id)
            .unwrap()
            .checkout_id
            .clone()),
        fixture.checkout
    );
    assert!(fixture.backend.status(&id).is_ok());
    dispose(&fixture, cx);
}

#[gpui_kit::test]
fn escape_cancels_a_sidebar_drag_without_reordering_or_selecting_the_drop_row(
    cx: &mut TestAppContext,
) {
    let fixture = open(cx);
    let id = fixture.sessions[0].clone();
    let other = fixture.sessions[1].clone();
    cx.update_window(fixture.handle, |_, window, cx| {
        let from = window
            .find(SharedString::from(format!("select-session-{id}")))
            .bounds()
            .center();
        let target = window
            .find(SharedString::from(format!("select-session-{other}")))
            .bounds();
        let to = gpui_kit::point(target.center().x, target.bottom() - px(3.));
        pointer_move(window, from, None, cx);
        pointer_down(window, from, cx);
        pointer_move(window, to, Some(MouseButton::Left), cx);
        assert!(fixture.view.read(cx).sidebar_drag.as_ref().unwrap().started);
        window.press("escape", cx);
        assert!(fixture.view.read(cx).sidebar_drag.is_none());
        pointer_up(window, to, cx);

        let from = window
            .find(SharedString::from(format!("select-session-{id}")))
            .bounds()
            .center();
        let target = window
            .find(SharedString::from(format!("select-session-{other}")))
            .bounds();
        let to = gpui_kit::point(target.center().x, target.bottom() - px(3.));
        pointer_move(window, from, None, cx);
        pointer_down(window, from, cx);
        pointer_move(window, to, Some(MouseButton::Left), cx);
        assert!(fixture.view.read(cx).sidebar_drag.as_ref().unwrap().started);
        pointer_exit(window, to, cx);
        assert!(fixture.view.read(cx).sidebar_drag.is_none());
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        let order = &app.navigation.checkout(&fixture.checkout).unwrap().sessions;
        assert_eq!(order[0].id, id);
        assert_eq!(order[1].id, other);
        assert_eq!(
            app.navigation.active_session.as_deref(),
            Some(other.as_str())
        );
    });
    dispose(&fixture, cx);
}

/// A window over one real Git repository, with no terminals of its own.
/// Named with a leading underscore because it exists to be dropped: the
/// worktree lives inside it and the row is read from the window, not the disk.
pub(super) struct GitFixture {
    _directory: TempDir,
    pub(super) handle: AnyWindowHandle,
    pub(super) view: Entity<TerminalWindow>,
    pub(super) root: PathBuf,
    pub(super) repo_id: String,
    pub(super) checkout: String,
}

pub(super) fn open_git(cx: &mut TestAppContext) -> GitFixture {
    let directory = tempfile::tempdir().unwrap();
    let root = git_repository(directory.path());
    let opened = navigation::open_directory(&root).unwrap();
    let repo_id = opened.repo.id.clone();
    let checkout = opened.checkout_id.clone();
    cx.update(|cx| {
        gpui_kit::init(cx);
        load_fonts(cx);
        crate::theme::install(cx);
    });
    let (handle, view) = cx.update(|cx| {
        gpui_kit::open_window(window_bounds(cx), cx, |_window, cx| {
            cx.new(|cx| {
                let mut app = TerminalWindow::empty(Arc::new(TerminalBackend::default()), true, cx);
                app.navigation.merge(opened, true);
                app
            })
        })
        .unwrap()
    });
    cx.update_window(handle, |_, window, cx| {
        window.activate_window();
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    GitFixture {
        _directory: directory,
        handle,
        view,
        root,
        repo_id,
        checkout,
    }
}

pub(super) fn close(fixture: &GitFixture, cx: &mut TestAppContext) {
    cx.update_window(fixture.handle, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    fixture.view.update(cx, |app, _| app.owner.stop());
}

pub(super) fn add_worktree(fixture: &GitFixture, cx: &mut TestAppContext) -> (String, PathBuf) {
    let repo = fixture.view.read_with(cx, |app, _| {
        app.navigation
            .repos
            .iter()
            .find(|repo| repo.id == fixture.repo_id)
            .unwrap()
            .clone()
    });
    let opened = navigation::create_worktree(&repo, "task", "feature/task", "").unwrap();
    let checkout = opened
        .repo
        .checkouts
        .iter()
        .find(|checkout| checkout.id == opened.checkout_id)
        .unwrap();
    let id = checkout.id.clone();
    let path = PathBuf::from(&checkout.path);
    fixture
        .view
        .update(cx, |app, _| app.navigation.merge(opened, false));
    (id, path)
}

#[gpui_kit::test]
async fn archiving_keeps_worktree_files_and_git_registration(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let (id, path) = add_worktree(&fixture, cx);
    let saved_file = path.join("keep me.txt");
    std::fs::write(&saved_file, "archive keeps files").unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.hover(SharedString::from(format!("select-checkout-{id}")), cx);
        window.render_frame(cx);
        window.click(SharedString::from(format!("worktree-actions-{id}")), cx);
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .worktree_removal
            .as_ref()
            .is_some_and(|draft| draft.info.is_some())
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.find("worktree-archive");
        window.click("worktree-archive", cx);
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture.view.read(cx).navigation.archived.contains(&id)
    })
    .await;

    assert!(path.is_dir());
    assert_eq!(
        std::fs::read_to_string(saved_file).unwrap(),
        "archive keeps files"
    );
    run_git(
        &fixture.root,
        &["show-ref", "--verify", "--quiet", "refs/heads/feature/task"],
    );
    let listed = Command::new("git")
        .current_dir(&fixture.root)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .unwrap();
    assert!(listed.status.success());
    assert!(String::from_utf8_lossy(&listed.stdout).contains(path.to_str().unwrap()));
    close(&fixture, cx);
}

#[gpui_kit::test]
fn stale_diff_totals_require_the_same_revision_and_checkout_membership(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    fixture.view.update(cx, |app, cx| {
        let id = fixture.checkout.clone();
        let path = app.navigation.checkout(&id).unwrap().path.clone();
        let stale_revision = app.membership_revision;
        app.membership_revision += 1;
        app.apply_diff_totals(
            stale_revision,
            vec![(
                id.clone(),
                path.clone(),
                Ok(DiffTotals {
                    additions: 9,
                    deletions: 3,
                }),
            )],
            cx,
        );
        assert!(!app.diff_totals.contains_key(&id));

        let current_revision = app.membership_revision;
        app.apply_diff_totals(
            current_revision,
            vec![(
                id.clone(),
                path.clone(),
                Ok(DiffTotals {
                    additions: 4,
                    deletions: 1,
                }),
            )],
            cx,
        );
        assert_eq!(app.diff_totals[&id].additions, 4);
        app.diff_totals.remove(&id);
        app.navigation.repos[0]
            .checkouts
            .retain(|checkout| checkout.id != id);
        app.apply_diff_totals(
            current_revision,
            vec![(id.clone(), path, Ok(DiffTotals::default()))],
            cx,
        );
        assert!(!app.diff_totals.contains_key(&id));
    });
    close(&fixture, cx);
}

#[gpui_kit::test]
fn late_worktree_defaults_cannot_update_a_reopened_dialog(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.open_worktree_dialog(&fixture.repo_id, window, cx);
            let stale_request = app.worktree.as_ref().unwrap().request_id;
            close_worktree_dialog(app, cx);
            app.open_worktree_dialog(&fixture.repo_id, window, cx);
            let current_request = app.worktree.as_ref().unwrap().request_id;
            assert_ne!(stale_request, current_request);

            app.apply_worktree_defaults(
                &fixture.repo_id,
                stale_request,
                Err("late response".into()),
                window,
                cx,
            );
            let draft = app.worktree.as_ref().unwrap();
            assert_eq!(draft.request_id, current_request);
            assert!(draft.error.is_none());
            assert!(draft.base.read(cx).value().is_empty());
            close_worktree_dialog(app, cx);
        });
    })
    .unwrap();
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn archive_does_not_wait_for_a_git_removal_preview(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let (id, path) = add_worktree(&fixture, cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.worktree_removal = Some(WorktreeRemovalDraft {
                request_id: NEXT_SESSION.fetch_add(1, Ordering::Relaxed),
                repo_id: fixture.repo_id.clone(),
                checkout_id: id.clone(),
                info: None,
                loading: true,
                busy: false,
                dirty_confirmed: false,
                unmerged_confirmed: false,
                delete_branch: false,
                error: None,
            });
            cx.notify();
        });
        window.render_frame(cx);
        window.find("worktree-archive");
        window.click("worktree-archive", cx);
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture.view.read(cx).navigation.archived.contains(&id)
    })
    .await;
    assert!(path.is_dir());
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn detached_worktree_delete_requires_acknowledging_unreferenced_commits(
    cx: &mut TestAppContext,
) {
    let fixture = open_git(cx);
    let path = fixture._directory.path().join("detached");
    run_git(
        &fixture.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            path.to_str().unwrap(),
            "HEAD",
        ],
    );
    std::fs::write(path.join("detached.txt"), "detached commit").unwrap();
    run_git(&path, &["add", "detached.txt"]);
    run_git(
        &path,
        &[
            "-c",
            "user.email=muster@example.test",
            "-c",
            "user.name=Muster",
            "commit",
            "--quiet",
            "-m",
            "detached-only commit",
        ],
    );
    let opened = navigation::open_directory(&path).unwrap();
    let id = opened.checkout_id.clone();
    fixture
        .view
        .update(cx, |app, _| app.navigation.merge(opened, false));
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.open_worktree_removal_dialog(&fixture.repo_id, &id, window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .worktree_removal
            .as_ref()
            .is_some_and(|draft| {
                draft
                    .info
                    .as_ref()
                    .is_some_and(|info| info.branch.is_none() && info.unmerged_commits == 1)
            })
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        assert!(!app.worktree_removal.as_ref().unwrap().unmerged_confirmed);
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture
            .view
            .update(cx, |app, cx| app.submit_worktree_delete(window, cx));
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert!(app.worktree_removal.as_ref().unwrap().error.is_some());
    });
    assert!(path.is_dir());

    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.find("worktree-confirm-unmerged");
        window.click("worktree-confirm-unmerged", cx);
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .worktree_removal
            .as_ref()
            .is_some_and(|draft| draft.unmerged_confirmed)
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("worktree-delete", cx);
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(30), |_, cx| {
        let app = fixture.view.read(cx);
        app.worktree_removal.is_none() && app.navigation.checkout(&id).is_none()
    })
    .await;
    assert!(!path.exists());
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn dirty_worktree_delete_needs_ack_and_preserves_branch_by_default(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let (id, path) = add_worktree(&fixture, cx);
    run_git(
        &path,
        &[
            "-c",
            "user.email=muster@example.test",
            "-c",
            "user.name=Muster",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "unmerged worktree commit",
        ],
    );
    std::fs::write(path.join("uncommitted file.txt"), "unsaved").unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.hover(SharedString::from(format!("select-checkout-{id}")), cx);
        window.render_frame(cx);
        window.click(SharedString::from(format!("worktree-actions-{id}")), cx);
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .worktree_removal
            .as_ref()
            .is_some_and(|draft| {
                draft.info.as_ref().is_some_and(|info| {
                    info.dirty_files
                        .iter()
                        .any(|file| file == "uncommitted file.txt")
                        && info.unmerged_commits == 1
                })
            })
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        let draft = app.worktree_removal.as_ref().unwrap();
        assert!(!draft.dirty_confirmed);
        assert!(!draft.delete_branch);
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("worktree-delete-branch", cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert!(app.worktree_removal.as_ref().unwrap().delete_branch);
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("worktree-delete-branch", cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert!(!app.worktree_removal.as_ref().unwrap().delete_branch);
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.find("worktree-confirm-dirty");
        window.find("worktree-delete-branch");
        window.find("worktree-delete");
        fixture
            .view
            .update(cx, |app, cx| app.submit_worktree_delete(window, cx));
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert!(app.worktree_removal.as_ref().unwrap().error.is_some());
    });
    assert!(path.is_dir(), "an unconfirmed delete removed the checkout");

    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("worktree-confirm-dirty", cx);
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert!(app.worktree_removal.as_ref().unwrap().dirty_confirmed);
    });
    cx.run_until_parked();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("worktree-confirm-dirty").checked(), Some(true));
        window.click("worktree-delete", cx);
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(30), |_, cx| {
        let app = fixture.view.read(cx);
        app.worktree_removal.is_none() && app.navigation.checkout(&id).is_none()
    })
    .await;
    assert!(!path.exists());
    run_git(
        &fixture.root,
        &["show-ref", "--verify", "--quiet", "refs/heads/feature/task"],
    );
    close(&fixture, cx);
}

fn removal_attempt(
    app: &mut TerminalWindow,
    repo_id: &str,
    id: &str,
    cx: &mut Context<TerminalWindow>,
) -> RemovalCheck {
    app.worktree_removal = Some(WorktreeRemovalDraft {
        request_id: 100,
        repo_id: repo_id.into(),
        checkout_id: id.into(),
        info: None,
        loading: false,
        busy: false,
        dirty_confirmed: false,
        unmerged_confirmed: false,
        delete_branch: false,
        error: None,
    });
    app.reserve_worktree_removal(id, 100, cx).unwrap()
}

#[gpui_kit::test]
fn removal_reserves_before_waiting_and_rechecks_pending_navigation_pi_and_identity(
    cx: &mut TestAppContext,
) {
    let fixture = open_git(cx);
    let (id, path) = add_worktree(&fixture, cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let check = removal_attempt(app, &fixture.repo_id, &id, cx);
            assert!(app.removing_checkouts.contains(&id));
            app.request_terminal(&id, window, cx);
            assert!(!app
                .navigation
                .pending
                .values()
                .any(|checkout| checkout == &id));
            // A pending request accepted before the reservation can complete during HTTP.
            app.navigation
                .pending
                .insert("earlier-shell".into(), id.clone());
            app.archive_worktree(check, cx);
            assert!(!app.navigation.archived.contains(&id));
            assert!(!app.removing_checkouts.contains(&id));
            assert!(app
                .worktree_removal
                .as_ref()
                .unwrap()
                .error
                .as_deref()
                .unwrap()
                .contains("pending"));
            app.navigation.pending.clear();

            let check = removal_attempt(app, &fixture.repo_id, &id, cx);
            app.navigation.home = Some(id.clone());
            app.archive_worktree(check, cx);
            assert!(!app.navigation.archived.contains(&id));
            app.navigation.home = None;

            let check = removal_attempt(app, &fixture.repo_id, &id, cx);
            let program = fixture._directory.path().join("pi");
            std::os::unix::fs::symlink("/bin/sh", &program).unwrap();
            let backend = std::sync::Arc::clone(app.owner.backend());
            backend
                .spawn(
                    "pi-started-during-check".into(),
                    crate::pty::SpawnOptions {
                        program,
                        args: vec!["-c".into(), "exec sleep 30".into()],
                        cwd: path.clone(),
                        cols: 80,
                        rows: 24,
                        env: vec![],
                        reader_end: None,
                    },
                    Box::new(|_| Ok(())),
                )
                .unwrap();
            assert!(app.checkout_has_running_pi(&id));
            app.archive_worktree(check, cx);
            assert!(!app.navigation.archived.contains(&id));
            assert!(app
                .worktree_removal
                .as_ref()
                .unwrap()
                .error
                .as_deref()
                .unwrap()
                .contains("Pi"));
            backend.close("pi-started-during-check").unwrap();

            let check = removal_attempt(app, &fixture.repo_id, &id, cx);
            let displaced = path.with_extension("original");
            std::fs::rename(&path, &displaced).unwrap();
            std::fs::create_dir(&path).unwrap();
            app.archive_worktree(check, cx);
            assert!(!app.navigation.archived.contains(&id));
            assert!(!app.removing_checkouts.contains(&id));
            std::fs::remove_dir(&path).unwrap();
            std::fs::rename(&displaced, &path).unwrap();
        });
    })
    .unwrap();
    assert!(path.join(".git").exists());
    close(&fixture, cx);
}

#[gpui_kit::test]
fn delete_completion_rechecks_activity_that_arrived_during_network_wait(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let (id, path) = add_worktree(&fixture, cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let repo = app
                .navigation
                .repos
                .iter()
                .find(|repo| repo.id == fixture.repo_id)
                .unwrap()
                .clone();
            let checkout = app.navigation.checkout(&id).unwrap().clone();
            let info = navigation::worktree_removal_info(&repo, &checkout).unwrap();
            let confirmation = muster_core::navigation::WorktreeRemovalConfirmation {
                checkout_id: info.checkout_id,
                path: info.path,
                branch: info.branch,
                head: info.head,
                dirty_files: info.dirty_files,
                default_ref: info.default_ref,
                unmerged_commits: info.unmerged_commits,
                confirm_dirty: false,
                confirm_unmerged: false,
                delete_branch: false,
            };
            let check = removal_attempt(app, &fixture.repo_id, &id, cx);
            app.navigation
                .pending
                .insert("started-before-reservation".into(), id.clone());
            app.remove_worktree_now(repo, checkout, confirmation, check, window, cx);
            assert!(app.navigation.checkout(&id).is_some());
            assert!(!app.removing_checkouts.contains(&id));
            assert!(app.worktree_removal.as_ref().unwrap().error.is_some());
            app.navigation.pending.clear();
        });
    })
    .unwrap();
    cx.run_until_parked();
    assert!(
        path.join(".git").exists(),
        "Git deletion must never start after failed revalidation"
    );
    close(&fixture, cx);
}

/// A worktree whose OpenCode activity cannot be read is not archived.
///
/// The service is asked before anything is archived, and an answer that could not be
/// read is not an idle session: the worktree waits for the person instead.
#[gpui_kit::test]
fn a_worktree_whose_opencode_activity_cannot_be_read_is_not_archived(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let (id, _path) = add_worktree(&fixture, cx);
    // A service that lists a session for the worktree but cannot read the pending
    // lists: nothing says whether that session is working.
    let mock = crate::agent_watch::mock::Mock::start(|request| {
        let directory = request.query_param("directory").unwrap_or_default();
        let (status, data) = match request.path.as_str() {
            "/api/session" => (
                200,
                serde_json::json!([{ "id": "ses_external", "location": { "directory": directory } }]),
            ),
            "/api/form" | "/api/permission/request" => (500, serde_json::json!({})),
            _ => (200, serde_json::json!({})),
        };
        crate::agent_watch::mock::reply(
            status,
            serde_json::json!({ "data": data, "location": { "directory": directory } }),
        )
    });
    let home = tempfile::tempdir().unwrap();
    let state = home.path().join(".local/state/opencode");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(
        state.join("service.json"),
        serde_json::json!({
            "url": format!("http://127.0.0.1:{}", mock.port()),
            "password": "secret",
            "version": "2.0.22",
        })
        .to_string(),
    )
    .unwrap();

    cx.update_window(fixture.handle, |_, _window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.service_home = Some(home.path().to_path_buf());
            app.worktree_removal = Some(WorktreeRemovalDraft {
                request_id: NEXT_SESSION.fetch_add(1, Ordering::Relaxed),
                repo_id: fixture.repo_id.clone(),
                checkout_id: id.clone(),
                info: None,
                loading: false,
                busy: false,
                dirty_confirmed: false,
                unmerged_confirmed: false,
                delete_branch: false,
                error: None,
            });
            app.submit_worktree_archive(cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
    fixture.view.read_with(cx, |app, _| {
        assert!(
            !app.navigation.archived.contains(&id),
            "a worktree whose activity could not be read was archived"
        );
        let error = app
            .worktree_removal
            .as_ref()
            .and_then(|draft| draft.error.as_deref())
            .unwrap_or_default();
        assert!(
            error.contains("could not be verified"),
            "the refusal says why: {error}"
        );
    });
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn pending_terminal_creation_blocks_archive_and_delete(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let (id, path) = add_worktree(&fixture, cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.navigation
                .pending
                .insert("pending-shell".into(), id.clone());
            app.open_worktree_removal_dialog(&fixture.repo_id, &id, window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .worktree_removal
            .as_ref()
            .is_some_and(|draft| draft.info.is_some())
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture
            .view
            .update(cx, |app, cx| app.submit_worktree_delete(window, cx));
        fixture
            .view
            .update(cx, |app, cx| app.submit_worktree_archive(cx));
        fixture.view.update(cx, |app, cx| {
            app.navigation.pending.remove("pending-shell");
            app.navigation
                .repos
                .iter_mut()
                .flat_map(|repo| &mut repo.checkouts)
                .find(|checkout| checkout.id == id)
                .unwrap()
                .sessions
                .push(Session {
                    id: "active-terminal".into(),
                    checkout_id: id.clone(),
                    name: "sh".into(),
                    created_at: "test".into(),
                    session_type: SessionType::Shell,
                    status: SavedSessionStatus::Active,
                });
            app.submit_worktree_delete(window, cx);
            app.submit_worktree_archive(cx);
            assert!(app.removing_checkouts.insert(id.clone()));
            app.request_terminal(&id, window, cx);
            assert!(app
                .navigation
                .pending
                .values()
                .all(|pending| pending != &id));
        });
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert!(app.navigation.checkout(&id).is_some());
        assert!(!app.navigation.archived.contains(&id));
        assert!(app.worktree_removal.as_ref().unwrap().error.is_some());
    });
    assert!(path.is_dir());
    close(&fixture, cx);
}

#[gpui_kit::test]
fn an_owned_terminal_blocks_lifecycle_even_if_its_sidebar_session_is_stale(
    cx: &mut TestAppContext,
) {
    let fixture = open(cx);
    fixture.view.update(cx, |app, _| {
        app.navigation
            .repos
            .iter_mut()
            .flat_map(|repo| &mut repo.checkouts)
            .find(|checkout| checkout.id == fixture.checkout)
            .unwrap()
            .sessions
            .clear();
        assert!(app.checkout_has_terminal_activity(&fixture.checkout));
    });
    dispose(&fixture, cx);
}

#[gpui_kit::test]
async fn a_worktree_is_created_from_the_dialog_and_a_shell_opens_in_it(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.open_worktree_dialog(&fixture.repo_id, window, cx)
        });
    })
    .unwrap();
    // The dialog's own answer from Git has to arrive before it can be created.
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .worktree
            .as_ref()
            .is_some_and(|draft| draft.location.is_some() && draft.base.read(cx).value() == "main")
    })
    .await;
    fixture.view.read_with(cx, |app, cx| {
        let draft = app.worktree.as_ref().expect("the dialog is open");
        assert_eq!(draft.base.read(cx).value(), "main");
        assert_eq!(draft.task.read(cx).value(), "new-task");
        assert_eq!(draft.branch.read(cx).value(), "feature/new-task");
    });
    cx.update_window(fixture.handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.update_window(fixture.handle, |_, window, _| {
        window.find("worktree-create");
        window.find("worktree-cancel");
    })
    .unwrap();

    // Typing a task name carries into the branch that is still only a suggestion.
    cx.update_window(fixture.handle, |_, window, cx| {
        window.input("checkout flow", cx)
    })
    .unwrap();
    fixture.view.read_with(cx, |app, cx| {
        let draft = app.worktree.as_ref().unwrap();
        assert_eq!(draft.branch.read(cx).value(), "feature/checkout-flow");
        assert!(!draft.branch_edited);
    });

    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("worktree-create", cx);
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(30), |_, cx| {
        let app = fixture.view.read(cx);
        app.worktree.is_none() && app.terminals.len() == 1
    })
    .await;

    let worktree = fixture.view.read_with(cx, |app, _| {
        let checkout = app
            .navigation
            .repos
            .iter()
            .flat_map(|repo| repo.checkouts.iter())
            .find(|checkout| checkout.branch.as_deref() == Some("feature/checkout-flow"))
            .expect("the new worktree has a row");
        PathBuf::from(&checkout.path)
    });
    assert_eq!(
        worktree.parent().and_then(|parent| parent.file_name()),
        fixture
            .root
            .canonicalize()
            .unwrap()
            .join(".worktrees")
            .file_name(),
        "the worktree did not land under its repository's .worktrees"
    );
    assert!(
        worktree.is_dir(),
        "Git did not create the worktree directory"
    );
    assert!(worktree.join(".git").exists());
    fixture.view.read_with(cx, |app, _| {
        let checkout = app
            .navigation
            .repos
            .iter()
            .flat_map(|repo| repo.checkouts.iter())
            .find(|checkout| checkout.branch.as_deref() == Some("feature/checkout-flow"))
            .expect("the new worktree has a row");
        assert_eq!(
            checkout.sessions.len(),
            1,
            "the shell opened in the worktree"
        );
        assert_eq!(
            app.navigation.active_checkout.as_deref(),
            Some(checkout.id.as_str())
        );
        assert_ne!(checkout.id, fixture.checkout);
    });
    assert_eq!(
        worktree.parent().and_then(|parent| parent.file_name()),
        fixture
            .root
            .canonicalize()
            .unwrap()
            .join(".worktrees")
            .file_name(),
        "the worktree did not land under its repository's .worktrees"
    );
    assert!(
        worktree.is_dir(),
        "Git did not create the worktree directory"
    );
    assert!(worktree.join(".git").exists());
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn creating_a_worktree_is_allowed_while_another_checkout_is_missing(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let missing_path = fixture._directory.path().join("missing-worktree");
    run_git(
        &fixture.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "stale",
            missing_path.to_str().unwrap(),
        ],
    );
    let missing_path = missing_path.canonicalize().unwrap();
    std::fs::remove_dir_all(&missing_path).unwrap();
    let opened = navigation::open_directory(&fixture.root).unwrap();
    let missing = opened
        .repo
        .checkouts
        .iter()
        .find(|checkout| checkout.path == missing_path.to_str().unwrap())
        .expect("Git still lists the unavailable worktree");
    assert!(missing.is_missing);
    let missing_id = missing.id.clone();
    fixture
        .view
        .update(cx, |app, _| app.navigation.merge(opened, false));

    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.open_worktree_dialog(&fixture.repo_id, window, cx)
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture.view.read_with(cx, |app, cx| {
            app.worktree
                .as_ref()
                .is_some_and(|draft| draft.base.read(cx).value() == "main")
        })
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture
            .view
            .update(cx, |app, cx| app.submit_worktree(window, cx));
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(30), |_, cx| {
        fixture.view.read(cx).navigation.repos.iter().any(|repo| {
            repo.checkouts
                .iter()
                .any(|checkout| checkout.branch.as_deref() == Some("feature/new-task"))
        })
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        assert!(app.worktree.is_none());
        assert!(app.navigation.checkout(&missing_id).unwrap().is_missing);
    });
    assert!(fixture.root.join(".worktrees/new-task").is_dir());
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn a_missing_default_branch_keeps_manual_base_creation_available(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    run_git(&fixture.root, &["branch", "-m", "trunk"]);
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.open_worktree_dialog(&fixture.repo_id, window, cx)
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture.view.read_with(cx, |app, _| {
            app.worktree.as_ref().is_some_and(|draft| {
                draft.location.is_some()
                    && draft
                        .error
                        .as_deref()
                        .is_some_and(|error| error.contains("default branch"))
            })
        })
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        let draft = app.worktree.as_ref().unwrap();
        assert_eq!(
            draft.location.as_deref(),
            Some(
                fixture
                    .root
                    .canonicalize()
                    .unwrap()
                    .join(".worktrees")
                    .as_path()
            )
        );
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let base = app.worktree.as_ref().unwrap().base.clone();
            base.update(cx, |input, cx| input.set_value("trunk", window, cx));
            app.submit_worktree(window, cx);
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(30), |_, cx| {
        fixture.view.read(cx).navigation.repos.iter().any(|repo| {
            repo.checkouts
                .iter()
                .any(|checkout| checkout.branch.as_deref() == Some("feature/new-task"))
        })
    })
    .await;
    assert!(fixture.root.join(".worktrees/new-task").is_dir());
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn a_worktree_with_nothing_to_start_from_is_refused_before_git_is_asked(
    cx: &mut TestAppContext,
) {
    let fixture = open_git(cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            app.open_worktree_dialog(&fixture.repo_id, window, cx)
        });
    })
    .unwrap();
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .worktree
            .as_ref()
            .is_some_and(|draft| draft.location.is_some() && draft.base.read(cx).value() == "main")
    })
    .await;

    // A person clears the base because Git named the wrong one.
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.view.update(cx, |app, cx| {
            let base = app.worktree.as_ref().unwrap().base.clone();
            base.update(cx, |input, cx| input.set_value(String::new(), window, cx));
        });
        window.render_frame(cx);
        window.find("worktree-create");
    })
    .unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture
            .view
            .update(cx, |app, cx| app.submit_worktree(window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    fixture.view.read_with(cx, |app, _| {
        let draft = app.worktree.as_ref().expect("the dialog refused to close");
        assert!(!draft.busy);
        assert!(draft.error.is_some(), "a refused creation needs a reason");
    });
    assert!(
        !fixture.root.join(".worktrees").exists(),
        "Git was asked before the dialog had an answer"
    );

    // Escape gives up on a dialog that has not touched Git yet.
    cx.update_window(fixture.handle, |_, window, cx| window.press("escape", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(fixture.view.read_with(cx, |app, _| app.worktree.is_none()));
    close(&fixture, cx);
}

#[gpui_kit::test]
async fn dismissing_one_missing_worktree_preserves_registrations_and_can_be_restored(
    cx: &mut TestAppContext,
) {
    let fixture = open_git(cx);
    let missing_path = fixture._directory.path().join("missing-worktree");
    let other_path = fixture._directory.path().join("other-missing-worktree");
    for (branch, path) in [("stale", &missing_path), ("other-stale", &other_path)] {
        run_git(
            &fixture.root,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                branch,
                path.to_str().unwrap(),
            ],
        );
    }
    let missing_path = missing_path.canonicalize().unwrap();
    let other_path = other_path.canonicalize().unwrap();
    std::fs::remove_dir_all(&missing_path).unwrap();
    std::fs::remove_dir_all(&other_path).unwrap();

    let opened = navigation::open_directory(&fixture.root).unwrap();
    let checkout_at = |path: &Path| {
        opened
            .repo
            .checkouts
            .iter()
            .find(|checkout| checkout.path == path.to_str().unwrap())
            .expect("Git still lists the missing worktree")
            .clone()
    };
    let missing_id = checkout_at(&missing_path).id.clone();
    let other_id = checkout_at(&other_path).id.clone();
    fixture
        .view
        .update(cx, |app, _| app.navigation.merge(opened, false));
    let state_path = fixture._directory.path().join("native-workspace.json");
    fixture.view.update(cx, |app, _| {
        app.owner
            .install_writer(Some(StateWriter::new(state_path.clone()).unwrap()));
    });

    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.hover(
            SharedString::from(format!("fold-checkout-{missing_id}")),
            cx,
        );
        window.render_frame(cx);
        let action = SharedString::from(format!("dismiss-missing-{missing_id}"));
        window.find(action.clone());
        window.click(action, cx);
    })
    .unwrap();
    assert!(confirmation_open(fixture.handle, cx));
    answer_confirmation(fixture.handle, "Cancel", cx);
    cx.run_until_parked();
    assert!(!fixture
        .view
        .read_with(cx, |app, _| app.navigation.archived.contains(&missing_id)));

    // A pending terminal that arrives during confirmation must prevent hiding the row.
    cx.update_window(fixture.handle, |_, window, cx| {
        window.hover(format!("fold-checkout-{missing_id}"), cx);
        window.render_frame(cx);
        window.click(format!("dismiss-missing-{missing_id}"), cx);
        fixture.view.update(cx, |app, _| {
            app.navigation
                .pending
                .insert("pending".into(), missing_id.clone());
        });
    })
    .unwrap();
    answer_confirmation(fixture.handle, "Hide checkout", cx);
    cx.run_until_parked();
    fixture.view.update(cx, |app, _| {
        assert!(!app.navigation.archived.contains(&missing_id));
        assert!(app
            .toasts
            .latest_error()
            .unwrap()
            .contains("terminal sessions"));
        app.navigation.pending.clear();
    });

    cx.update_window(fixture.handle, |_, window, cx| {
        window.hover(format!("fold-checkout-{missing_id}"), cx);
        window.render_frame(cx);
        window.click(format!("dismiss-missing-{missing_id}"), cx);
    })
    .unwrap();
    answer_confirmation(fixture.handle, "Hide checkout", cx);
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .navigation
            .archived
            .contains(&missing_id)
    })
    .await;
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, _| {
        state_path.exists()
    })
    .await;

    fixture.view.read_with(cx, |app, _| {
        assert!(app.navigation.checkout(&missing_id).is_some());
        assert!(app.navigation.checkout(&other_id).is_some());
        assert!(app.navigation.archived.contains(&missing_id));
        assert!(!app.navigation.archived.contains(&other_id));
        assert_eq!(
            app.navigation.persistent_state().archived_checkout_ids,
            vec![missing_id.clone()]
        );
    });
    let saved = navigation::load(&state_path).unwrap();
    assert_eq!(saved.archived_checkout_ids, vec![missing_id.clone()]);

    // A normal repository refresh retains the dismissal and both stale registrations.
    let refreshed = navigation::open_directory(&fixture.root).unwrap();
    fixture.view.update(cx, |app, _| {
        app.navigation.merge(refreshed, false);
        assert!(app.navigation.archived.contains(&missing_id));
        assert!(!app.navigation.archived.contains(&other_id));
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.hover(SharedString::from(format!("fold-checkout-{other_id}")), cx);
        window.render_frame(cx);
        window.find(format!("dismiss-missing-{other_id}"));
    })
    .unwrap();
    let listed = Command::new("git")
        .current_dir(&fixture.root)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .unwrap();
    assert!(listed.status.success());
    let registrations = String::from_utf8_lossy(&listed.stdout);
    for (path, branch) in [(&missing_path, "stale"), (&other_path, "other-stale")] {
        assert!(registrations.contains(path.to_str().unwrap()));
        assert!(registrations.contains(&format!("branch refs/heads/{branch}")));
        let reference = format!("refs/heads/{branch}");
        run_git(
            &fixture.root,
            &["show-ref", "--verify", "--quiet", &reference],
        );
    }

    // Restoring the archived id makes the retained checkout visible again.
    fixture.view.update(cx, |app, _| {
        assert!(app.navigation.archived.remove(&missing_id));
        app.membership_revision += 1;
        app.persist();
    });
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        !fixture
            .view
            .read(cx)
            .navigation
            .archived
            .contains(&missing_id)
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        assert!(app.navigation.checkout(&missing_id).is_some());
        assert_eq!(
            app.navigation
                .checkout(&missing_id)
                .unwrap()
                .branch
                .as_deref(),
            Some("stale")
        );
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.hover(
            SharedString::from(format!("fold-checkout-{missing_id}")),
            cx,
        );
        window.render_frame(cx);
        window.find(format!("dismiss-missing-{missing_id}"));
    })
    .unwrap();
    close(&fixture, cx);
}

#[test]
fn a_checkouts_line_slot_stays_empty_unless_git_was_asked_and_changed_something() {
    // Three states, one drawing: uncounted, counted clean and counted. The
    // first two both say nothing, because the row cannot tell a person that
    // Git was never asked, and a zero is a claim nobody measured.
    assert_eq!(TerminalWindow::line_slot(None), None);
    assert_eq!(
        TerminalWindow::line_slot(Some(&DiffTotals::default())),
        None
    );
    assert_eq!(
        TerminalWindow::line_slot(Some(&DiffTotals {
            additions: 12,
            deletions: 4,
        })),
        Some((12, 4))
    );
    assert_eq!(
        TerminalWindow::line_slot(Some(&DiffTotals {
            additions: 0,
            deletions: 7,
        })),
        Some((0, 7))
    );
}

#[test]
fn ownership_closes_only_requested_session_and_shutdown_refuses_late_spawn() {
    let backend = Arc::new(TerminalBackend::default());
    let owner = SessionOwner::new(backend.clone(), true);
    let directory = tempfile::tempdir().unwrap();
    let create = |id: &str| {
        let mut config = TerminalSessionConfig::new(
            id.into(),
            directory.path().to_owned(),
            80,
            24,
            Some(PathBuf::from("/bin/sh")),
        );
        config.args = Some(vec!["-c".into(), "sleep 30".into()]);
        config
    };
    owner.spawn(create("owner-one")).unwrap();
    owner.spawn(create("owner-two")).unwrap();
    owner.close_one("owner-one").unwrap();
    assert!(backend.status("owner-one").is_err());
    assert!(backend.status("owner-two").is_ok());
    owner.stop();
    assert!(backend.status("owner-two").is_err());
    assert!(owner.spawn(create("owner-late")).is_err());
    owner.stop();
}

#[test]
fn closing_a_terminal_racing_window_shutdown_closes_it_once() {
    let backend = Arc::new(TerminalBackend::default());
    let owner = SessionOwner::new(backend.clone(), true);
    let directory = tempfile::tempdir().unwrap();
    let mut config = TerminalSessionConfig::new(
        "owner-race".into(),
        directory.path().to_owned(),
        80,
        24,
        Some(PathBuf::from("/bin/sh")),
    );
    config.args = Some(vec!["-c".into(), "sleep 30".into()]);
    owner.spawn(config).unwrap();

    let barrier = Arc::new(std::sync::Barrier::new(3));
    let close_owner = owner.clone();
    let close_barrier = barrier.clone();
    let closer = std::thread::spawn(move || {
        close_barrier.wait();
        close_owner.close_one("owner-race")
    });
    let stop_owner = owner.clone();
    let stop_barrier = barrier.clone();
    let stopper = std::thread::spawn(move || {
        stop_barrier.wait();
        stop_owner.stop();
    });
    barrier.wait();

    closer.join().unwrap().unwrap();
    stopper.join().unwrap();
    assert!(backend.status("owner-race").is_err());
}

#[test]
fn shortcuts_respect_platform_and_keyboard_layout() {
    let parse = |key: &str| gpui_kit::Keystroke::parse(key).unwrap();
    let prefix = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    assert_eq!(
        shortcut(&parse(&format!("{prefix}-/"))),
        Some(Shortcut::Panels)
    );
    assert_eq!(
        shortcut(&parse(&format!("{prefix}-shift-7"))),
        Some(Shortcut::Panels)
    );
    assert_eq!(
        shortcut(&parse(&format!("{prefix}-n"))),
        Some(Shortcut::Terminal)
    );
    for suffix in ["7", "t", "alt-n"] {
        assert_eq!(shortcut(&parse(&format!("{prefix}-{suffix}"))), None);
    }
    assert_eq!(shortcut(&parse("escape")), None);
}

#[gpui_kit::test]
async fn closing_a_busy_terminal_requires_consent_and_keeps_its_sibling(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let id = fixture.sessions[0].clone();
    // Busy is the last poll's answer about the foreground process group, so it
    // is waited for rather than assumed: a close that lands before the poll has
    // seen `cat` is a close of an idle terminal, which asks nothing and takes
    // the very prompt this test is about.
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .terminals
            .get(&id)
            .is_some_and(|terminal| terminal.shell.session.status().foreground.running)
    })
    .await;
    let previous_focus = cx
        .update_window(fixture.handle, |_, window, cx| {
            let focus = window.focused(cx).expect("the terminal has focus");
            // The close is behind the row's hover, so the pointer arrives on the row first.
            window.hover(SharedString::from(format!("select-session-{id}")), cx);
            window.click(SharedString::from(format!("close-session-{id}")), cx);
            focus
        })
        .unwrap();
    assert!(confirmation_open(fixture.handle, cx));
    cx.update_window(fixture.handle, |_, window, cx| {
        window.press("escape", cx);
        assert!(
            previous_focus.is_focused(window),
            "Escape restores terminal focus"
        );
    })
    .unwrap();
    assert!(!confirmation_open(fixture.handle, cx));
    cx.run_until_parked();
    assert!(fixture.backend.status(&id).is_ok());
    assert!(!fixture
        .view
        .read_with(cx, |app, _| app.terminals[&id].shell.session.is_closing()));
    cx.update_window(fixture.handle, |_, window, cx| {
        window.hover(SharedString::from(format!("select-session-{id}")), cx);
        window.click(SharedString::from(format!("close-session-{id}")), cx);
    })
    .unwrap();
    assert!(confirmation_open(fixture.handle, cx));
    cx.update_window(fixture.handle, |_, window, cx| window.press("enter", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(!confirmation_open(fixture.handle, cx));
    assert!(
        fixture.backend.status(&id).is_ok(),
        "Enter without a selected action cancels"
    );
    cx.update_window(fixture.handle, |_, window, cx| {
        window.hover(SharedString::from(format!("select-session-{id}")), cx);
        window.click(SharedString::from(format!("close-session-{id}")), cx);
    })
    .unwrap();
    answer_confirmation(fixture.handle, "Close terminal", cx);
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        !fixture.view.read(cx).terminals.contains_key(&id)
    })
    .await;
    assert!(fixture.backend.status(&id).is_err());
    assert!(fixture.backend.status(&fixture.sessions[1]).is_ok());
    assert_eq!(
        fixture
            .view
            .read_with(cx, |app, _| app.navigation.active_session.clone()),
        Some(fixture.sessions[1].clone())
    );
    dispose(&fixture, cx);
}

#[gpui_kit::test]
async fn native_directory_picker_adds_a_checkout_without_spawning_a_shell(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let directory = tempfile::tempdir().unwrap();
    let canonical = directory.path().canonicalize().unwrap();
    let checkout = muster_core::workspace::checkout_id_for_path(canonical.to_str().unwrap());
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("open-directory", cx)
    })
    .unwrap();
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|options| {
        assert!(!options.files && options.directories && !options.multiple);
        Some(vec![canonical.clone()])
    });
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .navigation
            .checkout(&checkout)
            .is_some()
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(
            app.navigation.active_checkout.as_deref(),
            Some(checkout.as_str())
        );
        assert_eq!(app.navigation.active_session, None);
        assert_eq!(app.terminals.len(), 2);
        let repo = app
            .navigation
            .repos
            .iter()
            .find(|repo| repo.checkouts.iter().any(|item| item.id == checkout))
            .unwrap();
        assert_eq!(repo.kind, RepoKind::Plain);
        assert!(app
            .navigation
            .checkout(&checkout)
            .unwrap()
            .sessions
            .is_empty());
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(format!("select-checkout-{checkout}")).visible());
        assert!(window.try_find("worktree-crumb").is_none());
        window.click("open-directory", cx)
    })
    .unwrap();
    cx.simulate_path_prompt_response(|_| None);
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        !fixture.view.read(cx).opening
    })
    .await;
    assert_eq!(
        fixture
            .view
            .read_with(cx, |app, _| app.navigation.repos.len()),
        2
    );
    dispose(&fixture, cx);
}

#[gpui_kit::test]
async fn a_plain_folder_with_a_running_terminal_still_leaves_the_panel(cx: &mut TestAppContext) {
    let fixture = open(cx);
    fixture.view.update(cx, |app, _| {
        app.navigation.home = Some(app.navigation.repos[0].checkouts[0].id.clone());
    });
    let directory = tempfile::tempdir().unwrap();
    let canonical = directory.path().canonicalize().unwrap();
    let checkout = muster_core::workspace::checkout_id_for_path(canonical.to_str().unwrap());
    let repo_id = muster_core::workspace::repo_id_for_path(canonical.to_str().unwrap());
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("open-directory", cx)
    })
    .unwrap();
    cx.simulate_path_prompt_response(|_| Some(vec![canonical.clone()]));
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .navigation
            .checkout(&checkout)
            .is_some()
    })
    .await;
    // What TerminalWindow::new does on every launch: the active workdir, which is
    // the directory just opened, gets a shell.
    fixture.view.update(cx, |app, _| {
        assert!(app.navigation.add_session(
            Session {
                id: "plain-folder-terminal".into(),
                checkout_id: checkout.clone(),
                name: "sh".into(),
                created_at: "now".into(),
                session_type: SessionType::Shell,
                status: SavedSessionStatus::Active,
            },
            None,
        ));
    });

    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.hover(SharedString::from(format!("fold-repo-{repo_id}")), cx);
        window.click(SharedString::from(format!("actions-{repo_id}")), cx);
    })
    .unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        // A plain folder's menu is the separator-less single item at index 0.
        window.within("items").click(0usize, cx);
    })
    .unwrap();
    assert!(confirmation_open(fixture.handle, cx));
    answer_confirmation(fixture.handle, "Remove from panel", cx);
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        fixture
            .view
            .read(cx)
            .navigation
            .checkout(&checkout)
            .is_none()
    })
    .await;
    cx.wait_for(fixture.handle, Duration::from_secs(10), |_, cx| {
        !fixture
            .view
            .read(cx)
            .terminals
            .contains_key("plain-folder-terminal")
    })
    .await;
    fixture.view.read_with(cx, |app, _| {
        assert!(app.navigation.active_checkout.is_none());
        assert!(app.navigation.active_session.is_none());
        // The rest of the panel is untouched, and nothing complained.
        assert_eq!(app.navigation.repos.len(), 1);
        assert!(app.toasts.latest_error().is_none());
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window
            .try_find(format!("select-checkout-{checkout}"))
            .is_none());
        assert!(window.try_find(format!("actions-{repo_id}")).is_none());
    })
    .unwrap();
    dispose(&fixture, cx);
}

/// The words a Pi row says, and whether it moves.
///
/// The three answers the panel has, and the one it does not: a turn that
/// stopped to ask something is parsed and kept, and drawn as nothing, because
/// the panel has no shape for a question this program asked.
#[test]
fn a_pi_row_says_what_pi_is_doing() {
    use crate::sidebar::pi_row_state;
    use crate::terminal::PiTaskState;

    let working = pi_row_state(Some(PiTaskState::Working));
    assert_eq!(working.note, "working");
    assert!(
        working.working,
        "a turn that is running is the one that moves"
    );
    assert!(!working.failed);

    let failed = pi_row_state(Some(PiTaskState::Error));
    assert_eq!(failed.note, "last task failed");
    assert!(!failed.working);
    assert!(failed.failed);

    let done = pi_row_state(Some(PiTaskState::Done));
    assert_eq!(done.note, "task complete");
    assert!(!done.working);
    assert!(!done.failed);

    let blocked = pi_row_state(Some(PiTaskState::Blocked));
    assert_eq!(blocked.note, "");
    assert!(!blocked.working);
    assert!(!blocked.failed);

    let idle = pi_row_state(None);
    assert_eq!(idle.note, "");
    assert!(!idle.working);
    assert!(!idle.failed);
}

/// A terminal row for a Pi that is working says so, and says the program's name.
///
/// The whole path, from the operating system's own answer about what is in
/// front of the shell to the words on the row: a link called `pi` is a process
/// the OS calls Pi, and the report it writes is the one the row wears.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[gpui_kit::test]
async fn a_pi_terminal_row_says_what_pi_is_doing(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let pi = fixture.directory.path().join("pi");
    std::os::unix::fs::symlink("/bin/sh", &pi).unwrap();
    let id = format!("pi-session-{}", std::process::id());
    let checkout = fixture.checkout.clone();
    let command = format!(
        "{} -c 'while :; do printf \"\\033]7501;state=working\\007\"; sleep 0.1; done'\n",
        pi.display()
    );
    fixture.view.update(cx, |app, cx| {
        let mut config = TerminalSessionConfig::new(
            id.clone(),
            fixture.directory.path().to_owned(),
            80,
            24,
            Some(PathBuf::from("/bin/sh")),
        );
        // An interactive shell, because job control is what puts a command in
        // front of the shell.
        config.args = Some(Vec::new());
        config.status_interval = Duration::from_millis(20);
        let shell = app.owner.spawn(config).unwrap();
        let terminal =
            cx.new(|cx| TerminalView::new(shell.session.clone(), shell.metrics.clone(), cx));
        app.terminals.insert(
            id.clone(),
            LiveTerminal {
                checkout_id: checkout.clone(),
                view: terminal,
                shell: shell.clone(),
            },
        );
        app.navigation.add_session(
            Session {
                id: id.clone(),
                checkout_id: checkout,
                name: "sh".into(),
                created_at: "test".into(),
                session_type: SessionType::Shell,
                status: SavedSessionStatus::Active,
            },
            None,
        );
        shell.session.send_text(&command).unwrap();
    });

    cx.wait_for(fixture.handle, Duration::from_secs(10), |window, cx| {
        window.render_frame(cx);
        window
            .try_find(format!("select-session-{id}"))
            .and_then(|row| row.label().map(str::to_owned))
            .is_some_and(|label| label.contains("working"))
    })
    .await;
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        let label = window
            .find(format!("select-session-{id}"))
            .label()
            .unwrap_or_default()
            .to_owned();
        assert!(
            label.contains("Pi"),
            "the row names the program that is in front: {label}"
        );
        assert!(
            label.contains("working"),
            "the row says what the program is doing: {label}"
        );
    })
    .unwrap();
    dispose(&fixture, cx);
}
