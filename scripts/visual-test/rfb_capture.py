#!/usr/bin/env python3
"""RFB (VNC) framebuffer capture → PPM.

Minimal VNC 3.3 client: negotiates "no auth" security, reads the
server-advertised framebuffer geometry, sends a non-incremental
FramebufferUpdateRequest, and decodes a single RAW-encoded update into a
P6 PPM. RAW is the only encoding we use because QEMU's `-vnc :N`
serves RAW by default (no cpu/bandwidth pressure on localhost, and the
decoder is ~10 lines).

Invoked by `run_scenario.sh` after waiting for the guest's demo to
render. Exits 0 if the capture succeeds; non-zero on connection or
decode failure. Captured PPM is written to the path given on argv;
`perceptual_diff.py` compares it to the committed golden.
"""
import socket
import struct
import sys


def capture(host: str, port: int, out_path: str) -> None:
    s = socket.create_connection((host, port), timeout=15)
    # ProtocolVersion handshake — server greets with b"RFB 003.008\n" (or
    # 003.007/003.003 on older QEMUs); we always answer 003.003 which is
    # the universal subset and pins the authentication phase to the
    # terse form (one u32 security_type, no list).
    s.recv(12)
    s.sendall(b"RFB 003.003\n")
    security = struct.unpack(">I", s.recv(4))[0]
    if security == 0:
        reason_len = struct.unpack(">I", s.recv(4))[0]
        reason = s.recv(reason_len).decode(errors="replace")
        raise RuntimeError(f"VNC auth failed: {reason}")
    # ClientInit: shared=1 (don't disconnect other clients).
    s.sendall(b"\x01")
    server_init = s.recv(24)
    width, height = struct.unpack(">HH", server_init[:4])
    name_len = struct.unpack(">I", server_init[20:24])[0]
    _name = s.recv(name_len)

    # Request a full non-incremental framebuffer update. `3` is
    # FramebufferUpdateRequest, incremental=0 forces a fresh frame.
    s.sendall(b"\x03\x00" + struct.pack(">HHHH", 0, 0, width, height))
    header = s.recv(4)
    _msg_type, _pad, rect_count = struct.unpack(">BBH", header)
    frame = [[(0, 0, 0)] * width for _ in range(height)]
    for _ in range(rect_count):
        rh = s.recv(12)
        rx, ry, rw, rh2, enc = struct.unpack(">HHHHi", rh)
        if enc != 0:  # RAW only; other encodings skipped intentionally.
            continue
        needed = rw * rh2 * 4
        buf = b""
        while len(buf) < needed:
            chunk = s.recv(needed - len(buf))
            if not chunk:
                raise RuntimeError("short read on RAW rectangle")
            buf += chunk
        # QEMU serves BGRA little-endian in the default pixel format;
        # the protocol allows SetPixelFormat to renegotiate, but we
        # don't, so BGRA it is.
        for py in range(rh2):
            for px in range(rw):
                off = (py * rw + px) * 4
                b, g, r, _a = buf[off : off + 4]
                frame[ry + py][rx + px] = (r, g, b)
    with open(out_path, "wb") as f:
        f.write(f"P6\n{width} {height}\n255\n".encode())
        for row in frame:
            for r, g, b in row:
                f.write(bytes([r, g, b]))
    nonblack = sum(1 for row in frame for px in row if px != (0, 0, 0))
    uniq = len({px for row in frame for px in row})
    print(
        f"captured {width}x{height} nonblack={nonblack}/{width*height} "
        f"unique_colors={uniq} → {out_path}"
    )


def main() -> int:
    if len(sys.argv) != 4:
        print(
            "usage: rfb_capture.py <host> <port> <out.ppm>",
            file=sys.stderr,
        )
        return 2
    host, port_s, out = sys.argv[1], sys.argv[2], sys.argv[3]
    capture(host, int(port_s), out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
