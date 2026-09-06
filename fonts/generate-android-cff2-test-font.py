"""Generate original geometric CFF2 outlines for Android fallback regression tests.

Run with Python and fonttools==4.59.1. No third-party font outlines are used.
The fixed timestamps and one-face collection make the fixture reproducible.
"""

from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.t2CharStringPen import T2CharStringPen
from fontTools.ttLib import TTCollection


def rectangle(left, bottom, right, top):
    pen = T2CharStringPen(None, None, CFF2=True)
    pen.moveTo((left, bottom))
    pen.lineTo((left, top))
    pen.lineTo((right, top))
    pen.lineTo((right, bottom))
    pen.closePath()
    return pen.getCharString()


def curved():
    pen = T2CharStringPen(None, None, CFF2=True)
    pen.moveTo((50, -100))
    pen.curveTo((50, 800), (900, 800), (900, -100))
    pen.closePath()
    return pen.getCharString()


builder = FontBuilder(1000, isTTF=False)
characters = {0x3000: "ideographicspace", 0x3131: "kiyeok", 0xAC00: "ga"}
names = [".notdef", *characters.values()]
builder.setupGlyphOrder(names)
builder.setupCharacterMap(characters)
builder.setupHorizontalMetrics({name: (1000, 50) for name in names})
builder.setupHorizontalHeader(ascent=900, descent=-250, lineGap=50)
builder.setupNameTable({"familyName": "Kokuban CFF2 Test", "styleName": "Regular",
                       "uniqueFontIdentifier": "KokubanCFF2Test",
                       "fullName": "Kokuban CFF2 Test Regular",
                       "psName": "KokubanCFF2Test-Regular", "version": "Version 1.0"})
builder.setupOS2(sTypoAscender=900, sTypoDescender=-250, sTypoLineGap=50,
                usWinAscent=900, usWinDescent=250)
builder.setupPost()
builder.setupFvar([("wght", 400, 400, 900, "Weight")], [])
outlines = {name: rectangle(50, 0, 900, 700) for name in names}
outlines["ideographicspace"] = T2CharStringPen(None, None, CFF2=True).getCharString()
outlines["ga"] = curved()
builder.setupCFF2(outlines, regions=[{"wght": (0.0, 1.0, 1.0)}])
builder.setupMaxp()
builder.font["head"].created = 2082844800
builder.font["head"].modified = 2082844800
builder.font.recalcTimestamp = False
collection = TTCollection()
collection.fonts = [builder.font]
collection.save(Path(__file__).with_name("android-cff2-test.ttc"))
