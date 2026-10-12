import { cpp } from "@codemirror/lang-cpp";
import { css } from "@codemirror/lang-css";
import { html } from "@codemirror/lang-html";
import { java } from "@codemirror/lang-java";
import { javascript } from "@codemirror/lang-javascript";
import { json } from "@codemirror/lang-json";
import { markdown } from "@codemirror/lang-markdown";
import { python } from "@codemirror/lang-python";
import { rust } from "@codemirror/lang-rust";
import { sql } from "@codemirror/lang-sql";
import { xml } from "@codemirror/lang-xml";
import { yaml } from "@codemirror/lang-yaml";
import {
  StreamLanguage,
  syntaxHighlighting,
  HighlightStyle,
  foldGutter,
  foldKeymap,
  indentOnInput,
  indentUnit,
  bracketMatching,
  type StringStream,
} from "@codemirror/language";
import {
  Compartment,
  EditorSelection,
  EditorState,
  RangeSet,
  RangeSetBuilder,
  StateField,
  type Extension,
  type Text,
} from "@codemirror/state";
import { closeBrackets, closeBracketsKeymap } from "@codemirror/autocomplete";
import { history, defaultKeymap, historyKeymap, indentWithTab, temporarilySetTabFocusMode } from "@codemirror/commands";
import {
  Decoration,
  EditorView,
  GutterMarker,
  crosshairCursor,
  drawSelection,
  dropCursor,
  gutter,
  highlightActiveLine,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
  rectangularSelection,
  type DecorationSet,
} from "@codemirror/view";
import { highlightTree } from "@lezer/highlight";
import { tags } from "@lezer/highlight";
import { csharp, kotlin, objectiveC } from "@codemirror/legacy-modes/mode/clike";
import { diff } from "@codemirror/legacy-modes/mode/diff";
import { dockerFile } from "@codemirror/legacy-modes/mode/dockerfile";
import { go } from "@codemirror/legacy-modes/mode/go";
import { gql } from "@codemirror/legacy-modes/mode/sql";
import { http } from "@codemirror/legacy-modes/mode/http";
import { nginx } from "@codemirror/legacy-modes/mode/nginx";
import { lua } from "@codemirror/legacy-modes/mode/lua";
import { perl } from "@codemirror/legacy-modes/mode/perl";
import { powerShell } from "@codemirror/legacy-modes/mode/powershell";
import { properties } from "@codemirror/legacy-modes/mode/properties";
import { ruby } from "@codemirror/legacy-modes/mode/ruby";
import { shell } from "@codemirror/legacy-modes/mode/shell";
import { swift } from "@codemirror/legacy-modes/mode/swift";
import { toml } from "@codemirror/legacy-modes/mode/toml";
import { FRONT_MATTER_FENCE, opensFrontMatter } from "./front-matter";
import type { LineRange } from "./changed-lines";
import type { IndentationSettings } from "../domain/settings";

export interface CodeEditorOptions {
  parent: HTMLElement;
  content: string;
  language: string | null;
  readingPosition: { top: number; left: number };
  /** What one level of indent is. It arrives with the rest of the settings and is applied to a live editor below. */
  indentation?: IndentationSettings;
  onChange: (content: string) => void;
  onScroll: (position: { top: number; left: number }) => void;
}

/**
 * The syntax tokens from `src/muster.css`, named the way Shiki's own theme names them so the same
 * declaration paints a read-only file and an editable one, and the way `HighlightStyle` takes them:
 * a custom property rather than a color, which is the whole of how this follows the theme.
 */
const syntax = (token: string) => `var(--muster-syntax-token-${token})`;

/**
 * `defaultHighlightStyle` is a light palette (dark red keywords, mid-blue strings) laid over
 * whatever editor background is in effect, where the darker half of it is barely readable in the
 * light theme and washed out in the dark one. These are the tokens the read-only preview already
 * renders the same file with, so a file stops changing color when it becomes editable.
 *
 * This has to name every tag a readable color is wanted for, and it is the only palette in play:
 * nothing injects `defaultHighlightStyle` as a fallback, so a tag missing here is painted in the
 * editor's own text color rather than in a light one.
 */
export const musterHighlightStyle = HighlightStyle.define([
  { tag: tags.comment, color: syntax("comment") },
  {
    tag: [tags.keyword, tags.controlKeyword, tags.moduleKeyword, tags.definitionKeyword, tags.operatorKeyword],
    color: syntax("keyword"),
  },
  { tag: [tags.string, tags.special(tags.string), tags.escape, tags.regexp], color: syntax("string") },
  {
    tag: [
      tags.atom,
      tags.bool,
      tags.literal,
      tags.null,
      tags.number,
      tags.propertyName,
      tags.definition(tags.propertyName),
      tags.constant(tags.variableName),
    ],
    color: syntax("constant"),
  },
  { tag: [tags.function(tags.variableName), tags.macroName], color: syntax("function") },
  // An identifier Zed leaves in the plain text color, which is what the read-only preview leaves it
  // in too; naming it here rather than omitting the tag keeps both views reading alike.
  {
    tag: [
      tags.variableName,
      tags.definition(tags.variableName),
      tags.labelName,
      tags.local(tags.variableName),
      tags.special(tags.variableName),
    ],
    color: "var(--muster-syntax-foreground)",
  },
  { tag: [tags.contentSeparator, tags.meta, tags.namespace], color: syntax("punctuation") },
  { tag: [tags.url, tags.link], color: syntax("link") },
  { tag: tags.inserted, color: syntax("inserted") },
  { tag: tags.deleted, color: syntax("deleted") },
  {
    tag: [tags.className, tags.tagName, tags.attributeName, tags.typeName, tags.quote],
    color: syntax("string-expression"),
  },
  { tag: tags.heading, color: syntax("function"), fontWeight: "bold" },
  { tag: tags.emphasis, fontStyle: "italic" },
  { tag: tags.strong, fontWeight: "bold" },
  { tag: tags.strikethrough, textDecoration: "line-through" },
  { tag: tags.invalid, color: syntax("deleted") },
]);

/**
 * A gitignore is a list of bare globs, which the `properties` mode the other INI files use reads as
 * one very long key, and a leading `!` negation as a comment, which is backwards. Only two
 * characters carry meaning: `#` opens a comment and must sit at the start of the line, and `!` at
 * that same spot re-includes a pattern. The glob metacharacters are marked so a `*.log` reads as
 * glob syntax rather than prose; everything else is a path, and stays in the base color.
 */
const gitignoreLanguage = StreamLanguage.define({
  name: "gitignore",
  token(stream: StringStream) {
    if (stream.sol()) {
      if (stream.eat("#")) {
        stream.skipToEnd();
        return "comment";
      }
      if (stream.eat("!")) return "keyword";
    }
    if (stream.eat(/[[\]*?]/)) return "operator";
    stream.next();
    return null;
  },
});

/**
 * What the editor needs for itself: a height it does not grow past, and a scroller that scrolls.
 *
 * The two colors CodeMirror hardcodes for a light page (a lavender selection, a black caret)
 * are not here: `EditorView.theme` refuses the selectors its own base theme uses (`&dark`), and a
 * shorter selector loses to that theme anyway. They live in `DocumentPane`'s stylesheet, which can
 * out-specify it.
 */
const musterTheme = EditorView.theme({
  "&": { height: "100%" },
  ".cm-scroller": { overflow: "auto" },
});

/**
 * What one level of indent is, as the two extensions that mean it.
 *
 * `indentUnit` is what Enter inserts and what the language's own indenter produces; the tab size
 * is what the caret moves by when it crosses a tab and what `shift-Tab` removes. They are the same
 * number to a person, so one preference sets both.
 */
function indentationExtension(indentation: IndentationSettings): Extension {
  const unit = indentation.useSpaces ? " ".repeat(indentation.size) : "\t";
  return [indentUnit.of(unit), EditorState.tabSize.of(indentation.size)];
}

/**
 * Tab indents, and Escape lets the keyboard leave.
 *
 * `indentWithTab` is what makes the preference visible: without it Tab does nothing at all in this
 * editor, because the default keymap leaves it unbound, and a document that indents on Enter but
 * not on Tab is the one place a person notices the difference between the two settings at once.
 * It indents with `indentUnit`, so the same two preferences decide whether that is two spaces or
 * a tab character.
 *
 * Binding Tab costs the one thing Tab is for, so the way out is one key: Escape puts the editor in
 * CodeMirror's tab-focus mode for a moment, and the next Tab moves the focus instead of the caret.
 * That is CodeMirror's own answer to the tradeoff rather than a shortcut invented here.
 */
const tabKeymap: Extension = keymap.of([indentWithTab, { key: "Escape", run: temporarilySetTabFocusMode }]);

/** The one compartment per editor, so the preference can change without rebuilding the document. */
const indentationCompartments = new WeakMap<EditorView, Compartment>();

/** The same for the lines Git says are changed, which arrive as the file is edited elsewhere. */
const changedLineCompartments = new WeakMap<EditorView, Compartment>();

/**
 * The gutter dot on a changed line.
 *
 * One marker for every line, drawn in whichever gutter cell the mark's position falls in. It is a
 * single shared instance on purpose: a gutter redraws itself whenever the document changes, and a
 * marker that built a fresh node each time would repaint every dot on every keystroke.
 */
class ChangedLineMarker extends GutterMarker {
  override toDOM(): HTMLElement {
    const dot = document.createElement("span");
    dot.className = "muster-changed-line-marker";
    return dot;
  }
}

const changedLineMarker = new ChangedLineMarker();

/**
 * The class a changed line wears, named in one place because the stylesheet, the editor and the
 * read-only renderer all have to agree on it — and a test has to ask for it by name.
 */
export const CHANGED_LINE_CLASS = "muster-changed-line";

/**
 * One set of marks, drawn two ways: as the gutter dot and as the line's tint.
 *
 * They come out of one field because they have to mean the same line. The marks are positions in
 * the document rather than the line numbers Git reported, and the field maps them through every
 * transaction — so typing above a change moves its dot and its tint down with the text instead of
 * leaving them behind on whatever slid into their place. The tint is read as a function of the view
 * for the same reason: a set captured when the ranges arrived would be a snapshot of a document that
 * has since been edited.
 */
function changedLinesExtension(ranges: readonly LineRange[]): Extension {
  const marks = StateField.define<RangeSet<GutterMarker>>({
    create: (state) => changedLineMarks(state.doc, ranges),
    update(markers, transaction) {
      // The ranges are a fact about the file as Git last read it, so an edit does not change them.
      return transaction.docChanged ? markers.map(transaction.changes) : markers;
    },
  });
  return [
    marks,
    gutter({ class: "muster-changed-gutter", markers: (view) => view.state.field(marks) }),
    EditorView.decorations.of((view) => changedLineTint(view.state.doc, view.state.field(marks))),
  ];
}

function changedLineMarks(doc: Text, ranges: readonly LineRange[]): RangeSet<GutterMarker> {
  if (ranges.length === 0) return RangeSet.empty;
  const builder = new RangeSetBuilder<GutterMarker>();
  for (const range of ranges) {
    // A file can have been shortened since Git counted it, so both ends are pinned to the document
    // rather than trusted: a line number past its end has no position to be a mark at.
    const last = Math.min(doc.lines, range.end);
    for (let number = Math.max(1, range.start); number <= last; number++) {
      const line = doc.line(number);
      builder.add(line.from, line.from, changedLineMarker);
    }
  }
  return builder.finish();
}

function changedLineTint(doc: Text, marks: RangeSet<GutterMarker>): DecorationSet {
  if (marks.size === 0) return Decoration.none;
  const builder = new RangeSetBuilder<Decoration>();
  marks.between(0, doc.length, (from) => {
    builder.add(from, from, Decoration.line({ class: CHANGED_LINE_CLASS }));
  });
  return builder.finish();
}

/**
 * Marks the lines this checkout has changed in the file this editor is showing.
 *
 * A compartment rather than a new editor for the same reason `setEditorIndentation` is one: the
 * alternative throws away the undo history, the scroll position and the cursor of a document
 * somebody is in the middle of reading, to repaint a gutter.
 */
export function setEditorChangedLines(view: EditorView, ranges: readonly LineRange[]): void {
  const compartment = changedLineCompartments.get(view);
  if (!compartment) return;
  view.dispatch({ effects: compartment.reconfigure(changedLinesExtension(ranges)) });
}

/**
 * Applies a new indentation to an editor that is already open.
 *
 * A compartment rather than a new editor because the alternative throws away the undo history, the
 * scroll position and the cursor of a document somebody is in the middle of reading, to change the
 * width of a tab.
 */
export function setEditorIndentation(view: EditorView, indentation: IndentationSettings): void {
  const compartment = indentationCompartments.get(view);
  if (!compartment) return;
  view.dispatch({ effects: compartment.reconfigure(indentationExtension(indentation)) });
}

/**
 * Puts text the reader did not type into an editor that is already open.
 *
 * The selection is carried across by hand because the change itself would not: replacing a whole
 * document maps every position inside it onto the start of the replacement, which is how formatting
 * a file would otherwise put the caret on line one. The clamped ranges are the same ones the reader
 * had, at the closest offsets the new text has to offer.
 *
 * Nothing is dispatched when the text is what the editor already holds, so the listener below is
 * not woken by a no-op and the undo history gains nothing.
 */
export function setEditorText(view: EditorView, text: string): void {
  if (view.state.doc.toString() === text) return;
  const { ranges, mainIndex } = view.state.selection;
  const end = text.length;
  view.dispatch({
    changes: { from: 0, to: view.state.doc.length, insert: text },
    selection: EditorSelection.create(
      ranges.map((range) => EditorSelection.range(Math.min(range.anchor, end), Math.min(range.head, end))),
      mainIndex,
    ),
  });
}

/**
 * The editor is imported by DocumentPane only after a Code view is opened. Shiki remains the
 * renderer for the read-only view, while this small CM6 setup owns editing, history, gutters, and
 * the language selected by the existing toolbar.
 */
export function createCodeEditor(options: CodeEditorOptions): EditorView {
  const indentation = new Compartment();
  const changedLines = new Compartment();
  const view = new EditorView({
    state: EditorState.create({
      doc: options.content,
      extensions: [
        // This is `basicSetup` written out, minus the search panel, the linter and the completion
        // popup. Those three are the only reason the `codemirror` package was a dependency, and
        // none of them is a thing this editor does: it draws source and takes typed text. Two
        // consequences worth keeping in mind before adding one back: the light palette
        // `basicSetup` also injects as a fallback does not come with it (so every readable color is
        // named in `musterHighlightStyle`), and a package that is not in `package.json` cannot be
        // imported here at all.
        lineNumbers(),
        highlightActiveLineGutter(),
        highlightSpecialChars(),
        drawSelection(),
        dropCursor(),
        rectangularSelection(),
        crosshairCursor(),
        highlightActiveLine(),
        foldGutter(),
        indentOnInput(),
        bracketMatching(),
        closeBrackets(),
        history(),
        EditorState.allowMultipleSelections.of(true),
        // Tab first, so it wins over anything in the maps below that also answers to it.
        tabKeymap,
        keymap.of([...closeBracketsKeymap, ...defaultKeymap, ...historyKeymap, ...foldKeymap]),
        // CodeMirror assumes a light page unless told otherwise, and the two things it paints
        // itself say so: a lavender selection and a black caret, both unreadable here. The
        // selection is drawn on its own layer, which the shell's `::selection` cannot reach.
        EditorView.darkTheme.of(true),
        musterTheme,
        indentation.of(indentationExtension(options.indentation ?? { useSpaces: true, size: 2 })),
        // Empty until the pane says which lines Git has changed, which it only asks about for a
        // file the checkout actually has open.
        changedLines.of(changedLinesExtension([])),
        languageExtension(options.language),
        syntaxHighlighting(musterHighlightStyle),
        EditorView.updateListener.of((update) => {
          if (update.docChanged) options.onChange(update.state.doc.toString());
        }),
        EditorView.domEventHandlers({
          scroll: (_event, editor) => {
            options.onScroll({ top: editor.scrollDOM.scrollTop, left: editor.scrollDOM.scrollLeft });
            return false;
          },
        }),
      ],
    }),
    parent: options.parent,
  });
  view.scrollDOM.scrollTop = options.readingPosition.top;
  view.scrollDOM.scrollLeft = options.readingPosition.left;
  indentationCompartments.set(view, indentation);
  changedLineCompartments.set(view, changedLines);
  return view;
}

/**
 * The metadata a Markdown document opens with, read as the YAML it is.
 *
 * The Markdown grammar has no front matter of its own (it sees a rule, a paragraph and another rule),
 * so the block is parsed with the YAML grammar over its own range and the result is laid over the
 * document as decorations. A grammar extension would have to be written into the Markdown parser,
 * which is where a mistake stops being a cosmetic one and starts being a broken editor; this cannot
 * do that, and it costs one parse of a handful of lines per edit.
 */
function frontMatterYaml(): Extension {
  return StateField.define<DecorationSet>({
    // Built here as well as on every edit: the field is created with the state, so a document that is
    // never typed into would otherwise never be read at all.
    create: (state) => frontMatterDecorations(state.doc),
    update(decorations, transaction) {
      if (!transaction.docChanged) return decorations;
      return frontMatterDecorations(transaction.state.doc);
    },
    provide: (field) => EditorView.decorations.from(field),
  });
}

/**
 * How many lines of a document may be looked at looking for the fence that closes its front matter.
 *
 * The scan below stops here, and this is why: a document that opens with `---` and never closes it
 * is prose, and there is no line at the end of the file to tell that from a block of metadata whose
 * closing fence is just past where a reader would ever look. Reading to the end of the document is
 * O(N) work on every keystroke, and a 200k-line file someone is typing in the middle of the top
 * would pay it on each one.
 *
 * 2000 lines is far past any metadata block a human writes -- the largest front matter this app has
 * seen in a real repository is a few dozen lines -- and it is only ever spent on a document that
 * opens with the fence, so the common case (no front matter) still costs one line.
 *
 * What this bounds is decoration only. The block is not parsed here: the YAML below is read out of
 * what the scan found, so a block past the bound is left as the flat Markdown text it is, and
 * nothing else about the document changes. Parsing, saving and the bytes on disk are untouched by it,
 * and neither is the preview, which reads the same block by its own rule in `front-matter.ts`.
 */
export const FRONT_MATTER_SCAN_LINES = 2000;

/**
 * How many lines the metadata block at the top of `doc` takes, or 0 when it does not open with one.
 *
 * Bounded by `FRONT_MATTER_SCAN_LINES` rather than by the end of the document: a fence that is never
 * written costs the scan to the bound and then nothing, rather than to the end of a file that may be
 * very long. A block that closes inside the bound is found exactly as before.
 */
function frontMatterDocLineCount(doc: Text): number {
  if (!opensFrontMatter(doc.line(1).text)) return 0;
  const last = Math.min(doc.lines, FRONT_MATTER_SCAN_LINES);
  for (let lineNumber = 2; lineNumber <= last; lineNumber += 1) {
    const fence = doc.line(lineNumber).text.trim();
    if (fence === FRONT_MATTER_FENCE || fence === "...") return lineNumber;
  }
  return 0;
}

function frontMatterDecorations(doc: Text): DecorationSet {
  const lineCount = frontMatterDocLineCount(doc);
  if (lineCount === 0) return Decoration.none;
  // Materialize only the YAML block, whose end is the line the scan above found, so the slice is as
  // long as the metadata and never as long as the file.
  const block = doc.sliceString(0, doc.line(lineCount).to);
  const tree = yaml().language.parser.parse(block);
  const builder = new RangeSetBuilder<Decoration>();
  highlightTree(
    tree,
    musterHighlightStyle,
    (from, to, classes) => {
      if (!classes) return;
      builder.add(from, to, Decoration.mark({ class: classes }));
    },
    0,
    block.length,
  );
  return builder.finish();
}

function languageExtension(language: string | null): Extension {
  switch (language) {
    case "c":
    case "cpp":
      return cpp();
    case "csharp":
      return StreamLanguage.define(csharp);
    case "css":
    case "scss":
    case "less":
      return css();
    case "docker":
      return StreamLanguage.define(dockerFile);
    case "diff":
      return StreamLanguage.define(diff);
    case "go":
      return StreamLanguage.define(go);
    case "graphql":
      return StreamLanguage.define(gql);
    case "html":
    case "vue":
      return html();
    case "http":
      return StreamLanguage.define(http);
    // A dotenv is a properties file, and the legacy modes have no tokenizer of its own for either.
    case "ini":
    case "dotenv":
      return StreamLanguage.define(properties);
    case "gitignore":
      return gitignoreLanguage;
    case "java":
      return java();
    case "javascript":
      return javascript();
    case "typescript":
    case "tsx":
      return javascript({ typescript: true, jsx: language === "tsx" });
    case "json":
    case "jsonc":
    case "json5":
      return json();
    case "kotlin":
      return StreamLanguage.define(kotlin);
    case "lua":
      return StreamLanguage.define(lua);
    case "markdown":
      return [markdown(), frontMatterYaml()];
    case "objective-c":
      return StreamLanguage.define(objectiveC);
    case "powershell":
      return StreamLanguage.define(powerShell);
    case "nginx":
      return StreamLanguage.define(nginx);
    case "perl":
      return StreamLanguage.define(perl);
    case "python":
      return python();
    case "ruby":
      return StreamLanguage.define(ruby);
    case "rust":
      return rust();
    case "shellscript":
      return StreamLanguage.define(shell);
    case "sql":
      return sql();
    case "swift":
      return StreamLanguage.define(swift);
    case "toml":
      return StreamLanguage.define(toml);
    case "xml":
      return xml();
    case "yaml":
      return yaml();
    case "plain-text":
    case "plaintext":
    case null:
      return [];
    default:
      // Unsupported Shiki grammars still get a real editor; the known names above retain the
      // extension/override selection while this fallback avoids pretending plain text is invalid.
      return [];
  }
}
