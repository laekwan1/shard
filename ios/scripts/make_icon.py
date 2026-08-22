#!/usr/bin/env python3
"""Generate the iOS app icon: the SHARD_MARK, modernised.

The mark stays the desktop's rounded square with a diagonal power-cut, but it is
filled with a teal→emerald gradient (not a flat block, which read as dated) on a
light ground, with soft corners and clean anti-aliased edges. The cut shows the
ground through the mark.
"""
import os
import numpy as np
from PIL import Image, ImageDraw

S = 1024
SS = 4                      # supersample for smooth edges
size = S * SS

BG = np.array([255, 255, 255], dtype=float)      # light ground, like YouTube/Chrome
TOP = np.array([0x3E, 0xE3, 0xCF], dtype=float)  # bright teal (top-left)
BOT = np.array([0x0E, 0x94, 0x88], dtype=float)  # deep teal / emerald (bottom-right)


def p(u):
    return int(round((u + 1.0) / 2.0 * size))


# --- the mark's shape as an alpha mask (rounded square minus the diagonal cut) -
mask = Image.new("L", (size, size), 0)
d = ImageDraw.Draw(mask)
x0, y0, x1, y1 = p(-0.72), p(-0.72), p(0.72), p(0.72)
radius = int(round(0.26 * (x1 - x0)))            # softer corners than before
d.rounded_rectangle([x0, y0, x1, y1], radius=radius, fill=255)
# Carve the power-cut back out (0 alpha) so the ground shows through it.
cut = int(round(0.20 * (x1 - x0)))
d.line([p(-1.5), p(-1.5), p(1.5), p(1.5)], fill=0, width=cut)

# --- a diagonal teal→emerald gradient ----------------------------------------
yy, xx = np.mgrid[0:size, 0:size]
t = ((xx + yy) / (2.0 * (size - 1)))[..., None]  # 0 at top-left → 1 at bottom-right
grad = (TOP * (1 - t) + BOT * t)                 # (size, size, 3)

# --- composite gradient over the ground using the mask -----------------------
alpha = (np.asarray(mask, dtype=float) / 255.0)[..., None]
out = grad * alpha + BG * (1 - alpha)
img = Image.fromarray(out.astype(np.uint8), "RGB").resize((S, S), Image.LANCZOS)

out_dir = os.path.join(os.path.dirname(__file__), "..",
                       "Shard", "Assets.xcassets", "AppIcon.appiconset")
os.makedirs(out_dir, exist_ok=True)
path = os.path.join(out_dir, "icon-1024.png")
img.save(path, "PNG")
print("wrote", path, img.size)
