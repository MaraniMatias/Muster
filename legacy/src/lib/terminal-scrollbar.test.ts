import { describe, expect, it } from "vitest";
import { scrollbarOffsetForTop, terminalScrollbarGeometry } from "./terminal-scrollbar";

// 24 rows in a 480px track with N lines behind them. These two make every number in the file come
// out whole, so a failure names a real disagreement rather than a rounding artefact.
const ROWS = 24;
const TRACK = 480;

function geometry(length: number, viewportY = 0, trackHeight = TRACK) {
  return terminalScrollbarGeometry({ length, viewportY, rows: ROWS }, trackHeight);
}

describe("terminal scrollbar geometry", () => {
  it("fills the track when there is nothing to scroll", () => {
    expect(geometry(ROWS)).toEqual({ top: 0, height: TRACK, scrollable: false, maxOffset: 0, travel: 0 });
    // A buffer that has not been given its first line yet is the same case and has to be answered
    // the same way, or the thumb would come out taller than the track on the terminal's first frame.
    expect(geometry(0)).toEqual(geometry(ROWS));
  });

  it("draws the thumb in proportion to how much of the buffer is on screen", () => {
    const short = geometry(48); // 24 of 48 lines visible: half the track
    expect(short.height).toBe(240);
    expect(short.maxOffset).toBe(24);
    expect(short.travel).toBe(240);

    const long = geometry(240); // one screen in ten
    expect(long.height).toBe(48);
    expect(long.maxOffset).toBe(216);
    expect(long.travel).toBe(432);
  });

  it("never draws a thumb too thin to grab", () => {
    // Ten thousand lines of scrollback under 24 rows is a 1.2px thumb, which is a stripe of grey
    // that cannot be aimed at. The floor is 28px, and it is paid for out of the travel rather than
    // out of the track, so the thumb still ends where the bottom of the scrollback is.
    const long = geometry(10_000);
    expect(long.height).toBe(28);
    expect(long.travel).toBe(TRACK - 28);
    expect(geometry(10_000, 0).top).toBe(0);
    expect(geometry(10_000, 9_976).top).toBe(TRACK - 28);

    // The floor is also why a screenful of scrollback in five is a 28px thumb and not the 24px its
    // fraction asks for: the two agree on where the thumb is at either end and differ only in how
    // far it travels, which is the trade a grabbable thumb is worth.
    expect(geometry(120).height).toBe(96);
    expect(geometry(120).travel).toBe(384);
  });

  it("puts the thumb at the fraction of the scrollback that is behind the viewport", () => {
    const at = (viewportY: number) => geometry(120, viewportY).top;
    expect(at(0)).toBe(0);
    expect(at(24)).toBe(96);
    expect(at(48)).toBe(192);
    expect(at(96)).toBe(384);
  });

  it("clamps a viewport that the buffer cannot vouch for", () => {
    // The buffer is the one input measured by a program that can be mid-write. A viewport past the
    // end of a scrollback that has just been trimmed has to land on the end rather than throw the
    // thumb off the bottom of its own track, and rows that exceed the buffer means a resize
    // landing between two frames.
    expect(geometry(120, 4_000).top).toBe(384);
    expect(geometry(120, -5).top).toBe(0);
    expect(terminalScrollbarGeometry({ length: 10, viewportY: 0, rows: 24 }, TRACK)).toEqual(geometry(24));
    expect(terminalScrollbarGeometry({ length: 120, viewportY: 0, rows: 0 }, TRACK).height).toBe(28);
  });

  it("answers nothing for a track that has no height, and recovers on the next measure", () => {
    // The overlay is in the DOM before it has a size on the frame the pane first appears, and a
    // thumb sized against a track of zero is a thumb of zero — which is a correct answer to the
    // wrong question, and one that has to be re-asked when the track has a size.
    expect(geometry(120, 48, 0)).toEqual({ top: 0, height: 0, scrollable: true, maxOffset: 96, travel: 0 });
    expect(geometry(120, 48, -10).height).toBe(0);
    expect(geometry(120, 48, 480).top).toBe(192);
  });
});

describe("scrollbar offset for a dragged thumb", () => {
  it("is the line the thumb's position stands for", () => {
    // The geometry and the drag have to agree about the same track, or a thumb dragged to the
    // bottom leaves the viewport somewhere else entirely. Both numbers come out of the same
    // function here, which is the point of it being one.
    const at = (top: number, length = 120) => scrollbarOffsetForTop(top, geometry(length));

    expect(at(0)).toBe(0);
    expect(at(96)).toBe(24);
    expect(at(192)).toBe(48);
    expect(at(384)).toBe(96);
  });

  it("clamps a thumb dragged off either end of the track", () => {
    const at = (top: number) => scrollbarOffsetForTop(top, geometry(120));
    expect(at(-100)).toBe(0);
    expect(at(10_000)).toBe(96);
  });

  it("rounds to a whole line, because there is no such thing as half a row", () => {
    // Travel of 384px over 96 lines is four lines to the pixel, so a drag that ends between two
    // pixels has to pick one of them. Rounding to the nearest rather than down is what keeps the
    // last line of scrollback reachable: a thumb dragged to the very bottom of its travel has to
    // be able to arrive at the last line, and rounding down would stop it half a line short.
    const travel = geometry(120).travel;
    expect(scrollbarOffsetForTop(travel, geometry(120))).toBe(96);
    expect(scrollbarOffsetForTop(travel * 0.51, geometry(120))).toBe(49);
    expect(scrollbarOffsetForTop(travel * 0.5, geometry(120))).toBe(48);
    expect(scrollbarOffsetForTop(travel * 0.49, geometry(120))).toBe(47);
  });

  it("has one position when the thumb fills the track", () => {
    expect(scrollbarOffsetForTop(120, geometry(24))).toBe(0);
    expect(scrollbarOffsetForTop(120, geometry(120, 0, 0))).toBe(0);
  });

  it("survives a scrollback too short to divide by", () => {
    // One line of travel and a thumb at the floor: a scrollback so long that the travel is smaller
    // than the fraction a drag reports. Rounding a 96-line buffer through a 1px track still has to
    // land inside the buffer.
    const geometry1 = terminalScrollbarGeometry({ length: 120, viewportY: 0, rows: 24 }, 29);
    expect(geometry1.travel).toBe(1);
    expect(scrollbarOffsetForTop(1, geometry1)).toBe(96);
    expect(scrollbarOffsetForTop(0, geometry1)).toBe(0);
  });
});
