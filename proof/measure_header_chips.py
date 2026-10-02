#!/usr/bin/env python3
"""Measure header-chip glyph centres against the traffic-light centre line.

The bus screenshot has no native traffic lights (macOS draws those, not the
app), but it does have the header chips. This groups the bright glyph pixels
in the header strip into column clusters and reports each cluster's vertical
centre, next to the centre line the app lays the chips out on.

The line is a WINDOW coordinate: macOS draws the native lights in absolute
window coordinates (workspace::traffic_light_origin_at returns
(TRAFFIC_LIGHT_ORIGIN, TRAFFIC_LIGHT_ORIGIN) from the window's own top
edge), so it is workspace::TRAFFIC_LIGHT_ORIGIN +
TRAFFIC_LIGHT_BTN_H/2 from the window top -- NOT from the header strip's
top edge, which sits REGION_PAD lower.

Usage: measure_header_chips.py <png> <scale> [x0] [x1] [y0] [y1]
Prints one line per glyph cluster and a verdict; exits 1 if any cluster's
centre is more than 1px off the line.
"""
import sys

sys.path.insert(0, __file__.rsplit("/", 1)[0])
from png_profile import load  # noqa: E402

TRAFFIC_LIGHT_ORIGIN = 20.0
TRAFFIC_LIGHT_BTN_H = 14.0
THRESHOLD = 140


def main():
    path = sys.argv[1]
    scale = float(sys.argv[2])
    x0, x1, y0, y1 = (int(v) for v in sys.argv[3:7]) if len(sys.argv) > 6 else (0, 0, 0, 0)

    w, h, ch, px = load(path)
    x0 = x0 or 0
    x1 = x1 or w
    y0 = y0 or 0
    y1 = y1 or h

    def lum(x, y):
        o = (y * w + x) * ch
        return 0.299 * px[o] + 0.587 * px[o + 1] + 0.114 * px[o + 2]

    # The line the app lays chips out on, from the WINDOW top (the lights
    # are drawn by macOS in absolute window coordinates).
    line = (TRAFFIC_LIGHT_ORIGIN + TRAFFIC_LIGHT_BTN_H / 2.0) * scale

    pts = [
        (x, y)
        for y in range(y0, y1)
        for x in range(x0, x1)
        if lum(x, y) >= THRESHOLD
    ]
    if not pts:
        print("no bright pixels in the window given — nothing to measure")
        return 1

    cols = sorted({x for x, _ in pts})
    clusters = []
    start = prev = None
    for x in cols:
        if start is None:
            start = prev = x
        elif x - prev <= 3:
            prev = x
        else:
            clusters.append((start, prev))
            start = prev = x
    if start is not None:
        clusters.append((start, prev))

    worst = 0.0
    print(f"expected chip centre line: y = {line:.1f}px (scale {scale})")
    for a, b in clusters:
        sub = [(x, y) for x, y in pts if a <= x <= b]
        ys = [y for _, y in sub]
        centre = (min(ys) + max(ys)) / 2.0
        delta = centre - line
        worst = max(worst, abs(delta))
        print(
            f"glyph x[{a},{b}] y[{min(ys)},{max(ys)}] "
            f"centre_y={centre:.1f} delta={delta:+.1f}px px={len(sub)}"
        )
    print(f"worst |delta| = {worst:.1f}px")
    if worst > 1.0:
        print("VERDICT: chips are NOT on the traffic-light centre line")
        return 1
    print("VERDICT: every glyph rides the traffic-light centre line (<=1px)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())