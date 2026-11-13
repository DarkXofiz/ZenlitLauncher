#!/usr/bin/env python3
"""Renders the Novara mark: an N whose right stem ends in a spark, on a dark squircle.

Writes every raster the project ships (Android launcher icon, hicolor sizes, window icon,
.ico/.icns, README logo) and the scalable SVG. Needs Pillow and NumPy.
Usage: python3 tools/render_icon.py
"""
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
S = 2048  # master canvas; everything is drawn here and scaled down

# Geometry in unit coordinates (shared with the SVG below).
STROKE = 0.115
LEFT = ((0.28, 0.31), (0.28, 0.74))
DIAG = ((0.28, 0.31), (0.72, 0.74))
RIGHT = ((0.72, 0.42), (0.72, 0.74))
STAR = (0.72, 0.27, 0.135)
RADIUS = 0.22

WHITE = (244, 247, 255)
TEAL = (94, 234, 212)
SKY = (56, 189, 248)
VIOLET = (124, 108, 255)
GOLD = (255, 217, 138)
BG_A = (18, 24, 56)
BG_B = (7, 10, 28)


def gradient(c0, c1, p0, p1):
    """Linear gradient from colour c0 at p0 to c1 at p1 (unit coordinates)."""
    ys, xs = np.mgrid[0:S, 0:S].astype(np.float32) / S
    dx, dy = p1[0] - p0[0], p1[1] - p0[1]
    t = ((xs - p0[0]) * dx + (ys - p0[1]) * dy) / (dx * dx + dy * dy)
    t = np.clip(t, 0, 1)[..., None]
    return (np.array(c0) * (1 - t) + np.array(c1) * t).astype(np.float32)


def stroke_mask(a, b):
    m = Image.new("L", (S, S), 0)
    d = ImageDraw.Draw(m)
    w = STROKE * S
    pa, pb = (a[0] * S, a[1] * S), (b[0] * S, b[1] * S)
    d.line([pa, pb], fill=255, width=round(w))
    for p in (pa, pb):
        d.ellipse([p[0] - w / 2, p[1] - w / 2, p[0] + w / 2, p[1] + w / 2], fill=255)
    return np.asarray(m, dtype=np.float32) / 255


def star_points(cx, cy, r, inner=0.26):
    k = r * inner
    return [
        (cx, cy - r), (cx + k, cy - k), (cx + r, cy), (cx + k, cy + k),
        (cx, cy + r), (cx - k, cy + k), (cx - r, cy), (cx - k, cy - k),
    ]


def star_mask():
    m = Image.new("L", (S, S), 0)
    cx, cy, r = STAR
    ImageDraw.Draw(m).polygon([(x * S, y * S) for x, y in star_points(cx, cy, r)], fill=255)
    return np.asarray(m, dtype=np.float32) / 255


def master():
    plate = Image.new("L", (S, S), 0)
    ImageDraw.Draw(plate).rounded_rectangle([0, 0, S - 1, S - 1], radius=RADIUS * S, fill=255)
    alpha = np.asarray(plate, dtype=np.float32) / 255
    img = gradient(BG_A, BG_B, (0, 0), (1, 1))
    layers = [
        (stroke_mask(*DIAG), gradient(TEAL, SKY, DIAG[0], DIAG[1])),
        (stroke_mask(*RIGHT), gradient(SKY, VIOLET, RIGHT[0], RIGHT[1])),
        (stroke_mask(*LEFT), np.broadcast_to(np.array(WHITE, np.float32), (S, S, 3))),
        (star_mask(), np.broadcast_to(np.array(GOLD, np.float32), (S, S, 3))),
    ]
    for mask, color in layers:
        img = img * (1 - mask[..., None]) + color * mask[..., None]
    rgba = np.dstack([img, alpha * 255]).clip(0, 255).astype(np.uint8)
    return Image.fromarray(rgba, "RGBA")


def svg(mark_only=False):
    def line(p, stroke):
        return (f'<path d="M{p[0][0]*16:g} {p[0][1]*16:g}L{p[1][0]*16:g} {p[1][1]*16:g}" '
                f'stroke="{stroke}" stroke-width="{STROKE*16:g}" stroke-linecap="round" fill="none"/>')
    cx, cy, r = STAR
    pts = " ".join(f"{x*16:.3f},{y*16:.3f}" for x, y in star_points(cx, cy, r))
    hexs = lambda c: "#%02x%02x%02x" % c
    def lg(i, c0, c1, p):
        return (f'<linearGradient id="{i}" gradientUnits="userSpaceOnUse" x1="{p[0][0]*16:g}" y1="{p[0][1]*16:g}" '
                f'x2="{p[1][0]*16:g}" y2="{p[1][1]*16:g}"><stop stop-color="{hexs(c0)}"/><stop offset="1" stop-color="{hexs(c1)}"/></linearGradient>')
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16">\n  <title>Novara</title>\n  <defs>\n'
        f'    {lg("bg", BG_A, BG_B, ((0, 0), (1, 1)))}\n'
        f'    {lg("d", TEAL, SKY, DIAG)}\n    {lg("r", SKY, VIOLET, RIGHT)}\n'
        f'    <clipPath id="tile"><rect width="16" height="16" rx="{RADIUS*16:g}"/></clipPath>\n  </defs>\n'
        '  <g clip-path="url(#tile)">\n'
        + ('' if mark_only else '    <rect width="16" height="16" fill="url(#bg)"/>\n') +
        f'    {line(DIAG, "url(#d)")}\n    {line(RIGHT, "url(#r)")}\n    {line(LEFT, hexs(WHITE))}\n'
        f'    <polygon points="{pts}" fill="{hexs(GOLD)}"/>\n  </g>\n</svg>\n'
    )


def write(path, image, size=None):
    path.parent.mkdir(parents=True, exist_ok=True)
    if size:
        image = image.resize((size, size), Image.LANCZOS)
    image.save(path)


def main():
    m = master()
    icons = ROOT / "packaging/icons"
    for size in (16, 24, 32, 48, 64, 128, 256, 512):
        write(icons / f"hicolor/{size}x{size}/apps/horizon.png", m, size)
    (icons / "hicolor/scalable/apps/horizon.svg").write_text(svg(), encoding="utf-8")
    sizes = [(s, s) for s in (16, 24, 32, 48, 64, 128, 256)]
    m.resize((256, 256), Image.LANCZOS).save(icons / "horizon.ico", sizes=sizes)
    m.resize((1024, 1024), Image.LANCZOS).save(icons / "horizon.icns")
    write(ROOT / "packaging/android/res/drawable-nodpi/launcher_icon.png", m, 512)
    write(ROOT / "crates/horizon-theme/icons/horizon-64.png", m, 64)
    write(ROOT / "docs/media/icon-256.png", m, 256)
    brand = ROOT / "docs/media/brand"
    write(brand / "icon-1024.png", m, 1024)
    tile = Image.new("RGBA", (1024, 1024), (0, 0, 0, 0))
    tile.paste(m.resize((824, 824), Image.LANCZOS), (100, 100))
    tile.save(brand / "icon-macos-1024.png")
    for name in ("icon.svg", "avatar.svg", "icon-macos.svg"):
        (brand / name).write_text(svg(), encoding="utf-8")
    for name in ("mark.svg", "mark-dark.svg"):
        (brand / name).write_text(svg(mark_only=True), encoding="utf-8")
    print("Novara icons written")


if __name__ == "__main__":
    main()
