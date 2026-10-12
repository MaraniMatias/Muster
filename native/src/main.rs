//! The native build's entry point.
//!
//! Three of gpui-kit's real entry points and nothing else: `application` opens
//! the platform, `init` initialises the layers, and `open_window` builds a
//! window over a [`TerminalWindow`].
//!
//! [`TerminalWindow`]: muster_native::app::TerminalWindow

use gpui_kit::{App, AppContext, Window};

use muster_native::app::{self, TerminalWindow};

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(|cx| {
            gpui_kit::init(cx);
            gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
            gpui_kit::open_window(
                app::window_bounds(cx),
                cx,
                |window: &mut Window, cx: &mut App| {
                    // Before the window exists: a quit that arrives while the shell
                    // is spawning has to find somebody already holding it.
                    app::install_quit_path(cx);
                    cx.new(|cx| TerminalWindow::new(window, cx))
                },
            )
            .expect("muster-native: failed to open window");
        });
}
