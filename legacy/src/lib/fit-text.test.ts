import { describe, expect, it } from "vitest";
import { nameSteps, pathSteps, widestThatFits } from "./fit-text";

describe("the shapes a path can be drawn at", () => {
  it("keeps a path short enough whole and only ever marks one that was shortened", () => {
    // Nothing was dropped from `/test`, so an ellipsis in front of it would claim a shortening the
    // row cannot back up. The empty step is the one after it: the header says nothing about where
    // the repo lives rather than half of where it lives.
    expect(pathSteps("/test")).toEqual(["/test", ""]);
    expect(pathSteps("/a/b")).toEqual(["/a/b", "…/b", ""]);
    expect(pathSteps("/Users/matiasmarani/Trabajo/xStudio/webapp")).toEqual(["…/xStudio/webapp", "…/webapp", ""]);
  });

  it("treats a path that is not there as one step of nothing", () => {
    // A group with no root has no path to draw, and a step that said so alongside the path itself
    // would be a rung the panel could weigh and never draw.
    expect(pathSteps("")).toEqual([""]);
  });

  it("reads a Windows path the same way as a POSIX one", () => {
    expect(pathSteps("C:\\work\\webapp\\dist")).toEqual(["…/webapp/dist", "…/dist", ""]);
  });
});

describe("the shapes a name can be drawn at", () => {
  it("gives a branch its namespace away and keeps the half that names the work", () => {
    // `bug/` is what the icon and the tooltip already say, and the ticket number is part of the
    // segment rather than one of its own, so this branch has exactly one segment to give away.
    expect(nameSteps("bug/13133933180-copy-id-into-the-lists-that-have-them")).toEqual([
      "bug/13133933180-copy-id-into-the-lists-that-have-them",
      "…/13133933180-copy-id-into-the-lists-that-have-them",
    ]);
    // A branch nested in two namespaces gives them up one at a time, and keeps the leaf.
    expect(nameSteps("feature/sidebar/weigh-a-label")).toEqual([
      "feature/sidebar/weigh-a-label",
      "…/sidebar/weigh-a-label",
      "…/weigh-a-label",
    ]);
  });

  it("leaves a name with no segments to give away as a single shape", () => {
    // The ellipsis is then the only thing that can shorten it, which is the last rung and not a
    // shape of its own.
    expect(nameSteps("main")).toEqual(["main"]);
    expect(nameSteps("")).toEqual([""]);
  });
});

describe("picking a shape that fits", () => {
  it("takes the widest one that fits, so nothing is shortened while something else could be", () => {
    expect(widestThatFits([120, 80, 40], 100)).toBe(1);
    expect(widestThatFits([120, 80, 40], 200)).toBe(0);
    expect(widestThatFits([120, 80, 40], 40)).toBe(2);
  });

  it("clamps to the narrowest shape rather than to nothing when not even it fits", () => {
    // The row still has to draw something: an ellipsis cutting the front of the narrowest whole
    // shape keeps the tail, where cutting the middle of the widest one would not.
    expect(widestThatFits([120, 80], 10)).toBe(1);
    expect(widestThatFits([], 10)).toBe(-1);
  });

  it("has no room to divide by, so nothing is narrowed on a panel that has not been laid out", () => {
    // Happy-dom lays nothing out, and the same is true of a panel that has not been mounted yet.
    // A room of zero must not be read as "nothing fits", which would leave every row drawing its
    // narrowest shape forever.
    expect(widestThatFits([120, 80], 0)).toBe(1);
  });
});
