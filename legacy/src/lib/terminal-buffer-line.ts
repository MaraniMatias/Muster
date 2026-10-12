/**
 * One logical line out of the terminal buffer, with every character still pointing at the cell it
 * was drawn in.
 *
 * Three things stand between a string and the place it was typed, and all three matter for
 * putting an underline under a path:
 *
 * - **Rows are 1-based to xterm and 0-based to `getLine`.** The link provider is handed a 1-based
 *   row, so reading it without the offset reads the line below.
 * - **A wrapped line is several rows.** A long path wraps, and the character that continues it
 *   is on the next row, so a line that is read as one row is missing half the path.
 * - **Columns are not string indices.** A CJK glyph or an emoji occupies two cells and combining
 *   marks share one, so `text[8]` is not column 8. Every character therefore carries the cell it
 *   came from, which is the only way an underline lands where the glyph is.
 *
 * So this reads cells rather than a translated string, and the cost is a walk over one line: a
 * hover is the only thing that asks for it, and xterm asks once per line.
 */
import type { IBuffer } from "@xterm/xterm";

/** Where one character of a logical line was drawn. Both are 0-based buffer coordinates. */
export interface LineCell {
  row: number;
  col: number;
  /** How many cells the character occupies: 1 normally, 2 for a wide glyph. */
  width: number;
}

export interface LogicalLine {
  /** The line as one string, however many rows it wraps across. */
  text: string;
  /** The cell each character of `text` was drawn in, in the same order. */
  cells: LineCell[];
}

/**
 * The logical line the 1-based row belongs to, wrapped rows and all.
 *
 * The walk goes back first because a wrapped row is the tail of something that started above it,
 * and that something is the line a person is looking at. The walk stops at the first row that did
 * not come from a wrap, and at the first row below that does not continue one.
 */
export function readLogicalLine(buffer: IBuffer, row: number): LogicalLine {
  const index = row - 1;
  if (index < 0) return { text: "", cells: [] };

  let first = index;
  while (first > 0 && buffer.getLine(first)?.isWrapped) first -= 1;

  const text: string[] = [];
  const cells: LineCell[] = [];
  for (let y = first; ; y += 1) {
    const line = buffer.getLine(y);
    // A row that does not exist ends the line: the buffer is the only thing that says where the
    // scrollback stops, and asking past it returns nothing rather than throwing.
    if (!line) break;
    appendRow(line, y, text, cells);
    if (!buffer.getLine(y + 1)?.isWrapped) break;
  }
  return { text: text.join(""), cells };
}

function appendRow(
  line: NonNullable<ReturnType<IBuffer["getLine"]>>,
  row: number,
  text: string[],
  cells: LineCell[],
): void {
  for (let x = 0; x < line.length; x += 1) {
    const cell = line.getCell(x);
    if (!cell) continue;
    const width = cell.getWidth();
    // The trailing half of a wide glyph holds no character of its own; the leading half already
    // contributed the whole glyph, and counting this one would shift every column after it.
    if (width === 0) continue;
    const chars = cell.getChars();
    if (!chars) continue;
    for (const char of chars) {
      text.push(char);
      cells.push({ row, col: x, width });
    }
  }
  // The padding to the right of the last glyph is not part of the line, and a path that ran into
  // it was never one. Trailing spaces only: a space inside a quoted path is part of the name.
  while (text.length > 0 && text[text.length - 1] === " ") {
    text.pop();
    cells.pop();
  }
}

/**
 * The cells a run of characters covers, one entry per row it touches.
 *
 * A path that wraps comes back as several runs, because one decoration is one row: xterm anchors
 * a decoration to a single marker, so a line-spanning underline would need one per row and the
 * cell geometry is not something to guess twice.
 */
export function cellRuns(cells: LineCell[], start: number, end: number): { row: number; from: number; to: number }[] {
  const runs: { row: number; from: number; to: number }[] = [];
  for (let index = start; index < end && index < cells.length; index += 1) {
    const cell = cells[index];
    const last = runs[runs.length - 1];
    if (last && last.row === cell.row) {
      last.to = cell.col + cell.width;
      continue;
    }
    runs.push({ row: cell.row, from: cell.col, to: cell.col + cell.width });
  }
  return runs;
}
