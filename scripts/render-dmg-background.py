#!/usr/bin/env python3
"""Render the chalkboard background of the macOS disk image window.

Maintainer tool: requires Pillow and the macOS system fonts Chalkduster and
Hiragino Maru Gothic. The rendered PNGs are committed, so packaging and CI do
not run this script. Icon positions come from assets/dmg/layout.json, which the
disk image settings also read.

    python3 scripts/render-dmg-background.py
"""

import json
import math
from pathlib import Path
import random

from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageFont

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / "assets" / "dmg"
CHALKDUSTER = "/System/Library/Fonts/Supplemental/Chalkduster.ttf"
KATAKANA_FONT = "/System/Library/Fonts/ヒラギノ丸ゴ ProN W4.ttc"
SEED = 0x6B6F6B75  # "koku": keeps chalk texture identical between renders

BOARD_TOP = (35, 99, 64)
BOARD_BOTTOM = (22, 72, 45)
CHALK = (244, 246, 240)
FRAME = (120, 82, 50)
FRAME_DARK = (84, 56, 34)
LEDGE_TOP = (222, 196, 150)
LEDGE_BOTTOM = (190, 158, 112)


def lerp(a, b, t):
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))


def seeded_noise(size, rng):
    """Uniform noise from the seeded generator (Image.effect_noise is not reproducible)."""
    return Image.frombytes("L", size, rng.randbytes(size[0] * size[1]))


def chalk_texture(size, rng, density):
    """Speckled alpha so strokes look like chalk rather than paint."""
    noise = seeded_noise(size, rng).point(lambda v: 255 if v < 255 * density else 110)
    return noise.filter(ImageFilter.GaussianBlur(0.4))


def chalk_layer(mask, rng, opacity):
    textured = ImageChops.multiply(mask, chalk_texture(mask.size, rng, 0.78))
    alpha = textured.point(lambda v: round(v * opacity))
    layer = Image.new("RGBA", mask.size, CHALK + (0,))
    layer.putalpha(alpha)
    return layer


def board(width, height, scale, rng):
    image = Image.new("RGBA", (width, height))
    pixels = image.load()
    cx, cy = width / 2, height * 0.42
    for y in range(height):
        base = lerp(BOARD_TOP, BOARD_BOTTOM, y / (height - 1))
        for x in range(width):
            # Gentle vignette toward the frame.
            d = math.hypot((x - cx) / width, (y - cy) / height)
            pixels[x, y] = lerp(base, (12, 45, 28), min(1.0, max(0.0, (d - 0.25) * 0.9))) + (255,)

    # Faint erased-chalk smudges.
    smudges = Image.new("L", (width, height), 0)
    draw = ImageDraw.Draw(smudges)
    for _ in range(9):
        x0 = rng.uniform(0, width)
        y0 = rng.uniform(0, height * 0.75)
        w = rng.uniform(120, 260) * scale
        h = rng.uniform(18, 40) * scale
        draw.ellipse((x0, y0, x0 + w, y0 + h), fill=round(rng.uniform(10, 22)))
    smudges = smudges.filter(ImageFilter.GaussianBlur(14 * scale))
    dust = seeded_noise((width, height), rng).point(lambda v: 18 if v > 247 else 0)
    haze = ImageChops.add(smudges, dust)
    white = Image.new("RGBA", (width, height), CHALK + (0,))
    white.putalpha(haze)
    return Image.alpha_composite(image, white)


def wood(draw, box, top, bottom, rng, scale, grain=True):
    x0, y0, x1, y1 = box
    for y in range(round(y0), round(y1)):
        t = (y - y0) / max(1, y1 - y0 - 1)
        draw.line((x0, y, x1, y), fill=lerp(top, bottom, t) + (255,))
    if grain:
        for _ in range(int((x1 - x0) / (18 * scale))):
            gy = rng.uniform(y0 + 2 * scale, y1 - 2 * scale)
            gx = rng.uniform(x0, x1 - 60 * scale)
            shade = lerp(bottom, FRAME_DARK, 0.35) + (70,)
            draw.line((gx, gy, gx + rng.uniform(40, 140) * scale, gy + rng.uniform(-1, 1) * scale),
                      fill=shade, width=max(1, round(scale)))


def chalk_arrow(size, start, end, scale, rng):
    mask = Image.new("L", size, 0)
    draw = ImageDraw.Draw(mask)
    (x0, y0), (x1, y1) = start, end
    steps = 60
    for stroke in range(3):
        jitter = rng.uniform(-1.2, 1.2) * scale
        points = []
        for i in range(steps + 1):
            t = i / steps
            wobble = math.sin(t * math.pi * 3 + stroke) * 2.2 * scale
            arc = -math.sin(t * math.pi) * 16 * scale  # a slight hand-drawn arc
            points.append((x0 + (x1 - x0) * t, y0 + (y1 - y0) * t + arc + wobble + jitter))
        draw.line(points, fill=230 - stroke * 40, width=round((6 - stroke) * scale), joint="curve")
    head = 20 * scale
    for side in (-1, 1):
        angle = math.radians(180 + side * 32)
        tip = (x1 + rng.uniform(-1, 1) * scale, y1)
        draw.line((tip, (tip[0] + head * math.cos(angle), tip[1] + head * math.sin(angle))),
                  fill=235, width=round(5.5 * scale))
    return mask


def chalk_folder(size, center, scale):
    mask = Image.new("L", size, 0)
    draw = ImageDraw.Draw(mask)
    cx, cy = center
    w, h = 64 * scale, 44 * scale
    left, top = cx - w / 2, cy - h / 2 + 6 * scale
    stroke = round(3.2 * scale)
    tab = [(left, top), (left + 22 * scale, top), (left + 28 * scale, top - 7 * scale),
           (left + w, top - 7 * scale), (left + w, top)]
    draw.line(tab, fill=210, width=stroke, joint="curve")
    draw.rounded_rectangle((left, top, left + w, top + h), radius=5 * scale, outline=235, width=stroke)
    # The Applications "A".
    ax0, ay0 = cx, top + 10 * scale
    draw.line(((cx - 11 * scale, top + h - 9 * scale), (ax0, ay0), (cx + 11 * scale, top + h - 9 * scale)),
              fill=225, width=round(2.6 * scale), joint="curve")
    draw.line(((cx - 7 * scale, top + h - 17 * scale), (cx + 7 * scale, top + h - 17 * scale)),
              fill=215, width=round(2.4 * scale))
    return mask


def text_mask(size, text, font, center, spacing=0):
    mask = Image.new("L", size, 0)
    draw = ImageDraw.Draw(mask)
    if spacing:
        widths = [draw.textlength(ch, font=font) for ch in text]
        total = sum(widths) + spacing * (len(text) - 1)
        x = center[0] - total / 2
        for ch, w in zip(text, widths):
            draw.text((x, center[1]), ch, font=font, fill=255, anchor="lm")
            x += w + spacing
    else:
        draw.text(center, text, font=font, fill=255, anchor="mm")
    return mask


def render(layout, scale):
    rng = random.Random(SEED)
    # Taller than the visible content: title bar heights differ between macOS releases.
    width, height = layout["background"]["width"] * scale, layout["background"]["height"] * scale
    size = (width, height)
    image = board(width, height, scale, rng)
    draw = ImageDraw.Draw(image)

    icon = layout["icon_size"] * scale
    (kx, ky), (ax, ay) = (tuple(v * scale for v in layout["icons"][name])
                          for name in ("Kokuban.app", "Applications"))

    # Chalk ledge under the icon labels, so Finder's label text stays readable.
    ledge_top = ky + icon / 2 + 2 * scale
    ledge_bottom = ledge_top + 30 * scale
    wood(draw, (0, ledge_top, width, ledge_bottom), LEDGE_TOP, LEDGE_BOTTOM, rng, scale)
    draw.line((0, ledge_top, width, ledge_top), fill=(246, 230, 196, 255), width=round(1.5 * scale))
    wood(draw, (0, ledge_bottom, width, height), FRAME, FRAME_DARK, rng, scale)
    draw.line((0, ledge_bottom, width, ledge_bottom), fill=FRAME_DARK + (255,), width=round(2 * scale))
    # Chalk sticks resting on the ledge.
    for x, length, tilt in ((width - 92 * scale, 32, -1.5), (width - 50 * scale, 20, 1.0)):
        y = ledge_top + 13 * scale
        draw.rounded_rectangle((x, y + tilt * scale, x + length * scale, y + 7 * scale + tilt * scale),
                               radius=3.5 * scale, fill=(247, 247, 242, 255), outline=(214, 214, 206, 255))

    # Side and top frame.
    frame = round(10 * scale)
    wood(draw, (0, 0, width, frame), FRAME, FRAME_DARK, rng, scale, grain=False)
    for x0 in (0, width - frame):
        draw.rectangle((x0, 0, x0 + frame, ledge_top), fill=FRAME + (255,))
    draw.rectangle((frame, frame, width - frame, ledge_top), outline=FRAME_DARK + (255,), width=max(1, round(scale)))

    katakana = ImageFont.truetype(KATAKANA_FONT, round(30 * scale))
    image = Image.alpha_composite(image, chalk_layer(
        text_mask(size, "コクバン", katakana, (width / 2, 54 * scale), spacing=round(10 * scale)), rng, 0.92))
    underline = Image.new("L", size, 0)
    ImageDraw.Draw(underline).line(((width / 2 - 70 * scale, 76 * scale), (width / 2 + 70 * scale, 77.5 * scale)),
                                   fill=150, width=round(2 * scale))
    image = Image.alpha_composite(image, chalk_layer(underline, rng, 0.7))

    title = ImageFont.truetype(CHALKDUSTER, round(23 * scale))
    image = Image.alpha_composite(image, chalk_layer(
        text_mask(size, "Drag Kokuban to Applications", title, (width / 2, 112 * scale)), rng, 0.95))

    # Some Finder releases draw only a dashed placeholder for the Applications
    # link. A chalk folder inside the icon's opaque area shows through it and is
    # hidden when Finder draws the real folder icon on top.
    image = Image.alpha_composite(image, chalk_layer(chalk_folder(size, (ax, ay), scale), rng, 0.85))

    gap = icon / 2 + 18 * scale
    arrow = chalk_arrow(size, (kx + gap, ky), (ax - gap, ay), scale, rng)
    image = Image.alpha_composite(image, chalk_layer(arrow, rng, 0.9))

    return image.convert("RGB")


def main():
    layout = json.loads((ASSETS / "layout.json").read_text())
    for scale, name in ((1, "background.png"), (2, "background@2x.png")):
        image = render(layout, scale)
        image.save(ASSETS / name, optimize=True, dpi=(72 * scale, 72 * scale))
        print(f"wrote {ASSETS / name} {image.size[0]}x{image.size[1]}")


if __name__ == "__main__":
    main()
