import { describe, expect, it } from "vitest";
import {
  DEFAULT_SETTINGS,
  EDITOR_FONT_SIZE_LIMITS,
  INDENTATION_SIZE_LIMITS,
  SETTINGS_SECTIONS,
  TERMINAL_FONT_SIZE_LIMITS,
  THEME_PREFERENCES,
  UI_FONT_SIZE_LIMITS,
  normalizeSettings,
  uiFontScale,
  valueAt,
  withValue,
} from "./settings";
import { ZOOM_STEPS } from "./zoom";

/**
 * The dark surface the app ships with, named here once.
 *
 * It is written in `src/domain/settings.ts` and again in the Rust defaults, because a file this app
 * does not own the writing of still has to have a value to read back. That is two copies that can
 * disagree, so the test states the answer rather than reading it from the thing it checks: an
 * expectation of `DEFAULT_SETTINGS` would pass whatever that copy happens to say today.
 */
const DEFAULT_CONTENT_BACKGROUND = "#16181c";

describe("the settings file", () => {
  it("names the three surfaces and everything they carry", () => {
    expect(DEFAULT_SETTINGS).toEqual({
      ui: {
        fontSize: 14,
        zoom: 1,
        theme: "system",
        contentBackground: DEFAULT_CONTENT_BACKGROUND,
        treeStickyScroll: true,
      },
      terminal: {
        fontSize: 16,
        ligatures: true,
        cursorBlink: true,
        cursorStyle: "block",
        scrollbar: "hidden",
        shellIntegration: true,
        followWorktree: "agent" as const,
        selectionCopy: true,
      },
      editor: { fontSize: 13, ligatures: true, cursorBlink: true, indentation: { useSpaces: true, size: 2 } },
      reviews: { storage: "default" },
    });
  });

  it("reads what was written, and a file with lines missing from it", () => {
    const written = {
      ui: {
        fontSize: 16,
        zoom: 1.2 as const,
        theme: "light" as const,
        contentBackground: "#aabbcc",
        treeStickyScroll: true,
      },
      terminal: {
        fontSize: 18,
        ligatures: false,
        cursorBlink: false,
        cursorStyle: "bar" as const,
        scrollbar: "always" as const,
        shellIntegration: false,
        followWorktree: "both" as const,
        selectionCopy: false,
      },
      editor: { fontSize: 15, ligatures: false, cursorBlink: false, indentation: { useSpaces: false, size: 4 } },
      reviews: { storage: "workdir" as const },
    };
    expect(normalizeSettings(JSON.parse(JSON.stringify(written)))).toEqual(written);
    // A file somebody edited by hand is allowed to be partial: a line they deleted is a preference
    // they did not set, not a shape this build cannot read. The one line that is there is kept.
    expect(normalizeSettings({ ui: { fontSize: 18 } })).toEqual({
      ...DEFAULT_SETTINGS,
      ui: { ...DEFAULT_SETTINGS.ui, fontSize: 18 },
    });
    expect(normalizeSettings(undefined)).toEqual(DEFAULT_SETTINGS);
    // A storage mode is one of two names, so a third in a hand-edited file is the app's folder
    // rather than a folder the app would have to invent.
    expect(normalizeSettings({ reviews: { storage: "elsewhere" } }).reviews.storage).toBe("default");
  });

  it("clamps a size outside the range and answers 100% for a scale it cannot draw", () => {
    expect(normalizeSettings({ ui: { fontSize: 400 } }).ui.fontSize).toBe(UI_FONT_SIZE_LIMITS.max);
    expect(normalizeSettings({ ui: { fontSize: 2 } }).ui.fontSize).toBe(UI_FONT_SIZE_LIMITS.min);
    expect(normalizeSettings({ terminal: { fontSize: 400 } }).terminal.fontSize).toBe(TERMINAL_FONT_SIZE_LIMITS.max);
    expect(normalizeSettings({ editor: { fontSize: 4 } }).editor.fontSize).toBe(EDITOR_FONT_SIZE_LIMITS.min);
    expect(normalizeSettings({ editor: { indentation: { size: 40 } } }).editor.indentation.size).toBe(
      INDENTATION_SIZE_LIMITS.max,
    );
    expect(normalizeSettings({ editor: { indentation: { size: 2.5 } } }).editor.indentation.size).toBe(
      DEFAULT_SETTINGS.editor.indentation.size,
    );
    // `.nan` is a number a YAML file can hold and no font size can be, so it is a default rather
    // than a value that reaches the renderer.
    expect(normalizeSettings({ ui: { fontSize: Number.NaN } }).ui.fontSize).toBe(DEFAULT_SETTINGS.ui.fontSize);
    expect(normalizeSettings({ ui: { zoom: 4 } }).ui.zoom).toBe(1.5);
    expect(normalizeSettings({ ui: { zoom: 0.1 } }).ui.zoom).toBe(0.8);
  });

  it("keeps a theme it knows and falls back to the system for one it does not", () => {
    expect(normalizeSettings({ ui: { theme: "light" } }).ui.theme).toBe("light");
    expect(normalizeSettings({ ui: { theme: "dark" } }).ui.theme).toBe("dark");
    // A preference rather than state worth refusing a file over, and there is a default that
    // answers for whatever the system is set to.
    expect(normalizeSettings({ ui: { theme: "solarized" } }).ui.theme).toBe("system");
  });

  it("accepts six-digit hex content backgrounds and defaults anything else", () => {
    expect(normalizeSettings({ ui: { contentBackground: "#A1b2C3" } }).ui.contentBackground).toBe("#a1b2c3");
    // A name rather than a color is what a hand-edited file and somebody picking from the OS picker
    // can both produce, and a value CSS cannot read is not a preference: it is the default instead.
    for (const invalid of ["red", "#12345", "#1234567", 42, null]) {
      expect(normalizeSettings({ ui: { contentBackground: invalid } }).ui.contentBackground, String(invalid)).toBe(
        DEFAULT_CONTENT_BACKGROUND,
      );
    }
  });

  it("keeps a scrollbar mode it knows and falls back to hidden for one it does not", () => {
    expect(normalizeSettings({ terminal: { scrollbar: "always" } }).terminal.scrollbar).toBe("always");
    expect(normalizeSettings({ terminal: { scrollbar: "warp" } }).terminal.scrollbar).toBe("hidden");
    // A boolean typed where a string belongs is a mistake, not a mode, and it is a preference
    // rather than state worth refusing the whole file over.
    expect(normalizeSettings({ terminal: { ligatures: "yes" } }).terminal.ligatures).toBe(
      DEFAULT_SETTINGS.terminal.ligatures,
    );
    expect(normalizeSettings({ terminal: { ligatures: false } }).terminal.ligatures).toBe(false);
  });

  it("keeps a cursor shape it knows and falls back to the block for one it does not", () => {
    expect(normalizeSettings({ terminal: { cursorStyle: "underline" } }).terminal.cursorStyle).toBe("underline");
    // A shape xterm.js cannot draw is a word somebody typed, and the block is what this build opens on.
    expect(normalizeSettings({ terminal: { cursorStyle: "beam" } }).terminal.cursorStyle).toBe("block");
  });

  it("keeps copy-on-selection as the boolean it is and defaults anything else to on", () => {
    expect(normalizeSettings({ terminal: { selectionCopy: false } }).terminal.selectionCopy).toBe(false);
    expect(normalizeSettings({ terminal: { selectionCopy: true } }).terminal.selectionCopy).toBe(true);
    // A file that never heard of the preference keeps the behaviour this build opens with, and a
    // string where a boolean belongs is a mistake rather than a reason to refuse the whole file.
    expect(normalizeSettings({}).terminal.selectionCopy).toBe(true);
    for (const invalid of ["yes", 1, null, {}]) {
      expect(normalizeSettings({ terminal: { selectionCopy: invalid } }).terminal.selectionCopy, String(invalid)).toBe(
        true,
      );
    }
  });

  it("keeps the four worktree follow modes and defaults invalid values to agent", () => {
    expect(DEFAULT_SETTINGS.terminal.followWorktree).toBe("agent");
    for (const mode of ["off", "cd", "agent", "both"]) {
      expect(normalizeSettings({ terminal: { followWorktree: mode } }).terminal.followWorktree).toBe(mode);
    }
    for (const mode of [undefined, null, true, "always"]) {
      expect(normalizeSettings({ terminal: { followWorktree: mode } }).terminal.followWorktree).toBe("agent");
    }
  });

  it("does not normalize deleted terminal preferences", () => {
    const terminal = normalizeSettings({
      terminal: {
        followAgentAcrossWorktrees: false,
        followDirectoryAcrossWorktrees: true,
        followSelection: "always",
        changeDirectoryOnMove: false,
      },
    }).terminal;
    expect(terminal).toEqual(DEFAULT_SETTINGS.terminal);
  });

  it("scales the interface against the size the design is drawn at", () => {
    expect(uiFontScale(DEFAULT_SETTINGS.ui.fontSize)).toBe(1);
    expect(uiFontScale(UI_FONT_SIZE_LIMITS.max)).toBeCloseTo(UI_FONT_SIZE_LIMITS.max / 14);
  });
});

describe("the form's schema", () => {
  it("names every preference once, and every path names a field", () => {
    const paths = SETTINGS_SECTIONS.flatMap((section) => section.fields.map((field) => field.path));
    expect(new Set(paths).size).toBe(paths.length);
    for (const section of SETTINGS_SECTIONS) {
      for (const field of section.fields) {
        expect(() => valueAt(DEFAULT_SETTINGS, field.path), field.path).not.toThrow();
        expect(field.label.length, field.path).toBeGreaterThan(0);
      }
    }
  });

  it("places worktree following before failed command reporting", () => {
    const terminal = SETTINGS_SECTIONS.find((section) =>
      section.fields.some((field) => field.path === "terminal.followWorktree"),
    )!;
    expect(terminal.fields.map((field) => field.path)).toEqual([
      "terminal.fontSize",
      "terminal.ligatures",
      "terminal.cursorStyle",
      "terminal.cursorBlink",
      "terminal.selectionCopy",
      "terminal.scrollbar",
      "terminal.followWorktree",
      "terminal.shellIntegration",
    ]);
  });

  it("offers the window scale as the steps the keyboard walks, and nothing else", () => {
    const zoom = SETTINGS_SECTIONS.flatMap((section) => section.fields).find((field) => field.path === "ui.zoom");
    expect(zoom?.kind).toBe("select");
    expect(zoom?.kind === "select" && zoom.options.map((option) => option.value)).toEqual(ZOOM_STEPS.map(String));
  });

  it("reads and writes through a path without editing the tree it was given", () => {
    const next = withValue(DEFAULT_SETTINGS, "editor.indentation.size", 4);
    expect(next.editor.indentation.size).toBe(4);
    expect(DEFAULT_SETTINGS.editor.indentation.size).toBe(2);

    expect(valueAt(next, "editor.indentation.useSpaces")).toBe(true);
    expect(withValue(DEFAULT_SETTINGS, "terminal.ligatures", false).terminal.ligatures).toBe(false);
    expect(valueAt(withValue(DEFAULT_SETTINGS, "ui.zoom", 1.3), "ui.zoom")).toBe(1.3);
  });

  it("describes native command reporting without claiming startup output is discarded", () => {
    const field = SETTINGS_SECTIONS.flatMap((section) => section.fields).find(
      (candidate) => candidate.path === "terminal.shellIntegration",
    );
    expect(field?.kind).toBe("toggle");
    // On by default, because it is the only way a failed command's row can be red: a command that
    // fails does not end the shell, so nothing else in the app can see that it did.
    expect(DEFAULT_SETTINGS.terminal.shellIntegration).toBe(true);
    expect(normalizeSettings(undefined).terminal.shellIntegration).toBe(true);
    expect(withValue(DEFAULT_SETTINGS, "terminal.shellIntegration", false).terminal.shellIntegration).toBe(false);
    const description = field && "description" in field ? field.description : "";
    expect(description).toMatch(/command failures/i);
    expect(description).toMatch(/without clearing shell startup output/i);
    expect(description).not.toMatch(/discarded/i);
    expect(description).toMatch(/new terminals only/i);
  });

  it("offers copy-on-selection as a toggle that is on by default", () => {
    const field = SETTINGS_SECTIONS.flatMap((section) => section.fields).find(
      (candidate) => candidate.path === "terminal.selectionCopy",
    );
    expect(field?.kind).toBe("toggle");
    // On is the behaviour this build opens with: a selection gesture copying what it selected is
    // what a terminal has always done here, so the preference describes it rather than adding it.
    expect(DEFAULT_SETTINGS.terminal.selectionCopy).toBe(true);
    expect(normalizeSettings(undefined).terminal.selectionCopy).toBe(true);
    expect(withValue(DEFAULT_SETTINGS, "terminal.selectionCopy", false).terminal.selectionCopy).toBe(false);
    expect(valueAt(DEFAULT_SETTINGS, "terminal.selectionCopy")).toBe(true);
  });

  it("turns a select's string back into the value it stands for", () => {
    const select = (path: string) =>
      SETTINGS_SECTIONS.flatMap((section) => section.fields).find((field) => field.path === path);
    const indent = select("editor.indentation.useSpaces");
    const scrollbar = select("terminal.scrollbar");
    const theme = select("ui.theme");
    expect(indent?.kind === "select" && indent.parse("false")).toBe(false);
    expect(indent?.kind === "select" && String(indent.parse("true"))).toBe("true");
    expect(scrollbar?.kind === "select" && scrollbar.parse("always")).toBe("always");
    // The two palettes and the answer to both, in that order: following the system is what somebody
    // who has never opened this dialog is doing, so it is what the field starts on.
    expect(theme?.kind === "select" && theme.options.map((option) => option.value)).toEqual([...THEME_PREFERENCES]);
    expect(theme?.kind === "select" && theme.parse("light")).toBe("light");
  });
});
