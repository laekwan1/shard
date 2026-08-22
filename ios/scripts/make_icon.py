#!/usr/bin/env python3
"""Generate the iOS app icon from the desktop's SHARD_MARK.

The desktop logo (assets/ui/app.js, SHARD_MARK) is a rounded square in the
accent colour with a diagonal "power-cut" gap crossing it. We reproduce it
full-bleed on the app's dark surface so the phone icon reads the same.

Coordinates follow the SVG: a 2x2 viewBox, the square spanning -0.72..0.72,
corner radius 0.22, and a diagonal stroke of width 0.22 removed from it.
"""
import os
from PIL import Image, ImageDraw

S = 1024
# A light ground like YouTube's and Chrome's icons, so the tile is bright on the
# home screen rather than a black square. The mark is the accent colour, and its
# power-cut shows the ground through it.
BACKGROUND = (255, 255, 255, 255)
ACCENT = (45, 212, 191, 255)    # --accent  #2dd4bf

# Supersample for clean edges, then downscale.
SS = 4
size = S * SS

img = Image.new("RGBA", (size, size), BACKGROUND)
draw = ImageDraw.Draw(img)


def p(u):
    return int(round((u + 1.0) / 2.0 * size))


# A centred rounded square, half the tile wide, in the accent colour — the logo
# sits on the light ground rather than filling the whole icon.
span = 0.5
x0, y0, x1, y1 = p(-span), p(-span), p(span), p(span)
radius = int(round(0.16 * (x1 - x0)))
draw.rounded_rectangle([x0, y0, x1, y1], radius=radius, fill=ACCENT)

# The diagonal power-cut in the background colour, clipped to the mark by only
# drawing where the mark is (a stroke across the whole tile is fine — outside the
# square it just repaints the ground its own colour).
width = int(round(0.15 * (x1 - x0)))
draw.line([p(-span - 0.2), p(-span - 0.2), p(span + 0.2), p(span + 0.2)],
          fill=BACKGROUND, width=width)

img = img.resize((S, S), Image.LANCZOS).convert("RGB")

out_dir = os.path.join(os.path.dirname(__file__), "..",
                       "Shard", "Assets.xcassets", "AppIcon.appiconset")
os.makedirs(out_dir, exist_ok=True)
out = os.path.join(out_dir, "icon-1024.png")
img.save(out, "PNG")
print("wrote", out, img.size)
