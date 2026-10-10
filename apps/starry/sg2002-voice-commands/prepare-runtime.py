#!/usr/bin/env python3
"""Use the stronger standard fence on C906's unsupported FENCE.TSO sites."""
import pathlib
import re
import struct
import subprocess
import sys


def prepare(path):
    data = bytearray(path.read_bytes())
    if (data[:6] != b"\x7fELF\x02\x01" or len(data) < 64
            or struct.unpack_from("<H", data, 18)[0] != 243):
        raise ValueError(f"{path}: expected a little-endian RISC-V ELF64 file")
    table = struct.unpack_from("<Q", data, 32)[0]
    entry_size, count = struct.unpack_from("<HH", data, 54)
    if entry_size != 56 or not count or table + entry_size * count > len(data):
        raise ValueError(f"{path}: invalid ELF program headers")
    segments = []
    for index in range(count):
        kind, flags, offset, address, _, size, _, _ = struct.unpack_from(
            "<IIQQQQQQ", data, table + index * entry_size)
        if kind == 1 and flags & 1:  # Executable PT_LOAD, not arbitrary data.
            if offset + size > len(data):
                raise ValueError(f"{path}: executable segment exceeds the file")
            segments.append((address, size, offset))
    assembly = subprocess.check_output(
        ["llvm-objdump-18", "-d", str(path)], text=True)
    addresses = re.findall(
        r"^\s*([0-9a-f]+):\s+0f 00 30 83\s+fence\.tso\s*$", assembly, re.M)
    for value in addresses:
        address = int(value, 16)
        offsets = [offset + address - start for start, size, offset in segments
                   if start <= address and address + 4 <= start + size]
        if len(offsets) != 1 or data[offsets[0]:offsets[0] + 4] != b"\x0f\x00\x30\x83":
            raise ValueError(f"{path}: inconsistent instruction at {value}")
        # FENCE RW,RW orders every pair ordered by FENCE.TSO, plus store/load.
        # Both encodings occupy four bytes, so relocations and addresses stay put.
        data[offsets[0]:offsets[0] + 4] = b"\x0f\x00\x30\x03"
    if addresses:
        path.write_bytes(data)
        print(f"{path.name}: replaced {len(addresses)} FENCE.TSO instructions")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit("Usage: prepare-runtime.py ELF...")
    for argument in sys.argv[1:]:
        prepare(pathlib.Path(argument))
