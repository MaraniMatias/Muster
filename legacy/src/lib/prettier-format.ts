/**
 * Formatting a document with Prettier, on a thread of its own.
 *
 * Prettier parses and prints synchronously: `format` hands back a promise because the browser wants
 * one, and the work in between is a straight block of CPU on the thread that called it. A
 * 5 000-line TypeScript file takes about five seconds that way, which is a five-second freeze of the
 * whole window. So the formatter runs in a worker and this module is the wire between the two.
 *
 * Nothing is loaded until the first click: the worker script is a chunk of its own, the core is
 * another and each plugin another, and a reader who only ever formats CSS never downloads the
 * TypeScript parser to find that out.
 */

import type { Options, Plugin } from "prettier";
import picomatch from "picomatch";
import PrettierWorker from "./prettier.worker?worker";

/** One plugin bundle, named here rather than spelled out at each call site. */
type PluginName = "babel" | "estree" | "graphql" | "html" | "markdown" | "postcss" | "typescript" | "yaml" | "xml";

/**
 * One `import()` per bundle, and a literal in each one.
 *
 * A specifier built from a string cannot be found by the bundler, which would leave it a bare
 * import at runtime and fail exactly when the reader is waiting on the answer. These are written
 * out so each is resolvable, and each becomes a chunk of its own.
 */
const BUNDLES: Readonly<Record<PluginName, () => Promise<Record<string, unknown>>>> = {
  babel: () => import("prettier/plugins/babel"),
  estree: () => import("prettier/plugins/estree"),
  graphql: () => import("prettier/plugins/graphql"),
  html: () => import("prettier/plugins/html"),
  markdown: () => import("prettier/plugins/markdown"),
  postcss: () => import("prettier/plugins/postcss"),
  typescript: () => import("prettier/plugins/typescript"),
  yaml: () => import("prettier/plugins/yaml"),
  xml: () => import("@prettier/plugin-xml"),
};

interface FormatTarget {
  parser: string;
  /**
   * Every bundle the parser needs, not only the one it is defined in: a parser produces a tree of
   * some shape and a printer of that shape puts it back together, and both are registered here.
   */
  plugins: readonly PluginName[];
}

/**
 * What Prettier can be asked to parse, keyed by the grammar names this app already uses.
 *
 * Prettier covers a fraction of them, so a grammar missing from this table is one the button has
 * to refuse rather than try. The key is the grammar the reader chose in the toolbar, not the
 * file's extension: the same file reads as two different things and Prettier is asked about the
 * one on screen.
 */
const TARGETS: ReadonlyMap<string, FormatTarget> = new Map<string, FormatTarget>([
  ["javascript", { parser: "babel", plugins: ["babel", "estree"] }],
  ["typescript", { parser: "typescript", plugins: ["typescript", "estree"] }],
  ["tsx", { parser: "typescript", plugins: ["typescript", "estree"] }],
  ["json", { parser: "json", plugins: ["babel", "estree"] }],
  ["jsonc", { parser: "jsonc", plugins: ["babel", "estree"] }],
  ["json5", { parser: "json5", plugins: ["babel", "estree"] }],
  ["css", { parser: "css", plugins: ["postcss"] }],
  ["scss", { parser: "scss", plugins: ["postcss"] }],
  ["less", { parser: "less", plugins: ["postcss"] }],
  ["html", { parser: "html", plugins: ["html"] }],
  ["xml", { parser: "xml", plugins: ["xml"] }],
  // A Vue template is HTML, and everything it holds inside is not: the `<script>` and the
  // interpolations are JavaScript, so the bundles that read those are registered too.
  ["vue", { parser: "vue", plugins: ["html", "babel", "estree"] }],
  // A fenced block is a document of its own, and YAML front matter is the one Prettier lays out
  // itself rather than handing to another parser, so the YAML bundle comes along as well.
  ["markdown", { parser: "markdown", plugins: ["markdown", "babel", "estree", "yaml"] }],
  ["yaml", { parser: "yaml", plugins: ["yaml"] }],
  ["graphql", { parser: "graphql", plugins: ["graphql"] }],
]);

function targetFor(language: string | null): FormatTarget {
  const target = language === null ? undefined : TARGETS.get(language);
  if (!target) throw new Error(`Prettier does not format ${language ?? "this file"}.`);
  return target;
}

/**
 * The Prettier parser for a grammar, or `null` when Prettier has none for it.
 *
 * Synchronous and table-only on purpose: the toolbar asks this to decide whether the button works
 * and what it says, and it cannot wait on a download to find out that Rust has no parser.
 */
export function formattingParser(language: string | null): string | null {
  return language === null ? null : (TARGETS.get(language)?.parser ?? null);
}

export interface FormatRequest {
  content: string;
  /** The grammar the editor is reading as, which is what chooses the parser. */
  language: string | null;
  /** The path Prettier reports a position against, and nothing more. */
  path: string;
  /** The options read from the checkout's own config, over Prettier's defaults. */
  options?: Record<string, unknown>;
  /** Path relative to the config directory, used only for override matching. */
  configPath?: string;
}

/** Same grouping and matcher options as Prettier resolveConfig, using its underlying glob engine. */
export function resolveFormatOptions(request: FormatRequest): Record<string, unknown> {
  const { overrides, ...options } = request.options ?? {};
  if (overrides !== undefined) {
    if (!Array.isArray(overrides)) throw new Error("Prettier overrides must be an array.");
    for (const entry of overrides) {
      const patterns: string[] = Array.isArray(entry.files) ? entry.files : [entry.files];
      const file = request.configPath ?? request.path;
      const matches = [true, false].some((basename) => {
        const group = patterns.filter((pattern) => pattern.includes("/") !== basename);
        return (
          group.length > 0 &&
          picomatch(group, {
            basename,
            dot: true,
            windows: false,
            ignore: entry.excludeFiles,
          })(file)
        );
      });
      if (matches) Object.assign(options, entry.options);
    }
  }
  delete options.parser;
  delete options.plugins;
  delete options.overrides;
  return options;
}

/**
 * The document as Prettier would write it. This is the half that runs on the worker thread.
 *
 * A parse error is Prettier's own and is left whole here, because the worker is what reduces it to
 * something a message can carry and it has all of it while it is doing that.
 *
 * The options come first and the three keys this owns come after them, so a config can say anything
 * about how the file looks and nothing about what it is parsed as.
 */
export async function runFormat(request: FormatRequest): Promise<string> {
  const target = targetFor(request.language);
  const [prettier, ...bundles] = await Promise.all([
    import("prettier/standalone"),
    ...target.plugins.map((name) => BUNDLES[name]()),
  ]);
  return prettier.format(request.content, {
    ...(resolveFormatOptions(request) as Options),
    // Strict XML preserves text, CDATA and inter-element whitespace regardless of project options.
    ...(target.parser === "xml" ? { xmlWhitespaceSensitivity: "strict" } : {}),
    parser: target.parser,
    filepath: request.path,
    // Each bundle is a namespace whose exports are the plugin's own parts, and a plugin is those
    // parts together. The assignment is what turns four namespaces back into what `plugins` takes.
    plugins: bundles.map((bundle) => (bundle.default ?? Object.assign({}, bundle)) as Plugin),
  });
}

export interface FormatCommand {
  id: number;
  request: FormatRequest;
}

export type FormatAnswer = { id: number; text: string } | { id: number; message: string };

/**
 * What a failure becomes on its way back to the pane.
 *
 * A structured clone carries the message of an `Error` and nothing else, and the code frame Prettier
 * draws under it is the source the reader is already looking at, one line per line of it. The first
 * line is the part worth carrying, and it already names the line and the column.
 */
export function failureAnswer(id: number, error: unknown): FormatAnswer {
  const message = error instanceof Error ? error.message : String(error);
  const frame = message.indexOf("\n");
  return { id, message: frame === -1 ? message : message.slice(0, frame) };
}

/**
 * The one worker, kept between clicks.
 *
 * Keeping it is what makes the second click fast: the core and the plugins are parsed and warmed
 * once, and a worker per format would pay that again every time. The cost is one idle thread
 * holding those modules, which is what anything that formats on demand is already paying.
 */
let worker: Worker | null = null;
let nextRequestId = 0;
const waiting = new Map<number, { resolve: (text: string) => void; reject: (error: Error) => void }>();

function prettierWorker(): Worker {
  if (worker !== null) return worker;
  const created = new PrettierWorker();
  const answer = (event: MessageEvent<FormatAnswer>) => {
    const pending = waiting.get(event.data.id);
    if (!pending) return;
    waiting.delete(event.data.id);
    if ("text" in event.data) pending.resolve(event.data.text);
    else pending.reject(new Error(event.data.message));
  };
  created.addEventListener("message", answer);
  // A worker that cannot start never answers, and every request would sit there forever. Its
  // failure goes back the way a format failure does, because that is what the reader is shown.
  created.addEventListener("error", (event) => {
    if (worker !== created) return;
    const failure = new Error(`Prettier could not start: ${event.message}`);
    for (const pending of waiting.values()) pending.reject(failure);
    waiting.clear();
    created.terminate();
    worker = null;
  });
  worker = created;
  return created;
}

/**
 * The document as Prettier would write it, without this thread waiting for it.
 *
 * The click and the button agree on what can be formatted because both ask the same table first,
 * and a grammar with no parser is refused here rather than across a thread boundary.
 */
export async function formatSource(request: FormatRequest): Promise<string> {
  targetFor(request.language);
  const id = ++nextRequestId;
  return new Promise<string>((resolve, reject) => {
    waiting.set(id, { resolve, reject });
    try {
      prettierWorker().postMessage({ id, request });
    } catch (error) {
      waiting.delete(id);
      reject(error instanceof Error ? error : new Error(String(error)));
    }
  });
}
