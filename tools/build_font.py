"""Build assets/fonts/8823Gothic-Regular.ttf: JetBrains Mono for Latin + BIZ UDGothic for Japanese.

Both fonts are SIL OFL 1.1. Merging them into ONE font gives every glyph the same
vertical metrics, so Latin and kana/kanji share a baseline (two separate fonts in egui
are each positioned by their own ascent, which makes mixed names look uneven).

Usage: python3 tools/build_font.py JetBrainsMono-Regular.ttf BIZUDGothic-Regular.ttf
       python3 tools/build_font.py --fit-only assets/fonts/8823Gothic-Regular.ttf  (fit step only, on a font merged
       before this step existed; running it twice shrinks the Japanese twice)
Requires: pip install fonttools
"""
import sys
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


if __name__ == "__main__" and sys.argv[1:2] == ["--fit-only"]:
    # Apply fit_japanese to an already merged font in place:
    #   python3 tools/build_font.py --fit-only assets/fonts/8823Gothic-Regular.ttf
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
# Drop layout/variation tables the merger can't reconcile; egui does no OpenType shaping.
for font in (jb, biz):
    for tag in ("GSUB", "GPOS", "GDEF", "BASE", "vhea", "vmtx", "VORG", "STAT", "gasp", "prep", "fpgm", "cvt ", "hdmx", "LTSH", "VDMX", "kern", "meta", "DSIG"):
        if tag in font:
            del font[tag]
jb.save("/tmp/_jb.ttf")
biz.save("/tmp/_biz.ttf")

# JetBrains Mono first: its glyphs win wherever both fonts have one (ASCII, Latin, symbols).
merged = Merger(options=Options(drop_tables=[])).merge(["/tmp/_jb.ttf", "/tmp/_biz.ttf"])

fit_japanese(merged)

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
