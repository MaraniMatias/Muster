import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { CATPPUCCIN_ICON_MARKUP } from "./catppuccin-icons";

/**
 * Read rather than imported: vitest hands back an empty string for a stylesheet it does not run,
 * and an empty table would make every assertion below pass for the wrong reason.
 */
const stylesheet = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "muster.css"), "utf8");

/** Where each palette lives: the dark half is the `:root` block ahead of the light override. */
const light = stylesheet.indexOf(':root[data-theme="light"]');
const PALETTES = {
  dark: stylesheet.slice(0, light),
  light: stylesheet.slice(light, stylesheet.indexOf("}", light)),
};

/** Every distinct token the drawings stroke or fill themselves with. */
const tokens = [
  ...new Set(
    [
      ...Object.values(CATPPUCCIN_ICON_MARKUP)
        .join("")
        .matchAll(/var\((--muster-icon-[a-z0-9-]+)\)/g),
    ].map(([, token]) => token),
  ),
];

describe("the file-type icons", () => {
  it("strokes with tokens, so one set of drawings serves both palettes", () => {
    const markup = Object.values(CATPPUCCIN_ICON_MARKUP).join("");
    // A palette colour left as a value here is a stroke the light theme cannot change: the icon
    // would keep the dark flavour's paint whatever `data-theme` says. The three the set paints with
    // outside the palette are the same in every flavour, so those are the only values allowed to
    // stay, and there are three of them.
    expect(tokens.length).toBeGreaterThan(0);
    const baked = new Set([...markup.matchAll(/(?:stroke|fill)="(#[0-9a-fA-F]{3,8})"/g)].map(([, color]) => color));
    expect([...baked].sort()).toEqual(["#3700ff", "#df8e1d", "#fff"]);
  });

  it("names a colour that each theme answers for", () => {
    // The generated module and the stylesheet are two files that have to agree, and this is the
    // only thing that asks: a token renamed in one is an icon that draws in no colour at all.
    for (const theme of ["dark", "light"] as const) {
      const unanswered = tokens.filter((token) => !PALETTES[theme].includes(`${token}:`));
      expect({ theme, unanswered }).toEqual({ theme, unanswered: [] });
    }
  });
});
