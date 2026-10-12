import { describe, expect, it } from "vitest";
import { ligatureRanges } from "./ligature-joiner";

describe("ligatureRanges", () => {
  it("finds the sequences Fira Code draws as one glyph", () => {
    expect(ligatureRanges("a != b")).toEqual([[2, 4]]);
    expect(ligatureRanges("a => b <- c -> d")).toEqual([
      [2, 4],
      [7, 9],
      [12, 14],
    ]);
    expect(ligatureRanges("|| && :: >= <=")).toEqual([
      [0, 2],
      [3, 5],
      [6, 8],
      [9, 11],
      [12, 14],
    ]);
  });

  it("takes the longest ligature at a position", () => {
    expect(ligatureRanges("x !== y")).toEqual([[2, 5]]);
    expect(ligatureRanges("x === y")).toEqual([[2, 5]]);
    expect(ligatureRanges("x <<= y")).toEqual([[2, 5]]);
    expect(ligatureRanges("a /// b")).toEqual([[2, 5]]);
  });

  it("never returns overlapping ranges", () => {
    // `>>` sits inside what `->` already covers, so it is not offered a second time.
    expect(ligatureRanges("->>")).toEqual([[0, 2]]);
    expect(ligatureRanges("=> =>")).toEqual([
      [0, 2],
      [3, 5],
    ]);
  });

  it("leaves text without ligatures alone", () => {
    expect(ligatureRanges("")).toEqual([]);
    expect(ligatureRanges("const total = price + tax;")).toEqual([]);
    expect(ligatureRanges(".")).toEqual([]);
  });
});
