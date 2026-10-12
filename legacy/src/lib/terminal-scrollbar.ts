/**
 * Where the thumb of Muster' own terminal scrollbar goes.
 *
 * xterm already draws a scrollbar, and this app hides it: the scrollable element that carries it
 * sits in the middle of the pane and the fit measures the grid against that box, so a scrollbar
 * that reserves its 10px is 10px the terminal has to give back as columns. The rules that hide it
 * are in `style.css` and the reason they exist is written there. What is left is a terminal that
 * can be scrolled (by wheel, by keyboard, by dragging a selection past the top) with nothing on
 * screen to say how much scrollback there is or where in it you are.
 *
 * So the scrollbar is drawn here instead, as an overlay that covers the right edge without
 * participating in layout. It cannot cost a column, it is not xterm's element to fight with, and
 * the arithmetic is the shared one in `overlay-scrollbar.ts` over the three numbers the DOM would
 * have given it anyway: the scrollback is the content, the visible rows are the viewport and
 * `viewportY` is how far it is scrolled. Only the names are the terminal's own, because a scrollback
 * is measured in lines rather than in pixels and this is the one caller that thinks in them.
 */

import { overlayScrollbarGeometry } from "./overlay-scrollbar";

export { scrollbarOffsetForTop } from "./overlay-scrollbar";

export interface TerminalScrollback {
  /** Every line the scrollback holds, the visible rows included. */
  length: number;
  /** The line the viewport starts at, counted from the top of the scrollback. */
  viewportY: number;
  /** How many lines the viewport shows. */
  rows: number;
}

/** The geometry is the shared one: a track of lines is a track of pixels. */
export type TerminalScrollbarGeometry = ReturnType<typeof overlayScrollbarGeometry>;

/**
 * The thumb for a viewport somewhere in a scrollback, drawn on a track of a known height.
 */
export function terminalScrollbarGeometry(
  scrollback: TerminalScrollback,
  trackHeight: number,
): TerminalScrollbarGeometry {
  return overlayScrollbarGeometry(
    {
      scrollTop: scrollback.viewportY,
      scrollHeight: scrollback.length,
      clientHeight: scrollback.rows,
    },
    trackHeight,
  );
}
