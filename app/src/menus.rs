//! The macOS menu bar: the app menu, and the Edit submenu.
//!
//! The app this one replaces built the same two submenus, and wrote down there
//! why the second one is not decoration: on macOS the key
//! equivalents on the Edit submenu are what carry `⌘C`, `⌘V`, `⌘X`, `⌘A` and
//! `⌘Z` to whatever is focused. This app already answers those from its key
//! bindings — the toolkit binds them, and the terminal answers `⌘C` and `⌘V` in
//! `on_key_down` before `input::press` refuses a platform-modified press. What
//! it did not have was the menu that shows them.
//!
//! That is also the one thing here that changes behaviour rather than adding it:
//! a menu item with a key equivalent is answered by AppKit before the key ever
//! reaches the window, so the terminal's `⌘C`/`⌘V` would stop being reached at
//! all. Which is why `TerminalView` also listens for `input::Copy` and
//! `input::Paste` (`terminal::view`), and why the Edit submenu carries the
//! toolkit's own actions rather than ones of this crate's own.
//!
//! macOS only, and only where [`install`] is called. On the other targets the
//! window's bar is the bar, and `Services` is a facility this platform does not
//! have. The menu is still built here so its shape is checkable anywhere.

use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use gpui_kit::{actions, App, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType};

actions!(
    muster,
    [
        /// The About panel, as the app menu's first item.
        About,
        /// The About panel's own `⌘W`. It lives here with the bindings that
        /// carry a menu item's equivalent, because that is where the app's
        /// key-level answers live; it is scoped to the panel's key context.
        CloseAbout,
        /// Leave the menu bar, as Hide does everywhere else.
        Hide,
        /// Leave every other application's menu bar.
        HideOthers,
        /// Stop the terminals and leave, through the app's own quit path.
        Quit
    ]
);

/// The menu bar, as data: what the platform is asked to draw.
///
/// The app menu comes first because on macOS the first submenu *is* the
/// application menu, and it answers to the app's name.
pub fn menus() -> Vec<Menu> {
    vec![
        Menu::new("Muster").items(vec![
            MenuItem::action("About Muster", About),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Muster", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::separator(),
            MenuItem::action("Quit Muster", Quit),
        ]),
        Menu::new("Edit").items(vec![
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ]),
    ]
}

/// Hand the menu bar to the platform and answer what it carries.
pub fn install(cx: &mut App) {
    // What the app menu's own items are drawn with: a menu item shows the
    // equivalent the keymap holds for its action, and nothing else. `⌘Q`,
    // `⌘H` and `⌘⌥H` are macOS's own; `⌘W` belongs to the About panel the app
    // menu opens, and is scoped to that panel's key context.
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("cmd-alt-h", HideOthers, None),
        KeyBinding::new("cmd-w", CloseAbout, Some(crate::about::CONTEXT)),
    ]);
    cx.on_action(|_: &About, cx| crate::about::open(cx));
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    // The workspace window owns the Quit decision, because it can preserve a
    // dirty document. It is registered application-wide rather than on one
    // element: the platform asks what is answerable for the focused element
    // when it draws the item, so an answer attached to the window's root is
    // only offered while something inside that root holds the focus.
    cx.on_action(|_: &Quit, cx| crate::app::quit_workspace_window(cx));
    cx.set_menus(menus());
}

#[cfg(test)]
#[path = "menus_tests.rs"]
mod tests;
