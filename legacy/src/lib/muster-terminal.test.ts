// @vitest-environment happy-dom
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import {
  attachTerminalRenderer,
  createMusterTerminal,
  enableTerminalSelectionCopy,
  musterTerminalTheme,
  setTerminalLigatures,
  watchTerminalRendererRecovery,
  MAX_RENDERER_RECOVERIES,
} from "./muster-terminal";

/**
 * The token table as it is written down. Read from disk rather than imported, because vitest hands
 * back an empty string for a stylesheet it does not run, and an empty table would make every
 * assertion below pass for the wrong reason.
 */
const stylesheet = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "muster.css"), "utf8");

const stubs = vi.hoisted(() => ({
  loaded: [] as string[],
  fontLoads: [] as [string, string | undefined][],
  webglFails: false,
  disposed: 0,
  loseContext: null as (() => void) | null,
  terminal: null as FakeTerminal | null,
}));

class FakeTerminal {
  options: Record<string, unknown>;
  unicode = { activeVersion: "6", versions: ["6", "11"] };
  element = document.createElement("div");
  modes = { mouseTrackingMode: "none" as "none" | "drag" };
  selection = "";
  writeParsedListeners: Array<() => void> = [];
  addons: FakeAddon[] = [];
  constructor(options: Record<string, unknown>) {
    this.options = options;
    stubs.terminal = this;
  }
  loadAddon(addon: FakeAddon) {
    this.addons.push(addon);
    addon.activate(this);
  }
  joiners = new Map<number, (text: string) => [number, number][]>();
  joinerFailures = false;
  nextJoinerId = 1;
  registerCharacterJoiner(handler: (text: string) => [number, number][]) {
    if (this.joinerFailures) throw new Error("Terminal must be opened first");
    const id = this.nextJoinerId++;
    this.joiners.set(id, handler);
    return id;
  }
  deregisterCharacterJoiner(id: number) {
    this.joiners.delete(id);
  }
  get joiner() {
    return [...this.joiners.values()][0] ?? null;
  }
  set joiner(_handler: ((text: string) => [number, number][]) | null) {
    // The fake kept a single joiner before the preference became switchable; the setter is kept so
    // nothing has to know that, and it is the last registered one xterm.js would draw.
  }
  onWriteParsed(listener: () => void) {
    this.writeParsedListeners.push(listener);
    return {
      dispose: () => (this.writeParsedListeners = this.writeParsedListeners.filter((item) => item !== listener)),
    };
  }
  hasSelection() {
    return Boolean(this.selection);
  }
  getSelection() {
    return this.selection;
  }
}

interface FakeAddon {
  name: string;
  activate(terminal: FakeTerminal): void;
  dispose?(): void;
}

vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    constructor(options: Record<string, unknown>) {
      stubs.terminal = new FakeTerminal(options);
      return stubs.terminal as never;
    }
  },
}));

vi.mock("@xterm/addon-unicode11", () => ({
  Unicode11Addon: class implements FakeAddon {
    name = "unicode11";
    activate(terminal: FakeTerminal) {
      terminal.unicode.versions.push("11");
      stubs.loaded.push("unicode11");
    }
  },
}));

vi.mock("@xterm/addon-clipboard", () => ({
  ClipboardAddon: class implements FakeAddon {
    name = "clipboard";
    activate() {
      stubs.loaded.push("clipboard");
    }
  },
}));

vi.mock("@xterm/addon-webgl", () => ({
  WebglAddon: class implements FakeAddon {
    name = "webgl";
    private _listener: (() => void) | null = null;
    onContextLoss(listener: () => void) {
      this._listener = listener;
    }
    activate() {
      if (stubs.webglFails) throw new Error("WebGL2 is not supported");
      stubs.loaded.push("webgl");
      stubs.loseContext = () => this._listener?.();
    }
    dispose() {
      stubs.disposed += 1;
    }
  },
}));

function fakeTerminal() {
  return new FakeTerminal({});
}

describe("createMusterTerminal", () => {
  beforeEach(() => {
    stubs.loaded = [];
    stubs.webglFails = false;
    stubs.disposed = 0;
    stubs.loseContext = null;
  });

  // B.1: the face and the colors come from the tokens; the size and the cursor are preferences.
  it("builds the terminal the panel has always drawn", () => {
    createMusterTerminal();

    expect(stubs.terminal?.options).toMatchObject({
      allowProposedApi: true,
      cursorBlink: true,
      // A block by default: the cell is filled with the theme's cursor color and the glyph under it
      // is the surface, and the panel that is not the one you are typing into draws a frame instead.
      cursorStyle: "block",
      cursorInactiveStyle: "outline",
      // Icons behind the text face: the atlas is rasterized per glyph from this string, so a
      // family that is not in it draws a statusline's separators as tofu.
      fontFamily: '"Muster Nerd Mono", "Muster Nerd Icons", monospace',
      fontSize: 16,
      lineHeight: 1.2,
      scrollback: 10000,
    });
  });

  it("paints itself out of the stylesheet rather than out of a palette of its own", () => {
    // Every color is a `--muster-*` token, so which one arrives is the theme's answer rather than
    // this file's: a terminal opened in the light palette gets the light one without a second copy
    // of the values anywhere.
    const style = document.createElement("style");
    style.textContent = `:root { --muster-content-bg-0: #282c33; --muster-content-text: #dce0e5; --muster-cursor: #c2c9f1; }
      :root[data-theme="light"] { --muster-content-bg-0: #fafafa; --muster-content-text: #242529; --muster-cursor: #242529; }`;
    document.head.append(style);
    try {
      // The cursor pair is the requirement rather than a detail of the palette: the cursor is the
      // theme's own `--muster-cursor` and the glyph under a block is the theme's background, so a
      // cell with default colors under the cursor is drawn inverted and neither value is written
      // here. Two values rather than one per cell, because a cell that arrives with a color of its
      // own does not hand it to the block.
      document.documentElement.dataset.theme = "dark";
      expect(musterTerminalTheme()).toMatchObject({
        background: "#282c33",
        foreground: "#dce0e5",
        cursor: "#c2c9f1",
        cursorAccent: "#282c33",
      });

      document.documentElement.dataset.theme = "light";
      expect(musterTerminalTheme()).toMatchObject({
        background: "#fafafa",
        foreground: "#242529",
        cursor: "#242529",
        cursorAccent: "#fafafa",
      });
    } finally {
      style.remove();
    }
  });

  it("builds the terminal at the size and cursor the settings ask for", () => {
    createMusterTerminal(20, false, "underline", 1.2);

    // The size is the preference's own, multiplied by the window's scale: the terminal's host
    // cancels the scale out so the grid is measured in screen pixels, which means the cell has to
    // be handed the scaled size rather than inheriting one.
    expect(stubs.terminal?.options).toMatchObject({ fontSize: 24, cursorBlink: false, cursorStyle: "underline" });
  });

  it("hands xterm.js a full ANSI palette, and every color of it out of the stylesheet", () => {
    // The names are read out of the real stylesheet rather than listed here, because the list that
    // matters is the one `muster.css` carries: a token renamed there has to take this down with it,
    // and xterm.js' own palette — which is not this window's — is what a missing one falls back to.
    const names = [...new Set([...stylesheet.matchAll(/(--muster-[a-z0-9-]+):/g)].map(([, name]) => name))];
    const colors = names.map((_, index) => `#${(index + 1).toString(16).padStart(6, "0")}`);
    const style = document.createElement("style");
    style.textContent = `:root { ${names.map((name, index) => `${name}: ${colors[index]};`).join(" ")} }`;
    document.head.append(style);
    try {
      const theme = musterTerminalTheme();

      expect(Object.keys(theme).sort()).toEqual([
        "background",
        "black",
        "blue",
        "brightBlack",
        "brightBlue",
        "brightCyan",
        "brightGreen",
        "brightMagenta",
        "brightRed",
        "brightWhite",
        "brightYellow",
        "cursor",
        "cursorAccent",
        "cyan",
        "foreground",
        "green",
        "magenta",
        "red",
        "selectionBackground",
        "white",
        "yellow",
      ]);
      for (const value of Object.values(theme)) expect(colors).toContain(value);
    } finally {
      style.remove();
    }
  });

  it("turns on Unicode 11 and the clipboard handler before the first render", () => {
    const terminal = createMusterTerminal() as unknown as FakeTerminal;

    // Both are proposed API in 6.0, and both change how the terminal measures itself or answers
    // a paste, so they have to be in place before it is on the page.
    expect(terminal.unicode.activeVersion).toBe("11");
    expect(stubs.loaded).toEqual(["unicode11", "clipboard"]);
  });
});

describe("setTerminalLigatures", () => {
  it("registers the joiner that draws the programming ligatures", () => {
    const terminal = createMusterTerminal() as unknown as FakeTerminal;

    setTerminalLigatures(terminal as never, true);

    expect(terminal.joiner?.("a => b && c")).toEqual([
      [2, 4],
      [7, 9],
    ]);
  });

  it("takes the joiner back when the preference is switched off, and puts it back", () => {
    const terminal = createMusterTerminal() as unknown as FakeTerminal;

    setTerminalLigatures(terminal as never, true);
    setTerminalLigatures(terminal as never, false);
    expect(terminal.joiner).toBeNull();

    setTerminalLigatures(terminal as never, true);
    expect(terminal.joiner?.("a => b")).toEqual([[2, 4]]);
    // Switching to the value the terminal is already on is a no-op, so a preference that is
    // re-applied does not stack joiners.
    setTerminalLigatures(terminal as never, true);
    expect(terminal.joiners.size).toBe(1);
  });

  it("does not take the terminal down when xterm.js refuses the joiner", () => {
    const terminal = createMusterTerminal() as unknown as FakeTerminal;
    terminal.joinerFailures = true;

    expect(() => setTerminalLigatures(terminal as never, true)).not.toThrow();
    // Nothing was registered, so nothing has to be taken back, and switching off is still safe.
    expect(() => setTerminalLigatures(terminal as never, false)).not.toThrow();
  });
});

describe("enableTerminalSelectionCopy", () => {
  it("copies a selection after a mouse drag", () => {
    const terminal = fakeTerminal();
    terminal.selection = "selected text";
    const copy = vi.fn();
    const disposable = enableTerminalSelectionCopy(terminal as never, copy);

    terminal.element.dispatchEvent(new MouseEvent("mousedown", { button: 0 }));
    document.dispatchEvent(new MouseEvent("mousemove", { buttons: 1 }));
    document.dispatchEvent(new MouseEvent("mouseup", { button: 0 }));

    expect(copy).toHaveBeenCalledWith("selected text");
    disposable.dispose();
  });

  // A double click selects the word xterm.js measured, so it copies like a drag does.
  it("copies the word or the line a double and a triple click select", () => {
    const terminal = fakeTerminal();
    const copy = vi.fn();
    const disposable = enableTerminalSelectionCopy(terminal as never, copy);

    for (const detail of [2, 3]) {
      terminal.selection = `click ${detail}`;
      terminal.element.dispatchEvent(new MouseEvent("mousedown", { button: 0, detail }));
      document.dispatchEvent(new MouseEvent("mouseup", { button: 0, detail }));
    }

    expect(copy).toHaveBeenNthCalledWith(1, "click 2");
    expect(copy).toHaveBeenNthCalledWith(2, "click 3");
    disposable.dispose();
  });

  it("does not copy a click or a drag without a selection", () => {
    const terminal = fakeTerminal();
    const copy = vi.fn();
    const disposable = enableTerminalSelectionCopy(terminal as never, copy);

    terminal.element.dispatchEvent(new MouseEvent("mousedown", { button: 0 }));
    document.dispatchEvent(new MouseEvent("mouseup", { button: 0 }));
    terminal.element.dispatchEvent(new MouseEvent("mousedown", { button: 0 }));
    document.dispatchEvent(new MouseEvent("mousemove", { buttons: 1 }));
    document.dispatchEvent(new MouseEvent("mouseup", { button: 0 }));
    terminal.element.dispatchEvent(new MouseEvent("mousedown", { button: 0, detail: 2 }));
    document.dispatchEvent(new MouseEvent("mouseup", { button: 0, detail: 2 }));

    expect(copy).not.toHaveBeenCalled();
    disposable.dispose();
  });

  it("forces macOS selection only while an application owns the mouse", () => {
    const terminal = fakeTerminal();
    const disposable = enableTerminalSelectionCopy(terminal as never, vi.fn());

    expect(terminal.options.macOptionClickForcesSelection).toBe(false);
    terminal.modes.mouseTrackingMode = "drag";
    terminal.writeParsedListeners.forEach((listener) => listener());
    expect(terminal.options.macOptionClickForcesSelection).toBe(true);
    terminal.modes.mouseTrackingMode = "none";
    terminal.writeParsedListeners.forEach((listener) => listener());
    expect(terminal.options.macOptionClickForcesSelection).toBe(false);
    disposable.dispose();
  });
});

describe("attachTerminalRenderer", () => {
  beforeEach(() => {
    stubs.loaded = [];
    stubs.webglFails = false;
    stubs.disposed = 0;
    stubs.loseContext = null;
  });

  it("draws with WebGL, because a TUI repaints the whole panel", () => {
    const terminal = fakeTerminal();

    expect(attachTerminalRenderer(terminal as never)).toBe("webgl");
    expect(stubs.loaded).toEqual(["webgl"]);
  });

  it("keeps the DOM renderer when WebGL2 is not there", () => {
    stubs.webglFails = true;

    expect(attachTerminalRenderer(fakeTerminal() as never)).toBe("dom");
    expect(stubs.loaded).toEqual([]);
  });

  it("lets go of the renderer when the context is lost, so the terminal keeps drawing", () => {
    const terminal = fakeTerminal();
    attachTerminalRenderer(terminal as never);

    stubs.loseContext?.();

    expect(stubs.disposed).toBe(1);
  });
});

describe("watchTerminalRendererRecovery", () => {
  /** happy-dom will not change its own answer, so the window's state is put in by hand. */
  function visibility(state: "visible" | "hidden") {
    Object.defineProperty(document, "visibilityState", { value: state, configurable: true });
    document.dispatchEvent(new Event("visibilitychange"));
  }

  /** A watcher left listening outlives its test and answers the next one's window, so none does. */
  let watching: Array<{ dispose(): void }> = [];

  /**
   * A watcher on a terminal that is already drawing with WebGL, which is the state the panel puts
   * it in: the renderer goes on first and the level it answers decides whether it is watched.
   */
  function watch(terminal: FakeTerminal, level: "webgl" | "dom" = "webgl") {
    if (level === "webgl") attachTerminalRenderer(terminal as never);
    const disposable = watchTerminalRendererRecovery(terminal as never, level);
    watching.push(disposable);
    return disposable;
  }

  beforeEach(() => {
    stubs.loaded = [];
    stubs.webglFails = false;
    stubs.disposed = 0;
    stubs.loseContext = null;
    visibility("visible");
  });

  afterEach(() => {
    watching.forEach((disposable) => disposable.dispose());
    watching = [];
    vi.useRealTimers();
  });

  it("spends no attempt on a window that comes back with its renderer intact", () => {
    watch(fakeTerminal());

    // Three ordinary round trips: switching apps, a panel, a minimize. Nothing was lost, so
    // nothing is asked for, and the renderer that was working is left alone.
    for (let round = 0; round < MAX_RENDERER_RECOVERIES + 2; round++) {
      visibility("hidden");
      visibility("visible");
    }

    expect(stubs.loaded).toEqual(["webgl"]);
    // And not disposed either, which is the other half of leaving a working renderer alone.
    expect(stubs.disposed).toBe(0);
  });

  it("puts WebGL back once the window is back, and only for the loss that asked for it", () => {
    const terminal = fakeTerminal();
    watch(terminal);

    visibility("hidden");
    stubs.loseContext?.();
    // The addon that lost the context let go of itself the moment it did, so xterm.js' own
    // renderer is back on the element and the abandoned canvas is off it.
    expect(stubs.disposed).toBe(1);
    expect(stubs.loaded).toEqual(["webgl"]);

    visibility("visible");

    // A fresh addon rather than the one that let go: it is a disposable, and a disposed one comes
    // back with its renderer already gone, which would leave the terminal on the fallback for good.
    expect(stubs.loaded).toEqual(["webgl", "webgl"]);
    expect(stubs.disposed).toBe(1);

    // One recovery per loss, so the window coming back afterwards is not a second attempt.
    visibility("hidden");
    visibility("visible");
    expect(stubs.loaded).toEqual(["webgl", "webgl"]);
  });

  it("recovers a loss that happens while the window is already visible", () => {
    // The delay is the point of this one: a machine that refused a context a moment ago is the
    // reason it refused, so nothing is asked for in that same instant.
    vi.useFakeTimers();
    watch(fakeTerminal());

    stubs.loseContext?.();
    expect(stubs.loaded).toEqual(["webgl"]);
    // The old addon is gone either way, so the terminal is on the fallback and drawing.
    expect(stubs.disposed).toBe(1);

    vi.runAllTimers();

    // Without a single visibility change, because the window is not the thing that was lost and
    // nobody may switch away for the rest of the day.
    expect(stubs.loaded).toEqual(["webgl", "webgl"]);
    expect(stubs.disposed).toBe(1);
  });

  it("holds a pending recovery until the window is back rather than drawing into a hidden one", () => {
    vi.useFakeTimers();
    watch(fakeTerminal());

    stubs.loseContext?.();
    // Hidden before the delay is up: the terminal is off screen, and a context asked for now is
    // one the machine hands out while nothing of ours is on it.
    visibility("hidden");
    vi.runAllTimers();
    expect(stubs.loaded).toEqual(["webgl"]);

    // And it costs nothing to wait: the window coming back runs the same recovery, rather than
    // leaving the loss to be asked about again.
    visibility("visible");
    expect(stubs.loaded).toEqual(["webgl", "webgl"]);
  });

  it("never retries a terminal that fell back, because nothing was lost to recover from", () => {
    // No addon and no watcher: a terminal that never had WebGL has nothing to lose one, so there
    // is nothing to retry and the fallback it already draws with is left alone.
    watch(fakeTerminal(), "dom");

    visibility("hidden");
    visibility("visible");

    expect(stubs.loaded).toEqual([]);
  });

  it("stops after a bounded number of tries rather than asking for a context on every wake-up", () => {
    const terminal = fakeTerminal();
    watch(terminal);

    for (let attempt = 0; attempt < MAX_RENDERER_RECOVERIES + 5; attempt++) {
      stubs.loseContext?.();
      visibility("visible");
    }

    expect(stubs.loaded).toHaveLength(1 + MAX_RENDERER_RECOVERIES);

    // Past the limit the terminal keeps the renderer it has, which is xterm.js' own, and stops
    // asking: a machine that loses the context for the reason it is being asked to hand out
    // another one does not stop answering with a new one.
    stubs.loseContext?.();
    visibility("visible");
    expect(stubs.loaded).toHaveLength(1 + MAX_RENDERER_RECOVERIES);
  });

  it("stops at the fallback when the context cannot be had again, and stays stopped", () => {
    watch(fakeTerminal());
    stubs.webglFails = true;

    stubs.loseContext?.();
    visibility("visible");

    // One try, and then the terminal is left on the renderer that always works. Asking again
    // would be the loop.
    expect(stubs.loaded).toEqual(["webgl"]);
    stubs.webglFails = false;

    for (let attempt = 0; attempt < 5; attempt++) {
      stubs.loseContext?.();
      visibility("visible");
    }
    expect(stubs.loaded).toEqual(["webgl"]);
  });

  it("takes the listener and the pending recovery down with the pane", () => {
    vi.useFakeTimers();
    const terminal = fakeTerminal();
    const removed = vi.spyOn(document, "removeEventListener");
    const disposable = watch(terminal);

    stubs.loseContext?.();
    expect(vi.getTimerCount()).toBe(1);

    disposable.dispose();

    // Nothing is left to fire for a terminal that is no longer on the page.
    expect(vi.getTimerCount()).toBe(0);
    expect(removed).toHaveBeenCalledWith("visibilitychange", expect.any(Function));
    vi.runAllTimers();
    expect(stubs.loaded).toEqual(["webgl"]);

    // And a window change on a terminal nobody is watching answers nobody.
    visibility("hidden");
    stubs.loseContext?.();
    visibility("visible");
    expect(stubs.loaded).toEqual(["webgl"]);
    removed.mockRestore();
  });
});

describe("preloadTerminalFonts", () => {
  // The kept promise is module state on purpose, so each of these needs the module fresh or the
  // first test's answer would be handed to the second. happy-dom has no font loading API either,
  // so `document.fonts` stands in for it.
  async function freshPreload(load: (font: string) => Promise<unknown>) {
    vi.resetModules();
    stubs.fontLoads = [];
    Object.defineProperty(document, "fonts", {
      value: {
        load: vi.fn((font: string, text?: string) => {
          stubs.fontLoads.push([font, text]);
          return load(font);
        }),
      },
      configurable: true,
    });
    const { preloadTerminalFonts } = await import("./muster-terminal");
    return preloadTerminalFonts();
  }

  // The whole point of preloading is that the second caller pays nothing, which only holds if
  // the answer is kept. Without this, every panel re-parses 1.1MB of woff2 on its own mount.
  it("asks for every bundled face once and hands every later caller the same promise", async () => {
    await freshPreload(() => Promise.resolve([]));
    const { preloadTerminalFonts } = await import("./muster-terminal");
    const first = preloadTerminalFonts();
    const second = preloadTerminalFonts();

    expect(second).toBe(first);
    // The icon face is asked for by name and with a character it has, because a family list is
    // where to look rather than a request, and a face holding only icons covers no space.
    expect(stubs.fontLoads).toEqual([
      ['16px "Muster Nerd Mono", "Muster Nerd Icons", monospace', undefined],
      ['700 16px "Muster Nerd Mono", "Muster Nerd Icons", monospace', undefined],
      ['16px "Muster Nerd Icons"', "\uE0B0"],
    ]);
    await expect(first).resolves.toHaveLength(3);
  });

  it("resolves rather than rejects when a face is missing, so a panel still opens", async () => {
    await expect(freshPreload(() => Promise.reject(new Error("no such face")))).resolves.toHaveLength(3);
  });
});
