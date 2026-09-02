#!/usr/bin/env python3
"""Validate and summarize AxVisor real-time benchmark sample logs."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import re
import statistics
import sys
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Iterable, Sequence


SAMPLE_PREFIX = "AXVISOR_RT_SAMPLE "
RUN_START_MARKER = "AXVISOR_RT_RUN_START"
RUN_COMPLETE_MARKER = "AXVISOR_RT_RUN_COMPLETE"
RUN_FAILED_PREFIX = "AXVISOR_RT_RUN_FAILED"
GUEST_CPUS_PREFIX = "AXVISOR_RT_GUEST_CPUS"
WORKLOAD_ACTIVE_PREFIX = "AXVISOR_RT_WORKLOAD_ACTIVE"
WORKLOAD_CLEANED_PREFIX = "AXVISOR_RT_WORKLOAD_CLEANED"
WORKLOAD_EXTERNAL_PREFIX = "AXVISOR_RT_WORKLOAD_EXTERNAL"
WORKLOAD_READY_PREFIX = "AXVISOR_RT_WORKLOAD_READY"
WORKLOAD_STOPPED_PREFIX = "AXVISOR_RT_WORKLOAD_STOPPED"
CPU_STAT_PREFIX = "AXVISOR_RT_CPUSTAT"
CPU_STRESS_MIN_BUSY_PERCENT = 50
CPU_STAT_COUNTERS = (
    "user",
    "nice",
    "system",
    "idle",
    "iowait",
    "irq",
    "softirq",
    "steal",
)
EXPECTED_METRICS = (
    "dispatch_latency",
    "emulated_irq_response",
    "periodic_jitter",
)
METADATA_METRICS = (
    "periodic_jitter",
    "dispatch_latency",
    "emulated_irq_response",
)
METADATA_FIELDS = {
    "schema_version",
    "run_id",
    "status",
    "started_at",
    "finished_at",
    "repository",
    "host",
    "qemu",
    "guest",
    "benchmark",
    "artifacts",
}
REPOSITORY_FIELDS = {
    "commit",
    "dirty",
    "source_snapshot_sha256",
    "tracked_diff_sha256",
    "untracked_source_file_count",
    "untracked_source_manifest_sha256",
}
HOST_FIELDS = {"system", "release", "machine"}
QEMU_FIELDS = {
    "binary",
    "version",
    "acceleration",
    "machine",
    "cpu",
    "host_cpu_count",
    "exit_code",
}
GUEST_FIELDS = {
    "architecture",
    "vcpu_count",
    "profile",
    "dedicated_host_cpu_ids",
    "vm_config",
}
BENCHMARK_FIELDS = {
    "iterations",
    "warmup_iterations",
    "period_ns",
    "guest_cpu",
    "fifo_priority",
    "workload",
    "metrics",
}
ARTIFACT_NAMES = {
    "input_rootfs",
    "injected_rootfs_pre_run",
    "probe",
    "guest_runner",
    "raw_log",
    "axvisor_config",
    "vm_config",
    "qemu_config",
}
PROFILE_GUEST_CONFIG = {
    "partitioned": (
        [2, 3],
        "os/axvisor/configs/vms/qemu/aarch64/linux-smp2-dedicated.toml",
    ),
    "shared": (
        [],
        "os/axvisor/configs/vms/qemu/aarch64/linux-smp2-shared.toml",
    ),
}
WORKLOAD_PATTERN = re.compile(
    r"^(?:idle|cpu-stress|external:[A-Za-z0-9][A-Za-z0-9._-]*)$"
)
SHA256_PATTERN = re.compile(r"^[0-9a-f]{64}$")
GIT_COMMIT_PATTERN = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")
REQUIRED_FIELDS = {
    "schema",
    "metric",
    "iteration",
    "cpu",
    "target_ns",
    "observed_ns",
    "latency_ns",
}


class AnalysisError(ValueError):
    """Raised when benchmark input violates the sample contract."""


@dataclass(frozen=True)
class Sample:
    """One validated latency sample emitted by the guest probe."""

    metric: str
    iteration: int
    cpu: int
    target_ns: int
    observed_ns: int
    latency_ns: int


def parse_samples(lines: Iterable[str]) -> list[Sample]:
    """Parse and validate all benchmark samples in an iterable of log lines."""
    samples: list[Sample] = []
    identities: set[tuple[str, int, int]] = set()
    for line_number, line in enumerate(lines, start=1):
        sample = parse_sample_line(line, line_number)
        if sample is None:
            continue
        identity = (sample.metric, sample.cpu, sample.iteration)
        if identity in identities:
            raise AnalysisError(
                f"line {line_number}: duplicate metric/cpu/iteration sample {identity}"
            )
        identities.add(identity)
        samples.append(sample)
    if not samples:
        raise AnalysisError("input contains no AXVISOR_RT_SAMPLE records")
    return samples


def summarize_samples(
    samples: Sequence[Sample], *, require_all_metrics: bool = True
) -> dict[str, object]:
    """Return deterministic nearest-rank latency statistics grouped by metric."""
    grouped: dict[str, list[int]] = {}
    for sample in samples:
        grouped.setdefault(sample.metric, []).append(sample.latency_ns)

    if require_all_metrics:
        missing = sorted(set(EXPECTED_METRICS).difference(grouped))
        if missing:
            raise AnalysisError(f"input is missing required metrics: {', '.join(missing)}")

    metrics = {
        metric: summarize_latencies(grouped[metric]) for metric in sorted(grouped)
    }
    return {"schema_version": 1, "metrics": metrics}


def analyze_file(input_path: Path, metadata_path: Path | None) -> dict[str, object]:
    """Analyze a raw log and optionally attach its run metadata."""
    raw_bytes = input_path.read_bytes()
    lines = raw_bytes.decode("utf-8", errors="replace").splitlines()
    samples = parse_samples(lines)
    metadata = None
    cpu_load = None
    if metadata_path is not None:
        metadata_value = json.loads(metadata_path.read_text(encoding="utf-8"))
        metadata = validate_complete_metadata(metadata_value)
        cpu_load = validate_capture_evidence(
            lines, metadata, hashlib.sha256(raw_bytes).hexdigest()
        )
        validate_samples_against_metadata(samples, metadata)

    result = summarize_samples(samples)
    result["input"] = {
        "path": str(input_path),
        "sha256": hashlib.sha256(raw_bytes).hexdigest(),
    }
    if metadata is not None:
        result["metadata"] = metadata
        result["cpu_load"] = cpu_load
    return result


def validate_complete_metadata(value: object) -> dict[str, object]:
    """Validate the complete schema-v1 provenance object without external packages."""
    if not isinstance(value, dict):
        raise AnalysisError("metadata must be a schema_version 1 JSON object")
    require_exact_keys(value, METADATA_FIELDS, "metadata")
    if value["schema_version"] != 1 or type(value["schema_version"]) is not int:
        raise AnalysisError("metadata schema_version must be integer 1")
    require_nonempty_string(value["run_id"], "metadata run_id")
    if value["status"] != "capture_complete":
        raise AnalysisError("metadata status must be capture_complete")
    started_at = require_timestamp(value["started_at"], "metadata started_at")
    finished_at = require_timestamp(value["finished_at"], "metadata finished_at")
    if finished_at < started_at:
        raise AnalysisError("metadata finished_at must not precede started_at")

    validate_repository_metadata(require_metadata_object(value, "repository"))
    validate_host_metadata(require_metadata_object(value, "host"))
    validate_qemu_metadata(require_metadata_object(value, "qemu"))
    guest = require_metadata_object(value, "guest")
    validate_guest_metadata(guest)
    validate_benchmark_metadata(require_metadata_object(value, "benchmark"), guest)
    validate_artifact_metadata(require_metadata_object(value, "artifacts"))
    return value


def validate_repository_metadata(repository: dict[str, object]) -> None:
    require_exact_keys(repository, REPOSITORY_FIELDS, "metadata repository")
    commit = require_nonempty_string(repository["commit"], "metadata repository commit")
    if GIT_COMMIT_PATTERN.fullmatch(commit) is None:
        raise AnalysisError("metadata repository commit must be a 40- or 64-digit hex ID")
    if type(repository["dirty"]) is not bool:
        raise AnalysisError("metadata repository dirty must be boolean")
    for field in (
        "source_snapshot_sha256",
        "tracked_diff_sha256",
        "untracked_source_manifest_sha256",
    ):
        require_sha256(repository[field], f"metadata repository {field}")
    file_count = repository["untracked_source_file_count"]
    if type(file_count) is not int or file_count < 0:
        raise AnalysisError(
            "metadata repository untracked_source_file_count must be a nonnegative integer"
        )


def validate_host_metadata(host: dict[str, object]) -> None:
    require_exact_keys(host, HOST_FIELDS, "metadata host")
    for field in HOST_FIELDS:
        require_nonempty_string(host[field], f"metadata host {field}")


def validate_qemu_metadata(qemu: dict[str, object]) -> None:
    require_exact_keys(qemu, QEMU_FIELDS, "metadata qemu")
    for field in ("binary", "version", "machine", "cpu"):
        require_nonempty_string(qemu[field], f"metadata qemu {field}")
    if qemu["acceleration"] != "tcg":
        raise AnalysisError("metadata qemu acceleration must be tcg")
    if qemu["machine"] != "virt,virtualization=on,gic-version=3":
        raise AnalysisError("metadata qemu machine does not match the harness contract")
    if qemu["cpu"] != "cortex-a72":
        raise AnalysisError("metadata qemu cpu must be cortex-a72")
    if type(qemu["host_cpu_count"]) is not int or qemu["host_cpu_count"] != 4:
        raise AnalysisError("metadata qemu host_cpu_count must be integer 4")
    if type(qemu["exit_code"]) is not int or qemu["exit_code"] != 0:
        raise AnalysisError("metadata QEMU exit code must be zero")


def validate_guest_metadata(guest: dict[str, object]) -> None:
    require_exact_keys(guest, GUEST_FIELDS, "metadata guest")
    if guest["architecture"] != "aarch64":
        raise AnalysisError("metadata guest architecture must be aarch64")
    if type(guest["vcpu_count"]) is not int or guest["vcpu_count"] != 2:
        raise AnalysisError("metadata guest vcpu_count must be integer 2")
    profile = guest["profile"]
    if not isinstance(profile, str) or profile not in PROFILE_GUEST_CONFIG:
        raise AnalysisError("metadata guest profile must be partitioned or shared")
    dedicated_host_cpu_ids, vm_config = PROFILE_GUEST_CONFIG[profile]
    if guest["dedicated_host_cpu_ids"] != dedicated_host_cpu_ids:
        raise AnalysisError(
            f"metadata guest dedicated_host_cpu_ids do not match {profile} profile"
        )
    if guest["vm_config"] != vm_config:
        raise AnalysisError(f"metadata guest vm_config does not match {profile} profile")


def validate_benchmark_metadata(
    benchmark: dict[str, object], guest: dict[str, object]
) -> None:
    require_exact_keys(benchmark, BENCHMARK_FIELDS, "metadata benchmark")
    require_positive_int(benchmark["iterations"], "metadata benchmark iterations")
    require_nonnegative_int(
        benchmark["warmup_iterations"], "metadata benchmark warmup_iterations"
    )
    require_positive_int(benchmark["period_ns"], "metadata benchmark period_ns")
    guest_cpu = require_nonnegative_int(
        benchmark["guest_cpu"], "metadata benchmark guest_cpu"
    )
    if guest_cpu >= guest["vcpu_count"]:
        raise AnalysisError("metadata benchmark guest_cpu is outside the guest vCPU count")
    fifo_priority = require_nonnegative_int(
        benchmark["fifo_priority"], "metadata benchmark fifo_priority"
    )
    if fifo_priority > 98:
        raise AnalysisError("metadata benchmark fifo_priority must be at most 98")
    workload = require_nonempty_string(
        benchmark["workload"], "metadata benchmark workload"
    )
    if WORKLOAD_PATTERN.fullmatch(workload) is None:
        raise AnalysisError("metadata benchmark workload has unsupported syntax")
    if workload == "cpu-stress" and guest_cpu != 0:
        raise AnalysisError("metadata cpu-stress benchmark must run measured probes on CPU 0")
    if benchmark["metrics"] != list(METADATA_METRICS):
        raise AnalysisError("metadata benchmark metrics do not match the analyzer contract")


def validate_artifact_metadata(artifacts: dict[str, object]) -> None:
    require_exact_keys(artifacts, ARTIFACT_NAMES, "metadata artifacts")
    for name in sorted(ARTIFACT_NAMES):
        artifact = artifacts[name]
        if not isinstance(artifact, dict):
            raise AnalysisError(f"metadata artifact {name} must be an object")
        require_exact_keys(artifact, {"path", "sha256"}, f"metadata artifact {name}")
        require_nonempty_string(artifact["path"], f"metadata artifact {name} path")
        require_sha256(artifact["sha256"], f"metadata artifact {name} sha256")


def validate_capture_evidence(
    lines: Sequence[str], metadata: dict[str, object], raw_sha256: str
) -> dict[str, object]:
    """Require successful-run, guest-topology, workload, and provenance evidence."""
    require_exact_marker(lines, RUN_START_MARKER)
    require_exact_marker(lines, RUN_COMPLETE_MARKER)
    if any(line.strip().startswith(RUN_FAILED_PREFIX) for line in lines):
        raise AnalysisError("capture contains AXVISOR_RT_RUN_FAILED")

    guest = require_metadata_object(metadata, "guest")
    vcpu_count = guest.get("vcpu_count")
    if type(vcpu_count) is not int or vcpu_count < 2:
        raise AnalysisError("metadata guest vcpu_count must be an integer of at least 2")
    guest_cpu_fields = require_marker_fields(
        lines, GUEST_CPUS_PREFIX, {"schema", "online"}
    )
    require_schema_one(guest_cpu_fields, GUEST_CPUS_PREFIX)
    online_cpus = parse_marker_int(guest_cpu_fields, "online", GUEST_CPUS_PREFIX)
    if online_cpus != vcpu_count:
        raise AnalysisError(
            f"online CPU count {online_cpus} does not match guest vcpu_count {vcpu_count}"
        )
    cpu_load = summarize_cpu_load(lines, vcpu_count)
    validate_measurement_order(lines)

    benchmark = require_metadata_object(metadata, "benchmark")
    workload = benchmark.get("workload")
    if not isinstance(workload, str) or not workload:
        raise AnalysisError("metadata benchmark workload must be a nonempty string")
    validate_workload_evidence(lines, workload, cpu_load)

    artifacts = require_metadata_object(metadata, "artifacts")
    raw_log = require_metadata_object(artifacts, "raw_log")
    recorded_sha256 = raw_log.get("sha256")
    if not isinstance(recorded_sha256, str) or recorded_sha256.lower() != raw_sha256:
        raise AnalysisError("metadata raw-log SHA-256 does not match the analyzed bytes")
    return cpu_load


def summarize_cpu_load(lines: Sequence[str], vcpu_count: int) -> dict[str, object]:
    """Validate paired guest /proc/stat snapshots and summarize CPU utilization."""
    records = marker_lines(lines, CPU_STAT_PREFIX)
    expected_record_count = vcpu_count * 2
    if len(records) != expected_record_count:
        raise AnalysisError(
            f"capture must contain {expected_record_count} {CPU_STAT_PREFIX} records; "
            f"found {len(records)}"
        )

    expected_fields = {"schema", "phase", "cpu", *CPU_STAT_COUNTERS}
    snapshots: dict[tuple[str, int], dict[str, int]] = {}
    for record in records:
        fields = parse_marker_record(record, CPU_STAT_PREFIX, expected_fields)
        require_schema_one(fields, CPU_STAT_PREFIX)
        phase = fields["phase"]
        if phase not in {"start", "end"}:
            raise AnalysisError(f"{CPU_STAT_PREFIX} has unsupported phase {phase!r}")
        cpu = parse_marker_int(fields, "cpu", CPU_STAT_PREFIX)
        if cpu >= vcpu_count:
            raise AnalysisError(
                f"{CPU_STAT_PREFIX} CPU {cpu} is outside guest vCPU count {vcpu_count}"
            )
        identity = (phase, cpu)
        if identity in snapshots:
            raise AnalysisError(f"duplicate {CPU_STAT_PREFIX} record for {identity}")
        snapshots[identity] = {
            counter: parse_marker_int(fields, counter, CPU_STAT_PREFIX)
            for counter in CPU_STAT_COUNTERS
        }

    expected_identities = {
        (phase, cpu) for phase in ("start", "end") for cpu in range(vcpu_count)
    }
    if set(snapshots) != expected_identities:
        raise AnalysisError(f"{CPU_STAT_PREFIX} records do not cover every guest CPU and phase")

    cpu_summaries: dict[str, object] = {}
    for cpu in range(vcpu_count):
        start = snapshots[("start", cpu)]
        end = snapshots[("end", cpu)]
        deltas: dict[str, int] = {}
        for counter in CPU_STAT_COUNTERS:
            delta = end[counter] - start[counter]
            if delta < 0:
                raise AnalysisError(
                    f"{CPU_STAT_PREFIX} CPU {cpu} counter {counter} regressed"
                )
            deltas[counter] = delta
        total_ticks = sum(deltas.values())
        if total_ticks == 0:
            raise AnalysisError(f"{CPU_STAT_PREFIX} CPU {cpu} has a zero-length window")
        idle_ticks = deltas["idle"] + deltas["iowait"]
        busy_ticks = total_ticks - idle_ticks
        cpu_summaries[str(cpu)] = {
            "busy_percent": round(100.0 * busy_ticks / total_ticks, 3),
            "busy_ticks": busy_ticks,
            "idle_ticks": idle_ticks,
            "total_ticks": total_ticks,
        }
    return {
        "schema_version": 1,
        "source": "guest /proc/stat",
        "unit": "scheduler ticks",
        "cpus": cpu_summaries,
    }


def validate_measurement_order(lines: Sequence[str]) -> None:
    """Require CPU-load snapshots to enclose every emitted measurement sample."""
    run_start = require_exact_marker(lines, RUN_START_MARKER)
    run_complete = require_exact_marker(lines, RUN_COMPLETE_MARKER)
    start_snapshots: list[int] = []
    end_snapshots: list[int] = []
    for index, record in marker_records_with_positions(lines, CPU_STAT_PREFIX):
        fields = parse_marker_record(
            record, CPU_STAT_PREFIX, {"schema", "phase", "cpu", *CPU_STAT_COUNTERS}
        )
        if fields["phase"] == "start":
            start_snapshots.append(index)
        elif fields["phase"] == "end":
            end_snapshots.append(index)
    sample_positions = [
        index for index, line in enumerate(lines) if line.strip().startswith(SAMPLE_PREFIX)
    ]
    if not start_snapshots or not end_snapshots or not sample_positions:
        raise AnalysisError("capture is missing ordered CPU snapshots or samples")
    if not (
        run_start < min(start_snapshots)
        and max(start_snapshots) < min(sample_positions)
        and max(sample_positions) < min(end_snapshots)
        and max(end_snapshots) < run_complete
    ):
        raise AnalysisError(
            "CPU start/end snapshots must enclose every sample inside the run markers"
        )


def validate_workload_evidence(
    lines: Sequence[str], workload: str, cpu_load: dict[str, object]
) -> None:
    """Require workload markers matching the requested benchmark workload."""
    active_records = marker_lines(lines, WORKLOAD_ACTIVE_PREFIX)
    cleaned_records = marker_lines(lines, WORKLOAD_CLEANED_PREFIX)
    external_records = marker_lines(lines, WORKLOAD_EXTERNAL_PREFIX)
    ready_records = marker_lines(lines, WORKLOAD_READY_PREFIX)
    stopped_records = marker_lines(lines, WORKLOAD_STOPPED_PREFIX)

    if workload == "idle":
        if cleaned_records or external_records or ready_records or stopped_records:
            raise AnalysisError("idle capture contains contradictory workload markers")
        fields = require_marker_fields(
            lines, WORKLOAD_ACTIVE_PREFIX, {"schema", "kind"}
        )
        require_schema_one(fields, WORKLOAD_ACTIVE_PREFIX)
        if fields["kind"] != "idle":
            raise AnalysisError("idle capture is missing its idle ACTIVE marker")
        return

    if workload == "cpu-stress":
        if external_records:
            raise AnalysisError("cpu-stress capture contains an external workload marker")
        ready = require_marker_fields(
            lines,
            WORKLOAD_READY_PREFIX,
            {"schema", "kind", "pid", "cpu"},
        )
        active = require_marker_fields(
            lines,
            WORKLOAD_ACTIVE_PREFIX,
            {"schema", "kind", "pid", "cpu", "affinity"},
        )
        cleaned = require_marker_fields(
            lines,
            WORKLOAD_CLEANED_PREFIX,
            {"schema", "kind", "pid", "status"},
        )
        stopped = require_marker_fields(
            lines,
            WORKLOAD_STOPPED_PREFIX,
            {"schema", "kind", "pid", "cpu"},
        )
        require_schema_one(ready, WORKLOAD_READY_PREFIX)
        require_schema_one(active, WORKLOAD_ACTIVE_PREFIX)
        require_schema_one(cleaned, WORKLOAD_CLEANED_PREFIX)
        require_schema_one(stopped, WORKLOAD_STOPPED_PREFIX)
        if any(
            marker["kind"] != "cpu-stress"
            for marker in (ready, active, stopped, cleaned)
        ):
            raise AnalysisError("cpu-stress markers have the wrong workload kind")
        ready_pid = parse_marker_int(ready, "pid", WORKLOAD_READY_PREFIX)
        active_pid = parse_marker_int(active, "pid", WORKLOAD_ACTIVE_PREFIX)
        stopped_pid = parse_marker_int(stopped, "pid", WORKLOAD_STOPPED_PREFIX)
        cleaned_pid = parse_marker_int(cleaned, "pid", WORKLOAD_CLEANED_PREFIX)
        if active_pid <= 0 or len({ready_pid, active_pid, stopped_pid, cleaned_pid}) != 1:
            raise AnalysisError("cpu-stress READY, ACTIVE, STOPPED, and CLEANED PIDs must match")
        if ready["cpu"] != "1" or stopped["cpu"] != "1":
            raise AnalysisError("cpu-stress READY and STOPPED markers must report CPU 1")
        if active["cpu"] != "1" or active["affinity"] != "1":
            raise AnalysisError("cpu-stress must be active with cpu=1 affinity=1")
        if cleaned["status"] != "0":
            raise AnalysisError("cpu-stress CLEANED status must be zero")
        validate_cpu_stress_marker_order(lines)
        validate_cpu_stress_load(cpu_load)
        return

    if workload.startswith("external:"):
        if active_records or cleaned_records or ready_records or stopped_records:
            raise AnalysisError("external capture contains managed workload markers")
        external = require_marker_fields(
            lines,
            WORKLOAD_EXTERNAL_PREFIX,
            {"schema", "verification", "label"},
        )
        require_schema_one(external, WORKLOAD_EXTERNAL_PREFIX)
        if external["verification"] != "caller" or external["label"] != workload:
            raise AnalysisError("external workload evidence does not match metadata")
        return

    raise AnalysisError(f"unsupported metadata workload {workload!r}")


def validate_cpu_stress_marker_order(lines: Sequence[str]) -> None:
    ready = require_single_marker_position(lines, WORKLOAD_READY_PREFIX)
    active = require_single_marker_position(lines, WORKLOAD_ACTIVE_PREFIX)
    stopped = require_single_marker_position(lines, WORKLOAD_STOPPED_PREFIX)
    cleaned = require_single_marker_position(lines, WORKLOAD_CLEANED_PREFIX)
    start_snapshots = [
        index
        for index, line in enumerate(lines)
        if line.strip().startswith(f"{CPU_STAT_PREFIX} ") and " phase=start " in line
    ]
    end_snapshots = [
        index
        for index, line in enumerate(lines)
        if line.strip().startswith(f"{CPU_STAT_PREFIX} ") and " phase=end " in line
    ]
    run_start = require_exact_marker(lines, RUN_START_MARKER)
    run_complete = require_exact_marker(lines, RUN_COMPLETE_MARKER)
    if not (run_start < ready < active < min(start_snapshots)):
        raise AnalysisError("cpu-stress must be READY and ACTIVE before measurement starts")
    if stopped <= max(end_snapshots):
        raise AnalysisError("cpu-stress stopped before the measurement window ended")
    if not (max(end_snapshots) < stopped < cleaned < run_complete):
        raise AnalysisError("cpu-stress STOPPED/CLEANED markers have invalid ordering")


def validate_cpu_stress_load(cpu_load: dict[str, object]) -> None:
    cpus = cpu_load.get("cpus")
    if not isinstance(cpus, dict) or not isinstance(cpus.get("1"), dict):
        raise AnalysisError("cpu-stress load summary is missing guest CPU 1")
    cpu_one = cpus["1"]
    busy_ticks = cpu_one.get("busy_ticks")
    total_ticks = cpu_one.get("total_ticks")
    if type(busy_ticks) is not int or type(total_ticks) is not int or total_ticks <= 0:
        raise AnalysisError("cpu-stress CPU 1 load counters are invalid")
    if busy_ticks * 100 < total_ticks * CPU_STRESS_MIN_BUSY_PERCENT:
        busy_percent = 100.0 * busy_ticks / total_ticks
        raise AnalysisError(
            "cpu-stress CPU 1 must be at least "
            f"{CPU_STRESS_MIN_BUSY_PERCENT}% busy; observed {busy_percent:.3f}%"
        )


def require_exact_keys(
    value: dict[str, object], expected_fields: set[str], label: str
) -> None:
    missing = sorted(expected_fields.difference(value))
    extra = sorted(set(value).difference(expected_fields))
    if missing:
        raise AnalysisError(f"{label} is missing fields: {', '.join(missing)}")
    if extra:
        raise AnalysisError(f"{label} has unknown fields: {', '.join(extra)}")


def require_nonempty_string(value: object, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise AnalysisError(f"{label} must be a nonempty string")
    return value


def require_timestamp(value: object, label: str) -> datetime:
    timestamp = require_nonempty_string(value, label)
    try:
        parsed = datetime.fromisoformat(timestamp.replace("Z", "+00:00"))
    except ValueError as error:
        raise AnalysisError(f"{label} must be an ISO-8601 date-time") from error
    if parsed.tzinfo is None:
        raise AnalysisError(f"{label} must include a timezone")
    return parsed


def require_sha256(value: object, label: str) -> str:
    if not isinstance(value, str) or SHA256_PATTERN.fullmatch(value) is None:
        raise AnalysisError(f"{label} must be a lowercase 64-digit SHA-256")
    return value


def require_nonnegative_int(value: object, label: str) -> int:
    if type(value) is not int or value < 0:
        raise AnalysisError(f"{label} must be a nonnegative integer")
    return value


def require_positive_int(value: object, label: str) -> int:
    parsed = require_nonnegative_int(value, label)
    if parsed == 0:
        raise AnalysisError(f"{label} must be positive")
    return parsed


def require_metadata_object(
    parent: dict[str, object], key: str
) -> dict[str, object]:
    value = parent.get(key)
    if not isinstance(value, dict):
        raise AnalysisError(f"metadata {key} must be an object")
    return value


def require_exact_marker(lines: Sequence[str], marker: str) -> int:
    positions = [index for index, line in enumerate(lines) if line.strip() == marker]
    if len(positions) != 1:
        raise AnalysisError(
            f"capture must contain exactly one {marker}; found {len(positions)}"
        )
    return positions[0]


def marker_lines(lines: Sequence[str], prefix: str) -> list[str]:
    marker_prefix = f"{prefix} "
    return [line.strip() for line in lines if line.strip().startswith(marker_prefix)]


def marker_records_with_positions(
    lines: Sequence[str], prefix: str
) -> list[tuple[int, str]]:
    marker_prefix = f"{prefix} "
    return [
        (index, line.strip())
        for index, line in enumerate(lines)
        if line.strip().startswith(marker_prefix)
    ]


def require_single_marker_position(lines: Sequence[str], prefix: str) -> int:
    records = marker_records_with_positions(lines, prefix)
    if len(records) != 1:
        raise AnalysisError(f"capture must contain exactly one {prefix}; found {len(records)}")
    return records[0][0]


def require_marker_fields(
    lines: Sequence[str], prefix: str, expected_fields: set[str]
) -> dict[str, str]:
    records = marker_lines(lines, prefix)
    if len(records) != 1:
        raise AnalysisError(f"capture must contain exactly one {prefix}; found {len(records)}")

    return parse_marker_record(records[0], prefix, expected_fields)


def parse_marker_record(
    record: str, prefix: str, expected_fields: set[str]
) -> dict[str, str]:
    fields: dict[str, str] = {}
    for token in record.removeprefix(f"{prefix} ").split():
        key, separator, value = token.partition("=")
        if not separator or not key or not value:
            raise AnalysisError(f"{prefix} contains malformed field {token!r}")
        if key in fields:
            raise AnalysisError(f"{prefix} contains duplicate field {key!r}")
        fields[key] = value
    missing = sorted(expected_fields.difference(fields))
    extra = sorted(set(fields).difference(expected_fields))
    if missing:
        raise AnalysisError(f"{prefix} is missing fields: {', '.join(missing)}")
    if extra:
        raise AnalysisError(f"{prefix} has unknown fields: {', '.join(extra)}")
    return fields


def require_schema_one(fields: dict[str, str], marker: str) -> None:
    if fields["schema"] != "1":
        raise AnalysisError(f"{marker} has unsupported schema")


def parse_marker_int(fields: dict[str, str], key: str, marker: str) -> int:
    try:
        value = int(fields[key], 10)
    except ValueError as error:
        raise AnalysisError(f"{marker} field {key} must be a base-10 integer") from error
    if value < 0:
        raise AnalysisError(f"{marker} field {key} must be nonnegative")
    return value


def validate_samples_against_metadata(
    samples: Sequence[Sample], metadata: dict[str, object]
) -> None:
    """Require the raw sample set promised by capture metadata."""
    benchmark = metadata.get("benchmark")
    if not isinstance(benchmark, dict):
        raise AnalysisError("metadata benchmark must be an object")

    iterations = benchmark.get("iterations")
    guest_cpu = benchmark.get("guest_cpu")
    metrics = benchmark.get("metrics")
    if type(iterations) is not int or iterations <= 0:
        raise AnalysisError("metadata benchmark iterations must be a positive integer")
    if type(guest_cpu) is not int or guest_cpu < 0:
        raise AnalysisError("metadata benchmark guest_cpu must be nonnegative")
    if not isinstance(metrics, list) or set(metrics) != set(EXPECTED_METRICS):
        raise AnalysisError("metadata benchmark metrics do not match the analyzer contract")

    expected_iterations = list(range(iterations))
    for metric in EXPECTED_METRICS:
        metric_samples = [sample for sample in samples if sample.metric == metric]
        if len(metric_samples) != iterations:
            raise AnalysisError(
                f"metric {metric} expected {iterations} samples, got {len(metric_samples)}"
            )
        if any(sample.cpu != guest_cpu for sample in metric_samples):
            raise AnalysisError(
                f"metric {metric} contains samples from a CPU other than {guest_cpu}"
            )
        observed_iterations = sorted(sample.iteration for sample in metric_samples)
        if observed_iterations != expected_iterations:
            raise AnalysisError(
                f"metric {metric} iterations are not the complete range 0..{iterations - 1}"
            )


def parse_sample_line(line: str, line_number: int) -> Sample | None:
    stripped = line.strip()
    if not stripped.startswith(SAMPLE_PREFIX):
        return None

    fields: dict[str, str] = {}
    for token in stripped.removeprefix(SAMPLE_PREFIX).split():
        key, separator, value = token.partition("=")
        if not separator or not key or not value:
            raise AnalysisError(f"line {line_number}: malformed field {token!r}")
        if key in fields:
            raise AnalysisError(f"line {line_number}: duplicate field {key!r}")
        fields[key] = value

    missing = sorted(REQUIRED_FIELDS.difference(fields))
    extra = sorted(set(fields).difference(REQUIRED_FIELDS))
    if missing:
        raise AnalysisError(f"line {line_number}: missing fields: {', '.join(missing)}")
    if extra:
        raise AnalysisError(f"line {line_number}: unknown fields: {', '.join(extra)}")
    if fields["schema"] != "1":
        raise AnalysisError(f"line {line_number}: unsupported sample schema")
    if fields["metric"] not in EXPECTED_METRICS:
        raise AnalysisError(
            f"line {line_number}: unsupported metric {fields['metric']!r}"
        )

    iteration = parse_nonnegative_int(fields["iteration"], "iteration", line_number)
    cpu = parse_nonnegative_int(fields["cpu"], "cpu", line_number)
    target_ns = parse_nonnegative_int(fields["target_ns"], "target_ns", line_number)
    observed_ns = parse_nonnegative_int(
        fields["observed_ns"], "observed_ns", line_number
    )
    latency_ns = parse_nonnegative_int(fields["latency_ns"], "latency_ns", line_number)
    if observed_ns - target_ns != latency_ns:
        raise AnalysisError(
            f"line {line_number}: latency_ns does not equal observed_ns - target_ns"
        )

    return Sample(
        metric=fields["metric"],
        iteration=iteration,
        cpu=cpu,
        target_ns=target_ns,
        observed_ns=observed_ns,
        latency_ns=latency_ns,
    )


def parse_nonnegative_int(value: str, field: str, line_number: int) -> int:
    try:
        parsed = int(value, 10)
    except ValueError as error:
        raise AnalysisError(
            f"line {line_number}: {field} must be a base-10 integer"
        ) from error
    if parsed < 0:
        raise AnalysisError(f"line {line_number}: {field} must be nonnegative")
    return parsed


def summarize_latencies(latencies: Sequence[int]) -> dict[str, int | float | str]:
    if not latencies:
        raise AnalysisError("cannot summarize an empty latency series")
    ordered = sorted(latencies)
    return {
        "unit": "ns",
        "count": len(ordered),
        "min_ns": ordered[0],
        "max_ns": ordered[-1],
        "mean_ns": round(statistics.fmean(ordered), 3),
        "population_stddev_ns": round(statistics.pstdev(ordered), 3),
        "p50_ns": nearest_rank(ordered, 0.50),
        "p90_ns": nearest_rank(ordered, 0.90),
        "p99_ns": nearest_rank(ordered, 0.99),
        "p999_ns": nearest_rank(ordered, 0.999),
    }


def nearest_rank(ordered_values: Sequence[int], percentile: float) -> int:
    """Return the nearest-rank percentile from an already sorted series."""
    if not ordered_values:
        raise AnalysisError("cannot select a percentile from an empty series")
    if not 0 < percentile <= 1:
        raise AnalysisError("percentile must be in the interval (0, 1]")
    rank = max(1, math.ceil(percentile * len(ordered_values)))
    return ordered_values[rank - 1]


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path, help="captured AxVisor/QEMU console log")
    parser.add_argument("--metadata", type=Path, help="schema-version 1 run metadata")
    parser.add_argument("--output", type=Path, help="summary JSON path; defaults to stdout")
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        result = analyze_file(args.input, args.metadata)
    except (AnalysisError, OSError, json.JSONDecodeError) as error:
        print(f"analysis failed: {error}", file=sys.stderr)
        return 2

    rendered = json.dumps(result, indent=2, sort_keys=True, allow_nan=False) + "\n"
    if args.output is None:
        sys.stdout.write(rendered)
    else:
        args.output.write_text(rendered, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
