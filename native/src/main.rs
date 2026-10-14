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
    // This stack guard outlives GPUI's quit callbacks and their final writer
    // flush. OS process death also releases it, without stale PID recovery.
    // Before anything that can log: without a sink installed, every record
    // the terminal and the window emit is discarded.
    muster_native::logging::install();
    let _instance = match muster_core::navigation::NativeInstanceLock::acquire() {
        Ok(instance) => instance,
        Err(error) => {
            eprintln!("muster-native: {error}");
            std::process::exit(1);
        }
    };
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(|cx| {
            gpui_kit::init(cx);
            muster_native::theme::install(cx);
            // Before any window exists: the sidebar's family has to be registered
            // before the first frame asks for it, or GPUI answers with a fallback.
            app::load_fonts(cx);
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
