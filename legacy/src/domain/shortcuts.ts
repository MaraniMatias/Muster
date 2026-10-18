/**
 * The modifier this platform's shortcuts are pressed with.
 *
 * macOS is ⌘ and everywhere else is Ctrl, and the two are not two spellings of one thing: on
 * macOS `Ctrl` is the Control key, which the terminal and the editor already answer to. `⌘N` and
 * `Ctrl+N` are the same chord in a Linux terminal panel and two different chords on a Mac, where
 * the second is readline's next-history. A shortcut that accepts both takes that key away from
 * whatever had the focus, which is the one thing a window-wide shortcut must not do.
 *
 * The platform is read at each call rather than once at import, so a window that moves between
 * machines — or a test that says which machine it is — is not answered from a value decided
 * before anybody asked.
 */

/** The part of a key event this decides from: which modifier keys are held down. */
export interface ModifierKey {
  metaKey: boolean;
  ctrlKey: boolean;
}

/** Whether what this window reports names a Mac. Anything else is the Ctrl platform. */
export function isMacPlatform(platform: string): boolean {
  // `Darwin` is in the test because a webview can report `X11; Darwin arm64` where another
  // reports `MacIntel`, and Darwin is the only part of the first that tells macOS apart from the
  // Linux it is otherwise shaped like. Neither string names the other.
  return /mac|darwin/i.test(platform);
}

/**
 * Whether this key carries the modifier a shortcut on this platform is pressed with.
 *
 * `platform` is the string to read, defaulting to what this window reports. It is the platform and
 * not the user agent: a webview that reports one always reports it — `MacIntel` on a Mac whatever
 * its architecture — while the user agent is a document about every browser there is, and reading
 * it would make a test unable to say which machine it is on.
 */
export function carriesAppModifier(event: ModifierKey, platform: string = navigator.platform): boolean {
  return isMacPlatform(platform) ? event.metaKey : event.ctrlKey;
}

/**
 * What the window answers to, written down so Settings can say so.
 *
 * These are not preferences, which is why they are not in `SETTINGS_SECTIONS`: none of them is
 * written to `~/.muster/config.yml`, none is restored, and a person cannot change one. They are a
 * record of fact about this build, the same way the credits are, so the dialog draws them and
 * writes nothing.
 *
 * A chord is a list of keys in press order rather than one string, because the modifier is a
 * different key on each platform and a chord written out once is wrong everywhere else. `MOD` is
 * that key: `shortcutChord` spells it the way the window this is read on presses it.
 */
export interface Shortcut {
  /** The keys in the order they are pressed. `MOD` is ⌘ on macOS and Ctrl everywhere else. */
  keys: readonly string[];
  /** What it does, in a line: the keys alone say nothing about what a person is trying to do. */
  description: string;
}

export interface ShortcutGroup {
  title: string;
  shortcuts: Shortcut[];
}

/**
 * The chords the window answers wherever it has the focus, and the few it answers inside one place.
 *
 * A chord here that the app stopped answering is a line of help that lies, and one it gained is a
 * line of help that is missing: this list and the handlers are two facts about the same thing and
 * `App.test.ts` is what keeps them from drifting apart.
 */
export const SHORTCUT_GROUPS: ShortcutGroup[] = [
  {
    title: "Window",
    shortcuts: [
      { keys: ["MOD", "/"], description: "Show or hide the side panels" },
      { keys: ["MOD", "N"], description: "New terminal in the open workdir" },
      { keys: ["MOD", "+"], description: "Make the window bigger" },
      { keys: ["MOD", "−"], description: "Make the window smaller" },
      { keys: ["MOD", "0"], description: "Back to 100%" },
    ],
  },
  {
    title: "In context",
    shortcuts: [
      { keys: ["Enter"], description: "Confirm the session name being typed" },
      { keys: ["Esc"], description: "Close a dialog, cancel a rename, dismiss a message" },
      { keys: ["MOD", "click"], description: "Open the file a link in the terminal points at" },
      { keys: ["Home"], description: "First section in the inspector, from the tab strip" },
      { keys: ["End"], description: "Last section in the inspector" },
    ],
  },
];

/**
 * A chord as this platform presses it, which is `⌘/` on a Mac and `Ctrl+/` anywhere else.
 *
 * The keys are joined with nothing on macOS and with `+` elsewhere, because a Mac writes a chord as
 * the glyphs sit on the keyboard — `⌘N` — while the other platforms have to say that the modifier is
 * a separate key to be pressed along with it.
 */
export function shortcutChord(keys: readonly string[], platform: string = navigator.platform): string {
  const mac = isMacPlatform(platform);
  return keys.map((key) => (key === "MOD" ? (mac ? "⌘" : "Ctrl") : key)).join(mac ? "" : "+");
}
