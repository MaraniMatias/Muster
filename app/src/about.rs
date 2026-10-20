//! The About panel: what the app menu's first item opens.
//!
//! Legacy's app menu carried the platform's own About item, filled from
//! Tauri's `AboutMetadata`: the name, the version, the copyright, the icon, and
//! the repository as the one line of credits. The toolkit has no About panel to
//! fill in, so this is the one Muster draws, and it says the same five things in
//! the order macOS shows them.
//!
//! A window, not an overlay, because that is what an About panel is: it can sit
//! behind the main window and it is closed with the platform's own close path,
//! `⌘W` included.

use std::cell::RefCell;
use std::sync::Arc;

use gpui_kit::{
    div, img, px, size, App, AppContext, Bounds, Context, FocusHandle, Image, ImageFormat,
    ImageSource, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, TestSupportExt, TitlebarOptions, WeakEntity, Window,
    WindowBackgroundAppearance, WindowBounds, WindowOptions,
};

use crate::menus::CloseAbout;
use crate::theme::Roles;

/// The key context the panel's own bindings live in, so `⌘W` closes the panel
/// and nothing else in the app.
pub(crate) const CONTEXT: &str = "About";

/// The name every surface of the app answers to.
const NAME: &str = "Muster";

/// The copyright the bundle carries and the panel repeats.
const COPYRIGHT: &str = "Copyright (c) 2026 Marani Matias Ezequiel";

/// The 512×512 the bundle's `.icns` was built from: one artwork, so the panel
/// and the Dock cannot drift apart.
const ICON: &[u8] = include_bytes!("../assets/icon.png");

/// The version line, read from the crate rather than from the bundle that holds
/// it, which is what Legacy's About read too.
fn version() -> String {
    format!("Version {}", env!("CARGO_PKG_VERSION"))
}

thread_local! {
    /// The panel that is open, while one is. macOS answers a second request for
    /// About by leaving the one that is there rather than by opening another, so
    /// what decides that is whether this one is still alive.
    static OPEN: RefCell<Option<WeakEntity<About>>> = const { RefCell::new(None) };
}

/// What the panel says about the app, over its own window.
pub struct About {
    focus: FocusHandle,
    icon: Arc<Image>,
}

impl About {
    fn new(window: &mut Window, cx: &mut App) -> Self {
        let focus = cx.focus_handle();
        // Focused on open, because a panel that cannot be closed by the key is
        // a panel with to be closed by the pointer.
        focus.focus(window, cx);
        About {
            focus,
            icon: Arc::new(Image::from_bytes(ImageFormat::Png, ICON.to_vec())),
        }
    }

    fn on_close(&mut self, _: &CloseAbout, window: &mut Window, _cx: &mut Context<Self>) {
        window.remove_window();
    }
}

impl Render for About {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let roles = Roles::of(cx);
        div()
            .id("muster-about")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(7.))
            .bg(roles.surface_raised)
            .track_focus(&self.focus)
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::on_close))
            .child(
                img(ImageSource::Image(Arc::clone(&self.icon)))
                    .id("about-icon")
                    .test_support()
                    .w(px(96.))
                    .h(px(96.)),
            )
            .child(
                div()
                    .id("about-name")
                    .test_support()
                    // The panel's rows are static text, so each one names itself
                    // rather than being four anonymous runs of it.
                    .aria_label(NAME)
                    .text_size(px(20.))
                    .text_color(roles.text)
                    .child(NAME),
            )
            .child(
                div()
                    .id("about-version")
                    .test_support()
                    .aria_label(version())
                    .text_size(px(12.))
                    .text_color(roles.text_muted)
                    .child(version()),
            )
            .child(
                div()
                    .id("about-copyright")
                    .test_support()
                    .aria_label(COPYRIGHT)
                    .text_size(px(11.))
                    .text_color(roles.text_muted)
                    .child(COPYRIGHT),
            )
            // The credits, and the only line of free text the platform's own
            // panel had room for. Read from Cargo for the reason `menus.rs`
            // records: `env!` is resolved by rustc, so the URL is in the binary
            // rather than looked up in the bundle at runtime.
            //
            // A link rather than a caption: clicking it hands the URL to the
            // platform's own opener, the same way the platform's own panel does,
            // and the pointer says so before the click — pointer, underline.
            .child(
                div()
                    .id("about-credits")
                    .test_support()
                    .aria_label(env!("CARGO_PKG_HOMEPAGE"))
                    .text_size(px(11.))
                    .text_color(roles.accent)
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .on_click(|_, _, cx| cx.open_url(env!("CARGO_PKG_HOMEPAGE")))
                    .child(env!("CARGO_PKG_HOMEPAGE")),
            )
    }
}

/// Open the panel, unless it is already open.
///
/// macOS answers a second request for About by leaving the one that is there,
/// and a weak reference is what says whether it is still there.
pub fn open(cx: &mut App) {
    let already_open = OPEN.with(|open| open.borrow().as_ref().and_then(WeakEntity::upgrade));
    if already_open.is_some() {
        return;
    }
    let opened = gpui_kit::open_window(window_options(cx), cx, |window, cx| {
        cx.new(|cx| About::new(window, cx))
    })
    .ok();
    if let Some((_, view)) = opened {
        OPEN.with(|open| *open.borrow_mut() = Some(view.downgrade()));
    }
}

/// The panel's window: the size its content is, and a title the platform draws.
fn window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(380.), px(340.)),
            cx,
        ))),
        window_min_size: Some(size(px(380.), px(340.))),
        titlebar: Some(TitlebarOptions {
            title: Some("About Muster".into()),
            ..TitlebarOptions::default()
        }),
        window_background: WindowBackgroundAppearance::Opaque,
        ..WindowOptions::default()
    }
}

#[cfg(test)]
#[path = "about_tests.rs"]
mod tests;
