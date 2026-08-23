#!/usr/bin/env python3
"""Compile and exercise the Linux GIPC client against a deterministic peer."""

from __future__ import annotations

import pathlib
import socket
import struct
import subprocess
import tempfile
import threading


ROOT = pathlib.Path(__file__).resolve().parents[3]
CLIENT = ROOT / "apps/starry/guest-ip-link/linux-client.c"


def crc32(data: bytes) -> int:
    crc = 0xFFFF_FFFF
    for byte in data:
        crc ^= byte
        for _ in range(8):
            crc = (crc >> 1) ^ (0xEDB8_8320 if crc & 1 else 0)
    return (~crc) & 0xFFFF_FFFF


def server() -> None:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", 4242))
        listener.listen(1)
        for attempt in range(2):
            connection, _ = listener.accept()
            if attempt == 0:
                connection.close()
                continue
            with connection:
                header = connection.recv(32)
                while len(header) < 32:
                    header += connection.recv(32 - len(header))
                payload_len = struct.unpack(">H", header[10:12])[0]
                payload = b""
                while len(payload) < payload_len:
                    payload += connection.recv(payload_len - len(payload))
                sequence = struct.unpack(">I", header[12:16])[0]
                status_payload = b"\x00\x00\x00\x01\x00\x00\x00\x00"
                response = bytearray(32 + len(status_payload))
                struct.pack_into(">I", response, 0, 0x4749_5043)
                response[4] = 1
                response[5] = 3
                struct.pack_into(">H", response, 8, 32)
                struct.pack_into(">H", response, 10, len(status_payload))
                struct.pack_into(">I", response, 12, sequence)
                response[32:] = status_payload
                struct.pack_into(">I", response, 26, crc32(response))
                connection.sendall(response)


def main() -> None:
    with tempfile.TemporaryDirectory() as directory:
        binary = pathlib.Path(directory) / "gipc-linux-client"
        subprocess.run(
            ["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", "-O2", str(CLIENT), "-o", str(binary)],
            check=True,
            cwd=ROOT,
        )
        thread = threading.Thread(target=server, daemon=True)
        thread.start()
        result = subprocess.run(
            [str(binary), "127.0.0.1"],
            check=True,
            text=True,
            capture_output=True,
            cwd=ROOT,
        )
        thread.join(timeout=2)
        assert "GIPC_LINUX_STATUS seq=1" in result.stdout, result.stdout
        assert "attempts=2 timeouts=1" in result.stdout, result.stdout
        assert "GIPC_LINUX_METRIC" in result.stdout, result.stdout
        print(result.stdout, end="")


if __name__ == "__main__":
    main()
