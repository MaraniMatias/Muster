/**
 * Writes the file-type icon table `app` draws with, from Legacy's own generated module.
 *
 * Legacy already holds the drawings: `legacy/src/lib/catppuccin-icons.ts` is the output of
 * `legacy/scripts/generate-catppuccin-icons.mjs`, which reads Catppuccin's theme and one flavour of
 * its icons. That generator is frozen with the rest of `legacy/`, and the Catppuccin checkout it
 * wants is not vendored here, so this reads the frozen module instead of the checkout. One source
 * of truth, no second copy of Catppuccin's rules to keep in step.
 *
 * ## The colour cannot be copied, only the shape
 *
 * Legacy writes `--muster-icon-*` into every stroke and a browser resolves those against the
 * palette in effect. The toolkit rasterizes an SVG to a single-colour alpha mask —
 * `SvgRenderer::render_alpha_mask` keeps `pixel.alpha()` and `Svg` paints it with one
 * `text_color` — so a stroke's own paint is discarded either way. That is what makes this a cut
 * rather than a copy: an icon becomes one layer per ink it paints with, each layer a complete SVG,
 * and the window stacks them in one box and tints each with the role that ink names. Which value a
 * layer is painted with is therefore irrelevant, only its alpha is read, so it is written as
 * `#000` and the real values stay in `app/src/theme.rs`, the one place this app names a colour.
 *
 * A `<g>` that carries no ink of its own is transparent: its children hold the inks, and the group
 * survives into every layer that keeps one of them. Everything svgo emits outside the palette is
 * gone by the time this runs — see below — so that is the only shape the walk has to know about.
 *
 * ## Why most of the rules are dropped
 *
 * A name is asked two things, and neither can hold a dot: what is before its *first* dot, and what
 * is after its *last* one. A rule key with a dot in it is therefore a rule nothing asks about —
 * `.alexrc`, `Cargo.lock`, `README.md`, `wally.lock` among them. Legacy keeps those rules because
 * it copies the theme wholesale and the dead ones cost it nothing; here they would be a third of
 * the table, so they are dropped along with the icons only they named. Dropping a key nothing can
 * reach cannot change what any name resolves to, and `app/src/file_icons.rs` pins the
 * invariant this rests on.
 *
 * Usage: node scripts/generate-native-file-icons.mjs
 */
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const ROOT = join(import.meta.dirname, "..");
const SOURCE = join(ROOT, "legacy", "src", "lib", "catppuccin-icons.ts");
const OUTPUT = join(ROOT, "app", "src", "file_icons", "data.rs");

/**
 * The inks Catppuccin's Mocha palette names, in the order `palette` names them.
 *
 * These are role names, not values: the value each one carries is a theme decision and lives in
 * `app/src/theme.rs`. The list is here so that an ink the drawings reach and the toolkit's
 * `Ink` cannot answer for is a failure here rather than a blank stroke in the window.
 */
const INKS = [
  "text",
  "overlay1",
  "red",
  "maroon",
  "flamingo",
  "rosewater",
  "pink",
  "mauve",
  "lavender",
  "blue",
  "sky",
  "sapphire",
  "teal",
  "green",
  "yellow",
  "peach",
];

/** Read one `export const NAME = {…}` table out of the module. */
function table(source, name) {
  const at = source.indexOf(`export const ${name}`);
  if (at < 0) throw new Error(`the module exports no ${name}`);
  const open = source.indexOf("{", source.indexOf("=", at));
  let depth = 0;
  let quoted = false;
  for (let i = open; i < source.length; i++) {
    const character = source[i];
    if (quoted) {
      if (character === "\\") i++;
      else if (character === '"') quoted = false;
      continue;
    }
    if (character === '"') quoted = true;
    else if (character === "{") depth++;
    else if (character === "}" && --depth === 0) return JSON.parse(source.slice(open, i + 1));
  }
  throw new Error(`${name} is not closed`);
}

/**
 * The markup as a tree.
 *
 * There is no text node to keep: svgo writes these icons with no whitespace between elements and
 * no content, so a tag is either open, self-closing or a close, and a close pops one level.
 */
function parse(markup) {
  const root = { tag: "#root", attrs: "", children: [] };
  const open = [root];
  for (let at = 0; at < markup.length; ) {
    if (markup[at] !== "<") {
      at++;
      continue;
    }
    const end = markup.indexOf(">", at);
    const tag = markup.slice(at + 1, end);
    at = end + 1;
    if (tag.startsWith("/")) {
      open.pop();
      continue;
    }
    const selfClosing = tag.endsWith("/");
    const body = selfClosing ? tag.slice(0, -1) : tag;
    const space = body.search(/\s/);
    const node = {
      tag: space < 0 ? body : body.slice(0, space),
      attrs: space < 0 ? "" : body.slice(space + 1),
      children: [],
    };
    open[open.length - 1].children.push(node);
    if (!selfClosing) open.push(node);
  }
  return root;
}

const serialise = (node) =>
  node.children.length
    ? `<${node.tag} ${node.attrs}>${node.children.map(serialise).join("")}</${node.tag}>`
    : `<${node.tag} ${node.attrs}/>`;

/** The ink an element paints with, or nothing when it paints with its parent's or with none. */
const inkOf = (attrs) => attrs.match(/var\(--muster-icon-([a-z0-9]+)\)/)?.[1] ?? null;

/**
 * The markup one ink paints with, or nothing when this ink paints nothing here.
 *
 * A node that *names* an ink owns its whole subtree, so cutting another ink drops it and
 * everything under it. A node that names none is transparent and is kept whenever one of its
 * descendants survived, which is what lets a drawing split: a `<g>` folds around only the children
 * of the ink being cut, and keeps the `fill="none"` its children rely on. A leaf is kept when the
 * ink it paints with — its own, or the one it inherited — is the one being cut.
 */
function layer(node, ink, inherited = null) {
  if (node.tag === "#root") {
    // The walk's own head: it names no ink and is never part of the drawing.
    return node.children.map((child) => layer(child, ink)).filter(Boolean).join("") || null;
  }
  const own = inkOf(node.attrs);
  if (own !== null && own !== ink) return null;
  const effective = own ?? inherited;
  if (node.children.length === 0) return effective === ink ? serialise(node) : null;
  const children = node.children.map((child) => layer(child, ink, effective)).filter(Boolean);
  return children.length ? `<${node.tag} ${node.attrs}>${children.join("")}</${node.tag}>` : null;
}

/**
 * One icon, cut into its inks: the layers in the order each ink is first met, so a layer painted
 * over another is the one Legacy paints over it.
 */
function layers(id, markup, viewBox) {
  const inks = [];
  (function walk(node) {
    const own = inkOf(node.attrs);
    if (own !== null && !inks.includes(own)) inks.push(own);
    node.children.forEach(walk);
  })(parse(markup));

  const body = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="${viewBox}">`;
  return inks.map((ink) => {
    const inner = layer(parse(markup), ink);
    if (inner === null) throw new Error(`${id} claims the ${ink} ink but paints none of it`);
    // The token Legacy wrote is a browser's custom property and usvg has no custom
    // properties at all, so it has to be a value it can parse. Which value is
    // irrelevant: only the alpha of what comes out is read, and the window tints the
    // layer with the role this ink names. `#000` is the one that names nothing.
    return { ink, markup: `${body}${inner.replaceAll(/var\(--muster-icon-[a-z0-9]+\)/g, "#000")}</svg>` };
  });
}

/** A Rust string literal, for markup that carries `"` and `\` throughout. */
const rust = (text) => `"${text.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`;

/** `overlay1` -> `Overlay1`, the variant `theme::Ink` answers for it with. */
const variant = (ink) => ink[0].toUpperCase() + ink.slice(1);

const module = readFileSync(SOURCE, "utf8");
const markup = table(module, "CATPPUCCIN_ICON_MARKUP");
const overrides = table(module, "CATPPUCCIN_ICON_VIEWBOX_OVERRIDE");
const viewBox = module.match(/CATPPUCCIN_ICON_VIEWBOX = "([^"]+)"/)[1];
const directory = table(module, "CATPPUCCIN_DIRECTORY_ICON").collapsed;

/** The rules a name can actually be asked about: see the header for why a dot rules one out. */
const asked = (entries) =>
  Object.fromEntries(Object.entries(entries).filter(([key]) => !key.includes(".")));

const namedDirectories = table(module, "CATPPUCCIN_NAMED_DIRECTORY");
const fileStems = asked(table(module, "CATPPUCCIN_FILE_STEM"));
const fileSuffixes = asked(table(module, "CATPPUCCIN_FILE_SUFFIX"));

// Every icon a name can reach, and no others. `_folder_open` is exported by Legacy's
// generator and reached by nothing: the file tree folds a row with a chevron.
const reachable = new Set([directory, "_file"]);
for (const icon of [...Object.values(namedDirectories), ...Object.values(fileStems), ...Object.values(fileSuffixes)]) {
  reachable.add(icon);
}
const missing = [...reachable].filter((id) => markup[id] === undefined);
if (missing.length) throw new Error(`the rules name ${missing.length} icons the markup lacks`);

const unknown = new Set();
const icons = [...reachable]
  .sort()
  .map((id) => {
    const cut = layers(id, markup[id], overrides[id] ?? viewBox);
    for (const { ink } of cut) if (!INKS.includes(ink)) unknown.add(ink);
    return { id, layers: cut };
  });
if (unknown.size) {
  throw new Error(`the drawings reach inks the toolkit has no role for: ${[...unknown].join(", ")}`);
}

/**
 * Every shape the drawing has is in exactly one layer.
 *
 * The cut is the only thing here that can lose a path or print one twice, and either would show up
 * as an icon that is subtly the wrong shape rather than as an error. Counting `d` attributes is
 * enough: it is what every one of these five tags carries and nothing else does.
 */
const shapes = (markup) => (markup.match(/\sd="/g) ?? []).length;
for (const { id, layers: cut } of icons) {
  const painted = cut.reduce((total, { markup }) => total + shapes(markup), 0);
  if (painted !== shapes(markup[id])) {
    throw new Error(`${id} draws ${shapes(markup[id])} shapes and its layers hold ${painted}`);
  }
}

const rules = (name, entries) =>
  `pub(crate) const ${name}: &[(&str, &str)] = &[\n${Object.entries(entries)
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([key, id]) => `    (${rust(key)}, ${rust(id)}),`)
    .join("\n")}\n];\n`;

mkdirSync(join(ROOT, "app", "src", "file_icons"), { recursive: true });
writeFileSync(
  OUTPUT,
  `// Generated by scripts/generate-native-file-icons.mjs. Do not edit.
//
// Catppuccin's drawings (16x16) from https://github.com/catppuccin/zed-icons, read out of Legacy's
// own generated module rather than from the icon checkout: the theme's rules and the markup of the
// ${icons.length} icons a name can reach, cut into one layer per ink so the window can tint each.
//
// Catppuccin is MIT-licensed, and that licence requires the notice below to travel with any copy
// or substantial portion of these drawings. Editing this file by hand would only throw the notice
// away, since regenerating it is what reinstalls it.
//
//     MIT License
//
//     Copyright (c) 2021 Catppuccin
//
//     Permission is hereby granted, free of charge, to any person obtaining a copy
//     of this software and associated documentation files (the "Software"), to deal
//     in the Software without restriction, including without limitation the rights
//     to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//     copies of the Software, and to permit persons to whom the Software is
//     furnished to do so, subject to the following conditions:
//
//     The above copyright notice and this permission notice shall be included in all
//     copies or substantial portions of the Software.
//
//     THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//     IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//     FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//     AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//     LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//     OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//     SOFTWARE.

use crate::theme::Ink;

use super::{FileIcon, Layer};

/// A directory with no name of its own, which is what a name the rules do not
/// reach is answered with.
pub(crate) const DIRECTORY: &str = ${rust(directory)};

/// A file whose name and extension reach no rule.
pub(crate) const FILE: &str = "_file";

/// A directory with a name of its own, by that name alone. Sorted, because
/// \`crate::file_icons\` binary searches it.
${rules("NAMED_DIRECTORY", namedDirectories)}
/// Matched against the name before its first dot, so README.md finds \`readme\`. No key
/// carries a dot: what is asked about never does.
${rules("FILE_STEM", fileStems)}
/// Matched against everything after the last dot, which is the same reason no key
/// carries one.
${rules("FILE_SUFFIX", fileSuffixes)}
/// Every icon a name can reach, sorted by name. A drawing that paints more than one
/// ink has one entry per ink, in the order the drawings first ask for them.
pub(crate) const ICONS: &[(&str, FileIcon)] = &[
${icons
  .map(
    ({ id, layers: cut }) =>
      `    (${rust(id)}, FileIcon { layers: &[\n${cut
        .map(({ ink, markup }) => `    Layer { ink: Ink::${variant(ink)}, markup: ${rust(markup)} },`)
        .join("\n")}\n    ] }),`,
  )
  .join("\n")}
];
`,
);

const dropped =
  Object.keys(table(module, "CATPPUCCIN_FILE_STEM")).length +
  Object.keys(table(module, "CATPPUCCIN_FILE_SUFFIX")).length -
  Object.keys(fileStems).length -
  Object.keys(fileSuffixes).length;

console.log(
  `wrote ${OUTPUT}: ${icons.length} icons, ` +
    `${icons.reduce((total, icon) => total + icon.layers.length, 0)} layers, ` +
    `${Object.keys(namedDirectories).length + Object.keys(fileStems).length + Object.keys(fileSuffixes).length} rules ` +
    `(${dropped} dropped as unreachable), ${(readFileSync(OUTPUT, "utf8").length / 1024).toFixed(0)} kB`,
);