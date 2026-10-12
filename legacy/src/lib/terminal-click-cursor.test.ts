// @vitest-environment happy-dom
import { describe, expect, it, vi } from "vitest";
import type { IBuffer, Terminal } from "@xterm/xterm";
import { Terminal as Headless } from "@xterm/headless";
import { enableClickToMoveCursor } from "./terminal-click-cursor";

/**
 * A terminal holding a shell at its prompt: rows of text, a cursor, and a box the click is measured
 * against.
 *
 * The rows are the part worth building by hand, because what a click lands on is read out of them:
 * where the prompt ends, where the command ends, and which cells a wide glyph left behind. Nothing
 * here renders — the module only reads the buffer, so a buffer is enough of a terminal.
 *
 * `rows` holds one string per buffer row, and `wide` the columns that start a two-cell glyph, whose
 * second cell is then blank and belongs to nothing.
 */
function fakeTerminal(
  options: {
    rows?: string[];
    wide?: number[];
    cursor?: { row: number; col: number };
    cols?: number;
    gridRows?: number;
    baseY?: number;
    viewportY?: number;
    type?: "normal" | "alternate";
  } = {},
) {
  const cols = options.cols ?? 10;
  const gridRows = options.gridRows ?? 4;
  const wide = options.wide ?? [];
  const rows = options.rows ?? Array.from({ length: gridRows }, () => "");
  const element = document.createElement("div");
  const screen = document.createElement("div");
  screen.className = "xterm-screen";
  element.appendChild(screen);
  // The box is exactly as many cells wide and tall as the terminal says it has, which is the whole
  // of what a click is measured against: ten pixels to the cell in each direction.
  screen.getBoundingClientRect = () => ({ left: 0, top: 0, width: cols * 10, height: gridRows * 10 }) as DOMRect;
  document.body.appendChild(element);

  const active = {
    type: options.type ?? ("normal" as "normal" | "alternate"),
    cursorX: options.cursor?.col ?? 0,
    cursorY: options.cursor?.row ?? 0,
    baseY: options.baseY ?? 0,
    viewportY: options.viewportY ?? 0,
    getLine: (row: number) =>
      row < 0 || row >= rows.length
        ? undefined
        : {
            getCell: (col: number) => ({
              getWidth: () => (wide.includes(col) ? 2 : wide.includes(col - 1) ? 0 : 1),
              getChars: () => rows[row][col] ?? "",
            }),
          },
  };
  // A marker is a row that moves with the buffer, so it is registered the way xterm.js registers
  // one: on the row the cursor is on at that moment.
  const marker = { line: undefined as number | undefined, dispose: vi.fn() };
  const buffer = { active };
  const terminal = {
    element,
    cols,
    rows: gridRows,
    modes: { mouseTrackingMode: "none" as string },
    cleared: 0,
    buffer,
    registerMarker() {
      marker.line = buffer.active.baseY + buffer.active.cursorY;
      return marker;
    },
    clearSelection() {
      terminal.cleared += 1;
    },
  };
  return { terminal: terminal as unknown as Terminal, raw: terminal, marker, element };
}

/** A click on one cell of the grid, which is where the cell under the pointer is read from. */
function click(element: HTMLElement, row: number, col: number, extra: MouseEventInit = {}) {
  const at = { clientX: col * 10 + 5, clientY: row * 10 + 5 };
  // Both halves of a click name the same cell: a press and a release that disagree is a drag.
  element.dispatchEvent(new MouseEvent("mousedown", { button: 0, ...at, ...extra }));
  document.dispatchEvent(new MouseEvent("mouseup", { button: 0, ...at, ...extra }));
}

const LEFT = "\u001b[D";
const RIGHT = "\u001b[C";

describe("enableClickToMoveCursor", () => {
  it("moves the cursor left by one key per cell between the click and the cursor", () => {
    // The prompt ends at column 2 of the second row, and four characters are typed after it.
    const { terminal, raw, element } = fakeTerminal({
      rows: ["", "> cd foo", ""],
      cursor: { row: 1, col: 2 },
    });
    const send = vi.fn();
    const registration = enableClickToMoveCursor(terminal, send);

    registration.inputStarted();
    raw.buffer.active.cursorX = 6;

    click(element, 1, 4);

    expect(send).toHaveBeenCalledWith(LEFT.repeat(2));
    registration.dispose();
  });

  it("moves to the exact blank cell after an unambiguous command end", () => {
    const { terminal, element } = fakeTerminal({
      rows: ["> abc"],
      cursor: { row: 0, col: 2 },
      gridRows: 1,
    });
    const send = vi.fn();
    const registration = enableClickToMoveCursor(terminal, send);

    registration.inputStarted();
    click(element, 0, 5);

    expect(send).toHaveBeenCalledWith(RIGHT.repeat(3));
    registration.dispose();
  });

  it("moves right when the click is to the right of a cursor that is not at the end of the line", () => {
    const { terminal, raw, element } = fakeTerminal({
      rows: ["", "> abcdefghij"],
      cursor: { row: 1, col: 2 },
    });
    const send = vi.fn();
    const registration = enableClickToMoveCursor(terminal, send);

    registration.inputStarted();
    // Two arrow keys back along a line that has already been typed to the end of the row.
    raw.buffer.active.cursorX = 4;
    click(element, 1, 6);

    expect(send).toHaveBeenCalledWith(RIGHT.repeat(2));
    registration.dispose();
  });

  it("counts a wide glyph once rather than once per cell", () => {
    // Columns 4 and 5 are one CJK character, so seven cells hold six characters.
    const { terminal, raw, element } = fakeTerminal({
      rows: ["", "> abcd本f"],
      wide: [5],
      cursor: { row: 1, col: 0 },
    });
    const send = vi.fn();
    const registration = enableClickToMoveCursor(terminal, send);

    registration.inputStarted();
    raw.buffer.active.cursorX = 7;
    click(element, 1, 0);

    expect(send).toHaveBeenCalledWith(LEFT.repeat(6));
    registration.dispose();
  });

  it("moves across a line that wrapped, where the answer is more cells than one row holds", () => {
    // The input starts at the top of row 1 and the cursor has wrapped onto row 2.
    const { terminal, raw, element } = fakeTerminal({
      cols: 6,
      rows: ["", "> abcdef", "ghijkl", ""],
      cursor: { row: 1, col: 0 },
    });
    const send = vi.fn();
    const registration = enableClickToMoveCursor(terminal, send);

    registration.inputStarted();
    raw.buffer.active.cursorX = 2;
    raw.buffer.active.cursorY = 2;
    click(element, 1, 2);

    // Columns 2 to 5 of row 1 and columns 0 and 1 of row 2: six keys back.
    expect(send).toHaveBeenCalledWith(LEFT.repeat(6));
    registration.dispose();
  });

  it("reaches the rows a wrapped command carries on into, below the cursor", () => {
    // The cursor has been moved back into the first row of a command that wraps twice.
    const { terminal, raw, element } = fakeTerminal({
      cols: 6,
      rows: ["", "> abcdef", "ghijkl", "mnopqr", ""],
      cursor: { row: 1, col: 0 },
    });
    const send = vi.fn();
    const registration = enableClickToMoveCursor(terminal, send);

    registration.inputStarted();
    raw.buffer.active.cursorX = 2;
    click(element, 3, 3);

    // Row 1 from column 2, all six of row 2, and the first three of row 3: thirteen keys forward.
    expect(send).toHaveBeenCalledWith(RIGHT.repeat(13));
    registration.dispose();
  });

  it("maps clicks to absolute rows at the bottom of nonempty scrollback", () => {
    const { terminal, raw, element } = fakeTerminal({
      rows: ["", "", "", "", "", "", "output", "history", "> abcdef", ""],
      cursor: { row: 2, col: 2 },
      baseY: 6,
      viewportY: 6,
    });
    const send = vi.fn();
    const registration = enableClickToMoveCursor(terminal, send);

    registration.inputStarted();
    raw.buffer.active.cursorX = 6;
    click(element, 2, 4);
    expect(send).toHaveBeenCalledWith(LEFT.repeat(2));

    send.mockClear();
    click(element, 0, 1);
    expect(send).not.toHaveBeenCalled();
    registration.dispose();
  });

  it("maps scrolled-up clicks to viewport rows and leaves visible output alone", () => {
    const { terminal, raw, element } = fakeTerminal({
      cols: 4,
      rows: ["", "", "", "log", "old", "stuff", "> ab", "cdef", "ghij", "klmn"],
      cursor: { row: 0, col: 2 },
      baseY: 6,
      viewportY: 6,
    });
    const send = vi.fn();
    const registration = enableClickToMoveCursor(terminal, send);

    registration.inputStarted();
    const buffer = raw.buffer.active;
    buffer.baseY = 8;
    buffer.viewportY = 3;
    buffer.cursorY = 1;
    buffer.cursorX = 2;

    // Screen row 3 is absolute buffer row 6 (input); baseY is not part of this conversion.
    click(element, 3, 3);
    expect(send).toHaveBeenCalledWith(LEFT.repeat(11));

    send.mockClear();
    click(element, 0, 0);
    expect(send).not.toHaveBeenCalled();
    registration.dispose();
  });

  it("leaves the prompt, the output above it and everything past the cursor alone", () => {
    const { terminal, raw, element } = fakeTerminal({
      rows: ["last week:", "> cd foo", ""],
      cursor: { row: 2, col: 4 },
    });
    const send = vi.fn();
    const registration = enableClickToMoveCursor(terminal, send);

    registration.inputStarted();
    raw.buffer.active.cursorX = 6;

    click(element, 2, 3); // inside the prompt
    click(element, 1, 5); // the output above the line
    click(element, 0, 0); // the oldest line on screen
    click(element, 2, 7); // past the cursor, where the next character will go

    expect(send).not.toHaveBeenCalled();
    registration.dispose();
  });

  it("does nothing until the shell has said where its input starts", () => {
    const { terminal, raw, element } = fakeTerminal({
      rows: ["", "> abcdef"],
      cursor: { row: 1, col: 0 },
    });
    const send = vi.fn();
    const registration = enableClickToMoveCursor(terminal, send);

    click(element, 1, 0);
    expect(send).not.toHaveBeenCalled();

    // And nothing again once a command has started, which is what a terminal running an editor does
    // between its own prompt and the cursor it would move for itself.
    registration.inputStarted();
    raw.buffer.active.cursorX = 4;
    registration.commandStarted();
    click(element, 1, 0);

    expect(send).not.toHaveBeenCalled();
    registration.dispose();
  });

  it("stays out of the way of a drag, a double click, a modifier and a mouse an application owns", () => {
    const { terminal, raw, element } = fakeTerminal({
      rows: ["", "> abcdef"],
      cursor: { row: 1, col: 0 },
    });
    const send = vi.fn();
    const registration = enableClickToMoveCursor(terminal, send);

    registration.inputStarted();
    raw.buffer.active.cursorX = 6;

    element.dispatchEvent(new MouseEvent("mousedown", { button: 0 }));
    document.dispatchEvent(new MouseEvent("mouseup", { button: 0, clientX: 400, clientY: 5 }));
    click(element, 1, 0, { detail: 2 });
    click(element, 1, 0, { ctrlKey: true });
    click(element, 1, 0, { metaKey: true });
    click(element, 1, 0, { shiftKey: true });
    click(element, 1, 0, { altKey: true });
    element.dispatchEvent(new MouseEvent("mousedown", { button: 2 }));
    document.dispatchEvent(new MouseEvent("mouseup", { button: 2, clientX: 5, clientY: 15 }));

    expect(send).not.toHaveBeenCalled();

    // The same click, once the program behind the PTY has taken the mouse for itself.
    raw.modes.mouseTrackingMode = "vt200";
    click(element, 1, 0);
    expect(send).not.toHaveBeenCalled();

    raw.modes.mouseTrackingMode = "none";
    click(element, 1, 0);
    expect(send).toHaveBeenCalledWith(LEFT.repeat(6));
    registration.dispose();
  });

  it("does not move the cursor for a drag that comes back to where it started", () => {
    // A selection from column 0 to column 6 and back is a selection, and the release lands on the
    // cell the press did: comparing only those two positions would read it as a click.
    const { terminal, raw, element } = fakeTerminal({
      rows: ["", "> abcdef"],
      cursor: { row: 1, col: 0 },
    });
    const send = vi.fn();
    const registration = enableClickToMoveCursor(terminal, send);

    registration.inputStarted();
    raw.buffer.active.cursorX = 6;

    element.dispatchEvent(new MouseEvent("mousedown", { button: 0, clientX: 5, clientY: 15 }));
    document.dispatchEvent(new MouseEvent("mousemove", { buttons: 1, clientX: 65, clientY: 15 }));
    document.dispatchEvent(new MouseEvent("mousemove", { buttons: 1, clientX: 5, clientY: 15 }));
    document.dispatchEvent(new MouseEvent("mouseup", { button: 0, clientX: 5, clientY: 15 }));

    expect(send).not.toHaveBeenCalled();
    registration.dispose();
  });

  it("clears the selection the click started, so the next key does not replace what was selected", () => {
    const { terminal, raw, element } = fakeTerminal({
      rows: ["", "> abcd"],
      cursor: { row: 1, col: 0 },
    });
    const registration = enableClickToMoveCursor(terminal, vi.fn());

    registration.inputStarted();
    raw.buffer.active.cursorX = 4;
    click(element, 1, 0);

    expect(raw.cleared).toBe(1);
    registration.dispose();
  });

  it("does not move a cursor on the alternate screen, where there is no prompt behind it", () => {
    const { terminal, raw, element } = fakeTerminal({ cursor: { row: 1, col: 0 }, type: "alternate" });
    const send = vi.fn();
    const registration = enableClickToMoveCursor(terminal, send);

    registration.inputStarted();
    raw.buffer.active.cursorX = 4;
    click(element, 1, 0);

    expect(send).not.toHaveBeenCalled();
    registration.dispose();
  });

  it("takes its listeners and its marker back when the registration is disposed", () => {
    const { terminal, raw, marker, element } = fakeTerminal({ cursor: { row: 1, col: 0 } });
    const send = vi.fn();
    const registration = enableClickToMoveCursor(terminal, send);

    registration.inputStarted();
    raw.buffer.active.cursorX = 4;
    registration.dispose();
    expect(marker.dispose).toHaveBeenCalled();

    click(element, 1, 0);
    expect(send).not.toHaveBeenCalled();
  });

  it("refuses ambiguous tab cells from a real xterm headless buffer", async () => {
    const headless = new Headless({ cols: 16, rows: 3, allowProposedApi: true });
    await new Promise<void>((resolve) => headless.write("> ", resolve));
    const { terminal, element } = fakeTerminal({ cols: 16, gridRows: 3 });
    (terminal.buffer as unknown as { active: IBuffer }).active = headless.buffer.active;
    const send = vi.fn();
    const registration = enableClickToMoveCursor(terminal, send);

    registration.inputStarted();
    await new Promise<void>((resolve) => headless.write("ab\tcd", resolve));
    const line = headless.buffer.active.getLine(0);
    expect(Array.from({ length: 10 }, (_, col) => line?.getCell(col)?.getChars())).toEqual([
      ">",
      " ",
      "a",
      "b",
      "",
      "",
      "",
      "",
      "c",
      "d",
    ]);
    expect(line?.getCell(4)?.getWidth()).toBe(1);

    // The public buffer exposes tab fill as empty cells with no way to identify their source.
    click(element, 0, 3);
    click(element, 0, 5);
    expect(send).not.toHaveBeenCalled();

    // Positions before and after the tab still work when the move does not cross its blank span.
    await new Promise<void>((resolve) => headless.write("\u001b[6D", resolve));
    expect(headless.buffer.active.cursorX).toBe(4);
    click(element, 0, 3);
    expect(send).toHaveBeenCalledWith(LEFT);

    // A move across the tab is ambiguous even though text follows its fill cells.
    send.mockClear();
    click(element, 0, 8);
    expect(send).not.toHaveBeenCalled();

    await new Promise<void>((resolve) => headless.write("\u001b[4C", resolve));
    expect(headless.buffer.active.cursorX).toBe(8);
    click(element, 0, 9);
    expect(send).toHaveBeenCalledWith(RIGHT);

    // The blank after the visible text is an endpoint, but reaching it crosses the tab's fill span.
    send.mockClear();
    await new Promise<void>((resolve) => headless.write("\u001b[6D", resolve));
    expect(headless.buffer.active.cursorX).toBe(2);
    click(element, 0, 10);
    expect(send).not.toHaveBeenCalled();

    registration.dispose();
    headless.dispose();
  });

  it("counts real typed spaces as shell cursor positions", async () => {
    const headless = new Headless({ cols: 16, rows: 3, allowProposedApi: true });
    await new Promise<void>((resolve) => headless.write("> ", resolve));
    const { terminal, element } = fakeTerminal({ cols: 16, gridRows: 3 });
    (terminal.buffer as unknown as { active: IBuffer }).active = headless.buffer.active;
    const send = vi.fn();
    const registration = enableClickToMoveCursor(terminal, send);

    registration.inputStarted();
    await new Promise<void>((resolve) => headless.write("a b", resolve));
    expect(headless.buffer.active.getLine(0)?.getCell(3)?.getChars()).toBe(" ");
    click(element, 0, 2);

    expect(send).toHaveBeenCalledWith(LEFT.repeat(3));
    registration.dispose();
    headless.dispose();
  });

  it("rejects mouseup coordinates outside the terminal grid instead of clamping them to an edge cell", () => {
    const { terminal, raw, element } = fakeTerminal({
      rows: ["", "> abcdefgh"],
      cursor: { row: 1, col: 0 },
    });
    const send = vi.fn();
    const registration = enableClickToMoveCursor(terminal, send);

    registration.inputStarted();
    raw.buffer.active.cursorX = 4;
    element.dispatchEvent(new MouseEvent("mousedown", { button: 0, clientX: 96, clientY: 15 }));
    document.dispatchEvent(new MouseEvent("mouseup", { button: 0, clientX: 100, clientY: 15 }));

    expect(send).not.toHaveBeenCalled();
    registration.dispose();
  });
});
