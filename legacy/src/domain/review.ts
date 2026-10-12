export type ReviewSide = "old" | "new";
export type ReviewTarget = "markdown" | "opencode";
export type ReviewNoteStatus = "draft" | "sent" | "resolved";

export interface ReviewAnchorCheck {
  id: string;
  currentCode: string;
}

/**
 * What the diff says about a note's anchor now.
 *
 * `unknown` is a real answer, not a fallback: a large diff only has the lines it has
 * loaded, so a note outside the window cannot be judged and must not be guessed at.
 */
export type AnchorOutcome = "unchanged" | "changed" | "missing" | "unknown";

export interface ReviewNote {
  id: string;
  checkoutId: string;
  path: string;
  side: ReviewSide;
  lineStart: number;
  lineEnd: number | null;
  content: string;
  /** Diff lines captured when the note was created, so exports never re-read the diff. */
  code: string;
  /** Fingerprint of the anchored line, captured with the note. */
  codeHash: string;
  /** Sticky: set when the anchored line drifts, cleared only by the user. */
  outdated: boolean;
  /** The round that last carried this note, if any. */
  roundId: string | null;
  status: ReviewNoteStatus;
  createdAt: string;
  updatedAt: string;
}

/** One batch of notes delivered to an agent as a single message. */
export type ReviewRoundStatus = "queued" | "dispatching" | "dispatched" | "acked" | "relocated";

export interface ReviewRound {
  id: string;
  checkoutId: string;
  sessionId: string | null;
  status: ReviewRoundStatus;
  /** Embedded in the prompt, which is what makes a reconnect safe. */
  marker: string;
  noteIds: string[];
  createdAt: string;
  updatedAt: string;
}

export interface ReviewContext {
  branch?: string;
  defaultBranch?: string;
  /** ISO date (yyyy-mm-dd) used in the heading. */
  date?: string;
}

export function localReviewTimestamp(now = new Date()): { date: string; timestamp: string } {
  const year = String(now.getFullYear()).padStart(4, "0");
  const month = String(now.getMonth() + 1).padStart(2, "0");
  const day = String(now.getDate()).padStart(2, "0");
  const hour = String(now.getHours()).padStart(2, "0");
  const minute = String(now.getMinutes()).padStart(2, "0");
  const date = `${year}-${month}-${day}`;
  return { date, timestamp: `${date}-${hour}${minute}` };
}

const fenceLanguageByExtension: Record<string, string> = {
  c: "c",
  cc: "cpp",
  cpp: "cpp",
  cs: "csharp",
  css: "css",
  go: "go",
  h: "c",
  hpp: "cpp",
  html: "html",
  java: "java",
  js: "js",
  json: "json",
  jsx: "jsx",
  kt: "kotlin",
  kts: "kotlin",
  less: "less",
  md: "md",
  php: "php",
  py: "python",
  rb: "ruby",
  rs: "rs",
  scss: "scss",
  sh: "sh",
  sql: "sql",
  swift: "swift",
  toml: "toml",
  ts: "ts",
  tsx: "tsx",
  vue: "vue",
  xml: "xml",
  yaml: "yaml",
  yml: "yaml",
  zsh: "sh",
};

export function reviewFenceLanguage(path: string): string {
  const extension = path.split("/").pop()?.split(".").pop()?.toLowerCase() ?? "";
  return fenceLanguageByExtension[extension] ?? "";
}

/** Both sides render the same way: a range only shows up when it spans more than one line. */
export function reviewLineRange(note: Pick<ReviewNote, "side" | "lineStart" | "lineEnd">): string {
  return note.lineEnd && note.lineEnd !== note.lineStart ? `${note.lineStart}-${note.lineEnd}` : String(note.lineStart);
}

/** Every line inside a note's range, in order, from a `${side}:${line}` keyed map. */
export function reviewRangeCode(
  texts: Map<string, string>,
  side: ReviewSide,
  lineStart: number,
  lineEnd: number | null,
): string {
  const last = lineEnd ?? lineStart;
  const collected: Array<[number, string]> = [];
  for (const [key, text] of texts) {
    const separator = key.indexOf(":");
    if (key.slice(0, separator) !== side) continue;
    const line = Number(key.slice(separator + 1));
    if (line >= lineStart && line <= last) collected.push([line, text]);
  }
  return collected
    .sort((first, second) => first[0] - second[0])
    .map(([, text]) => text)
    .join("\n");
}

function quote(text: string): string {
  return text
    .trim()
    .split("\n")
    .map((line) => `> ${line}`.trimEnd())
    .join("\n");
}

/** Markdown export compatible with the CodeReview.nvim default format. */
export function buildReviewMarkdown(notes: ReviewNote[], context: ReviewContext = {}): string {
  if (notes.length === 0) return "";
  const paths = [...new Set(notes.map((note) => note.path))].sort((first, second) => first.localeCompare(second));
  const range = [context.defaultBranch, context.branch].filter(Boolean).join("..") || "working tree";
  const heading = context.date ?? new Date().toISOString().slice(0, 10);
  const sections = paths.map((path) => {
    const language = reviewFenceLanguage(path);
    const blocks = notes
      .filter((note) => note.path === path)
      .sort((first, second) => first.lineStart - second.lineStart)
      .map((note) => {
        const label = reviewLineRange(note);
        const code = note.code ? `\`\`\`${language}{${label}}\n${note.code}\n\`\`\`\n` : "";
        return `${code}\n${quote(note.content)}`;
      });
    return `## ${path}\n\n${blocks.join("\n\n---\n\n")}`;
  });
  return [
    `# Code Review ${heading}`,
    "",
    `> \`${range}\` — ${paths.length} ${paths.length === 1 ? "file" : "files"}, ${notes.length} ${notes.length === 1 ? "note" : "notes"}`,
    "",
    sections.join("\n\n"),
    "",
  ].join("\n");
}

/** One line of a diff: its code, and the sign the gutter marks it with. */
export interface DiffLine {
  text: string;
  /** "+" for a line the diff added, "-" for one it removed, " " for one it left alone. */
  mark: "+" | "-" | " ";
}

/**
 * Every line behind both sides of a unified patch, keyed by `${side}:${line}`.
 *
 * The sign travels with the code because it cannot be recovered from it: old line 9 and new line
 * 9 are two different lines, so a number on both sides says nothing about whether either was added
 * or removed.
 */
export function readDiffLines(patch: string): Map<string, DiffLine> {
  const lines = new Map<string, DiffLine>();
  let oldLine = 0;
  let newLine = 0;
  for (const line of patch.split("\n")) {
    const hunk = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(line);
    if (hunk) {
      oldLine = Number(hunk[1]);
      newLine = Number(hunk[2]);
    } else if (line.startsWith("+")) {
      lines.set(`new:${newLine}`, { text: line.slice(1), mark: "+" });
      newLine += 1;
    } else if (line.startsWith("-")) {
      lines.set(`old:${oldLine}`, { text: line.slice(1), mark: "-" });
      oldLine += 1;
    } else if (line.startsWith(" ")) {
      const context: DiffLine = { text: line.slice(1), mark: " " };
      lines.set(`old:${oldLine}`, context);
      lines.set(`new:${newLine}`, context);
      oldLine += 1;
      newLine += 1;
    }
  }
  return lines;
}

/** Whether a note still needs to reach the agent. */
export function isReviewableNote(note: ReviewNote): boolean {
  return note.status === "draft" || note.status === "sent";
}

/**
 * The verdict for a note after the agent's turn, from what the diff can prove.
 *
 * `unchanged` and `missing` are both "the agent's turn is over and this note can be
 * resolved", but they are reported apart because a deleted line is worth saying out loud.
 */
export function anchorOutcome(outcome: AnchorOutcome): {
  resolvable: boolean;
  message: string;
} {
  switch (outcome) {
    case "unchanged":
      return { resolvable: true, message: "The line this note points at is unchanged." };
    case "missing":
      return { resolvable: true, message: "The line this note points at no longer exists." };
    case "changed":
      return { resolvable: false, message: "The line changed, so this note is outdated." };
    default:
      return { resolvable: false, message: "Not enough of this diff is loaded to tell." };
  }
}
