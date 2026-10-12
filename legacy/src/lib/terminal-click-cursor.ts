/**
 * A plain click on a shell prompt puts the shell's own cursor where the click was, so the next
 * character is typed there instead of at the end of the line.
 *
 * A terminal is not a text field. Everything typed goes to whatever is reading the PTY, and a shell
 * only knows where its cursor is because the keys that moved it were sent. A click is a mouse event
 * that never reached the shell at all — xterm turns it into a selection, which is the right thing
 * for output and useless for a command line. So the click is measured here and answered with the
 * keys a person pressing the arrow keys would have sent, which is the only thing the shell's line
 * editor understands. It is what alt+click already does in xterm.js, without the modifier: nobody
 * holds alt to place a cursor in the command they are writing.
 *
 * Which cells may be clicked is the shell's answer and not this one's. OSC 133's `B` marker rides at
 * the end of `PS1`, so the cell the terminal was parsing it on is where the editable line begins, and
 * the cursor is the other end of it: a click between those two is inside the command being written,
 * and a click anywhere else is on a prompt or on output, which is left alone. The same markers keep
 * this away from the applications that already handle clicks themselves. The `C` marker arrives
 * before a command runs, so nothing is editable once one starts, and an editor running behind the
 * prompt — vim, an OpenCode session — turns on mouse tracking as well. Neither is asked what the
 * shell thinks it is doing; both say the same thing, which is that this line is not being edited
 * here.
 *
 * The line's start is held in a marker rather than as a row number, because the buffer scrolls and
 * reflows under a prompt that is being typed at, and a row number would quietly come to name a
 * different line once it did.
 *
 * No Vue and no IPC: the only thing this asks of the session is where to write, and the only thing
 * it reads is the terminal it was handed.
 */
import type { IDisposable, IBuffer, IMarker, Terminal } from "@xterm/xterm";

/**
 * How far the mouse may travel between the press and the release and still count as a click.
 *
 * A hand is not a mouse button: a click lands a pixel or two off, and a drag that ends where it
 * began is still a drag. Three pixels is under what a click wanders and under what a selection of
 * any length moves.
 */
const DRAG_SLOP_PX = 3;

/** The arrow keys a shell's line editor moves on, spelled as every shell expects to receive them. */
const RIGHT = "\u001b[C";
const LEFT = "\u001b[D";

export interface ClickToMoveCursor extends IDisposable {
  /**
   * The shell printed a prompt and is reading a line: the cursor is where that line starts being
   * editable.
   *
   * Called from the shell-integration handler rather than on a timer, because the cell it marks is
   * the one the terminal's cursor was on as the marker was parsed — the prompt keeps printing after
   * it, and anything asked later reads a cursor that has already moved on.
   */
  inputStarted(): void;
  /** A command started, so nothing left on screen belongs to a line being edited. */
  commandStarted(): void;
}

interface Cell {
  /** A row of the buffer, counted from its first line rather than from the viewport. */
  row: number;
  col: number;
}

/**
 * Puts the shell's cursor where a plain click was.
 *
 * `send` is the session's own input queue rather than the terminal's `onData`, so what this writes
 * is queued, accounted for and refused in the same order as a keystroke, and a session that has
 * gone away takes the click with it.
 */
export function enableClickToMoveCursor(terminal: Terminal, send: (data: string) => void): ClickToMoveCursor {
  let marker: IMarker | null = null;
  // The column the prompt ended on, kept beside the marker because a marker's line moves with the
  // buffer and its column cannot: only the shell knows how wide its own prompt was. A resize moves
  // it, and the shell repairs it without being asked — its line editor redraws the whole line
  // after a resize, which prints the prompt again and says where the input starts all over.
  let inputColumn = 0;
  // Where a press is, and whether it has already become a drag. Only the release position is not
  // enough: a drag that comes back to where it started ends where it began and is still a drag.
  let pressedAt: { x: number; y: number; dragged: boolean } | null = null;

  const forget = (): void => {
    marker?.dispose();
    marker = null;
  };

  const inputStarted = (): void => {
    forget();
    const buffer = terminal.buffer.active;
    // The alternate screen has no scrollback and no prompt behind it, so a marker there would
    // outlive the line it names. It is where an editor runs, and those do not come through here.
    if (buffer.type !== "normal") return;
    // The terminal's own marker, registered on the row its cursor is on, which is what the marker
    // follows as the buffer scrolls and reflows under a prompt being typed at.
    marker = terminal.registerMarker();
    inputColumn = buffer.cursorX;
  };

  const onMouseDown = (event: MouseEvent): void => {
    // Only the button a click is. Anything else is a middle-click paste or a context menu, both of
    // which the terminal already answers.
    pressedAt = event.button === 0 ? { x: event.clientX, y: event.clientY, dragged: false } : null;
  };

  /** Watched on the way, because a selection that returns to where it started is still one. */
  const onMouseMove = (event: MouseEvent): void => {
    if (!pressedAt || pressedAt.dragged) return;
    if (Math.hypot(event.clientX - pressedAt.x, event.clientY - pressedAt.y) > DRAG_SLOP_PX) {
      pressedAt.dragged = true;
    }
  };

  const onMouseUp = (event: MouseEvent): void => {
    const pressed = pressedAt;
    pressedAt = null;
    if (!pressed || event.button !== 0) return;
    // A second click is a double click, and a double click selects a word. That is what the second
    // click is, and the word under the pointer is what the person asked for.
    if (event.detail > 1) return;
    // Every modifier means the click was something else: ctrl and cmd open a path (see
    // `terminal-file-links.ts`), shift extends a selection, and alt is xterm.js' own cursor move.
    if (event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
    if (pressed.dragged) return;
    // An application that asked for the mouse is drawing its own thing and reads clicks itself.
    if (terminal.modes.mouseTrackingMode !== "none") return;
    const moves = cursorMoves(terminal, marker?.line, inputColumn, event);
    if (moves === null) return;
    send(moves);
    // The selection this click just started is not what was asked for, and leaving it means the
    // next keystroke replaces whatever was selected before the click.
    terminal.clearSelection();
  };

  const element = terminal.element;
  element?.addEventListener("mousedown", onMouseDown);
  // On the document rather than the element: a drag that ends outside the terminal is a drag, and so
  // is the release of a click that walked out of it and came back.
  element?.ownerDocument.addEventListener("mousemove", onMouseMove);
  element?.ownerDocument.addEventListener("mouseup", onMouseUp);

  return {
    inputStarted,
    commandStarted: forget,
    dispose() {
      element?.removeEventListener("mousedown", onMouseDown);
      element?.ownerDocument.removeEventListener("mousemove", onMouseMove);
      element?.ownerDocument.removeEventListener("mouseup", onMouseUp);
      forget();
    },
  };
}

/**
 * The keys that move the shell's cursor from where it is to the cell under a click, or `null` when
 * that cell is not somewhere the cursor can go.
 *
 * `inputRow` is the buffer row the prompt ended on and `inputColumn` the cell on it. A row of
 * `undefined` is the answer for every terminal that has not reported a prompt ending: a shell this
 * does not know how to ask, a prompt that has not been printed yet, or a line that has scrolled out
 * of the scrollback and gone with its marker.
 */
function cursorMoves(
  terminal: Terminal,
  inputRow: number | undefined,
  inputColumn: number,
  event: MouseEvent,
): string | null {
  if (inputRow === undefined) return null;
  const buffer = terminal.buffer.active;
  if (buffer.type !== "normal") return null;

  const clicked = cellUnder(terminal, event);
  if (!clicked) return null;
  const cursor: Cell = { row: buffer.baseY + buffer.cursorY, col: buffer.cursorX };

  // Left of where the line begins is the prompt, and past the end of the line is where the shell
  // will be after the next character rather than where it is now: neither is a position the cursor
  // is at, and moving it there would answer a click with a jump nobody asked for.
  if (compare(clicked, { row: inputRow, col: inputColumn }) < 0) return null;
  // Only a click ahead of the cursor needs the line end. Empty tab-fill cells do not end a line
  // when later text follows; cellsBetween rejects any movement across those ambiguous columns.
  const lineEnd =
    compare(clicked, cursor) > 0 ? endOfLine(buffer, terminal.cols, cursor, buffer.baseY + terminal.rows - 1) : null;
  if (lineEnd && compare(clicked, lineEnd) > 0) return null;

  const target = buffer.getLine(clicked.row)?.getCell(clicked.col);
  if (!target) return null;
  // A blank cell is a valid caret at the exact line end, but not in the middle of a tab-fill span.
  if (target.getWidth() !== 0 && !target.getChars() && (!lineEnd || compare(clicked, lineEnd) !== 0)) return null;
  // Walked from the cursor towards the click, because that is the direction the keys go in.
  const steps = cellsBetween(buffer, terminal.cols, cursor, clicked);
  if (steps === null || steps === 0) return null;
  return steps < 0 ? LEFT.repeat(-steps) : RIGHT.repeat(steps);
}

/** Where one cell is in the buffer relative to another. */
function compare(left: Cell, right: Cell): number {
  return left.row - right.row || left.col - right.col;
}

/**
 * The cell just past the last character of the line the cursor is on, wherever that line ends.
 *
 * A click to the right of the cursor is inside the command only when there is command to the right
 * of the cursor, which is what a cursor looks like after somebody has moved it back along a line
 * they have already typed. Everything past the last character is where the next keystroke is going
 * to land, and the shell's cursor stops there, so a click out there is a click on nothing.
 *
 * The line is walked rather than its row read off, because a command longer than the terminal is
 * wide carries on into the rows below: stopping at the end of the cursor's own row would refuse
 * every click on the rest of the command, which is most of it on a narrow window.
 */
function endOfLine(buffer: IBuffer, cols: number, cursor: Cell, lastRow: number): Cell {
  for (let row = cursor.row; row <= lastRow; row += 1) {
    const line = buffer.getLine(row);
    for (let col = row === cursor.row ? cursor.col : 0; col < cols; col += 1) {
      const cell = line?.getCell(col);
      // The second half of a wide glyph holds nothing of its own and is not the end of anything.
      if (cell?.getWidth() === 0) continue;
      if (!cell?.getChars()) {
        // xterm leaves tab-fill cells empty; if text follows on this row, keep scanning for the end.
        let nextCol = col + 1;
        while (nextCol < cols) {
          const next = line?.getCell(nextCol);
          if (next?.getWidth() !== 0 && next?.getChars()) break;
          nextCol += 1;
        }
        if (nextCol < cols) {
          col = nextCol - 1;
          continue;
        }
        return { row, col };
      }
    }
  }
  return { row: lastRow, col: cols };
}

/**
 * The cell a click landed on, in buffer rows.
 *
 * xterm.js has no public way to turn a pixel into a cell — the one it uses reads the renderer's
 * measured cell size, which is not in its API — but both renderers do keep the grid in one element
 * sized to exactly `cols` by `rows` cells, so a click is a fraction of that box. It is the same
 * arithmetic the renderer used to put the glyph there in the first place.
 */
function cellUnder(terminal: Terminal, event: MouseEvent): Cell | null {
  const screen = terminal.element?.querySelector<HTMLElement>(".xterm-screen");
  if (!screen || terminal.cols < 1 || terminal.rows < 1) return null;
  const rect = screen.getBoundingClientRect();
  if (rect.width <= 0 || rect.height <= 0) return null;
  const x = event.clientX - rect.left;
  const y = event.clientY - rect.top;
  if (x < 0 || x >= rect.width || y < 0 || y >= rect.height) return null;
  const col = Math.floor((x / rect.width) * terminal.cols);
  const row = Math.floor((y / rect.height) * terminal.rows);
  // Visible row zero is the viewport's absolute buffer row, independent of where the buffer's
  // bottom (baseY) is; this stays correct both at the bottom and while scrolled into history.
  return { row: terminal.buffer.active.viewportY + row, col };
}

/**
 * How many keys it takes to walk from one cell to another, negative when the answer is to the left.
 *
 * Cells, not columns: the trailing half of a wide glyph is not a character the line editor holds,
 * and counting it puts the cursor one place past where it was clicked. xterm leaves tab-fill cells
 * empty and does not expose their origin, so an empty cell in the span makes the mapping ambiguous.
 */
function cellsBetween(buffer: IBuffer, cols: number, from: Cell, to: Cell): number | null {
  // Walked from whichever cell comes first in the buffer, because the cells between two cells are
  // the same however they are approached, and a click above the cursor — on a line that wrapped —
  // is the ordinary case here rather than the one that could be refused.
  const forward = compare(from, to) < 0;
  const first = forward ? from : to;
  const last = forward ? to : from;
  let steps = 0;
  for (let row = first.row; row <= last.row; row += 1) {
    const line = buffer.getLine(row);
    if (!line) continue;
    const start = row === first.row ? first.col : 0;
    const end = row === last.row ? last.col : cols;
    for (let col = start; col < end; col += 1) {
      const cell = line.getCell(col);
      // A width of 0 is the cell a wide glyph's second half leaves behind; it holds no character.
      if (!cell) return null;
      if (cell.getWidth() === 0) continue;
      if (!cell.getChars()) return null;
      steps += 1;
    }
  }
  return forward ? steps : -steps;
}
