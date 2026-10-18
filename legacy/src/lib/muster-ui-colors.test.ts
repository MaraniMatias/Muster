import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { highlighter } from "@git-diff-view/vue";
import type { DiffAST } from "@git-diff-view/vue";
import { describe, expect, it } from "vitest";

const stylesheet = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "muster.css"), "utf8");
const sidebarStyles = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "..", "components", "Sidebar.vue"),
  "utf8",
);
const fileDiffTemplate = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "..", "components", "FileDiff.vue"),
  "utf8",
);
const documentPaneTemplate = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "..", "components", "DocumentPane.vue"),
  "utf8",
);
const chromeStart = stylesheet.indexOf("/* UI chrome");

/**
 * One sample per language the library falls back to, chosen to reach the token kinds that are easy
 * to leave behind: a template literal, a class declaration, a list marker, bold text.
 */
const diffSamples: Record<string, string> = {
  typescript: [
    "const a = computed(() => x.value);",
    "class Foo { private readonly bar = 1; }",
    "const s = `tpl ${a} end`;",
    "export default defineComponent({ props: { onChange: (v: string) => void } });",
    "// line",
    "/* block */",
  ].join("\n"),
  vue: '<template>\n  <div class="a" :prop="v" @click="go">{{ msg }}</div>\n</template>\n',
  xml: '<!-- c -->\n<a href="x">t</a>',
  css: ".a { color: red; margin: 0 auto !important; }",
  python: '@decorator\nclass A(B):\n    def f(self, x: int = 1) -> str:\n        return f"{x!r}"  # c',
  rust: "pub fn main() { let x = 1u32; }",
  go: "package main\nfunc main() {}",
  ruby: 'def f(a)\n  puts "#{a}"\nend',
  scala: "object A { def f(x: Int): Int = x + 1 }",
  elixir: "defmodule A do\n  def f(x), do: x + 1\nend",
  java: "public class A { void m() { var x = 1; } }",
  yaml: "key: value\nlist:\n  - a\n# c",
  markdown: "# H1\n\n**bold** _em_ `code`\n\n- item\n\n~~gone~~",
  bash: 'set -e\nfor f in *.txt; do echo "$f"; done',
  json: '{ "a": 1, "b": [true, null] }',
  php: "<?php\nfunction f($a) { return $a ?? 1; }",
};

/** Every `hljs-*` class the diff library's grammars put on a token of these samples. */
function diffSyntaxClasses(): Set<string> {
  const classes = new Set<string>();
  const walk = (node: unknown): void => {
    const element = node as {
      properties?: { className?: string[] | string };
      children?: unknown[];
    };
    const name = element.properties?.className;
    for (const value of Array.isArray(name) ? name : name ? [name] : []) {
      // A sub-language wrapper is named after the grammar it holds, not prefixed, so it is not a
      // token class and there is nothing in the stylesheet to match it against.
      if (value.startsWith("hljs-")) classes.add(value);
    }
    for (const child of element.children ?? []) walk(child);
  };
  for (const [language, source] of Object.entries(diffSamples)) {
    walk(highlighter.getAST(source, undefined, language, "dark") as DiffAST);
  }
  return classes;
}

function block(selector: string, css = stylesheet): string {
  const selectorStart = css.indexOf(selector, css === stylesheet ? chromeStart : 0);
  const open = css.indexOf("{", selectorStart);
  return css.slice(open + 1, css.indexOf("}", open));
}

function declarations(css: string): Record<string, string> {
  return Object.fromEntries(
    [...css.matchAll(/(--[\w-]+):\s*([^;]+);/g)].map(([, name, value]) => [name!, value!.trim()]),
  );
}

const darkTokens = declarations(block(":root {"));
const themes = {
  dark: darkTokens,
  light: { ...darkTokens, ...declarations(block(':root[data-theme="light"]')) },
};
const referenceSwatches = {
  dark: {
    "--muster-bg-0": "#16181d",
    "--muster-bg-1": "#1c1f26",
    "--muster-bg-2": "#242830",
    "--muster-border": "#2c313b",
    "--muster-border-strong": "#3a404c",
    "--muster-text": "#d7dae0",
    "--muster-text-muted": "#8b909c",
    "--muster-text-disabled": "#565b67",
    "--muster-el": "#242830",
    "--muster-el-hover": "#2d323c",
    "--muster-el-active": "#363c48",
    "--muster-el-selected": "#2c3b57",
    "--muster-accent": "#5b97ff",
    "--muster-accent-fg": "#0d1626",
    "--muster-accent-tint": "#1f2f4d",
    "--muster-danger": "#f07178",
    "--muster-success": "#5fc98a",
    "--muster-warning": "#e3b341",
    "--muster-focus": "#5b97ff",
  },
  light: {
    "--muster-bg-0": "#f6f6f7",
    "--muster-bg-1": "#ffffff",
    "--muster-bg-2": "#ffffff",
    "--muster-border": "#e1e2e5",
    "--muster-border-strong": "#c9cbd0",
    "--muster-text": "#1c1e23",
    "--muster-text-muted": "#6b7080",
    "--muster-text-disabled": "#a9adb8",
    "--muster-el": "#eef0f2",
    "--muster-el-hover": "#e4e6ea",
    "--muster-el-active": "#d9dce2",
    "--muster-el-selected": "#d6e4ff",
    "--muster-accent": "#2f6fed",
    "--muster-accent-fg": "#ffffff",
    "--muster-accent-tint": "#e3ecff",
    "--muster-danger": "#d63b3b",
    "--muster-success": "#1f9d55",
    "--muster-warning": "#c98a0a",
    "--muster-focus": "#2f6fed",
  },
};
const referenceContentSwatches = {
  dark: {
    "--muster-content-bg-0": "#222436",
    "--muster-content-bg-1": "#2f343e",
    "--muster-content-bg-2": "#363c46",
    "--muster-content-border": "#464b57",
    "--muster-content-text": "#dce0e5",
    "--muster-content-text-muted": "#a9afbc",
    "--muster-content-text-faint": "#767d8d",
    "--muster-content-accent": "#74ade8",
    "--muster-content-added": "#a1c181",
    "--muster-content-removed": "#d97f84",
    "--muster-selection": "#74ade83d",
  },
  light: {
    "--muster-content-bg-0": "#fafafa",
    "--muster-content-bg-1": "#ebebec",
    "--muster-content-bg-2": "#dfdfe0",
    "--muster-content-border": "#c9c9ca",
    "--muster-content-text": "#242529",
    "--muster-content-text-muted": "#58585a",
    "--muster-content-text-faint": "#8a8b8f",
    "--muster-content-accent": "#455fd0",
    "--muster-content-added": "#3f7a35",
    "--muster-content-removed": "#b8453a",
    "--muster-selection": "#455fd03d",
  },
};

function token(theme: Record<string, string>, name: string): string {
  const value = theme[name];
  if (!value) throw new Error(`Missing ${name}`);
  const ref = /^var\((--[\w-]+)\)$/.exec(value);
  return ref ? token(theme, ref[1]!) : value;
}

function channels(hex: string): number[] {
  const value = hex.replace(/^#/, "");
  return [0, 2, 4].map((offset) => Number.parseInt(value.slice(offset, offset + 2), 16));
}

function hex(rgb: number[]): string {
  return `#${rgb.map((value) => Math.round(value).toString(16).padStart(2, "0")).join("")}`;
}

function mix(first: string, second: string, firstShare: number): string {
  const a = channels(first);
  const b = channels(second);
  return hex(a.map((value, index) => value * firstShare + b[index]! * (1 - firstShare)));
}

function brightness(color: string, factor: number): string {
  return hex(channels(color).map((value) => Math.min(255, value * factor)));
}

function composite(foreground: string, background: string, opacity: number): string {
  const fg = channels(foreground);
  const bg = channels(background);
  return hex(fg.map((value, index) => value * opacity + bg[index]! * (1 - opacity)));
}

function luminance(color: string): number {
  const [r, g, b] = channels(color).map((value) => {
    const channel = value / 255;
    return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r! + 0.7152 * g! + 0.0722 * b!;
}

function contrast(foreground: string, background: string): number {
  const a = luminance(foreground);
  const b = luminance(background);
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

function expectReadable(foreground: string, background: string) {
  expect(contrast(foreground, background), `${foreground} on ${background}`).toBeGreaterThanOrEqual(4.5);
}

describe("UI foreground tokens", () => {
  it("preserves the exact Raymond palette swatches", () => {
    for (const [mode, expected] of Object.entries(referenceSwatches)) {
      for (const [name, color] of Object.entries(expected)) {
        expect(token(themes[mode as keyof typeof themes], name), `${mode} ${name}`).toBe(color);
      }
    }
  });

  it("keeps the editor and diff content palettes unchanged", () => {
    for (const [mode, expected] of Object.entries(referenceContentSwatches)) {
      for (const [name, color] of Object.entries(expected)) {
        expect(token(themes[mode as keyof typeof themes], name), `${mode} ${name}`).toBe(color);
      }
    }
  });

  it("draws the editor's caret as a block in the two colors the terminal draws its own with", () => {
    // The caret is the one part of the editor the stylesheet owns rather than CodeMirror: it draws
    // the element, Muster decides it is a block, and it is `--muster-cursor` because that is the
    // color xterm.js is handed for `cursor`. The width is load-bearing rather than cosmetic:
    // CodeMirror leaves a caret's width unset, and a background on a zero-width box is not drawn.
    const caret = block(".cm-focused .cm-scroller .cm-cursorLayer .cm-cursor) {", documentPaneTemplate);
    expect(caret).toContain("border-left: none;");
    expect(caret).toContain("width: 1ch;");
    expect(caret).toContain("background: var(--muster-cursor);");
    // The preference is a declaration and not a second animation: CodeMirror blinks the layer
    // itself, so the property is what the toggle in the settings dialog has to reach the caret by.
    expect(block(".cm-focused .cm-scroller .cm-cursorLayer) {", documentPaneTemplate)).toContain(
      "animation-play-state: var(--muster-editor-cursor-blink, running);",
    );
    // A block nobody can see is the failure, so the cell it fills has to stand off the surface in
    // both palettes rather than only the dark one — which is why the light answer to the dark
    // palette's lavender is ink rather than the same lavender at a lower alpha.
    for (const theme of Object.values(themes)) {
      expectReadable(token(theme, "--muster-cursor"), token(theme, "--muster-content-bg-0"));
    }
  });

  it("paints every token the fallback grammars emit, so none is left in the plain text color", () => {
    // A diff is highlighted by Muster' own Shiki grammars, which arrive carrying Muster' own tokens
    // and need no stylesheet at all. The library falls back to its own grammars for any language
    // Shiki is not loaded for, and those arrive as `.hljs-*` classes that FileDiff.vue has to
    // recolor. A class those emit and no rule names is a token left in the library's GitHub palette,
    // which is how five of them were being painted before.
    const rules = new Set([...fileDiffTemplate.matchAll(/\.hljs-([\w-]+)/g)].map(([, name]) => `hljs-${name}`));
    const unmapped = [...diffSyntaxClasses()].filter((name) => !rules.has(name)).sort();
    expect(unmapped, `no rule in FileDiff.vue paints ${unmapped.join(" ")}`).toEqual([]);
    // The rule has to be inside the diff's own selector, not merely mentioned somewhere in the file.
    expect(fileDiffTemplate).toContain(".diff-line-syntax-raw");
  });

  it("keeps the sidebar's two row surfaces readable", () => {
    // The panel has two surfaces again and both carry text: the hover surface, and — now that
    // selection has its own — the selected tint. The primary ink has to be readable on each in both
    // palettes, and the close cross paints whichever of the two its row is on, so it never reads as
    // a hole in the row.
    expect(block(".workdir-row:hover {", sidebarStyles)).toContain("background: var(--muster-el-hover);");
    expect(block(".workdir-row.selected {", sidebarStyles)).toContain("background: var(--muster-el-selected);");
    expect(block(".workdir-row.selected {", sidebarStyles)).toContain("color: var(--muster-text);");
    expect(block(".workdir-close {", sidebarStyles)).toContain("background: var(--muster-bg-1);");
    for (const theme of Object.values(themes)) {
      const foreground = token(theme, "--muster-text");
      expectReadable(foreground, token(theme, "--muster-el-hover"));
      expectReadable(foreground, token(theme, "--muster-el-selected"));
      // The selected row's glyph is primary ink rather than the secondary one a context glyph wears,
      // because on the selected tint the secondary ink is under the contrast floor.
      expectReadable(foreground, token(theme, "--muster-el-selected"));
    }
  });

  it("uses a readable content foreground for virtualized diff errors", () => {
    const message = fileDiffTemplate.indexOf('row.error || "Loading diff page…"');
    const element = fileDiffTemplate.slice(
      fileDiffTemplate.lastIndexOf("<span", message),
      fileDiffTemplate.indexOf("</span>", message),
    );
    expect(element).toContain("--muster-content-text-muted");
    for (const theme of Object.values(themes)) {
      expectReadable(token(theme, "--muster-content-text-muted"), token(theme, "--muster-content-bg-0"));
    }
  });

  it("keep small semantic text readable across themes and interactive surface states", () => {
    for (const [mode, theme] of Object.entries(themes)) {
      const get = (name: string) => token(theme, name);
      const surfaces = [
        "--muster-bg-0",
        "--muster-bg-1",
        "--muster-bg-2",
        "--muster-el",
        "--muster-el-hover",
        "--muster-el-active",
        "--muster-el-selected",
      ];

      for (const [role, alphas] of [
        ["danger", [0.12, 0.2]],
        ["success", []],
        ["warning", []],
      ] as const) {
        const foreground = get(`--muster-${role}-fg`);
        for (const surface of surfaces) {
          const background = get(surface);
          expectReadable(foreground, background);
          for (const alpha of alphas) {
            expectReadable(foreground, composite(get(`--muster-${role}`), background, alpha));
          }
        }
      }

      const tint = get("--muster-accent-tint");
      const tintForeground = get("--muster-accent-tint-fg");
      expectReadable(tintForeground, tint);
      expectReadable(brightness(tintForeground, 0.96), brightness(tint, 0.96));
      expectReadable(tintForeground, mix(tint, get("--muster-accent"), 0.82));

      const accent = get("--muster-accent");
      const accentForeground = get("--muster-accent-fg");
      expectReadable(accentForeground, accent);
      if (mode === "light") {
        expectReadable(accentForeground, mix(accent, get("--muster-text"), 0.88));
        expectReadable(accentForeground, mix(accent, get("--muster-text"), 0.84));
      } else {
        for (const factor of [1.08, 0.94]) {
          expectReadable(brightness(accentForeground, factor), brightness(accent, factor));
        }
      }

      const placeholder = get("--muster-placeholder-fg");
      for (const surface of ["--muster-bg-0", "--muster-bg-1", "--muster-bg-2"]) {
        expectReadable(placeholder, get(surface));
      }
    }
  });
});
