#!/usr/bin/env python3
"""Validate and summarize sustained ATK Task 1 periodic/YOLO runs."""

from __future__ import annotations

import argparse
import csv
import html
import math
import re
import statistics
from dataclasses import dataclass
from pathlib import Path


ROW = re.compile(r"(\d+),(\d+),(\d+),(\d+),(\d+)")
COMPLETE = re.compile(r"PERIODIC LATENCY COMPLETE samples=(\d+)")
CSV_HEADER = "sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns"
INTEGER_FIELDS = {
    name: re.compile(rf"\b{name}=(\d+)\b")
    for name in ("elapsed_ms", "infer_us", "sample")
}
FATAL_MARKERS = (
    re.compile(r"\bESR_EL2\b", re.IGNORECASE),
    re.compile(r"\bfatal IRQ\b", re.IGNORECASE),
    re.compile(r"\bpanic(?:ked)?\b", re.IGNORECASE),
    re.compile(r"TASK1_ARM_ERROR"),
)


@dataclass(frozen=True)
class InferenceSample:
    infer_us: int
    elapsed_ms: int | None
    sample: int | None


@dataclass(frozen=True)
class RunData:
    path: Path
    scheduler: str
    rows: list[tuple[int, int, int, int, int]]
    inferences: list[InferenceSample]
    periodic_duration_seconds: float
    inference_duration_seconds: float


def percentile_nearest_rank(values: list[int], fraction: float) -> int:
    if not values:
        raise ValueError("percentile requires at least one value")
    ordered = sorted(values)
    return ordered[max(0, math.ceil(fraction * len(ordered)) - 1)]


def parse(
    path: Path,
    *,
    expected_samples: int | None = None,
    min_inferences: int = 0,
    min_runtime_seconds: float = 0,
) -> RunData:
    text = path.read_text(errors="replace")
    reject_fatal_markers(path, text)
    start = text.find("PERIODIC LATENCY START")
    completion = COMPLETE.search(text, start)
    if start < 0 or completion is None:
        raise ValueError(f"{path}: incomplete periodic latency block")
    header = text.find(CSV_HEADER, start, completion.start())
    if header < 0:
        raise ValueError(f"{path}: periodic latency CSV header is missing")

    rows = parse_periodic_rows(text[header + len(CSV_HEADER) : completion.start()])
    validate_periodic_rows(path, rows, int(completion.group(1)), expected_samples)
    inferences = parse_inferences(text)
    if len(inferences) < min_inferences:
        raise ValueError(
            f"{path}: expected at least {min_inferences} YOLO inferences, got {len(inferences)}"
        )

    # timestamp_ns is relative to the probe's measurement base. actual_ns may
    # be an absolute guest counter value (as in Zephyr), so it must not be used
    # as the run duration.
    periodic_duration_seconds = rows[-1][1] / 1_000_000_000
    elapsed_values = [
        sample.elapsed_ms for sample in inferences if sample.elapsed_ms is not None
    ]
    inference_duration_seconds = max(elapsed_values, default=0) / 1000
    if periodic_duration_seconds < min_runtime_seconds:
        raise ValueError(
            f"{path}: periodic runtime {periodic_duration_seconds:.3f}s is below "
            f"{min_runtime_seconds:.3f}s"
        )
    if min_runtime_seconds > 0 and inference_duration_seconds < min_runtime_seconds:
        raise ValueError(
            f"{path}: YOLO runtime {inference_duration_seconds:.3f}s is below "
            f"{min_runtime_seconds:.3f}s; sustained overlap is not proven"
        )

    dropped = [int(value) for value in re.findall(r"output_dropped=(\d+)", text)]
    if any(value != 0 for value in dropped):
        raise ValueError(f"{path}: guest console output was dropped: {dropped}")
    return RunData(
        path=path,
        scheduler=parse_scheduler(path, text),
        rows=rows,
        inferences=inferences,
        periodic_duration_seconds=periodic_duration_seconds,
        inference_duration_seconds=inference_duration_seconds,
    )


def reject_fatal_markers(path: Path, text: str) -> None:
    for marker in FATAL_MARKERS:
        match = marker.search(text)
        if match is not None:
            line = text.count("\n", 0, match.start()) + 1
            raise ValueError(f"{path}:{line}: fatal marker {match.group(0)!r}")


def parse_periodic_rows(block: str) -> list[tuple[int, int, int, int, int]]:
    rows = []
    for line in block.splitlines():
        match = ROW.fullmatch(line.strip())
        if match:
            rows.append(tuple(map(int, match.groups())))
    return rows


def validate_periodic_rows(
    path: Path,
    rows: list[tuple[int, int, int, int, int]],
    declared_samples: int,
    expected_samples: int | None,
) -> None:
    if not rows:
        raise ValueError(f"{path}: no periodic latency samples")
    sequence = [row[0] for row in rows]
    if sequence != list(range(len(rows))):
        raise ValueError(f"{path}: non-contiguous periodic sample sequence")
    if declared_samples != len(rows):
        raise ValueError(f"{path}: declared sample count does not match CSV rows")
    if expected_samples is not None and len(rows) != expected_samples:
        raise ValueError(
            f"{path}: expected {expected_samples} periodic samples, got {len(rows)}"
        )
    timestamps = [row[1] for row in rows]
    if any(current <= previous for previous, current in zip(timestamps, timestamps[1:])):
        raise ValueError(f"{path}: periodic timestamps are not strictly increasing")
    # Each field is converted from counter cycles independently, so integer
    # division may make (actual - deadline) differ from jitter by one ns.
    if any(row[2] > row[3] or abs((row[3] - row[2]) - row[4]) > 1 for row in rows):
        raise ValueError(f"{path}: inconsistent deadline, actual, or jitter fields")


def parse_inferences(text: str) -> list[InferenceSample]:
    samples = []
    for line in text.splitlines():
        if "TASK3_INFER " not in line or "model=yolo11n.ncnn" not in line:
            continue
        infer_us = extract_integer(line, "infer_us")
        if infer_us is None:
            continue
        samples.append(
            InferenceSample(
                infer_us=infer_us,
                elapsed_ms=extract_integer(line, "elapsed_ms"),
                sample=extract_integer(line, "sample"),
            )
        )
    return samples


def extract_integer(line: str, name: str) -> int | None:
    match = INTEGER_FIELDS[name].search(line)
    return int(match.group(1)) if match is not None else None


def parse_scheduler(path: Path, text: str) -> str:
    runner = re.search(r"TASK1_RUNNER scheduler=(rr|fp-rr)\b", text)
    if runner is not None:
        return runner.group(1)
    name = path.name.lower()
    if "fp-rr" in name:
        return "fp-rr"
    if re.search(r"(?:^|[-_.])rr(?:[-_.]|$)", name):
        return "rr"
    raise ValueError(f"{path}: cannot determine scheduler from log metadata or filename")


def summarize_run(run: RunData, period_ms: int) -> dict[str, object]:
    jitter = [row[4] for row in run.rows]
    inference = [sample.infer_us for sample in run.inferences]
    inference_per_minute: float | str = (
        len(inference) * 60 / run.inference_duration_seconds
        if len(inference) >= 2 and run.inference_duration_seconds > 0
        else ""
    )
    return {
        "scheduler": run.scheduler,
        "log": run.path.name,
        "samples": len(jitter),
        "runtime_seconds": round(run.periodic_duration_seconds, 3),
        "min_ns": min(jitter),
        "mean_ns": round(statistics.mean(jitter)),
        "p50_ns": percentile_nearest_rank(jitter, 0.50),
        "p90_ns": percentile_nearest_rank(jitter, 0.90),
        "p95_ns": percentile_nearest_rank(jitter, 0.95),
        "p99_ns": percentile_nearest_rank(jitter, 0.99),
        "p99_9_ns": percentile_nearest_rank(jitter, 0.999),
        "max_ns": max(jitter),
        "over_1ms": sum(value > 1_000_000 for value in jitter),
        "over_10ms": sum(value > 10_000_000 for value in jitter),
        "deadline_misses": sum(value > period_ms * 1_000_000 for value in jitter),
        "inference_samples": len(inference),
        "inference_runtime_seconds": round(run.inference_duration_seconds, 3),
        "inference_mean_us": round(statistics.mean(inference)) if inference else "",
        "inference_p95_us": percentile_nearest_rank(inference, 0.95) if inference else "",
        "inference_p99_us": percentile_nearest_rank(inference, 0.99) if inference else "",
        "inference_max_us": max(inference) if inference else "",
        "inferences_per_minute": (
            round(inference_per_minute, 3) if inference_per_minute != "" else ""
        ),
    }


def summarize_groups(summaries: list[dict[str, object]]) -> list[dict[str, object]]:
    fields = (
        "samples",
        "runtime_seconds",
        "mean_ns",
        "p50_ns",
        "p99_ns",
        "p99_9_ns",
        "max_ns",
        "over_1ms",
        "over_10ms",
        "deadline_misses",
        "inference_samples",
        "inference_mean_us",
        "inference_p95_us",
        "inference_p99_us",
        "inference_max_us",
        "inferences_per_minute",
    )
    groups = []
    for scheduler in ("rr", "fp-rr"):
        runs = [summary for summary in summaries if summary["scheduler"] == scheduler]
        if not runs:
            continue
        group: dict[str, object] = {"scheduler": scheduler, "runs": len(runs)}
        for field in fields:
            values = [run[field] for run in runs if run[field] != ""]
            group[f"median_{field}"] = round(statistics.median(values), 3) if values else ""
        groups.append(group)
    return groups


def write_csv(path: Path, rows: list[dict[str, object]]) -> None:
    with path.open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


def write_chart(path: Path, groups: list[dict[str, object]]) -> None:
    p99_values = [float(group["median_p99_ns"]) / 1_000_000 for group in groups]
    throughput_values = [
        float(group["median_inferences_per_minute"] or 0) for group in groups
    ]
    p99_max = max(p99_values, default=1) or 1
    throughput_max = max(throughput_values, default=1) or 1
    colors = {"rr": "#d95f02", "fp-rr": "#1b9e77"}
    blocks = [
        '<svg xmlns="http://www.w3.org/2000/svg" width="720" height="360" viewBox="0 0 720 360">',
        '<rect width="720" height="360" fill="white"/>',
        '<text x="360" y="28" text-anchor="middle" font-family="sans-serif" font-size="18">Task 1 sustained YOLO A/B medians</text>',
    ]
    for panel, (title, values, maximum, unit) in enumerate(
        (("RTOS P99 jitter", p99_values, p99_max, "ms"), ("YOLO throughput", throughput_values, throughput_max, "infer/min"))
    ):
        origin_x = 40 + panel * 350
        blocks.append(
            f'<text x="{origin_x + 145}" y="62" text-anchor="middle" font-family="sans-serif" font-size="14">{html.escape(title)}</text>'
        )
        for index, (group, value) in enumerate(zip(groups, values)):
            bar_height = 190 * value / maximum
            x = origin_x + 45 + index * 120
            y = 280 - bar_height
            scheduler = str(group["scheduler"])
            blocks.extend(
                (
                    f'<rect x="{x}" y="{y:.2f}" width="70" height="{bar_height:.2f}" fill="{colors.get(scheduler, "#666")}"/>',
                    f'<text x="{x + 35}" y="{y - 7:.2f}" text-anchor="middle" font-family="sans-serif" font-size="12">{value:.3f} {unit}</text>',
                    f'<text x="{x + 35}" y="302" text-anchor="middle" font-family="sans-serif" font-size="12">{html.escape(scheduler)}</text>',
                )
            )
    blocks.append("</svg>")
    path.write_text("\n".join(blocks) + "\n")


def write_report(path: Path, groups: list[dict[str, object]], run_count: int) -> None:
    by_scheduler = {str(group["scheduler"]): group for group in groups}
    lines = [
        "# Sustained real-YOLO Task 1 A/B",
        "",
        f"Validated runs: {run_count}. Every accepted run has contiguous periodic samples, "
        "a matching completion count, the requested real ncnn/YOLO inference count, and no "
        "fatal marker.",
        "",
        "| Scheduler | Runs | Median P99 | Median P99.9 | Median max | YOLO mean | YOLO P99 | Throughput |",
        "|---|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for scheduler in ("rr", "fp-rr"):
        group = by_scheduler.get(scheduler)
        if group is None:
            continue
        throughput = group["median_inferences_per_minute"]
        throughput_text = f"{float(throughput):.3f}/min" if throughput != "" else "n/a"
        lines.append(
            f"| {scheduler} | {group['runs']} | {float(group['median_p99_ns']) / 1_000_000:.3f} ms | "
            f"{float(group['median_p99_9_ns']) / 1_000_000:.3f} ms | "
            f"{float(group['median_max_ns']) / 1_000_000:.3f} ms | "
            f"{float(group['median_inference_mean_us']) / 1000:.3f} ms | "
            f"{float(group['median_inference_p99_us']) / 1000:.3f} ms | "
            f"{throughput_text} |"
        )
    if "rr" in by_scheduler and "fp-rr" in by_scheduler:
        rr = float(by_scheduler["rr"]["median_p99_ns"])
        fp_rr = float(by_scheduler["fp-rr"]["median_p99_ns"])
        reduction = (rr - fp_rr) * 100 / rr if rr else 0
        lines.extend(
            (
                "",
                f"Median per-run P99 changes from {rr / 1_000_000:.3f} ms to "
                f"{fp_rr / 1_000_000:.3f} ms ({reduction:.3f}% reduction).",
                "",
                "The per-run and scheduler-median CSV files remain the source of truth; this "
                "report does not claim a native-RTOS bound or hide YOLO tail-latency/throughput trade-offs.",
            )
        )
    path.write_text("\n".join(lines) + "\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output_dir", type=Path)
    parser.add_argument("logs", nargs="+", type=Path)
    parser.add_argument("--expected-samples", type=int)
    parser.add_argument("--min-inferences", type=int, default=0)
    parser.add_argument("--min-runtime-seconds", type=float, default=0)
    parser.add_argument("--period-ms", type=int, default=10)
    parser.add_argument("--expected-runs-per-scheduler", type=int, default=0)
    args = parser.parse_args()
    if args.period_ms <= 0 or args.min_inferences < 0 or args.min_runtime_seconds < 0:
        parser.error("period must be positive and minimums must be nonnegative")

    args.output_dir.mkdir(parents=True, exist_ok=True)
    runs = [
        parse(
            path,
            expected_samples=args.expected_samples,
            min_inferences=args.min_inferences,
            min_runtime_seconds=args.min_runtime_seconds,
        )
        for path in args.logs
    ]
    summaries = []
    for run in runs:
        csv_path = args.output_dir / f"{run.path.stem}-periodic.csv"
        with csv_path.open("w", newline="") as stream:
            writer = csv.writer(stream)
            writer.writerow(("sequence", "timestamp_ns", "deadline_ns", "actual_ns", "jitter_ns"))
            writer.writerows(run.rows)
        summaries.append(summarize_run(run, args.period_ms))

    groups = summarize_groups(summaries)
    if args.expected_runs_per_scheduler:
        counts = {str(group["scheduler"]): int(group["runs"]) for group in groups}
        for scheduler in ("rr", "fp-rr"):
            if counts.get(scheduler, 0) != args.expected_runs_per_scheduler:
                raise ValueError(
                    f"expected {args.expected_runs_per_scheduler} {scheduler} runs, "
                    f"got {counts.get(scheduler, 0)}"
                )

    write_csv(args.output_dir / "periodic-summary.csv", summaries)
    write_csv(args.output_dir / "scheduler-medians.csv", groups)
    write_chart(args.output_dir / "scheduler-comparison.svg", groups)
    write_report(args.output_dir / "SUMMARY.md", groups, len(runs))
    print(args.output_dir / "SUMMARY.md")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
