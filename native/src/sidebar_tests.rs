//! Integration tests draw the production sidebar and dispatch real pointer/key events.

use std::process::Command;

use super::*;
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{AnyWindowHandle, TestAppContext};
use tempfile::TempDir;

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

#[test]
fn a_task_name_suggests_a_branch_only_while_the_person_has_not_edited_it() {
    assert_eq!(TerminalWindow::slug_for("Fix login"), "Fix-login");
    assert_eq!(TerminalWindow::slug_for("  a//b  "), "a-b");
    assert_eq!(TerminalWindow::slug_for("..."), "new-task");
    assert_eq!(TerminalWindow::slug_for(".hidden/x"), "hidden-x");
}

struct Fixture {
    _directory: TempDir,
    handle: AnyWindowHandle,
    view: Entity<TerminalWindow>,
    backend: Arc<TerminalBackend>,
    checkout: String,
    sessions: Vec<String>,
}

fn open(cx: &mut TestAppContext) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repo::plain(directory.path(), "test").unwrap();
    let checkout = repo.checkouts[0].id.clone();
    let backend = Arc::new(TerminalBackend::default());
    let mut sessions = Vec::new();
    cx.update(|cx| {
        gpui_kit::init(cx);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
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
        _directory: directory,
        handle,
        view,
        backend,
        checkout,
        sessions,
    }
}

fn dispose(fixture: &Fixture, cx: &mut TestAppContext) {
    cx.update_window(fixture.handle, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    fixture.view.update(cx, |app, _| app.owner.stop());
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
        assert!(app.notice.as_ref().unwrap().contains("no cd was sent"));
    });
    assert!(fixture.backend.status(&id).is_ok());
    dispose(&fixture, cx);
}

/// A window over one real Git repository, with no terminals of its own.
/// Named with a leading underscore because it exists to be dropped: the
/// worktree lives inside it and the row is read from the window, not the disk.
struct GitFixture {
    _directory: TempDir,
    handle: AnyWindowHandle,
    view: Entity<TerminalWindow>,
    root: PathBuf,
    repo_id: String,
    checkout: String,
}

fn open_git(cx: &mut TestAppContext) -> GitFixture {
    let directory = tempfile::tempdir().unwrap();
    let root = git_repository(directory.path());
    let opened = navigation::open_directory(&root).unwrap();
    let repo_id = opened.repo.id.clone();
    let checkout = opened.checkout_id.clone();
    cx.update(|cx| {
        gpui_kit::init(cx);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
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

fn close(fixture: &GitFixture, cx: &mut TestAppContext) {
    cx.update_window(fixture.handle, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    fixture.view.update(cx, |app, _| app.owner.stop());
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
            .is_some_and(|draft| draft.location.is_some())
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
        fixture
            .view
            .update(cx, |app, cx| app.submit_worktree(window, cx));
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
            .is_some_and(|draft| draft.location.is_some())
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
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click(SharedString::from(format!("close-session-{id}")), cx);
    })
    .unwrap();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    assert!(fixture.backend.status(&id).is_ok());
    assert!(!fixture
        .view
        .read_with(cx, |app, _| app.terminals[&id].shell.session.is_closing()));
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click(SharedString::from(format!("close-session-{id}")), cx);
    })
    .unwrap();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Close terminal");
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
        assert!(app
            .navigation
            .checkout(&checkout)
            .unwrap()
            .sessions
            .is_empty());
    });
    cx.update_window(fixture.handle, |_, window, cx| {
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

#[test]
fn serial_writer_flushes_the_last_snapshot_before_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("native-workspace.json");
    let writer = StateWriter::new(path.clone()).unwrap();
    writer.send(NavigationState {
        sidebar_width: 300.,
        ..NavigationState::default()
    });
    writer.send(NavigationState {
        sidebar_width: 410.,
        ..NavigationState::default()
    });
    let errors = writer.error.clone();
    drop(writer);
    assert_eq!(navigation::load(&path).unwrap().sidebar_width, 410.);
    assert!(errors.lock().unwrap().is_none());
}
