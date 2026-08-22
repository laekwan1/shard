#!/usr/bin/env python3
"""Generate the iOS app icon — the original flat SHARD_MARK, with the ground
following the system theme.

Two images: a light-ground one and a dark-ground one. The asset catalog picks
between them by appearance (iOS 18+), so the icon's background matches light or
dark mode; older systems use the light one. The mark itself is the desktop's
flat teal rounded square with the diagonal power-cut — no gradient, no extra
rounding.
"""
import os
import numpy as np
from PIL import Image, ImageDraw

S = 1024
SS = 4
size = S * SS
ACCENT = np.array([0x2D, 0xD4, 0xBF], dtype=float)   # --accent #2dd4bf
LIGHT = np.array([255, 255, 255], dtype=float)
DARK = np.array([0x11, 0x13, 0x18], dtype=float)      # --surface #111318


def p(u):
    return int(round((u + 1.0) / 2.0 * size))


def mark_alpha():
    m = Image.new("L", (size, size), 0)
    d = ImageDraw.Draw(m)
    x0, y0, x1, y1 = p(-0.72), p(-0.72), p(0.72), p(0.72)
    d.rounded_rectangle([x0, y0, x1, y1], radius=int(round(0.22 / 2.0 * size)), fill=255)
    d.line([p(-1.5), p(-1.5), p(1.5), p(1.5)], fill=0, width=int(round(0.22 / 2.0 * size)))
    return (np.asarray(m, dtype=float) / 255.0)[..., None]


def render(ground, name, out_dir):
    a = mark_alpha()
    out = ACCENT * a + ground * (1 - a)
    img = Image.fromarray(out.astype(np.uint8), "RGB").resize((S, S), Image.LANCZOS)
    img.save(os.path.join(out_dir, name), "PNG")


out_dir = os.path.join(os.path.dirname(__file__), "..",
                       "Shard", "Assets.xcassets", "AppIcon.appiconset")
os.makedirs(out_dir, exist_ok=True)
render(LIGHT, "icon-1024-light.png", out_dir)
render(DARK, "icon-1024-dark.png", out_dir)
# The old single icon is superseded by the two appearance-specific ones.
old = os.path.join(out_dir, "icon-1024.png")
if os.path.exists(old):
    os.remove(old)
print("wrote light + dark icons to", out_dir)
