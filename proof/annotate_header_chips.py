#!/usr/bin/env python3
"""Annotate a pwrde bus screenshot with the traffic-light geometry.

The native traffic lights are drawn by macOS, not by the app, so the bus
`screenshot` PNG never contains them. This script overlays what the app
*does* know — the traffic-light button band and its centre line — on the
captured frame, so a reviewer can see the header chips riding that line.

Usage:
  annotate_header_chips.py <in.png> <out.png> <scale> [--crop x0,y0,x1,y1]
"""
import struct
import sys
import zlib

sys.path.insert(0, __file__.rsplit("/", 1)[0])
from png_profile import load  # noqa: E402

# Mirrors workspace::TRAFFIC_LIGHT_ORIGIN / TRAFFIC_LIGHT_BTN_H (logical px).
TRAFFIC_LIGHT_ORIGIN = 20.0
TRAFFIC_LIGHT_BTN_H = 14.0
REGION_PAD = 10.0  # the header strips start here -- NOT where the lights are
LIGHT = (255, 96, 96)
BAND = (255, 214, 64)
LINE = (255, 64, 160)


def write_png(path, w, h, ch, px):
    raw = bytearray()
    stride = w * ch
    for y in range(h):
        raw.append(0)
        raw += px[y * stride : (y + 1) * stride]

    def chunk(typ, data):
        return (
            struct.pack(">I", len(data))
            + typ
            + data
            + struct.pack(">I", zlib.crc32(typ + data) & 0xFFFFFFFF)
        )

    body = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(bytes(raw), 6))
        + chunk(b"IEND", b"")
    )
    open(path, "wb").write(body)


def blend(px, ch, w, x, y, colour, a):
    if x < 0 or y < 0 or x >= w:
        return
    o = (y * w + x) * ch
    for k in range(3):
        px[o + k] = int(px[o + k] * (1 - a) + colour[k] * a)


def hline(px, ch, w, y, x0, x1, colour, dash=4):
    for x in range(x0, x1):
        if (x // dash) % 2 == 0:
            blend(px, ch, w, x, y, colour, 0.95)


def vline(px, ch, w, x, y0, y1, colour, dash=4):
    x = int(round(x))
    for y in range(y0, y1):
        if (y // dash) % 2 == 0:
            blend(px, ch, w, x, y, colour, 0.95)


def main():
    src, dst = sys.argv[1], sys.argv[2]
    scale = float(sys.argv[3])
    crop = None
    if len(sys.argv) > 4 and sys.argv[4] == "--crop":
        crop = tuple(int(v) for v in sys.argv[5].split(","))

    w, h, ch, px = load(src)
    px = bytearray(px)

    top = round(REGION_PAD * scale)          # header strip top (card/list rect y)
    # The lights float in ABSOLUTE window coordinates, so their band is
    # measured from the window top -- not from the strip top above.
    light_y0 = round(TRAFFIC_LIGHT_ORIGIN * scale)
    light_y1 = round((TRAFFIC_LIGHT_ORIGIN + TRAFFIC_LIGHT_BTN_H) * scale)
    centre = (light_y0 + light_y1) / 2.0

    x1 = w
    if crop:
        x0, y0, x1, y1 = crop
    else:
        y0 = 0
        y1 = min(h, light_y1 + 24)

    # The traffic-light button band, its two end rails and its centre line.
    for y in range(light_y0, light_y1 + 1):
        for x in range(0, x1):
            if y in (light_y0, light_y1):
                blend(px, ch, w, x, y, BAND, 0.30)
    vline(px, ch, w, 20 * scale, light_y0, light_y1, BAND)
    vline(px, ch, w, 60 * scale, light_y0, light_y1, BAND)
    hline(px, ch, w, int(centre), 0, x1, LINE)
    # Vertical tick at the centre line's value so a text label can name it.
    for x in range(0, x1):
        blend(px, ch, w, x, int(centre), LINE, 0.95)

    x_start, y_start = (crop[0], crop[1]) if crop else (0, 0)
    x_end, y_end = x1, y1
    out = bytearray()
    stride = w * ch  # the SOURCE row width, not the crop width
    for y in range(y_start, y_end):
        out += px[y * stride + x_start * ch : y * stride + x_end * ch]
    write_png(dst, x_end - x_start, y_end - y_start, ch, out)

    print(
        f"strip_top={top}px light_band={light_y0}..{light_y1}px "
        f"centre={centre}px annotated={dst} ({x_end - x_start}x{y_end - y_start}px)"
    )


if __name__ == "__main__":
    main()