//! The menu bar, as the platform is handed it.
//!
//! Legacy's menu was the same two submenus, in the same order, with the same
//! items; these tests are what says so. The Edit submenu's assertions are about
//! the OS actions rather than the labels because that is the part that decides
//! what AppKit answers: an item carrying `OsAction::Copy` is the one `⌘C`
//! reaches, and the toolkit's actions are what this app's fields and its
//! terminal both listen for.

use gpui_kit::{Keystroke, OsAction, OwnedMenuItem, SystemMenuType, TestAppContext};

use super::*;

/// The app menu comes first because on macOS the first submenu *is* the
/// application menu, and it carries what macOS expects an app menu to carry.
#[gpui_kit::test]
fn the_app_menu_is_first_and_carries_what_macos_expects(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::theme::install(cx);
        install(cx);
    });
    let menus = cx.update(|cx| cx.get_menus().expect("the menu bar was installed"));
    let app = menus.first().expect("the app menu is there");
    assert_eq!(app.name.as_ref(), "Muster");
    assert_eq!(
        item_names(&app.items),
        vec![
            "About Muster",
            "",
            "Services",
            "",
            "Hide Muster",
            "Hide Others",
            "",
            "Quit Muster",
        ],
        "the same items, in the same order, as Legacy's app menu"
    );
    assert!(
        app.items
            .iter()
            .any(|item| matches!(item, OwnedMenuItem::SystemMenu(menu) if menu.menu_type == SystemMenuType::Services)),
        "Services is the one submenu the platform fills in"
    );
}

/// The Edit submenu is not decoration: these are the key equivalents that carry
/// `⌘C`, `⌘V`, `⌘X`, `⌘A` and `⌘Z` to whatever is focused.
#[gpui_kit::test]
fn the_edit_submenu_carries_the_answers_rather_than_the_names(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::theme::install(cx);
        install(cx);
    });
    let menus = cx.update(|cx| cx.get_menus().expect("the menu bar was installed"));
    let edit = menus.get(1).expect("the Edit submenu follows the app menu");
    assert_eq!(edit.name.as_ref(), "Edit");
    assert_eq!(
        os_actions(&edit.items),
        vec![
            ("Undo", Some("Undo")),
            ("Redo", Some("Redo")),
            ("Cut", Some("Cut")),
            ("Copy", Some("Copy")),
            ("Paste", Some("Paste")),
            ("Select All", Some("SelectAll")),
        ],
        "each item is the OS answer as well as this app's action"
    );
}

/// The three the app menu has no key binding for anywhere else. A menu item is
/// drawn with the equivalent the keymap holds, so without these they would be
/// three items with nothing beside their names.
#[gpui_kit::test]
fn the_app_menu_items_carry_their_key_equivalents(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::theme::install(cx);
        install(cx);
    });
    for (keystroke, action) in [
        ("cmd-q", "muster::Quit"),
        ("cmd-h", "muster::Hide"),
        ("cmd-alt-h", "muster::HideOthers"),
    ] {
        let key = Keystroke::parse(keystroke).expect("the keystroke parses");
        let bound = cx.update(|cx| cx.key_bindings().borrow().all_bindings_for_input(&[key]));
        assert!(
            bound
                .iter()
                .any(|binding| binding.action().name() == action),
            "{keystroke} answers the app menu's own item, so the item is drawn with it"
        );
    }
}

fn item_names(items: &[OwnedMenuItem]) -> Vec<&str> {
    items
        .iter()
        .map(|item| match item {
            OwnedMenuItem::Separator => "",
            OwnedMenuItem::Action { name, .. } => name.as_ref(),
            OwnedMenuItem::SystemMenu(menu) => menu.name.as_ref(),
            OwnedMenuItem::Submenu(menu) => menu.name.as_ref(),
        })
        .collect()
}

/// `(label, OS action)` for every action item, named rather than compared with
/// `Debug`: `OsAction` derives none, and what matters here is which item
/// carries which answer.
fn os_actions(items: &[OwnedMenuItem]) -> Vec<(&str, Option<&'static str>)> {
    items
        .iter()
        .filter_map(|item| match item {
            OwnedMenuItem::Action {
                name, os_action, ..
            } => Some((name.as_ref(), os_action.map(os_action_name))),
            _ => None,
        })
        .collect()
}

fn os_action_name(action: OsAction) -> &'static str {
    match action {
        OsAction::Cut => "Cut",
        OsAction::Copy => "Copy",
        OsAction::Paste => "Paste",
        OsAction::SelectAll => "SelectAll",
        OsAction::Undo => "Undo",
        OsAction::Redo => "Redo",
    }
}
