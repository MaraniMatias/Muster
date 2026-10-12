/**
 * What a right-click opens, and where it is allowed to open anything.
 *
 * The window is a webview, and a webview answers a right-click with a menu of its own: Reload,
 * Back, Forward, Look Up, and Inspect Element in a build with a debugger attached. Reload is the
 * one that costs something. The app keeps unsaved drafts in the code editor and writes into the
 * user's repositories, so a reload drops every open draft on the floor. And it does it from a
 * right-click that landed on a sidebar row and did not look like it had hit anything at all.
 *
 * There is no way to take that menu away from the host. wry carries `with_default_context_menus`,
 * but it is applied in its WebView2 path and nowhere else, and Tauri does not expose it, so the
 * event is the only lever there is.
 *
 * That menu used to be kept on the surfaces text is selected from — the editor, the terminal, the
 * read views, the fields — because it is where Cut, Copy and Paste arrive already greyed out when
 * there is nothing to act on. It is kept nowhere now, and the reason is what macOS draws on top of
 * it.
 *
 * While a process is watching the mouse — a screen recorder, a clip tool, a remote-desktop client —
 * a right-click opens two menus: the webview's, and a second one beside it carrying Insert Emoji
 * and Insert Unicode Control Character. Two menus for one click, and both of them offering things
 * the app can do from the keyboard instead. Preventing the event is what takes both away, because
 * the second menu is drawn by the same WebKit that draws the first, and nothing in Rust says
 * otherwise. The capture cannot be told apart from the safe case either: the webview is never told
 * that a process is watching the mouse, so a menu kept for the clicks where nothing is capturing
 * would also be kept on every click of the capture, which is the one click it cannot be kept for.
 *
 * So it goes everywhere, including the editor, the terminal and the fields. What that menu was
 * worth is not lost: `menus.rs` builds an Edit submenu of Undo, Redo, Cut, Copy, Paste and Select
 * All, and the key equivalents on those items send `copy:`, `paste:`, `cut:` and `selectAll:` down
 * the responder chain into the webview, so ⌘C and ⌘V reach the editor, the terminal and a field the
 * way the menu reached them. The app's own right-click menus — the group menu and the move menu in
 * the sidebar — are drawn in the page, so they are none of this menu's business.
 */

/**
 * Denies the webview menu everywhere, on every surface and for every target.
 *
 * Capture, so that a library handling the event on its way up cannot reach the webview's menu by
 * stopping it before it gets here.
 */
export function installContextMenu(root: Document = document): void {
  root.addEventListener("contextmenu", (event) => event.preventDefault(), true);
}
