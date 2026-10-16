//! Integration tests draw the production title bar over the fixtures the
//! sidebar tests already open: the crumbs, the menus they open, and the window
//! the bar belongs to.
//!
//! The window here is the test platform's, which answers `start_window_move`
//! with `unimplemented!()`. That is what gives the drag case its teeth: a press
//! that reached the bar would ask the platform to move the window and take the
//! test down with it, so surviving the gesture is the assertion.

use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{px, TestAppContext};

use super::sidebar_tests::{close, dispose, open, open_git, GitFixture};
use super::*;

/// The bar is the window's own chrome, the crumbs name where the window is, and
/// the actions sit at the far end of it.
#[gpui_kit::test]
fn the_bar_draws_the_crumb_line_over_the_actions(cx: &mut TestAppContext) {
    let fixture = open(cx);
    cx.update_window(fixture.handle, |_, window, _| {
        assert!(window.find("title-bar").visible(), "the bar is the chrome");
        assert!(window.find("repo-crumb").visible());
        assert!(window.find("item-crumb").visible());
        assert!(
            window.try_find("worktree-crumb").is_none(),
            "a plain directory has no worktree level to name"
        );
        let crumb = window.find("repo-crumb").bounds();
        let action = window.find("toggle-panels").bounds();
        assert!(
            crumb.bottom() <= px(32.),
            "the crumb line belongs to the 32px bar"
        );
        assert!(
            crumb.right() < action.left(),
            "the crumbs never reach the actions at the far end"
        );
    })
    .unwrap();
    dispose(&fixture, cx);
}

/// Each crumb says what the sidebar says about the same thing: the repository,
/// the branch a Git one is on, and the terminal open inside it.
#[gpui_kit::test]
fn the_crumbs_say_what_the_sidebar_says(cx: &mut TestAppContext) {
    let plain = open(cx);
    let repo_name = plain
        .view
        .read_with(cx, |app, _| app.navigation.repos[0].name.clone());
    cx.update_window(plain.handle, |_, window, _| {
        assert_eq!(window.find("repo-crumb").label(), Some(repo_name.as_str()));
        assert!(matches!(
            window.find("item-crumb").label(),
            Some("sh" | "cat")
        ));
    })
    .unwrap();
    dispose(&plain, cx);

    let git = open_git(cx);
    cx.update_window(git.handle, |_, window, _| {
        assert_eq!(window.find("repo-crumb").label(), Some("repo"));
        assert_eq!(window.find("worktree-crumb").label(), Some("main"));
        assert!(
            window.try_find("item-crumb").is_none(),
            "a workdir with no terminal open has none to name"
        );
    })
    .unwrap();
    close(&git, cx);
}

/// The glyphs are the ones the sidebar gives the rows they name, and the workdir
/// wears none: it is the name that is always there.
#[test]
fn a_crumb_wears_the_glyph_of_the_row_it_names() {
    assert_eq!(crumb_glyph(Crumb::Workdir, None), CrumbGlyph::None);
    assert_eq!(
        crumb_glyph(Crumb::Worktree, None),
        CrumbGlyph::Named(IconName::GitBranch)
    );
    assert_eq!(
        crumb_glyph(Crumb::Item, None),
        CrumbGlyph::Named(IconName::SquareTerminal)
    );
}

/// A crumb that names a file wears that file's own icon, under the last segment of its path:
/// an icon resolves from a name alone, and a path with separators in it finds no rule.
#[test]
fn a_crumb_naming_a_file_wears_that_files_icon() {
    assert_eq!(
        crumb_glyph(Crumb::Item, Some("main.rs")),
        CrumbGlyph::File("main.rs".into())
    );
}

/// One open name for the whole line: opening one crumb closes the others rather
/// than stacking them.
#[gpui_kit::test]
fn opening_one_crumb_closes_the_previous_one(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("repo-crumb", cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        fixture.view.read_with(cx, |app, _| app.open_crumb),
        Some(Crumb::Workdir)
    );
    cx.update_window(fixture.handle, |_, window, cx| {
        assert!(
            window.find("workdir-open-directory").visible(),
            "the workdir menu opened"
        );
        // A searchable palette, not a plain list of rows.
        assert!(
            window.find("command").visible(),
            "the menu has a search field"
        );
        assert!(
            window.find("command").bounds().size.width >= px(280.)
                && window.find("command").bounds().size.width <= px(288.)
        );
        window.click("worktree-crumb", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        fixture.view.read_with(cx, |app, _| app.open_crumb),
        Some(Crumb::Worktree)
    );
    cx.update_window(fixture.handle, |_, window, _| {
        assert!(
            window.try_find("workdir-open-directory").is_none(),
            "the workdir menu closed with its crumb"
        );
        assert!(
            window.find("worktree-new").visible(),
            "the worktree menu opened in its place"
        );
        let row = window.find("crumb-menu-row-0").bounds();
        let separator = window.find("crumb-menu-separator").bounds();
        assert!(
            (row.left().as_f32() - separator.left().as_f32()).abs() <= 1.
                && (row.right().as_f32() - separator.right().as_f32()).abs() <= 1.,
            "menu row should span the same content width as the footer: {row:?} vs {separator:?}"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// The session crumb is controlled by the same open name as the searchable crumbs.
#[gpui_kit::test]
fn opening_the_session_menu_closes_the_previous_crumb(cx: &mut TestAppContext) {
    let fixture = open(cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("item-crumb", cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        fixture.view.read_with(cx, |app, _| app.open_crumb),
        Some(Crumb::Item)
    );

    cx.update_window(fixture.handle, |_, window, cx| {
        assert!(window.find("item-new-terminal").visible());
        window.click("repo-crumb", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        fixture.view.read_with(cx, |app, _| app.open_crumb),
        Some(Crumb::Workdir)
    );
    cx.update_window(fixture.handle, |_, window, cx| {
        assert!(window.try_find("item-new-terminal").is_none());
        window.click("item-crumb", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        fixture.view.read_with(cx, |app, _| app.open_crumb),
        Some(Crumb::Item)
    );
    cx.update_window(fixture.handle, |_, window, _| {
        assert!(window.find("item-new-terminal").visible());
        assert!(window.try_find("workdir-open-directory").is_none());
        assert!(
            window.find("command").bounds().size.width >= px(280.)
                && window.find("command").bounds().size.width <= px(288.)
        );
    })
    .unwrap();
    dispose(&fixture, cx);
}

#[gpui_kit::test]
fn explicit_terminal_selection_closes_file_document_and_keeps_terminal(cx: &mut TestAppContext) {
    let fixture = open(cx);
    fixture.view.update(cx, |app, cx| {
        let other = format!("other-checkout:{}", fixture.checkout);
        app.open_document = Some((
            other,
            muster_core::files::FilePreview {
                path: "source.rs".into(),
                text: "fn main() {}".into(),
            },
        ));
        cx.notify();
    });
    let session = fixture.sessions[0].clone();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("close-document").is_none());
        window.click(format!("select-session-{session}"), cx);
    })
    .unwrap();
    assert!(fixture
        .view
        .read_with(cx, |app, _| app.open_document.is_none()));
    assert!(fixture
        .view
        .read_with(cx, |app, _| app.terminals.contains_key(&session)));
    dispose(&fixture, cx);
}

#[gpui_kit::test]
fn closing_a_document_restores_the_retained_terminal(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let session = fixture.sessions[0].clone();
    fixture.view.update(cx, |app, cx| {
        app.open_document = Some((
            fixture.checkout.clone(),
            muster_core::files::FilePreview {
                path: "source.rs".into(),
                text: "fn main() {}".into(),
            },
        ));
        cx.notify();
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("close-document", cx);
        window.render_frame(cx);
        assert!(window.try_find("close-document").is_none());
        assert!(fixture
            .view
            .read_with(cx, |app, _| app.terminals.contains_key(&session)));
    })
    .unwrap();
    dispose(&fixture, cx);
}

#[gpui_kit::test]
fn checkout_change_hides_another_checkouts_document(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let other_dir = tempfile::tempdir().unwrap();
    let opened = navigation::open_directory(other_dir.path()).unwrap();
    let other_checkout = opened.checkout_id.clone();
    fixture.view.update(cx, |app, cx| {
        app.navigation.merge(opened, false);
        app.open_document = Some((
            other_checkout,
            muster_core::files::FilePreview {
                path: "elsewhere.rs".into(),
                text: "wrong checkout".into(),
            },
        ));
        cx.notify();
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("close-document").is_none());
        assert!(window.find("repo-crumb").visible());
    })
    .unwrap();
    close(&fixture, cx);
}

/// Filtering changes which rows are visible, not the target of the original row.
#[gpui_kit::test]
fn a_filtered_worktree_selection_targets_the_matching_checkout(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let target = add_branch(&fixture, "feature/filtered-target", false, cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("worktree-crumb", cx)
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.input("filtered-target", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        fixture
            .view
            .read_with(cx, |app, _| app.navigation.active_checkout.clone()),
        Some(target)
    );
    assert_eq!(
        fixture.view.read_with(cx, |app, _| app.open_crumb),
        None,
        "confirming a row closes its menu"
    );
    close(&fixture, cx);
}

/// Archived and missing checkouts stay in the picker as disabled rows.
#[gpui_kit::test]
fn archived_and_missing_worktrees_cannot_be_selected(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    let target = add_branch(&fixture, "feature/unavailable-target", false, cx);
    fixture.view.update(cx, |app, cx| {
        app.navigation.archived.insert(target.clone());
        cx.notify();
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("worktree-crumb", cx)
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.input("unavailable-target", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        fixture
            .view
            .read_with(cx, |app, _| app.navigation.active_checkout.clone()),
        Some(fixture.checkout.clone())
    );

    fixture.view.update(cx, |app, cx| {
        app.navigation.archived.remove(&target);
        app.navigation
            .repos
            .iter_mut()
            .flat_map(|repo| &mut repo.checkouts)
            .find(|checkout| checkout.id == target)
            .unwrap()
            .is_missing = true;
        cx.notify();
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        fixture
            .view
            .read_with(cx, |app, _| app.navigation.active_checkout.clone()),
        Some(fixture.checkout.clone())
    );
    assert_eq!(
        fixture.view.read_with(cx, |app, _| app.open_crumb),
        Some(Crumb::Worktree),
        "an unavailable row did not dismiss the picker"
    );
    close(&fixture, cx);
}

#[gpui_kit::test]
fn detached_worktree_uses_the_titlebar_label(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    fixture.view.update(cx, |app, cx| {
        app.navigation
            .repos
            .iter_mut()
            .flat_map(|repo| &mut repo.checkouts)
            .find(|checkout| checkout.id == fixture.checkout)
            .unwrap()
            .branch = None;
        cx.notify();
    });
    cx.update_window(fixture.handle, |_, window, _| {
        assert_eq!(window.find("worktree-crumb").label(), Some("Detached"));
    })
    .unwrap();
    close(&fixture, cx);
}

#[gpui_kit::test]
fn a_saved_session_without_a_live_terminal_has_no_item_crumb(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    fixture.view.update(cx, |app, cx| {
        let revision = app.navigation.revision;
        app.navigation.add_session(
            Session {
                id: "not-presented".into(),
                checkout_id: fixture.checkout.clone(),
                name: "shell".into(),
                created_at: "test".into(),
                session_type: SessionType::Shell,
                status: SavedSessionStatus::Active,
            },
            Some(revision),
        );
        cx.notify();
    });
    cx.update_window(fixture.handle, |_, window, _| {
        assert!(window.try_find("item-crumb").is_none());
    })
    .unwrap();
    close(&fixture, cx);
}

/// Opening a plain folder from the titlebar is the same directory choice the
/// sidebar offers, without starting a terminal or creating a Git worktree.
#[gpui_kit::test]
async fn the_repo_menu_opens_a_plain_directory_without_spawning_a_terminal(
    cx: &mut TestAppContext,
) {
    let fixture = open_git(cx);
    let directory = tempfile::tempdir().unwrap();
    let canonical = directory.path().canonicalize().unwrap();
    let checkout = muster_core::workspace::checkout_id_for_path(canonical.to_str().unwrap());

    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("repo-crumb", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(fixture.handle, |_, window, cx| {
        assert!(window.find("workdir-open-directory").visible());
        window.click("workdir-open-directory", cx);
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
        let repo = app
            .navigation
            .repos
            .iter()
            .find(|repo| repo.checkouts.iter().any(|item| item.id == checkout))
            .unwrap();
        assert_eq!(repo.kind, RepoKind::Plain);
        assert_eq!(
            app.navigation.active_checkout.as_deref(),
            Some(checkout.as_str())
        );
        assert_eq!(app.navigation.active_session, None);
        assert!(
            app.terminals.is_empty(),
            "opening a directory starts no shell"
        );
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
    })
    .unwrap();
    close(&fixture, cx);
}

#[gpui_kit::test]
fn starting_a_worktree_closes_the_picker_before_opening_the_dialog(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("worktree-crumb", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("worktree-new", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(fixture.view.read_with(cx, |app, _| app.worktree.is_some()));
    assert_eq!(fixture.view.read_with(cx, |app, _| app.open_crumb), None);
    close(&fixture, cx);
}

/// Empty bar space is the window's to drag; a crumb is the crumb's, even when
/// the gesture that follows looks exactly like a drag.
#[gpui_kit::test]
fn a_press_that_starts_on_a_crumb_does_not_move_the_window(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let before = cx
        .update_window(fixture.handle, |_, window, _| window.viewport_size())
        .unwrap();
    let from = cx
        .update_window(fixture.handle, |_, window, _| {
            window.find("item-crumb").bounds().center()
        })
        .unwrap();
    cx.update_window(fixture.handle, |_, window, cx| {
        window.drag(from, gpui_kit::point(px(600.), px(600.)), cx)
    })
    .unwrap();
    cx.run_until_parked();
    // The press was the crumb's: it opened its menu rather than moving anything.
    assert_eq!(
        fixture.view.read_with(cx, |app, _| app.open_crumb),
        Some(Crumb::Item)
    );
    assert_eq!(
        cx.update_window(fixture.handle, |_, window, _| window.viewport_size())
            .unwrap(),
        before,
        "a drag that started on a crumb left the window where it was"
    );
    dispose(&fixture, cx);
}

/// A name too long for the line gives up whole segments rather than characters,
/// keeps saying what it names, and never reaches the actions at the far end.
#[gpui_kit::test]
fn a_long_name_gives_up_segments_and_keeps_saying_what_it_names(cx: &mut TestAppContext) {
    let fixture = open_git(cx);
    // A name longer than any bar this window can be given: the line has to give
    // something up for it whatever the width.
    let long = format!("feature/{}", "a-long-branch-name-".repeat(12));
    add_branch(&fixture, &long, true, cx);
    cx.update_window(fixture.handle, |_, window, _| {
        let crumb = window.find("worktree-crumb");
        assert_eq!(
            crumb.label(),
            Some(long.as_str()),
            "the whole name is what the crumb still says"
        );
        assert!(
            crumb.bounds().size.width < sidebar_text_width(window, &long, 13.),
            "a name the line cannot hold gave something up"
        );
        assert!(
            crumb.bounds().size.width > px(300.),
            "the branch gets the space the short repo crumb leaves unused"
        );
        assert!(
            crumb.bounds().right() < window.find("toggle-panels").bounds().left(),
            "the actions at the far end are still their own"
        );
    })
    .unwrap();
    close(&fixture, cx);
}

/// Hiding the panels is not a reason to take the keyboard off what is being read.
///
/// The shortcut and the bar's own button are the same action, and a change set
/// in the middle owns the keyboard exactly as a preview does: both are views
/// the terminal was replaced by.
#[gpui_kit::test]
fn hiding_the_panels_leaves_the_keyboard_where_the_view_left_it(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let checkout = fixture.checkout.clone();
    fixture.view.update(cx, |app, cx| {
        app.open_diff = Some((checkout.clone(), None));
        cx.notify();
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        // Nothing holds the keyboard, so anything focused afterwards was put
        // there by the button.
        window.blur(cx);
        window.click("toggle-panels", cx);
        assert!(
            !fixture
                .view
                .read_with(cx, |app, _| app.navigation.panels_visible),
            "the button did not hide the panels"
        );
        assert!(
            !fixture.view.read_with(cx, |app, cx| app
                .terminal()
                .is_some_and(|terminal| terminal.read(cx).focus_handle(cx).is_focused(window))),
            "hiding the panels took the keyboard off the change set"
        );
    })
    .unwrap();
    fixture.view.read_with(cx, |app, _| {
        assert_eq!(app.open_diff, Some((checkout, None)));
    });
    dispose(&fixture, cx);
}

/// A worktree on a branch long enough to need shortening, added to the same
/// repository the fixture opened.
fn add_branch(fixture: &GitFixture, branch: &str, select: bool, cx: &mut TestAppContext) -> String {
    let repo = fixture.view.read_with(cx, |app, _| {
        app.navigation
            .repos
            .iter()
            .find(|repo| repo.id == fixture.repo_id)
            .unwrap()
            .clone()
    });
    let task = format!("titlebar-{}", NEXT_SESSION.fetch_add(1, Ordering::Relaxed));
    let opened = navigation::create_worktree(&repo, &task, branch, "").unwrap();
    let target = opened.checkout_id.clone();
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture
            .view
            .update(cx, |app, _| app.navigation.merge(opened, select));
        window.render_frame(cx);
    })
    .unwrap();
    target
}
