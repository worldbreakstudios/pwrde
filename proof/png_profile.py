#!/usr/bin/env python3
"""Row/column brightness profiler for pwrde bus screenshots (stdlib only).

Usage: png_profile.py <png> [x0] [x1] [y0] [y1]
Prints, for each row in the y range, the count of "bright" pixels (luminance
>= 140) inside the x range, so header rows can be compared numerically.
"""
import struct
import sys
import zlib


def load(path):
    d = open(path, "rb").read()
    i, idat, w, h, bd, ct = 8, b"", None, None, None, None
    while i < len(d):
        ln = struct.unpack(">I", d[i : i + 4])[0]
        typ = d[i + 4 : i + 8]
        data = d[i + 8 : i + 8 + ln]
        i += 12 + ln
        if typ == b"IHDR":
            w, h, bd, ct = struct.unpack(">IIBB", data[:10])
        elif typ == b"IDAT":
            idat += data
        elif typ == b"IEND":
            break
    raw = zlib.decompress(idat)
    ch = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[ct]
    bpp, stride = ch * bd // 8, w * ch
    out, prev, pos = bytearray(), bytearray(stride), 0
    for _ in range(h):
        f = raw[pos]
        pos += 1
        line = bytearray(raw[pos : pos + stride])
        pos += stride
        for x in range(stride):
            a = line[x - bpp] if x >= bpp else 0
            b = prev[x]
            c = prev[x - bpp] if x >= bpp else 0
            if f == 1:
                line[x] = (line[x] + a) & 255
            elif f == 2:
                line[x] = (line[x] + b) & 255
            elif f == 3:
                line[x] = (line[x] + (a + b) // 2) & 255
            elif f == 4:
                p_ = a + b - c
                pa, pb, pc = abs(p_ - a), abs(p_ - b), abs(p_ - c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[x] = (line[x] + pr) & 255
        out += line
        prev = line
    return w, h, ch, bytes(out)


def lum(px, ch, x, y):
    o = (y * 0 + 0)  # placeholder to keep linters quiet
    return px


def main():
    path = sys.argv[1]
    x0, x1, y0, y1 = (int(v) for v in sys.argv[2:6]) if len(sys.argv) > 5 else (0, 0, 0, 0)
    w, h, ch, px = load(path)
    x0 = x0 or 0
    x1 = x1 or w
    y0 = y0 or 0
    y1 = y1 or h
    print(f"{path}: {w}x{h} ch={ch} window=[{x0},{x1})x[{y0},{y1})")
    for y in range(y0, min(y1, h)):
        n = 0
        for x in range(x0, min(x1, w)):
            o = (y * w + x) * ch
            r, g, b = px[o], px[o + 1], px[o + 2]
            if (0.299 * r + 0.587 * g + 0.114 * b) >= 140:
                n += 1
        if n:
            print(f"y={y:4d} bright={n}")


if __name__ == "__main__":
    main()