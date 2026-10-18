import { describe, expect, it } from "vitest";
import { DiffFile, getSyntaxLineTemplate } from "@git-diff-view/vue";
import { prepareDiffHighlighting } from "./diff-highlighter";
import { detectedLanguageName, PLAIN_TEXT } from "./source-languages";

/**
 * A `.vue` file with the three blocks a Vue SFC has, and the script one holding TypeScript that a
 * grammar only reads as TypeScript if it was shown the tag that opened the block.
 */
const VUE_OLD = [
  "<template>",
  '  <section class="panel" :data-open="open">',
  "    <h2>{{ title }}</h2>",
  '    <button type="button" @click="toggle">{{ open ? "Close" : "Open" }}</button>',
  "  </section>",
  "</template>",
  "",
  '<script setup lang="ts">',
  'import { computed, ref } from "vue";',
  "",
  "const props = defineProps<{ title: string }>();",
  "const open = ref(false);",
  "const count = ref(0);",
  "const total = computed(() => count.value * 2);",
  "",
  "function toggle() {",
  "  open.value = !open.value;",
  "  count.value += 1;",
  "}",
  "",
  "defineExpose({ open, total });",
  "</script>",
  "",
  "<style scoped>",
  ".panel {",
  "  display: flex;",
  "  gap: 8px;",
  "}",
  "</style>",
];

const VUE_NEW = VUE_OLD.map((line, index) =>
  index === 13 ? "const total = computed(() => count.value * 2 + props.title.length);" : line,
);

/**
 * `VUE_OLD` with its template never closed and no script block opened, so line 14 is the same line of
 * code at the same number as in `VUE_OLD` and the grammar reads it as template text rather than as a
 * declaration.
 *
 * Two checkouts hold their own `src/Widget.vue` and nothing about a diff says which one it came from.
 * What makes the two answers different rather than one answer twice is this: the hunk they change is
 * the same line of code at the same number in both, so the window the library rebuilds out of it is
 * the same bytes, and only the file around that line says what the line means.
 */
const TEMPLATE_OLD = VUE_OLD.map((line, index) => {
  if (index === 5) return "  </div>";
  if (index === 7) return "";
  return line;
});
const TEMPLATE_NEW = TEMPLATE_OLD.map((line, index) =>
  index === 13 ? "const total = computed(() => count.value * 2 + props.title.length);" : line,
);

/**
 * One hunk of the diff between two versions of a file, built out of the two of them.
 *
 * The rows have to be the lines the file holds at those numbers, or the tokens read for the file
 * would rightly refuse them — which is the point of building them here rather than writing them out.
 * `index` is the line that was replaced, counted from zero.
 */
function hunkAt(index: number, oldLines: string[], newLines: string[], context = 3): string {
  const first = Math.max(0, index - context);
  const last = Math.min(oldLines.length - 1, index + context);
  const rows: string[] = [];
  for (let line = first; line < index; line += 1) rows.push(` ${oldLines[line]}`);
  rows.push(`-${oldLines[index]}`, `+${newLines[index]}`);
  for (let line = index + 1; line <= last; line += 1) rows.push(` ${oldLines[line]}`);
  return [`@@ -${first + 1},${last - first + 1} +${first + 1},${last - first + 1} @@`, ...rows].join("\n");
}

/** The whole of `git diff` for one file, which is the preamble plus one or more hunks. */
function patchOf(path: string, ...hunks: string[]): string {
  return [
    `diff --git a/${path} b/${path}`,
    "index 1111111..2222222 100644",
    `--- a/${path}`,
    `+++ b/${path}`,
    ...hunks,
  ].join("\n");
}

/** The line of lines, ended the way the file ends it. */
function text(lines: string[]): string {
  return `${lines.join("\n")}\n`;
}

/**
 * One diff, built the way `FileDiff.vue` builds it: one `DiffFile` per hunk, with no file content of
 * its own, which is what has the library rebuild each side of the hunk out of the patch.
 *
 * `identity` is the identity `FileDiff.vue` gives each set of hunks. The library keys its own reading
 * of a window by the text of that window when it is given none, and two diffs of two checkouts hold
 * byte-identical windows, so without one they would read each other's syntax.
 */
async function diffOf(
  path: string,
  patch: string,
  contents: { old?: string; new?: string },
  identity: string,
  /** How far into the file the diff reaches, which is how much of it is read. */
  lines?: number,
): Promise<{ highlighter: Awaited<ReturnType<typeof prepareDiffHighlighting>>; files: DiffFile[] }> {
  const language = detectedLanguageName(path);
  const highlighter = await prepareDiffHighlighting(language, contents, lines);
  const preamble: string[] = [];
  const sections: string[] = [];
  let current: string[] | null = null;
  for (const line of patch.split("\n")) {
    if (line.startsWith("@@")) {
      if (current) sections.push([...preamble, ...current].join("\n"));
      current = [line];
    } else if (current) current.push(line);
    else preamble.push(line);
  }
  if (current) sections.push([...preamble, ...current].join("\n"));
  const files = sections.map((section, index) => {
    const file = new DiffFile(`a/${path}`, "", `b/${path}`, "", [section], language, language, `${identity}:${index}`);
    file.initTheme("dark");
    file.init();
    file.buildUnifiedDiffLines();
    file.initSyntax({ registerHighlighter: highlighter ?? undefined });
    return file;
  });
  return { highlighter, files };
}

const ESCAPED: Record<string, string> = {
  "&amp;": "&",
  "&lt;": "<",
  "&gt;": ">",
  "&quot;": '"',
  "&#39;": "'",
};

function decode(markup: string): string {
  return markup.replace(/&(?:amp|lt|gt|quot|#39);/g, (entity) => ESCAPED[entity]);
}

/** The line the browser reads, which is what the spans between them carry between them. */
function lineOf(markup: string | undefined): string {
  return markup === undefined ? "" : decode(markup.replace(/<[^>]*>/g, ""));
}

/**
 * The color of the run of tokens `needle` falls in, which is one token's claim on a piece of a line.
 *
 * Going through the runs rather than looking for a color before the text is what says which token a
 * piece of text belongs to: a keyword that happens to be preceded by some other token's color would
 * pass a search for "any color before this text" while being painted as punctuation.
 */
function colorOf(markup: string | undefined, needle: string): string | undefined {
  if (!markup) return undefined;
  const runs = [...markup.matchAll(/<span[^>]*style="([^"]*)"[^>]*>([^<]*)<\/span>/g)].map(([, style, text]) => ({
    text: decode(text),
    color: /(?:^|;)color:([^;]*)/.exec(style)?.[1] ?? "",
  }));
  const at = runs
    .map((run) => run.text)
    .join("")
    .indexOf(needle);
  if (at < 0) return undefined;
  let start = 0;
  for (const run of runs) {
    if (at >= start && at < start + run.text.length) return run.color;
    start += run.text.length;
  }
  return undefined;
}

const KEYWORD = "var(--muster-syntax-token-keyword)";
const FUNCTION = "var(--muster-syntax-token-function)";
const CONSTANT = "var(--muster-syntax-token-constant)";
const FOREGROUND = "var(--muster-syntax-foreground)";

/** The markup a diff draws for one line, which is the whole chain this module is responsible for. */
function drawn(file: DiffFile, side: "old" | "new", line: number): string | undefined {
  const syntax = side === "old" ? file.getOldSyntaxLine(line) : file.getNewSyntaxLine(line);
  return syntax ? getSyntaxLineTemplate(syntax) : undefined;
}

/**
 * The colors a line is painted in, one per run of tokens.
 *
 * A line the grammar read as nothing in particular is a single run of the plain text color, which is
 * how markup the grammar could not classify looks; a line it read is several colors.
 */
function colorsOf(markup: string | undefined): string[] {
  return [
    ...new Set(
      [...(markup ?? "").matchAll(/<span[^>]*style="([^"]*)"/g)]
        .map(([, style]) => /(?:^|;)color:([^;]*)/.exec(style)?.[1] ?? "")
        .filter(Boolean),
    ),
  ];
}

describe("diff highlighter", () => {
  it('reads a Vue hunk inside <script setup lang="ts"> as the TypeScript it is', async () => {
    // The whole file is read for this, and it is the whole file that makes it work: the hunk carries
    // no `<script setup lang="ts">` of its own, so a grammar handed the hunk reads the TypeScript in
    // it as markup — one span of one color for the entire line, which is what it did before.
    const path = "src/Widget.vue";
    const { highlighter, files } = await diffOf(
      path,
      patchOf(path, hunkAt(13, VUE_OLD, VUE_NEW)),
      { old: text(VUE_OLD), new: text(VUE_NEW) },
      "vue:1",
    );
    expect(highlighter).toBeDefined();
    const [file] = files;
    const added = drawn(file, "new", 14);
    const removed = drawn(file, "old", 14);

    // Line 14 is where the patch put it, and what it says is what the tokens were read from.
    expect(lineOf(added)).toBe("const total = computed(() => count.value * 2 + props.title.length);");
    expect(lineOf(removed)).toBe("const total = computed(() => count.value * 2);");

    // Each of these is one token, named, rather than "some color somewhere before this text".
    expect(colorOf(added, "const")).toBe(KEYWORD);
    expect(colorOf(added, "computed")).toBe(FUNCTION);
    expect(colorOf(added, "total")).toBe(CONSTANT);
    // The arrow and the arithmetic are what highlight.js hands back as bare text, so no stylesheet
    // could paint them at all; Shiki names them, and names them apart from the punctuation.
    expect(colorOf(added, "=>")).toBe(KEYWORD);
    expect(colorOf(added, "+")).toBe(KEYWORD);
    expect(colorOf(added, "(")).toBe(FOREGROUND);
    // A context line of the hunk is read from the file too, not from the hunk.
    expect(lineOf(drawn(file, "new", 12))).toBe("const open = ref(false);");
    expect(colorOf(drawn(file, "new", 12), "ref")).toBe(FUNCTION);
  });

  it("keeps two diffs of the same path and the same window apart", async () => {
    // A path is not an identity, and neither is a diff: two checkouts hold their own `src/Widget.vue`,
    // and the hunk they change is the same line of code at the same number in both. What says which
    // checkout a diff came from is the file around that line, which is outside the hunk — so the window
    // the library rebuilds out of the two hunks is the same bytes, and only a reading of the whole file
    // can tell them apart.
    const path = "src/Widget.vue";
    const sfc = await diffOf(
      path,
      patchOf(path, hunkAt(13, VUE_OLD, VUE_NEW)),
      {
        old: text(VUE_OLD),
        new: text(VUE_NEW),
      },
      "checkout-a:1",
    );
    const markup = await diffOf(
      path,
      patchOf(path, hunkAt(13, TEMPLATE_OLD, TEMPLATE_NEW)),
      {
        old: text(TEMPLATE_OLD),
        new: text(TEMPLATE_NEW),
      },
      "checkout-b:1",
    );

    // The windows are the same bytes, which is what makes this worth a test at all.
    expect(markup.files[0].getNewFileContent()).toBe(sfc.files[0].getNewFileContent());
    expect(markup.highlighter).not.toBe(sfc.highlighter);

    // The one line is a declaration in the file whose script block is open there, and text in the file
    // whose template is: several colors against one.
    expect(lineOf(drawn(sfc.files[0], "new", 14))).toBe(lineOf(drawn(markup.files[0], "new", 14)));
    expect(colorOf(drawn(sfc.files[0], "new", 14), "computed")).toBe(FUNCTION);
    expect(colorsOf(drawn(sfc.files[0], "new", 14)).length).toBeGreaterThan(2);
    expect(colorsOf(drawn(markup.files[0], "new", 14))).toEqual([FOREGROUND]);
  });

  it("answers each diff on its own, so one file's syntax is never another's", async () => {
    // The answer is an object that closes over one reading, and nothing is published anywhere: two
    // reads of one file, in whatever order they land, are two objects and neither can reach the other.
    const first = await prepareDiffHighlighting("vue", { old: text(VUE_OLD), new: text(VUE_NEW) });
    const second = await prepareDiffHighlighting("vue", { old: text(TEMPLATE_OLD), new: text(TEMPLATE_NEW) });
    expect(first).toBeDefined();
    expect(second).toBeDefined();
    expect(first).not.toBe(second);

    // The line each one reads is its own file's: a declaration in one, template text in the other.
    const line = (highlighter: typeof first, source: string) =>
      getSyntaxLineTemplate(
        highlighter!.processAST(highlighter!.getAST(source, "b/src/Widget.vue", "vue")).syntaxFileObject[14],
      );
    expect(colorOf(line(first, text(VUE_NEW)), "computed")).toBe(FUNCTION);
    expect(colorsOf(line(second, text(TEMPLATE_NEW)))).toEqual([FOREGROUND]);
    // And the first is still the first after the second was read.
    expect(colorOf(line(first, text(VUE_NEW)), "computed")).toBe(FUNCTION);
  });

  it("reads two same-path diffs whose windows are byte-identical as two files, not one", async () => {
    // The window the library rebuilds out of a hunk is the hunk's own rows at their line numbers, so
    // two checkouts whose change is the same line of code hold byte-identical windows. What a line in
    // one means is decided by the file around it, and each diff answers for its own.
    const path = "src/Widget.vue";
    const sfc = await diffOf(
      path,
      patchOf(path, hunkAt(13, VUE_OLD, VUE_NEW)),
      {
        old: text(VUE_OLD),
        new: text(VUE_NEW),
      },
      "checkout-a:1",
    );
    const markup = await diffOf(
      path,
      patchOf(path, hunkAt(13, TEMPLATE_OLD, TEMPLATE_NEW)),
      {
        old: text(TEMPLATE_OLD),
        new: text(TEMPLATE_NEW),
      },
      "checkout-b:1",
    );

    // The windows are the same bytes, which is what makes this worth a test at all.
    expect(markup.files[0].getNewFileContent()).toBe(sfc.files[0].getNewFileContent());
    expect(lineOf(drawn(markup.files[0], "new", 14))).toBe(lineOf(drawn(sfc.files[0], "new", 14)));

    // The declaration is a declaration in the file whose script block is open there, and text the
    // grammar had nothing to say about in the file whose template is.
    expect(colorsOf(drawn(sfc.files[0], "new", 14)).length).toBeGreaterThan(2);
    expect(colorsOf(drawn(markup.files[0], "new", 14))).toEqual([FOREGROUND]);
    // And each still reads its own file afterwards, rather than the other's taking over.
    expect(colorOf(drawn(sfc.files[0], "new", 14), "computed")).toBe(FUNCTION);
  });

  it("reads a change 2500 lines into a file, where the diff used to stop reading", async () => {
    // The library counts a line for every line before the hunk when it measures how much there is to
    // read, and gave up on its own cap of 2000: a one-line change deep in a file came out with no
    // syntax at all, which is most of what a diff of a real file looks like.
    const path = "src/deep.ts";
    const old = Array.from({ length: 2600 }, (_, line) => `export const value${line} = ${line};`);
    const next = old.map((line, index) => (index === 2499 ? "export const value2499 = 4242;" : line));
    const { highlighter, files } = await diffOf(
      path,
      patchOf(path, hunkAt(2499, old, next)),
      { old: text(old), new: text(next) },
      "deep:1",
    );
    expect(highlighter).toBeDefined();

    const [file] = files;
    const added = drawn(file, "new", 2500);
    expect(lineOf(added)).toBe("export const value2499 = 4242;");
    expect(colorOf(added, "export")).toBe(KEYWORD);
    expect(colorOf(added, "value2499")).toBe(CONSTANT);
    expect(colorOf(added, "4242")).toBe(CONSTANT);
    // A context line of the hunk, three lines above the change, is read from the file too.
    expect(colorOf(drawn(file, "new", 2499), "value2498")).toBe(CONSTANT);
  });

  it("reads only as far into a file as the diff reaches, and hands back the rest to the library", async () => {
    // A change near the top of a long file is read as the top of that file: the library builds each
    // hunk's window out of the file's own lines from the first to the one the hunk ends on, so the
    // lines past that are never asked about and are a pass over the file for nothing.
    const path = "src/long.ts";
    const old = Array.from({ length: 400 }, (_, line) => `export const value${line} = ${line};`);
    const next = old.map((line, index) => (index === 9 ? "export const value9 = 4242;" : line));
    const patch = patchOf(path, hunkAt(9, old, next));
    const { highlighter, files } = await diffOf(path, patch, { old: text(old), new: text(next) }, "long:1", 13);
    expect(highlighter).toBeDefined();

    // The hunk reaches line 13, and the lines it paints are read from those lines of the file.
    const [file] = files;
    const added = drawn(file, "new", 10);
    expect(lineOf(added)).toBe("export const value9 = 4242;");
    expect(colorOf(added, "export")).toBe(KEYWORD);
    expect(colorOf(added, "value9")).toBe(CONSTANT);
    expect(colorOf(added, "4242")).toBe(CONSTANT);

    // A window that reaches past what was read is a window nothing can be said about, and handing
    // back nothing is what leaves the library to draw that one its own way.
    const past = highlighter!.getAST("const a = 1;\nconst b = 2;", "b/src/long.ts");
    expect(past).toBeUndefined();
  });

  it("reads every hunk of a file from the one read of it, each at its own lines", async () => {
    // One file is read once and every hunk of it is cut out of that, so two hunks at different
    // depths cannot read each other's lines: the shallow one is still the shallow file's lines.
    const path = "src/gallery/GalleryView.ts";
    const old = Array.from({ length: 400 }, (_, line) => `const gallery${line} = ref(${line});`);
    const next = old.map((line, index) =>
      index === 40 ? "const gallery40 = ref(4242);" : index === 300 ? "const gallery300 = ref(2424);" : line,
    );
    const { highlighter, files } = await diffOf(
      path,
      patchOf(path, hunkAt(40, old, next), hunkAt(300, old, next)),
      { old: text(old), new: text(next) },
      "gallery:1",
    );
    expect(highlighter).toBeDefined();
    // One `DiffFile` per hunk, which is how the panel draws them.
    expect(files).toHaveLength(2);

    const [shallow, deep] = files;
    expect(lineOf(drawn(shallow, "new", 41))).toBe("const gallery40 = ref(4242);");
    expect(colorOf(drawn(shallow, "new", 41), "gallery40")).toBe(CONSTANT);
    expect(lineOf(drawn(deep, "new", 301))).toBe("const gallery300 = ref(2424);");
    expect(colorOf(drawn(deep, "new", 301), "gallery300")).toBe(CONSTANT);
    // A line of the deep hunk is not the shallow hunk's line of the same number.
    expect(colorOf(drawn(deep, "new", 41), "gallery40")).toBeUndefined();
  });

  it("draws the line the file holds, blank lines and last line included", async () => {
    // The line numbers a hunk is drawn at are what a review note is anchored to, so a line that
    // moves would move every note under it. What is drawn is the file's own text, escaped, with the
    // empty lines still being lines.
    const path = "src/escaped.ts";
    const old = [
      'const label = "<script>alert(1)</script>";',
      "",
      "export function report(): string {",
      "  // a comment",
      "  /* a block",
      "     comment */",
      "  return `${label} & more`;",
      "}",
    ];
    const next = old.map((line, index) => (index === 3 ? "  // a changed comment" : line));
    const { files } = await diffOf(
      path,
      patchOf(path, hunkAt(3, old, next)),
      {
        old: text(old),
        new: text(next),
      },
      "escaped:1",
    );
    const [file] = files;

    // Line 4 is the one the hunk changed, and it is the changed one that is drawn.
    expect(lineOf(drawn(file, "new", 4))).toBe("  // a changed comment");
    expect(lineOf(drawn(file, "old", 4))).toBe("  // a comment");
    expect(colorOf(drawn(file, "new", 4), "//")).toMatch(/token-comment/);
    // The blank line and the lines the hunk never mentions keep their own numbers.
    expect(lineOf(drawn(file, "new", 2))).toBe("");
    expect(lineOf(drawn(file, "new", 3))).toBe("export function report(): string {");
    // The last line the hunk draws is the file's, punctuation and all, and its own `&` is escaped.
    expect(lineOf(drawn(file, "new", 7))).toBe("  return `${label} & more`;");
    expect(drawn(file, "new", 7)).toContain("&amp;");
    // A file's own text never becomes markup: nothing here parses the file, the tree is built from
    // tokens and the library escapes on the way out.
    const script = drawn(file, "new", 1) ?? "";
    expect(script).toContain("&lt;script&gt;");
    expect(script).not.toContain("<script>");
  });

  it("draws a CRLF file's lines as themselves, carriage return and all", async () => {
    // Git prints the carriage return it found, and a repository with `autocrlf` cleans it out of the
    // file it prints, so both are read here. What reaches the browser is the line either way.
    const path = "src/windows.ts";
    const old = ["const total = 1;", "", "export const shown = total;", ""];
    const next = old.map((line, index) => (index === 2 ? "export const shown = total + 1;" : line));
    const crlf = (lines: string[]) => lines.map((line) => `${line}\r`).join("\n");
    const { files } = await diffOf(
      path,
      patchOf(path, hunkAt(2, old, next)),
      {
        old: crlf(old),
        new: crlf(next),
      },
      "windows:1",
    );
    const [file] = files;

    expect(lineOf(drawn(file, "new", 3))).toBe("export const shown = total + 1;");
    expect(colorOf(drawn(file, "new", 3), "shown")).toBe(CONSTANT);
    // The blank line before it is a line, and it is still a line.
    expect(lineOf(drawn(file, "new", 2))).toBe("");
  });

  it("reads an added file's new side and a removed file's old side", async () => {
    // An added file has no old side and a removed one no new side, which the backend says by leaving
    // the text out rather than sending it empty. Each side is read on its own, and the side that
    // holds the lines the diff draws is the one that is there.
    const added = "src/created.ts";
    const created = ["export const created = computed(() => 1);", "export const other = created + 2;"];
    const addedDiff = await diffOf(added, patchOf(added, hunkAt(0, [""], created)), { new: text(created) }, "added:1");
    expect(addedDiff.highlighter).toBeDefined();
    expect(lineOf(drawn(addedDiff.files[0], "new", 1))).toBe("export const created = computed(() => 1);");
    expect(colorOf(drawn(addedDiff.files[0], "new", 1), "computed")).toBe(FUNCTION);

    const removed = "src/removed.ts";
    const gone = ["export const gone = computed(() => 1);", "export const other = gone + 2;"];
    const removedDiff = await diffOf(removed, patchOf(removed, hunkAt(0, gone, [""])), { old: text(gone) }, "gone:1");
    expect(removedDiff.highlighter).toBeDefined();
    expect(colorOf(drawn(removedDiff.files[0], "old", 1), "computed")).toBe(FUNCTION);
  });

  it("reads a renamed file's two sides from the two texts of it", async () => {
    // A rename's patch names where the file came from, and the two texts of it are still the merge
    // base's blob and the file on disk. Which path the diff is opened for changes neither.
    const path = "src/gallery/RenamedView.ts";
    const old = ["export const before = computed(() => 1);", "export const held = before + 2;"];
    const next = old.map((line, index) => (index === 0 ? "export const before = computed(() => 2);" : line));
    const patch = [
      "diff --git a/src/gallery/View.ts b/src/gallery/RenamedView.ts",
      "similarity index 92%",
      "rename from src/gallery/View.ts",
      `rename to ${path}`,
      "index 1111111..2222222 100644",
      "--- a/src/gallery/View.ts",
      `+++ b/${path}`,
      hunkAt(0, old, next),
    ].join("\n");
    const { files } = await diffOf(path, patch, { old: text(old), new: text(next) }, "renamed:1");
    expect(colorOf(drawn(files[0], "old", 1), "computed")).toBe(FUNCTION);
    expect(colorOf(drawn(files[0], "new", 1), "computed")).toBe(FUNCTION);
  });

  it("leaves a line alone rather than painting it with another line's colors", async () => {
    // The text a diff draws comes from the patch and the colors from the file as it was read, and the
    // two were read moments apart. A file that moved in between leaves lines the tokens are not of,
    // and the honest thing to draw then is what the library draws on its own — never a line colored as
    // if it were the line that is now at that number.
    const path = "src/moved.ts";
    const old = ["const first = computed(() => 1);", "const second = computed(() => 2);"];
    const next = ["const first = computed(() => 3);", "const second = computed(() => 4);"];
    const patch = patchOf(path, hunkAt(0, old, next));
    // The file is read again from text the patch has not caught up with, which is what a diff read a
    // moment after its patch looks like from here.
    const { files } = await diffOf(path, patch, { old: text(old), new: text(old) }, "moved:1");

    // The line is still the patch's line, drawn; it is only that none of it is ours to color.
    expect(lineOf(drawn(files[0], "new", 1))).toBe(next[0]);
    expect(drawn(files[0], "new", 1)).not.toContain("muster-syntax");
  });

  it("leaves a file with no text of its own to the library, which is what the diff falls back to", async () => {
    // What the backend leaves out: a binary diff, a diff too large to hold, a file past the cap. There
    // is nothing to read, so there is nothing to hand over and the library highlights the file the
    // way it always has — the state it was in before any of this.
    const path = "assets/large.ts";
    const { highlighter, files } = await diffOf(path, patchOf(path, "@@ -1 +1 @@\n-old\n+new\n"), {}, "none:1");
    expect(highlighter).toBeUndefined();
    const markup = drawn(files[0], "new", 1) ?? "";
    expect(markup).toContain("hljs-");
    expect(markup).not.toContain("muster-syntax");
  });

  it("reads nothing past the limits it reads a file under", async () => {
    // Two limits, each one a real thing: the bytes a side is read in, which is what bounds the
    // reading's time and its tokens both, and how deep a file is read whole.
    const deep = Array.from({ length: 9000 }, (_, line) => `export const value${line} = ${line};`).join("\n");
    expect(await prepareDiffHighlighting("typescript", { new: deep })).toBeUndefined();
    expect(await prepareDiffHighlighting("typescript", { new: "x".repeat(512 * 1024 + 1) })).toBeUndefined();
    // The cap is in bytes and not in characters: `é` is one character and two bytes, so this file is
    // a third of the cap in characters and twice it in bytes. A file under both is read.
    expect(await prepareDiffHighlighting("typescript", { new: "é".repeat(300_000) })).toBeUndefined();
    expect(
      await prepareDiffHighlighting("typescript", {
        new: text(Array.from({ length: 200 }, (_, line) => `export const value${line} = ${line};`)),
      }),
    ).toBeDefined();
  });

  it("answers each diff on its own, so a late one cannot reach the diff that replaced it", async () => {
    // Nothing is published: what comes back belongs to the caller alone. Two reads of one path, the
    // first of them slower than the second, are two answers rather than one that overwrote the other,
    // and the slow one has nothing to reach the diff that replaced it with, because there is no
    // shared state for it to change.
    const first = prepareDiffHighlighting("vue", { new: text(VUE_OLD) });
    const second = prepareDiffHighlighting("vue", { new: text(VUE_NEW) });
    const [one, two] = await Promise.all([first, second]);
    expect(one).toBeDefined();
    expect(two).toBeDefined();
    expect(one).not.toBe(two);

    // Each still reads the file it was given, whichever order the two were asked in.
    const line = (highlighter: typeof one, source: string) =>
      colorOf(
        getSyntaxLineTemplate(
          highlighter!.processAST(highlighter!.getAST(source, "b/src/Widget.vue", "vue")).syntaxFileObject[14],
        ),
        "total",
      );
    expect(line(one, text(VUE_OLD))).toBe(CONSTANT);
    expect(line(two, text(VUE_NEW))).toBe(CONSTANT);

    // A third read of the older file is its own answer, and leaves the newer ones as they were.
    const three = await prepareDiffHighlighting("vue", { new: text(VUE_OLD) });
    expect(three).not.toBe(one);
    expect(line(three, text(VUE_OLD))).toBe(CONSTANT);
    expect(line(two, text(VUE_NEW))).toBe(CONSTANT);
  });

  it("claims a language only once its grammar is loaded", async () => {
    // A language nothing is loaded for is the library's to answer for, which is what keeps a file
    // Muster has no grammar for rendering the way it always has rather than rendering nothing.
    expect(await prepareDiffHighlighting("no-such-language", { new: "x\n" })).toBeUndefined();
    expect(await prepareDiffHighlighting(undefined, { new: "x\n" })).toBeUndefined();
    // Plaintext is deliberately in no allowlist, so this is not ours to answer either.
    expect(await prepareDiffHighlighting(PLAIN_TEXT, { new: "text\n" })).toBeUndefined();

    expect(await prepareDiffHighlighting("typescript", { new: "const a = 1;\n" })).toBeDefined();
    // A second ask costs nothing, and the answer does not change.
    expect(await prepareDiffHighlighting("typescript", { new: "const a = 1;\n" })).toBeDefined();
  });

  it("reads a grammar under the name Shiki knows it by, not the name Muster calls it", async () => {
    // `gitignore` is Muster' own name for a file of bare globs, and Shiki has no such grammar, so it
    // borrows `ini`. The name is only how the grammar is asked for.
    const path = ".gitignore";
    const lines = ["# a comment", "*.log", ""];
    const { highlighter, files } = await diffOf(
      path,
      patchOf(path, hunkAt(0, [""], lines)),
      {
        new: text(lines),
      },
      "ignore:1",
    );
    expect(highlighter).toBeDefined();
    expect(colorOf(drawn(files[0], "new", 1), "# a comment")).toMatch(/token-comment/);
  });
});
