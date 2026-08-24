#!/usr/bin/env python3
"""Tiny host responder for the StarryOS userspace UDP smoke test."""

from __future__ import annotations

import argparse
import socket


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--bind", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=4242)
    args = parser.parse_args()
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.bind((args.bind, args.port))
        print(f"STARRY_UDP_ECHO_READY bind={args.bind}:{args.port}", flush=True)
        while True:
            payload, peer = sock.recvfrom(2048)
            print(
                f"STARRY_UDP_ECHO_RX peer={peer[0]}:{peer[1]} bytes={len(payload)} "
                f"payload={payload!r}",
                flush=True,
            )
            if payload == b"STARRY_UDP_PROBE_V1":
                sock.sendto(b"STARRY_UDP_ACK_V1", peer)
                print("STARRY_UDP_ECHO_TX payload=b'STARRY_UDP_ACK_V1'", flush=True)


if __name__ == "__main__":
    raise SystemExit(main())
