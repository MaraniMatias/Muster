import { describe, expect, it } from "vitest";
import { SHORTCUT_GROUPS, carriesAppModifier, isMacPlatform, shortcutChord } from "./shortcuts";

describe("the platform's shortcut modifier", () => {
  it("is Cmd on a Mac and Ctrl everywhere else", () => {
    // The two are not two spellings of one chord: on a Mac Ctrl is the Control key, and the
    // terminal answers to it — Ctrl+N walks back through the history — so a shortcut that takes
    // both takes a key that was already spoken for.
    expect(carriesAppModifier({ metaKey: true, ctrlKey: false }, "MacIntel")).toBe(true);
    expect(carriesAppModifier({ metaKey: false, ctrlKey: true }, "MacIntel")).toBe(false);
    expect(carriesAppModifier({ metaKey: false, ctrlKey: true }, "X11; Darwin arm64")).toBe(false);

    expect(carriesAppModifier({ metaKey: false, ctrlKey: true }, "Linux x86_64")).toBe(true);
    expect(carriesAppModifier({ metaKey: true, ctrlKey: false }, "Linux x86_64")).toBe(false);

    expect(carriesAppModifier({ metaKey: false, ctrlKey: true }, "Win32")).toBe(true);
    expect(carriesAppModifier({ metaKey: true, ctrlKey: false }, "Win32")).toBe(false);
  });

  it("reads this window's own platform when it is not told which one to read", () => {
    // No argument is the app's own call, so what it answers is a fact about the machine this
    // window is on rather than one passed in by whoever is asking. The platform is enough on its
    // own, which is what lets a test say which machine it is on.
    expect(carriesAppModifier({ metaKey: true, ctrlKey: false })).toBe(isMacPlatform(navigator.platform));
  });

  it("answers nothing for a key carrying no modifier at all", () => {
    for (const platform of ["MacIntel", "Linux x86_64"]) {
      expect(carriesAppModifier({ metaKey: false, ctrlKey: false }, platform)).toBe(false);
    }
  });

  it("reads the platform rather than the user agent's opinion of it", () => {
    // The two halves disagree on the same machine: an older WebKit names `MacIntel` and a current
    // one on Apple silicon names `X11; Darwin arm64`, and only the second is caught by its kernel.
    expect(isMacPlatform("MacIntel")).toBe(true);
    expect(isMacPlatform("macOS")).toBe(true);
    expect(isMacPlatform("X11; Darwin arm64")).toBe(true);
    expect(isMacPlatform("MacIntel Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)")).toBe(true);

    // Linux is the same X11 without the Darwin, which is exactly the pair that has to differ.
    expect(isMacPlatform("X11; Linux x86_64 Mozilla/5.0 (X11; Linux x86_64)")).toBe(false);
    expect(isMacPlatform("Linux x86_64")).toBe(false);
    expect(isMacPlatform("Win32 Mozilla/5.0 (Windows NT 10.0; Win64; x64)")).toBe(false);
  });
});

describe("the chords the Settings dialog writes down", () => {
  it("names a chord and something it does", () => {
    // A chord on its own says which keys and nothing about what a person is trying to do, and a row
    // with no keys to press is a row nobody can use. Both are what makes the section unreadable
    // rather than merely incomplete.
    expect(SHORTCUT_GROUPS.length).toBeGreaterThan(0);
    for (const group of SHORTCUT_GROUPS) {
      expect(group.title.length, group.title).toBeGreaterThan(0);
      expect(group.shortcuts.length, group.title).toBeGreaterThan(0);
      for (const shortcut of group.shortcuts) {
        expect(shortcut.keys.length, shortcut.description).toBeGreaterThan(0);
        expect(shortcut.description.trim(), shortcut.keys.join("")).not.toBe("");
      }
    }
  });

  it("has no two of the same chord written down twice", () => {
    // Two rows with the same keys are one shortcut listed twice, which reads as two things to do.
    const chords = SHORTCUT_GROUPS.flatMap((group) => group.shortcuts.map((shortcut) => shortcut.keys.join(" ")));
    expect(new Set(chords).size).toBe(chords.length);
  });

  it("spells each chord the way the platform it is shown on presses it", () => {
    // The one chord that means one thing on both, so the two spellings are both answerable.
    expect(shortcutChord(["MOD", "/"], "MacIntel")).toBe("⌘/");
    expect(shortcutChord(["MOD", "/"], "Linux x86_64")).toBe("Ctrl+/");
    expect(shortcutChord(["MOD", "N"], "X11; Darwin arm64")).toBe("⌘N");
    // A key with no modifier in it is the same on both, joined the same way: `F2` is not `Ctrl-F2`.
    expect(shortcutChord(["Esc"], "MacIntel")).toBe("Esc");
    expect(shortcutChord(["Esc"], "Linux x86_64")).toBe("Esc");
    expect(shortcutChord(["MOD", "click"], "MacIntel")).toBe("⌘click");
  });

  it("answers every chord as keys rather than as the token that stands for one of them", () => {
    // `shortcutChord` spells one token and joins the rest, so a chord it does not recognise would
    // reach the screen as the word itself: a row reading `MOD N` is a row nobody can press. This is
    // the property that makes the list above safe to draw, and the dialog test is what checks the
    // drawn text carries none of it.
    //
    // The `+` is only ever a separator on the Ctrl platform: on a Mac it is the zoom-in key itself,
    // so `⌘+` is a chord and not a `⌘` with something glued to it.
    for (const group of SHORTCUT_GROUPS) {
      for (const shortcut of group.shortcuts) {
        expect(shortcutChord(shortcut.keys, "MacIntel"), shortcut.description).not.toMatch(/MOD|Ctrl/);
        expect(shortcutChord(shortcut.keys, "Linux x86_64"), shortcut.description).not.toMatch(/MOD|⌘/);
      }
    }
  });
});
