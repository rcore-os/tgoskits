#!/usr/bin/env python3
"""Validate task-2 topology invariants before starting a Guest."""

from __future__ import annotations

import argparse
import ipaddress
import re
import sys
from pathlib import Path


MAC_RE = re.compile(r"^[0-9a-f]{2}(:[0-9a-f]{2}){5}$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
HEX_RE = re.compile(r"^0x[0-9a-f]+$")


def parse_hex(value: object, field: str, failures: list[str]) -> int | None:
    """Parse a canonical hexadecimal address from the evidence manifest."""
    if not isinstance(value, str) or not HEX_RE.fullmatch(value):
        failures.append(f"{field} must be a lowercase hexadecimal value")
        return None
    return int(value, 16)


def read_manifest(path: Path) -> dict[str, dict[str, object]]:
    """Parse the small manifest subset without adding a TOML dependency."""
    sections: dict[str, dict[str, object]] = {}
    section: dict[str, object] | None = None
    for line_number, raw in enumerate(path.read_text().splitlines(), 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if line.startswith("[") and line.endswith("]"):
            name = line[1:-1]
            section = sections.setdefault(name, {})
            continue
        if section is None or "=" not in line:
            raise ValueError(f"{path}:{line_number}: invalid manifest line")
        key, value = (part.strip() for part in line.split("=", 1))
        if value.startswith('"') and value.endswith('"'):
            section[key] = value[1:-1]
        elif value.isdigit():
            section[key] = int(value)
        else:
            raise ValueError(f"{path}:{line_number}: unsupported value {value!r}")
    return sections


def validate(manifest: dict[str, dict[str, object]]) -> list[str]:
    failures: list[str] = []
    for required in ("protocol", "evidence", "linux", "rtos"):
        if required not in manifest:
            failures.append(f"missing [{required}] section")
    if failures:
        return failures

    protocol = manifest["protocol"]
    if protocol.get("version") != 1:
        failures.append("protocol.version must be 1")
    if not 1 <= int(protocol.get("udp_port", 0)) <= 65535:
        failures.append("protocol.udp_port must be a valid UDP port")

    evidence = manifest["evidence"]
    if not isinstance(evidence.get("host_dtb"), str) or not str(evidence["host_dtb"]).startswith("tmp/"):
        failures.append("evidence.host_dtb must stay under tmp/")
    if not isinstance(evidence.get("host_dtb_sha256"), str) or not SHA256_RE.fullmatch(
        str(evidence["host_dtb_sha256"])
    ):
        failures.append("evidence.host_dtb_sha256 must be a lowercase SHA-256")

    guests = [manifest["linux"], manifest["rtos"]]
    for name, guest in zip(("linux", "rtos"), guests):
        if not isinstance(guest.get("vm_id"), int) or int(guest["vm_id"]) <= 0:
            failures.append(f"{name}.vm_id must be a positive integer")
        mac = guest.get("mac")
        if not isinstance(mac, str) or not MAC_RE.fullmatch(mac):
            failures.append(f"{name}.mac must be canonical lowercase MAC")
        try:
            ipaddress.IPv4Address(str(guest.get("ip")))
        except ipaddress.AddressValueError:
            failures.append(f"{name}.ip must be IPv4")
        transport = guest.get("transport")
        if transport not in ("virtio-mmio", "virtio-net-pci"):
            failures.append(f"{name}.transport must be virtio-mmio or virtio-net-pci")
        if transport == "virtio-net-pci":
            slot = str(guest.get("pci_slot", ""))
            if not re.fullmatch(r"0x[0-9a-f]+", slot):
                failures.append(f"{name}.pci_slot must be a hexadecimal slot")
        elif transport == "virtio-mmio":
            path = guest.get("fdt_path")
            if not isinstance(path, str) or not re.fullmatch(r"/virtio_mmio@[0-9a-f]+", path):
                failures.append(f"{name}.fdt_path must identify one virtio-mmio node")
        for irq_name in ("host_hwirq", "guest_irq"):
            if not isinstance(guest.get(irq_name), int) or int(guest[irq_name]) < 0:
                failures.append(f"{name}.{irq_name} must be nonnegative")
        pcap = guest.get("pcap")
        if not isinstance(pcap, str) or not pcap.startswith("tmp/"):
            failures.append(f"{name}.pcap must stay under tmp/")

        if not isinstance(guest.get("device_set"), str) or not guest.get("device_set"):
            failures.append(f"{name}.device_set must identify the final assigned device set")
        if guest.get("dma_address_space") != "gpa=hpa":
            failures.append(f"{name}.dma_address_space must state the observed identity mapping")
        dma_evidence = guest.get("dma_evidence", "guest-log")
        if dma_evidence not in ("guest-log", "identity-map"):
            failures.append(
                f"{name}.dma_evidence must be guest-log or identity-map"
            )

        configured_start = parse_hex(
            guest.get("configured_gpa_start"), f"{name}.configured_gpa_start", failures
        )
        configured_size = parse_hex(
            guest.get("configured_memory_size"), f"{name}.configured_memory_size", failures
        )
        effective_gpa = parse_hex(
            guest.get("effective_gpa_start"), f"{name}.effective_gpa_start", failures
        )
        effective_hpa = parse_hex(
            guest.get("effective_hpa_start"), f"{name}.effective_hpa_start", failures
        )
        effective_size = parse_hex(
            guest.get("effective_memory_size"), f"{name}.effective_memory_size", failures
        )
        mmio_gpa = parse_hex(guest.get("mmio_gpa"), f"{name}.mmio_gpa", failures)
        mmio_hpa = parse_hex(guest.get("mmio_hpa"), f"{name}.mmio_hpa", failures)
        mmio_size = parse_hex(guest.get("mmio_size"), f"{name}.mmio_size", failures)
        if dma_evidence == "guest-log":
            dma_start = parse_hex(
                guest.get("dma_observed_start"), f"{name}.dma_observed_start", failures
            )
            dma_end = parse_hex(
                guest.get("dma_observed_end"), f"{name}.dma_observed_end", failures
            )
        else:
            dma_start = dma_end = None
        if configured_start is not None and configured_size is not None and configured_size <= 0:
            failures.append(f"{name}.configured_memory_size must be positive")
        if effective_size is not None and effective_size <= 0:
            failures.append(f"{name}.effective_memory_size must be positive")
        if mmio_size is not None and mmio_size <= 0:
            failures.append(f"{name}.mmio_size must be positive")
        if (
            effective_gpa is not None
            and effective_hpa is not None
            and effective_gpa != effective_hpa
        ):
            failures.append(f"{name}.effective GPA/HPA must match for the identity DMA run")
        if dma_start is not None and dma_end is not None and dma_end <= dma_start:
            failures.append(f"{name}.dma_observed_end must be greater than start")
        if (
            effective_hpa is not None
            and effective_size is not None
            and dma_start is not None
            and dma_end is not None
            and not (effective_hpa <= dma_start < dma_end <= effective_hpa + effective_size)
        ):
            failures.append(f"{name}.observed DMA range must be inside effective HPA memory")
        if mmio_gpa is not None and mmio_hpa is not None and mmio_gpa != mmio_hpa:
            failures.append(f"{name}.MMIO GPA/HPA must match for the identity passthrough run")

        guest["_ranges"] = {
            "gpa": (effective_gpa, effective_size),
            "hpa": (effective_hpa, effective_size),
            "mmio_gpa": (mmio_gpa, mmio_size),
            "mmio_hpa": (mmio_hpa, mmio_size),
            "dma": (dma_start, dma_end),
        }

    for field in ("vm_id", "mac", "ip", "fdt_path", "device_set", "host_hwirq", "guest_irq"):
        if guests[0].get(field) == guests[1].get(field):
            failures.append(f"linux and rtos share {field}")

    def ranges_overlap(left: tuple[int | None, int | None], right: tuple[int | None, int | None]) -> bool:
        if None in left or None in right:
            return False
        left_start, left_size = left
        right_start, right_size = right
        assert left_start is not None and left_size is not None
        assert right_start is not None and right_size is not None
        return left_start < right_start + right_size and right_start < left_start + left_size

    for kind in ("gpa", "hpa", "mmio_gpa", "mmio_hpa"):
        if ranges_overlap(guests[0]["_ranges"][kind], guests[1]["_ranges"][kind]):
            failures.append(f"linux and rtos overlap {kind} ranges")
    for name, guest in zip(("linux", "rtos"), guests):
        dma_start, dma_end = guest["_ranges"]["dma"]
        mmio_start, mmio_size = guest["_ranges"]["mmio_hpa"]
        if None not in (dma_start, dma_end, mmio_start, mmio_size):
            assert dma_start is not None and dma_end is not None
            assert mmio_start is not None and mmio_size is not None
            if dma_start < mmio_start + mmio_size and mmio_start < dma_end:
                failures.append(f"{name}.observed DMA range overlaps its MMIO range")
    if all(guest.get("transport") == "virtio-net-pci" for guest in guests):
        if guests[0].get("pci_slot") == guests[1].get("pci_slot"):
            failures.append("linux and rtos share pci_slot")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    args = parser.parse_args()
    try:
        failures = validate(read_manifest(args.manifest))
    except (OSError, ValueError) as error:
        print(f"FAIL: {error}")
        return 1
    if failures:
        print("FAIL")
        for failure in failures:
            print(f"- {failure}")
        return 1
    print(f"PASS: {args.manifest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
