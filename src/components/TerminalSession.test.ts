// @vitest-environment happy-dom
import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Checkout, TerminalSessionStatus } from "../domain/workspace";
import { DEFAULT_SETTINGS } from "../domain/settings";
import {
  closeTerminal,
  createTerminal,
  getTerminalStatus,
  launchPiReviewTerminal,
  loadTerminalLayout,
  moveTerminal,
  resizeTerminal,
  saveTerminalLayout,
  writeTerminal,
} from "../lib/ipc";
import type { TerminalLaunch } from "../lib/ipc";
import { useToasts } from "../presentation/toasts";
import { theme } from "../presentation/theme";
import { PTY_OUTPUT_REPORT_STEP_BYTES } from "../lib/terminal-renderer";
import SessionPane from "./SessionPane.vue";
import TerminalSession from "./TerminalSession.vue";

const { MockTerminal, terminalMock } = vi.hoisted(() => {
  const terminalMock = {
    channel: null as { onmessage: (buffer: ArrayBuffer) => void } | null,
    input: null as ((value: string) => void) | null,
    resizes: [] as Array<(size: { cols: number; rows: number }) => void>,
    titles: [] as Array<(title: string) => void>,
    scrolls: [] as Array<() => void>,
    customKeyHandlers: [] as Array<(event: KeyboardEvent) => boolean>,
    /** What the shell's OSC 133 hook said, by ident: the parser hands each marker to the handler. */
    oscHandlers: new Map<number, (data: string) => boolean>(),
    output: [] as number[][],
    /**
     * The callbacks xterm owes the writer, one per write handed over and not parsed yet. A test
     * that releases them is a renderer catching up; a test that does not is one that cannot.
     */
    writeCallbacks: [] as Array<() => void>,
    openCalls: 0,
    focusCalls: 0,
    clearTextureAtlasCalls: 0,
    screenWidth: 0,
    /** The scrollback the mock terminal reports, and the only way a test grows or scrolls it. */
    buffer: { length: 24, viewportY: 0 },
    /** What the component asked xterm to do, in the order it asked. */
    scrollToLines: [] as number[],
    scrollLineCounts: [] as number[],
    events: [] as string[],
    fontLoads: [] as string[],
    fontLoadPromise: null as Promise<FontFace[]> | null,
    /** The size and the scale the terminal was built at, which is the size of its cell from its first frame. */
    builtAt: { fontSize: 16, cursorBlink: true, cursorStyle: "block", zoom: 1 } as {
      fontSize: number;
      cursorBlink: boolean;
      cursorStyle: string;
      zoom: number;
    },
    terminal: null as MockTerminal | null,
  };
  class MockTerminal {
    cols = 80;
    rows = 24;
    options: Record<string, unknown> = {};
    element?: HTMLElement;
    constructor() {
      terminalMock.terminal = this;
    }
    loadAddon() {}
    open(element: HTMLElement) {
      this.element = element;
      terminalMock.events.push("open");
      if (terminalMock.screenWidth) element.appendChild(document.createElement("div")).className = "xterm-screen";
      terminalMock.openCalls += 1;
    }
    resize(cols: number, rows: number) {
      this.cols = cols;
      this.rows = rows;
      terminalMock.resizes.forEach((callback) => callback({ cols, rows }));
    }
    onData(callback: (value: string) => void) {
      terminalMock.input = callback;
    }
    attachCustomKeyEventHandler(handler: (event: KeyboardEvent) => boolean) {
      terminalMock.customKeyHandlers.push(handler);
    }
    onResize(callback: (size: { cols: number; rows: number }) => void) {
      terminalMock.resizes.push(callback);
    }
    onTitleChange(callback: (title: string) => void) {
      terminalMock.titles.push(callback);
      return { dispose: () => {} };
    }
    onScroll(callback: () => void) {
      terminalMock.scrolls.push(callback);
    }
    /** xterm moves the viewport and only then tells anyone, which is what the thumb has to follow. */
    scrollToLine(line: number) {
      terminalMock.scrollToLines.push(line);
      terminalMock.buffer = { ...terminalMock.buffer, viewportY: Math.max(0, line) };
      terminalMock.scrolls.forEach((callback) => callback());
    }
    scrollLines(amount: number) {
      terminalMock.scrollLineCounts.push(amount);
      const viewportY = Math.min(
        Math.max(0, terminalMock.buffer.viewportY + amount),
        Math.max(0, terminalMock.buffer.length - this.rows),
      );
      terminalMock.buffer = { ...terminalMock.buffer, viewportY };
      terminalMock.scrolls.forEach((callback) => callback());
    }
    get buffer() {
      return { active: { ...terminalMock.buffer, baseY: 0, rows: this.rows } };
    }
    write(data: Uint8Array, callback?: () => void) {
      terminalMock.output.push(Array.from(data));
      if (callback) terminalMock.writeCallbacks.push(callback);
    }
    focus() {
      terminalMock.focusCalls += 1;
    }
    clearTextureAtlas() {
      terminalMock.clearTextureAtlasCalls += 1;
    }
    registerLinkProvider() {
      return { dispose: () => {} };
    }
    /** The OSC handlers the shell-integration module registers, so a test can deliver a marker. */
    parser = {
      registerOscHandler: (ident: number, callback: (data: string) => boolean) => {
        terminalMock.oscHandlers.set(ident, callback);
        return {
          dispose: () => {
            terminalMock.oscHandlers.delete(ident);
          },
        };
      },
    };
    registerMarker() {
      return { dispose: () => {} };
    }
    registerDecoration() {
      return { dispose: () => {} };
    }
    dispose() {}
  }
  return { MockTerminal, terminalMock };
});

vi.mock("@tauri-apps/api/core", () => ({
  Channel: class MockChannel {
    onmessage: (buffer: ArrayBuffer) => void = () => {};

    constructor() {
      terminalMock.channel = this;
    }
  },
}));

// The terminal's face, palette, addons, ligatures and renderer are the lib's business and are
// asserted there. What this file owns is the wiring: a terminal on the page, with both.
const terminalLib = vi.hoisted(() => ({
  attachTerminalRenderer: vi.fn(),
  watchTerminalRendererRecovery: vi.fn(() => ({ dispose: vi.fn() })),
  setTerminalLigatures: vi.fn(),
  // Takes the copy callback so the `selectionCopy` gate can be exercised through the same callback
  // a real selection gesture would call, rather than by re-implementing the gesture here. The
  // callback is named for the type only; the test reads it back off `mock.calls`.
  enableTerminalSelectionCopy: vi.fn((_terminal: unknown, copy: (text: string) => void) => {
    void copy;
    return { dispose: vi.fn() };
  }),
  fitCalls: 0,
  /** Stand-in for the palette the lib reads out of the stylesheet, so a switch is visible here. */
  theme: { background: "#282c33" } as Record<string, string>,
}));

// happy-dom has no font loading API. Record the requests so this test can pin that xterm opens only
// after both bundled faces are ready, rather than painting its first atlas from the fallback.
Object.defineProperty(document, "fonts", {
  value: {
    ready: Promise.resolve(),
    load: vi.fn((font: string) => {
      terminalMock.events.push(`font:${font}`);
      terminalMock.fontLoads.push(font);
      return terminalMock.fontLoadPromise ?? Promise.resolve([]);
    }),
  },
  configurable: true,
});

// And no layout either, so every element measures zero and the fit that sizes the grid would never
// run. 80x24 is what a mock terminal of this size already reports, so the dimensions are the ones
// the rest of this file already asserts against.
for (const [property, value] of [
  ["clientWidth", 640],
  ["clientHeight", 480],
] as const) {
  Object.defineProperty(HTMLElement.prototype, property, { value, configurable: true });
}

// The real preload is memoized, so it would only ever load the faces once for the whole file and
// the ordering below would be untestable. Asking for them per mount keeps the guarantee this file
// exists to pin: xterm opens only after both bundled weights are ready.
vi.mock("../lib/muster-terminal", () => ({
  createMusterTerminal: (fontSize: number, cursorBlink: boolean, cursorStyle: string, zoom: number) => {
    terminalMock.builtAt = { fontSize, cursorBlink, cursorStyle, zoom };
    return new MockTerminal();
  },
  terminalFontSize: (fontSize: number, zoom: number) => fontSize * zoom,
  musterTerminalTheme: () => terminalLib.theme,
  setTerminalLigatures: terminalLib.setTerminalLigatures,
  enableTerminalSelectionCopy: terminalLib.enableTerminalSelectionCopy,
  attachTerminalRenderer: terminalLib.attachTerminalRenderer,
  watchTerminalRendererRecovery: terminalLib.watchTerminalRendererRecovery,
  preloadTerminalFonts: () =>
    Promise.allSettled([
      document.fonts.load('16px "Muster Nerd Mono", "Muster Nerd Icons", monospace'),
      document.fonts.load('700 16px "Muster Nerd Mono", "Muster Nerd Icons", monospace'),
      document.fonts.load('16px "Muster Nerd Icons"', "\uE0B0"),
    ]),
}));

vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class MockFitAddon {
    fit() {
      terminalLib.fitCalls += 1;
      if (terminalMock.screenWidth) terminalMock.terminal!.cols = 77;
    }
  },
}));

vi.mock("../lib/ipc", () => ({
  closeTerminal: vi.fn(),
  createTerminal: vi.fn(),
  getTerminalStatus: vi.fn(),
  launchPiReviewTerminal: vi.fn(),
  loadTerminalLayout: vi.fn(),
  moveTerminal: vi.fn(),
  resizeTerminal: vi.fn(),
  saveTerminalLayout: vi.fn(),
  writeTerminal: vi.fn(),
}));

/** The window's only answer to the reader, which is a Tauri event. */
const appEvents = vi.hoisted(() => ({ emit: vi.fn(async () => {}) }));

vi.mock("@tauri-apps/api/event", () => ({ emit: appEvents.emit }));

/**
 * The file-link registration, recorded rather than performed: what matters to a terminal is the
 * directory its links are looked up in, and the provider is the only thing that turns one into a
 * path the backend can resolve.
 */
const fileLinks = vi.hoisted(() => ({
  registrations: [] as Array<{
    options: {
      checkoutId: string;
      workingDirectory?: string;
      refreshWorkingDirectory?: (checkoutId: string) => Promise<string | null>;
    };
    dispose: ReturnType<typeof vi.fn>;
  }>,
}));

vi.mock("../lib/terminal-file-links", () => ({
  registerFilePathLinks: (_terminal: unknown, options: { checkoutId: string; workingDirectory?: string }) => {
    const registration = { options, dispose: vi.fn() };
    fileLinks.registrations.push(registration);
    return registration;
  },
}));

// The clipboard is the one thing a selection gesture reaches outside the terminal, so it is the
// only thing the `selectionCopy` preference is allowed to turn on and off. Mocked here rather than
// in the lib, which is where the gesture itself lives.
const clipboard = vi.hoisted(() => ({ writes: [] as string[] }));
vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({
  writeText: (text: string) => {
    clipboard.writes.push(text);
    return Promise.resolve();
  },
}));

const workspace = { repos: [], activeCheckoutId: "checkout:repo", activeSessionId: "session:new" };
const created = {
  session: {
    id: "session:new",
    type: "shell" as const,
    checkoutId: "checkout:repo",
    name: "zsh",
    createdAt: "now",
    status: "inactive" as const,
  },
  workspace,
};

/**
 * Moves the mock's scrollback the way xterm does: the buffer first, then the event, so whatever is
 * listening reads a position that has already moved. The line count is the one thing the tests
 * below depend on being exact — 120 lines of scrollback under 24 rows leaves 96 lines of travel
 * either side of a 480px track, and every number in them is a whole number as a result.
 */
function scrollbackTo(viewportY: number, length = 120) {
  terminalMock.buffer = { length, viewportY };
  terminalMock.scrolls.forEach((callback) => callback());
}

describe("TerminalSession UI", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    clipboard.writes.length = 0;
    terminalMock.channel = null;
    terminalMock.input = null;
    terminalMock.resizes = [];
    terminalMock.titles = [];
    terminalMock.scrolls = [];
    terminalMock.customKeyHandlers = [];
    terminalMock.oscHandlers.clear();
    terminalMock.output = [];
    terminalMock.writeCallbacks = [];
    terminalMock.openCalls = 0;
    terminalMock.focusCalls = 0;
    terminalMock.clearTextureAtlasCalls = 0;
    terminalMock.screenWidth = 0;
    terminalMock.buffer = { length: 24, viewportY: 0 };
    terminalMock.scrollToLines = [];
    terminalMock.scrollLineCounts = [];
    terminalMock.events = [];
    terminalMock.fontLoads = [];
    terminalMock.fontLoadPromise = null;
    terminalMock.builtAt = { fontSize: 16, cursorBlink: true, cursorStyle: "block", zoom: 1 };
    terminalMock.terminal = null;
    fileLinks.registrations.length = 0;
    terminalLib.fitCalls = 0;
    vi.mocked(createTerminal).mockResolvedValue(created);
    vi.mocked(getTerminalStatus).mockResolvedValue({ state: "running" });
    vi.mocked(launchPiReviewTerminal).mockReset();
    vi.mocked(closeTerminal).mockResolvedValue(workspace);
    // `clearAllMocks` empties the calls but keeps an implementation, so a test that leaves this one
    // refusing every size would refuse them for the rest of the file too.
    vi.mocked(resizeTerminal).mockReset();
    vi.mocked(writeTerminal).mockReset();
  });

  it("opens the requested Pi review in a new inactive terminal", async () => {
    const request = { ids: ["note:one"], markdown: "# Review" };
    const launch: TerminalLaunch = { kind: "pi-review", request };
    vi.mocked(launchPiReviewTerminal).mockResolvedValue(created);
    const wrapper = mount(TerminalSession, {
      props: { checkoutId: "checkout:repo", active: false, launch },
    });
    await flushPromises();

    expect(launchPiReviewTerminal).toHaveBeenCalledWith(request, "checkout:repo", 80, 24, expect.anything());
    expect(createTerminal).not.toHaveBeenCalled();
    expect(wrapper.emitted("created")).toHaveLength(1);
    wrapper.unmount();
  });

  it("keeps channel output binary and sends Unicode input and fitted dimensions to the session", async () => {
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    const output = Uint8Array.of(0, 0xc3, 0xa9, 0x1b, 0x5b, 0x33, 0x31, 0x6d);
    terminalMock.channel?.onmessage(output.buffer as ArrayBuffer);
    terminalMock.input?.("λ pasted");
    terminalMock.resizes[0]?.({ cols: 97, rows: 31 });
    await flushPromises();

    expect(terminalMock.output).toEqual([Array.from(output)]);
    expect(createTerminal).toHaveBeenCalledWith("checkout:repo", 80, 24, expect.anything());
    // Both are refused by xterm.js until the terminal is on the page.
    expect(terminalMock.openCalls).toBe(1);
    expect(terminalLib.setTerminalLigatures).toHaveBeenCalledWith(expect.anything(), true);
    expect(terminalLib.enableTerminalSelectionCopy).toHaveBeenCalledTimes(1);
    expect(terminalLib.attachTerminalRenderer).toHaveBeenCalledTimes(1);
    expect(writeTerminal).toHaveBeenCalledWith("checkout:repo", "session:new", new TextEncoder().encode("λ pasted"));
    expect(resizeTerminal).toHaveBeenCalledWith("checkout:repo", "session:new", 97, 31);
    wrapper.unmount();
  });

  it("copies a selection only while the setting says to, and reads it on the click", async () => {
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    // The callback the lib was handed, which is what a selection gesture ends up calling.
    const copy = vi.mocked(terminalLib.enableTerminalSelectionCopy).mock.calls[0]![1]!;
    copy("selected while on");
    await flushPromises();
    expect(clipboard.writes).toEqual(["selected while on"]);

    // The setting is read on the click and not at registration, so it reaches a terminal that was
    // already open: the same callback, a prop change, no new registration.
    await wrapper.setProps({ selectionCopy: false });
    expect(terminalLib.enableTerminalSelectionCopy).toHaveBeenCalledTimes(1);
    copy("selected while off");
    await flushPromises();
    // Nothing new reached the clipboard, and the selection itself was never the component's to take
    // away — only the copy of it is gated.
    expect(clipboard.writes).toEqual(["selected while on"]);

    // And back on, so the setting is a switch rather than a one-way door.
    await wrapper.setProps({ selectionCopy: true });
    copy("selected again");
    await flushPromises();
    expect(clipboard.writes).toEqual(["selected while on", "selected again"]);
    wrapper.unmount();
  });

  it("answers Shift+Enter in the encoding the program announced, and drops no modifier", async () => {
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    const handler = terminalMock.customKeyHandlers[0]!;
    // A program that never said anything gets the line ending, which is what it reads as Enter.
    expect(handler(new KeyboardEvent("keydown", { key: "Enter", shiftKey: true }))).toBe(false);
    await flushPromises();
    expect(writeTerminal).toHaveBeenLastCalledWith("checkout:repo", "session:new", new TextEncoder().encode("\n"));

    // The other modifiers are other shortcuts, and this leaves them to xterm rather than answer a
    // Ctrl+Shift+Enter as a Shift+Enter.
    for (const modifier of [{ ctrlKey: true }, { altKey: true }, { metaKey: true }]) {
      expect(handler(new KeyboardEvent("keydown", { key: "Enter", shiftKey: true, ...modifier }))).toBe(true);
    }
    // The character event xterm looks at a second time, and the release, neither of which may send
    // anything: xterm's own carriage return beside ours is a message sent and a line broken at once.
    expect(handler(new KeyboardEvent("keypress", { key: "Enter", shiftKey: true }))).toBe(false);
    expect(handler(new KeyboardEvent("keyup", { key: "Enter", shiftKey: true }))).toBe(true);
    // A bare Return is xterm's, and it still arrives.
    expect(handler(new KeyboardEvent("keydown", { key: "Enter" }))).toBe(true);
    terminalMock.input?.("\r");

    // A program that pushed the flags gets the key with its modifier attached.
    terminalMock.channel?.onmessage(new TextEncoder().encode("\u001b[>1u").buffer as ArrayBuffer);
    expect(handler(new KeyboardEvent("keydown", { key: "Enter", shiftKey: true }))).toBe(false);
    await flushPromises();

    expect(writeTerminal).toHaveBeenCalledTimes(3);
    expect(writeTerminal).toHaveBeenNthCalledWith(1, "checkout:repo", "session:new", new TextEncoder().encode("\n"));
    expect(writeTerminal).toHaveBeenNthCalledWith(2, "checkout:repo", "session:new", new TextEncoder().encode("\r"));
    expect(writeTerminal).toHaveBeenLastCalledWith(
      "checkout:repo",
      "session:new",
      new TextEncoder().encode("\u001b[13;2u"),
    );
    wrapper.unmount();
  });

  it("rejects terminal input above the byte budget with a visible error", async () => {
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    terminalMock.input?.("x".repeat(1024 * 1024 + 1));
    await flushPromises();

    expect(writeTerminal).not.toHaveBeenCalled();
    expect(wrapper.get('[role="alert"]').text()).toContain("1 MiB limit");
    wrapper.unmount();
  });

  it("bounds queued terminal input by request count", async () => {
    let finishFirstWrite!: () => void;
    vi.mocked(writeTerminal).mockImplementationOnce(() => new Promise<void>((resolve) => (finishFirstWrite = resolve)));
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    for (let request = 0; request < 129; request += 1) terminalMock.input?.("x");
    await flushPromises();
    expect(writeTerminal).toHaveBeenCalledOnce();
    expect(wrapper.get('[role="alert"]').text()).toContain("queue is full");

    finishFirstWrite();
    await flushPromises();
    expect(writeTerminal).toHaveBeenCalledTimes(128);
    wrapper.unmount();
  });

  it("publishes sanitized OSC terminal title changes", async () => {
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    terminalMock.titles[0]?.(" OpenCode: review task\u001b[31m ");
    expect(wrapper.emitted("statusChanged")?.at(-1)?.[0]).toMatchObject({
      state: "running",
      terminalTitle: "OpenCode: review task[31m",
    });

    terminalMock.titles[0]?.("\u001b[0m");
    expect(wrapper.emitted("statusChanged")?.at(-1)?.[0]).toMatchObject({ terminalTitle: "[0m" });
    terminalMock.titles[0]?.("  ");
    expect(wrapper.emitted("statusChanged")?.at(-1)?.[0]).toMatchObject({ terminalTitle: null });
    wrapper.unmount();
  });

  it("hands the link provider the directory the shell is in, and re-registers it on a cd", async () => {
    // A listing prints bare names, so what a name means depends on where the shell is sitting: the
    // provider has to be told, and it has to be told again when the shell moves, because a
    // `LICENSE` underlined under the directory it left is a name that opens the wrong file.
    vi.useFakeTimers();
    vi.mocked(getTerminalStatus).mockResolvedValue({
      state: "running",
      workingDirectory: "/work/feature/src/lib",
    });
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    const first = fileLinks.registrations.at(-1)!;
    expect(first.options.workingDirectory).toBe("/work/feature/src/lib");
    expect(first.options.checkoutId).toBe("checkout:repo");
    vi.mocked(getTerminalStatus).mockResolvedValueOnce({
      state: "running",
      workingDirectory: "/work/feature/child",
    });
    await expect(first.options.refreshWorkingDirectory?.("checkout:repo")).resolves.toBe("/work/feature/child");
    expect(getTerminalStatus).toHaveBeenLastCalledWith("checkout:repo", "session:new");
    vi.mocked(getTerminalStatus).mockResolvedValueOnce({ state: "exited" });
    await expect(first.options.refreshWorkingDirectory?.("checkout:repo")).resolves.toBeNull();

    vi.mocked(getTerminalStatus).mockResolvedValue({
      state: "running",
      workingDirectory: "/work/feature/docs",
    });
    await vi.advanceTimersByTimeAsync(750);
    await flushPromises();

    const second = fileLinks.registrations.at(-1)!;
    expect(fileLinks.registrations.length).toBeGreaterThan(1);
    expect(second.options.workingDirectory).toBe("/work/feature/docs");
    // The old provider is taken back rather than left asking the same question twice.
    expect(first.dispose).toHaveBeenCalled();
    second.dispose.mockClear();
    wrapper.unmount();
    expect(second.dispose).toHaveBeenCalled();
    vi.useRealTimers();
  });

  it("publishes the shell's last command exit code, and keeps it across the status poll", async () => {
    vi.useFakeTimers();
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    // A terminal with shell integration turned off never sees an OSC marker, and nothing invents the
    // field for it: the status is the backend's answer with the frontend's own two additions, and a
    // failure the backend cannot see has to arrive from the shell or not at all.
    await vi.advanceTimersByTimeAsync(750);
    const polled = wrapper.emitted("statusChanged")?.at(-1)?.[0] as Record<string, unknown>;
    expect(polled.lastCommandExit).toBeUndefined();

    // The backend cannot supply this: a command that fails does not end the shell, so the answer
    // arrives from the OSC 133 hook the session was started with.
    terminalMock.oscHandlers.get(133)?.("D;1");
    expect(wrapper.emitted("statusChanged")?.at(-1)?.[0]).toMatchObject({
      state: "running",
      lastCommandExit: 1,
    });

    // A command that succeeds is not a failure, and the field is published either way so the sidebar
    // has something to stop treating the session as red.
    terminalMock.oscHandlers.get(133)?.("D;0");
    expect(wrapper.emitted("statusChanged")?.at(-1)?.[0]).toMatchObject({ lastCommandExit: 0 });

    // The poll replaces the whole status object every 750ms with an answer that carries no exit code
    // at all, so without the re-merge in `updateStatus` this value would be gone by the next tick and
    // the sidebar bar would flicker blue between polls.
    terminalMock.oscHandlers.get(133)?.("D;2");
    await vi.advanceTimersByTimeAsync(1_500);
    expect(getTerminalStatus).toHaveBeenCalled();
    expect(wrapper.emitted("statusChanged")?.at(-1)?.[0]).toMatchObject({ lastCommandExit: 2 });

    // A failing command emits its exit code, and then the shell goes on to print the next prompt.
    // The row has to still be red once that prompt is on screen: the prompt arrives immediately
    // after the command, every single time, so clearing on it would mean the row is never red.
    terminalMock.oscHandlers.get(133)?.("D;1");
    terminalMock.oscHandlers.get(133)?.("B");
    const afterPrompt = wrapper.emitted("statusChanged")?.at(-1)?.[0] as { lastCommandExit?: number };
    expect(afterPrompt.lastCommandExit).toBe(1);

    // A command starting clears it, and the clear survives the poll too rather than coming back.
    terminalMock.oscHandlers.get(133)?.("C");
    await vi.advanceTimersByTimeAsync(750);
    const cleared = wrapper.emitted("statusChanged")?.at(-1)?.[0] as { lastCommandExit?: number };
    expect(cleared.lastCommandExit).toBeUndefined();
    wrapper.unmount();
    vi.useRealTimers();
  });

  it("answers Pi OSC queries before confirming task state with terminal identity", async () => {
    let resolveStatus!: (status: TerminalSessionStatus) => void;
    vi.mocked(getTerminalStatus).mockReturnValueOnce(new Promise((resolve) => (resolveStatus = resolve)));
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    const osc = terminalMock.oscHandlers.get(7501);
    expect(osc).toBeDefined();
    osc?.("?");
    await flushPromises();
    osc?.("state=working:app=pi");
    expect(writeTerminal).toHaveBeenCalledTimes(1);
    expect(wrapper.emitted("statusChanged")).toBeUndefined();
    const queryReply = vi.mocked(writeTerminal).mock.calls[0]?.[2];
    expect(queryReply).toBeDefined();
    expect(new TextDecoder().decode(queryReply!)).toBe("\u001b]7501;?\u001b\\");

    resolveStatus({ state: "running", foregroundProcess: true, foregroundApp: "pi" });
    await flushPromises();
    expect(wrapper.emitted("statusChanged")?.at(-1)?.[0]).toMatchObject({ piTaskState: "working" });
    wrapper.unmount();
    expect(terminalMock.oscHandlers.has(7501)).toBe(false);
  });

  it("queues initial direct-Pi OSC queries until the new terminal is named", async () => {
    let resolveCreate!: (result: typeof created) => void;
    vi.mocked(createTerminal).mockReturnValueOnce(new Promise((resolve) => (resolveCreate = resolve)));
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    const osc = terminalMock.oscHandlers.get(7501);
    expect(osc?.("?")).toBe(true);
    expect(writeTerminal).not.toHaveBeenCalled();

    resolveCreate(created);
    await flushPromises();
    expect(writeTerminal).toHaveBeenCalledTimes(1);
    const queryReply = vi.mocked(writeTerminal).mock.calls[0]?.[2];
    expect(queryReply).toBeDefined();
    expect(new TextDecoder().decode(queryReply!)).toBe("\u001b]7501;?\u001b\\");
    wrapper.unmount();
  });

  it("queues Pi OSC query replies until the newly launched terminal is named", async () => {
    let resolveLaunch!: (result: typeof created) => void;
    vi.mocked(launchPiReviewTerminal).mockReturnValueOnce(new Promise((resolve) => (resolveLaunch = resolve)));
    const launch: TerminalLaunch = { kind: "pi-review", request: { markdown: "# Review" } };
    const wrapper = mount(TerminalSession, {
      props: { checkoutId: "checkout:repo", active: false, launch },
    });
    await flushPromises();

    const osc = terminalMock.oscHandlers.get(7501);
    expect(osc?.("?")).toBe(true);
    expect(writeTerminal).not.toHaveBeenCalled();

    resolveLaunch(created);
    await flushPromises();
    expect(writeTerminal).toHaveBeenCalledTimes(1);
    const queryReply = vi.mocked(writeTerminal).mock.calls[0]?.[2];
    expect(queryReply).toBeDefined();
    expect(new TextDecoder().decode(queryReply!)).toBe("\u001b]7501;?\u001b\\");
    wrapper.unmount();
  });

  it("activates manually started Pi after an idle-shell poll without OSC 133 hooks", async () => {
    let resolvePiStatus!: (status: TerminalSessionStatus) => void;
    vi.mocked(getTerminalStatus)
      .mockResolvedValueOnce({ state: "running", foregroundProcess: true, foregroundApp: "zsh" })
      .mockReturnValueOnce(new Promise((resolve) => (resolvePiStatus = resolve)));
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();
    expect(getTerminalStatus).toHaveBeenCalledTimes(1);

    const osc = terminalMock.oscHandlers.get(7501);
    expect(osc?.("?")).toBe(true);
    await flushPromises();
    const queryReply = vi.mocked(writeTerminal).mock.calls[0]?.[2];
    expect(queryReply).toBeDefined();
    expect(new TextDecoder().decode(queryReply!)).toBe("\u001b]7501;?\u001b\\");
    expect(osc?.("state=working:app=opencode")).toBe(false);
    expect(osc?.("state=blocked:kind=unknown:app=pi")).toBe(false);
    expect(getTerminalStatus).toHaveBeenCalledTimes(1);

    expect(osc?.("state=working:app=pi")).toBe(true);
    await flushPromises();
    expect(getTerminalStatus).toHaveBeenCalledTimes(2);
    expect(wrapper.emitted("statusChanged")?.at(-1)?.[0]).not.toHaveProperty("piTaskState");

    resolvePiStatus({ state: "running", foregroundProcess: true, foregroundApp: "pi" });
    await flushPromises();
    const statuses = (wrapper.emitted("statusChanged") ?? []).map(([status]) => status as TerminalSessionStatus);
    expect(statuses.at(-1)).toMatchObject({ foregroundApp: "pi", piTaskState: "working" });
    expect(statuses.filter((status) => status.piTaskState).every((status) => status.foregroundApp === "pi")).toBe(true);
    wrapper.unmount();
  });

  it("uses OSC 9;4 as Pi activity and clears to neutral rather than complete", async () => {
    vi.mocked(getTerminalStatus).mockResolvedValue({
      state: "running",
      foregroundProcess: true,
      foregroundApp: "pi",
    });
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    const progress = terminalMock.oscHandlers.get(9);
    expect(progress?.("4;3")).toBe(true);
    expect(wrapper.emitted("statusChanged")?.at(-1)?.[0]).toMatchObject({ piTaskState: "working" });
    expect(progress?.("4;0")).toBe(true);
    const cleared = wrapper.emitted("statusChanged")?.at(-1)?.[0];
    expect(cleared).not.toHaveProperty("piTaskState");
    expect(cleared).not.toHaveProperty("done");
    wrapper.unmount();
  });

  it("keeps native Pi states authoritative over OSC 9;4 progress", async () => {
    vi.mocked(getTerminalStatus).mockResolvedValue({
      state: "running",
      foregroundProcess: true,
      foregroundApp: "pi",
    });
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    const status = terminalMock.oscHandlers.get(7501);
    const progress = terminalMock.oscHandlers.get(9);
    status?.("state=working:app=pi");
    progress?.("4;0");
    expect(wrapper.emitted("statusChanged")?.at(-1)?.[0]).toMatchObject({ piTaskState: "working" });
    status?.("state=done:app=pi");
    progress?.("4;3");
    expect(wrapper.emitted("statusChanged")?.at(-1)?.[0]).toMatchObject({ piTaskState: "done" });
    status?.("state=error:app=pi");
    progress?.("4;3");
    expect(wrapper.emitted("statusChanged")?.at(-1)?.[0]).toMatchObject({ piTaskState: "error" });
    wrapper.unmount();
  });

  it("holds OSC 9;4 activity until a fresh poll confirms foreground Pi", async () => {
    let resolveStatus!: (status: TerminalSessionStatus) => void;
    vi.mocked(getTerminalStatus)
      .mockReturnValueOnce(new Promise((resolve) => (resolveStatus = resolve)))
      .mockResolvedValue({ state: "running", foregroundProcess: true, foregroundApp: "pi" });
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();
    expect(getTerminalStatus).toHaveBeenCalledTimes(1);

    terminalMock.oscHandlers.get(9)?.("4;3");
    expect(wrapper.emitted("statusChanged")).toBeUndefined();
    resolveStatus({ state: "running", foregroundProcess: true, foregroundApp: "pi" });
    await flushPromises();
    expect(wrapper.emitted("statusChanged")?.at(-1)?.[0]).toMatchObject({
      foregroundApp: "pi",
      piTaskState: "working",
    });
    wrapper.unmount();
  });

  it("rejects OSC 9;4 activity while another program is foreground", async () => {
    vi.mocked(getTerminalStatus).mockResolvedValue({
      state: "running",
      foregroundProcess: true,
      foregroundApp: "zsh",
    });
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    terminalMock.oscHandlers.get(9)?.("4;3");
    await flushPromises();
    const statuses = (wrapper.emitted("statusChanged") ?? []).map(([status]) => status as TerminalSessionStatus);
    expect(statuses.every((status) => status.piTaskState === undefined)).toBe(true);
    wrapper.unmount();
  });

  it("accepts native Pi reports in hidden panes only while Pi is foreground", async () => {
    vi.mocked(getTerminalStatus).mockResolvedValue({
      state: "running",
      foregroundProcess: true,
      foregroundApp: "pi",
    });
    const wrapper = mount(TerminalSession, {
      props: { checkoutId: "checkout:repo", active: true, visible: false },
    });
    await flushPromises();

    terminalMock.oscHandlers.get(7501)?.("state=working:app=pi");
    expect(wrapper.emitted("statusChanged")?.at(-1)?.[0]).toMatchObject({ piTaskState: "working" });
    wrapper.unmount();
  });

  it("rejects a Pi report when the OS has not identified Pi as the foreground process", async () => {
    vi.mocked(getTerminalStatus).mockResolvedValue({
      state: "running",
      foregroundProcess: false,
      foregroundApp: "pi",
    });
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    terminalMock.oscHandlers.get(7501)?.("state=working:app=pi");
    await flushPromises();
    const statuses = (wrapper.emitted("statusChanged") ?? []).map(([status]) => status as TerminalSessionStatus);
    expect(statuses.every((status) => status.piTaskState === undefined)).toBe(true);
    wrapper.unmount();
  });

  it("keeps Pi diagnostics bounded and omits terminal message contents", async () => {
    const debug = vi.spyOn(console, "debug").mockImplementation(() => {});
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    try {
      await flushPromises();

      const osc = terminalMock.oscHandlers.get(7501);
      const progress = terminalMock.oscHandlers.get(9);
      osc?.("?");
      osc?.("state=working:app=pi:msg=c2VjcmV0");
      osc?.("state=working:app=pi:msg=c2VjcmV0");
      progress?.("4;3");
      progress?.("4;3");
      progress?.("4;0");
      progress?.("4;0");
      await flushPromises();

      expect(debug.mock.calls.filter(([, event]) => event === "query-received")).toHaveLength(1);
      expect(debug.mock.calls.filter(([, event]) => event === "reply-queued")).toHaveLength(1);
      expect(debug.mock.calls.filter(([, event]) => event === "reply-write-succeeded")).toHaveLength(1);
      expect(debug.mock.calls.filter(([, event]) => event === "status-report")).toHaveLength(1);
      expect(debug.mock.calls.filter(([, event]) => event === "progress-report")).toHaveLength(2);
      expect(JSON.stringify(debug.mock.calls)).not.toContain("c2VjcmV0");
      expect(JSON.stringify(debug.mock.calls)).not.toContain("secret");
    } finally {
      wrapper.unmount();
      debug.mockRestore();
    }
  });

  it("diagnoses failed Pi status reply writes without logging the error text", async () => {
    const debug = vi.spyOn(console, "debug").mockImplementation(() => {});
    vi.mocked(writeTerminal).mockRejectedValueOnce(new Error("private terminal error"));
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    try {
      await flushPromises();

      terminalMock.oscHandlers.get(7501)?.("?");
      await flushPromises();

      expect(debug.mock.calls.filter(([, event]) => event === "reply-queued")).toHaveLength(1);
      expect(debug.mock.calls.filter(([, event]) => event === "reply-write-failed")).toHaveLength(1);
      expect(JSON.stringify(debug.mock.calls)).not.toContain("private terminal error");
    } finally {
      wrapper.unmount();
      debug.mockRestore();
    }
  });

  it("clears Pi task state when its foreground process changes and ignores stale reports", async () => {
    vi.useFakeTimers();
    vi.mocked(getTerminalStatus)
      .mockResolvedValueOnce({ state: "running", foregroundProcess: true, foregroundApp: "pi" })
      .mockResolvedValue({ state: "running", foregroundProcess: true, foregroundApp: "zsh" });
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();
    terminalMock.oscHandlers.get(7501)?.("state=error:app=pi");
    expect(wrapper.emitted("statusChanged")?.at(-1)?.[0]).toMatchObject({ piTaskState: "error" });

    await vi.advanceTimersByTimeAsync(750);
    terminalMock.oscHandlers.get(7501)?.("state=done:app=pi");
    expect(wrapper.emitted("statusChanged")?.at(-1)?.[0]).not.toHaveProperty("piTaskState");
    wrapper.unmount();
    vi.useRealTimers();
  });

  it("waits for every bundled face before opening xterm", async () => {
    let releaseFonts!: () => void;
    terminalMock.fontLoadPromise = new Promise<FontFace[]>((resolve) => {
      releaseFonts = () => resolve([]);
    });
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    expect(terminalMock.fontLoads).toEqual([
      '16px "Muster Nerd Mono", "Muster Nerd Icons", monospace',
      '700 16px "Muster Nerd Mono", "Muster Nerd Icons", monospace',
      '16px "Muster Nerd Icons"',
    ]);
    expect(terminalMock.openCalls).toBe(0);
    releaseFonts();
    await flushPromises();
    expect(terminalMock.events.at(-1)).toBe("open");
    // The icon face is waited on as well: opening before it lands measures the grid without it,
    // and the icon glyphs are the ones the atlas is about.
    expect(terminalMock.events.indexOf("open")).toBeGreaterThan(
      terminalMock.events.indexOf('font:16px "Muster Nerd Icons"'),
    );
    expect(terminalMock.clearTextureAtlasCalls).toBe(0);
    wrapper.unmount();
  });

  it("recovers the column FitAddon reserves for a hidden scrollbar without clipping the grid", async () => {
    terminalMock.screenWidth = 732; // 77 columns of 9.5px, rounded by xterm
    const bounds = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (
      this: HTMLElement,
    ) {
      const width = this.classList.contains("xterm-screen") ? terminalMock.screenWidth : 747.7;
      return { width } as DOMRect;
    });
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    expect(terminalMock.terminal?.cols).toBe(78); // 78 * 9.5 = 741px fits; 79 would be clipped
    expect(createTerminal).toHaveBeenCalledWith("checkout:repo", 78, 24, expect.anything());
    wrapper.unmount();
    bounds.mockRestore();
  });

  it("draws no scrollbar at all in the default hidden mode", async () => {
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    // Not merely invisible: there is no element. xterm's own scrollbar is already hidden, because
    // it reserves the ten pixels the grid is measured against, and a second one drawn over the
    // edge would be decoration on a terminal that already scrolls with the wheel.
    expect(wrapper.find(".terminal-scrollbar").exists()).toBe(false);
    wrapper.unmount();
  });

  it("sizes the thumb to the scrollback and moves it with the viewport", async () => {
    terminalMock.buffer = { length: 120, viewportY: 0 };
    const wrapper = mount(TerminalSession, {
      props: { checkoutId: "checkout:repo", active: true, scrollbar: "always" },
    });
    await flushPromises();

    // 24 rows of a 480px track with 120 lines behind them: the thumb is a fifth of the track, and
    // the viewport is at the top of the scrollback, so the thumb is too.
    expect(wrapper.find(".terminal-scrollbar-idle").exists()).toBe(false);
    expect(wrapper.get(".terminal-scrollbar").attributes()).toMatchObject({
      role: "scrollbar",
      "aria-orientation": "vertical",
      "aria-valuemin": "0",
      "aria-valuemax": "96",
      "aria-valuenow": "0",
      tabindex: "0",
    });
    expect(wrapper.get(".terminal-scrollbar-thumb").attributes("style")).toBe("top: 0px; height: 96px;");

    scrollbackTo(48);
    await flushPromises();

    // Halfway through the scrollback is halfway down the thumb's travel, which is the whole point
    // of deriving one from the other: the thumb is where the viewport is, not near it.
    expect(wrapper.get(".terminal-scrollbar-thumb").attributes("style")).toBe("top: 192px; height: 96px;");
    expect(wrapper.get(".terminal-scrollbar").attributes("aria-valuenow")).toBe("48");
    wrapper.unmount();
  });

  it("leaves `always` showing a full-height thumb when there is nothing to scroll", async () => {
    const wrapper = mount(TerminalSession, {
      props: { checkoutId: "checkout:repo", active: true, scrollbar: "always" },
    });
    await flushPromises();

    // The history fits the screen, so there is no position to be anywhere but the only one. A
    // person who asked for a permanent scrollbar is told there is nothing to scroll rather than
    // left wondering whether the setting took.
    expect(wrapper.get(".terminal-scrollbar-thumb").attributes("style")).toBe("top: 0px; height: 480px;");
    wrapper.unmount();
  });

  it("shows the thumb while `auto` is being scrolled and takes it away when nothing moves", async () => {
    vi.useFakeTimers();
    try {
      terminalMock.buffer = { length: 120, viewportY: 0 };
      const wrapper = mount(TerminalSession, {
        props: { checkoutId: "checkout:repo", active: true, scrollbar: "auto" },
      });
      await flushPromises();

      // A pane nobody is scrolling looks like a pane with no scrollbar, which is the reason to
      // choose `auto` over `always` in the first place.
      expect(wrapper.find(".terminal-scrollbar-idle").exists()).toBe(true);

      scrollbackTo(48);
      await flushPromises();
      expect(wrapper.find(".terminal-scrollbar-idle").exists()).toBe(false);

      await vi.advanceTimersByTimeAsync(899);
      expect(wrapper.find(".terminal-scrollbar-idle").exists()).toBe(false);
      await vi.advanceTimersByTimeAsync(1);
      expect(wrapper.find(".terminal-scrollbar-idle").exists()).toBe(true);
      wrapper.unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not flash a thumb in `auto` for a scrollback that fits on screen", async () => {
    const wrapper = mount(TerminalSession, {
      props: { checkoutId: "checkout:repo", active: true, scrollbar: "auto" },
    });
    await flushPromises();

    // 24 lines of buffer under 24 rows is a terminal with no scrollback, which is what every
    // terminal is for the first few seconds of its life. A thumb sized to a track it cannot move
    // along says nothing, so `auto` keeps quiet even when the wheel ticks over it.
    scrollbackTo(0, 24);
    await flushPromises();
    expect(wrapper.find(".terminal-scrollbar-idle").exists()).toBe(true);
    wrapper.unmount();
  });

  it("keeps the thumb up for as long as `auto` keeps being scrolled", async () => {
    vi.useFakeTimers();
    try {
      terminalMock.buffer = { length: 120, viewportY: 0 };
      const wrapper = mount(TerminalSession, {
        props: { checkoutId: "checkout:repo", active: true, scrollbar: "auto" },
      });
      await flushPromises();

      // Output arriving at the bottom moves the viewport, and the thumb with it. A build that
      // prints for a minute keeps its scrollbar up for that minute, because the thumb really is
      // moving the whole time; the fade is for when the output stops.
      for (let line = 1; line <= 5; line += 1) {
        await vi.advanceTimersByTimeAsync(800);
        scrollbackTo(line * 10);
      }
      expect(wrapper.find(".terminal-scrollbar-idle").exists()).toBe(false);
      await vi.advanceTimersByTimeAsync(900);
      expect(wrapper.find(".terminal-scrollbar-idle").exists()).toBe(true);
      wrapper.unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("scrolls to the line a dragged thumb stands for, and jumps on a press beside it", async () => {
    terminalMock.buffer = { length: 120, viewportY: 0 };
    const wrapper = mount(TerminalSession, {
      props: { checkoutId: "checkout:repo", active: true, scrollbar: "always" },
    });
    await flushPromises();
    const track = wrapper.get(".terminal-scrollbar");

    // Grabbed 24px down a thumb that starts at the top of the track, so the drag keeps the offset
    // it was caught at instead of snapping the thumb's middle under the pointer.
    await track.trigger("pointerdown", { clientY: 24, pointerId: 1 });
    expect(terminalMock.scrollToLines).toEqual([0]);
    await track.trigger("pointermove", { clientY: 480, pointerId: 1 });
    expect(terminalMock.scrollToLines).toEqual([0, 96]);
    expect(wrapper.get(".terminal-scrollbar-thumb").attributes("style")).toBe("top: 384px; height: 96px;");
    await track.trigger("pointerup", { clientY: 480, pointerId: 1 });

    // A press on the empty track puts the middle of the thumb under the pointer instead, which is
    // what every other scrollbar does and what makes clicking above or below the thumb a jump
    // rather than a press that does nothing at all: 200px is 48 above the thumb's middle, so the
    // thumb goes 152px along a 384px travel, which is line 38 of 96.
    await track.trigger("pointerdown", { clientY: 200, pointerId: 2 });
    expect(terminalMock.scrollToLines).toEqual([0, 96, 38]);
    wrapper.unmount();
  });

  it("builds the grid at the scaled cell size and cancels the scale on its own host", async () => {
    // The host is the one thing in the window that is not scaled: the grid is measured and
    // rasterized in the screen's own pixels, so leaving the scale on it would let the compositor
    // stretch a canvas that had already been drawn.
    const wrapper = mount(TerminalSession, {
      props: { checkoutId: "checkout:repo", active: true, fontSize: 20, zoom: 0.8 },
    });
    await flushPromises();

    // The cell is the preference's own size times the window's scale, and it is the scale alone
    // that is cancelled on the host: the size is a real change to the grid, not a transform.
    expect(terminalMock.builtAt).toEqual({ fontSize: 20, cursorBlink: true, cursorStyle: "block", zoom: 0.8 });
    expect(wrapper.get(".terminal-host").attributes("style")).toBe("zoom: 1.25;");
    wrapper.unmount();
  });

  it("redraws the grid at the new cell size when the window is scaled, and refits it", async () => {
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();
    const fitsBefore = terminalLib.fitCalls;

    await wrapper.setProps({ fontSize: 18, zoom: 1.2 });
    await flushPromises();

    // 18px at 120% is a cell of 21.6, and xterm re-measures and repaints on the assignment; the
    // fit is what tells the PTY how many columns the new cell leaves it.
    expect(terminalMock.terminal?.options).toEqual({ fontSize: 18 * 1.2, cursorBlink: true, cursorStyle: "block" });
    expect(terminalLib.fitCalls).toBeGreaterThan(fitsBefore);
    expect(wrapper.get(".terminal-host").attributes("style")).toBe("zoom: 0.8333333333333334;");
    wrapper.unmount();
  });

  it("repaints an open terminal in the new palette, and keeps its session", async () => {
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();
    const closes = vi.mocked(closeTerminal).mock.calls.length;

    // xterm.js' colors are an object rather than declarations, so the stylesheet cannot reach a
    // terminal on its own: the panel reads the palette again and hands it over. The PTY is not told
    // anything, so nothing is lost to a change of palette.
    terminalLib.theme = { background: "#fafafa" };
    theme.value = "light";
    await flushPromises();

    expect(terminalMock.terminal?.options.theme).toEqual({ background: "#fafafa" });
    expect(vi.mocked(closeTerminal).mock.calls.length).toBe(closes);
    wrapper.unmount();
    theme.value = "dark";
  });

  it("reads a drag on the scaled track as a position in the track's own units", async () => {
    terminalMock.buffer = { length: 120, viewportY: 0 };
    const wrapper = mount(TerminalSession, {
      props: { checkoutId: "checkout:repo", active: true, scrollbar: "always", zoom: 1.2 },
    });
    await flushPromises();
    const track = wrapper.get(".terminal-scrollbar");

    // A 480px track at 120% is 576px on the screen, so its middle is at 288 — which is 240 in the
    // track's own units, and line 48 of the 96 lines of travel. Reading the pointer as 288 would
    // have put the thumb on line 60, forty-eight pixels below where it was let go.
    await track.trigger("pointerdown", { clientY: 288, pointerId: 1 });
    expect(terminalMock.scrollToLines).toEqual([48]);
    wrapper.unmount();
  });

  it("keeps `auto` visible during a held drag, then fades after release", async () => {
    vi.useFakeTimers();
    try {
      terminalMock.buffer = { length: 120, viewportY: 0 };
      const wrapper = mount(TerminalSession, {
        props: { checkoutId: "checkout:repo", active: true, scrollbar: "auto" },
      });
      await flushPromises();
      const track = wrapper.get(".terminal-scrollbar");

      await track.trigger("pointerdown", { clientY: 24, pointerId: 1, button: 0 });
      await vi.advanceTimersByTimeAsync(2_000);
      expect(wrapper.find(".terminal-scrollbar-idle").exists()).toBe(false);

      await track.trigger("pointerup", { clientY: 24, pointerId: 1, button: 0 });
      await vi.advanceTimersByTimeAsync(899);
      expect(wrapper.find(".terminal-scrollbar-idle").exists()).toBe(false);
      await vi.advanceTimersByTimeAsync(1);
      expect(wrapper.find(".terminal-scrollbar-idle").exists()).toBe(true);
      wrapper.unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("ignores a move from a pointer that is not the one dragging", async () => {
    terminalMock.buffer = { length: 120, viewportY: 0 };
    const wrapper = mount(TerminalSession, {
      props: { checkoutId: "checkout:repo", active: true, scrollbar: "always" },
    });
    await flushPromises();
    const track = wrapper.get(".terminal-scrollbar");

    await track.trigger("pointerdown", { clientY: 24, pointerId: 1 });
    await track.trigger("pointermove", { clientY: 480, pointerId: 2 });
    expect(terminalMock.scrollToLines).toEqual([0]);
    await track.trigger("pointerup", { clientY: 24, pointerId: 1 });
    await track.trigger("pointermove", { clientY: 480, pointerId: 1 });
    expect(terminalMock.scrollToLines).toEqual([0]);
    wrapper.unmount();
  });

  it("does not drag a thumb on a terminal with no scrollback to drag through", async () => {
    const wrapper = mount(TerminalSession, {
      props: { checkoutId: "checkout:repo", active: true, scrollbar: "always" },
    });
    await flushPromises();

    await wrapper.get(".terminal-scrollbar").trigger("pointerdown", { clientY: 240, pointerId: 1 });
    expect(terminalMock.scrollToLines).toEqual([]);
    wrapper.unmount();
  });

  it("preserves wheel delta instead of turning every trackpad tick into a page", async () => {
    terminalMock.buffer = { length: 120, viewportY: 0 };
    const wrapper = mount(TerminalSession, {
      props: { checkoutId: "checkout:repo", active: true, scrollbar: "always" },
    });
    await flushPromises();
    const track = wrapper.get(".terminal-scrollbar");

    // Line-mode wheels keep their line count. Pixel-mode trackpad gestures accumulate to a whole
    // cell, so several small deltas move smoothly instead of each becoming a full-screen jump.
    const lineWheel = new WheelEvent("wheel", {
      deltaY: 3,
      deltaMode: WheelEvent.DOM_DELTA_LINE,
      cancelable: true,
    });
    track.element.dispatchEvent(lineWheel);
    expect(lineWheel.defaultPrevented).toBe(true);
    expect(terminalMock.scrollLineCounts).toEqual([3]);
    track.element.dispatchEvent(
      new WheelEvent("wheel", { deltaY: -3, deltaMode: WheelEvent.DOM_DELTA_LINE, cancelable: true }),
    );
    expect(terminalMock.scrollLineCounts).toEqual([3, -3]);
    track.element.dispatchEvent(
      new WheelEvent("wheel", { deltaY: 10, deltaMode: WheelEvent.DOM_DELTA_PIXEL, cancelable: true }),
    );
    expect(terminalMock.scrollLineCounts).toEqual([3, -3]);
    track.element.dispatchEvent(
      new WheelEvent("wheel", { deltaY: 10, deltaMode: WheelEvent.DOM_DELTA_PIXEL, cancelable: true }),
    );
    expect(terminalMock.scrollLineCounts).toEqual([3, -3, 1]);
    wrapper.unmount();
  });

  it("supports keyboard scrolling on the accessible scrollbar", async () => {
    terminalMock.buffer = { length: 120, viewportY: 48 };
    const wrapper = mount(TerminalSession, {
      props: { checkoutId: "checkout:repo", active: true, scrollbar: "always" },
    });
    await flushPromises();
    const scrollbar = wrapper.get(".terminal-scrollbar");

    await scrollbar.trigger("keydown", { key: "ArrowDown" });
    await scrollbar.trigger("keydown", { key: "PageUp" });
    await scrollbar.trigger("keydown", { key: "End" });
    expect(terminalMock.scrollToLines).toEqual([49, 26, 96]);
    wrapper.unmount();
  });

  it("picks up the mode chosen after the terminal is already open", async () => {
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();
    expect(wrapper.find(".terminal-scrollbar").exists()).toBe(false);

    // The thumb is sized against a track that has to be in the document first, so the switch waits
    // for the element to exist rather than measuring nothing.
    terminalMock.buffer = { length: 120, viewportY: 0 };
    await wrapper.setProps({ scrollbar: "auto" });
    await flushPromises();
    expect(wrapper.get(".terminal-scrollbar-thumb").attributes("style")).toBe("top: 0px; height: 96px;");
    // Turning `auto` on shows it once, so the answer to "did that do anything" is on screen, and
    // the same fade as a scroll is what takes it away again.
    expect(wrapper.find(".terminal-scrollbar-idle").exists()).toBe(false);

    await wrapper.setProps({ scrollbar: "hidden" });
    await flushPromises();
    expect(wrapper.find(".terminal-scrollbar").exists()).toBe(false);
    wrapper.unmount();
  });

  it("closes a shell at a prompt without asking, because nothing is lost", async () => {
    const confirm = vi.spyOn(window, "confirm");
    vi.mocked(getTerminalStatus).mockResolvedValue({ state: "running", foregroundProcess: false });
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    await wrapper.vm.requestClose();
    await flushPromises();
    // A shell at a prompt is `running` too, and the browser's dialog is never reached: it is not the
    // app's own, and nothing is being stopped.
    expect(confirm).not.toHaveBeenCalled();
    expect(closeTerminal).toHaveBeenCalledWith("checkout:repo", "session:new");
    expect(wrapper.emitted("closed")).toHaveLength(1);
    wrapper.unmount();
    confirm.mockRestore();
  });

  it("asks in the app's own dialog before stopping a process, and cancelling stops nothing", async () => {
    const confirm = vi.spyOn(window, "confirm");
    vi.mocked(getTerminalStatus).mockResolvedValue({
      state: "running",
      foregroundProcess: true,
      foregroundApp: "pnpm",
    });
    const wrapper = mount(TerminalSession, {
      props: { checkoutId: "checkout:repo", name: "web", active: true },
    });
    await flushPromises();

    await wrapper.vm.requestClose();
    await flushPromises();
    // Asked where the rest of the app asks, and about what: the terminal by the name it is called and
    // the program that would be stopped. A native confirm could say neither.
    const dialog = document.body.querySelector('[role="dialog"]')!;
    expect(dialog.textContent).toContain("web");
    expect(dialog.textContent).toContain("pnpm");
    expect(confirm).not.toHaveBeenCalled();
    expect(closeTerminal).not.toHaveBeenCalled();

    // Cancel, or Escape: the process is left running and the terminal keeps what it printed.
    (dialog.querySelector("footer button") as HTMLButtonElement).click();
    await flushPromises();
    expect(closeTerminal).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("asks outside the pane that asked, so a terminal that is not on screen still shows its question", async () => {
    // The measured bug: `SessionPane` keeps every terminal mounted with `v-show`, so a pane that is
    // not the active one is `display: none` rather than gone — and it still answers. A dialog drawn
    // inside that pane existed in the DOM and could not be seen, which is why closing a background
    // terminal with a build in it asked a question nobody could reach. Only `body` is outside it.
    vi.mocked(getTerminalStatus).mockResolvedValue({
      state: "running",
      foregroundProcess: true,
      foregroundApp: "pnpm",
    });
    const hidden = document.createElement("div");
    hidden.style.display = "none";
    document.body.appendChild(hidden);
    const wrapper = mount(TerminalSession, {
      props: { checkoutId: "checkout:repo", name: "web", active: true },
      attachTo: hidden,
    });
    await flushPromises();

    await wrapper.vm.requestClose();
    await flushPromises();

    const dialog = document.body.querySelector('[role="dialog"]');
    expect(dialog).not.toBeNull();
    // Outside the hidden subtree, which is the only thing that could have taken it out of sight.
    expect(hidden.contains(dialog)).toBe(false);
    expect(dialog?.textContent).toContain("pnpm");
    expect(closeTerminal).not.toHaveBeenCalled();

    // And the answer still reaches the terminal that asked, from outside its own pane.
    const buttons = dialog?.querySelectorAll("footer button") ?? [];
    (buttons[buttons.length - 1] as HTMLButtonElement).click();
    await flushPromises();
    expect(closeTerminal).toHaveBeenCalledWith("checkout:repo", "session:new");

    wrapper.unmount();
    hidden.remove();
  });

  it("stops the process once the question is answered yes", async () => {
    vi.mocked(getTerminalStatus).mockResolvedValue({
      state: "running",
      foregroundProcess: true,
      foregroundApp: "pnpm",
    });
    const wrapper = mount(TerminalSession, {
      props: { checkoutId: "checkout:repo", name: "web", active: true },
    });
    await flushPromises();

    await wrapper.vm.requestClose();
    await flushPromises();
    const buttons = document.body.querySelectorAll('[role="dialog"] footer button');
    (buttons[buttons.length - 1] as HTMLButtonElement).click();
    await flushPromises();
    expect(closeTerminal).toHaveBeenCalledWith("checkout:repo", "session:new");
    expect(wrapper.emitted("closed")).toHaveLength(1);
    wrapper.unmount();
  });

  it("closes nothing when the status read fails, because 'unknown' is not 'nothing running'", async () => {
    vi.mocked(getTerminalStatus).mockRejectedValue(new Error("the backend is unreachable"));
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    await wrapper.vm.requestClose();
    await flushPromises();
    expect(closeTerminal).not.toHaveBeenCalled();
    expect(document.body.querySelector('[role="dialog"]')).toBeNull();
    expect(wrapper.get('[role="alert"]').text()).toContain("the backend is unreachable");
    wrapper.unmount();
  });

  it("opens an ordinary terminal without a review prompt", async () => {
    const wrapper = mount(TerminalSession, {
      props: { checkoutId: "checkout:repo", active: true },
    });
    await flushPromises();

    expect(createTerminal).toHaveBeenCalledWith("checkout:repo", 80, 24, expect.anything());
    wrapper.unmount();
  });

  it("does not steal focus when a hidden terminal finishes starting", async () => {
    let resolveCreate!: (value: typeof created) => void;
    vi.mocked(createTerminal).mockReturnValue(
      new Promise((resolve) => {
        resolveCreate = resolve;
      }),
    );
    const wrapper = mount(TerminalSession, {
      props: { checkoutId: "checkout:repo", active: true, visible: false, focused: true },
    });
    await flushPromises();
    expect(createTerminal).toHaveBeenCalledTimes(1);
    resolveCreate(created);
    await flushPromises();
    expect(terminalMock.focusCalls).toBe(0);

    await wrapper.setProps({ visible: true, focused: true });
    await flushPromises();
    expect(terminalMock.focusCalls).toBe(1);
    wrapper.unmount();
  });

  it("keeps resize requests scoped to each terminal session", async () => {
    const first = { ...created, session: { ...created.session, id: "session:first" } };
    const second = { ...created, session: { ...created.session, id: "session:second" } };
    vi.mocked(createTerminal).mockResolvedValueOnce(first).mockResolvedValueOnce(second);
    const wrapper = mount({
      components: { TerminalSession },
      template:
        '<div><TerminalSession checkout-id="checkout:repo" :active="true" /><TerminalSession checkout-id="checkout:repo" :active="true" /></div>',
    });
    await flushPromises();

    terminalMock.resizes[0]({ cols: 91, rows: 30 });
    terminalMock.resizes[1]({ cols: 103, rows: 37 });
    await flushPromises();

    expect(resizeTerminal).toHaveBeenCalledWith("checkout:repo", "session:first", 91, 30);
    expect(resizeTerminal).toHaveBeenCalledWith("checkout:repo", "session:second", 103, 37);
    wrapper.unmount();
  });

  it("coalesces resize requests while a terminal resize is pending", async () => {
    let finishFirstResize!: () => void;
    vi.mocked(resizeTerminal).mockImplementationOnce(
      () => new Promise<void>((resolve) => (finishFirstResize = resolve)),
    );
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    terminalMock.resizes[0]?.({ cols: 97, rows: 31 });
    await flushPromises();
    terminalMock.resizes[0]?.({ cols: 100, rows: 32 });
    terminalMock.resizes[0]?.({ cols: 104, rows: 34 });
    finishFirstResize();
    await flushPromises();

    expect(resizeTerminal).toHaveBeenCalledTimes(2);
    expect(resizeTerminal).toHaveBeenNthCalledWith(1, "checkout:repo", "session:new", 97, 31);
    expect(resizeTerminal).toHaveBeenNthCalledWith(2, "checkout:repo", "session:new", 104, 34);
    wrapper.unmount();
  });

  it("sends a resize that lands while the previous one is still in flight", async () => {
    // The window between a send resolving and the queue draining: the loop has already compared
    // its size against the latest and found them equal, so it breaks, but the chain has not
    // reached the point where new work is accepted again. A measurement arriving in that window is
    // refused by the queue and has to be picked up when it drains, or the shell keeps wrapping to
    // the width the pane had before the layout moved.
    let finishResize!: () => void;
    vi.mocked(resizeTerminal).mockImplementationOnce(() => new Promise<void>((resolve) => (finishResize = resolve)));
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    terminalMock.resizes[0]?.({ cols: 97, rows: 31 });
    await flushPromises();
    expect(resizeTerminal).toHaveBeenCalledTimes(1);

    finishResize();
    // Inside the window, and not after it: one hop past the comparison, three before the drain.
    await Promise.resolve();
    terminalMock.resizes[0]?.({ cols: 104, rows: 34 });
    await flushPromises();

    expect(resizeTerminal).toHaveBeenNthCalledWith(2, "checkout:repo", "session:new", 104, 34);
    wrapper.unmount();
  });

  it("stops asking once the backend refuses a size", async () => {
    vi.mocked(resizeTerminal).mockRejectedValue(new Error("PTY dimensions must be non-zero"));
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    terminalMock.resizes[0]?.({ cols: 97, rows: 31 });
    await flushPromises();
    expect(resizeTerminal).toHaveBeenCalledTimes(1);

    // The refused size counts as attempted, so the drain does not ask for it again on its own.
    await flushPromises();
    await flushPromises();
    expect(resizeTerminal).toHaveBeenCalledTimes(1);
    expect(wrapper.get('[role="alert"]').text()).toContain("PTY dimensions must be non-zero");
    wrapper.unmount();
  });

  it("sends no resize after the close has begun, however late it arrives", async () => {
    // The drain re-checks the last measurement as it finishes, so a size that arrives during the
    // close would otherwise be picked up and sent behind the close, against a session the backend
    // is about to shut down.
    let finishResize!: () => void;
    vi.mocked(resizeTerminal).mockImplementationOnce(() => new Promise<void>((resolve) => (finishResize = resolve)));
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();
    vi.mocked(getTerminalStatus).mockResolvedValue({ state: "running" });

    terminalMock.resizes[0]?.({ cols: 97, rows: 31 });
    await flushPromises();
    expect(resizeTerminal).toHaveBeenCalledTimes(1);

    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    const closed = wrapper.vm.requestClose();
    await flushPromises();
    finishResize();
    terminalMock.resizes[0]?.({ cols: 104, rows: 34 });
    await closed;
    await flushPromises();

    expect(resizeTerminal).toHaveBeenCalledTimes(1);
    expect(closeTerminal).toHaveBeenCalledOnce();
    wrapper.unmount();
    confirm.mockRestore();
  });

  it("waits for a resize the drain starts as it finishes before it closes the session", async () => {
    // The close has to be the end of the queue rather than a moment in it: a send still in flight
    // when the PTY goes is a size the session never learns about.
    let finishResize!: () => void;
    vi.mocked(resizeTerminal).mockImplementationOnce(() => new Promise<void>((resolve) => (finishResize = resolve)));
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();
    vi.mocked(getTerminalStatus).mockResolvedValue({ state: "running" });

    terminalMock.resizes[0]?.({ cols: 97, rows: 31 });
    await flushPromises();
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    const closed = wrapper.vm.requestClose();
    await flushPromises();
    expect(closeTerminal).not.toHaveBeenCalled();

    finishResize();
    await closed;

    expect(closeTerminal).toHaveBeenCalledOnce();
    wrapper.unmount();
    confirm.mockRestore();
  });

  it("waits for input in flight to reach the PTY before it closes the session", async () => {
    // The last keystrokes typed before the close are the ones the close cannot take back: they are
    // in the queue, the command they spell out is not in the buffer yet, and the PTY they were meant
    // for is what the close takes away.
    let finishWrite!: () => void;
    vi.mocked(writeTerminal).mockImplementationOnce(() => new Promise<void>((resolve) => (finishWrite = resolve)));
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();
    vi.mocked(getTerminalStatus).mockResolvedValue({ state: "running", foregroundProcess: false });

    terminalMock.input?.("cd /work/repo\n");
    await flushPromises();
    expect(writeTerminal).toHaveBeenCalledOnce();
    const closed = wrapper.vm.requestClose();
    await flushPromises();
    // Still not closed, because the write this keystroke is waiting behind has not been written yet.
    expect(closeTerminal).not.toHaveBeenCalled();

    finishWrite();
    await closed;

    expect(writeTerminal).toHaveBeenCalledWith(
      "checkout:repo",
      "session:new",
      new TextEncoder().encode("cd /work/repo\n"),
    );
    expect(closeTerminal).toHaveBeenCalledOnce();
    wrapper.unmount();
  });

  it("closes a session whose input IPC never settles, because waiting for it is bounded", async () => {
    // The backend has its own PTY write deadline, but this promise models an IPC call that never
    // answers. Close must still reach its backend command after the frontend drain budget expires.
    vi.useFakeTimers();
    const { toasts, dismiss } = useToasts();
    for (const toast of [...toasts.value]) dismiss(toast.id);
    let finishWrite!: () => void;
    const unhandled: unknown[] = [];
    const collect = (cause: unknown) => unhandled.push(cause);
    process.on("unhandledRejection", collect);
    try {
      vi.mocked(writeTerminal).mockImplementationOnce(() => new Promise<void>((resolve) => (finishWrite = resolve)));
      const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
      await flushPromises();
      vi.mocked(getTerminalStatus).mockResolvedValue({ state: "running", foregroundProcess: false });

      terminalMock.input?.("y");
      terminalMock.input?.("must not be sent after close");
      await flushPromises();
      expect(writeTerminal).toHaveBeenCalledOnce();
      const closed = wrapper.vm.requestClose();
      await flushPromises();
      expect(closeTerminal).not.toHaveBeenCalled();

      // Bounded, not given up on at once: a shell that is merely slow still gets its keystroke, and
      // the wait is abandoned at a moment where it is clear the answer is never coming.
      await vi.advanceTimersByTimeAsync(999);
      expect(closeTerminal).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(1);

      // Which is the whole of it: the close reaches the backend anyway, because ending a session
      // never takes the writer lock the blocked write is holding.
      expect(closeTerminal).toHaveBeenCalledWith("checkout:repo", "session:new");
      await closed;
      expect(wrapper.emitted("closed")).toHaveLength(1);

      // Closing invalidates the request waiting behind this one. Even if the active request later
      // succeeds, it must not release stale input back into the closed session.
      expect(wrapper.get('[role="alert"]').text()).toContain("still pending when close timed out");
      expect(toasts.value.map((toast) => toast.message)).toContain(
        "Terminal input was still pending when close timed out; it may have been partially delivered, and queued input was discarded.",
      );
      finishWrite();
      await flushPromises();
      expect(writeTerminal).toHaveBeenCalledOnce();
      expect(unhandled).toEqual([]);
      wrapper.unmount();
      for (const toast of [...toasts.value]) dismiss(toast.id);
    } finally {
      process.off("unhandledRejection", collect);
      vi.useRealTimers();
    }
  });

  it("drops a resize that arrives after the pane is gone", async () => {
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();
    vi.mocked(getTerminalStatus).mockResolvedValue({ state: "running" });

    wrapper.unmount();
    terminalMock.resizes[0]?.({ cols: 104, rows: 34 });
    await flushPromises();

    expect(resizeTerminal).not.toHaveBeenCalled();
  });

  it("closes the session when its process exits, so an ended shell does not linger", async () => {
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    vi.mocked(getTerminalStatus).mockResolvedValue({ state: "exited", exitCode: 0 });
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    // `exit` and `:q` end a shell on purpose, and the panel is where that has to show up. Left
    // open, the session is a frozen last frame that reads as a hung app plus a live-looking entry
    // in the sidebar, and closing it means finding the close button first.
    expect(wrapper.emitted("statusChanged")).toEqual([[{ state: "exited", exitCode: 0 }]]);
    expect(closeTerminal).toHaveBeenCalledWith("checkout:repo", "session:new");
    expect(wrapper.emitted("closed")).toEqual([[workspace]]);
    // The process is already gone, so the question that stopping a live one needs is never asked.
    expect(confirm).not.toHaveBeenCalled();
    wrapper.unmount();
    confirm.mockRestore();
  });

  it("changes the directory of an idle shell, and leaves a busy one alone", async () => {
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();
    vi.mocked(getTerminalStatus).mockResolvedValue({ state: "running", foregroundProcess: false });
    const changed = await wrapper.vm.changeDirectory("/work/repo-wt");
    expect(changed).toBe(true);
    expect(writeTerminal).toHaveBeenLastCalledWith(
      "checkout:repo",
      "session:new",
      new TextEncoder().encode("cd '/work/repo-wt'\n"),
    );

    // A build in front of the shell is not told anything: typing `cd` would feed the build.
    vi.mocked(getTerminalStatus).mockResolvedValue({ state: "running", foregroundProcess: true });
    expect(await wrapper.vm.changeDirectory("/work/repo-wt")).toBe(false);

    // A path is typed into a shell, so anything that could end the line or start a second command
    // is refused rather than quoted into shape.
    vi.mocked(getTerminalStatus).mockResolvedValue({ state: "running", foregroundProcess: false });
    await wrapper.vm.changeDirectory("/work/repo\nrm -rf /");
    expect(writeTerminal).toHaveBeenCalledTimes(1);
    wrapper.unmount();
  });

  it("discards writes queued behind a failed input and accepts a fresh input", async () => {
    let rejectFirstWrite!: (cause: unknown) => void;
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();
    vi.mocked(writeTerminal).mockImplementationOnce(
      () => new Promise<void>((_, reject) => (rejectFirstWrite = reject)),
    );

    terminalMock.input?.("first");
    terminalMock.input?.("queued");
    terminalMock.input?.("discarded");
    await flushPromises();
    expect(writeTerminal).toHaveBeenCalledOnce();

    rejectFirstWrite(new Error("PTY input timed out after writing 2 of 5 bytes"));
    await flushPromises();
    expect(writeTerminal).toHaveBeenCalledOnce();
    expect(wrapper.get('[role="alert"]').text()).toContain("discarded 2 queued writes (15 bytes)");

    terminalMock.input?.("new");
    await flushPromises();
    expect(writeTerminal).toHaveBeenCalledTimes(2);
    expect(writeTerminal).toHaveBeenLastCalledWith("checkout:repo", "session:new", new TextEncoder().encode("new"));
    wrapper.unmount();
  });

  it("never retargets queued input when the terminal moves checkouts", async () => {
    let finishFirstWrite!: () => void;
    vi.mocked(writeTerminal).mockImplementationOnce(() => new Promise<void>((resolve) => (finishFirstWrite = resolve)));
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    terminalMock.input?.("sent to current checkout");
    terminalMock.input?.("must not be retargeted");
    await flushPromises();
    expect(writeTerminal).toHaveBeenCalledOnce();
    await wrapper.setProps({ checkoutId: "checkout:target" });
    finishFirstWrite();
    await flushPromises();

    expect(writeTerminal).toHaveBeenCalledOnce();
    expect(writeTerminal).toHaveBeenCalledWith(
      "checkout:repo",
      "session:new",
      new TextEncoder().encode("sent to current checkout"),
    );
    expect(wrapper.get('[role="alert"]').text()).toContain("session changed");
    wrapper.unmount();
  });

  it("does not send input still queued when the terminal unmounts", async () => {
    let finishFirstWrite!: () => void;
    vi.mocked(writeTerminal).mockImplementationOnce(() => new Promise<void>((resolve) => (finishFirstWrite = resolve)));
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    terminalMock.input?.("in flight");
    terminalMock.input?.("stale queued input");
    await flushPromises();
    expect(writeTerminal).toHaveBeenCalledOnce();

    wrapper.unmount();
    finishFirstWrite();
    await flushPromises();
    expect(writeTerminal).toHaveBeenCalledOnce();
  });

  it("keeps the real PTY error visible after a successful move when the cd write fails", async () => {
    const { toasts, dismiss } = useToasts();
    for (const toast of [...toasts.value]) dismiss(toast.id);
    const checkout: Checkout = {
      id: "checkout:repo",
      repoId: "repo:repo",
      path: "/work/repo",
      canonicalPath: "/work/repo",
      isPrimary: true,
      changedFiles: 0,
      isMissing: false,
      sessions: [],
    };
    const target = { ...checkout, id: "checkout:target", path: "/work/target", isPrimary: false };
    const error = {
      code: "terminal_ownership_mismatch",
      message: "terminal session does not belong to the requested checkout",
    };
    vi.mocked(loadTerminalLayout).mockResolvedValue(null);
    vi.mocked(saveTerminalLayout).mockResolvedValue(undefined);
    vi.mocked(moveTerminal).mockResolvedValue({
      repos: [],
      activeCheckoutId: target.id,
      activeSessionId: created.session.id,
    });

    const wrapper = mount(SessionPane, {
      props: {
        checkout,
        checkouts: [checkout, target],
        activeSessionId: null,
        isOpening: true,
        shellRequest: null,
        terminalSettings: { ...DEFAULT_SETTINGS.terminal },
      },
    });
    await wrapper.setProps({
      isOpening: false,
      shellRequest: { checkoutId: checkout.id, token: 1 },
      activeSessionId: created.session.id,
      registeredSessionIds: [created.session.id],
    });
    await flushPromises();
    const terminalSession = wrapper.findComponent(TerminalSession);
    expect(terminalSession.exists()).toBe(true);
    vi.mocked(writeTerminal).mockRejectedValueOnce(error);

    const result = await wrapper.vm.moveSession(created.session.id, target.id, 0);
    await flushPromises();

    expect(result).toEqual({ moved: true, directoryChange: "not-written" });
    expect(moveTerminal).toHaveBeenCalledWith(checkout.id, created.session.id, target.id, true);
    expect(saveTerminalLayout).toHaveBeenCalledWith(
      target.id,
      expect.objectContaining({ sessionOrder: [created.session.id] }),
    );
    expect(writeTerminal).toHaveBeenCalledWith(
      target.id,
      created.session.id,
      new TextEncoder().encode("cd '/work/target'\n"),
    );
    expect(toasts.value.map((toast) => toast.message)).toEqual([
      target.path + ": the terminal moved, but its directory was not changed.",
    ]);
    expect(toasts.value.map((toast) => toast.message)).not.toContain("busy");
    expect(terminalSession.get('[role="alert"]').text()).toBe(error.message);
    wrapper.unmount();
    for (const toast of [...toasts.value]) dismiss(toast.id);
  });

  it("stops asking for status once the process is gone", async () => {
    vi.useFakeTimers();
    try {
      vi.mocked(getTerminalStatus).mockResolvedValue({ state: "exited", exitCode: 0 });
      const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
      await flushPromises();
      const callsAfterExit = vi.mocked(getTerminalStatus).mock.calls.length;

      await vi.advanceTimersByTimeAsync(5000);

      // A closed session has nothing left to report, and polling it would only produce errors
      // for a session the backend no longer has.
      expect(vi.mocked(getTerminalStatus).mock.calls.length).toBe(callsAfterExit);
      wrapper.unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("drops a status read that lands after the pane is gone", async () => {
    // A poll still in flight when the view goes away answers to nobody: publishing it writes state
    // on a component that is not there, and an `exited` in that reply would ask a session that is
    // already gone to close itself.
    let resolveStatus!: (status: TerminalSessionStatus) => void;
    vi.mocked(getTerminalStatus)
      .mockResolvedValueOnce({ state: "running", foregroundProcess: false })
      .mockReturnValueOnce(
        new Promise((resolve) => {
          resolveStatus = resolve;
        }),
      );
    // Before the mount, so the interval the session starts is the faked one and this poll can be
    // made to happen at all.
    vi.useFakeTimers();
    // Listeners rather than `wrapper.emitted`, because the pane they belonged to is gone by the
    // time the reply lands and only a listener outside it can still say whether it was called.
    const onStatusChanged = vi.fn();
    const onClosed = vi.fn();
    try {
      const wrapper = mount(TerminalSession, {
        props: { checkoutId: "checkout:repo", active: true, onStatusChanged, onClosed },
      });
      await flushPromises();
      expect(onStatusChanged).toHaveBeenCalledExactlyOnceWith({ state: "running", foregroundProcess: false });

      await vi.advanceTimersByTimeAsync(750);
      expect(vi.mocked(getTerminalStatus).mock.calls.length).toBe(2);
      wrapper.unmount();
      resolveStatus({ state: "exited", exitCode: 0 });
      await vi.advanceTimersByTimeAsync(1);

      // The exit is a real one and the pane that would have shown it is gone: nothing is published
      // on the strength of a status nobody is left to read, and nothing is closed for it either.
      expect(onStatusChanged).toHaveBeenCalledTimes(1);
      expect(onClosed).not.toHaveBeenCalled();
      expect(closeTerminal).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("drops a status error for a checkout the terminal no longer belongs to", async () => {
    vi.useFakeTimers();
    let rejectStatus!: (cause: unknown) => void;
    vi.mocked(getTerminalStatus)
      .mockResolvedValueOnce({ state: "running" })
      .mockReturnValueOnce(
        new Promise<TerminalSessionStatus>((_, reject) => {
          rejectStatus = reject;
        }),
      )
      .mockResolvedValue({ state: "running" });
    const onStatusChanged = vi.fn();
    try {
      const wrapper = mount(TerminalSession, {
        props: { checkoutId: "checkout:repo", active: true, onStatusChanged },
      });
      await flushPromises();
      await vi.advanceTimersByTimeAsync(750);
      expect(vi.mocked(getTerminalStatus).mock.calls).toHaveLength(2);

      await wrapper.setProps({ checkoutId: "checkout:other" });
      rejectStatus(new Error("stale checkout poll failed"));
      await flushPromises();

      expect(wrapper.find('[role="alert"]').exists()).toBe(false);
      expect(onStatusChanged).toHaveBeenCalledTimes(2);
      expect(vi.mocked(getTerminalStatus).mock.calls.at(-1)).toEqual(["checkout:other", "session:new"]);
      wrapper.unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("bounds status polls, deduplicates transient errors, then recovers and detects exit", async () => {
    vi.useFakeTimers();
    const temporaryError = new Error("the backend is temporarily unavailable");
    let rejectStatus!: (cause: unknown) => void;
    vi.mocked(getTerminalStatus)
      .mockResolvedValueOnce({ state: "running" })
      .mockReturnValueOnce(
        new Promise<TerminalSessionStatus>((_, reject) => {
          rejectStatus = reject;
        }),
      )
      .mockRejectedValueOnce(temporaryError)
      .mockResolvedValueOnce({ state: "running" })
      .mockResolvedValueOnce({ state: "exited", exitCode: 0 })
      .mockResolvedValue({ state: "running" });
    try {
      const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
      await flushPromises();
      await vi.advanceTimersByTimeAsync(750);
      expect(vi.mocked(getTerminalStatus).mock.calls).toHaveLength(2);

      // A slow IPC read occupies the single poll slot, regardless of how many interval ticks pass.
      await vi.advanceTimersByTimeAsync(3750);
      expect(vi.mocked(getTerminalStatus).mock.calls).toHaveLength(2);
      rejectStatus(temporaryError);
      await flushPromises();
      expect(wrapper.get('[role="alert"]').text()).toContain(temporaryError.message);

      const alert = wrapper.get('[role="alert"]').element;
      const mutations: MutationRecord[] = [];
      const observer = new MutationObserver((records) => mutations.push(...records));
      observer.observe(alert, { childList: true, subtree: true, characterData: true });
      await vi.advanceTimersByTimeAsync(750);
      await flushPromises();
      expect(vi.mocked(getTerminalStatus).mock.calls).toHaveLength(3);
      expect(wrapper.get('[role="alert"]').text()).toContain(temporaryError.message);
      expect(mutations).toHaveLength(0);
      observer.disconnect();

      await vi.advanceTimersByTimeAsync(750);
      await flushPromises();
      expect(wrapper.find('[role="alert"]').exists()).toBe(false);
      await vi.advanceTimersByTimeAsync(750);
      await flushPromises();
      expect(wrapper.emitted("statusChanged")?.at(-1)?.[0]).toMatchObject({ state: "exited", exitCode: 0 });
      expect(closeTerminal).toHaveBeenCalledWith("checkout:repo", "session:new");
      wrapper.unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("surfaces a missing session with a controlled retry and restores normal polling on recovery", async () => {
    vi.useFakeTimers();
    vi.mocked(getTerminalStatus)
      .mockResolvedValueOnce({ state: "running" })
      .mockRejectedValueOnce({
        code: "terminal_session_missing",
        message: "terminal session ID is not registered",
      })
      .mockResolvedValue({ state: "running" });
    try {
      const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
      await flushPromises();
      await vi.advanceTimersByTimeAsync(750);
      await flushPromises();

      expect(wrapper.get('[role="alert"]').text()).toContain("Terminal session is unavailable.");
      expect(wrapper.get('[role="alert"]').text()).toContain("terminal session ID is not registered");
      expect(wrapper.get('[role="alert"] button').text()).toBe("Retry");
      await vi.advanceTimersByTimeAsync(4999);
      expect(vi.mocked(getTerminalStatus).mock.calls).toHaveLength(2);

      await wrapper.get('[role="alert"] button').trigger("click");
      await flushPromises();
      expect(wrapper.find('[role="alert"]').exists()).toBe(false);
      await vi.advanceTimersByTimeAsync(750);
      expect(vi.mocked(getTerminalStatus).mock.calls).toHaveLength(4);
      wrapper.unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("asks a hidden terminal far less often, and at full pace again once it is on screen", async () => {
    vi.useFakeTimers();
    try {
      const wrapper = mount(TerminalSession, {
        props: { checkoutId: "checkout:repo", active: true, visible: false },
      });
      await flushPromises();
      const afterStart = vi.mocked(getTerminalStatus).mock.calls.length;

      // The row in the sidebar still has to name the process in front of the shell, so a terminal
      // nobody is looking at keeps asking. It just stops asking four times a second about it.
      await vi.advanceTimersByTimeAsync(5000);
      expect(vi.mocked(getTerminalStatus).mock.calls.length).toBe(afterStart + 1);

      await wrapper.setProps({ visible: true });
      await flushPromises();
      await vi.advanceTimersByTimeAsync(1500);
      expect(vi.mocked(getTerminalStatus).mock.calls.length).toBe(afterStart + 3);
      wrapper.unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("closes a session that exits on a hidden terminal's slower poll, and stops asking", async () => {
    vi.useFakeTimers();
    try {
      vi.mocked(getTerminalStatus).mockResolvedValue({ state: "exited", exitCode: 0 });
      const wrapper = mount(TerminalSession, {
        props: { checkoutId: "checkout:repo", active: true, visible: false },
      });
      await flushPromises();

      // The slow pace must not cost the exit its detection: `exit` still closes the session, and
      // the interval that found it goes with it rather than polling a session nobody has.
      expect(wrapper.emitted("statusChanged")).toEqual([[{ state: "exited", exitCode: 0 }]]);
      expect(closeTerminal).toHaveBeenCalledWith("checkout:repo", "session:new");
      expect(wrapper.emitted("closed")).toEqual([[workspace]]);
      const callsAfterExit = vi.mocked(getTerminalStatus).mock.calls.length;
      await vi.advanceTimersByTimeAsync(20000);
      expect(vi.mocked(getTerminalStatus).mock.calls.length).toBe(callsAfterExit);
      wrapper.unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("tells the reader how much of its output has been parsed, and stops telling it once the pane is gone", async () => {
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();
    appEvents.emit.mockClear();

    // Two report steps' worth of output arriving while xterm parses none of it. The reader counted
    // it the moment it handed it over, so there is nothing new to say here: the number it is being
    // given is how much it has *parsed*, and that is still nothing.
    terminalMock.channel?.onmessage(new Uint8Array(2 * PTY_OUTPUT_REPORT_STEP_BYTES).buffer);
    expect(appEvents.emit).not.toHaveBeenCalled();

    // The renderer catches up, and the total it reports is what the reader consumes against the
    // bytes it sent — including the ones that were still in the channel a moment ago.
    terminalMock.writeCallbacks.shift()?.();
    expect(appEvents.emit).toHaveBeenLastCalledWith("terminal-output-flow", {
      sessionId: "session:new",
      parsed: 2 * PTY_OUTPUT_REPORT_STEP_BYTES,
    });
    // And a session that is still idle says nothing more: the reader only needs to hear the total
    // move, and a report per chunk would be an event per 64 KiB of a busy build.
    appEvents.emit.mockClear();
    terminalMock.writeCallbacks.shift()?.();
    expect(appEvents.emit).not.toHaveBeenCalled();

    wrapper.unmount();
    // Nothing is going to parse this session's output any more, so the reader is told that rather
    // than left holding the PTY until its own timeout, five seconds later.
    expect(appEvents.emit).toHaveBeenCalledWith("terminal-output-flow", { sessionId: "session:new" });
  });

  it("loses no output and reorders none while the renderer falls behind", async () => {
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    const burst = Array.from({ length: 8 }, (_, index) => Uint8Array.of(index));
    for (const chunk of burst) terminalMock.channel?.onmessage(chunk.buffer as ArrayBuffer);
    // Only the first chunk is with xterm while it is parsing it, and nothing is dropped to make
    // room for the rest: once the renderer has worked through everything, every byte is there
    // once and in the order it arrived.
    expect(terminalMock.output.flat()).toEqual([0]);
    while (terminalMock.writeCallbacks.length > 0) terminalMock.writeCallbacks.shift()?.();
    expect(terminalMock.output.flat()).toEqual(burst.map((_, index) => index));
    wrapper.unmount();
  });

  it("watches for a lost renderer context, so a terminal that lost one does not stay on the fallback", async () => {
    const wrapper = mount(TerminalSession, { props: { checkoutId: "checkout:repo", active: true } });
    await flushPromises();

    expect(terminalLib.watchTerminalRendererRecovery).toHaveBeenCalledWith(
      expect.anything(),
      terminalLib.attachTerminalRenderer.mock.results.at(-1)?.value,
    );
    wrapper.unmount();
    // The listener goes with the pane: a terminal that is no longer on the page must not go on
    // taking a WebGL context from the machine it is no longer drawing on.
    const recovery = terminalLib.watchTerminalRendererRecovery.mock.results.at(-1)?.value as {
      dispose: () => void;
    };
    expect(recovery.dispose).toHaveBeenCalled();
  });
});
