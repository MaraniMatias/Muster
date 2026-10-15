//! What the About panel says, and how it goes away.
//!
//! Legacy's About was the platform's own, filled from Tauri's metadata: the
//! name, the version, the copyright, the icon, and the repository as the one
//! line of credits. The toolkit has no such panel, so these are the assertions
//! that Muster's says the same five things.

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AnyWindowHandle, AppContext, Entity, TestAppContext};

use super::*;

/// The five things, in the order macOS shows them.
#[gpui_kit::test]
fn the_panel_says_what_the_platforms_own_panel_said(cx: &mut TestAppContext) {
    let (handle, _view) = open_panel(cx);
    cx.update_window(handle, |_, window, _| {
        assert!(window.find("about-icon").visible(), "the app's own icon");
        assert_eq!(window.find("about-name").label(), Some(NAME));
        assert_eq!(
            window.find("about-version").label(),
            Some(version().as_str())
        );
        assert_eq!(window.find("about-copyright").label(), Some(COPYRIGHT));
        assert_eq!(
            window.find("about-credits").label(),
            Some(env!("CARGO_PKG_HOMEPAGE"))
        );
    })
    .unwrap();
    dispose(handle, cx);
}

/// The credits line is the one thing the panel can answer a question about, so
/// clicking it has to reach the browser: a URL the panel shows and never opens
/// is a caption.
#[gpui_kit::test]
fn clicking_the_credits_opens_the_repository(cx: &mut TestAppContext) {
    let (handle, _view) = open_panel(cx);
    cx.update_window(handle, |_, window, cx| window.click("about-credits", cx))
        .unwrap();
    assert_eq!(
        cx.opened_url().as_deref(),
        Some(env!("CARGO_PKG_HOMEPAGE")),
        "the repository line opens where the platform opens links"
    );
    dispose(handle, cx);
}

/// `⌘W` is what closes a window on macOS, so it has to close this one: a panel
/// that answers the platform's close key is closed the way every other window
/// is, and a panel that does not is closed by pointing at it.
#[gpui_kit::test]
fn the_close_key_closes_the_panel(cx: &mut TestAppContext) {
    cx.update(super::super::menus::install);
    let (handle, _view) = open_panel(cx);
    cx.update_window(handle, |_, window, cx| window.press("cmd-w", cx))
        .unwrap();
    assert!(
        cx.update(|cx| !cx.windows().contains(&handle)),
        "⌘W closes the panel the same way it closes any window"
    );
}

/// One panel at a time: macOS answers a second About by leaving the one that is
/// there, and a second window would be a second thing to find and close.
#[gpui_kit::test]
fn a_second_request_finds_the_panel_that_is_already_open(cx: &mut TestAppContext) {
    ready(cx);
    cx.update(open);
    let before = cx.update(|cx| cx.windows().len());
    cx.update(open);
    assert_eq!(
        cx.update(|cx| cx.windows().len()),
        before,
        "opening About twice is still one panel"
    );
    let Some(handle) = cx.update(|cx| cx.windows().pop()) else {
        panic!("the panel opened")
    };
    dispose(handle, cx);
}

fn open_panel(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<About>) {
    ready(cx);
    cx.update(|cx| {
        gpui_kit::open_window(window_options(cx), cx, |window, cx| {
            cx.new(|cx| About::new(window, cx))
        })
    })
    .unwrap()
}

/// What the panel draws with: the installed palette, which is a global the app
/// sets before any window exists and the panel reads like every other surface.
fn ready(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::theme::install(cx);
    });
}

fn dispose(handle: AnyWindowHandle, cx: &mut TestAppContext) {
    cx.update_window(handle, |_, window, _| window.remove_window())
        .unwrap();
}
