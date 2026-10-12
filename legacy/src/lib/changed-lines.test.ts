import { describe, expect, it } from "vitest";
import { changedLineRanges } from "./changed-lines";

/** A patch shaped the way `git diff` writes one, from a file header down to its last body line. */
function patch(...lines: string[]): string {
  return ["diff --git a/x b/x", "--- a/x", "+++ b/x", ...lines].join("\n");
}

describe("The lines a patch says a file has changed", () => {
  it("marks the added line and not the context around it", () => {
    // The header describes a six-line window, so taking its count for the answer would mark lines
    // 1 to 6. Only line 2 is a line this change wrote.
    expect(changedLineRanges(patch("@@ -1,5 +1,6 @@", " a", "+b", " c", " d", " e"))).toEqual([{ start: 2, end: 2 }]);
  });

  it("reads a run of more than one line as one range", () => {
    expect(changedLineRanges(patch("@@ -10,7 +10,9 @@", " a", "+b", "+c", " d"))).toEqual([{ start: 11, end: 12 }]);
  });

  it("takes an omitted count as the single line Git means by it", () => {
    // `@@ -3 +4 @@` is one line on each side. Reading the missing count as zero would drop it.
    expect(changedLineRanges(patch("@@ -3 +4 @@", "+b"))).toEqual([{ start: 4, end: 4 }]);
  });

  it("marks the line an insertion at the end of a file lands on", () => {
    // `+4,1` after a three-line file is line 4, which is where the added line actually is: the
    // insertion point is the old file's last line, so the new side is one past it.
    expect(changedLineRanges(patch("@@ -3,0 +4,1 @@", "+d"))).toEqual([{ start: 4, end: 4 }]);
  });

  it("takes nothing from a hunk that only removes lines", () => {
    // A new side of zero lines is a file deleted down to nothing: there is no line in the new file
    // that could have changed, and `+0` is not a line to mark.
    expect(changedLineRanges(patch("@@ -1,3 +0,0 @@", "-a", "-b", "-c"))).toEqual([]);
  });

  it("counts a removal as taking up no room in the new file", () => {
    // Four lines become three: `a`, `d`, `e`. So `d` is line 2, which only holds if the two
    // removed lines were passed without moving the new side past them.
    expect(changedLineRanges(patch("@@ -1,4 +1,3 @@", " a", "-b", "-c", "+d", " e"))).toEqual([{ start: 2, end: 2 }]);
  });

  it("reads every hunk of a patch, in order", () => {
    expect(
      changedLineRanges(patch("@@ -1,4 +1,5 @@", " a", "+b", " c", "@@ -40,3 +41,4 @@", " d", "+e", " f")),
    ).toEqual([
      { start: 2, end: 2 },
      { start: 42, end: 42 },
    ]);
  });

  it("joins two runs that touch, so no line is asked for twice", () => {
    // Lines 2 and 3 are one changed block split across two hunks. Drawing them as two would hand a
    // builder the same positions twice, which it refuses.
    expect(changedLineRanges(patch("@@ -1,2 +1,3 @@", " a", "+b", " c", "@@ -3,0 +3,1 @@", "+d"))).toEqual([
      { start: 2, end: 3 },
    ]);
  });

  it("leaves two runs apart when context stands between them", () => {
    expect(changedLineRanges(patch("@@ -1,3 +1,4 @@", " a", "+b", " c", "@@ -4,0 +5,1 @@", "+e"))).toEqual([
      { start: 2, end: 2 },
      { start: 5, end: 5 },
    ]);
  });

  it("does not read a body line as a header, however much it looks like one", () => {
    // A line of source can be a hunk header. It is written with its `+` in front, and taking it
    // for a header would move the new side to line 50 and mark everything after it there.
    expect(changedLineRanges(patch("@@ -1,3 +1,4 @@", " a", "+@@ -50,2 +50,2 @@", " b"))).toEqual([
      { start: 2, end: 2 },
    ]);
  });

  it("claims no line for the end-of-file marker", () => {
    expect(changedLineRanges(patch("@@ -1,1 +1,2 @@", " a", "+b", "\\ No newline at end of file"))).toEqual([
      { start: 2, end: 2 },
    ]);
  });

  it("has nothing to say about a patch that changes nothing", () => {
    expect(changedLineRanges("")).toEqual([]);
    expect(changedLineRanges(patch("index 1111111..2222222 100644"))).toEqual([]);
  });
});
