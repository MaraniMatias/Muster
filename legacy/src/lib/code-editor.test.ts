// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { forceParsing, syntaxTree } from "@codemirror/language";
import { redo, undo } from "@codemirror/commands";
import { EditorSelection, Text } from "@codemirror/state";
import type { EditorView } from "@codemirror/view";
import type { IndentationSettings } from "../domain/settings";
import {
  createCodeEditor,
  setEditorChangedLines,
  setEditorIndentation,
  setEditorText,
  CHANGED_LINE_CLASS,
  FRONT_MATTER_SCAN_LINES,
} from "./code-editor";

const views = new Set<EditorView>();
const hosts = new Set<HTMLElement>();
let initialStyles = new Set<HTMLStyleElement>();

beforeEach(() => {
  initialStyles = new Set(document.querySelectorAll("style"));
});

afterEach(() => {
  for (const view of views) view.destroy();
  views.clear();
  for (const host of hosts) host.remove();
  hosts.clear();
  for (const style of document.querySelectorAll("style")) {
    if (!initialStyles.has(style)) style.remove();
  }
});

function mount(language: string, content: string): EditorView {
  const host = document.createElement("div");
  hosts.add(host);
  document.body.appendChild(host);
  const view = createCodeEditor({
    parent: host,
    content,
    language,
    readingPosition: { top: 0, left: 0 },
    onChange: () => {},
    onScroll: () => {},
  });
  views.add(view);
  return view;
}

/**
 * The token the highlighter produced for each line, keyed by the text it covers, so an assertion
 * can name the piece of a gitignore it is about rather than a position that shifts with the file.
 */
function tokensByLine(view: EditorView): Map<string, string[]> {
  const tokens = new Map<string, string[]>();
  syntaxTree(view.state).iterate({
    enter: (node) => {
      if (node.name === "Document") return;
      const text = view.state.sliceDoc(node.from, node.to);
      const named = tokens.get(text);
      if (named) named.push(node.name);
      else tokens.set(text, [node.name]);
    },
  });
  return tokens;
}

/**
 * The color the editor actually painted a token with, read back out of the injected stylesheet. It
 * is whatever the rule carries: a hex, or the `var(--muster-syntax-*)` the theme resolves.
 */
function colorForSpan(span: Element): string | undefined {
  const classes = span.className.split(/\s+/);
  if (classes.length === 0) return undefined;
  const css = Array.from(document.querySelectorAll("style"))
    .map((style) => style.textContent ?? "")
    .join("}");
  for (const block of css.split("}")) {
    const rule = block.match(/^\s*\.(.+?)\s*\{\s*(?:color:\s*(#[0-9a-f]{3,8}|var\(--muster-[a-z0-9-]+\)))?/i);
    if (rule?.[2] && rule[1].split(/\s+/).some((name) => classes.includes(name))) return rule[2];
  }
  return undefined;
}

function colorPaintedOn(view: EditorView, text: string): string | undefined {
  const span = Array.from(view.contentDOM.querySelectorAll("span")).find((node) => node.textContent === text);
  return span ? colorForSpan(span) : undefined;
}

/** Every node name in the tree, so an assertion can name a construct instead of a position. */
function nodeNames(view: EditorView): string[] {
  const names: string[] = [];
  syntaxTree(view.state).iterate({
    enter: (node) => {
      names.push(node.name);
    },
  });
  return names;
}

describe("code editor", () => {
  it("reads a gitignore as comments, negations and globs, never as one long key", () => {
    const view = mount("gitignore", "# deps\nnode_modules\n!.env.example\n*.log");
    const tokens = tokensByLine(view);

    expect(tokens.get("# deps")).toEqual(["comment"]);
    // The `properties` mode that serves `.gitconfig` reads a bare glob as one long `def` run and
    // paints a leading `!` as a comment, which is the opposite of what a negation means.
    expect(tokens.get("node_modules")).toBeUndefined();
    expect(tokens.get("!")).toEqual(["keyword"]);
    expect(tokens.get("*")).toEqual(["operator"]);
    expect([...tokens.values()].flat().some((name) => name?.includes("definition"))).toBe(false);
  });

  it("does not read an escaped hash as a comment, since git only honours one at the line start", () => {
    const view = mount("gitignore", "\\#not-a-comment");
    expect(tokensByLine(view).get("\\#not-a-comment")).toBeUndefined();
  });

  it("reads the metadata a Markdown document opens with as the YAML it is", () => {
    const view = mount(
      "markdown",
      ["---", "name: tamis", 'color: "#F2B84B"', "# a note", "---", "", "# Tamis", "", "Body."].join("\n"),
    );
    expect(forceParsing(view, view.state.doc.length)).toBe(true);

    // A key and its value are YAML, so they take the colors YAML gives them rather than the flat
    // text the Markdown grammar would leave the block as. The comment is YAML's too.
    expect(colorPaintedOn(view, "name")).toBe("var(--muster-syntax-token-constant)");
    expect(colorPaintedOn(view, "# a note")).toBe("var(--muster-syntax-token-comment)");

    // The document behind the block is still read as Markdown: a heading is still a heading.
    expect(nodeNames(view)).toContain("ATXHeading1");
  });

  it("keeps CRLF front matter and following Markdown aligned", () => {
    const view = mount("markdown", ["---", "name: tamis", "...", "# Tamis"].join("\r\n"));

    expect(colorPaintedOn(view, "name")).toBe("var(--muster-syntax-token-constant)");
    expect(nodeNames(view)).toContain("ATXHeading1");
  });

  it("avoids splitting a huge Markdown document when it has no front matter", () => {
    const source = ["# Title", ...Array.from({ length: 12_000 }, (_, index) => "Body " + index)].join("\n");
    const view = mount("markdown", source);
    const documentLength = view.state.doc.length;
    const split = vi.spyOn(String.prototype, "split");

    try {
      view.dispatch({ changes: { from: documentLength, insert: "!" } });

      expect(documentLength).toBeGreaterThan(100_000);
      expect(
        split.mock.contexts.some((context) => typeof context === "string" && context.length >= documentLength),
      ).toBe(false);
    } finally {
      split.mockRestore();
    }
  });

  it("leaves a document that only opens with a rule alone", () => {
    // A rule with nothing to close it is not metadata, so nothing in it is read as YAML: the words
    // stay the flat text the Markdown grammar leaves them, and the rule is a rule.
    const view = mount("markdown", "---\nname: thing\n\n# Heading");
    expect(forceParsing(view, view.state.doc.length)).toBe(true);

    expect(colorPaintedOn(view, "name")).toBeUndefined();
    expect(nodeNames(view)).toContain("HorizontalRule");
    expect(nodeNames(view)).toContain("ATXHeading1");
  });

  it("stops looking for a closing fence at a bound instead of at the end of the file", () => {
    // A document that opens with a fence and never closes it is prose, and looking for the fence
    // that would say so used to cost a walk to the end of the file on every keystroke. This counts
    // the lines that walk asked about: it stops at the bound whatever the file's length, so a
    // 20,000-line document costs what a 2,000-line one does.
    const source = ["---", ...Array.from({ length: 20_000 }, (_, index) => `body ${index}`)].join("\n");
    const view = mount("markdown", source);
    const original = Text.prototype.line;
    const asked = new Set<string>();
    const line = vi.spyOn(Text.prototype, "line");

    try {
      line.mockImplementation(function (this: Text, at: number) {
        asked.add(String(at));
        return original.call(this, at);
      });
      view.dispatch({ changes: { from: view.state.doc.length, insert: "!" } });

      expect(view.state.doc.lines).toBeGreaterThan(20_000);
      // The furthest line the fence search may reach, and the one after it.
      expect(Math.max(...[...asked].map(Number))).toBe(FRONT_MATTER_SCAN_LINES);
      expect(asked.has(String(FRONT_MATTER_SCAN_LINES + 1))).toBe(false);
      // And nothing in the unclosed block is decoration: it is the flat text Markdown leaves it.
      expect(colorPaintedOn(view, "body 0")).toBeUndefined();
    } finally {
      line.mockRestore();
    }
  });

  it("reads a block whose closing fence is at the bound, and leaves one past it plain", () => {
    // The bound is where the search gives up, not where a block stops being metadata: a closing
    // fence on the last line it may look at is YAML like any other, and one a line further is not
    // found at all. Both blocks are the same length apart by a single line, and both are read by
    // their first line, which is the one a test can see.
    const block = (closingAt: number) =>
      ["---", ...Array.from({ length: closingAt - 2 }, (_, index) => `key${index}: value`), "..."].join("\n");

    const atBound = mount("markdown", block(FRONT_MATTER_SCAN_LINES));
    expect(forceParsing(atBound, atBound.state.doc.length)).toBe(true);
    expect(colorPaintedOn(atBound, "key0")).toBe("var(--muster-syntax-token-constant)");

    const pastBound = mount("markdown", block(FRONT_MATTER_SCAN_LINES + 1));
    expect(forceParsing(pastBound, pastBound.state.doc.length)).toBe(true);
    expect(colorPaintedOn(pastBound, "key0")).toBeUndefined();
  });

  it("paints tokens in the palette the read-only preview already uses", () => {
    const view = mount("typescript", "const answer: number = 42;\n// note");

    // The tokens are the ones the Shiki theme names for the same grammar, so a file stops changing
    // color when it becomes editable — and `var()` rather than a hex, which is what makes the same
    // markup readable in either palette without a second render.
    expect(colorPaintedOn(view, "const")).toBe("var(--muster-syntax-token-keyword)");
    expect(colorPaintedOn(view, "answer")).toBe("var(--muster-syntax-foreground)");
    expect(colorPaintedOn(view, "number")).toBe("var(--muster-syntax-token-string-expression)");
    expect(colorPaintedOn(view, "42")).toBe("var(--muster-syntax-token-constant)");
    expect(colorPaintedOn(view, "// note")).toBe("var(--muster-syntax-token-comment)");
  });

  it("paints every token with a token, and with no palette of its own", () => {
    // Nothing injects `defaultHighlightStyle` as a fallback and this list names no color, so a hex
    // here is a palette that survived the change: it would be unreadable in one of the two themes,
    // and it would only show up in a language nobody happened to open while this was written.
    const documents = [
      ["typescript", "const answer: number = 42;\n// note"],
      ["python", "def answer():\n    return 42"],
      ["json", '{"answer": 42, "nested": {"deep": [true, null]}}'],
      ["yaml", "answer: 42\nnested:\n  deep: true"],
      ["markdown", ["---", "name: tamis", "---", "", "# Title", "", "- item", "> quote"].join("\n")],
      ["gitignore", "# deps\nnode_modules\n!.env.example\n*.log"],
    ] as const;

    for (const [language, content] of documents) {
      const view = mount(language, content);
      const painted = Array.from(view.contentDOM.querySelectorAll("span"))
        .map((span) => colorForSpan(span))
        .filter((color): color is string => color !== undefined);

      expect({ language, painted }).toEqual({
        language,
        painted: painted.filter((color) => color.startsWith("var(--muster-")),
      });
    }
  });

  it("has the gutter and the undo history the extension list claims", () => {
    const view = mount("typescript", "const answer = 1;");

    expect(view.dom.querySelector(".cm-lineNumbers")).not.toBeNull();

    // The history is one line of the list nothing else pulls in, so a typo there is invisible
    // until someone types into the document and then cannot take it back.
    view.dispatch({ changes: { from: view.state.doc.length, insert: "// typed" } });
    expect(view.state.doc.toString()).toBe("const answer = 1;// typed");
    expect(undo(view)).toBe(true);
    expect(view.state.doc.toString()).toBe("const answer = 1;");
    expect(redo(view)).toBe(true);
    expect(view.state.doc.toString()).toBe("const answer = 1;// typed");
  });
});

describe("indentation", () => {
  /** The key CodeMirror's own Tab handling goes through, which is what a person pressing it hits. */
  function pressTab(view: EditorView, shiftKey = false) {
    view.contentDOM.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Tab", shiftKey, bubbles: true, cancelable: true }),
    );
  }

  function mountWith(indentation: IndentationSettings, content = "") {
    const host = document.createElement("div");
    hosts.add(host);
    document.body.appendChild(host);
    const view = createCodeEditor({
      parent: host,
      content,
      language: "plaintext",
      readingPosition: { top: 0, left: 0 },
      indentation,
      onChange: () => {},
      onScroll: () => {},
    });
    views.add(view);
    return view;
  }

  it("indents with the spaces the settings ask for", () => {
    const view = mountWith({ useSpaces: true, size: 2 });

    pressTab(view);

    // Without a Tab binding nothing happens at all, which is the failure this pins: the default
    // keymap leaves Tab unbound, so the preference would be a number in a file and not in a document.
    expect(view.state.doc.toString()).toBe("  ");
  });

  it("indents with a tab character when the settings ask for tabs", () => {
    const view = mountWith({ useSpaces: false, size: 4 });

    pressTab(view);

    expect(view.state.doc.toString()).toBe("\t");
  });

  it("removes one level with shift-Tab", () => {
    const view = mountWith({ useSpaces: true, size: 2 }, "    indented");

    view.dispatch({ selection: { anchor: view.state.doc.length } });
    pressTab(view, true);

    expect(view.state.doc.toString()).toBe("  indented");
  });

  it("moves the caret by the indent size, which is the same number", () => {
    const view = mountWith({ useSpaces: true, size: 4 });

    expect(view.state.tabSize).toBe(4);
  });

  it("reconfigures a document that is already open without losing what is in it", () => {
    const view = mountWith({ useSpaces: true, size: 2 }, "const answer = 1;");
    view.dispatch({ changes: { from: view.state.doc.length, insert: "// typed" } });
    view.dispatch({ selection: { anchor: 4 } });

    setEditorIndentation(view, { useSpaces: false, size: 8 });

    // A new editor would be the other way to apply this, and it would take the text, the caret and
    // the undo history with it to change the width of a tab.
    expect(view.state.doc.toString()).toBe("const answer = 1;// typed");
    expect(view.state.selection.main.anchor).toBe(4);
    expect(undo(view)).toBe(true);
    expect(view.state.doc.toString()).toBe("const answer = 1;");
    expect(view.state.tabSize).toBe(8);
    expect(redo(view)).toBe(true);

    // The new unit, at the line the caret is on, with the text the reconfigure had to preserve
    // still around it.
    view.dispatch({ selection: { anchor: 4 } });
    pressTab(view);
    expect(view.state.doc.toString()).toBe("\tconst answer = 1;// typed");
  });

  it("does nothing to a view this build did not build", () => {
    // The compartment is per editor, so an editor from elsewhere has none to reconfigure and the
    // call is a no-op rather than a throw from a component that has no way to know.
    expect(() => setEditorIndentation({} as EditorView, { useSpaces: false, size: 4 })).not.toThrow();
  });
});

describe("the lines the checkout has changed", () => {
  /** The text of every line the editor painted as changed, which is what a reader looks for. */
  function changedTexts(view: EditorView): string[] {
    return Array.from(view.contentDOM.querySelectorAll(`.${CHANGED_LINE_CLASS}`)).map((line) => line.textContent ?? "");
  }

  it("marks the lines it is given, and no others", () => {
    const view = mount("typescript", ["const a = 1;", "const b = 2;", "const c = 3;", "const d = 4;"].join("\n"));

    setEditorChangedLines(view, [{ start: 2, end: 3 }]);

    expect(changedTexts(view)).toEqual(["const b = 2;", "const c = 3;"]);
  });

  it("marks nothing before it is told about anything", () => {
    // The preview asks Git about a file separately from reading it, and usually after the editor
    // is already up. An editor that marked something on its own would be inventing a change.
    const view = mount("typescript", "const a = 1;\nconst b = 2;");

    expect(changedTexts(view)).toEqual([]);
  });

  it("carries the mark into the gutter as well as onto the line", () => {
    const view = mount("typescript", "const a = 1;\nconst b = 2;");

    setEditorChangedLines(view, [{ start: 2, end: 2 }]);

    // The line itself can lose the tint to the editor's active-line background, so the dot is what
    // carries the mark on the line somebody is standing on.
    expect(view.dom.querySelector(".muster-changed-line-marker")).not.toBeNull();
  });

  it("keeps a mark on its own line as the text above it is edited", () => {
    const view = mount("typescript", ["const a = 1;", "const b = 2;", "const c = 3;"].join("\n"));
    setEditorChangedLines(view, [{ start: 3, end: 3 }]);

    view.dispatch({ changes: { from: 0, insert: "const z = 0;\n" } });

    // The mark is a position in the document, so inserting above it has to move it down rather than
    // leave it sitting on whatever slid into its place.
    expect(changedTexts(view)).toEqual(["const c = 3;"]);
  });

  it("leaves off a mark for a line the file no longer has", () => {
    // Git counted the file before it was shortened, so a range can outrun the document. Asking the
    // builder for a line that is not there would throw in the middle of a paint.
    const view = mount("typescript", "const a = 1;");

    expect(() => setEditorChangedLines(view, [{ start: 1, end: 90 }])).not.toThrow();
    expect(changedTexts(view)).toEqual(["const a = 1;"]);
  });

  it("replaces the marks it had with the ones it is given", () => {
    const view = mount("typescript", ["const a = 1;", "const b = 2;", "const c = 3;"].join("\n"));
    setEditorChangedLines(view, [{ start: 1, end: 1 }]);

    setEditorChangedLines(view, [{ start: 3, end: 3 }]);

    expect(changedTexts(view)).toEqual(["const c = 3;"]);
  });

  it("takes the marks away when the file turns out to be unchanged", () => {
    const view = mount("typescript", "const a = 1;\nconst b = 2;");
    setEditorChangedLines(view, [{ start: 2, end: 2 }]);

    setEditorChangedLines(view, []);

    expect(changedTexts(view)).toEqual([]);
    expect(view.dom.querySelector(".muster-changed-line-marker")).toBeNull();
  });

  it("does nothing to a view this build did not build", () => {
    expect(() => setEditorChangedLines({} as EditorView, [{ start: 1, end: 1 }])).not.toThrow();
  });
});

describe("setEditorText", () => {
  it("puts the new text in and leaves the reader where they were", () => {
    const view = mount("typescript", "const a = 1;\nconst b = 2;");
    view.dispatch({ selection: { anchor: 21, head: 21 } });

    setEditorText(view, "const a = 1;\nconst b = 2000;");

    expect(view.state.doc.toString()).toBe("const a = 1;\nconst b = 2000;");
    expect(view.state.selection.main.head).toBe(21);
  });

  it("clamps a position the shorter text no longer has", () => {
    const view = mount("typescript", "const a = 1;\nconst b = 2;");
    view.dispatch({ selection: { anchor: 23, head: 23 } });

    setEditorText(view, "const a = 1;\n");

    expect(view.state.selection.main.head).toBe(view.state.doc.length);
  });

  it("keeps every range of a multi-cursor selection", () => {
    const view = mount("typescript", "const a = 1;\nconst b = 2;");
    view.dispatch({ selection: EditorSelection.create([EditorSelection.cursor(6), EditorSelection.cursor(20)], 1) });

    setEditorText(view, "const a = 1;\nconst b = 22;");

    expect(view.state.selection.ranges.map((range) => range.head)).toEqual([6, 20]);
    expect(view.state.selection.mainIndex).toBe(1);
  });

  it("wakes nobody when the text is what the editor already holds", () => {
    const onChange = vi.fn();
    const host = document.createElement("div");
    hosts.add(host);
    document.body.appendChild(host);
    const view = createCodeEditor({
      parent: host,
      content: "const a = 1;",
      language: "typescript",
      readingPosition: { top: 0, left: 0 },
      onChange,
      onScroll: () => {},
    });
    views.add(view);
    view.dispatch({ selection: { anchor: 5, head: 5 } });

    setEditorText(view, "const a = 1;");

    expect(onChange).not.toHaveBeenCalled();
    expect(view.state.selection.main.head).toBe(5);
  });
});
