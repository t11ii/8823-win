"""Build assets/fonts/8823Gothic-Regular.ttf: JetBrains Mono for Latin + BIZ UDGothic for Japanese.

Both fonts are SIL OFL 1.1. Merging them into ONE font gives every glyph the same
vertical metrics, so Latin and kana/kanji share a baseline (two separate fonts in egui
are each positioned by their own ascent, which makes mixed names look uneven).

Usage: python3 tools/build_font.py JetBrainsMono-Regular.ttf BIZUDGothic-Regular.ttf
Requires: pip install fonttools
"""
import sys
from fontTools.ttLib import TTFont
from fontTools.ttLib.scaleUpem import scale_upem
from fontTools.merge import Merger, Options

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
