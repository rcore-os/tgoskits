#!/usr/bin/env python3
"""Verify P1/P2 Ethernet pcap evidence without third-party packages.

The verifier checks both transport-level UDP traffic and, when `T2N1` frames
are present, application sequence/ACK accounting. It never turns an empty or
malformed capture into a PASS merely because QEMU exited successfully.
"""

from __future__ import annotations

import argparse
import ipaddress
import struct
import sys
import zlib
from collections import Counter
from pathlib import Path


ETH_P_IPV4 = 0x0800
ETH_P_ARP = 0x0806
IPPROTO_UDP = 17
FRAME_MAGIC = b"T2N1"
FRAME_HEADER_LEN = 28


def read_pcap(path: Path) -> tuple[int, list[bytes]]:
    data = path.read_bytes()
    if len(data) < 24:
        raise ValueError(f"{path}: pcap header is truncated")
    magic = data[:4]
    if magic == b"\xd4\xc3\xb2\xa1":
        endian = "<"
    elif magic == b"\xa1\xb2\xc3\xd4":
        endian = ">"
    else:
        raise ValueError(f"{path}: unsupported pcap magic {magic!r}")
    _, _, _, _, _, link_type = struct.unpack(endian + "HHIIII", data[4:24])
    packets: list[bytes] = []
    offset = 24
    while offset + 16 <= len(data):
        _, _, included_len, _ = struct.unpack(endian + "IIII", data[offset : offset + 16])
        offset += 16
        if included_len > len(data) - offset:
            raise ValueError(f"{path}: truncated packet body")
        packets.append(data[offset : offset + included_len])
        offset += included_len
    if offset != len(data):
        raise ValueError(f"{path}: trailing bytes after packet records")
    return link_type, packets


def parse_udp(packet: bytes) -> tuple[str, str, int, int, bytes] | None:
    if len(packet) < 14:
        return None
    ethertype = int.from_bytes(packet[12:14], "big")
    offset = 14
    if ethertype in (0x8100, 0x88A8):
        if len(packet) < 18:
            return None
        ethertype = int.from_bytes(packet[16:18], "big")
        offset = 18
    if ethertype != ETH_P_IPV4 or len(packet) < offset + 20:
        return None
    ip = packet[offset:]
    version_ihl = ip[0]
    if version_ihl >> 4 != 4:
        return None
    ihl = (version_ihl & 0x0F) * 4
    if ihl < 20 or len(ip) < ihl + 8 or ip[9] != IPPROTO_UDP:
        return None
    src = str(ipaddress.IPv4Address(ip[12:16]))
    dst = str(ipaddress.IPv4Address(ip[16:20]))
    udp = ip[ihl:]
    source_port, destination_port, length, _ = struct.unpack("!HHHH", udp[:8])
    if length < 8 or length > len(udp):
        return None
    return src, dst, source_port, destination_port, udp[8:length]


def parse_task2_frame(payload: bytes) -> tuple[int, int, int, bytes] | None:
    if len(payload) < FRAME_HEADER_LEN or payload[:4] != FRAME_MAGIC:
        return None
    if payload[4] != 1:
        return None
    flags = int.from_bytes(payload[6:8], "big")
    if flags & ~1:
        return None
    kind = payload[5]
    if kind not in (1, 2, 3, 4, 5):
        return None
    sequence = int.from_bytes(payload[12:16], "big")
    acknowledgement = int.from_bytes(payload[16:20], "big")
    declared_length = int.from_bytes(payload[20:22], "big")
    if declared_length != len(payload) - FRAME_HEADER_LEN:
        return None
    expected_checksum = int.from_bytes(payload[24:28], "big")
    checksum_input = bytearray(payload)
    checksum_input[24:28] = b"\x00\x00\x00\x00"
    if zlib.crc32(checksum_input) & 0xFFFFFFFF != expected_checksum:
        return None
    return kind, sequence, acknowledgement, payload[FRAME_HEADER_LEN:]


def analyze(path: Path, tag: bytes | None) -> dict[str, object]:
    link_type, packets = read_pcap(path)
    stats: Counter[str] = Counter()
    pairs: Counter[tuple[str, str]] = Counter()
    ports: Counter[tuple[int, int]] = Counter()
    sequences: Counter[int] = Counter()
    acknowledgements: Counter[int] = Counter()
    task2_kinds: Counter[int] = Counter()
    task2_signature: Counter[tuple[str, str, int, int, int]] = Counter()
    tagged = 0
    for packet in packets:
        stats["packets"] += 1
        if len(packet) >= 14 and int.from_bytes(packet[12:14], "big") == ETH_P_ARP:
            stats["arp"] += 1
        parsed = parse_udp(packet)
        if parsed is None:
            continue
        stats["udp"] += 1
        src, dst, source_port, destination_port, payload = parsed
        pairs[(src, dst)] += 1
        ports[(source_port, destination_port)] += 1
        frame = parse_task2_frame(payload)
        if frame is not None:
            kind, sequence, acknowledgement, body = frame
            task2_kinds[kind] += 1
            task2_signature[(src, dst, kind, sequence, acknowledgement)] += 1
            if kind in (1, 2):
                sequences[sequence] += 1
            if kind == 4:
                acknowledgements[acknowledgement] += 1
            if tag and tag in body:
                tagged += 1
        elif tag and tag in payload:
            tagged += 1
    stats["tagged"] = tagged
    stats["task2_frames"] = sum(task2_kinds.values())
    return {
        "path": path,
        "link_type": link_type,
        "stats": stats,
        "pairs": pairs,
        "ports": ports,
        "sequences": sequences,
        "acknowledgements": acknowledgements,
        "task2_kinds": task2_kinds,
        "task2_signature": task2_signature,
    }


def verify(item: dict[str, object], args: argparse.Namespace) -> list[str]:
    failures: list[str] = []
    stats: Counter[str] = item["stats"]  # type: ignore[assignment]
    path: Path = item["path"]  # type: ignore[assignment]
    if item["link_type"] != 1:
        failures.append(f"{path}: link type {item['link_type']} is not Ethernet(1)")
    if stats["packets"] == 0:
        failures.append(f"{path}: no packets captured")
    if stats["udp"] < args.min_udp:
        failures.append(f"{path}: UDP packets={stats['udp']} < {args.min_udp}")
    if args.tag and stats["tagged"] == 0:
        failures.append(f"{path}: no payload contains tag {args.tag!r}")
    pairs: Counter[tuple[str, str]] = item["pairs"]  # type: ignore[assignment]
    if args.src and not any(src == args.src for src, _ in pairs):
        failures.append(f"{path}: source IP {args.src} not found")
    if args.dst and not any(dst == args.dst for _, dst in pairs):
        failures.append(f"{path}: destination IP {args.dst} not found")
    ports: Counter[tuple[int, int]] = item["ports"]  # type: ignore[assignment]
    if args.port and not any(args.port in pair for pair in ports):
        failures.append(f"{path}: UDP port {args.port} not found")
    task2_kinds: Counter[int] = item["task2_kinds"]  # type: ignore[assignment]
    if args.require_task2 and not task2_kinds:
        failures.append(f"{path}: no T2N1 application frames found")
    reliable = task2_kinds[1] + task2_kinds[2]
    acked = task2_kinds[4]
    if args.min_ack_rate is not None and reliable:
        if acked * 100 < reliable * args.min_ack_rate:
            failures.append(f"{path}: ACK rate {acked}/{reliable} below {args.min_ack_rate}%")
    return failures


def verify_pair(left: dict[str, object], right: dict[str, object]) -> list[str]:
    """Require two captures to contain the same directed T2N1 frame ledger."""

    left_signature: Counter[tuple[str, str, int, int, int]] = left[
        "task2_signature"
    ]  # type: ignore[assignment]
    right_signature: Counter[tuple[str, str, int, int, int]] = right[
        "task2_signature"
    ]  # type: ignore[assignment]
    if left_signature == right_signature:
        return []
    return [
        f"pcap sequence ledger mismatch: {left['path']}={dict(left_signature)} "
        f"versus {right['path']}={dict(right_signature)}"
    ]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "pcaps",
        nargs="+",
        type=Path,
        help="one capture for P1, or two independent captures for P2/P3",
    )
    parser.add_argument("--tag", default="probe")
    parser.add_argument("--port", type=int, default=4242)
    parser.add_argument("--src", default="10.0.42.15")
    parser.add_argument("--dst", default="10.0.42.2")
    parser.add_argument("--min-udp", type=int, default=1)
    parser.add_argument("--min-ack-rate", type=int)
    parser.add_argument("--require-task2", action="store_true")
    args = parser.parse_args()
    if len(args.pcaps) not in (1, 2):
        parser.error("provide exactly one pcap (P1) or two pcaps (P2/P3)")
    tag = args.tag.encode() if args.tag else None
    try:
        reports = [analyze(path, tag) for path in args.pcaps]
    except (OSError, ValueError) as error:
        print(f"FAIL: {error}")
        return 1
    failures = [failure for report in reports for failure in verify(report, args)]
    if len(reports) == 2 and args.require_task2:
        failures.extend(verify_pair(reports[0], reports[1]))
    for report in reports:
        stats: Counter[str] = report["stats"]  # type: ignore[assignment]
        print(
            f"{report['path']}: packets={stats['packets']} arp={stats['arp']} "
            f"udp={stats['udp']} tagged={stats['tagged']} "
            f"task2={stats['task2_frames']} kinds={dict(report['task2_kinds'])}"
        )
    if failures:
        print("FAIL")
        print("\n".join(f"- {failure}" for failure in failures))
        return 1
    print("PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
