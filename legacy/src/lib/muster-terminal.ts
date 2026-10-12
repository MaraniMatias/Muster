import { ClipboardAddon } from "@xterm/addon-clipboard";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import type { ITheme, IDisposable } from "@xterm/xterm";
import type { TerminalCursorStyle } from "../domain/settings";
import { ligatureRanges } from "./ligature-joiner";

/**
 * The faces shared by the terminal and editor and bundled in `src/assets/fonts`, icons included.
 *
 * The icon face is in the list rather than behind the terminal's back because the atlas is
 * rasterized per glyph from this string: a face that is not in it gets a tofu box or a borrowed
 * face drawn at the cell width, and a statusline is mostly separators.
 */
export const TERMINAL_FONT_FAMILY = '"Muster Nerd Mono", "Muster Nerd Icons", monospace';

/**
 * A character from the icon face, asked for by name.
 *
 * The face carries nothing but icons, so the load has to say which one or it is never fetched:
 * `document.fonts.load` asks the browser whether a face covers the given text, and with no text it
 * asks about a space, which this face does not have. U+E0B0 is the powerline separator, the glyph
 * drawn on more lines of a terminal than any other icon, and it is the character to keep when the
 * icon set is ever trimmed.
 */
const ICON_PROBE = "\uE0B0";

/**
 * The cell size the terminal draws at, which is the size it is drawn at whether or not the
 * window around it is scaled.
 *
 * The window's scale is a `zoom` on the root element, and the terminal's host cancels it out, so
 * the grid is measured and rasterized at this size in the screen's own pixels. That is the whole
 * reason for the cancel: leaving the host scaled would let the compositor stretch an already drawn
 * WebGL canvas, which is how a terminal ends up legible and soft at the same time.
 */
export function terminalFontSize(fontSize: number, zoom: number): number {
  return fontSize * zoom;
}

/**
 * The terminal's colors, read out of the stylesheet rather than written here (B.1).
 *
 * Every color xterm.js wants is a `--muster-*` token, so this is a table of what to ask for rather
 * than a palette: the two themes live in `src/muster.css` and switching between them is a matter
 * of reading the same names again. A name the stylesheet does not answer is left out instead of
 * defaulted here, which leaves xterm.js to draw that one with its own color rather than to paint it
 * with a value nothing else in the app agrees on.
 *
 * The background is in the theme rather than left to CSS because xterm.js paints it as an inline
 * style: the stylesheet has one declaration for the surface and out-specifies that one, rather than
 * pretending the canvas is not painted.
 */
const TERMINAL_THEME_TOKENS = {
  background: "--muster-content-bg-0",
  foreground: "--muster-content-text",
  cursor: "--muster-cursor",
  cursorAccent: "--muster-content-bg-0",
  selectionBackground: "--muster-selection",
  black: "--muster-ansi-black",
  red: "--muster-ansi-red",
  green: "--muster-ansi-green",
  yellow: "--muster-ansi-yellow",
  blue: "--muster-ansi-blue",
  magenta: "--muster-ansi-magenta",
  cyan: "--muster-ansi-cyan",
  white: "--muster-ansi-white",
  brightBlack: "--muster-ansi-bright-black",
  brightRed: "--muster-ansi-bright-red",
  brightGreen: "--muster-ansi-bright-green",
  brightYellow: "--muster-ansi-bright-yellow",
  brightBlue: "--muster-ansi-bright-blue",
  brightMagenta: "--muster-ansi-bright-magenta",
  brightCyan: "--muster-ansi-bright-cyan",
  brightWhite: "--muster-ansi-bright-white",
} as const;

/**
 * The palette as it is right now, for a terminal that is already on screen.
 *
 * A theme switch reaches a terminal through this and not through the terminal itself: xterm.js
 * repaints on `options.theme` and keeps its buffer, so the PTY behind it is never told anything and
 * no session is lost to a change of palette.
 */
export function musterTerminalTheme(): ITheme {
  const computed = getComputedStyle(document.documentElement);
  const theme: ITheme = {};
  for (const [name, token] of Object.entries(TERMINAL_THEME_TOKENS)) {
    const value = computed.getPropertyValue(token).trim();
    // Assigning by name rather than by index: `ITheme` carries properties of other types, so a
    // keyed write is the only one of the three that type-checks against the table above.
    if (value) Object.assign(theme, { [name]: value });
  }
  return theme;
}

/** Which renderer ended up on screen. `dom` is the one that always works. */
export type RendererLevel = "webgl" | "dom";

/**
 * The terminal, configured the way this panel wants it.
 *
 * `allowProposedApi` is on for the ligature joiner and the Unicode provider, which are both
 * experimental in 6.0; it is the price of the two, and it is why this function is the only
 * place a terminal is built.
 *
 * The scale is a parameter because the terminal's cell size has to be the size it is drawn at
 * before it is ever opened: a terminal built at the unscaled cell and scaled afterwards spends
 * its first frame measuring a grid that is already the wrong size. The size and the cursor are
 * preferences from `~/.muster/config.yml`, read here for the same reason; the ligatures are not,
 * because the joiner only exists once the terminal is on the page.
 *
 * The cursor's shape is a preference from the same file, and the shape it opens on is a block: a
 * cell filled with the theme's `--muster-cursor` and the glyph under it in the background, so the
 * caret is the one thing on the page drawn the other way round. Whether it blinks is the other
 * half of that choice, and a program that asks for another shape through DECSCUSR is answered out
 * of this one rather than fought with it.
 *
 * It is two colors and not one per cell (a cell that arrives with a color of its own does not
 * hand it to the block) and the panel that is not holding the keyboard draws a frame rather than
 * the focused shape, which says so in shape rather than in a color that has to differ from the
 * focused one.
 */
export function createMusterTerminal(
  fontSize = 16,
  cursorBlink = true,
  cursorStyle: TerminalCursorStyle = "block",
  zoom = 1,
): Terminal {
  const terminal = new Terminal({
    allowProposedApi: true,
    cursorBlink,
    cursorStyle,
    cursorInactiveStyle: "outline",
    fontFamily: TERMINAL_FONT_FAMILY,
    fontSize: terminalFontSize(fontSize, zoom),
    lineHeight: 1.2,
    scrollback: 10000,
    theme: musterTerminalTheme(),
  });
  // Widths and combining marks as Unicode 11 sees them, so emoji and CJK stop breaking the
  // grid that Neovim and the agent TUI draw their panels on. It has to be the active version
  // before the first render, so it cannot wait for `open`.
  terminal.loadAddon(new Unicode11Addon());
  terminal.unicode.activeVersion = "11";
  // xterm.js has no OSC 52 handler of its own, and without one the copy action in Neovim and in
  // the agent TUI does nothing at all.
  terminal.loadAddon(new ClipboardAddon());
  return terminal;
}

let terminalFonts: Promise<unknown> | undefined;

/**
 * All three bundled faces, and the one thing on the path to a drawn terminal that is not the
 * terminal's own work.
 *
 * xterm.js cannot measure its grid against a face it does not have, so a panel that opens
 * before these land draws its first frame from the fallback and lays the columns out wrong.
 * The files are local, but they are not small, and parsing them is the part of opening a
 * terminal a person waits on without being able to see why. The icon face is asked for on its
 * own rather than through the family list above, because a list is not a request: it says where
 * to look once a glyph turns up, and a glyph from the private use range only turns up in a
 * session that has already drawn its first frame at the width the fallback gave it.
 *
 * The request goes out when the app starts rather than when a panel mounts, and the answer is
 * kept: a panel that mounts later awaits a promise that is already settled, so the wait is a
 * microtask instead of a font parse. Callers still await it: the guarantee is that the faces
 * are ready before `open`, not that they were asked for early.
 */
export function preloadTerminalFonts(): Promise<unknown> {
  terminalFonts ??= Promise.allSettled([
    document.fonts.load(`16px ${TERMINAL_FONT_FAMILY}`),
    document.fonts.load(`700 16px ${TERMINAL_FONT_FAMILY}`),
    document.fonts.load(`16px "Muster Nerd Icons"`, ICON_PROBE),
  ]);
  return terminalFonts;
}

/**
 * The ligatures, which only xterm.js takes once the terminal is on the page: it throws
 * "Terminal must be opened first" before that, and a terminal that cannot be created at all is
 * a far worse outcome than one that draws `=>` as two characters.
 */
function registerLigatures(terminal: Terminal): number {
  try {
    return terminal.registerCharacterJoiner(ligatureRanges);
  } catch {
    // Nothing to announce: the panel still works, it just spells the sequences out. The id is
    // never handed out, so the joiner that was asked for does not exist to be taken back.
    return -1;
  }
}

/** The joiner id each live terminal has, so the preference can be switched and not only set. */
const ligatureJoiners = new WeakMap<Terminal, number>();

/**
 * Turns the ligatures on or off on a terminal that is already on screen.
 *
 * Turning them off deregisters the joiner rather than registering another: xterm.js keeps one at a
 * time, so a second request would silently replace the first and leave nothing to take back. The
 * panel is not recreated for this, which is why the setting reaches the terminals that were
 * already open and not only the next one to be drawn.
 */
export function setTerminalLigatures(terminal: Terminal, enabled: boolean): void {
  if (enabled === ligatureJoiners.has(terminal)) return;
  if (enabled) {
    const joiner = registerLigatures(terminal);
    // A terminal that refused the joiner has none to take back, and remembering a fake one would
    // deregister somebody else's.
    if (joiner >= 0) ligatureJoiners.set(terminal, joiner);
    return;
  }
  terminal.deregisterCharacterJoiner(ligatureJoiners.get(terminal)!);
  ligatureJoiners.delete(terminal);
}

/**
 * Copies whatever a selection gesture leaves selected, the way a text field does: a drag, and a
 * double or triple click, which xterm.js turns into a word or a line. A click that selects nothing
 * copies nothing.
 *
 * It also lets macOS users select text in applications that take over the terminal mouse.
 * Option-drag is xterm.js' force-selection gesture; it is enabled only while an application has
 * mouse tracking active, so the shell keeps its normal rectangular Option selection.
 */
export function enableTerminalSelectionCopy(
  terminal: Terminal,
  copy: (text: string) => void | Promise<void>,
): IDisposable {
  const element = terminal.element;
  if (!element) return { dispose: () => {} };

  const document = element.ownerDocument;
  let dragging = false;
  let moved = false;
  // xterm.js selects the word or the line on the second and third click of a multiple click, and
  // it does it before `mouseup`, so the selection is already there by the time this reads it.
  let multipleClick = false;

  const syncSelectionMode = () => {
    const forceSelection = terminal.modes.mouseTrackingMode !== "none";
    if (terminal.options.macOptionClickForcesSelection !== forceSelection) {
      terminal.options.macOptionClickForcesSelection = forceSelection;
    }
  };
  const onMouseDown = (event: MouseEvent) => {
    if (event.button !== 0) return;
    dragging = true;
    moved = false;
    multipleClick = event.detail > 1;
  };
  const onMouseMove = () => {
    if (dragging) moved = true;
  };
  const onMouseUp = () => {
    if (!dragging) return;
    dragging = false;
    if ((moved || multipleClick) && terminal.hasSelection()) void copy(terminal.getSelection());
  };

  syncSelectionMode();
  element.addEventListener("mousedown", onMouseDown);
  document.addEventListener("mousemove", onMouseMove);
  document.addEventListener("mouseup", onMouseUp);
  const writeParsed = terminal.onWriteParsed(syncSelectionMode);

  return {
    dispose: () => {
      element.removeEventListener("mousedown", onMouseDown);
      document.removeEventListener("mousemove", onMouseMove);
      document.removeEventListener("mouseup", onMouseUp);
      writeParsed.dispose();
    },
  };
}

/**
 * Who is waiting for a lost context on each terminal, keyed by terminal so a closed one takes its
 * listeners with it.
 *
 * The channel exists because the two ends are called in an order that leaves no room for a
 * callback: the panel attaches the renderer first, since the level it gets back is what decides
 * whether there is anything to watch at all, and only then asks for the watcher. Without it the
 * only thing that can say a context was lost is the window coming back, which is a guess.
 */
const contextLossListeners = new WeakMap<Terminal, Set<() => void>>();

/** Subscribes one watcher to a terminal's losses, and hands back the way out of it. */
function listenForContextLoss(terminal: Terminal, listener: () => void): IDisposable {
  const listeners = contextLossListeners.get(terminal) ?? new Set();
  listeners.add(listener);
  contextLossListeners.set(terminal, listeners);
  return {
    dispose: () => {
      listeners.delete(listener);
      if (listeners.size === 0) contextLossListeners.delete(terminal);
    },
  };
}

/**
 * The renderer, once the terminal is on the page and can be measured.
 *
 * WebGL, because a full-panel TUI repaints on every token and the DOM renderer pays for that in
 * spans. Anything that goes wrong leaves xterm.js' own renderer, which is what the panel drew
 * with before, so the worst case is the behaviour this app already had.
 */
export function attachTerminalRenderer(terminal: Terminal): RendererLevel {
  try {
    // The renderer reads xterm.js internals, so it is pinned to the version it was built
    // against (0.19 with 6.0) and has to move with it.
    const webgl = new WebglAddon();
    // WKWebView drops the context when the machine sleeps or under memory pressure, and letting
    // go of the addon puts the DOM renderer back in its place. The loss is published as well,
    // because that event is the only real evidence the machine is refusing a context: the addon
    // raises it only once the browser has failed to restore the one it had, and it cannot mean a
    // terminal that was never in trouble.
    webgl.onContextLoss(() => {
      webgl.dispose();
      contextLossListeners.get(terminal)?.forEach((listener) => listener());
    });
    terminal.loadAddon(webgl);
    return "webgl";
  } catch {
    return "dom";
  }
}

/**
 * How many times a terminal asks for WebGL again across its whole life.
 *
 * Three, and the number is the point rather than a detail: a machine that is out of memory loses
 * the context for the reason that asking for it again makes worse, so a terminal that kept trying
 * would spend the rest of the day acquiring contexts on every wake-up. What is left after three is
 * the DOM renderer, which is where the fallback already was and where it stays.
 */
export const MAX_RENDERER_RECOVERIES = 3;

/**
 * How long a loss waits before a window that is already on screen asks for a context again.
 *
 * A pause rather than the loss callback itself, because the machine that just refused a context
 * is the reason it refused: sleep and memory pressure are still true a frame later, so a context
 * asked for in that instant is asked for out of the same shortage. Long enough to be a pause,
 * short enough that a terminal left on the fallback is not what the reader sees when they come
 * back to it.
 */
const RENDERER_RECOVERY_DELAY_MS = 1000;

/**
 * Puts WebGL back on a terminal that lost it, at most `MAX_RENDERER_RECOVERIES` times.
 *
 * The addon is what says the context was lost, and the watcher only asks after that: a window
 * coming back says nothing at all about the renderer, so counting visibility changes as attempts
 * spent three of the three on terminals that were drawing perfectly, and the terminal that then
 * lost one for real had nothing left. An attempt here is a context asked for again.
 *
 * The recovery is a new addon rather than the old one, and that is not a detail: `WebglAddon` is
 * a disposable, so reloading one that has already been disposed leaves its renderer disposed on
 * arrival and the terminal on the fallback with no way back. The old one was disposed by the loss
 * itself, which is what puts xterm.js' own renderer back and takes the abandoned canvas off the
 * screen element, so nothing is left stacked up and nothing is disposed twice. A fresh one builds
 * a fresh `WebglRenderer`, and the render service swaps the DOM renderer out for it through the
 * same `setRenderer` call the addon already makes when it lets go.
 *
 * Waiting for the window to come back is what keeps this from thrashing: a context lost while the
 * window was hidden is usually restored by the time anybody looks at it, and the reader behind it
 * is producing output the whole time. A loss while it is already visible has no such moment to
 * wait for, and it gets the delay above instead — the recovery runs on its own rather than on the
 * next time the window happens to change, because a terminal that loses a context under memory
 * pressure can sit visible for the rest of the day and should not be on the fallback for all of
 * it. A terminal that never had WebGL is not recovering from a lost context and is not retried at
 * all, which leaves the existing fallback exactly as it was.
 */
export function watchTerminalRendererRecovery(
  terminal: Terminal,
  level: RendererLevel,
  attach: () => RendererLevel = () => attachTerminalRenderer(terminal),
): IDisposable {
  if (level !== "webgl") return { dispose: () => {} };
  let attempts = 0;
  // A context that was lost and has not been replaced yet. Only the addon's own event sets this,
  // which is what keeps a healthy terminal from spending an attempt on every wake-up.
  let lost = false;
  let retryTimer: ReturnType<typeof setTimeout> | undefined;

  const recover = () => {
    if (retryTimer !== undefined) clearTimeout(retryTimer);
    retryTimer = undefined;
    if (!lost) return;
    // Hidden is not a failure to recover, it is a renderer nothing is drawing: the listener below
    // runs this the moment the window is back, so waiting here costs the reader nothing and burns
    // none of the attempts above.
    if (document.visibilityState !== "visible") return;
    if (attempts >= MAX_RENDERER_RECOVERIES) {
      stop();
      return;
    }
    attempts += 1;
    lost = false;
    // A terminal that cannot have WebGL again stays on the renderer that always works, and this
    // stops asking: the answer did not change, and repeating it is what a recovery loop is.
    if (attach() === "dom") stop();
  };

  const onContextLoss = () => {
    lost = true;
    // Already visible, so nothing else is going to run the recovery, and one timer covers a
    // second loss that lands before it fires.
    if (document.visibilityState === "visible" && retryTimer === undefined) {
      retryTimer = setTimeout(recover, RENDERER_RECOVERY_DELAY_MS);
    }
  };

  const onVisibilityChange = () => {
    if (document.visibilityState === "visible") recover();
  };

  const stop = () => {
    document.removeEventListener("visibilitychange", onVisibilityChange);
    loss.dispose();
  };

  const loss = listenForContextLoss(terminal, onContextLoss);
  document.addEventListener("visibilitychange", onVisibilityChange);
  return {
    dispose: () => {
      // A pending recovery belongs to a terminal that is no longer on the page, so it goes with it
      // rather than waking up to ask for a context nothing is drawing.
      if (retryTimer !== undefined) clearTimeout(retryTimer);
      retryTimer = undefined;
      stop();
    },
  };
}
