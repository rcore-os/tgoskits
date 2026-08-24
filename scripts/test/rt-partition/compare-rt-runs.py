#!/usr/bin/env python3
"""Compare repeated baseline and modified RT experiment archives."""

from __future__ import annotations

import argparse
import statistics
import sys
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class RunMetrics:
    path: Path
    git_commit: str
    p99_jitter_ns: int
    p99_9_jitter_ns: int
    max_jitter_ns: int
    deadline_misses_tolerance: int
    linux_avg_latency_us: int
    linux_p99_latency_us: int
    linux_p99_9_latency_us: int
    linux_max_latency_us: int
    linux_overflow_samples: int
    linux_total_samples: int


def read_key_values(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    for line in path.read_text().splitlines():
        if not line or line.startswith("#"):
            continue
        if "=" not in line:
            raise ValueError(f"{path} contains a malformed line: {line}")
        name, value = line.split("=", 1)
        values[name] = value
    return values


def required(values: dict[str, str], name: str, path: Path) -> str:
    try:
        return values[name]
    except KeyError as error:
        raise ValueError(f"{path} is missing {name}") from error


def read_run(path: Path) -> RunMetrics:
    meta_path = path / "meta.txt"
    stats_path = path / "zephyr-stats.txt"
    linux_stats_path = path / "cyclictest-summary.txt"
    meta = read_key_values(meta_path)
    stats = read_key_values(stats_path)
    linux_stats = read_key_values(linux_stats_path)
    return RunMetrics(
        path=path,
        git_commit=required(meta, "git_commit", meta_path),
        p99_jitter_ns=int(required(stats, "p99_jitter_ns", stats_path)),
        p99_9_jitter_ns=int(required(stats, "p99_9_jitter_ns", stats_path)),
        max_jitter_ns=int(required(stats, "max_jitter_ns", stats_path)),
        deadline_misses_tolerance=int(
            required(stats, "deadline_misses_tolerance", stats_path)
        ),
        linux_avg_latency_us=int(
            required(linux_stats, "avg_latency_us", linux_stats_path)
        ),
        linux_p99_latency_us=int(
            required(linux_stats, "p99_latency_us", linux_stats_path)
        ),
        linux_p99_9_latency_us=int(
            required(linux_stats, "p99_9_latency_us", linux_stats_path)
        ),
        linux_max_latency_us=int(
            required(linux_stats, "max_latency_us", linux_stats_path)
        ),
        linux_overflow_samples=int(
            required(linux_stats, "overflow_samples", linux_stats_path)
        ),
        linux_total_samples=int(
            required(linux_stats, "total_samples", linux_stats_path)
        ),
    )


def read_group(name: str, paths: list[Path]) -> list[RunMetrics]:
    runs = [read_run(path) for path in paths]
    commits = {run.git_commit for run in runs}
    if len(commits) != 1:
        raise ValueError(f"{name} runs use multiple git commits: {sorted(commits)}")
    return runs


def format_number(value: float | int) -> str:
    numeric = float(value)
    return str(int(numeric)) if numeric.is_integer() else f"{numeric:.6f}"


def append_metric_summary(
    lines: list[str], group_name: str, metric_name: str, values: list[int]
) -> None:
    lines.extend(
        (
            f"{group_name}_{metric_name}_median={format_number(statistics.median(values))}",
            f"{group_name}_{metric_name}_min={min(values)}",
            f"{group_name}_{metric_name}_max={max(values)}",
        )
    )


def build_summary(
    baseline_label: str,
    baseline: list[RunMetrics],
    modified_label: str,
    modified: list[RunMetrics],
) -> str:
    lines = [
        f"baseline_label={baseline_label}",
        f"baseline_git_commit={baseline[0].git_commit}",
        f"baseline_runs={len(baseline)}",
        f"modified_label={modified_label}",
        f"modified_git_commit={modified[0].git_commit}",
        f"modified_runs={len(modified)}",
    ]
    for metric_name in (
        "p99_jitter_ns",
        "p99_9_jitter_ns",
        "max_jitter_ns",
        "deadline_misses_tolerance",
    ):
        append_metric_summary(
            lines,
            "baseline",
            metric_name,
            [getattr(run, metric_name) for run in baseline],
        )
        append_metric_summary(
            lines,
            "modified",
            metric_name,
            [getattr(run, metric_name) for run in modified],
        )

        # Explicit aliases keep old consumers working while making the RTOS
        # scope unambiguous beside the Linux cyclictest metrics below.
        append_metric_summary(
            lines,
            "baseline_zephyr",
            metric_name,
            [getattr(run, metric_name) for run in baseline],
        )
        append_metric_summary(
            lines,
            "modified_zephyr",
            metric_name,
            [getattr(run, metric_name) for run in modified],
        )

    for metric_name in (
        "linux_avg_latency_us",
        "linux_p99_latency_us",
        "linux_p99_9_latency_us",
        "linux_max_latency_us",
        "linux_overflow_samples",
        "linux_total_samples",
    ):
        append_metric_summary(
            lines,
            "baseline",
            metric_name,
            [getattr(run, metric_name) for run in baseline],
        )
        append_metric_summary(
            lines,
            "modified",
            metric_name,
            [getattr(run, metric_name) for run in modified],
        )

    baseline_p99 = statistics.median(run.p99_jitter_ns for run in baseline)
    modified_p99 = statistics.median(run.p99_jitter_ns for run in modified)
    if modified_p99 == 0:
        lines.append("p99_improvement_ratio=not-computable")
        lines.append("zephyr_p99_improvement_ratio=not-computable")
    else:
        lines.append(f"p99_improvement_ratio={baseline_p99 / modified_p99:.6f}")
        lines.append(
            f"zephyr_p99_improvement_ratio={baseline_p99 / modified_p99:.6f}"
        )

    baseline_linux_p99 = statistics.median(
        run.linux_p99_latency_us for run in baseline
    )
    modified_linux_p99 = statistics.median(
        run.linux_p99_latency_us for run in modified
    )
    if modified_linux_p99 == 0:
        lines.append("linux_p99_improvement_ratio=not-computable")
    else:
        lines.append(
            "linux_p99_improvement_ratio="
            f"{baseline_linux_p99 / modified_linux_p99:.6f}"
        )

    if len(baseline) == len(modified):
        paired_ratios = [
            before.p99_jitter_ns / after.p99_jitter_ns
            for before, after in zip(baseline, modified)
            if after.p99_jitter_ns > 0
        ]
        if len(paired_ratios) == len(baseline):
            lines.append(
                "paired_p99_improvement_ratio_median="
                f"{statistics.median(paired_ratios):.6f}"
            )
            lines.append(
                "paired_zephyr_p99_improvement_ratio_median="
                f"{statistics.median(paired_ratios):.6f}"
            )

        paired_linux_ratios = [
            before.linux_p99_latency_us / after.linux_p99_latency_us
            for before, after in zip(baseline, modified)
            if after.linux_p99_latency_us > 0
        ]
        if len(paired_linux_ratios) == len(baseline):
            lines.append(
                "paired_linux_p99_improvement_ratio_median="
                f"{statistics.median(paired_linux_ratios):.6f}"
            )

    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline-label", default="baseline")
    parser.add_argument("--baseline", nargs="+", required=True, type=Path)
    parser.add_argument("--modified-label", default="modified")
    parser.add_argument("--modified", nargs="+", required=True, type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    try:
        baseline = read_group("baseline", args.baseline)
        modified = read_group("modified", args.modified)
        summary = build_summary(
            args.baseline_label, baseline, args.modified_label, modified
        )
        if args.output is None:
            sys.stdout.write(summary)
        else:
            args.output.write_text(summary)
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
