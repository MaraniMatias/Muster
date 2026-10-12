import { processAST } from "@git-diff-view/vue";
import type { DiffAST, DiffFileHighlighter } from "@git-diff-view/vue";
import type { HighlighterCore } from "shiki/core";
import { languageForName, loadSourceHighlighter } from "./source-highlighter";

/**
 * Shiki's `FontStyle` is a `declare const enum` in a package that re-exports it as a type only, so
 * the flags it sets are named here rather than imported. `NotSet` is `-1`, which has every bit set:
 * that is "the grammar says nothing", and it must not be read as italic and bold both.
 */
const ITALIC = 1;
const BOLD = 2;
const UNDERLINE = 4;
const STRIKETHROUGH = 8;

/** Shiki's `Token`, which is what `codeToTokens` gives one run of same-colored text. */
interface Token {
  content: string;
  color?: string;
  fontStyle?: number;
}

/**
 * The two limits one side of a file is read into tokens under, which are the two shapes a file can
 * be too big in rather than two ways of saying the same one:
 *
 * - bytes, which is what the reading's time and its tokens both come from, and the size the backend
 *   ships a side in, so nothing is read here that was not worth sending. It is the one that holds for
 *   a file of few long lines, such as the ones a formatter has put one statement to a line on;
 * - lines, which is how deep a file is read whole, and the count the library gates a diff on. It is
 *   the one that holds for a file of many short ones, which the bytes never notice.
 *
 * Past either one the side is not read and the diff keeps what the library draws on its own. Neither
 * is a limit on where in a file a change may be: the tokens are the file's own, so a hunk on line
 * 2500 is read as the 2500th line of a file rather than as the first line of a fragment.
 */
const MAX_SYNTAX_BYTES = 512 * 1024;
const MAX_SYNTAX_LINES = 8_000;

/** What a diff is told its highlighter is called, which is also what the browser is told. */
const HIGHLIGHTER_NAME = "muster-shiki";

/** One side of a file, read into the lines a diff window is cut out of. */
interface SideContext {
  /** Shiki's tokens, one entry per line of the file, in the order the file reads. */
  lines: readonly (readonly Token[])[];
  /** What each of those lines says, which is what a window is checked against before it is painted. */
  texts: readonly string[];
}

/** What a line with nothing in it is read as, since a blank line is still a line. */
const BLANK: readonly Token[] = [{ content: "" }];

/**
 * Loaded grammars, by the name Muster calls the language, and the loads in flight for them.
 *
 * The only state this module keeps. A grammar is a parser and nothing here can make it say anything
 * untrue, so one is shared by every diff that is opened; what a diff says is not shared, because two
 * checkouts hold two files under the same name and two versions of one file are two answers.
 */
const loaded = new Map<string, HighlighterCore>();
const loading = new Map<string, Promise<boolean>>();

/**
 * The style attribute for one token.
 *
 * The color is a CSS variable Shiki resolved against Muster' own theme, so it is a `var(...)` by
 * construction and never anything a file could have put there. The declarations are joined by hand
 * because the library reads `style` as a string rather than a property bag.
 */
function styleFor(token: Token): string {
  const declarations: string[] = [];
  if (token.color) declarations.push(`color:${token.color}`);
  // `-1` is the grammar saying nothing, and every bit of it is set, so it is not read as a style.
  if (token.fontStyle !== undefined && token.fontStyle >= 0) {
    if (token.fontStyle & ITALIC) declarations.push("font-style:italic");
    if (token.fontStyle & BOLD) declarations.push("font-weight:bold");
    if (token.fontStyle & UNDERLINE) declarations.push("text-decoration:underline");
    if (token.fontStyle & STRIKETHROUGH) declarations.push("text-decoration:line-through");
  }
  return declarations.join(";");
}

/** One span, which is what the library walks and what carries a token's color to the browser. */
function span(token: Token, value: string): unknown {
  return {
    type: "element",
    tagName: "span",
    properties: { className: [], style: styleFor(token) },
    children: [{ type: "text", value }],
  };
}

/** What a run of tokens says, which is the line they are the only tokens of. */
function textOf(tokens: readonly Token[]): string {
  return tokens.map((token) => token.content).join("");
}

/** The line a carriage return leaves off a diff's own text. */
function withoutCarriageReturn(line: string): string {
  return line.endsWith("\r") ? line.slice(0, -1) : line;
}

/**
 * `source` cut to its first `lines` lines, which is all a diff that reaches no further can be asked
 * about, or all of it when `lines` says nothing.
 *
 * Newlines included, so what is read is the same text the file starts with rather than a shorter
 * file that happens to end mid-line.
 */
function firstLines(source: string, lines: number | undefined): string {
  if (lines === undefined || lines <= 0) return source;
  let at = -1;
  for (let seen = 0; seen < lines; seen += 1) {
    at = source.indexOf("\n", at + 1);
    if (at < 0) return source;
  }
  return source.slice(0, at + 1);
}

/**
 * What `source` says, read into the lines a diff window is cut out of, or nothing.
 *
 * The whole of one side is read, not a hunk of it: a hunk is a fragment, and a fragment is read by
 * a grammar that never saw the file it came from, which for a `.vue` hunk inside
 * `<script setup lang="ts">` means the TypeScript in it is markup, painted as markup. `lines` is how
 * far into the file the diff reaches, which is as far as a grammar is asked anything.
 */
function readSide(
  instance: HighlighterCore,
  grammar: string,
  source: string,
  lines: number | undefined,
): SideContext | undefined {
  const text = firstLines(source, lines);
  if (isOverByteCap(text) || countLines(text) > MAX_SYNTAX_LINES) return undefined;
  let read: readonly (readonly Token[])[];
  try {
    read = instance.codeToTokens(text, { lang: grammar, theme: "muster" }).tokens;
  } catch {
    // A grammar that throws on this input is not read at all, which leaves the diff to the library's
    // own highlighter rather than to a file the grammar has already had its way with.
    return undefined;
  }
  return { lines: read, texts: read.map(textOf) };
}

/**
 * Whether `source` weighs more than the cap, which is a question about bytes and not about
 * characters: `é` is one character and two bytes, so a file of a third of the cap in characters can
 * still be over it. Three bytes is the most one character weighs in UTF-8, so under a third of the
 * cap in characters it cannot be over, and no encoding is needed to know that.
 */
function isOverByteCap(source: string): boolean {
  return source.length * 3 > MAX_SYNTAX_BYTES && new TextEncoder().encode(source).byteLength > MAX_SYNTAX_BYTES;
}

/** How many lines `source` has, which is what a diff is counted in and is counted without splitting. */
function countLines(source: string): number {
  let lines = 1;
  for (let at = source.indexOf("\n"); at >= 0; at = source.indexOf("\n", at + 1)) lines += 1;
  return lines;
}

/**
 * The tokens of the window `raw` holds, as the tree the library walks.
 *
 * A diff is one `DiffFile` per hunk with no file content of its own, so the library rebuilds each
 * side of that hunk out of the patch: the lines of the hunk at their own numbers, and a bare newline
 * in every line before it. Line N of what it hands over is line N of the file, which is what lets
 * tokens read from the whole file be cut to this window instead of the window being parsed as a
 * file of its own, and what makes the library's line cap, measured against those placeholders, a
 * limit on how deep in a file a change may be rather than on how much is worth reading.
 *
 * A window line the diff has no text for is one the library drew as an empty line, or one it drew
 * nothing at all, and both are drawn empty. So that is all it is given, whatever the file says at
 * that number. Every line it does have text for has to be the line the tokens were read from: a diff
 * draws its text from the patch and takes its colors from here, and the two were read from the same
 * file moments apart, so a file that is not the same one anymore is one whose lines these tokens are
 * not of, and handing back nothing is what leaves the library to draw them its own way instead.
 */
function astForWindow(context: SideContext, window: string[]): DiffAST | undefined {
  const children: unknown[] = [];
  const last = window.length - 1;
  for (let index = 0; index <= last; index += 1) {
    const raw = window[index];
    const text = context.texts[index];
    let tokens: readonly Token[] = BLANK;
    if (raw !== "") {
      if (text !== raw && (text === undefined || text !== withoutCarriageReturn(raw))) return undefined;
      const line = context.lines[index];
      if (line && line.length > 0) tokens = line;
    }
    tokens.forEach((token, position) => {
      const endsLine = position === tokens.length - 1 && index !== last;
      children.push(span(token, endsLine ? `${token.content}\n` : token.content));
    });
  }
  return { type: "root", children } as DiffAST;
}

/** The side a diff view is asking about, which the name of the file it was given says. */
function sideFor(fileName: string | undefined): "old" | "new" | undefined {
  if (!fileName || fileName.length < 3 || fileName[1] !== "/") return undefined;
  if (fileName[0] === "a") return "old";
  return fileName[0] === "b" ? "new" : undefined;
}

/**
 * The highlighter for one diff of one file at one version, closing over that version's tokens.
 *
 * The tokens are held by this object and by nothing else, which is what keeps two checkouts that
 * hold `src/App.vue` apart: the path a diff was opened for says nothing about which checkout it came
 * from, and two of those files are two answers to the same question. The library caches its own
 * reading of a window by the text of that window, so the `DiffFile`s a diff is drawn from also carry
 * an identity of their own (`FileDiff.vue` gives each set one) and this object's identity is the
 * third thing that has to be its own, which is why there is one of these per diff rather than one.
 */
function createHighlighter(sides: { old?: SideContext; new?: SideContext }): DiffFileHighlighter {
  return {
    name: HIGHLIGHTER_NAME,
    // The type is what tells the library a palette switch need not re-tokenize: the tokens name
    // variables, not colors, so they are already right for whichever palette is in effect.
    type: "class",
    maxLineToIgnoreSyntax: MAX_SYNTAX_LINES,
    setMaxLineToIgnoreSyntax(value: number) {
      this.maxLineToIgnoreSyntax = value;
    },
    ignoreSyntaxHighlightList: [],
    setIgnoreSyntaxHighlightList(value: (string | RegExp)[]) {
      this.ignoreSyntaxHighlightList = value;
    },
    /**
     * Whether this highlighter can tokenize `lang` right now, which the library asks before every
     * file and which decides the fallback. It is keyed on a loaded grammar rather than on a list of
     * known names, so a language whose grammar failed to load is the library's to answer for.
     */
    hasRegisteredCurrentLang: (lang: string) => loaded.has(lang),
    // The library's own type says this always returns a tree, while its code guards on it not doing
    // so (`if (!this.ast) return;`), so the cast is the library's type being narrower than the
    // library. Handing back nothing is what leaves a side to whatever the library had already drawn
    // for it, which is its own reading of the same window rather than half of ours.
    getAST: ((raw: string, fileName?: string) => {
      const side = sideFor(fileName);
      const context = side ? sides[side] : undefined;
      return context ? astForWindow(context, raw.split("\n")) : undefined;
    }) as DiffFileHighlighter["getAST"],
    processAST,
  };
}

/**
 * Reads the grammar `language` is highlighted with, and the two sides of one file that grammar reads,
 * and answers with a highlighter that closes over them.
 *
 * `lines` is how far into the file the diff reaches, and only that much of each side is read: the
 * library builds a hunk's window out of the file's lines from the first to the one that hunk ends on,
 * so a change near the top of a large file is read as the top of that file and not as all of it.
 *
 * Resolves to nothing when there is nothing to highlight with, which is also what decides the
 * fallback: a language Muster has no grammar for, a side the backend had no text for, and a side past
 * one of the limits above all leave the library to highlight the file its own way, which is what it
 * has always done with a file it has no grammar for.
 *
 * Nothing is published anywhere. What comes back belongs to the caller alone, so an answer that
 * arrives after the diff that asked for it has been left behind cannot reach the diff that replaced
 * it: there is no shared state for it to change.
 */
export async function prepareDiffHighlighting(
  language: string | undefined,
  contents: { old?: string; new?: string },
  lines?: number,
): Promise<DiffFileHighlighter | undefined> {
  const read = language === undefined ? undefined : await loadGrammar(language);
  if (!read) return undefined;
  const old = contents.old === undefined ? undefined : readSide(read.instance, read.grammar, contents.old, lines);
  const added = contents.new === undefined ? undefined : readSide(read.instance, read.grammar, contents.new, lines);
  return old || added ? createHighlighter({ old, new: added }) : undefined;
}

/**
 * The grammar a language is highlighted with, loaded once however many diffs of it are open.
 *
 * Named by Muster rather than by Shiki, because that is the name the library asks about: a file is
 * read as the language its path detected, and `gitignore` is the name of a grammar Shiki calls `ini`.
 */
async function loadGrammar(language: string): Promise<{ instance: HighlighterCore; grammar: string } | undefined> {
  const known = languageForName(language);
  if (!known) return undefined;
  const instance = await loadOnce(language);
  return instance ? { instance, grammar: known.shikiName ?? known.name } : undefined;
}

/** The one load of `language`, however many diffs of it ask for it at the same moment. */
async function loadOnce(language: string): Promise<HighlighterCore | undefined> {
  const cached = loaded.get(language);
  if (cached) return cached;
  const inFlight = loading.get(language);
  if (inFlight) return (await inFlight) ? loaded.get(language) : undefined;
  const request = loadSourceHighlighter(language)
    .then((instance) => {
      loaded.set(language, instance);
      return true;
    })
    .catch(() => false)
    .finally(() => loading.delete(language));
  loading.set(language, request);
  return (await request) ? loaded.get(language) : undefined;
}
