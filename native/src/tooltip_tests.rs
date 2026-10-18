//! What a tooltip waits for, and what it waits for again.
//!
//! The wait is the tooltip's whole contract: browsing a list must not paint
//! every row's tooltip on the way past, and a trigger that has not been rested
//! on must not wear the last one's.

use std::time::Duration;

use gpui_kit::test::TestWindowExt;
use gpui_kit::TestAppContext;
use muster_core::files::FileEntry;

use super::sidebar_tests::{dispose, open, pointer_move};
use super::*;

/// One millisecond short of the wait the toolkit installs for a trigger.
const BEFORE: Duration = Duration::from_millis(499);

/// Let the simulated clock run, and everything the wait woke with it.
fn clock(cx: &TestAppContext, by: Duration) {
    cx.executor().advance_clock(by);
    cx.run_until_parked();
}

/// A pointer that rests on a row is answered after the wait, and the row it
/// moves to next waits for itself instead of taking over what was up.
#[gpui_kit::test]
fn every_trigger_waits_its_own_time(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let repo = fixture
        .view
        .read_with(cx, |app, _| app.navigation.repos[0].id.clone());
    let checkout = fixture.checkout.clone();

    // Two triggers in one window, both naming what they are: the worktree row
    // and the repository heading above it.
    let (row, heading) = cx
        .update_window(fixture.handle, |_, window, cx| {
            window.render_frame(cx);
            (
                window.find(format!("select-checkout-{checkout}")).bounds(),
                window.find(format!("fold-repo-{repo}")).bounds(),
            )
        })
        .unwrap();

    cx.update_window(fixture.handle, |_, window, cx| {
        assert!(
            window.try_find("tooltip").is_none(),
            "a tooltip before the pointer had arrived"
        );
        pointer_move(window, heading.center(), None, cx);
        window.render_frame(cx);
        assert!(
            window.try_find("tooltip").is_none(),
            "the row answered a pointer that had not rested on it"
        );
    })
    .unwrap();

    clock(cx, BEFORE);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("tooltip").is_none(),
            "the wait was up early"
        );
    })
    .unwrap();

    clock(cx, Duration::from_millis(1));
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("tooltip").is_some(),
            "the row waited and nothing answered"
        );

        // Moving on is what the grace period is for, and it is not what this
        // app does: the next row takes nothing with it.
        pointer_move(window, row.center(), None, cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(
            window.try_find("tooltip").is_none(),
            "the next trigger inherited the last one's tooltip"
        );
    })
    .unwrap();

    clock(cx, BEFORE);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("tooltip").is_none(),
            "the row the pointer moved to did not wait"
        );
    })
    .unwrap();

    clock(cx, Duration::from_millis(1));
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("tooltip").is_some(),
            "the second trigger waited and nothing answered"
        );
    })
    .unwrap();

    dispose(&fixture, cx);
}

/// A control that cannot be hovered still names itself. Several triggers are
/// disabled exactly when their tooltip is the answer, which is why the trigger
/// is a canvas over the control rather than the control's own hover.
#[gpui_kit::test]
fn a_disabled_row_still_names_itself(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let checkout = fixture.checkout.clone();
    cx.update_window(fixture.handle, |_, _, cx| {
        fixture.view.update(cx, |app, cx| {
            app.navigation
                .repos
                .iter_mut()
                .flat_map(|repo| repo.checkouts.iter_mut())
                .find(|entry| entry.id == checkout)
                .expect("the fixture's own checkout")
                .is_missing = true;
            cx.notify();
        })
    })
    .unwrap();

    let mut drawn = false;
    for _ in 0..32 {
        cx.run_until_parked();
        drawn = cx
            .update_window(fixture.handle, |_, window, cx| {
                window.render_frame(cx);
                window
                    .try_find(format!("select-checkout-{checkout}"))
                    .is_some()
            })
            .unwrap();
        if drawn {
            break;
        }
    }
    assert!(drawn, "the missing worktree is on screen");

    cx.update_window(fixture.handle, |_, window, cx| {
        // The row is the missing-checkout one, which is the disabled branch:
        // it announces itself as unavailable.
        let row = window.find(format!("select-checkout-{checkout}"));
        assert!(
            row.label()
                .is_some_and(|label| label.starts_with("Unavailable worktree")),
            "the fixture's own worktree is not the one that is missing"
        );
        let row = row.bounds();
        pointer_move(window, row.center(), None, cx);
        window.render_frame(cx);
    })
    .unwrap();

    clock(cx, BEFORE);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("tooltip").is_none(),
            "a disabled row answered before its wait was up"
        );
    })
    .unwrap();

    clock(cx, Duration::from_millis(1));
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("tooltip").is_some(),
            "a row that cannot be hovered named nothing"
        );
    })
    .unwrap();

    dispose(&fixture, cx);
}

/// over the row rather than the row itself: a path is named once the pointer
/// has rested on the row, and the row below waits for itself as well.
#[gpui_kit::test]
fn a_file_row_names_its_path_after_the_pointer_rests_on_it(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let key = |path: &str| (fixture.checkout.clone(), path.to_owned());
    cx.update_window(fixture.handle, |_, _, cx| {
        fixture.view.update(cx, |app, cx| {
            app.file_entries = std::collections::HashMap::from([(
                key(""),
                ["one.txt", "two.txt"]
                    .into_iter()
                    .map(|name| FileEntry {
                        name: name.into(),
                        path: name.into(),
                        directory: false,
                    })
                    .collect(),
            )]);
            app.inspector_tab = InspectorTab::Files;
            cx.notify();
        })
    })
    .unwrap();

    let mut rows = false;
    for _ in 0..32 {
        cx.run_until_parked();
        rows = cx
            .update_window(fixture.handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find("file-control:one.txt").is_some()
            })
            .unwrap();
        if rows {
            break;
        }
    }
    assert!(rows, "the file rows are on screen");

    let (one, two) = cx
        .update_window(fixture.handle, |_, window, _| {
            (
                window.find("file-control:one.txt").bounds(),
                window.find("file-control:two.txt").bounds(),
            )
        })
        .unwrap();

    cx.update_window(fixture.handle, |_, window, cx| {
        pointer_move(window, one.center(), None, cx);
        window.render_frame(cx);
        assert!(
            window.try_find("tooltip").is_none(),
            "a file row answered a pointer that had not rested on it"
        );
    })
    .unwrap();

    clock(cx, BEFORE);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("tooltip").is_none(),
            "the wait was up early"
        );
    })
    .unwrap();

    clock(cx, Duration::from_millis(1));
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("tooltip").is_some(),
            "the row waited and nothing answered"
        );

        pointer_move(window, two.center(), None, cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(
            window.try_find("tooltip").is_none(),
            "the row the pointer moved to did not wait"
        );
    })
    .unwrap();

    clock(cx, BEFORE);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("tooltip").is_none(),
            "the second file row did not wait either"
        );
    })
    .unwrap();

    clock(cx, Duration::from_millis(1));
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("tooltip").is_some(),
            "the second file row waited and nothing answered"
        );
    })
    .unwrap();

    dispose(&fixture, cx);
}
