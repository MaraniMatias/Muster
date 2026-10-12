import { Terminal } from "@xterm/headless";
import { describe, expect, it } from "vitest";
import { createPtyOutputWriter, PTY_OUTPUT_REPORT_STEP_BYTES } from "./terminal-renderer";

/**
 * A renderer that parses only when the test says so, which is what a window under load looks like
 * from here: bytes arrive far faster than xterm gets through them.
 */
function slowRenderer() {
  const writes: Uint8Array[] = [];
  const parsed: Array<() => void> = [];
  return {
    writes,
    renderer: {
      write(data: Uint8Array, callback?: () => void) {
        writes.push(data);
        if (callback) parsed.push(callback);
      },
    },
    parseOne() {
      parsed.shift()?.();
    },
    parseAll() {
      while (parsed.length > 0) parsed.shift()?.();
    },
  };
}

function chunk(size: number, fill = 0x61) {
  return new Uint8Array(size).fill(fill).buffer as ArrayBuffer;
}

async function drain(writer: { pending: number }) {
  while (writer.pending > 0) await new Promise((resolve) => setTimeout(resolve, 0));
}

describe("PTY output writer", () => {
  it("passes split UTF-8 and ANSI byte chunks intact to xterm", async () => {
    const terminal = new Terminal({ allowProposedApi: true, cols: 40, rows: 5 });
    const writer = createPtyOutputWriter(terminal, () => {});
    const output = new TextEncoder().encode("\u001b[31mred λ\u001b[0m\r\nsecond line\r\n");

    writer.push(output.subarray(0, 5).slice().buffer as ArrayBuffer);
    writer.push(output.subarray(5, 8).slice().buffer as ArrayBuffer);
    writer.push(output.subarray(8).slice().buffer as ArrayBuffer);
    await drain(writer);

    expect(writer.pending).toBe(0);
    expect(terminal.buffer.active.getLine(0)?.translateToString(true)).toBe("red λ");
    expect(terminal.buffer.active.getLine(1)?.translateToString(true)).toBe("second line");
    expect(terminal.buffer.active.getLine(0)?.getCell(0)?.getFgColor()).toBe(1);
    terminal.dispose();
  });

  it("preserves alternate-screen transitions through the same byte adapter", async () => {
    const terminal = new Terminal({ allowProposedApi: true, cols: 40, rows: 5 });
    const writer = createPtyOutputWriter(terminal, () => {});

    writer.push(new TextEncoder().encode("main text\r\n").buffer as ArrayBuffer);
    await drain(writer);
    writer.push(new TextEncoder().encode("\u001b[?1049hALT screen").buffer as ArrayBuffer);
    await drain(writer);

    expect(terminal.buffer.active.type).toBe("alternate");
    expect(
      Array.from({ length: terminal.rows }, (_, row) =>
        terminal.buffer.active.getLine(row)?.translateToString(true),
      ).join(" "),
    ).toContain("ALT screen");

    writer.push(new TextEncoder().encode("\u001b[?1049l").buffer as ArrayBuffer);
    await drain(writer);
    expect(terminal.buffer.active.type).toBe("normal");
    expect(terminal.buffer.active.getLine(0)?.translateToString(true)).toBe("main text");
    terminal.dispose();
  });

  // A 64 KiB chunk under load is one parse task per chunk, and a `cat` of a large file is
  // thousands of them queued behind the keyboard. Joining them is the difference between a burst
  // being drawn and a burst being parsed.
  it("joins everything that arrives while a write is being parsed into one write", () => {
    const { renderer, writes, parseOne } = slowRenderer();
    const writer = createPtyOutputWriter(renderer, () => {});

    for (let index = 0; index < 16; index++) writer.push(chunk(64 * 1024, index));
    // The first chunk is already with the renderer and the other fifteen are still this writer's
    // queue, so a burst of this shape is one parse at a time rather than one parse per chunk.
    expect(writes).toHaveLength(1);
    expect(writer.pending).toBe(16 * 64 * 1024);

    parseOne();
    expect(writes).toHaveLength(2);
    expect(writes[1]).toHaveLength(15 * 64 * 1024);
    // And in the order they arrived, which is the only order that is allowed to survive: each
    // chunk is filled with its own index, so a join that dropped or reordered one shows here.
    const joined = writes[1]!;
    for (let index = 0; index < 15; index++) {
      expect(joined[index * 64 * 1024]).toBe(index + 1);
      expect(joined[(index + 1) * 64 * 1024 - 1]).toBe(index + 1);
    }

    // One write is with the renderer and the rest are its queue, so nothing is pending as far as
    // the parser is concerned only once the last write has been parsed too.
    expect(writer.pending).toBe(15 * 64 * 1024);
    parseOne();
    expect(writer.pending).toBe(0);
    writer.dispose();
  });

  it("counts what the renderer has not parsed and reports what it has parsed in steps", () => {
    const { renderer, parseAll } = slowRenderer();
    const reported: number[] = [];
    const writer = createPtyOutputWriter(renderer, (parsed) => reported.push(parsed));
    const step = PTY_OUTPUT_REPORT_STEP_BYTES;

    for (let round = 0; round < 4; round++) {
      writer.push(chunk(step));
      writer.push(chunk(step));
      writer.push(chunk(step));
      // A chunk still waiting its turn is pending as well: it is exactly the work the reader is
      // being stopped for, and a count that only moved once the bytes reached the renderer would
      // let the queue grow by a whole burst between two reports.
      expect(writer.pending).toBe(3 * step);
      parseAll();
      expect(writer.pending).toBe(0);
    }

    // A report per step rather than per chunk, and the number it carries is the running
    // total of what the renderer has parsed: the count changes on every chunk and a session under
    // load changes it thousands of times a second. A round of three chunks is parsed as two writes,
    // so it reports as it drains — once for the first of them and once for the last — and a report
    // in between would carry a total that had moved by a step rather than by a whole round.
    expect(reported).toEqual([
      0,
      ...Array.from({ length: 4 }, (_, round) => [(3 * round + 1) * step, (3 * round + 3) * step]).flat(),
    ]);
    // And the total only ever moves forwards, which is what makes a report that is late or
    // repeated harmless to the reader rather than a rewind: it can consume against it.
    expect(reported[reported.length - 1]).toBe(4 * 3 * step);
    writer.dispose();
  });

  // The bug this protocol exists to fix: the reader counts a chunk as handed over before the
  // channel carries it, so a window that reports only what it has received describes an empty
  // queue for a burst that is still in flight.
  it("counts chunks that have not reached the renderer yet as output the reader is still owed", () => {
    const { renderer, parseAll } = slowRenderer();
    const reported: number[] = [];
    const step = PTY_OUTPUT_REPORT_STEP_BYTES;
    const writer = createPtyOutputWriter(renderer, (parsed) => reported.push(parsed));

    for (let index = 0; index < 8; index++) writer.push(chunk(step));
    // One chunk is with xterm and being parsed, seven are this writer's own queue, and the reader
    // has been sent nothing it has not handed over yet: so the total it would report for all
    // eight is the one it already reported, and the queue it is being stopped for is eight steps.
    expect(writer.pending).toBe(8 * step);
    expect(reported).toEqual([0]);

    parseAll();
    // What moves the reader's count is a parsed total, and it covers everything: the reader takes
    // the difference between what it sent and this, which is what keeps the chunks that were still
    // travelling when the last report went out inside the queue it is being stopped for.
    expect(reported).toEqual([0, step, 8 * step]);
    expect(writer.pending).toBe(0);
    writer.dispose();
  });

  it("loses nothing and reorders nothing across a burst a slow renderer cannot keep up with", () => {
    const { renderer, writes, parseOne } = slowRenderer();
    const writer = createPtyOutputWriter(renderer, () => {});
    const burst = Array.from({ length: 40 }, (_, index) => new Uint8Array([index]));

    for (const byte of burst) writer.push(byte.buffer);
    expect(writer.pending).toBe(burst.length);
    // Nothing is handed over while the previous write is still being parsed, and nothing is
    // thrown away to make room for the rest.
    expect(writes).toHaveLength(1);
    parseOne();

    expect(writes.flatMap((write) => Array.from(write))).toEqual(burst.map((_, index) => index));
    parseOne();
    expect(writer.pending).toBe(0);
    writer.dispose();
  });

  it("stops writing once disposed, because the terminal behind it is gone", () => {
    const { renderer, writes } = slowRenderer();
    const writer = createPtyOutputWriter(renderer, () => {});

    writer.push(chunk(8));
    writer.dispose();
    writer.push(chunk(8));

    expect(writes).toHaveLength(1);
  });
});
