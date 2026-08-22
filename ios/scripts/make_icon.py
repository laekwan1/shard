#!/usr/bin/env python3
"""iOS app icon: the flat SHARD_MARK (teal, diagonal power-cut) on a white
ground. Single image — the light look, kept in every appearance."""
import os
import numpy as np
from PIL import Image, ImageDraw

S = 1024; SS = 4; size = S * SS
ACCENT = np.array([0x18, 0xB6, 0xA4], dtype=float)   # accent, slightly deep teal
BG = np.array([255, 255, 255], dtype=float)

def p(u): return int(round((u + 1.0) / 2.0 * size))

m = Image.new("L", (size, size), 0)
d = ImageDraw.Draw(m)
x0, y0, x1, y1 = p(-0.72), p(-0.72), p(0.72), p(0.72)
d.rounded_rectangle([x0, y0, x1, y1], radius=int(round(0.22 / 2.0 * size)), fill=255)
d.line([p(-1.5), p(-1.5), p(1.5), p(1.5)], fill=0, width=int(round(0.22 / 2.0 * size)))
a = (np.asarray(m, dtype=float) / 255.0)[..., None]
out = ACCENT * a + BG * (1 - a)
img = Image.fromarray(out.astype(np.uint8), "RGB").resize((S, S), Image.LANCZOS)

d0 = os.path.join(os.path.dirname(__file__), "..", "Shard", "Assets.xcassets", "AppIcon.appiconset")
for junk in ("icon-1024-light.png", "icon-1024-dark.png"):
    j = os.path.join(d0, junk)
    if os.path.exists(j): os.remove(j)
img.save(os.path.join(d0, "icon-1024.png"), "PNG")
print("wrote icon-1024.png")
