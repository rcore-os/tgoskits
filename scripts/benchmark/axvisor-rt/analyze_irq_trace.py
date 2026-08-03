#!/usr/bin/env python3
"""Pair AxVisor timer injections with StarryOS guest IRQ entries."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import math
import statistics
import sys
from collections import defaultdict
from pathlib import Path
from typing import Iterable, Sequence


HOST_HEADER = "AXVISOR_RT_HOST_TRACE"
HOST_IRQ = "AXVISOR_RT_HOST_IRQ"
HOST_PCPU = "AXVISOR_RT_HOST_PCPU"
HOST_VCPU = "AXVISOR_RT_HOST_VCPU"
HOST_NOISE = "AXVISOR_RT_HOST_NOISE"
HOST_NOISE_PCPU = "AXVISOR_RT_HOST_NOISE_PCPU"
HOST_COMPLETE = "AXVISOR_RT_HOST_TRACE_COMPLETE"
GUEST_HEADER = "AXVISOR_RT_GUEST_IRQ_TRACE"
GUEST_IRQ = "AXVISOR_RT_GUEST_IRQ"
GUEST_COMPLETE = "AXVISOR_RT_GUEST_IRQ_TRACE_COMPLETE"


class AnalysisError(ValueError):
    """Raised when direct IRQ evidence is incomplete or internally inconsistent."""


def _read_text(path: Path) -> tuple[bytes, str]:
    raw = path.read_bytes()
    payload = gzip.decompress(raw) if raw.startswith(b"\x1f\x8b") else raw
    return raw, payload.decode("utf-8")


def _parse_marker(line: str, marker: str, line_number: int) -> dict[str, str]:
    prefix = marker + " "
    if not line.startswith(prefix):
        raise AnalysisError(f"line {line_number}: expected {marker}")
    fields: dict[str, str] = {}
    for token in line[len(prefix) :].split():
        if "=" not in token:
            raise AnalysisError(f"line {line_number}: malformed {marker} token {token!r}")
        key, value = token.split("=", 1)
        if not key or not value or key in fields:
            raise AnalysisError(f"line {line_number}: malformed or duplicate {marker} field")
        fields[key] = value
    if fields.get("schema") != "1":
        raise AnalysisError(f"line {line_number}: {marker} must use schema=1")
    return fields


def _collect_markers(
    lines: Iterable[str], markers: Sequence[str]
) -> dict[str, list[dict[str, str]]]:
    records = {marker: [] for marker in markers}
    ordered_markers = sorted(markers, key=len, reverse=True)
    for line_number, raw_line in enumerate(lines, start=1):
        line = raw_line.strip()
        for marker in ordered_markers:
            if line.startswith(marker + " "):
                records[marker].append(_parse_marker(line, marker, line_number))
                break
    return records


def _single(
    records: dict[str, list[dict[str, str]]], marker: str
) -> dict[str, str]:
    matches = records[marker]
    if len(matches) != 1:
        raise AnalysisError(f"trace must contain exactly one {marker}; found {len(matches)}")
    return matches[0]


def _optional_single(
    records: dict[str, list[dict[str, str]]], marker: str
) -> dict[str, str] | None:
    matches = records[marker]
    if len(matches) > 1:
        raise AnalysisError(f"trace may contain at most one {marker}; found {len(matches)}")
    return matches[0] if matches else None


def _integer(
    fields: dict[str, str], key: str, marker: str, *, default: int | None = None
) -> int:
    value = fields.get(key)
    if value is None:
        if default is not None:
            return default
        raise AnalysisError(f"{marker} is missing {key}")
    try:
        parsed = int(value, 0)
    except ValueError as error:
        raise AnalysisError(f"{marker} {key} must be an integer") from error
    if parsed < 0:
        raise AnalysisError(f"{marker} {key} must be nonnegative")
    return parsed


def _validate_record_count(
    header: dict[str, str],
    footer: dict[str, str],
    actual: int,
    label: str,
) -> None:
    declared = _integer(header, "records", label)
    completed = _integer(footer, "records", f"{label} completion")
    if declared != actual or completed != actual:
        raise AnalysisError(
            f"{label} record count mismatch: header={declared}, footer={completed}, actual={actual}"
        )


def _validate_lossless_header(fields: dict[str, str], marker: str) -> None:
    for key in (
        "dropped",
        "incomplete",
        "failed_injections",
        "unowned_virtual_timer_irqs",
        "counter_frequency_mismatches",
    ):
        value = _integer(fields, key, marker, default=0)
        if value != 0:
            raise AnalysisError(f"{marker} reports {key}={value}; evidence is not lossless")


def _parse_host_accounting(
    records: dict[str, list[dict[str, str]]],
    header: dict[str, str],
) -> dict[str, list[dict[str, int]]]:
    if not records[HOST_PCPU] or not records[HOST_VCPU]:
        raise AnalysisError("host trace is missing independent pCPU/vCPU accounting")

    start_ticks = _integer(header, "start_ticks", HOST_HEADER)
    end_ticks = _integer(header, "end_ticks", HOST_HEADER)
    if end_ticks <= start_ticks:
        raise AnalysisError("host trace capture window must be positive")
    expected_wall = end_ticks - start_ticks

    pcpus: list[dict[str, int]] = []
    seen_pcpus: set[int] = set()
    for fields in records[HOST_PCPU]:
        item = {
            key: _integer(fields, key, HOST_PCPU)
            for key in ("pcpu", "wall_ticks", "running_ticks", "idle_ticks")
        }
        if item["pcpu"] in seen_pcpus:
            raise AnalysisError(f"duplicate host pCPU accounting for CPU {item['pcpu']}")
        seen_pcpus.add(item["pcpu"])
        if item["wall_ticks"] != expected_wall:
            raise AnalysisError("host pCPU wall time does not match the capture window")
        if item["running_ticks"] + item["idle_ticks"] != item["wall_ticks"]:
            raise AnalysisError("host pCPU running+idle ticks do not equal wall ticks")
        pcpus.append(item)

    vm_id = _integer(header, "vm", HOST_HEADER)
    vcpus: list[dict[str, int]] = []
    seen_vcpus: set[tuple[int, int]] = set()
    vcpu_fields = (
        "vm",
        "vcpu",
        "run_count",
        "run_ticks",
        "max_run_ticks",
        "wait_count",
        "wait_ticks",
        "max_wait_ticks",
        "pcpu_mask",
        "migrations",
    )
    for fields in records[HOST_VCPU]:
        item = {key: _integer(fields, key, HOST_VCPU) for key in vcpu_fields}
        identity = (item["vm"], item["vcpu"])
        if identity in seen_vcpus:
            raise AnalysisError(f"duplicate host vCPU accounting for {identity}")
        seen_vcpus.add(identity)
        if item["vm"] != vm_id:
            raise AnalysisError("host vCPU accounting belongs to the wrong VM")
        if item["run_count"] == 0 or item["pcpu_mask"] == 0:
            raise AnalysisError("host vCPU accounting must report execution and a pCPU mask")
        if item["max_run_ticks"] > item["run_ticks"]:
            raise AnalysisError("host vCPU max run interval exceeds total run time")
        if item["max_wait_ticks"] > item["wait_ticks"]:
            raise AnalysisError("host vCPU max wait interval exceeds total wait time")
        vcpus.append(item)

    return {
        "pcpus": sorted(pcpus, key=lambda item: item["pcpu"]),
        "vcpus": sorted(vcpus, key=lambda item: (item["vm"], item["vcpu"])),
    }


def _parse_host_noise(
    records: dict[str, list[dict[str, str]]],
    header: dict[str, str],
) -> dict[str, object] | None:
    fields = _optional_single(records, HOST_NOISE)
    pcpu_fields = records[HOST_NOISE_PCPU]
    if fields is None:
        if pcpu_fields:
            raise AnalysisError("host-noise pCPU records have no host-noise summary")
        return None

    integer_fields = (
        "requested_pcpu",
        "affinity_mask",
        "observed_pcpu_mask",
        "max_duration_ms",
        "start_ticks",
        "end_ticks",
        "elapsed_ticks",
        "elapsed_ns",
        "loop_iterations",
    )
    noise = {key: _integer(fields, key, HOST_NOISE) for key in integer_fields}
    requested_pcpu = noise["requested_pcpu"]
    if requested_pcpu >= 128:
        raise AnalysisError("host-noise requested pCPU is outside the evidence mask")
    expected_mask = 1 << requested_pcpu
    if noise["affinity_mask"] != expected_mask:
        raise AnalysisError("host-noise affinity is not the requested singleton pCPU")
    if noise["observed_pcpu_mask"] != expected_mask:
        raise AnalysisError("host-noise execution escaped its singleton pCPU")
    if noise["max_duration_ms"] == 0 or noise["loop_iterations"] == 0:
        raise AnalysisError("host-noise duration and loop count must be positive")
    if noise["end_ticks"] <= noise["start_ticks"]:
        raise AnalysisError("host-noise execution window must be positive")
    if noise["elapsed_ticks"] != noise["end_ticks"] - noise["start_ticks"]:
        raise AnalysisError("host-noise elapsed ticks do not match its execution window")
    if fields.get("stop_reason") != "guest-complete":
        raise AnalysisError("host-noise must remain active until guest completion")
    if fields.get("intensity") != "busy-loop":
        raise AnalysisError("host-noise intensity must be busy-loop")

    trace_start = _integer(header, "start_ticks", HOST_HEADER)
    trace_end = _integer(header, "end_ticks", HOST_HEADER)
    if noise["start_ticks"] > trace_start or noise["end_ticks"] < trace_end:
        raise AnalysisError("host-noise does not cover the complete host trace window")

    per_cpu: list[dict[str, int]] = []
    seen_cpus: set[int] = set()
    for item_fields in pcpu_fields:
        item = {
            key: _integer(item_fields, key, HOST_NOISE_PCPU)
            for key in ("pcpu", "observed_wall_ticks")
        }
        if item["pcpu"] in seen_cpus:
            raise AnalysisError(f"duplicate host-noise pCPU record for CPU {item['pcpu']}")
        if item["observed_wall_ticks"] == 0:
            raise AnalysisError("host-noise observed pCPU wall time must be positive")
        seen_cpus.add(item["pcpu"])
        per_cpu.append(item)
    if seen_cpus != {requested_pcpu}:
        raise AnalysisError("host-noise pCPU time does not match requested placement")
    if sum(item["observed_wall_ticks"] for item in per_cpu) != noise["elapsed_ticks"]:
        raise AnalysisError("host-noise observed pCPU wall time does not equal elapsed ticks")

    return {
        **noise,
        "stop_reason": fields["stop_reason"],
        "intensity": fields["intensity"],
        "pcpus": per_cpu,
        "covers_host_trace": True,
    }


def _parse_irq_records(
    fields_list: Sequence[dict[str, str]], marker: str, keys: Sequence[str]
) -> list[dict[str, int]]:
    parsed = [
        {key: _integer(fields, key, marker) for key in keys}
        for fields in fields_list
    ]
    sequences = [record["sequence"] for record in parsed]
    if len(sequences) != len(set(sequences)):
        raise AnalysisError(f"{marker} contains duplicate sequence numbers")
    return parsed


def _pair_events(
    host_events: Sequence[dict[str, int]], guest_events: Sequence[dict[str, int]]
) -> tuple[list[int], list[dict[str, int]]]:
    by_host_vcpu: dict[int, list[dict[str, int]]] = defaultdict(list)
    by_guest_vcpu: dict[int, list[dict[str, int]]] = defaultdict(list)
    for event in host_events:
        if event["injected"] != 1:
            raise AnalysisError("host trace contains an unsuccessful injection record")
        by_host_vcpu[event["vcpu"]].append(event)
    for event in guest_events:
        by_guest_vcpu[event["vcpu"]].append(event)

    if set(by_host_vcpu) != set(by_guest_vcpu):
        raise AnalysisError("unpaired host injection or guest IRQ vCPU stream")

    latencies: list[int] = []
    pairs: list[dict[str, int]] = []
    for vcpu_id in sorted(by_guest_vcpu):
        hosts = sorted(by_host_vcpu[vcpu_id], key=lambda item: item["guest_counter_ticks"])
        guests = sorted(by_guest_vcpu[vcpu_id], key=lambda item: item["guest_entry_ticks"])
        host_index = 0
        aligned = False
        for guest in guests:
            eligible: list[dict[str, int]] = []
            while (
                host_index < len(hosts)
                and hosts[host_index]["guest_counter_ticks"] <= guest["guest_entry_ticks"]
            ):
                eligible.append(hosts[host_index])
                host_index += 1
            if not eligible:
                raise AnalysisError(
                    f"guest IRQ on vCPU {vcpu_id} has no preceding host injection"
                )
            if aligned and len(eligible) != 1:
                raise AnalysisError(
                    f"unpaired host injection after alignment on vCPU {vcpu_id}"
                )
            host = eligible[-1]
            aligned = True
            if host["virtual_irq"] != guest["irq"]:
                raise AnalysisError("paired host/guest events use different virtual IRQs")
            latency_ticks = guest["guest_entry_ticks"] - host["guest_counter_ticks"]
            pairs.append(
                {
                    "vcpu": vcpu_id,
                    "host_sequence": host["sequence"],
                    "guest_sequence": guest["sequence"],
                    "latency_ticks": latency_ticks,
                }
            )
            latencies.append(latency_ticks)
        # Extra injections after the final guest entry are expected while the
        # guest stops its trace and writes it to the root filesystem.

    if not latencies:
        raise AnalysisError("direct IRQ traces contain no pairable events")
    return latencies, pairs


def _nearest_rank(ordered: Sequence[int], percentile: float) -> int:
    rank = max(1, math.ceil(percentile * len(ordered)))
    return ordered[rank - 1]


def _summarize_ticks(latencies_ticks: Sequence[int], frequency_hz: int) -> dict[str, object]:
    samples_ns = sorted((ticks * 1_000_000_000) // frequency_hz for ticks in latencies_ticks)
    return {
        "unit": "ns",
        "count": len(samples_ns),
        "samples_ns": samples_ns,
        "min_ns": samples_ns[0],
        "max_ns": samples_ns[-1],
        "mean_ns": round(statistics.fmean(samples_ns), 3),
        "population_stddev_ns": round(statistics.pstdev(samples_ns), 3),
        "p50_ns": _nearest_rank(samples_ns, 0.50),
        "p90_ns": _nearest_rank(samples_ns, 0.90),
        "p99_ns": _nearest_rank(samples_ns, 0.99),
        "p999_ns": _nearest_rank(samples_ns, 0.999),
    }


def analyze_irq_traces(host_path: Path, guest_path: Path) -> dict[str, object]:
    """Validate and pair one lossless host/guest architectural-timer trace."""
    host_raw, host_text = _read_text(host_path)
    guest_raw, guest_text = _read_text(guest_path)
    host_records = _collect_markers(
        host_text.splitlines(),
        (
            HOST_HEADER,
            HOST_IRQ,
            HOST_PCPU,
            HOST_VCPU,
            HOST_NOISE,
            HOST_NOISE_PCPU,
            HOST_COMPLETE,
        ),
    )
    guest_records = _collect_markers(
        guest_text.splitlines(), (GUEST_HEADER, GUEST_IRQ, GUEST_COMPLETE)
    )

    host_header = _single(host_records, HOST_HEADER)
    guest_header = _single(guest_records, GUEST_HEADER)
    host_footer = _single(host_records, HOST_COMPLETE)
    guest_footer = _single(guest_records, GUEST_COMPLETE)
    _validate_lossless_header(host_header, HOST_HEADER)
    _validate_lossless_header(guest_header, GUEST_HEADER)
    _validate_record_count(
        host_header, host_footer, len(host_records[HOST_IRQ]), "host trace"
    )
    _validate_record_count(
        guest_header, guest_footer, len(guest_records[GUEST_IRQ]), "guest trace"
    )

    host_frequency = _integer(host_header, "counter_frequency_hz", HOST_HEADER)
    guest_frequency = _integer(guest_header, "counter_frequency_hz", GUEST_HEADER)
    if host_frequency == 0 or guest_frequency == 0:
        raise AnalysisError("architectural counter frequency must be positive")
    if host_frequency != guest_frequency:
        raise AnalysisError(
            f"host and guest counter frequencies differ: {host_frequency} != {guest_frequency}"
        )

    host_events = _parse_irq_records(
        host_records[HOST_IRQ],
        HOST_IRQ,
        (
            "sequence",
            "vm",
            "vcpu",
            "pcpu",
            "physical_irq",
            "virtual_irq",
            "host_counter_ticks",
            "guest_counter_ticks",
            "forwarding_ticks",
            "injected",
        ),
    )
    guest_events = _parse_irq_records(
        guest_records[GUEST_IRQ],
        GUEST_IRQ,
        ("sequence", "vcpu", "irq", "guest_entry_ticks", "handler_ticks"),
    )
    vm_id = _integer(host_header, "vm", HOST_HEADER)
    if any(event["vm"] != vm_id for event in host_events):
        raise AnalysisError("host IRQ trace contains records from another VM")

    host_accounting = _parse_host_accounting(host_records, host_header)
    host_noise = _parse_host_noise(host_records, host_header)
    accounting_by_vcpu = {item["vcpu"]: item for item in host_accounting["vcpus"]}
    for event in host_events:
        accounting = accounting_by_vcpu.get(event["vcpu"])
        if accounting is None:
            raise AnalysisError("host IRQ record has no matching vCPU accounting")
        if event["pcpu"] < 64 and accounting["pcpu_mask"] & (1 << event["pcpu"]) == 0:
            raise AnalysisError("host IRQ pCPU is absent from its vCPU accounting mask")

    latency_ticks, pairs = _pair_events(host_events, guest_events)
    return {
        "schema_version": 1,
        "inputs": {
            "host": {"path": str(host_path), "sha256": hashlib.sha256(host_raw).hexdigest()},
            "guest": {"path": str(guest_path), "sha256": hashlib.sha256(guest_raw).hexdigest()},
        },
        "counter_validation": {
            "frequency_hz": host_frequency,
            "domain": "guest-virtual-counter",
            "translation": "guest_counter_ticks = CNTPCT_EL0 - CNTVOFF_EL2 (mod 2^64)",
        },
        "virtual_timer_injection_to_guest_irq_ns": _summarize_ticks(
            latency_ticks, host_frequency
        ),
        "pairing": {
            "pair_count": len(pairs),
            "leading_host_events_may_be_discarded_for_initial_alignment": True,
            "trailing_host_events_after_guest_trace_stop_are_allowed": True,
            "pairs": pairs,
        },
        "host_accounting": host_accounting,
        "host_noise": host_noise,
    }


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("host", type=Path, help="AxVisor .host.log trace")
    parser.add_argument("guest", type=Path, help="StarryOS guest IRQ trace, optionally gzip")
    parser.add_argument("--output", type=Path, help="summary JSON; defaults to stdout")
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        result = analyze_irq_traces(args.host, args.guest)
    except (AnalysisError, OSError, UnicodeDecodeError, gzip.BadGzipFile) as error:
        print(f"direct IRQ trace analysis failed: {error}", file=sys.stderr)
        return 2
    rendered = json.dumps(result, indent=2, sort_keys=True, allow_nan=False) + "\n"
    if args.output is None:
        sys.stdout.write(rendered)
    else:
        args.output.write_text(rendered, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
