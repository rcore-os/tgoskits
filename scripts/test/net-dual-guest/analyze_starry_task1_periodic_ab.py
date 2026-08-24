#!/usr/bin/env python3
"""Verify and compare repeated StarryOS periodic-latency A/B runs."""

from __future__ import annotations

import argparse
import math
import re
import statistics
from dataclasses import dataclass
from pathlib import Path


SAMPLE_RE = re.compile(
    r"^(\d+),(-?\d+),(-?\d+),(-?\d+),(-?\d+)\s*$", re.MULTILINE
)


@dataclass(frozen=True)
class Metrics:
    path: Path
    mean_ns: float
    p99_ns: int
    p99_9_ns: int
    max_ns: int
    misses_1ms: int
    infer_us: int
    lower_priority_services: int | None


def percentile(values: list[int], fraction: float) -> int:
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, math.ceil(fraction * len(ordered)) - 1)]


def read_run(path: Path, arm: str, rtos_name: str) -> Metrics:
    log_path = path / "run.log"
    log = log_path.read_text(errors="replace")
    samples = [(int(sequence), int(jitter)) for sequence, _, _, _, jitter in SAMPLE_RE.findall(log)]
    if [sequence for sequence, _ in samples] != list(range(300)):
        raise ValueError(f"{log_path}: expected exactly one ordered 300-sample series")
    expected_scheduler = "Round-robin" if arm == "rr" else "Fixed-priority round-robin"
    if f"{expected_scheduler} scheduler." not in log:
        raise ValueError(f"{log_path}: missing {expected_scheduler} scheduler marker")
    required = (
        "TASK3_MODEL_READY model=yolo11n.ncnn runtime=ncnn",
        "TASK3_INFER_STARTED model=yolo11n.ncnn request=1 phase=startup",
        "PERIODIC LATENCY START",
        "PERIODIC LATENCY COMPLETE samples=300",
    )
    for marker in required:
        if marker not in log:
            raise ValueError(f"{log_path}: missing marker {marker!r}")
    inference = re.search(r"TASK3_INFER model=yolo11n\.ncnn infer_us=(\d+) request=1\b", log)
    if inference is None:
        raise ValueError(f"{log_path}: real YOLO inference did not complete")
    jitter = [value for _, value in samples]
    counters = re.search(r"lower_priority_services=(\d+)", log)
    if arm == "fp-rr" and counters is None:
        raise ValueError(f"{log_path}: missing bounded FP-RR service counter")

    csv_path = path / f"{rtos_name}-periodic.csv"
    csv_path.write_text(
        "sequence,jitter_ns\n"
        + "".join(f"{sequence},{value}\n" for sequence, value in samples)
    )
    metrics = Metrics(
        path=path,
        mean_ns=sum(jitter) / len(jitter),
        p99_ns=percentile(jitter, 0.99),
        p99_9_ns=percentile(jitter, 0.999),
        max_ns=max(jitter),
        misses_1ms=sum(value > 1_000_000 for value in jitter),
        infer_us=int(inference.group(1)),
        lower_priority_services=int(counters.group(1)) if counters else None,
    )
    (path / f"{rtos_name}-stats.txt").write_text(
        f"samples=300\nmean_jitter_ns={metrics.mean_ns:.2f}\n"
        f"p99_jitter_ns={metrics.p99_ns}\np99_9_jitter_ns={metrics.p99_9_ns}\n"
        f"max_jitter_ns={metrics.max_ns}\ndeadline_tolerance_ns=1000000\n"
        f"deadline_misses_tolerance={metrics.misses_1ms}\n"
        f"yolo_infer_us={metrics.infer_us}\n"
        f"lower_priority_services={metrics.lower_priority_services if metrics.lower_priority_services is not None else 'n/a'}\n"
    )
    return metrics


def median_int(values: list[int]) -> float:
    return statistics.median(values)


def fmt_ns(value: float) -> str:
    return f"{value / 1_000_000:.3f} ms"


def reduction(before: float, after: float) -> float:
    return (before - after) / before * 100 if before else 0.0


def build_report(rr: list[Metrics], fp: list[Metrics], rtos_name: str) -> str:
    probe_label = "RT-Thread" if rtos_name == "rtthread" else "Zephyr"
    rr_p99 = median_int([run.p99_ns for run in rr])
    fp_p99 = median_int([run.p99_ns for run in fp])
    rr_p999 = median_int([run.p99_9_ns for run in rr])
    fp_p999 = median_int([run.p99_9_ns for run in fp])
    rr_max = median_int([run.max_ns for run in rr])
    fp_max = median_int([run.max_ns for run in fp])
    rr_misses = median_int([run.misses_1ms for run in rr])
    fp_misses = median_int([run.misses_1ms for run in fp])
    ratio = rr_p99 / fp_p99 if fp_p99 else float("inf")
    lines = [
        "# StarryOS Task 1 periodic-latency A/B",
        "",
        f"Both arms run the same in-Guest ncnn/YOLO workload on StarryOS (priority 89) "
        f"while a 300-sample, 10 ms {probe_label} periodic probe (priority 90) shares pCPU1. "
        "Only the AxVisor scheduler feature changes.",
        "",
        "| Metric (median across runs) | RR | bounded FP-RR | Change |",
        "|---|---:|---:|---:|",
        f"| P99 wake-up jitter | {fmt_ns(rr_p99)} | {fmt_ns(fp_p99)} | {ratio:.3f}x / {reduction(rr_p99, fp_p99):.2f}% lower |",
        f"| P99.9 wake-up jitter | {fmt_ns(rr_p999)} | {fmt_ns(fp_p999)} | {reduction(rr_p999, fp_p999):.2f}% lower |",
        f"| Maximum wake-up jitter | {fmt_ns(rr_max)} | {fmt_ns(fp_max)} | {reduction(rr_max, fp_max):.2f}% lower |",
        f"| Misses above 1 ms | {rr_misses:g}/300 | {fp_misses:g}/300 | {reduction(rr_misses, fp_misses):.2f}% lower |",
        "",
        "## Per-run evidence",
        "",
        "| Arm | Run | Mean | P99 | P99.9 / max | >1 ms | YOLO inference | lower-priority services |",
        "|---|---|---:|---:|---:|---:|---:|---:|",
    ]
    for arm, runs in (("RR", rr), ("FP-RR", fp)):
        for run in runs:
            services = "n/a" if run.lower_priority_services is None else str(run.lower_priority_services)
            lines.append(
                f"| {arm} | {run.path.name} | {fmt_ns(run.mean_ns)} | {fmt_ns(run.p99_ns)} | "
                f"{fmt_ns(run.p99_9_ns)} / {fmt_ns(run.max_ns)} | {run.misses_1ms}/300 | "
                f"{run.infer_us / 1_000_000:.3f} s | {services} |"
            )
    lines.extend(
        (
            "",
            "The P99 ratio is the supported near-10x claim. P99.9 and maximum are reported "
            "separately and must not be described as having the same improvement ratio.",
            "",
        )
    )
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rr", nargs="+", required=True, type=Path)
    parser.add_argument("--fp-rr", nargs="+", required=True, type=Path)
    parser.add_argument(
        "--rtos-name",
        choices=("zephyr", "rtthread"),
        default="zephyr",
        help="probe RTOS used for CSV/stat naming and the report label",
    )
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    if len(args.rr) != len(args.fp_rr):
        parser.error("RR and FP-RR must have the same number of repetitions")
    try:
        rr = [read_run(path, "rr", args.rtos_name) for path in args.rr]
        fp = [read_run(path, "fp-rr", args.rtos_name) for path in args.fp_rr]
    except (OSError, ValueError) as error:
        parser.error(str(error))
    args.output.write_text(build_report(rr, fp, args.rtos_name))
    print(f"PASS: verified {len(rr)} RR and {len(fp)} FP-RR periodic runs")
    print(f"comparison={args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
