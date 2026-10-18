# Bundled fonts

Three files, under two family names, shipping the same typography as the two
patched TTF this replaced for 4.6MB of them.

`FiraCode-Regular.woff2` and `FiraCode-Bold.woff2` are
[Fira Code](https://github.com/tonsky/FiraCode) 6.2, from the `woff2/` folder
of the release zip with the TrueType hinting removed by
`scripts/build-fonts.py`. Outlines, advance widths and the GSUB/GPOS features
are untouched, so these are the faces upstream drew. The license is
`LICENSE-FiraCode.txt` (SIL Open Font License 1.1), it declares no reserved font
name, and it travels with the files, which is what that license asks for.

`NerdSymbols.woff2` is the icon half of
[FiraCode Nerd Font Mono](https://github.com/ryanoasis/nerd-fonts) 3.2.1, made
from the same base Fira Code with the Nerd Fonts glyphs patched in. It is cut
down to the 10,093 glyphs used for icons: the private-use range, plus the ten
symbols patched outside it (`U+23FB-23FE`, `U+26A1`, `U+276C-276F`,
`U+2B58`). The outlines and advance widths are those of the patched font, so
the icons keep their terminal-cell width. It is shipped exactly as built. The
source glyph-license inventory has not been verified; the Fira Code OFL notice
does not establish the license for separate patched glyphs.

They are bundled so terminal and editor typography is consistent regardless of
which fonts happen to be installed on the machine. The terminal waits for these
faces before opening xterm, so its initial grid and glyph atlas use the bundled
font rather than a fallback.

## Why the hinting is stripped

Upstream ships full TrueType hinting: a 3.6KB `fpgm`, a `prep`, a `cvt `, and a
`gasp` that asks for grid-fitting at every ppem, so nothing escapes it at the
sizes a terminal actually uses. macOS applies none of it, because CoreText
rasterizes outlines as they are. Linux applies all of it, because FreeType
executes it, and at a 1x display a 14px cell is exactly where snapping stems to
the pixel grid reads as uneven stem weight from glyph to glyph. Under
fontconfig's default `hintmetrics=hintfull` the advances get rounded to whole
pixels too, so the terminal cell stops matching the macOS one.

Dropping the bytecode has both platforms draw the same unhinted outlines, which
is what the app already looked like on macOS. `scripts/build-fonts.py` splices
the instruction bytes out and leaves every other byte alone, then checks the
result it is about to write: outlines, advances, cmap, `name` and the GSUB/GPOS
feature lists all have to match the input, and no glyph may still carry
bytecode.

`NerdSymbols.woff2` is left hinted, deliberately. 9 of its 10089 drawn glyphs
carry instructions -- `.notdef` and `U+EE00-EE09`, none of them in the powerline
range the terminal loads -- against every drawn glyph of the text faces. Making
that consistent would mean rewriting a 908KB third-party file to move 2KB of
bytecode, so it stays byte-identical to what the subset produced.

## Why the icons are a separate file

They used to be inside both text faces, which meant a face nobody reads was
downloaded and parsed twice. More than the weight, the icon half cannot be
replaced by an upstream Nerd Fonts symbols file: those draw on a 1em grid while
this cell is 0.6154em, so xterm measures every icon as nearly two cells wide.
The icons have to come from a font patched _to_ this cell, which is why this one
is a subset of the file it replaced rather than a download.

To reproduce the subset (the patched TTF is a build input and is not bundled):

```sh
python3 -m fontTools.subset FiraCodeNerdFontMono-Regular.ttf \
  --unicodes='U+23FB-23FE,U+26A1,U+276C-276F,U+2B58,U+E000-F8FF,U+F0000-FFFFD' \
  --name-IDs='*' --notdef-outline --layout-features='*' --drop-tables=FFTM,DSIG \
  --flavor=woff2 --output-file=NerdSymbols.woff2
```

Then take the hinting out of the two text faces:

```sh
python3 scripts/build-fonts.py \
  FiraCode-Regular.woff2 FiraCode-Bold.woff2
```

The compressed bytes depend on the fontTools version, so after regenerating,
re-pin `EXPECTED_FONT_SHA256` in `scripts/artifacts.test.mjs` with the output of
`shasum -a 256 src/assets/fonts/*.woff2`.

To check the shipped faces are unhinted without rewriting them:

```sh
python3 scripts/build-fonts.py --self-test
```

It asserts no glyph in each face carries instructions, then reinjects
instructions into one glyph, writes the face out, reads it back, and asserts the
count is exactly that glyph. That last part is the regression guard: a face read
back from disk keeps every glyph unexpanded, and an unexpanded glyph has no
`program` at all, so a count that skips decompiling reports nothing and would
wave a hinted face straight through.

To change the face, replace the files, keep the names, and update the
`@font-face` blocks in `src/muster.css` and `TERMINAL_FONT_FAMILY` in
`src/lib/muster-terminal.ts` together — the app has one face on purpose. The
icon family has to be named in every place the text one is, or a glyph in the
private use range falls back to a face of its own and draws at the wrong width.
