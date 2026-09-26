#!/usr/bin/env python3
"""Verify the remote desktop through its own VNC server.

The check speaks RFB 3.8 to Xvnc on the guest loopback, reads a full raw
framebuffer update and looks for the banner's yellow corner blocks, so it fails
when the server cannot be reached, the handshake does not complete, or the
framebuffer it serves does not hold the desktop that was drawn.
"""

import socket
import struct
import sys

HOST, PORT = "127.0.0.1", 5900
# Centres of the banner's 40x40 yellow blocks; negative values count from the far edge.
CORNERS = ((40, 40), (-40, 40), (40, -40), (-40, -40))


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
    offered = recv_exact(sock, count)
    if 1 not in offered:
        raise ValueError(f"security type None not offered: {list(offered)}")
    sock.sendall(b"\x01")
    if struct.unpack(">I", recv_exact(sock, 4))[0] != 0:
        raise ValueError("security handshake failed")
    sock.sendall(b"\x01")
    width, height = struct.unpack(">HH", recv_exact(sock, 4))
    recv_exact(sock, 16)
    (name_length,) = struct.unpack(">I", recv_exact(sock, 4))
    name = recv_exact(sock, name_length).decode(errors="replace")
    return greeting.decode().strip(), width, height, name


def request_raw_frame(sock, width, height):
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


def pixel(frame, width, height, x, y):
    offset = ((y % height) * width + (x % width)) * 4
    blue, green, red = frame[offset:offset + 3]
    return red, green, blue


def main():
    with socket.create_connection((HOST, PORT), timeout=60) as sock:
        greeting, width, height, name = handshake(sock)
        print(f"REMOTE_DESKTOP_RFB greeting={greeting} size={width}x{height} desktop={name}", flush=True)

        frame = request_raw_frame(sock, width, height)
        samples = [pixel(frame, width, height, x, y) for x, y in CORNERS]
        yellow = sum(1 for rgb in samples if rgb == (255, 255, 0))
        print(f"REMOTE_DESKTOP_RFB banner corners yellow={yellow}/4 samples={samples}", flush=True)
        if yellow != len(CORNERS):
            print("REMOTE_DESKTOP_RFB the framebuffer served over VNC does not hold the banner", file=sys.stderr)
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
