#!/usr/bin/env python3
"""Prove the page reached the screen by reading the guest's own scanout.

The guest asks the host's QEMU VNC server - reachable as 10.0.2.2 through
SLIRP - for a raw framebuffer and counts distinct colours on a 4-pixel grid.
On StarryOS and Linux alike the bare Weston desktop shows about 33 colours and
Firefox's window before the page paints 63 to 93. A remote page's count is
whatever its current markup rasterizes to - one real 4399 load painted 711 -
so the count alone is only a diagnostic for pages this repository does not
ship. Navigation and cache entries only show the document was fetched; a probe
colour drawn by a bundled page shows it was drawn.

Usage: render-check.py HOST PORT [MIN_COLOURS [PROBE_RGB [MIN_PROBE_PIXELS]]]

PROBE_RGB is a literal `R,G,B` triple. When it is given, the probe pixels
within a small tolerance of that colour are counted as well, and both gates
must pass. A page-specific colour is the stable half of the check: the colour
count of a remote page depends on what that site currently serves, while the
probe pixel count only depends on a page this repository ships.
"""

import socket
import struct
import sys


def recv_exact(sock, size):
    data = bytearray()
    while len(data) < size:
        chunk = sock.recv(size - len(data))
        if not chunk:
            raise EOFError(f"server closed after {len(data)} of {size} bytes")
        data += chunk
    return bytes(data)


def handshake(sock):
    greeting = recv_exact(sock, 12)
    if not greeting.startswith(b"RFB 003."):
        raise ValueError(f"not an RFB greeting: {greeting!r}")
    sock.sendall(b"RFB 003.008\n")
    count = recv_exact(sock, 1)[0]
    if count == 0:
        (length,) = struct.unpack(">I", recv_exact(sock, 4))
        raise ValueError(f"server refused the connection: {recv_exact(sock, length)!r}")
    if 1 not in recv_exact(sock, count):
        raise ValueError("security type None not offered")
    sock.sendall(b"\x01")
    if struct.unpack(">I", recv_exact(sock, 4))[0] != 0:
        raise ValueError("security handshake failed")
    sock.sendall(b"\x01")
    width, height = struct.unpack(">HH", recv_exact(sock, 4))
    recv_exact(sock, 16)
    (name_length,) = struct.unpack(">I", recv_exact(sock, 4))
    recv_exact(sock, name_length)
    return width, height


def raw_frame(sock, width, height):
    # 32bpp little-endian true colour stores every pixel as B, G, R, padding.
    sock.sendall(struct.pack(">BxxxBBBBHHHBBBxxx", 0, 32, 24, 0, 1, 255, 255, 255, 16, 8, 0))
    sock.sendall(struct.pack(">BxHi", 2, 1, 0))
    sock.sendall(struct.pack(">BBHHHH", 3, 0, 0, 0, width, height))
    frame = bytearray(width * height * 4)
    while True:
        kind = recv_exact(sock, 1)[0]
        if kind == 0:
            recv_exact(sock, 1)
            (rects,) = struct.unpack(">H", recv_exact(sock, 2))
            for _ in range(rects):
                x, y, w, h, encoding = struct.unpack(">HHHHi", recv_exact(sock, 12))
                if encoding != 0:
                    raise ValueError(f"server answered with encoding {encoding}, not raw")
                data = recv_exact(sock, w * h * 4)
                for row in range(h):
                    start = ((y + row) * width + x) * 4
                    frame[start:start + w * 4] = data[row * w * 4:(row + 1) * w * 4]
            return frame
        if kind == 1:
            recv_exact(sock, 1)
            _, entries = struct.unpack(">HH", recv_exact(sock, 4))
            recv_exact(sock, entries * 6)
        elif kind == 3:
            recv_exact(sock, 3)
            (length,) = struct.unpack(">I", recv_exact(sock, 4))
            recv_exact(sock, length)
        elif kind != 2:
            raise ValueError(f"unexpected server message type {kind}")


def parse_probe(text):
    parts = [int(part) for part in text.split(",")]
    if len(parts) != 3 or any(part < 0 or part > 255 for part in parts):
        raise ValueError(f"probe colour must be R,G,B in 0..255, not {text!r}")
    return tuple(parts)


def count_probe_pixels(frame, probe, tolerance=4):
    """Counts pixels within `tolerance` of the probe colour.

    The frame is 32bpp little-endian true colour, so every pixel is stored as
    B, G, R, padding. The tolerance absorbs a round trip through the VNC server
    without letting an unrelated window colour be mistaken for the probe.
    """
    red, green, blue = probe
    count = 0
    for offset in range(0, len(frame), 4):
        if (
            abs(frame[offset + 2] - red) <= tolerance
            and abs(frame[offset + 1] - green) <= tolerance
            and abs(frame[offset] - blue) <= tolerance
        ):
            count += 1
    return count


def main():
    host, port = sys.argv[1], int(sys.argv[2])
    minimum = int(sys.argv[3]) if len(sys.argv) > 3 else 1000
    probe = parse_probe(sys.argv[4]) if len(sys.argv) > 4 else None
    minimum_probe = int(sys.argv[5]) if len(sys.argv) > 5 else 0
    with socket.create_connection((host, port), timeout=60) as sock:
        width, height = handshake(sock)
        frame = raw_frame(sock, width, height)
    colours = {bytes(frame[i:i + 3]) for i in range(0, len(frame), 16)}
    verdict = (
        f"WEB_BROWSER_RENDER scanout={width}x{height} colours={len(colours)} minimum={minimum}"
    )
    passed = len(colours) >= minimum
    if probe is not None:
        pixels = count_probe_pixels(frame, probe)
        verdict += (
            f" probe={probe[0]},{probe[1]},{probe[2]} probe_pixels={pixels}"
            f" minimum_probe={minimum_probe}"
        )
        passed = passed and pixels >= minimum_probe
    print(verdict, flush=True)
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
