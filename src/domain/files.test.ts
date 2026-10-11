import { describe, expect, it } from "vitest";
import { absoluteFilePath } from "./files";

describe("absoluteFilePath", () => {
  it("joins the checkout directory and the file's own path with one separator", () => {
    expect(absoluteFilePath("/Users/dev/muster", "src/app.ts")).toBe("/Users/dev/muster/src/app.ts");
  });

  it("does not double the separator when the checkout directory carries a trailing one", () => {
    expect(absoluteFilePath("/Users/dev/muster/", "docs/readme.md")).toBe("/Users/dev/muster/docs/readme.md");
    expect(absoluteFilePath("/Users/dev/muster///", "docs/readme.md")).toBe("/Users/dev/muster/docs/readme.md");
  });

  it("leaves a checkout directory that is only separators to the file's own path", () => {
    expect(absoluteFilePath("/", "notes.txt")).toBe("/notes.txt");
  });

  it.each(["review-default", "review-workdir"] as const)("uses the pinned %s root for exported files", (origin) => {
    expect(absoluteFilePath("/repo", "notes.md", origin, "/Users/dev/.muster/reviews/")).toBe(
      "/Users/dev/.muster/reviews/notes.md",
    );
  });
});
