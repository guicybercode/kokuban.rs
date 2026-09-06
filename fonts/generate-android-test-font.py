"""Generate the original, geometric font used only by Android atlas unit tests.

Run with Python and fonttools==4.59.1. The fixture has no third-party outlines.
The constant timestamps make the output reproducible.
"""

from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen


def rectangle(left, bottom, right, top):
    pen = TTGlyphPen(None)
    if right > left:
        pen.moveTo((left, bottom))
        pen.lineTo((left, top))
        pen.lineTo((right, top))
        pen.lineTo((right, bottom))
        pen.closePath()
    return pen.glyph()


builder = FontBuilder(1000, isTTF=True)
characters = {32: "space", 63: "question", 65: "A", 77: "M", 103: "g", 233: "eacute", 0x03A9: "curve", 0xFFFD: "replacement"}
names = [".notdef", *characters.values()]
builder.setupGlyphOrder(names)
builder.setupCharacterMap(characters)
glyphs = {name: rectangle(50, 0, 500, 700) for name in names}
glyphs["space"] = rectangle(0, 0, 0, 0)
glyphs["g"] = rectangle(50, -200, 500, 500)
glyphs["eacute"] = rectangle(50, 0, 500, 850)
curve = TTGlyphPen(None)
curve.moveTo((50, -100))
curve.qCurveTo((275, 1000), (500, -100))
curve.closePath()
glyphs["curve"] = curve.glyph()
builder.setupGlyf(glyphs)
builder.setupHorizontalMetrics({name: (600, 50) for name in names})
builder.setupHorizontalHeader(ascent=900, descent=-250, lineGap=50)
builder.setupNameTable({"familyName": "Kokuban Atlas Test", "styleName": "Regular", "uniqueFontIdentifier": "KokubanAtlasTest", "fullName": "Kokuban Atlas Test Regular", "psName": "KokubanAtlasTest-Regular", "version": "Version 1.0"})
builder.setupOS2(sTypoAscender=900, sTypoDescender=-250, sTypoLineGap=50, usWinAscent=900, usWinDescent=250)
builder.setupPost(isFixedPitch=1)
builder.setupMaxp()
builder.font["head"].created = 2082844800
builder.font["head"].modified = 2082844800
builder.font.recalcTimestamp = False
builder.save(Path(__file__).with_name("android-test.ttf"))
