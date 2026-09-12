#!/usr/bin/env python3
# Generates a font-free banner PNG (colored bars + a marker cross and blocks)
# so the remote VNC view shows unmistakable, deterministic content regardless
# of X font availability. No external dependencies.
import struct, zlib, sys

W, H = 1024, 768
out = sys.argv[1] if len(sys.argv) > 1 else "banner.png"

img = bytearray(W * H * 3)

def put(x, y, r, g, b):
    if 0 <= x < W and 0 <= y < H:
        o = (y * W + x) * 3
        img[o], img[o + 1], img[o + 2] = r, g, b

def rect(x0, y0, x1, y1, c):
    for y in range(max(0, y0), min(H, y1)):
        for x in range(max(0, x0), min(W, x1)):
            put(x, y, *c)

# steel-blue background
rect(0, 0, W, H, (32, 68, 108))
# horizontal color bars (SMPTE-ish) across the top third
bars = [(230, 60, 60), (230, 200, 60), (60, 200, 90),
        (60, 160, 230), (170, 90, 210), (235, 235, 235)]
bw = W // len(bars)
for i, c in enumerate(bars):
    rect(i * bw, 40, (i + 1) * bw, 220, c)
# a centered white panel with a red diagonal cross (unique, easy to eyeball)
rect(212, 300, 812, 620, (245, 245, 245))
for t in range(600):
    x = 212 + t
    y0 = 300 + t * 320 // 600
    for d in (-2, -1, 0, 1, 2):
        put(x, y0 + d, 200, 40, 40)
        put(x, 620 - (y0 - 300) + d, 200, 40, 40)
# corner registration blocks
for (cx, cy) in ((20, 20), (W - 60, 20), (20, H - 60), (W - 60, H - 60)):
    rect(cx, cy, cx + 40, cy + 40, (255, 255, 0))


def png(w, h, rgb):
    raw = bytearray()
    for y in range(h):
        raw.append(0)
        raw += rgb[y * w * 3:(y + 1) * w * 3]

    def chunk(tag, d):
        c = tag + d
        return struct.pack(">I", len(d)) + c + struct.pack(">I", zlib.crc32(c) & 0xffffffff)

    sig = b"\x89PNG\r\n\x1a\n"
    ihdr = struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)
    return sig + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(bytes(raw), 6)) + chunk(b"IEND", b"")


import os
os.makedirs(os.path.dirname(out) or ".", exist_ok=True)
open(out, "wb").write(png(W, H, img))
print(f"banner written: {out} ({W}x{H})")
