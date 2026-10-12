/**
 * Fira Code's programming ligatures, the ones a code panel is actually about. Longer sequences
 * come first so `!==` wins over `!=` and `===` over `==`.
 *
 * Only symbols: the Latin ones (`fi`, `fl`, `ffi`) reshape ordinary words in logs and diffs,
 * which is not what this panel is for. A sequence the font turns out not to ligate is harmless,
 * because the renderer then draws the same characters one after the other.
 */
const LIGATURES = [
  "<<=",
  ">>=",
  "!==",
  "===",
  "<->",
  "///",
  ":::",
  "...",
  "!=",
  "==",
  "=>",
  "->",
  "<-",
  "<=",
  ">=",
  "&&",
  "||",
  "::",
  "..",
  "<<",
  ">>",
  "//",
  "/*",
  "*/",
  "**",
  "##",
  "~~",
  "++",
  "--",
  "|-|",
  "|-",
  "-|",
  "|>",
  "<|",
  "[[",
  "]]",
  "{{",
  "}}",
];

/** Grouped by length, so the scan can try the longest candidate first at every position. */
const BY_LENGTH: string[][] = [];
for (const ligature of LIGATURES) {
  const length = ligature.length;
  BY_LENGTH[length] ??= [];
  BY_LENGTH[length].push(ligature);
}
const LONGEST = BY_LENGTH.length - 1;
const STARTERS = new Set(LIGATURES.map((ligature) => ligature[0]));

/**
 * The ranges xterm.js should draw as a single glyph, as `[start, end)` indexes into `text`.
 *
 * xterm.js hands over one run of cells that share a foreground and a background, and it calls
 * this on every render, so the text is walked once and only the characters that can open a
 * ligature are examined. The ranges stay as small as the ligature they cover: a joined run is
 * drawn through a slower path than single cells.
 */
export function ligatureRanges(text: string): [number, number][] {
  const ranges: [number, number][] = [];
  for (let index = 0; index < text.length - 1; index++) {
    if (!STARTERS.has(text[index])) continue;
    let length = 0;
    for (let candidate = LONGEST; candidate > 1 && length === 0; candidate--) {
      for (const ligature of BY_LENGTH[candidate] ?? []) {
        if (text.startsWith(ligature, index)) {
          length = candidate;
          break;
        }
      }
    }
    if (length === 0) continue;
    ranges.push([index, index + length]);
    index += length - 1;
  }
  return ranges;
}
