# Native fonts

Fira Code Regular and Bold are the unmodified TrueType faces from the official
[Fira Code 6.2 release](https://github.com/tonsky/FiraCode/releases/tag/6.2),
extracted from `ttf/` in `Fira_Code_v6.2.zip`.
They are embedded for the native sidebar and terminal; no system installation
or runtime download is needed.
The SIL Open Font License is included in `LICENSE-FiraCode.txt`.

`NerdSymbols.ttf` is the icon half of
[FiraCode Nerd Font Mono](https://github.com/ryanoasis/nerd-fonts) 3.2.1 — the
same base Fira Code with the Nerd Fonts glyphs patched in — subset to the
10,093 glyphs those glyphs live in (`U+23FB-23FE`, `U+26A1`, `U+276C-276F`,
`U+2B58`, `U+E000-F8FF`, `U+F0000-FFFFD`), and converted from the WOFF2 the old app
shipped to TrueType, because the same reason the two text faces are TrueType: the
macOS text system registers embedded fonts through Core Graphics, which does
not read WOFF2. Its outlines and advances are the patched font's, so a glyph
keeps the 1200-of-1950-unit advance — one cell — the parser lays its cell out
on, and the two text faces are left byte-identical rather than carrying a face
nobody can read.

The terminal draws a private use character with this family and everything else
with the text faces (`terminal::view::icon_family_for`), which is what the
`"Muster Nerd Mono", "Muster Nerd Icons"` stack did in the browser.

## The one glyph that is not from the subset

NerdSymbols carries an `m`. It is Fira Code's, copied in verbatim by the recipe
below, and nothing draws it. Core Text's font loader drops any family with no
`m` glyph — the precaution against a metrics call unwrapping a missing glyph —
and a dropped family does not fail a request for it: it answers with somebody
else's face. Without this glyph the icon family loads, is silently swapped for a
fallback, and every private use character goes back to being a tofu box.

## Reproducing

```sh
python3 -m fontTools.subset FiraCodeNerdFontMono-Regular.ttf \
  --unicodes='U+23FB-23FE,U+26A1,U+276C-276F,U+2B58,U+E000-F8FF,U+F0000-FFFFD' \
  --name-IDs='*' --notdef-outline --layout-features='*' --drop-tables=FFTM,DSIG \
  --output-file=NerdSymbols.ttf
```

Then the graft, which is the whole of what separates this file from the subset
(`icons` is the face read above, `m` is the glyph name in Fira Code's cmap):

```python
from fontTools.ttLib import TTFont
import copy

icons = TTFont("NerdSymbols.ttf", recalcBBoxes=False)
text = TTFont("FiraCode-Regular.ttf")
glyph = text["glyf"][text.getBestCmap()[0x6D]]
glyph.expand(text["glyf"])

icons["hmtx"].metrics["m"] = text["hmtx"]["m"]
icons["glyf"].glyphs["m"] = copy.deepcopy(glyph)
icons.getGlyphOrder().append("m")
icons["maxp"].numGlyphs = len(icons.getGlyphOrder())
for table in icons["cmap"].tables:
    if table.isUnicode():
        table.cmap[0x6D] = "m"
icons.save("NerdSymbols.ttf")
```

`hmtx` is read before `numGlyphs` moves, and `recalcBBoxes` stays off: the face
is one `longHorMetric` and 10,094 left side bearings, so the read is sized by the
old glyph count, and expanding `glyf` with recalculation on moves the 787 icons
whose cached `xMin` sits a unit from their real leftmost point. Both Fira Code
faces and this one are on the same 1950-unit grid, which is why the `m` is
copied rather than scaled.

The source patched TTF is a build input and is not bundled.

SHA-256:

- `FiraCode-Regular.ttf`: `5992ab9640e2df491b2f609467b1de60e8bc39b2c28db184342a0592d98f6117`
- `FiraCode-Bold.ttf`: `41f6554e845e2f5b70adad3950122334b866aac436793b7742ade600067701be`
- `NerdSymbols.ttf`: `4a217d996a34bddb3747b4bf1c18da1a23cf1935972094849911cb10cb54c4dd`