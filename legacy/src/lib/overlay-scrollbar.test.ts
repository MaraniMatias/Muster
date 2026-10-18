import { describe, expect, it } from "vitest";
import { overlayScrollbarGeometry, scrollbarOffsetForTop } from "./overlay-scrollbar";

// A 480px track with 960px of content behind a 480px viewport. These three make every number in the
// file come out whole, so a failure names a real disagreement rather than a rounding artefact.
const VIEWPORT = 480;
const TRACK = 480;

function geometry(scrollHeight: number, scrollTop = 0, trackHeight = TRACK) {
  return overlayScrollbarGeometry({ scrollTop, scrollHeight, clientHeight: VIEWPORT }, trackHeight);
}

describe("overlay scrollbar geometry", () => {
  it("fills the track when there is nothing to scroll", () => {
    expect(geometry(VIEWPORT)).toEqual({ top: 0, height: TRACK, scrollable: false, maxOffset: 0, travel: 0 });
    // A box that has not measured its content yet is the same case and has to be answered the same
    // way, or the thumb would come out taller than the track on the frame the pane first appears.
    expect(geometry(0)).toEqual(geometry(VIEWPORT));
    // Content shorter than the viewport is the same answer rather than a negative travel.
    expect(geometry(120)).toEqual(geometry(VIEWPORT));
  });

  it("draws the thumb in proportion to how much of the content is on screen", () => {
    const half = geometry(960); // 480 of 960px visible: half the track
    expect(half.height).toBe(240);
    expect(half.maxOffset).toBe(480);
    expect(half.travel).toBe(240);

    const tenth = geometry(4_800); // one screen in ten
    expect(tenth.height).toBe(48);
    expect(tenth.maxOffset).toBe(4_320);
    expect(tenth.travel).toBe(432);
  });

  it("never draws a thumb too thin to grab", () => {
    // Ten thousand pixels of content under 480 of viewport is a 0.5px thumb, which is a stripe of
    // grey that cannot be aimed at. The floor is 28px, and it is paid for out of the travel rather
    // than out of the track, so the thumb still ends where the bottom of the content is.
    const long = geometry(10_000);
    expect(long.height).toBe(28);
    expect(long.travel).toBe(TRACK - 28);
    expect(geometry(10_000, 0).top).toBe(0);
    expect(geometry(10_000, 9_520).top).toBe(TRACK - 28);

    // The floor is also why a screenful in five is a 28px thumb and not the 24px its fraction asks
    // for: the two agree on where the thumb is at either end and differ only in how far it travels,
    // which is the trade a grabbable thumb is worth.
    expect(geometry(2_400).height).toBe(96);
    expect(geometry(2_400).travel).toBe(384);
  });

  it("puts the thumb at the fraction of the content that is behind the viewport", () => {
    const at = (scrollTop: number) => geometry(2_400, scrollTop).top;
    expect(at(0)).toBe(0);
    expect(at(480)).toBe(96);
    expect(at(960)).toBe(192);
    expect(at(1_920)).toBe(384);
  });

  it("clamps a viewport the box cannot vouch for", () => {
    // A box whose content just got shorter — a row folded, a document replaced — can be scrolled
    // past its own end for the frame between the two, and the thumb has to land on the bottom of
    // the track rather than off it. A fractional offset is answered as the pixel it is nearest,
    // because a thumb that chases half a pixel never settles.
    expect(geometry(2_400, 40_000).top).toBe(384);
    expect(geometry(2_400, -5).top).toBe(0);
    expect(overlayScrollbarGeometry({ scrollTop: 0.4, scrollHeight: 2_400, clientHeight: VIEWPORT }, TRACK).top).toBe(
      0,
    );
    expect(overlayScrollbarGeometry({ scrollTop: 10, scrollHeight: 10, clientHeight: VIEWPORT }, TRACK)).toEqual(
      geometry(VIEWPORT),
    );
    expect(overlayScrollbarGeometry({ scrollTop: 0, scrollHeight: 2_400, clientHeight: 0 }, TRACK).height).toBe(28);
  });

  it("answers nothing for a track that has no height, and recovers on the next measure", () => {
    // The overlay is in the DOM before it has a size on the frame the pane first appears, and a
    // thumb sized against a track of zero is a thumb of zero — which is a correct answer to the
    // wrong question, and one that has to be re-asked when the track has a size.
    expect(geometry(2_400, 960, 0)).toEqual({ top: 0, height: 0, scrollable: true, maxOffset: 1_920, travel: 0 });
    expect(geometry(2_400, 960, -10).height).toBe(0);
    expect(geometry(2_400, 960, 480).top).toBe(192);
  });
});

describe("scrollbar offset for a dragged thumb", () => {
  it("is the position the thumb's place stands for", () => {
    // The geometry and the drag have to agree about the same track, or a thumb dragged to the
    // bottom leaves the box somewhere else entirely. Both numbers come out of the same function
    // here, which is the point of it being one.
    const at = (top: number, scrollHeight = 2_400) => scrollbarOffsetForTop(top, geometry(scrollHeight));

    expect(at(0)).toBe(0);
    expect(at(96)).toBe(480);
    expect(at(192)).toBe(960);
    expect(at(384)).toBe(1_920);
  });

  it("clamps a thumb dragged off either end of the track", () => {
    const at = (top: number) => scrollbarOffsetForTop(top, geometry(2_400));
    expect(at(-100)).toBe(0);
    expect(at(10_000)).toBe(1_920);
  });

  it("rounds to a whole pixel, because the arithmetic lands between two of them", () => {
    // Travel of 384px over 1,920px is five to the pixel, so a drag that ends between two pixels has
    // to pick one of them. Rounding to the nearest rather than down is what keeps the bottom of the
    // content reachable: a thumb dragged to the very bottom of its travel has to arrive at the last
    // px rather than stop a fraction short of it.
    const travel = geometry(2_400).travel;
    expect(scrollbarOffsetForTop(travel, geometry(2_400))).toBe(1_920);
    expect(scrollbarOffsetForTop(travel * 0.51, geometry(2_400))).toBe(979);
    expect(scrollbarOffsetForTop(travel * 0.5, geometry(2_400))).toBe(960);
    expect(scrollbarOffsetForTop(travel * 0.49, geometry(2_400))).toBe(941);
  });

  it("has one position when the thumb fills the track", () => {
    expect(scrollbarOffsetForTop(120, geometry(VIEWPORT))).toBe(0);
    expect(scrollbarOffsetForTop(120, geometry(2_400, 0, 0))).toBe(0);
  });

  it("survives a scrollport too long to divide by", () => {
    // One pixel of travel and a thumb at the floor: a content height so long that the travel is
    // smaller than the fraction a drag reports. Rounding a 1,920px offset through a 1px track still
    // has to land inside the content.
    const cramped = geometry(2_400, 0, 29);
    expect(cramped.travel).toBe(1);
    expect(scrollbarOffsetForTop(1, cramped)).toBe(1_920);
    expect(scrollbarOffsetForTop(0, cramped)).toBe(0);
  });
});
