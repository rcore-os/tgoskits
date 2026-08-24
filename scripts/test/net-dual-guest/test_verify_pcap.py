#!/usr/bin/env python3
"""Deterministic tests for the dependency-free pcap verifier."""

from __future__ import annotations

import importlib.util
import struct
import tempfile
import unittest
import zlib
from pathlib import Path
from unittest.mock import patch


MODULE_PATH = Path(__file__).with_name("verify_pcap.py")
SPEC = importlib.util.spec_from_file_location("verify_pcap", MODULE_PATH)
assert SPEC and SPEC.loader
VERIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFY)


def task2_frame(*, kind: int = 1, sequence: int = 1, body: bytes = b"probe") -> bytes:
    frame = bytearray(VERIFY.FRAME_HEADER_LEN + len(body))
    frame[:4] = VERIFY.FRAME_MAGIC
    frame[4] = 1
    frame[5] = kind
    frame[6:8] = (1 if kind in (1, 2) else 0).to_bytes(2, "big")
    frame[8:12] = (7).to_bytes(4, "big")
    frame[12:16] = sequence.to_bytes(4, "big")
    frame[20:22] = len(body).to_bytes(2, "big")
    frame[24:28] = b"\0" * 4
    frame[28:] = body
    frame[24:28] = (zlib.crc32(frame) & 0xFFFFFFFF).to_bytes(4, "big")
    return bytes(frame)


def ethernet_udp(payload: bytes, source: str = "10.0.42.1", destination: str = "10.0.42.2") -> bytes:
    import ipaddress

    src = ipaddress.IPv4Address(source).packed
    dst = ipaddress.IPv4Address(destination).packed
    udp = struct.pack("!HHHH", 4242, 4242, 8 + len(payload), 0) + payload
    ip = bytes([0x45, 0]) + struct.pack("!H", 20 + len(udp)) + b"\0\0\0\0" + bytes([64, 17]) + b"\0\0" + src + dst
    return b"\0" * 12 + b"\x08\0" + ip + udp


def write_pcap(path: Path, packets: list[bytes], link_type: int = 1) -> None:
    header = struct.pack("<IHHIIII", 0xA1B2C3D4, 2, 4, 0, 0, 65535, link_type)
    records = b"".join(struct.pack("<IIII", 0, index, len(packet), len(packet)) + packet for index, packet in enumerate(packets))
    path.write_bytes(header + records)


class VerifyPcapTests(unittest.TestCase):
    def test_two_captures_require_matching_task2_sequence_signatures(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            left = Path(directory) / "left.pcap"
            right = Path(directory) / "right.pcap"
            first = task2_frame(kind=1, sequence=1)
            second = task2_frame(kind=2, sequence=1, body=b"status")
            write_pcap(left, [ethernet_udp(first), ethernet_udp(second)])
            write_pcap(right, [ethernet_udp(first), ethernet_udp(second)])
            left_item = VERIFY.analyze(left, None)
            right_item = VERIFY.analyze(right, None)
            self.assertEqual(VERIFY.verify_pair(left_item, right_item), [])

            mismatched = task2_frame(kind=2, sequence=2, body=b"status")
            write_pcap(right, [ethernet_udp(first), ethernet_udp(mismatched)])
            right_item = VERIFY.analyze(right, None)
            self.assertTrue(VERIFY.verify_pair(left_item, right_item))

    def test_single_capture_is_valid_input_for_p1(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "p1.pcap"
            write_pcap(path, [ethernet_udp(task2_frame())])
            argv = [
                "verify_pcap.py",
                str(path),
                "--tag",
                "",
                "--src",
                "10.0.42.1",
                "--dst",
                "10.0.42.2",
                "--port",
                "4242",
                "--min-udp",
                "1",
                "--require-task2",
            ]
            with patch("sys.argv", argv):
                self.assertEqual(VERIFY.main(), 0)

    def test_empty_capture_fails(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            paths = [Path(directory) / name for name in ("a.pcap", "b.pcap")]
            for path in paths:
                write_pcap(path, [])
            args = type("Args", (), {"min_udp": 1, "tag": b"probe", "src": None, "dst": None, "port": None, "require_task2": False, "min_ack_rate": None})()
            failures = VERIFY.verify(VERIFY.analyze(paths[0], b"probe"), args)
            self.assertTrue(failures)

    def test_valid_task2_capture_passes_and_bad_crc_is_not_task2(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "valid.pcap"
            frame = task2_frame()
            write_pcap(path, [ethernet_udp(frame)])
            item = VERIFY.analyze(path, None)
            args = type("Args", (), {"min_udp": 1, "tag": None, "src": None, "dst": None, "port": 4242, "require_task2": True, "min_ack_rate": None})()
            self.assertEqual(VERIFY.verify(item, args), [])

            corrupted = bytearray(frame)
            corrupted[-1] ^= 0xFF
            bad_path = Path(directory) / "bad.pcap"
            write_pcap(bad_path, [ethernet_udp(bytes(corrupted))])
            bad_item = VERIFY.analyze(bad_path, None)
            self.assertEqual(bad_item["task2_kinds"], {})
            self.assertTrue(VERIFY.verify(bad_item, args))

    def test_legacy_probe_does_not_satisfy_require_task2(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "legacy.pcap"
            write_pcap(path, [ethernet_udp(b"probe legacy")])
            item = VERIFY.analyze(path, b"probe")
            args = type("Args", (), {"min_udp": 1, "tag": b"probe", "src": None, "dst": None, "port": 4242, "require_task2": True, "min_ack_rate": None})()
            self.assertTrue(VERIFY.verify(item, args))


if __name__ == "__main__":
    unittest.main()
