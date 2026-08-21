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
SURFACE = (17, 19, 24, 255)     # --surface #111318
ACCENT = (45, 212, 191, 255)    # --accent  #2dd4bf


def u2px(u):
    """SVG unit in [-1,1] -> pixel."""
    return (u + 1.0) / 2.0 * S


# Supersample for clean edges, then downscale.
SS = 4
size = S * SS

img = Image.new("RGBA", (size, size), SURFACE)
draw = ImageDraw.Draw(img)


def p(u):
    return int(round((u + 1.0) / 2.0 * size))


# Rounded square in the accent colour.
x0, y0, x1, y1 = p(-0.72), p(-0.72), p(0.72), p(0.72)
radius = int(round(0.22 / 2.0 * size))
draw.rounded_rectangle([x0, y0, x1, y1], radius=radius, fill=ACCENT)

# The diagonal power-cut: a stroke of the surface colour from corner to corner,
# so the gap shows the background through the mark, exactly as the SVG mask does.
width = int(round(0.22 / 2.0 * size))
draw.line([p(-1.5), p(-1.5), p(1.5), p(1.5)], fill=SURFACE, width=width)

img = img.resize((S, S), Image.LANCZOS).convert("RGB")

out_dir = os.path.join(os.path.dirname(__file__), "..",
                       "Shard", "Assets.xcassets", "AppIcon.appiconset")
os.makedirs(out_dir, exist_ok=True)
out = os.path.join(out_dir, "icon-1024.png")
img.save(out, "PNG")
print("wrote", out, img.size)
