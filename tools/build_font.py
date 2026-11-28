"""Build assets/fonts/8823Gothic-Regular.ttf: JetBrains Mono for Latin + BIZ UDGothic for Japanese.

Both fonts are SIL OFL 1.1. Merging them into ONE font gives every glyph the same
vertical metrics, so Latin and kana/kanji share a baseline (two separate fonts in egui
are each positioned by their own ascent, which makes mixed names look uneven).

Usage: uv run --with fonttools tools/build_font.py JetBrainsMono-Regular.ttf BIZUDGothic-Regular.ttf
       uv run --with fonttools tools/build_font.py --widths-only assets/fonts/8823Gothic-Regular.ttf
       uv run --with fonttools tools/build_font.py --fit-only assets/fonts/8823Gothic-Regular.ttf

Use --fit-only on a merged font that has never had the optical fit applied.
"""
import sys
from pathlib import Path
from tempfile import TemporaryDirectory
from fontTools.ttLib import TTFont
from fontTools.ttLib.scaleUpem import scale_upem
from fontTools.merge import Merger, Options


# Kana and kanji fill the whole em (about -195..1750 units) while JetBrains Mono capitals
# span 0..1495, so in mixed names the Japanese towers over the Latin and looks shifted.
# Shrink the Japanese glyphs around the Latin optical centre and drop them a little
# (option F, picked by Takeo 2026-11-20). Advance widths stay the same.
JA_SCALE = 0.92
JA_SHIFT = -40  # font units at 2048 upem (about 2% of the em)
LATIN_CENTRE_Y = 748  # middle of the capitals and digits


def is_japanese(u):
    return (0x2E80 <= u <= 0x2FFF or 0x3000 <= u <= 0x30FF or 0x3190 <= u <= 0x9FFF
            or 0xF900 <= u <= 0xFAFF or 0xFF00 <= u <= 0xFFEF or u >= 0x20000)


def fit_japanese(font):
    """Scale and shift the Japanese glyphs (outlines are redrawn, composites decomposed)."""
    from fontTools.pens.ttGlyphPen import TTGlyphPen
    from fontTools.pens.transformPen import TransformPen

    names = {g for u, g in font.getBestCmap().items() if is_japanese(u)}
    glyphs, glyf, hmtx = font.getGlyphSet(), font["glyf"], font["hmtx"]
    s, cy = JA_SCALE, LATIN_CENTRE_Y
    redrawn = {}
    for n in names:
        cx = hmtx[n][0] / 2
        pen = TTGlyphPen(glyphs)
        glyphs[n].draw(TransformPen(pen, (s, 0, 0, s, cx - s * cx, cy - s * cy + JA_SHIFT)))
        redrawn[n] = pen.glyph()
    for n, g in redrawn.items():
        glyf[n] = g
        g.recalcBounds(glyf)
        hmtx[n] = (hmtx[n][0], getattr(g, "xMin", 0))
    return len(names)


def align_widths(font):
    """Fit visible glyphs to half-em and full-em cells."""
    from fontTools.pens.recordingPen import DecomposingRecordingPen
    from fontTools.pens.ttGlyphPen import TTGlyphPen
    from fontTools.pens.transformPen import TransformPen

    half = font["head"].unitsPerEm // 2
    glyphs, glyf, hmtx = font.getGlyphSet(), font["glyf"], font["hmtx"]
    redrawn = {}
    for name in sorted(set(font.getBestCmap().values())):
        advance, _ = hmtx[name]
        if advance in (0, half, 2 * half):
            continue
        width = half if advance < 1.5 * half else 2 * half
        scale = min(1.0, width / advance)
        offset = (width - scale * advance) / 2
        # Resolve composite outlines before resizing their base glyphs.
        recording = DecomposingRecordingPen(glyphs)
        glyphs[name].draw(recording)
        pen = TTGlyphPen(None)
        recording.replay(TransformPen(pen, (scale, 0, 0, 1, offset, 0)))
        redrawn[name] = (width, pen.glyph())

    for name, (width, glyph) in redrawn.items():
        glyf[name] = glyph
        glyph.recalcBounds(glyf)
        hmtx[name] = (width, getattr(glyph, "xMin", 0))
    return len(redrawn)


if __name__ == "__main__" and sys.argv[1:2] == ["--widths-only"]:
    f = TTFont(sys.argv[2], recalcTimestamp=False)
    count = align_widths(f)
    if count:
        f.save(sys.argv[2])
    print("aligned", count, "glyphs")
    sys.exit(0)


if __name__ == "__main__" and sys.argv[1:2] == ["--fit-only"]:
    # Apply fit_japanese to an already merged font in place:
    #   uv run --with fonttools tools/build_font.py --fit-only assets/fonts/8823Gothic-Regular.ttf
    f = TTFont(sys.argv[2])
    print("adjusted", fit_japanese(f), "Japanese glyphs")
    f.save(sys.argv[2])
    sys.exit(0)

jb_path, biz_path = sys.argv[1], sys.argv[2]
out = "assets/fonts/8823Gothic-Regular.ttf"

biz = TTFont(biz_path)
upem = biz["head"].unitsPerEm  # 2048
jb = TTFont(jb_path)
scale_upem(jb, upem)
# Drop layout and variation tables the merger can't reconcile.
for font in (jb, biz):
    for tag in ("GSUB", "GPOS", "GDEF", "BASE", "vhea", "vmtx", "VORG", "STAT", "gasp", "prep", "fpgm", "cvt ", "hdmx", "LTSH", "VDMX", "kern", "meta", "DSIG"):
        if tag in font:
            del font[tag]
# JetBrains Mono first: its glyphs win wherever both fonts have one (ASCII, Latin, symbols).
with TemporaryDirectory() as temporary:
    latin_path = Path(temporary) / "latin.ttf"
    japanese_path = Path(temporary) / "japanese.ttf"
    jb.save(latin_path)
    biz.save(japanese_path)
    merged = Merger(options=Options(drop_tables=[])).merge([str(latin_path), str(japanese_path)])

fit_japanese(merged)
align_widths(merged)

# One set of vertical metrics for everything (BIZ UDGothic's em box, a bit of room below for g/j/y).
asc, desc = 1802, -346
merged["hhea"].ascent, merged["hhea"].descent, merged["hhea"].lineGap = asc, desc, 0
os2 = merged["OS/2"]
os2.sTypoAscender, os2.sTypoDescender, os2.sTypoLineGap = asc, desc, 0
os2.usWinAscent, os2.usWinDescent = asc, -desc

name = merged["name"]
for rec in list(name.names):
    if rec.nameID in (1, 3, 4, 6, 16, 17, 21, 22):
        name.removeNames(nameID=rec.nameID)
for nid, val in ((1, "8823 Gothic"), (2, "Regular"), (3, "8823 Gothic Regular"), (4, "8823 Gothic Regular"), (6, "8823Gothic-Regular")):
    name.setName(val, nid, 3, 1, 0x409)
name.setName(
    "Copyright 2020 The JetBrains Mono Project Authors; Copyright 2022 The BIZ UDGothic Project Authors. Merged for 8823.",
    0, 3, 1, 0x409,
)
merged.save(out)
print("wrote", out)
