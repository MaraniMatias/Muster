import { describe, expect, it } from "vitest";
import { diffRowHeight } from "./use-large-diff";

describe("diffRowHeight", () => {
  it("grows the row with the font the diff reads at", () => {
    // The window's arithmetic is in whole pixels and in rows the markup paints, so the same font size
    // has to name the same height every time: a row that measures a fraction more than the padding
    // that stands in for the rows off screen is a diff that drifts as it is scrolled.
    expect(diffRowHeight(13)).toBe(20);
    // The editor's own range, and both ends of it still hold their text.
    expect(diffRowHeight(9)).toBe(14);
    expect(diffRowHeight(32)).toBe(50);
  });
});
