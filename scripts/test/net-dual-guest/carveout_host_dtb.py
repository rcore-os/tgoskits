#!/usr/bin/env python3
"""Add the Task-2 guest RAM carveouts to a QEMU host DTB.

QEMU's dumpdtb contains a valid reservation map, but normally only has its
terminating zero entry.  This tool expands that map while keeping the DTB
structure and strings blocks byte-for-byte intact.  It deliberately refuses
overlapping or malformed reservations so a missing carveout cannot silently
turn a ``MAP_RESERVED`` guest into an allocator collision.
"""

from __future__ import annotations

import argparse
import struct
from pathlib import Path

FDT_MAGIC = 0xD00D_FEED
HEADER_WORDS = 10
HEADER_SIZE = HEADER_WORDS * 4


def read_header(blob: bytes) -> tuple[int, ...]:
    if len(blob) < HEADER_SIZE:
        raise ValueError("truncated FDT header")
    header = struct.unpack_from(">10I", blob)
    if header[0] != FDT_MAGIC:
        raise ValueError(f"invalid FDT magic: 0x{header[0]:08x}")
    if header[1] > len(blob) or header[1] < HEADER_SIZE:
        raise ValueError("invalid FDT total size")
    return header


def reservations(blob: bytes, header: tuple[int, ...]) -> list[tuple[int, int]]:
    offset = header[4]
    total = header[1]
    result = []
    while offset + 16 <= total:
        address, size = struct.unpack_from(">2Q", blob, offset)
        offset += 16
        if address == 0 and size == 0:
            return result
        if size == 0:
            raise ValueError(f"zero-sized reservation at 0x{address:x}")
        result.append((address, size))
    raise ValueError("unterminated FDT reservation map")


def add_reservations(blob: bytes, requested: list[tuple[int, int]]) -> bytes:
    header = read_header(blob)
    existing = reservations(blob, header)
    all_ranges = existing + requested
    for index, (start, size) in enumerate(all_ranges):
        if size == 0:
            raise ValueError(f"zero-sized requested reservation at 0x{start:x}")
        end = start + size
        if end <= start:
            raise ValueError(f"reservation overflows address space at 0x{start:x}")
        for other_start, other_size in all_ranges[index + 1 :]:
            other_end = other_start + other_size
            if start < other_end and other_start < end:
                raise ValueError(
                    f"overlapping reservations: 0x{start:x}+0x{size:x} and "
                    f"0x{other_start:x}+0x{other_size:x}"
                )

    map_offset = header[4]
    struct_offset = header[2]
    if map_offset + 16 > struct_offset:
        raise ValueError("FDT reservation map overlaps structure block")

    # Keep the original map order, then append the explicitly requested
    # ranges.  The original terminator is replaced by the new entries and a
    # fresh terminator.  Shift the blocks as one unit; this also preserves any
    # padding QEMU placed after the strings block.
    new_map = b"".join(struct.pack(">2Q", start, size) for start, size in all_ranges)
    new_map += struct.pack(">2Q", 0, 0)
    old_map_size = (len(existing) + 1) * 16
    inserted = len(new_map) - old_map_size
    prefix = blob[:map_offset]
    suffix = blob[map_offset + old_map_size : header[1]]
    new_total = header[1] + inserted
    output = bytearray(prefix + new_map + suffix)
    if len(output) < new_total:
        output.extend(b"\0" * (new_total - len(output)))
    output = output[:new_total]

    updated = list(header)
    updated[1] = new_total
    updated[2] += inserted
    updated[3] += inserted
    struct.pack_into(">10I", output, 0, *updated)
    return bytes(output)


def parse_range(value: str) -> tuple[int, int]:
    try:
        start_text, size_text = value.split(":", 1)
        start = int(start_text, 0)
        size = int(size_text, 0)
    except ValueError as error:
        raise argparse.ArgumentTypeError("range must be START:SIZE") from error
    if start < 0 or size <= 0:
        raise argparse.ArgumentTypeError("range requires START >= 0 and SIZE > 0")
    return start, size


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("input", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("ranges", nargs="+", type=parse_range, metavar="START:SIZE")
    args = parser.parse_args()
    try:
        result = add_reservations(args.input.read_bytes(), args.ranges)
        # Reparse the output before publishing it.  This is a cheap structural
        # gate and catches accidental offset/header corruption in this tool.
        output_header = read_header(result)
        actual = reservations(result, output_header)
        if any(item not in actual for item in args.ranges):
            raise ValueError(f"reservation verification mismatch: {actual!r}")
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_bytes(result)
    except (OSError, ValueError) as error:
        parser.error(str(error))
    print(f"host_dtb={args.output}")
    for start, size in args.ranges:
        print(f"reserved=0x{start:x}:0x{size:x}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
