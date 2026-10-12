import { describe, expect, it } from "vitest";
import {
  anchorOutcome,
  readDiffLines,
  buildReviewMarkdown,
  localReviewTimestamp,
  reviewFenceLanguage,
  reviewLineRange,
  isReviewableNote,
  reviewRangeCode,
  type ReviewNote,
} from "./review";

function note(overrides: Partial<ReviewNote>): ReviewNote {
  return {
    id: "note:1",
    checkoutId: "checkout:repo",
    path: "src/foo.js",
    side: "new",
    lineStart: 10,
    lineEnd: null,
    content: "revisit this calculation",
    code: "const result = a + b;",
    codeHash: "0000000000000001",
    outdated: false,
    roundId: null,
    status: "draft",
    createdAt: "2026-03-14T10:00:00Z",
    updatedAt: "2026-03-14T10:00:00Z",
    ...overrides,
  };
}

describe("Review markdown export", () => {
  it("uses the date it is given instead of the UTC one", () => {
    const markdown = buildReviewMarkdown([note({})], { date: "2026-03-14" });
    expect(markdown.startsWith("# Code Review 2026-03-14\n")).toBe(true);
  });

  it("local_review_timestamp_is_zero_padded_across_the_hour_and_month_boundaries", () => {
    expect(localReviewTimestamp(new Date(2026, 2, 1, 9, 5))).toEqual({
      date: "2026-03-01",
      timestamp: "2026-03-01-0905",
    });
    expect(localReviewTimestamp(new Date(2026, 2, 1, 0, 0))).toEqual({
      date: "2026-03-01",
      timestamp: "2026-03-01-0000",
    });
  });

  it("matches the CodeReview.nvim default format", () => {
    const markdown = buildReviewMarkdown(
      [
        note({ id: "note:1" }),
        note({
          id: "note:2",
          lineStart: 67,
          lineEnd: 72,
          code: "function handleUser(user) {\n  if (user.name) {\n    return user.name;\n  }\n}",
          content: "null check `user` before `.name`",
        }),
      ],
      { branch: "feature", defaultBranch: "main", date: "2026-03-14" },
    );

    expect(markdown).toBe(
      [
        "# Code Review 2026-03-14",
        "",
        "> `main..feature` — 1 file, 2 notes",
        "",
        "## src/foo.js",
        "",
        "```js{10}",
        "const result = a + b;",
        "```",
        "",
        "> revisit this calculation",
        "",
        "---",
        "",
        "```js{67-72}",
        "function handleUser(user) {",
        "  if (user.name) {",
        "    return user.name;",
        "  }",
        "}",
        "```",
        "",
        "> null check `user` before `.name`",
        "",
      ].join("\n"),
    );
  });

  it("groups notes by file, orders them by line and counts files and notes", () => {
    const markdown = buildReviewMarkdown(
      [
        note({ id: "note:1", path: "src/beta.ts", lineStart: 4, content: "second file" }),
        note({ id: "note:2", path: "src/alpha.ts", lineStart: 20, content: "later line" }),
        note({ id: "note:3", path: "src/alpha.ts", lineStart: 3, content: "first line" }),
      ],
      { branch: "feature", defaultBranch: "main", date: "2026-03-14" },
    );

    expect(markdown).toContain("> `main..feature` — 2 files, 3 notes");
    expect(markdown.indexOf("## src/alpha.ts")).toBeLessThan(markdown.indexOf("## src/beta.ts"));
    expect(markdown.indexOf("> first line")).toBeLessThan(markdown.indexOf("> later line"));
    expect(markdown).toContain("```ts{3}");
  });

  it("uses single notes in the header and renders a range on either side of the diff", () => {
    const markdown = buildReviewMarkdown(
      [note({ id: "note:1", side: "old", lineStart: 12, lineEnd: 14, content: "keep the guard" })],
      { date: "2026-03-14" },
    );

    expect(markdown).toContain("> `working tree` — 1 file, 1 note");
    // The old side used to collapse to a bare line number, which hid the range.
    expect(markdown).toContain("```js{12-14}");
  });

  it("renders a single-line note without a range and collects the code inside a range", () => {
    expect(reviewLineRange(note({ lineStart: 12, lineEnd: null }))).toBe("12");
    expect(reviewLineRange(note({ lineStart: 12, lineEnd: 12 }))).toBe("12");
    expect(reviewLineRange(note({ lineStart: 12, lineEnd: 14 }))).toBe("12-14");

    const texts = new Map([
      ["new:10", "ten"],
      ["new:11", "eleven"],
      ["new:12", "twelve"],
      ["old:10", "old ten"],
    ]);
    expect(reviewRangeCode(texts, "new", 10, 12)).toBe("ten\neleven\ntwelve");
    // Missing lines are simply absent, and the other side never leaks in.
    expect(reviewRangeCode(texts, "new", 12, null)).toBe("twelve");
    expect(reviewRangeCode(texts, "old", 10, 14)).toBe("old ten");
  });

  it("quotes multi-line note text and returns an empty export without notes", () => {
    const markdown = buildReviewMarkdown([note({ id: "note:1", content: "first line\nsecond line" })], {
      date: "2026-03-14",
    });

    expect(markdown).toContain("> first line\n> second line");
    expect(buildReviewMarkdown([])).toBe("");
  });

  it("maps extensions to fence languages and falls back to an empty language", () => {
    expect(reviewFenceLanguage("src/app.vue")).toBe("vue");
    expect(reviewFenceLanguage("Makefile")).toBe("");
  });

  it("decides which notes still need to reach the agent", () => {
    expect(isReviewableNote(note({ status: "draft" }))).toBe(true);
    expect(isReviewableNote(note({ status: "sent" }))).toBe(true);
    expect(isReviewableNote(note({ status: "resolved" }))).toBe(false);
  });

  it("reports what the diff can prove about an anchor, and what it cannot", () => {
    // Unchanged and deleted both let the user resolve, but they are not the same statement.
    expect(anchorOutcome("unchanged").resolvable).toBe(true);
    expect(anchorOutcome("missing").resolvable).toBe(true);
    expect(anchorOutcome("missing").message).toContain("no longer exists");
    expect(anchorOutcome("changed").resolvable).toBe(false);
    // A large diff only knows the lines it has loaded, and must say so rather than guess.
    expect(anchorOutcome("unknown").resolvable).toBe(false);
  });

  it("indexes the code behind every side of a unified patch", () => {
    const lines = readDiffLines(
      [
        "diff --git a/src/foo.js b/src/foo.js",
        "--- a/src/foo.js",
        "+++ b/src/foo.js",
        "@@ -8,4 +8,4 @@ export function foo() {",
        " const before = 1;",
        "-const after = 2;",
        "+const after = 3;",
        " const last = 4;",
      ].join("\n"),
    );
    const text = (side: "old" | "new", line: number) => lines.get(`${side}:${line}`)?.text ?? "";

    expect(text("new", 8)).toBe("const before = 1;");
    expect(text("old", 9)).toBe("const after = 2;");
    expect(text("new", 9)).toBe("const after = 3;");
    expect(text("new", 10)).toBe("const last = 4;");
    expect(text("new", 99)).toBe("");

    // Old 9 and new 9 are two different lines that happen to share a number, so the sign cannot be
    // read off which sides have it: only the patch knows which of the two was added.
    expect(lines.get("old:9")?.mark).toBe("-");
    expect(lines.get("new:9")?.mark).toBe("+");
    expect(lines.get("old:8")?.mark).toBe(" ");
    expect(lines.get("new:8")?.mark).toBe(" ");
  });
});
