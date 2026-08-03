#!/usr/bin/env python3
"""Validate and summarize lossless StarryOS physical-board RT captures."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import sys
from pathlib import Path
from types import ModuleType
from typing import Sequence


def _load_base_analyzer() -> ModuleType:
    module_name = "_axvisor_rt_base_analyze"
    existing = sys.modules.get(module_name)
    if existing is not None:
        return existing
    spec = importlib.util.spec_from_file_location(
        module_name, Path(__file__).with_name("analyze.py")
    )
    if spec is None or spec.loader is None:
        raise RuntimeError("cannot load the base AxVisor RT analyzer")
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    spec.loader.exec_module(module)
    return module


def _load_irq_analyzer() -> ModuleType:
    module_name = "_axvisor_rt_irq_analyze"
    existing = sys.modules.get(module_name)
    if existing is not None:
        return existing
    spec = importlib.util.spec_from_file_location(
        module_name, Path(__file__).with_name("analyze_irq_trace.py")
    )
    if spec is None or spec.loader is None:
        raise RuntimeError("cannot load the AxVisor direct IRQ trace analyzer")
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    spec.loader.exec_module(module)
    return module


base = _load_base_analyzer()
irq_analysis = _load_irq_analyzer()
AnalysisError = base.AnalysisError
EXPECTED_METRICS = tuple(base.EXPECTED_METRICS)
RUN_START_MARKER = "AXVISOR_RT_RUN_START"
RUN_COMPLETE_MARKER = "AXVISOR_RT_RUN_COMPLETE"
GUEST_CPUS_PREFIX = "AXVISOR_RT_GUEST_CPUS"
CAPTURE_PREFIX = "AXVISOR_RT_STARRY_CAPTURE"
METRIC_COMPLETE_PREFIX = "AXVISOR_RT_METRIC_COMPLETE"
WORKLOAD_READY_PREFIX = "AXVISOR_RT_WORKLOAD_READY"
WORKLOAD_ACTIVE_PREFIX = "AXVISOR_RT_WORKLOAD_ACTIVE"
WORKLOAD_STOPPED_PREFIX = "AXVISOR_RT_WORKLOAD_STOPPED"
WORKLOAD_CLEANED_PREFIX = "AXVISOR_RT_WORKLOAD_CLEANED"
GUEST_IRQ_TRACE_FILE_PREFIX = "AXVISOR_RT_GUEST_IRQ_TRACE_FILE"

PROFILE_CONTRACTS: dict[str, dict[str, object]] = {
    "shared": {
        "dedicated_cpus": False,
        "phys_cpu_sets": ["0x2", "0x4"],
        "vm_config": (
            "scripts/benchmark/axvisor-rt/config/"
            "starry-orangepi-5-plus-smp2-shared.toml"
        ),
    },
    "partitioned": {
        "dedicated_cpus": True,
        "phys_cpu_sets": ["0x2", "0x4"],
        "vm_config": (
            "scripts/benchmark/axvisor-rt/config/"
            "starry-orangepi-5-plus-smp2-partitioned.toml"
        ),
    },
}
FILESYSTEM_STATES = (
    "clean",
    "not-recorded",
    "unclean-orphans-raw-stable-after-copy-repair",
)

METRIC_SEMANTICS: dict[str, dict[str, str]] = {
    "periodic_jitter": {
        "authority": "direct-guest-observation",
        "meaning": "absolute CLOCK_MONOTONIC wake-up jitter inside StarryOS",
    },
    "dispatch_latency": {
        "authority": "direct-guest-observation",
        "meaning": "pthread/eventfd wake-to-observation latency inside StarryOS",
    },
    "emulated_irq_response": {
        "authority": "proxy",
        "meaning": (
            "timerfd expiration-to-read latency; this is not direct AxVisor "
            "interrupt-injection-to-guest-IRQ-entry latency"
        ),
    },
    "virtual_timer_injection_to_guest_irq": {
        "authority": "direct-cross-layer-observation",
        "meaning": (
            "AxVisor hardware virtual-timer injection to StarryOS timer IRQ "
            "handler entry in the shared guest CNTVCT_EL0 counter domain"
        ),
    },
}


def marker_records(
    lines: Sequence[str], prefix: str
) -> list[tuple[int, str]]:
    marker_prefix = f"{prefix} "
    return [
        (index, line.strip())
        for index, line in enumerate(lines)
        if line.strip().startswith(marker_prefix)
    ]


def require_exact_marker(lines: Sequence[str], marker: str) -> int:
    positions = [index for index, line in enumerate(lines) if line.strip() == marker]
    if len(positions) != 1:
        raise AnalysisError(
            f"capture must contain exactly one {marker}; found {len(positions)}"
        )
    return positions[0]


def require_marker(
    lines: Sequence[str], prefix: str, expected_fields: set[str]
) -> tuple[int, dict[str, str]]:
    records = marker_records(lines, prefix)
    if len(records) != 1:
        raise AnalysisError(
            f"capture must contain exactly one {prefix}; found {len(records)}"
        )
    position, record = records[0]
    return position, base.parse_marker_record(record, prefix, expected_fields)


def parse_nonnegative_field(
    fields: dict[str, str], key: str, marker: str
) -> int:
    value = base.parse_marker_int(fields, key, marker)
    return value


def validate_workload(
    lines: Sequence[str],
    workload: str,
    stress_cpu: int,
    first_sample: int,
    last_sample: int,
) -> tuple[int, int]:
    if marker_records(lines, "AXVISOR_RT_WORKLOAD_EXTERNAL"):
        raise AnalysisError("StarryOS board capture cannot use an external workload marker")

    if workload == "idle":
        active_position, active = require_marker(
            lines, WORKLOAD_ACTIVE_PREFIX, {"schema", "kind"}
        )
        base.require_schema_one(active, WORKLOAD_ACTIVE_PREFIX)
        if active["kind"] != "idle":
            raise AnalysisError("idle capture has the wrong ACTIVE workload kind")
        for prefix in (
            WORKLOAD_READY_PREFIX,
            WORKLOAD_STOPPED_PREFIX,
            WORKLOAD_CLEANED_PREFIX,
        ):
            if marker_records(lines, prefix):
                raise AnalysisError(f"idle capture contains contradictory {prefix}")
        if active_position >= first_sample:
            raise AnalysisError("idle workload must become active before sampling")
        return active_position, last_sample

    if workload != "cpu-stress":
        raise AnalysisError(f"unsupported StarryOS board workload {workload!r}")

    ready_position, ready = require_marker(
        lines,
        WORKLOAD_READY_PREFIX,
        {"schema", "kind", "pid", "cpu"},
    )
    active_position, active = require_marker(
        lines,
        WORKLOAD_ACTIVE_PREFIX,
        {"schema", "kind", "pid", "cpu", "affinity"},
    )
    stopped_position, stopped = require_marker(
        lines,
        WORKLOAD_STOPPED_PREFIX,
        {"schema", "kind", "pid", "cpu"},
    )
    cleaned_position, cleaned = require_marker(
        lines,
        WORKLOAD_CLEANED_PREFIX,
        {"schema", "kind", "pid", "status"},
    )
    for prefix, fields in (
        (WORKLOAD_READY_PREFIX, ready),
        (WORKLOAD_ACTIVE_PREFIX, active),
        (WORKLOAD_STOPPED_PREFIX, stopped),
        (WORKLOAD_CLEANED_PREFIX, cleaned),
    ):
        base.require_schema_one(fields, prefix)
        if fields["kind"] != "cpu-stress":
            raise AnalysisError(f"{prefix} has the wrong workload kind")

    pids = {
        parse_nonnegative_field(ready, "pid", WORKLOAD_READY_PREFIX),
        parse_nonnegative_field(active, "pid", WORKLOAD_ACTIVE_PREFIX),
        parse_nonnegative_field(stopped, "pid", WORKLOAD_STOPPED_PREFIX),
        parse_nonnegative_field(cleaned, "pid", WORKLOAD_CLEANED_PREFIX),
    }
    if len(pids) != 1 or next(iter(pids)) == 0:
        raise AnalysisError("cpu-stress lifecycle PIDs must match and be positive")
    expected_cpu = str(stress_cpu)
    if ready["cpu"] != expected_cpu or stopped["cpu"] != expected_cpu:
        raise AnalysisError("cpu-stress READY/STOPPED CPU does not match the profile")
    if active["cpu"] != expected_cpu or active["affinity"] != expected_cpu:
        raise AnalysisError("cpu-stress ACTIVE CPU/affinity does not match the profile")
    if cleaned["status"] != "0":
        raise AnalysisError("cpu-stress cleanup status must be zero")
    if not (
        ready_position
        < active_position
        < first_sample
        <= last_sample
        < stopped_position
        < cleaned_position
    ):
        raise AnalysisError("cpu-stress lifecycle does not bracket the sample window")
    return ready_position, cleaned_position


def validate_metric_completion(
    lines: Sequence[str], iterations: int, sample_positions: dict[str, list[int]]
) -> list[int]:
    completions: dict[str, tuple[int, dict[str, str]]] = {}
    for position, record in marker_records(lines, METRIC_COMPLETE_PREFIX):
        fields = base.parse_marker_record(
            record,
            METRIC_COMPLETE_PREFIX,
            {"schema", "metric", "count"},
        )
        base.require_schema_one(fields, METRIC_COMPLETE_PREFIX)
        metric = fields["metric"]
        if metric not in EXPECTED_METRICS:
            raise AnalysisError(f"unsupported metric completion {metric!r}")
        if metric in completions:
            raise AnalysisError(
                f"capture must contain exactly one completion for {metric}"
            )
        if parse_nonnegative_field(fields, "count", METRIC_COMPLETE_PREFIX) != iterations:
            raise AnalysisError(f"completion count for {metric} does not match iterations")
        completions[metric] = (position, fields)

    missing = sorted(set(EXPECTED_METRICS).difference(completions))
    if missing:
        raise AnalysisError(
            "capture must contain exactly one completion for each metric; missing "
            + ", ".join(missing)
        )
    for metric in EXPECTED_METRICS:
        if completions[metric][0] <= max(sample_positions[metric]):
            raise AnalysisError(f"completion for {metric} precedes its final sample")
    return [completions[metric][0] for metric in EXPECTED_METRICS]


def analyze_starry_file(
    input_path: Path,
    *,
    profile: str,
    expected_workload: str | None = None,
    expected_iterations: int | None = None,
    evidence_path: str | None = None,
    filesystem_state: str = "not-recorded",
    host_trace_path: Path | None = None,
    guest_irq_trace_path: Path | None = None,
    host_trace_evidence_path: str | None = None,
    guest_irq_trace_evidence_path: str | None = None,
    expected_host_noise_pcpu: int | None = None,
) -> dict[str, object]:
    """Validate one extracted StarryOS raw log and return deterministic statistics."""
    if profile not in PROFILE_CONTRACTS:
        raise AnalysisError(f"unsupported StarryOS board profile {profile!r}")
    if filesystem_state not in FILESYSTEM_STATES:
        raise AnalysisError(f"unsupported snapshot filesystem state {filesystem_state!r}")
    if expected_iterations is not None and expected_iterations <= 0:
        raise AnalysisError("expected iterations must be positive")
    if (host_trace_path is None) != (guest_irq_trace_path is None):
        raise AnalysisError("host and guest direct IRQ traces must be supplied together")
    if expected_host_noise_pcpu is not None and expected_host_noise_pcpu < 0:
        raise AnalysisError("expected host-noise pCPU must be nonnegative")
    if expected_host_noise_pcpu is not None and host_trace_path is None:
        raise AnalysisError("expected host noise requires the independent host trace")

    raw_bytes = input_path.read_bytes()
    lines = raw_bytes.decode("utf-8").splitlines()
    run_start = require_exact_marker(lines, RUN_START_MARKER)
    run_complete = require_exact_marker(lines, RUN_COMPLETE_MARKER)
    if run_start >= run_complete:
        raise AnalysisError("run completion precedes run start")
    if any(
        line.strip().startswith(
            ("AXVISOR_RT_RUN_FAILED", "AXVISOR_RT_STARRY_CAPTURE_FAILED")
        )
        for line in lines
    ):
        raise AnalysisError("capture contains a failure marker")

    guest_position, guest = require_marker(
        lines, GUEST_CPUS_PREFIX, {"schema", "os", "online"}
    )
    base.require_schema_one(guest, GUEST_CPUS_PREFIX)
    if guest["os"] != "starryos":
        raise AnalysisError("physical-board capture must identify StarryOS")
    online = parse_nonnegative_field(guest, "online", GUEST_CPUS_PREFIX)
    if online != 2:
        raise AnalysisError(f"StarryOS board capture requires 2 online vCPUs; got {online}")

    capture_position, capture = require_marker(
        lines,
        CAPTURE_PREFIX,
        {
            "schema",
            "iterations",
            "warmup",
            "period_us",
            "measurement_cpu",
            "stress_cpu",
            "fifo_priority",
            "workload",
        },
    )
    base.require_schema_one(capture, CAPTURE_PREFIX)
    iterations = parse_nonnegative_field(capture, "iterations", CAPTURE_PREFIX)
    warmup = parse_nonnegative_field(capture, "warmup", CAPTURE_PREFIX)
    period_us = parse_nonnegative_field(capture, "period_us", CAPTURE_PREFIX)
    measurement_cpu = parse_nonnegative_field(
        capture, "measurement_cpu", CAPTURE_PREFIX
    )
    stress_cpu = parse_nonnegative_field(capture, "stress_cpu", CAPTURE_PREFIX)
    fifo_priority = parse_nonnegative_field(
        capture, "fifo_priority", CAPTURE_PREFIX
    )
    workload = capture["workload"]
    if iterations == 0 or period_us == 0:
        raise AnalysisError("capture iterations and period_us must be positive")
    if not 0 < fifo_priority <= 98:
        raise AnalysisError("capture FIFO priority must be in 1..98")
    if measurement_cpu >= online or stress_cpu >= online:
        raise AnalysisError("capture CPU role is outside the online vCPU set")
    if measurement_cpu == stress_cpu:
        raise AnalysisError("measurement and stress CPU roles must not overlap")
    if expected_iterations is not None and iterations != expected_iterations:
        raise AnalysisError(
            f"expected {expected_iterations} iterations; capture declares {iterations}"
        )
    if expected_workload is not None and workload != expected_workload:
        raise AnalysisError(
            f"expected workload {expected_workload}; capture declares {workload}"
        )

    samples = base.parse_samples(lines)
    samples_by_metric = {
        metric: [sample for sample in samples if sample.metric == metric]
        for metric in EXPECTED_METRICS
    }
    sample_positions: dict[str, list[int]] = {metric: [] for metric in EXPECTED_METRICS}
    for index, line in enumerate(lines, start=1):
        sample = base.parse_sample_line(line, index)
        if sample is not None:
            sample_positions[sample.metric].append(index - 1)

    expected_sequence = list(range(iterations))
    for metric, metric_samples in samples_by_metric.items():
        if len(metric_samples) != iterations:
            raise AnalysisError(
                f"metric {metric} expected {iterations} samples, got {len(metric_samples)}"
            )
        if any(sample.cpu != measurement_cpu for sample in metric_samples):
            raise AnalysisError(
                f"metric {metric} contains a sample outside measurement CPU "
                f"{measurement_cpu}"
            )
        if sorted(sample.iteration for sample in metric_samples) != expected_sequence:
            raise AnalysisError(
                f"metric {metric} iterations are not the complete range 0..{iterations - 1}"
            )

    first_sample = min(position for values in sample_positions.values() for position in values)
    last_sample = max(position for values in sample_positions.values() for position in values)
    completion_positions = validate_metric_completion(
        lines, iterations, sample_positions
    )
    workload_start, workload_end = validate_workload(
        lines, workload, stress_cpu, first_sample, last_sample
    )
    if not (
        run_start
        < guest_position
        < capture_position
        < workload_start
        < first_sample
        <= last_sample
        <= workload_end
        < run_complete
    ):
        raise AnalysisError("capture markers do not bracket the measurement window")
    if max(completion_positions) >= run_complete:
        raise AnalysisError("metric completion appears after run completion")

    summary = base.summarize_samples(samples)
    direct_irq = None
    guest_trace_markers = marker_records(lines, GUEST_IRQ_TRACE_FILE_PREFIX)
    if host_trace_path is not None and guest_irq_trace_path is not None:
        _, guest_trace_file = require_marker(
            lines,
            GUEST_IRQ_TRACE_FILE_PREFIX,
            {"schema", "path", "compression", "bytes", "sha256"},
        )
        base.require_schema_one(guest_trace_file, GUEST_IRQ_TRACE_FILE_PREFIX)
        if guest_trace_file["path"] != "/var/lib/axvisor-rt/guest-timer-trace.log.gz":
            raise AnalysisError("guest IRQ trace marker has an unexpected in-guest path")
        if guest_trace_file["compression"] != "gzip":
            raise AnalysisError("guest IRQ trace marker must declare gzip compression")
        guest_trace_bytes = guest_irq_trace_path.read_bytes()
        declared_bytes = parse_nonnegative_field(
            guest_trace_file, "bytes", GUEST_IRQ_TRACE_FILE_PREFIX
        )
        if declared_bytes != len(guest_trace_bytes):
            raise AnalysisError("guest IRQ trace byte count differs from the raw-log marker")
        declared_sha256 = guest_trace_file["sha256"]
        actual_sha256 = hashlib.sha256(guest_trace_bytes).hexdigest()
        if declared_sha256 != actual_sha256:
            raise AnalysisError("guest IRQ trace SHA-256 differs from the raw-log marker")
        try:
            direct_irq = irq_analysis.analyze_irq_traces(
                host_trace_path, guest_irq_trace_path
            )
        except irq_analysis.AnalysisError as error:
            raise AnalysisError(f"direct IRQ trace is invalid: {error}") from error
        direct_irq["inputs"]["host"]["path"] = (
            host_trace_evidence_path
            if host_trace_evidence_path is not None
            else str(host_trace_path)
        )
        direct_irq["inputs"]["guest"]["path"] = (
            guest_irq_trace_evidence_path
            if guest_irq_trace_evidence_path is not None
            else str(guest_irq_trace_path)
        )
        summary["metrics"]["virtual_timer_injection_to_guest_irq"] = direct_irq[
            "virtual_timer_injection_to_guest_irq_ns"
        ]
        host_noise = direct_irq["host_noise"]
        if expected_host_noise_pcpu is not None:
            if host_noise is None:
                raise AnalysisError("host trace is missing required host-noise evidence")
            if host_noise["requested_pcpu"] != expected_host_noise_pcpu:
                raise AnalysisError(
                    "host-noise placement differs from the expected pCPU "
                    f"{expected_host_noise_pcpu}"
                )
    elif guest_trace_markers:
        raise AnalysisError(
            "raw log declares a guest IRQ trace; provide both host and guest trace artifacts"
        )

    result = {
        "schema_version": 1,
        "capture": {
            "platform": "OrangePi-5-Plus",
            "os": "starryos",
            "profile": profile,
            "profile_authority": (
                "declared by the harvest invocation; corroborate with the archived "
                "AxVisor and VM config hashes"
            ),
            "workload": workload,
            "vcpu_count": online,
            "iterations_per_metric": iterations,
            "sample_count": len(samples),
            "warmup_iterations": warmup,
            "period_us": period_us,
            "measurement_cpu": measurement_cpu,
            "stress_cpu": stress_cpu,
            "fifo_priority": fifo_priority,
        },
        "input": {
            "path": evidence_path if evidence_path is not None else str(input_path),
            "sha256": hashlib.sha256(raw_bytes).hexdigest(),
            "line_count": len(lines),
            "snapshot_filesystem_state": filesystem_state,
        },
        "metrics": summary["metrics"],
        "metric_semantics": METRIC_SEMANTICS,
        "profile_contract": PROFILE_CONTRACTS[profile],
        "host_pcpu_accounting": {
            "status": "not-collected",
            "reason": (
                "guest raw data cannot prove AxVisor host pCPU/vCPU accounting; "
                "collect that evidence from an independent host-side trace"
            ),
        },
        "host_noise": {
            "status": "not-configured",
        },
    }
    if direct_irq is not None:
        result["direct_irq_trace"] = direct_irq
        result["host_pcpu_accounting"] = {
            "status": "collected",
            "source": "AxVisor architectural idle and vCPU runtime trace",
            **direct_irq["host_accounting"],
        }
        if direct_irq["host_noise"] is not None:
            result["host_noise"] = {
                "status": "collected",
                **direct_irq["host_noise"],
            }
    return result


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path, help="raw.log extracted from the snapshot")
    parser.add_argument("--profile", choices=sorted(PROFILE_CONTRACTS), required=True)
    parser.add_argument("--expected-workload", choices=("idle", "cpu-stress"))
    parser.add_argument("--expected-iterations", type=int)
    parser.add_argument(
        "--evidence-path",
        help="stable path to record in JSON when input is a temporary harvest file",
    )
    parser.add_argument("--host-trace", type=Path, help="AxVisor .host.log trace")
    parser.add_argument(
        "--guest-irq-trace", type=Path, help="StarryOS guest timer IRQ trace"
    )
    parser.add_argument(
        "--host-trace-evidence-path",
        help="stable host-trace path to record when harvesting through a temporary file",
    )
    parser.add_argument(
        "--guest-irq-trace-evidence-path",
        help="stable guest-trace path to record when harvesting through a temporary file",
    )
    parser.add_argument(
        "--expected-host-noise-pcpu",
        type=int,
        help="require bounded host-noise evidence on this singleton pCPU",
    )
    parser.add_argument(
        "--filesystem-state",
        choices=FILESYSTEM_STATES,
        default="not-recorded",
        help="read-only snapshot validation result from the harvest step",
    )
    parser.add_argument("--output", type=Path, help="summary JSON; defaults to stdout")
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        result = analyze_starry_file(
            args.input,
            profile=args.profile,
            expected_workload=args.expected_workload,
            expected_iterations=args.expected_iterations,
            evidence_path=args.evidence_path,
            filesystem_state=args.filesystem_state,
            host_trace_path=args.host_trace,
            guest_irq_trace_path=args.guest_irq_trace,
            host_trace_evidence_path=args.host_trace_evidence_path,
            guest_irq_trace_evidence_path=args.guest_irq_trace_evidence_path,
            expected_host_noise_pcpu=args.expected_host_noise_pcpu,
        )
    except (
        AnalysisError,
        OSError,
        UnicodeDecodeError,
        json.JSONDecodeError,
    ) as error:
        print(f"StarryOS board analysis failed: {error}", file=sys.stderr)
        return 2

    rendered = json.dumps(result, indent=2, sort_keys=True, allow_nan=False) + "\n"
    if args.output is None:
        sys.stdout.write(rendered)
    else:
        args.output.write_text(rendered, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
