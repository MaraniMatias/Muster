/**
 * What a document can be highlighted as, with no Shiki in it.
 *
 * `source-highlighter.ts` imports `shiki/core` at module scope, so the document toolbar cannot
 * import anything from it without dragging the highlighter into the first chunk: the toolbar needs
 * the detected language and the list of choices synchronously, long before anyone asks a
 * highlighter for anything. It owns the file-name and extension allowlists and the labels, while
 * `source-highlighter.ts` keeps the grammar loaders and resolves every name through those tables.
 */

export interface SourceLanguageOption {
  name: string;
  label: string;
}

/**
 * Muster' own "no grammar". Shiki v4 ships no plaintext language, so this name is in no allowlist
 * and resolves to no highlighting at all, which is how a plain file has always rendered.
 */
export const PLAIN_TEXT = "plaintext";

/** The grammars the toolbar offers, by the name Shiki knows each one by. */
export const SELECTABLE_LANGUAGES: readonly SourceLanguageOption[] = [
  { name: "c", label: "C" },
  { name: "cpp", label: "C++" },
  { name: "csharp", label: "C#" },
  { name: "css", label: "CSS" },
  { name: "diff", label: "Diff" },
  { name: "docker", label: "Dockerfile" },
  { name: "go", label: "Go" },
  { name: "graphql", label: "GraphQL" },
  { name: "html", label: "HTML" },
  { name: "ini", label: "INI" },
  { name: "java", label: "Java" },
  { name: "javascript", label: "JavaScript" },
  { name: "json", label: "JSON" },
  { name: "json5", label: "JSON5" },
  { name: "jsonc", label: "JSON with Comments" },
  { name: "kotlin", label: "Kotlin" },
  { name: "less", label: "Less" },
  { name: "log", label: "Log" },
  { name: "lua", label: "Lua" },
  { name: "make", label: "Makefile" },
  { name: "markdown", label: "Markdown" },
  { name: "nginx", label: "Nginx" },
  { name: "php", label: "PHP" },
  { name: "powershell", label: "PowerShell" },
  { name: "python", label: "Python" },
  { name: "ruby", label: "Ruby" },
  { name: "rust", label: "Rust" },
  { name: "scss", label: "SCSS" },
  { name: "shellscript", label: "Shell" },
  { name: "sql", label: "SQL" },
  { name: "swift", label: "Swift" },
  { name: "toml", label: "TOML" },
  { name: "tsx", label: "TSX" },
  { name: "typescript", label: "TypeScript" },
  { name: "vue", label: "Vue" },
  { name: "xml", label: "XML" },
  { name: "yaml", label: "YAML" },
];

/**
 * The file extensions the highlighter answers for, mapped to a grammar name. It moved here from
 * `source-highlighter.ts` so the same table decides both what the toolbar detects and what the
 * highlighter loads. An extension that names no grammar is not a mistake: it means the file
 * renders plain until the reader forces one from the toolbar.
 */
export const LANGUAGE_BY_EXTENSION: ReadonlyMap<string, string> = new Map<string, string>([
  ["c", "c"],
  ["cs", "csharp"],
  ["css", "css"],
  ["go", "go"],
  ["gql", "graphql"],
  ["graphql", "graphql"],
  ["h", "c"],
  ["htm", "html"],
  ["html", "html"],
  ["java", "java"],
  ["js", "javascript"],
  ["jsx", "javascript"],
  ["json", "json"],
  ["jsonc", "jsonc"],
  ["kt", "kotlin"],
  ["kts", "kotlin"],
  ["adb", "ada"],
  ["adoc", "asciidoc"],
  ["asciidoc", "asciidoc"],
  ["bash", "shellscript"],
  ["bib", "bibtex"],
  ["cc", "cpp"],
  ["cjs", "javascript"],
  ["clj", "clojure"],
  ["cljs", "clojure"],
  ["cmake", "cmake"],
  ["cpp", "cpp"],
  ["csv", "csv"],
  ["cxx", "cpp"],
  ["dart", "dart"],
  ["diff", "diff"],
  ["dockerfile", "docker"],
  ["edn", "clojure"],
  ["erl", "erlang"],
  ["ex", "elixir"],
  ["exs", "elixir"],
  ["fish", "fish"],
  ["fs", "fsharp"],
  ["fsx", "fsharp"],
  ["gd", "gdscript"],
  ["glsl", "glsl"],
  ["hh", "cpp"],
  ["hpp", "cpp"],
  ["hxx", "cpp"],
  ["ini", "ini"],
  ["jl", "julia"],
  ["json5", "json5"],
  ["jsonl", "jsonl"],
  ["less", "less"],
  ["lisp", "common-lisp"],
  ["log", "log"],
  ["lua", "lua"],
  ["m", "objective-c"],
  ["make", "make"],
  ["makefile", "make"],
  ["md", "markdown"],
  ["mdown", "markdown"],
  ["markdown", "markdown"],
  ["mjs", "javascript"],
  ["mk", "make"],
  ["ml", "ocaml"],
  ["mmd", "mermaid"],
  ["mts", "typescript"],
  ["nu", "nushell"],
  ["objc", "objective-c"],
  ["patch", "diff"],
  ["php", "php"],
  ["pl", "perl"],
  ["prisma", "prisma"],
  ["proto", "proto"],
  ["ps1", "powershell"],
  ["py", "python"],
  ["pyw", "python"],
  ["r", "r"],
  ["rb", "ruby"],
  ["rs", "rust"],
  ["sass", "sass"],
  ["scala", "scala"],
  ["scss", "scss"],
  ["sh", "shellscript"],
  ["sol", "solidity"],
  ["sql", "sql"],
  ["styl", "stylus"],
  ["sv", "system-verilog"],
  ["svg", "xml"],
  ["svelte", "svelte"],
  ["swift", "swift"],
  ["tex", "latex"],
  ["tf", "terraform"],
  ["toml", "toml"],
  ["ts", "typescript"],
  ["tsx", "tsx"],
  ["v", "verilog"],
  ["vb", "vb"],
  ["vhd", "vhdl"],
  ["vhdl", "vhdl"],
  ["vim", "viml"],
  ["vue", "vue"],
  ["wat", "wasm"],
  ["wgsl", "wgsl"],
  ["xml", "xml"],
  ["yaml", "yaml"],
  ["yml", "yaml"],
  ["zig", "zig"],
  ["zsh", "shellscript"],
]);

/**
 * The file names that ask for a grammar on their own, because a leading dot is the whole name and
 * the extension table cannot see them: `.gitignore` ends in `gitignore`, not in a suffix anyone
 * registered. Matched against the basename in any directory, the way VS Code and GitHub do, so
 * `docs/.gitignore` reads like the root one.
 *
 * `gitignore` is its own name rather than `ini` on purpose. Shiki has no gitignore grammar, so it
 * borrows `ini`, which renders the patterns correctly, but the editor has no `ini` tokenizer that
 * survives a file of bare patterns, and naming the family separately is what lets it use one.
 */
export const LANGUAGE_BY_FILENAME: ReadonlyMap<string, string> = new Map<string, string>([
  // INI: a `[section]` header over `key = value` lines, or the attribute names gitattributes holds.
  [".curlrc", "ini"],
  // `[TERM]` sections over bare `LINK file1 file2` lines.
  [".dircolors", "ini"],
  [".editorconfig", "ini"],
  [".gitattributes", "ini"],
  [".gitconfig", "ini"],
  [".gitmodules", "ini"],
  // Readline settings, which is `set editing-mode emacs` under no section at all.
  [".inputrc", "ini"],
  [".npmrc", "ini"],
  // The `[core] editor = ...` Git reads when nothing else has said which editor.
  [".selected_editor", "ini"],
  // `set -g option value` over `%if` blocks, which is what an INI grammar reads best.
  [".tmux.conf", "ini"],
  [".yarnrc", "ini"],
  // One pattern per line under `#` comments.
  [".cvsignore", "gitignore"],
  [".dockerignore", "gitignore"],
  [".eslintignore", "gitignore"],
  [".gitignore", "gitignore"],
  [".hgignore", "gitignore"],
  [".npmignore", "gitignore"],
  [".prettierignore", "gitignore"],
  // Shell startup files: bash, zsh, ksh and tcsh each spell the same file their own way, and the
  // body is what a `.sh` file already renders with.
  [".bash_login", "shellscript"],
  [".bash_logout", "shellscript"],
  [".bash_profile", "shellscript"],
  [".bashrc", "shellscript"],
  [".cshrc", "shellscript"],
  [".kshrc", "shellscript"],
  [".login", "shellscript"],
  [".profile", "shellscript"],
  [".shrc", "shellscript"],
  [".tcshrc", "shellscript"],
  [".zlogin", "shellscript"],
  [".zlogout", "shellscript"],
  [".zprofile", "shellscript"],
  [".zshenv", "shellscript"],
  [".zshrc", "shellscript"],
  // SQL under a line or two of `\command` meta-commands.
  [".psqlrc", "sql"],
  // Dotfiles whose body is bare JSON.
  [".babelrc", "json"],
  [".eslintrc", "json"],
  [".jshintrc", "json"],
  [".prettierrc", "json"],
  [".swcrc", "json"],
  [".watchmanconfig", "json"],
]);

const labelByName = new Map(SELECTABLE_LANGUAGES.map((language) => [language.name, language.label]));
const extensionsByName = new Map<string, string[]>();
for (const [extension, name] of LANGUAGE_BY_EXTENSION) {
  const owned = extensionsByName.get(name);
  if (owned) owned.push(extension);
  else extensionsByName.set(name, [extension]);
}

/**
 * The grammar a path asks for, or undefined when neither its name nor its extension asks for one.
 * The name is checked first because for a dotfile it is the only part that carries the format.
 */
export function detectedLanguageName(path: string): string | undefined {
  const fileName = path.split(/[/\\]/).pop()?.toLowerCase() ?? "";
  // `.env.local` and `.env.production.local` are the same format as `.env`, and a table of every
  // suffix anyone has ever typed would stop at the first one it forgot. The dot is what separates
  // them from `.envrc`, which is a shell script.
  if (fileName === ".env" || fileName.startsWith(".env.")) return "dotenv";
  const named = LANGUAGE_BY_FILENAME.get(fileName);
  if (named) return named;
  // The same holds for a dotfile wearing a variant of its own name: `.zshrc.local` and
  // `.bashrc.macos` are the files above under somebody else's name, and listing them would stop at
  // the first hostname anyone had typed. The second dot is what separates them from a dotfile that
  // is only named after one, such as `.eslintrc.json`.
  const variantAt = fileName.indexOf(".", 1);
  const variant = variantAt > 0 ? LANGUAGE_BY_FILENAME.get(fileName.slice(0, variantAt)) : undefined;
  return variant ?? extensionLanguageName(fileName);
}

function extensionLanguageName(fileName: string): string | undefined {
  const extension = fileName.split(".").pop()?.toLowerCase();
  return extension === undefined ? undefined : LANGUAGE_BY_EXTENSION.get(extension);
}

/** What the toolbar writes down: the written name when it has one, title case when it does not. */
export function languageLabel(name: string): string {
  return labelByName.get(name) ?? titleCase(name);
}

/** Every extension that already asks for `name`, so the search finds it by the suffix it wears. */
export function languageExtensions(name: string): readonly string[] {
  return extensionsByName.get(name) ?? [];
}

function titleCase(name: string): string {
  return name
    .split(/[-_]/)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(" ");
}
