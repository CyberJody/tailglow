#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Convert a 24/32-bit ZPixmap XWD (xwd -root) to PNG, and optionally print
the color of some pixels.

Usage: xwd2png.py IN.xwd OUT.png [X,Y ...]
"""
import struct
import sys

from PIL import Image


def read_xwd(path):
    data = open(path, "rb").read()
    hdr = struct.unpack(">25I", data[:100])
    header_size, version, fmt, depth, width, height = hdr[0:6]
    byte_order, bpp, bytes_per_line, ncolors = hdr[7], hdr[11], hdr[12], hdr[19]
    if version != 7 or fmt != 2 or bpp not in (24, 32):
        raise SystemExit(f"unsupported xwd: version {version} format {fmt} bpp {bpp}")
    off = header_size + ncolors * 12
    raw = data[off : off + bytes_per_line * height]
    # byte_order 0 = LSBFirst: pixels are B G R (X) in memory.
    if bpp == 32:
        mode = "BGRX" if byte_order == 0 else "XRGB"
    else:
        mode = "BGR" if byte_order == 0 else "RGB"
    return Image.frombuffer("RGB", (width, height), raw, "raw", mode, bytes_per_line, 1)


def main():
    img = read_xwd(sys.argv[1])
    img.save(sys.argv[2])
    for xy in sys.argv[3:]:
        x, y = (int(v) for v in xy.split(","))
        r, g, b = img.getpixel((x, y))
        lum = (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255
        print(f"pixel {x},{y} = #{r:02x}{g:02x}{b:02x} ({'dark' if lum < 0.5 else 'light'})")


if __name__ == "__main__":
    main()
