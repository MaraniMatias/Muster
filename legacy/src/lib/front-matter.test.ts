import { describe, expect, it } from "vitest";
import {
  FRONT_MATTER_FENCE,
  frontMatterContent,
  frontMatterLineCount,
  opensFrontMatter,
  withoutFrontMatter,
} from "./front-matter";

describe("Front matter", () => {
  it("counts the block a document opens with", () => {
    const source = ["---", "title: Add the export", "tags: [api]", "---", "", "# Body"].join("\n");

    expect(frontMatterLineCount(source)).toBe(4);
    expect(frontMatterContent(source)).toBe("title: Add the export\ntags: [api]");
    expect(withoutFrontMatter(source)).toBe("\n# Body");
  });

  it("reads a document whose only content is its metadata", () => {
    expect(withoutFrontMatter("---\ntitle: x\n---")).toBe("");
    expect(withoutFrontMatter("---\n---\nBody.")).toBe("Body.");
  });

  it("takes a fence with the indentation a block may carry, and one closed with dots", () => {
    expect(frontMatterLineCount("  ---\n  title: x\n  ---\nBody.")).toBe(3);
    expect(frontMatterLineCount("---\ntitle: x\n...\nBody.")).toBe(3);
  });

  it("leaves a document that only opens with a rule alone", () => {
    // A rule with nothing to close it is prose. Reading the rest of the file as its metadata would
    // take the heading with it, which is what this shape is for: a document that opens with a rule.
    for (const source of ["---\n\n# Heading\n\nbody", "---\nname: thing\n\nbody", "---"]) {
      expect(frontMatterLineCount(source)).toBe(0);
      expect(withoutFrontMatter(source)).toBe(source);
    }
  });

  it("leaves a document alone when the fence is anywhere but the top", () => {
    const source = "# Title\n\n---\ntitle: x\n---\n\nbody";

    expect(frontMatterLineCount(source)).toBe(0);
    expect(withoutFrontMatter(source)).toBe(source);
  });

  it("returns no content when there is no closed metadata block", () => {
    expect(frontMatterContent("# Heading")).toBeNull();
    expect(frontMatterContent("---\ntitle: x")).toBeNull();
  });

  it("reads a document whose lines end with CRLF", () => {
    expect(frontMatterLineCount("---\r\ntitle: x\r\n---\r\n\r\nBody.")).toBe(3);
    expect(withoutFrontMatter("---\r\ntitle: x\r\n---\r\n\r\nBody.")).toBe("\nBody.");
  });

  it("names one fence, so the preview and the source view agree on what it is", () => {
    expect(opensFrontMatter(FRONT_MATTER_FENCE)).toBe(true);
    expect(opensFrontMatter("  ---  ")).toBe(true);
    expect(opensFrontMatter("----")).toBe(false);
    expect(opensFrontMatter("...")).toBe(false);
    expect(opensFrontMatter("--- title")).toBe(false);
    expect(opensFrontMatter("")).toBe(false);
  });
});
