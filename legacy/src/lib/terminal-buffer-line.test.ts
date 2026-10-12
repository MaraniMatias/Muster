import { describe, expect, it } from "vitest";
import { cellRuns, readLogicalLine } from "./terminal-buffer-line";
import type { IBuffer } from "@xterm/xterm";

/**
 * A buffer written by hand, so the coordinates under test are the ones xterm would produce rather
 * than the ones a mock guessed. Rows are 0-based here because that is what `getLine` takes, which
 * is the whole point the tests are about.
 */
/**
 * A cell as xterm holds it. A plain string is the common one: one character in one cell. A wide
 * glyph is the case the tests exist for, and it is two cells — the glyph and the empty one that
 * follows it — which is why the width is spelled out rather than guessed.
 */
type Cell = string | { chars: string; width: number };

class FakeLine {
  constructor(
    readonly cells: Cell[],
    readonly isWrapped = false,
  ) {}
  get length() {
    return this.cells.length;
  }
  getCell(x: number) {
    const cell = this.cells[x];
    if (cell === undefined) return undefined;
    return {
      getChars: () => (typeof cell === "string" ? cell : cell.chars),
      getWidth: () => (typeof cell === "string" ? (cell === "" ? 0 : 1) : cell.width),
    };
  }
  translateToString() {
    return this.cells.map((cell) => (typeof cell === "string" ? cell : cell.chars)).join("");
  }
}

function bufferOf(rows: FakeLine[]): IBuffer {
  return { getLine: (y: number) => rows[y] } as unknown as IBuffer;
}

describe("readLogicalLine", () => {
  it("reads the row xterm names, not the one below it", () => {
    const buffer = bufferOf([new FakeLine(["a", "b"]), new FakeLine(["c", "d"])]);

    expect(readLogicalLine(buffer, 1).text).toBe("ab");
    expect(readLogicalLine(buffer, 2).text).toBe("cd");
  });

  it("joins a wrapped path back into the line that starts it", () => {
    const buffer = bufferOf([new FakeLine(["s", "r", "c", "/"]), new FakeLine(["a", "b"], true)]);

    const line = readLogicalLine(buffer, 2);

    expect(line.text).toBe("src/ab");
    // The first four characters are on the row xterm named, the last two on the row above the
    // screen, which is the whole reason the rows travel with the characters.
    expect(line.cells.slice(0, 4).map((cell) => cell.row)).toEqual([0, 0, 0, 0]);
    expect(line.cells.slice(4).map((cell) => cell.row)).toEqual([1, 1]);
  });

  it("walks back over every wrapped row above the one hovered", () => {
    const buffer = bufferOf([new FakeLine(["a"]), new FakeLine(["b"], true), new FakeLine(["c"], true)]);

    expect(readLogicalLine(buffer, 3).text).toBe("abc");
  });

  it("stops a line at the row that did not come from a wrap", () => {
    const buffer = bufferOf([new FakeLine(["a"]), new FakeLine(["b"]), new FakeLine(["c"], true)]);

    // Row 1 ends where row 2 begins, so it is only its own character. Row 2 owns row 3, which is
    // the continuation, and row 3 belongs to neither on its own.
    expect(readLogicalLine(buffer, 1).text).toBe("a");
    expect(readLogicalLine(buffer, 2).text).toBe("bc");
  });

  it("keeps a space inside a path, which is part of its name", () => {
    const buffer = bufferOf([new FakeLine(['"', "m", "y", " ", "n", "o", "t", "e", "s", ".", "m", "d", '"'])]);

    expect(readLogicalLine(buffer, 1).text).toBe('"my notes.md"');
  });

  it("drops the padding to the right of the last glyph", () => {
    const buffer = bufferOf([new FakeLine(["a", "b", "", "", ""])]);

    expect(readLogicalLine(buffer, 1).text).toBe("ab");
  });

  it("is empty for a row that does not exist", () => {
    expect(readLogicalLine(bufferOf([]), 1)).toEqual({ text: "", cells: [] });
  });
});

describe("cellRuns", () => {
  it("covers the columns a run of characters sits on", () => {
    const buffer = bufferOf([new FakeLine(["s", "r", "c", "/", "a", "b"])]);

    expect(cellRuns(readLogicalLine(buffer, 1).cells, 0, 6)).toEqual([{ row: 0, from: 0, to: 6 }]);
  });

  it("gives one run per row for a path that wraps", () => {
    const buffer = bufferOf([new FakeLine(["a", "b", "c"]), new FakeLine(["d", "e", "f"], true)]);

    expect(cellRuns(readLogicalLine(buffer, 2).cells, 1, 5)).toEqual([
      { row: 0, from: 1, to: 3 },
      { row: 1, from: 0, to: 2 },
    ]);
  });

  it("counts the second cell of a wide glyph as covered", () => {
    // `界` is drawn in one cell with an empty cell after it, so a run over the one character in
    // `text` is two columns wide.
    const buffer = bufferOf([new FakeLine(["a", { chars: "界", width: 2 }, { chars: "", width: 0 }, "b"])]);

    expect(readLogicalLine(buffer, 1).text).toBe("a界b");
    expect(cellRuns(readLogicalLine(buffer, 1).cells, 1, 2)).toEqual([{ row: 0, from: 1, to: 3 }]);
  });
});
