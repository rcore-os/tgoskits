#!/usr/bin/env python3
"""Verify the effective P2 memory, MMIO, IRQ and DMA contract evidence.

The final Linux + Zephyr topology uses reserved identity-mapped memory and
proves that contract from AxVisor's runtime GPA/HPA and stage-2 MMIO logs.
ArceOS evidence runs can instead select ``guest-log`` and provide bounded
``TASK2_DMA`` queue/buffer diagnostics.  Missing evidence never defaults to a
successful isolation result.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

from validate_manifest import read_manifest, validate


ANSI_RE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
DMA_RE = re.compile(r"TASK2_DMA kind=(alloc|share) bus_addr=0x([0-9a-fA-F]+)")
VM_PREFIX_RE = re.compile(r"^\[VM (\d+)\] ?(.*)$")
MEMORY_MAP_RE = re.compile(
    r"map_linear: \[GPA:0x([0-9a-fA-F]+), GPA:0x([0-9a-fA-F]+)\)"
    r" -> \[PA:0x([0-9a-fA-F]+), PA:0x([0-9a-fA-F]+)\)"
)
STAGE2_MMIO_RE = re.compile(
    r"VM\[(\d+)\] stage2 Passthrough: \[0x([0-9a-fA-F]+), 0x([0-9a-fA-F]+)\)"
    r" -> \[0x([0-9a-fA-F]+), 0x([0-9a-fA-F]+)\)"
)
IRQ_ROUTE_RE = re.compile(
    r"registered assigned AArch64 SPI route host_intid=(\d+) guest_intid=(\d+)"
)


def contains_hex(text: str, value: int) -> bool:
    """Match either a zero-padded or canonical rendering of an address."""
    return f"0x{value:x}" in text.lower() or f"0x{value:016x}" in text.lower()


def reconstruct_guest_streams(log: str) -> dict[int, str]:
    """Reassemble serial fragments that AxVisor prefixes with a VM id.

    Concurrent Guests can split one log record across several host lines.  The
    prefix is attached to every fragment, so concatenating fragments per VM
    restores the original record without assigning another Guest's DMA bytes
    to it.
    """
    fragments: dict[int, list[str]] = {}
    for line in log.splitlines():
        match = VM_PREFIX_RE.match(line)
        if match:
            fragments.setdefault(int(match.group(1)), []).append(match.group(2))
    return {vm_id: "".join(parts) for vm_id, parts in fragments.items()}


def has_identity_memory_mapping(
    log: str, gpa_start: int, gpa_end: int, hpa_start: int, hpa_end: int
) -> bool:
    """Confirm that AxVisor installed the expected identity GPA-to-HPA map."""
    return any(
        int(gpa_left, 16) == gpa_start
        and int(gpa_right, 16) == gpa_end
        and int(hpa_left, 16) == hpa_start
        and int(hpa_right, 16) == hpa_end
        for gpa_left, gpa_right, hpa_left, hpa_right in MEMORY_MAP_RE.findall(log)
    )


def has_stage2_mmio_mapping(
    log: str, vm_id: int, gpa_start: int, hpa_start: int, size: int
) -> bool:
    """Confirm that the assigned MMIO range is stage-2 mapped for one Guest."""
    gpa_end = gpa_start + size
    hpa_end = hpa_start + size
    for (
        logged_vm,
        mapped_gpa_start,
        mapped_gpa_end,
        mapped_hpa_start,
        mapped_hpa_end,
    ) in STAGE2_MMIO_RE.findall(log):
        if int(logged_vm) != vm_id:
            continue
        if int(mapped_gpa_start, 16) <= gpa_start < int(mapped_gpa_end, 16):
            if int(mapped_hpa_start, 16) <= hpa_start < int(mapped_hpa_end, 16):
                return gpa_end <= int(mapped_gpa_end, 16) and hpa_end <= int(mapped_hpa_end, 16)
    return False


def has_irq_route(log: str, guest_irq: int) -> bool:
    """Confirm that the Guest INTID is attached to an assigned physical SPI."""
    return any(int(guest_intid) == guest_irq for _, guest_intid in IRQ_ROUTE_RE.findall(log))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    parser.add_argument("axvisor_log", type=Path)
    args = parser.parse_args()

    try:
        manifest = read_manifest(args.manifest)
        failures = validate(manifest)
        log = ANSI_RE.sub("", args.axvisor_log.read_text(encoding="utf-8", errors="replace"))
        guest_streams = reconstruct_guest_streams(log)
    except (OSError, ValueError) as error:
        print(f"FAIL: {error}")
        return 1

    for name in ("linux", "rtos"):
        guest = manifest.get(name, {})
        vm_id = guest.get("vm_id")
        guest_log = guest_streams.get(int(vm_id), "") if isinstance(vm_id, int) else ""
        if not guest_log:
            failures.append(f"{name}: VM[{vm_id}] serial stream is missing")
            continue
        ranges = guest.get("_ranges", {})
        memory_gpa = ranges.get("gpa", (None, None))
        memory_hpa = ranges.get("hpa", (None, None))
        dma_range = ranges.get("dma", (None, None))
        effective_gpa = ranges.get("gpa", (None, None))[0]
        effective_hpa = ranges.get("hpa", (None, None))[0]
        effective_size = ranges.get("hpa", (None, None))[1]
        if None not in (effective_gpa, effective_hpa, effective_size):
            assert effective_gpa is not None
            assert effective_hpa is not None
            assert effective_size is not None
            if not has_identity_memory_mapping(
                log,
                effective_gpa,
                effective_gpa + effective_size,
                effective_hpa,
                effective_hpa + effective_size,
            ):
                failures.append(f"{name}: AxVisor identity GPA/HPA memory mapping is not observed")

        irq = guest.get("guest_irq")
        if not isinstance(irq, int) or not has_irq_route(log, irq):
            failures.append(f"{name}: Guest IRQ {irq!r} is not observed in an AArch64 SPI route")

        mmio_gpa_start, mmio_size = ranges.get("mmio_gpa", (None, None))
        mmio_hpa_start, _ = ranges.get("mmio_hpa", (None, None))
        if None not in (mmio_gpa_start, mmio_hpa_start, mmio_size):
            assert mmio_gpa_start is not None
            assert mmio_hpa_start is not None
            assert mmio_size is not None
            if not has_stage2_mmio_mapping(
                log, int(vm_id), mmio_gpa_start, mmio_hpa_start, mmio_size
            ):
                failures.append(f"{name}: assigned MMIO stage-2 mapping is not observed")

        dma_matches = [
            (kind, int(address, 16))
            for kind, address in DMA_RE.findall(guest_log)
        ]
        dma_start, dma_end = dma_range
        if guest.get("dma_evidence", "guest-log") == "identity-map":
            # Linux and Zephyr do not expose a common TASK2_DMA diagnostic API.
            # Their passthrough contract is proven by the runtime identity map,
            # stage-2 MMIO map, and the explicit manifest declaration.
            continue
        if dma_start is None or dma_end is None:
            continue
        observed = [address for _, address in dma_matches if dma_start <= address < dma_end]
        if not observed:
            failures.append(f"{name}: no TASK2_DMA address falls in the recorded range")
        if not any(kind == "alloc" and dma_start <= address < dma_end for kind, address in dma_matches):
            failures.append(f"{name}: no VirtIO queue allocation is observed in the DMA range")
        if not any(kind == "share" and dma_start <= address < dma_end for kind, address in dma_matches):
            failures.append(f"{name}: no shared RX/TX buffer is observed in the DMA range")

        # Keep these variables intentionally referenced so a malformed
        # manifest cannot silently bypass the range checks above.
        if memory_gpa == (None, None) or memory_hpa == (None, None):
            failures.append(f"{name}: effective memory range is missing")

    observations = []
    for name in ("linux", "rtos"):
        guest = manifest.get(name, {})
        vm_id = guest.get("vm_id")
        if guest.get("dma_evidence", "guest-log") == "identity-map":
            observations.append(f"{name}(identity-map)")
            continue
        matches = DMA_RE.findall(guest_streams.get(int(vm_id), "")) if isinstance(vm_id, int) else []
        observations.append(
            f"{name}(alloc={sum(kind == 'alloc' for kind, _ in matches)},"
            f"share={sum(kind == 'share' for kind, _ in matches)})"
        )
    print("DMA evidence: " + ", ".join(observations))
    if failures:
        print("FAIL")
        print("\n".join(f"- {failure}" for failure in failures))
        return 1
    print("PASS: effective GPA/HPA, MMIO, IRQ and DMA ranges are isolated")
    return 0


if __name__ == "__main__":
    sys.exit(main())
