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

/// The sections the panel is drawn in, and the rows each one shows.
///
/// The rows are looked for a section at a time: the panel shows one section,
/// and the rows of the others are not drawn at all.
const SECTIONS: [(&str, &[&str]); 4] = [
    (
        "settings-section-interface",
        &["settings-ui-font-size", "settings-ui-sticky"],
    ),
    (
        "settings-section-terminal",
        &[
            "settings-terminal-font-size",
            "settings-terminal-ligatures",
            "settings-terminal-cursor",
            "settings-terminal-cursor-blink",
            "settings-terminal-selection-copy",
            "settings-terminal-scrollbar",
            "settings-terminal-shell-integration",
            "settings-terminal-follow-worktree",
        ],
    ),
    (
        "settings-section-editor",
        &[
            "settings-editor-font-size",
            "settings-editor-ligatures",
            "settings-editor-cursor-blink",
            "settings-editor-indent-with",
            "settings-editor-indent-size",
        ],
    ),
    (
        "settings-section-review",
        &[
            "settings-review-storage",
            "settings-review-clear",
            "settings-review-forget",
        ],
    ),
];

/// The section the panel is showing.
fn selected_section(cx: &mut TestAppContext, panel: &Panel) -> usize {
    panel.fixture.view.read_with(cx, |app, _| {
        app.settings
            .as_ref()
            .expect("the panel is open")
            .selected_section
    })
}

/// Show one section of the panel.
///
/// The sidebar is not a scroll area, so the section is always on screen and
/// the click lands on it.
fn select_section(cx: &mut TestAppContext, panel: &Panel, section: usize) {
    let id = SECTIONS[section].0;
    // The click is retried: a click that lands while the card is still
    // settling into place is a click on nothing.
    for _ in 0..5 {
        if selected_section(cx, panel) == section {
            return;
        }
        cx.update_window(panel.fixture.handle, |_, window, cx| {
            window.click(id, cx);
            window.render_frame(cx);
        })
        .unwrap();
    }
    panic!("the section {id} could not be chosen");
}

/// Where the panel's rows are scrolled to, read off the first row of the
/// section on screen.
fn body_offset(cx: &mut TestAppContext, panel: &Panel) -> Option<gpui_kit::Pixels> {
    let first = SECTIONS[selected_section(cx, panel)].1[0];
    cx.update_window(panel.fixture.handle, |_, window, _| {
        window.try_find(first).map(|row| row.bounds().origin.y)
    })
    .unwrap()
}

/// Scroll the panel's rows by `delta` pixels.
///
/// A wheel only reaches the scroll area it is over, so the anchor is a row of
/// the section on screen, and the scroll is made where the panel is drawn
/// rather than where the window is.
fn scroll_body(cx: &mut TestAppContext, panel: &Panel, delta: f32) {
    let section = selected_section(cx, panel);
    for anchor in SECTIONS[section].1 {
        let visible = cx
            .update_window(panel.fixture.handle, |_, window, _| {
                window.try_find(*anchor).map(|found| found.visible()) == Some(true)
            })
            .unwrap();
        if !visible {
            continue;
        }
        let before = body_offset(cx, panel);
        cx.update_window(panel.fixture.handle, |_, window, cx| {
            window.scroll(*anchor, ScrollDelta::Pixels(point(px(0.), px(delta))), cx);
        })
        .unwrap();
        if body_offset(cx, panel) != before {
            return;
        }
    }
}

/// Scroll the panel's rows to the top, so the section is read from its first row.
fn scroll_to_the_top(cx: &mut TestAppContext, panel: &Panel) {
    for _ in 0..40 {
        let before = body_offset(cx, panel);
        scroll_body(cx, panel, 200.);
        if body_offset(cx, panel) == before {
            return;
        }
    }
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
    // The terminal section is the longest one, so it is the one that does not fit.
    select_section(cx, &panel, 1);
    scroll_to_the_top(cx, &panel);
    cx.update_window(panel.fixture.handle, |_, window, _| {
        assert!(
            !window.find("settings-terminal-follow-worktree").visible(),
            "the last row of the section is below the fold, which is what has to be reachable"
        );
    })
    .unwrap();
    scroll_body(cx, &panel, -600.);
    cx.update_window(panel.fixture.handle, |_, window, _| {
        assert!(
            window.find("settings-terminal-follow-worktree").visible(),
            "the wheel reaches the rows the card could not show"
        );
    })
    .unwrap();
    dispose(&panel.fixture, cx);
}

/// Every preference this build keeps has a row, and nothing it has no answer for is offered.
///
/// The rows are looked for a section at a time, because a row is only in the
/// panel while its section is the one on screen.
#[gpui_kit::test]
fn every_preference_has_a_row_and_nothing_else_does(cx: &mut TestAppContext) {
    let panel = open_panel(cx);
    for (section, (_, rows)) in SECTIONS.iter().enumerate() {
        select_section(cx, &panel, section);
        cx.update_window(panel.fixture.handle, |_, window, _| {
            for id in *rows {
                assert!(window.try_find(*id).is_some(), "{id} is a row of the panel");
            }
        })
        .unwrap();
    }
    select_section(cx, &panel, 0);
    cx.update_window(panel.fixture.handle, |_, window, _| {
        for absent in [
            "settings-terminal-cursor",
            "settings-editor-font-size",
            "settings-review-storage",
        ] {
            assert!(
                window.try_find(absent).is_none(),
                "{absent} belongs to a section that is not on screen"
            );
        }
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

/// The sidebar of sections stands beside the rows, not above them.
///
/// The panel is a card with the sections on the left and the rows of the one
/// on screen on the right: the sections are where a person goes, and the rows
/// are what they came for.
#[gpui_kit::test]
fn the_sections_stand_beside_the_rows(cx: &mut TestAppContext) {
    let panel = open_panel(cx);
    cx.update_window(panel.fixture.handle, |_, window, _| {
        let section = window.find("settings-section-interface").bounds();
        let content = window.find("settings-content").bounds();
        let row = window.find("settings-ui-font-size").bounds();
        assert!(
            section.right() <= content.left(),
            "the sidebar is on the left of the rows: {section:?} {content:?}"
        );
        assert!(
            content.left() <= row.left(),
            "the rows are on the right of the sidebar: {content:?} {row:?}"
        );
        assert!(
            content.size.height >= section.size.height,
            "the sidebar runs the height of the card: {section:?} {content:?}"
        );
    })
    .unwrap();
    dispose(&panel.fixture, cx);
}

/// The sidebar chooses the section, and only that section's rows are drawn.
///
/// The panel is a sidebar of sections and the rows of the one on it, so a
/// section a person did not choose is not on screen at all.
#[gpui_kit::test]
fn the_sidebar_shows_one_section_at_a_time(cx: &mut TestAppContext) {
    let panel = open_panel(cx);
    assert_eq!(
        selected_section(cx, &panel),
        0,
        "the panel opens on the first section"
    );
    cx.update_window(panel.fixture.handle, |_, window, _| {
        assert!(
            window.find("settings-ui-font-size").visible(),
            "the rows of the section on screen are on screen"
        );
    })
    .unwrap();
    select_section(cx, &panel, 1);
    assert_eq!(
        selected_section(cx, &panel),
        1,
        "the terminal section is the one that was chosen"
    );
    cx.update_window(panel.fixture.handle, |_, window, _| {
        assert!(
            window.find("settings-terminal-cursor").visible(),
            "the rows of the chosen section are on screen"
        );
        assert!(
            window.try_find("settings-ui-font-size").is_none(),
            "the rows of the other sections are not drawn"
        );
    })
    .unwrap();
    dispose(&panel.fixture, cx);
}

/// The checkbox owns its full preference row; picker and number field share a height.
#[gpui_kit::test]
fn settings_form_controls_share_a_compact_height(cx: &mut TestAppContext) {
    let panel = open_panel(cx);
    select_section(cx, &panel, 0);
    cx.update_window(panel.fixture.handle, |_, window, _| {
        let checkbox = window
            .find("settings-ui-sticky")
            .bounds()
            .size
            .height
            .as_f32();
        assert!(
            (checkbox - 43.).abs() <= 1.,
            "the checkbox covers the text and the row's padding: {checkbox}"
        );
    })
    .unwrap();
    select_section(cx, &panel, 1);
    cx.update_window(panel.fixture.handle, |_, window, _| {
        let heights = [
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
            (heights[0] - heights[1]).abs() <= 1.,
            "picker and number field should share a height: {heights:?}"
        );
    })
    .unwrap();
    dispose(&panel.fixture, cx);
}

#[gpui_kit::test]
fn a_checkbox_changes_only_the_draft_until_apply(cx: &mut TestAppContext) {
    let panel = open_panel(cx);
    select_section(cx, &panel, 0);
    let checked = panel.fixture.view.read_with(cx, |app, _| {
        app.settings.as_ref().unwrap().draft.ui.tree_sticky_scroll
    });
    cx.update_window(panel.fixture.handle, |_, window, cx| {
        let checkbox = window.find("settings-ui-sticky");
        assert_eq!(checkbox.role(), Some(gpui_kit::Role::CheckBox));
        assert_eq!(checkbox.checked(), Some(checked));
        let row = checkbox.bounds();
        let field = window.find("settings-ui-font-size").bounds();
        assert!(
            row.size.width > field.size.width * 2.,
            "hover and activation must cover the label, not just the indicator"
        );
        assert!((row.right() - field.right()).as_f32().abs() <= 1.);
        window.hover("settings-ui-sticky", cx);
        assert_eq!(window.find("settings-ui-sticky").checked(), Some(checked));
        window.click("settings-ui-sticky", cx);
        assert_eq!(window.find("settings-ui-sticky").checked(), Some(!checked));
    })
    .unwrap();
    assert!(!panel.path().exists());
    apply(cx, &panel);
    assert_eq!(
        settings::load(&panel.path()).unwrap().ui.tree_sticky_scroll,
        !checked
    );
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
    select_section(cx, &panel, 3);
    cx.update_window(panel.fixture.handle, |_, window, _| {
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
    select_section(cx, &panel, 3);
    cx.update_window(panel.fixture.handle, |_, window, cx| {
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
    select_section(cx, &panel, 3);
    cx.update_window(panel.fixture.handle, |_, window, cx| {
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

#[gpui_kit::test]
fn blocked_apply_keeps_the_ui_responsive_and_failure_keeps_the_draft(cx: &mut TestAppContext) {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    let panel = open_panel(cx);
    edit(cx, &panel, |settings| settings.terminal.ligatures = false);
    let number = terminal_size_field(&panel, cx);
    cx.update_window(panel.fixture.handle, move |_, window, cx| {
        number.update(cx, |input, cx| input.set_value("23", window, cx));
    })
    .unwrap();

    let writes = Arc::new(AtomicUsize::new(0));
    let write_count = writes.clone();
    let delay = cx
        .background_executor
        .timer(std::time::Duration::from_secs(1));
    let save = cx.background_spawn(async move {
        write_count.fetch_add(1, Ordering::SeqCst);
        delay.await;
        Err("blocked test failure".to_owned())
    });
    let view = panel.fixture.view.clone();
    cx.update_window(panel.fixture.handle, move |_, _, cx| {
        view.update(cx, |app, _| {
            app.settings.as_mut().unwrap().save_override = Some(save);
        });
    })
    .unwrap();

    let view = panel.fixture.view.clone();
    cx.update_window(panel.fixture.handle, move |_, window, cx| {
        view.update(cx, |app, cx| app.apply_settings_draft(window, cx));
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(panel.fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("settings-cancel", cx);
        window.press("escape", cx);
        assert!(window.try_find("settings-apply").is_some());
    })
    .unwrap();

    // A second Apply is a no-op while the first is blocked; an ordinary UI update still runs.
    let view = panel.fixture.view.clone();
    cx.update_window(panel.fixture.handle, move |_, window, cx| {
        view.update(cx, |app, cx| {
            app.edit_settings_draft(cx, |settings| settings.terminal.ligatures = true);
            app.apply_settings_draft(window, cx);
        });
    })
    .unwrap();
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert!(panel.fixture.view.read_with(cx, |app, _| {
        app.settings.as_ref().is_some_and(|draft| draft.saving)
    }));

    cx.background_executor
        .advance_clock(std::time::Duration::from_secs(1));
    cx.run_until_parked();
    let view = &panel.fixture.view;
    assert!(view.read_with(cx, |app, _| app.settings.is_some()));
    assert!(!view.read_with(cx, |app, _| {
        app.settings.as_ref().unwrap().draft.terminal.ligatures
    }));
    assert_eq!(
        terminal_size_field(&panel, cx).read_with(cx, |input, _| input.value().to_string()),
        "23"
    );
    assert!(view.read_with(cx, |app, _| {
        app.settings
            .as_ref()
            .unwrap()
            .error
            .as_deref()
            .is_some_and(|error| error.contains("blocked test failure"))
    }));
    dispose(&panel.fixture, cx);
}

#[gpui_kit::test]
fn a_finished_save_cannot_close_a_replacement_draft(cx: &mut TestAppContext) {
    let panel = open_panel(cx);
    let delay = cx
        .background_executor
        .timer(std::time::Duration::from_secs(1));
    let save = cx.background_spawn(async move {
        delay.await;
        Ok(())
    });
    let view = panel.fixture.view.clone();
    cx.update_window(panel.fixture.handle, move |_, window, cx| {
        view.update(cx, |app, cx| {
            app.settings.as_mut().unwrap().save_override = Some(save);
            app.apply_settings_draft(window, cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
    // Simulate replacement by an owner, not a Cancel of an already-started write.
    let view = panel.fixture.view.clone();
    cx.update_window(panel.fixture.handle, move |_, _, cx| {
        view.update(cx, |app, _| app.settings = None);
    })
    .unwrap();
    panel.reopen(cx);
    cx.background_executor
        .advance_clock(std::time::Duration::from_secs(1));
    cx.run_until_parked();
    assert!(panel
        .fixture
        .view
        .read_with(cx, |app, _| app.settings.is_some()));
    dispose(&panel.fixture, cx);
}
