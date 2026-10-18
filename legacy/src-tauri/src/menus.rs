//! The macOS menu bar: the app menu, and the one submenu the platform will not let an app do
//! without.
//!
//! Tauri builds a menu of its own when the builder is given none, and that one carries File, View,
//! Window and an empty Help. So a small menu bar is not the absence of menu code, it is menu code
//! that says what stays. These are the two submenus that stay.

use tauri::menu::{AboutMetadata, Menu, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Runtime};

/// The menu bar the app opens with.
pub fn build<R: Runtime>(handle: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let app_menu = Submenu::with_items(
        handle,
        handle.package_info().name.clone(),
        true,
        &[
            &PredefinedMenuItem::about(handle, None, Some(about_metadata(handle)))?,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::services(handle, None)?,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::hide(handle, None)?,
            &PredefinedMenuItem::hide_others(handle, None)?,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::quit(handle, None)?,
        ],
    )?;

    // The separators are cosmetic and every one of the items is not. On macOS the webview is
    // never told about `⌘C`, `⌘V`, `⌘X`, `⌘A` or `⌘Z`; the key equivalents on these items are
    // what send `copy:`, `paste:`, `cut:`, `selectAll:` and `undo:` down the responder chain into
    // it. An app whose menu bar has no Edit submenu cannot copy out of its own search field, its
    // own editor or its own terminal, and the copy menu on the right-click does not stand in.
    let edit_menu = Submenu::with_items(
        handle,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(handle, None)?,
            &PredefinedMenuItem::redo(handle, None)?,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::cut(handle, None)?,
            &PredefinedMenuItem::copy(handle, None)?,
            &PredefinedMenuItem::paste(handle, None)?,
            &PredefinedMenuItem::select_all(handle, None)?,
        ],
    )?;

    // The app menu comes first because on macOS the first submenu *is* the application menu.
    Menu::with_items(handle, &[&app_menu, &edit_menu])
}

/// What the About panel says: the fields macOS shows, and the only ones it shows.
///
/// `AboutMetadata` has a `website`, and macOS ignores it. Of the whole struct the platform draws
/// the name, the version, the short version, the copyright, an icon and the credits, so the one
/// line of free text is where the repository goes.
///
/// The credits are read from Cargo's `homepage` and not from `bundle.homepage`, which looks like
/// the field to read and is not: the codegen that embeds the config into the binary hardcodes that
/// one to `None` (`ToTokens for BundleConfig` in tauri-utils), along with `copyright` and
/// `publisher`, so anything read off `config().bundle` for display is silently absent at runtime.
/// `env!` is resolved by rustc and does land in the binary, and Cargo already carries the same URL.
pub(crate) fn about_metadata<R: Runtime>(handle: &AppHandle<R>) -> AboutMetadata<'_> {
    AboutMetadata {
        name: Some(handle.package_info().name.clone()),
        version: Some(handle.package_info().version.to_string()),
        credits: Some(env!("CARGO_PKG_HOMEPAGE").to_string()),
        ..Default::default()
    }
}
