#!/usr/bin/env python3
"""Strip TrueType hinting out of the bundled text faces.

Both text faces shipped Fira Code's full hinting bytecode: a 3.6KB `fpgm`, a
`prep`, a `cvt `, and a `gasp` asking for grid-fitting at every ppem with no
large-ppem escape hatch. CoreText, which macOS uses, ignores that bytecode
entirely, so the app already looked right there. FreeType, which Linux uses,
executes it, and at 1x a 14px cell is exactly where snapping stems to the pixel
grid shows: uneven stem weight between glyphs, and under fontconfig's default
`hintmetrics=hintfull`, advances rounded to whole pixels so the terminal cell
drifts from the macOS one.

Removing the bytecode leaves both platforms rasterizing the same unhinted
outlines -- which is what macOS already drew, and what `src/muster.css` says
bundling these faces is for. Outlines, advance widths and the GSUB/GPOS
features Fira Code's ligatures depend on are left untouched.

Inputs are the Fira Code 6.2 release `woff2/` folder. Re-running this on an
already-stripped file rewrites the same bytes, so it doubles as the check that
the committed faces are hint-free.

Note that emptying a glyph's `program` is not enough: `_g_l_y_f.py` decides
whether to emit an instruction block from `hasattr(glyph, "program")`, so the
attribute has to go. `removeHinting()` is what fontTools' own subsetter uses
for `--no-hinting`, and it only removes the instructions when the glyph still
holds its unexpanded `data` -- so this script never touches `getGlyphSet()` or
any other expanding accessor before trimming.

`recalcBBoxes=False` is what keeps the outlines bit-for-bit. With it on, `glyf`
expands every glyph and renormalizes the bounds, and in NerdSymbols 787 icons
whose cached `xMin` sits one unit away from their real leftmost point come back
moved by that unit. With it off, `glyf.compile` writes each trimmed `data`
verbatim, so the only bytes that change are the instruction bytes that `trim`
splices out.

The committed binaries were produced with fontTools 4.62.1. A different version
can compress differently, and the sha256 pins in `scripts/artifacts.test.mjs`
are what will report that.

Usage:
    python3 scripts/build-fonts.py FiraCode-Regular.woff2 FiraCode-Bold.woff2 ...
    python3 scripts/build-fonts.py --self-test [faces...]
"""

import io
import sys
from pathlib import Path

from fontTools.pens.recordingPen import RecordingPen
from fontTools.ttLib import TTFont

HINT_TABLES = ("fpgm", "prep", "cvt ", "gasp")
# `head.flags` bit 2: "instructions may depend on pointsize".
POINTSIZE_FLAG = 1 << 2
BUNDLED_FONTS = Path(__file__).resolve().parent.parent / "src" / "assets" / "fonts"
# Only the text faces get de-hinted, and only they are expected to be unhinted
# afterwards. NerdSymbols is shipped as upstream patched it: 9 of its 10089 drawn
# glyphs carry instructions (`.notdef` and U+EE00-EE09), none of them in the
# powerline range the terminal loads, against every drawn glyph of the text
# faces. Rewriting a 908KB third-party icon file to move 2KB of bytecode is not
# a trade worth making, so it stays byte-identical to the file it was built from.
DEHINTED_FACES = ("FiraCode-Regular.woff2", "FiraCode-Bold.woff2")


def glyphs_with_bytecode(font):
    """Glyphs carrying instructions. Expands `font`, so never call this on a face being edited.

    The `ensureDecompiled()` call is load-bearing: a face read back from disk keeps
    every glyph unexpanded, and an unexpanded glyph has no `program` at all, so
    counting without expanding first reports nothing no matter how much bytecode
    is actually in the file. `self_test` pins that down.
    """
    glyf = font["glyf"]
    glyf.ensureDecompiled()
    return [
        name
        for name, glyph in glyf.glyphs.items()
        if (program := getattr(glyph, "program", None)) is not None and program.bytecode
    ]


def outlines(font):
    """Every glyph's advance plus its full drawing, composites included."""
    glyph_set = font.getGlyphSet()
    drawn = {}
    for name in font.getGlyphOrder():
        pen = RecordingPen()
        glyph_set[name].draw(pen)
        drawn[name] = (glyph_set[name].width, pen.value)
    return drawn


def layout_features(font, tag):
    if tag not in font:
        return None
    table = font[tag]
    table.ensureDecompiled()
    return sorted(record.FeatureTag for record in table.table.FeatureList.FeatureRecord)


def fingerprint(font):
    """Everything this transform must leave alone."""
    return {
        "glyphOrder": font.getGlyphOrder(),
        "numGlyphs": font["maxp"].numGlyphs,
        "unitsPerEm": font["head"].unitsPerEm,
        "hmtx": [font["hmtx"][name] for name in font.getGlyphOrder()],
        "outlines": outlines(font),
        "cmap": sorted(font.getBestCmap().items()),
        "names": [font["name"].getDebugName(name_id) for name_id in range(18)],
        "italicAngle": font["post"].italicAngle,
        "widthClass": font["OS/2"].usWidthClass,
        "GSUB": layout_features(font, "GSUB"),
        "GPOS": layout_features(font, "GPOS"),
    }


def assert_unhinted(font):
    leftover = glyphs_with_bytecode(font)
    assert not leftover, f"glyphs still carry hinting bytecode: {leftover[:5]}"
    present = [tag for tag in HINT_TABLES if tag in font]
    assert not present, f"hinting tables survived: {present}"
    assert not font["head"].flags & POINTSIZE_FLAG, (
        "head.flags still advertises pointsize-dependent instructions"
    )


def strip(path):
    raw = Path(path).read_bytes()
    source = TTFont(io.BytesIO(raw))
    expected = fingerprint(source)
    cleared = len(glyphs_with_bytecode(source))

    target = TTFont(io.BytesIO(raw), recalcTimestamp=False, recalcBBoxes=False)
    for glyph in target["glyf"].glyphs.values():
        glyph.removeHinting()
    for tag in HINT_TABLES:
        if tag in target:
            del target[tag]
    target["head"].flags &= ~POINTSIZE_FLAG

    output = io.BytesIO()
    target.flavor = "woff2"
    target.save(output)
    written = output.getvalue()

    # Verify the bytes that are about to be committed, not the in-memory face.
    result = TTFont(io.BytesIO(written))
    assert result.flavor == "woff2", f"{path}: lost its woff2 flavor"
    assert_unhinted(result)
    assert fingerprint(result) == expected, (
        f"{path}: transform touched something it must not"
    )

    Path(path).write_bytes(written)
    return cleared, len(written)


def self_test(paths):
    """Pin down that instructions hidden in an unexpanded glyph still get counted.

    Builds nothing synthetic: takes a shipped (unhinted) face, gives one glyph
    instructions back, writes it out, reads it back and requires the count to be
    exactly that glyph. The reloaded face has every glyph unexpanded, so this is
    the case that used to pass silently and let a hinted face through the assert.
    """
    from fontTools.ttLib.tables.ttProgram import Program

    assert paths, "self-test needs at least one face; a zero-face run passes vacuously"
    for path in paths:
        raw = Path(path).read_bytes()
        assert not glyphs_with_bytecode(TTFont(io.BytesIO(raw))), (
            f"{path}: shipped face is still hinted"
        )

        target = TTFont(
            io.BytesIO(raw), recalcTimestamp=False, recalcBBoxes=False
        ).getGlyphOrder()[1]
        face = TTFont(io.BytesIO(raw), recalcTimestamp=False, recalcBBoxes=False)
        glyph = face["glyf"].glyphs[target]
        glyph.expand(face["glyf"])
        glyph.program = Program()
        glyph.program.fromBytecode(b"\xb0\x01\x00\x00\x2b")

        output = io.BytesIO()
        face.flavor = "woff2"
        face.save(output)
        found = glyphs_with_bytecode(TTFont(io.BytesIO(output.getvalue())))
        assert found == [target], (
            f"{path}: reinjected bytecode went unnoticed, found {found}"
        )


def main(argv):
    if argv and argv[0] == "--self-test":
        paths = argv[1:] or [str(BUNDLED_FONTS / name) for name in DEHINTED_FACES]
        self_test(paths)
        print(
            f"self-test ok over {len(paths)} face(s): de-hinted faces carry no instructions, and instructions in an unexpanded glyph are still counted"
        )
        return 0
    if not argv:
        print(__doc__, file=sys.stderr)
        return 2
    for path in argv:
        cleared, size = strip(path)
        print(f"{path}: stripped hinting from {cleared} glyphs -> {size} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
