//! What the preferences panel promises, checked through the window that draws it.
//!
//! That the panel opens is not worth a test — a dialog that renders is what a window is for.
//! What is worth pinning is the promise it makes about the *file*: nothing reaches it before
//! Apply, a thrown-away draft leaves it as it was, and what was written is what the next open
//! shows. The other two are the failure it has to survive, a settings file somebody broke by
//! hand, and the row list, which is what says whether a preference this build has no answer
//! for is being offered.
//!
//! Every test hands the panel a temporary file rather than reaching the machine's own, so
//! running these cannot touch a person's preferences.

use gpui_kit::test::TestWindowExt;
use gpui_kit::{point, px, AppContext, ScrollDelta, TestAppContext};
use muster_core::review::ReviewStorage;
use muster_core::settings::{self, AppSettings, TerminalCursorStyle};

use super::sidebar_tests::{dispose, open, Fixture};
use crate::tooltip::SHOW_DELAY;

/// A short wait is not the wait: the app holds a trigger's tooltip for
/// `tooltip::SHOW_DELAY`, and the panel's own triggers are the ones that say
/// why they are disabled.
const TOOLTIP_WAIT: std::time::Duration = SHOW_DELAY;

/// The panel, opened on a temporary file this test owns.
///
/// The directory outlives the fixture because the panel is re-opened against the same file at
/// the end of most of these, which is the whole claim being made: what Apply wrote is what the
/// next open shows.
struct Panel {
    fixture: Fixture,
    directory: tempfile::TempDir,
}

impl Panel {
    fn path(&self) -> std::path::PathBuf {
        self.directory.path().join("settings.json")
    }

    /// Open the panel again on the same file, as a person reopening it would.
    fn reopen(&self, cx: &mut TestAppContext) {
        let view = self.fixture.view.clone();
        let path = self.path();
        cx.update_window(self.fixture.handle, move |_, window, cx| {
            view.update(cx, |app, cx| app.open_settings_at(path, window, cx));
        })
        .unwrap();
    }
}

fn open_panel(cx: &mut TestAppContext) -> Panel {
    let panel = Panel {
        fixture: open(cx),
        directory: tempfile::tempdir().unwrap(),
    };
    panel.reopen(cx);
    panel
}

/// Change the draft the way a control does, without going through a keystroke.
fn edit(cx: &mut TestAppContext, panel: &Panel, edit: fn(&mut AppSettings)) {
    let view = panel.fixture.view.clone();
    cx.update_window(panel.fixture.handle, move |_, _, cx| {
        view.update(cx, |app, cx| app.edit_settings_draft(cx, edit));
    })
    .unwrap();
}

/// Apply, which is the only door to the file.
fn apply(cx: &mut TestAppContext, panel: &Panel) {
    let view = panel.fixture.view.clone();
    cx.update_window(panel.fixture.handle, move |_, window, cx| {
        view.update(cx, |app, cx| app.apply_settings_draft(window, cx));
    })
    .unwrap();
    cx.run_until_parked();
}

/// The terminal's size field, the one these tests type into.
fn terminal_size_field(
    panel: &Panel,
    cx: &mut TestAppContext,
) -> gpui_kit::Entity<gpui_kit::component::input::InputState> {
    panel.fixture.view.read_with(cx, |app, _| {
        app.settings
            .as_ref()
            .expect("the panel is open")
            .terminal_font_size
            .clone()
    })
}

/// A changed draft is still a draft: the file is not touched until Apply.
#[gpui_kit::test]
fn nothing_reaches_the_file_before_apply(cx: &mut TestAppContext) {
    let panel = open_panel(cx);
    edit(cx, &panel, |settings| {
        settings.terminal.ligatures = false;
        settings.terminal.cursor_style = TerminalCursorStyle::Underline;
    });
    assert!(
        !panel.path().exists(),
        "the draft was changed and nothing was written"
    );
    dispose(&panel.fixture, cx);
}

/// Cancel throws the draft away, so the next open is the file rather than what was typed.
#[gpui_kit::test]
fn a_thrown_away_draft_leaves_the_file_as_it_was(cx: &mut TestAppContext) {
    let panel = open_panel(cx);
    edit(cx, &panel, |settings| settings.terminal.ligatures = false);
    let view = panel.fixture.view.clone();
    cx.update_window(panel.fixture.handle, move |_, _, cx| {
        view.update(cx, |app, cx| {
            // The door Cancel and Escape both walk out of.
            app.settings = None;
            cx.notify();
        });
    })
    .unwrap();
    assert!(!panel.path().exists());

    panel.reopen(cx);
    assert!(
        panel
            .fixture
            .view
            .read_with(cx, |app, _| app
                .settings
                .as_ref()
                .expect("the panel is open")
                .draft)
            .terminal
            .ligatures,
        "the change was in the draft, and the draft was thrown away"
    );
    dispose(&panel.fixture, cx);
}

/// Apply writes the draft, closes the panel, and the next open reads the file back.
#[gpui_kit::test]
fn apply_writes_the_draft_and_the_next_open_reads_it_back(cx: &mut TestAppContext) {
    let panel = open_panel(cx);
    edit(cx, &panel, |settings| {
        settings.terminal.ligatures = false;
        settings.terminal.cursor_style = TerminalCursorStyle::Bar;
    });
    // The size is chosen in its field, which is where the panel reads it from: writing one
    // into the model would be a preference nobody can reach with a keyboard.
    let number = terminal_size_field(&panel, cx);
    cx.update_window(panel.fixture.handle, move |_, window, cx| {
        number.update(cx, |input, cx| input.set_value("18", window, cx));
    })
    .unwrap();
    apply(cx, &panel);
    assert!(
        panel
            .fixture
            .view
            .read_with(cx, |app, _| app.settings.is_none()),
        "the panel closes on an Apply that landed, which is how a person is told it did"
    );

    let written = settings::load(&panel.path()).expect("what was written is readable");
    assert!(!written.terminal.ligatures);
    assert_eq!(written.terminal.cursor_style, TerminalCursorStyle::Bar);
    assert_eq!(written.terminal.font_size, 18.0);

    panel.reopen(cx);
    assert_eq!(
        panel.fixture.view.read_with(cx, |app, _| {
            app.settings
                .as_ref()
                .expect("the panel is open")
                .draft
                .terminal
                .cursor_style
        }),
        TerminalCursorStyle::Bar,
        "the file, not the defaults it was opened on the first time"
    );
    dispose(&panel.fixture, cx);
}

/// Somebody edited the file by hand and got it wrong. The panel still opens — on the defaults
/// it draws, with the failure on screen — because whoever can fix it is who opened it. Apply
/// then refuses, rather than replacing a typo they could have found.
#[gpui_kit::test]
fn a_broken_file_opens_the_panel_says_so_and_is_never_written_over(cx: &mut TestAppContext) {
    let panel = open_panel(cx);
    std::fs::write(panel.path(), "{ not json").unwrap();
    panel.reopen(cx);
    cx.update_window(panel.fixture.handle, |_, window, _| {
        assert!(
            window.find("settings-apply").visible(),
            "the panel opens on a file it could not read"
        );
    })
    .unwrap();
    assert_eq!(
        panel.fixture.view.read_with(cx, |app, _| {
            app.settings.as_ref().expect("the panel is open").draft
        }),
        AppSettings::default(),
        "the fields are the defaults; the failure is what is shown beside them"
    );
    assert!(
        panel.fixture.view.read_with(cx, |app, _| app
            .settings
            .as_ref()
            .expect("open")
            .error
            .is_some()),
        "and the failure is on screen"
    );

    apply(cx, &panel);
    assert_eq!(
        std::fs::read_to_string(panel.path()).unwrap(),
        "{ not json",
        "the file is left exactly as it was"
    );
    dispose(&panel.fixture, cx);
}

/// Reset replaces the draft rather than editing it, controls included: a Reset that changed
/// only the model would leave numbers on screen that Apply does not mean.
#[gpui_kit::test]
fn reset_puts_the_defaults_back_in_the_fields_too(cx: &mut TestAppContext) {
    let panel = open_panel(cx);
    let number = terminal_size_field(&panel, cx);
    cx.update_window(panel.fixture.handle, move |_, window, cx| {
        number.update(cx, |input, cx| input.set_value("22", window, cx));
    })
    .unwrap();
    edit(cx, &panel, |settings| settings.terminal.ligatures = false);

    let view = panel.fixture.view.clone();
    cx.update_window(panel.fixture.handle, move |_, window, cx| {
        view.update(cx, |app, cx| app.reset_settings_draft(window, cx));
    })
    .unwrap();
    assert_eq!(
        panel.fixture.view.read_with(cx, |app, cx| {
            app.settings
                .as_ref()
                .expect("the panel is open")
                .resolve(cx)
        }),
        AppSettings::default(),
        "the field that said 22 says 14 again, not just the model behind it"
    );
    dispose(&panel.fixture, cx);
}

/// A size outside its range is clamped on the way in, and the field is told what was written,
/// so a person who typed 400 sees the number that landed rather than the one they did not.
#[gpui_kit::test]
fn a_size_out_of_range_is_clamped_and_the_field_is_told(cx: &mut TestAppContext) {
    let panel = open_panel(cx);
    let number = terminal_size_field(&panel, cx);
    cx.update_window(panel.fixture.handle, move |_, window, cx| {
        number.update(cx, |input, cx| input.set_value("400", window, cx));
    })
    .unwrap();
    apply(cx, &panel);

    let written = settings::load(&panel.path()).expect("what was written is readable");
    assert_eq!(
        written.terminal.font_size,
        settings::TERMINAL_FONT_SIZE_LIMITS.max,
        "400px is not a size this app can draw"
    );
    panel.reopen(cx);
    assert_eq!(
        panel.fixture.view.read_with(cx, |app, cx| {
            app.settings
                .as_ref()
                .expect("the panel is open")
                .resolve(cx)
                .terminal
                .font_size
        }),
        settings::TERMINAL_FONT_SIZE_LIMITS.max,
        "and the field shows the number that is on file"
    );
    dispose(&panel.fixture, cx);
}

/// The panel is taller than a dialog is allowed to be, so its body is a scroll area: the last
/// row is off the bottom until the wheel reaches it, and a body that cannot scroll would leave
/// the rows under the footer with no way to them at all.
#[gpui_kit::test]
fn the_body_scrolls_to_the_rows_that_do_not_fit(cx: &mut TestAppContext) {
    let panel = open_panel(cx);
    cx.update_window(panel.fixture.handle, |_, window, cx| {
        assert!(
            window.find("settings-ui-font-size").visible(),
            "the panel opens at its first row"
        );
        assert!(
            !window.find("settings-review-storage").visible(),
            "and its last row is below the fold, which is what has to be reachable"
        );
        // GPUI's sign: a negative delta is down.
        window.scroll(
            "settings-ui-font-size",
            ScrollDelta::Pixels(point(px(0.), px(-600.))),
            cx,
        );
        assert!(
            window.find("settings-review-storage").visible(),
            "the wheel reaches the rows the dialog could not show"
        );
    })
    .unwrap();
    dispose(&panel.fixture, cx);
}

/// Every preference this build keeps has a row, and nothing it has no answer for is offered.
#[gpui_kit::test]
fn every_preference_has_a_row_and_nothing_else_does(cx: &mut TestAppContext) {
    let panel = open_panel(cx);
    cx.update_window(panel.fixture.handle, |_, window, _| {
        for id in [
            "settings-ui-font-size",
            "settings-ui-sticky",
            "settings-terminal-font-size",
            "settings-terminal-ligatures",
            "settings-terminal-cursor",
            "settings-terminal-cursor-blink",
            "settings-terminal-selection-copy",
            "settings-terminal-scrollbar",
            "settings-terminal-shell-integration",
            "settings-editor-font-size",
            "settings-editor-ligatures",
            "settings-editor-cursor-blink",
            "settings-editor-indent-with",
            "settings-editor-indent-size",
            "settings-review-storage",
        ] {
            assert!(window.try_find(id).is_some(), "{id} is a row of the panel");
        }
        assert!(window.find("settings-ui-font-size").visible());
        // The theme, the editor background and the interface scale are not preferences this
        // build has: one appearance is dark and `theme.rs` is its only authority.
        for absent in [
            "settings-ui-theme",
            "settings-ui-content-background",
            "settings-ui-zoom",
        ] {
            assert!(
                window.try_find(absent).is_none(),
                "{absent} is not a preference here"
            );
        }
    })
    .unwrap();
    dispose(&panel.fixture, cx);
}

/// The switch is compact, and the picker and number field share the same height.
#[gpui_kit::test]
fn settings_form_controls_share_a_compact_height(cx: &mut TestAppContext) {
    let panel = open_panel(cx);
    cx.update_window(panel.fixture.handle, |_, window, _| {
        let heights = [
            window
                .find("settings-ui-sticky")
                .bounds()
                .size
                .height
                .as_f32(),
            window
                .find("settings-terminal-cursor")
                .bounds()
                .size
                .height
                .as_f32(),
            window
                .find("settings-terminal-font-size")
                .bounds()
                .size
                .height
                .as_f32(),
        ];
        assert!(
            (heights[0] - 16.).abs() <= 1.,
            "compact switch: {heights:?}"
        );
        assert!(
            (heights[1] - heights[2]).abs() <= 1.,
            "picker and number field should share a height: {heights:?}"
        );
    })
    .unwrap();
    dispose(&panel.fixture, cx);
}

/// The two maintenance actions share one line and are no wider than they read.
///
/// They used to be rows of their own, which stretched each of them across the whole
/// panel: two full-width bars with a label centred in each, the width a toolbar is
/// drawn at and the place a maintenance command must not be given.
#[gpui_kit::test]
fn the_review_actions_share_a_line_and_are_not_the_width_of_the_panel(cx: &mut TestAppContext) {
    let panel = open_panel(cx);
    cx.update_window(panel.fixture.handle, |_, window, cx| {
        window.scroll(
            "settings-ui-font-size",
            ScrollDelta::Pixels(point(px(0.), px(-600.))),
            cx,
        );
        window.render_frame(cx);
        let row = window.find("settings-review-storage").bounds();
        let clear = window.find("settings-review-clear").bounds();
        let forget = window.find("settings-review-forget").bounds();
        assert!(
            (clear.origin.y - forget.origin.y).abs() < px(1.),
            "the two actions are one line: {clear:?} {forget:?}"
        );
        // Right-aligned to the row's own edge, with only the gap between them
        // left over: that is what two buttons as wide as they read look like.
        let content_right = row.origin.x + row.size.width;
        assert!(
            clear.origin.x + clear.size.width + px(8.) + forget.size.width - content_right < px(1.),
            "neither action was stretched across the panel: {clear:?} {forget:?} {row:?}"
        );
    })
    .unwrap();
    dispose(&panel.fixture, cx);
}

/// The folder a round is written to is named under the row that chooses it, in full,
/// once the pointer rests on it.
///
/// The path is cut to the column, and the tail is the half that identifies a
/// checkout: a path nobody can read the end of is not a place they can go and check.
#[gpui_kit::test]
fn the_review_folder_is_named_in_full_after_the_pointer_rests_on_it(cx: &mut TestAppContext) {
    let panel = open_panel(cx);
    cx.update_window(panel.fixture.handle, |_, window, cx| {
        window.scroll(
            "settings-ui-font-size",
            ScrollDelta::Pixels(point(px(0.), px(-600.))),
            cx,
        );
        window.render_frame(cx);
        assert!(
            {
                let folder = window.find("settings-review-folder").bounds();
                let picker = window.find("settings-review-storage").bounds();
                folder.origin.x + folder.size.width <= picker.origin.x - px(16.)
            },
            "the path is cut before the picker rather than running under it"
        );
        window.hover("settings-review-folder", cx);
        window.render_frame(cx);
        assert!(
            window.try_find("tooltip").is_none(),
            "the folder answered before its wait was up"
        );
    })
    .unwrap();

    cx.executor().advance_clock(TOOLTIP_WAIT);
    cx.run_until_parked();
    cx.update_window(panel.fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("tooltip").is_some(),
            "a path cut to the row was never named in full"
        );
    })
    .unwrap();
    dispose(&panel.fixture, cx);
}

/// Clearing the cached exports is offered from the panel itself.
///
/// The cache is one folder under the home directory that every checkout writes to, so
/// the action needed a checkout rather than a diff open: it used to be disabled in
/// exactly the state the panel is opened in, which is a control nobody can reach and
/// no reason on screen.
#[gpui_kit::test]
fn clearing_cached_exports_is_offered_with_no_diff_open(cx: &mut TestAppContext) {
    let panel = Panel {
        fixture: open(cx),
        directory: tempfile::tempdir().unwrap(),
    };
    // The app cache, which is the one folder under the home directory that the
    // action clears. It is not the default: rounds default to the checkout.
    cx.update_window(panel.fixture.handle, |_, _, cx| {
        panel.fixture.view.update(cx, |app, cx| {
            app.navigation.review_storage = ReviewStorage::Default;
            cx.notify();
        });
    })
    .unwrap();
    panel.reopen(cx);
    assert!(
        panel
            .fixture
            .view
            .read_with(cx, |app, _| app.open_diff.is_none()),
        "the panel is opened from the title bar, with no diff open"
    );
    cx.update_window(panel.fixture.handle, |_, window, cx| {
        window.scroll(
            "settings-ui-font-size",
            ScrollDelta::Pixels(point(px(0.), px(-600.))),
            cx,
        );
        window.click("settings-review-clear", cx);
        window.render_frame(cx);
        assert!(
            window.try_find("confirm-clear-review-exports").is_some(),
            "the action did nothing, which is what a disabled control does"
        );
    })
    .unwrap();
    dispose(&panel.fixture, cx);
}
