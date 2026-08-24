#!/usr/bin/env python3
"""Verify endpoint MMIO and SPI identities in a QEMU device tree blob."""

from __future__ import annotations

import argparse
import hashlib
import struct
import sys
from pathlib import Path

from validate_manifest import parse_hex, read_manifest, validate


FDT_BEGIN_NODE = 1
FDT_END_NODE = 2
FDT_PROP = 3
FDT_NOP = 4
FDT_END = 9


def parse_nodes(path: Path) -> dict[str, dict[str, bytes]]:
    """Parse the small FDT subset needed for ``reg`` and ``interrupts``."""
    blob = path.read_bytes()
    if len(blob) < 40:
        raise ValueError(f"{path}: truncated FDT header")
    magic, total_size, struct_offset, strings_offset = struct.unpack_from(">4I4x", blob)
    if magic != 0xD00DFEED or total_size > len(blob):
        raise ValueError(f"{path}: invalid FDT header")

    strings = blob[strings_offset:total_size]
    offset = struct_offset
    stack: list[str] = []
    nodes: dict[str, dict[str, bytes]] = {}
    while offset < total_size:
        token = struct.unpack_from(">I", blob, offset)[0]
        offset += 4
        if token == FDT_BEGIN_NODE:
            end = blob.index(0, offset, total_size)
            name = blob[offset:end].decode("ascii", "replace")
            offset = (end + 4) & ~3
            stack.append(name)
            node_path = "/" + "/".join(part for part in stack if part)
            nodes.setdefault(node_path, {})
        elif token == FDT_END_NODE:
            if not stack:
                raise ValueError(f"{path}: unmatched FDT_END_NODE")
            stack.pop()
        elif token == FDT_PROP:
            length, name_offset = struct.unpack_from(">2I", blob, offset)
            offset += 8
            value = blob[offset : offset + length]
            offset = (offset + length + 3) & ~3
            name_end = strings.index(0, name_offset)
            name = strings[name_offset:name_end].decode("ascii", "replace")
            if stack:
                node_path = "/" + "/".join(part for part in stack if part)
                nodes[node_path][name] = value
        elif token == FDT_NOP:
            continue
        elif token == FDT_END:
            break
        else:
            raise ValueError(f"{path}: unknown FDT token {token}")
    return nodes


def cells(value: bytes) -> list[int]:
    if len(value) % 4:
        return []
    return [int.from_bytes(value[i : i + 4], "big") for i in range(0, len(value), 4)]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    parser.add_argument("dtb", type=Path)
    args = parser.parse_args()
    try:
        manifest = read_manifest(args.manifest)
        failures = validate(manifest)
        expected_hash = manifest["evidence"]["host_dtb_sha256"]
        actual_hash = hashlib.sha256(args.dtb.read_bytes()).hexdigest()
        if actual_hash != expected_hash:
            failures.append(
                f"host DTB hash mismatch: expected={expected_hash} actual={actual_hash}"
            )
        nodes = parse_nodes(args.dtb)
    except (OSError, ValueError) as error:
        print(f"FAIL: {error}")
        return 1

    for name in ("linux", "rtos"):
        guest = manifest[name]
        path = str(guest["fdt_path"])
        node = nodes.get(path)
        if node is None:
            failures.append(f"{name}: missing FDT node {path}")
            continue
        reg = cells(node.get("reg", b""))
        mmio_hpa = parse_hex(guest.get("mmio_hpa"), f"{name}.mmio_hpa", failures)
        mmio_size = parse_hex(guest.get("mmio_size"), f"{name}.mmio_size", failures)
        if len(reg) < 4:
            failures.append(f"{name}: {path} has no two-cell reg range")
        elif (mmio_hpa, mmio_size) != ((reg[0] << 32) | reg[1], (reg[2] << 32) | reg[3]):
            failures.append(
                f"{name}: FDT reg does not match manifest: "
                f"fdt={((reg[0] << 32) | reg[1], (reg[2] << 32) | reg[3])} "
                f"manifest={(mmio_hpa, mmio_size)}"
            )
        irq_cells = cells(node.get("interrupts", b""))
        host_hwirq = guest.get("host_hwirq")
        if len(irq_cells) < 2 or irq_cells[1] != host_hwirq:
            failures.append(
                f"{name}: FDT SPI does not match manifest: fdt={irq_cells[1] if len(irq_cells) >= 2 else None} "
                f"manifest={host_hwirq}"
            )

    if failures:
        print("FAIL")
        print("\n".join(f"- {failure}" for failure in failures))
        return 1
    print(f"PASS: {args.dtb} contains the two disjoint VirtIO endpoints")
    return 0


if __name__ == "__main__":
    sys.exit(main())
