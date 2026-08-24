#!/usr/bin/env python3
"""Summarize the single-variable post-VM-exit yield A/B experiment."""

import argparse
import re
import statistics
from pathlib import Path


def parse_fields(text: str) -> dict[str, float]:
    fields: dict[str, float] = {}
    for key, value in re.findall(r"([a-zA-Z0-9_.]+)=([^ ]+)", text):
        try:
            fields[key] = float(value)
        except ValueError:
            continue
    return fields


def read_fields(path: Path) -> dict[str, float]:
    return parse_fields(path.read_text(errors="replace").replace("\n", " "))


def read_vcpu_fields(path: Path, vcpu_id: int) -> dict[str, float]:
    prefix = f"vcpu={vcpu_id} "
    for line in path.read_text(errors="replace").splitlines():
        if line.strip().startswith(prefix):
            return parse_fields(line)
    raise ValueError(f"missing {prefix.strip()} diagnostics in {path}")


def load_run(path: Path, vcpu_id: int) -> dict[str, float]:
    vmexit = read_vcpu_fields(path / "vmexit-stat.txt", vcpu_id)
    timerlat = read_fields(path / "linux-timerlat-latency-summary.txt")
    cyclic = read_fields(path / "cyclictest-summary.txt")
    zephyr = read_fields(path / "zephyr-stats.txt")

    values = {
        "post_vmexit_yields": vmexit["post_vmexit_yields"],
        "direct_overlaps": vmexit["direct_overlaps"],
        "callback_dispatch_p50_us": vmexit["callback_to_run_dispatch_p50_ns"] / 1000,
        "callback_dispatch_p99_us": vmexit["callback_to_run_dispatch_p99_ns"] / 1000,
        "callback_dispatch_p99_9_us": vmexit["callback_to_run_dispatch_p99_9_ns"] / 1000,
        "direct_dispatch_p50_us": vmexit["direct_to_run_dispatch_p50_ns"] / 1000,
        "direct_dispatch_p99_us": vmexit["direct_to_run_dispatch_p99_ns"] / 1000,
        "direct_dispatch_p99_9_us": vmexit["direct_to_run_dispatch_p99_9_ns"] / 1000,
        "timerlat_irq_p99_us": timerlat["irq_latency_ns_p99"] / 1000,
        "timerlat_irq_p99_9_us": timerlat["irq_latency_ns_p99_9"] / 1000,
        "timerlat_thread_p99_us": timerlat["thread_latency_ns_p99"] / 1000,
        "timerlat_thread_p99_9_us": timerlat["thread_latency_ns_p99_9"] / 1000,
        "irq_to_thread_p99_us": timerlat["irq_to_thread_ns_p99"] / 1000,
        "irq_to_thread_p99_9_us": timerlat["irq_to_thread_ns_p99_9"] / 1000,
        "cyclictest_p99_us": cyclic["p99_latency_us"],
        "cyclictest_p99_9_us": cyclic["p99_9_latency_us"],
        "cyclictest_max_us": cyclic["max_latency_us"],
        "zephyr_p99_us": zephyr["p99_jitter_ns"] / 1000,
        "zephyr_p99_9_us": zephyr["p99_9_jitter_ns"] / 1000,
    }
    optional_vmexit_metrics = {
        "callback_guest_entry_p50_us": "callback_to_guest_entry_p50_ns",
        "callback_guest_entry_p99_us": "callback_to_guest_entry_p99_ns",
        "callback_guest_entry_p99_9_us": "callback_to_guest_entry_p99_9_ns",
        "direct_guest_entry_p50_us": "direct_to_guest_entry_p50_ns",
        "direct_guest_entry_p99_us": "direct_to_guest_entry_p99_ns",
        "direct_guest_entry_p99_9_us": "direct_to_guest_entry_p99_9_ns",
    }
    for output_key, input_key in optional_vmexit_metrics.items():
        if input_key in vmexit:
            values[output_key] = vmexit[input_key] / 1000
    return values


def reduction(before: float, after: float) -> float:
    if before == 0:
        raise ValueError("cannot calculate reduction from a zero baseline")
    return (before - after) * 100.0 / before


def format_values(values: list[float]) -> str:
    return ",".join(f"{value:.3f}" for value in values)


def summarize(
    baseline_paths: list[Path], modified_paths: list[Path], vcpu_id: int
) -> str:
    if len(baseline_paths) != len(modified_paths):
        raise ValueError("baseline and modified run counts must match")
    if not baseline_paths:
        raise ValueError("at least one A/B pair is required")

    baseline = [load_run(path, vcpu_id) for path in baseline_paths]
    modified = [load_run(path, vcpu_id) for path in modified_paths]
    baseline_yields = [run["post_vmexit_yields"] for run in baseline]
    modified_yields = [run["post_vmexit_yields"] for run in modified]
    if any(value <= 0 for value in baseline_yields):
        raise ValueError("baseline post_vmexit_yields must be non-zero")
    if any(value != 0 for value in modified_yields):
        raise ValueError("modified post_vmexit_yields must be zero")

    lines = [
        "post-VM-exit yield single-variable A/B",
        f"pairs={len(baseline)}",
        f"vcpu_id={vcpu_id}",
        f"baseline_post_vmexit_yields={format_values(baseline_yields)}",
        f"modified_post_vmexit_yields={format_values(modified_yields)}",
        "baseline_direct_overlaps="
        + format_values([run["direct_overlaps"] for run in baseline]),
        "modified_direct_overlaps="
        + format_values([run["direct_overlaps"] for run in modified]),
    ]

    metrics = [key for key in baseline[0] if key not in {"post_vmexit_yields", "direct_overlaps"}]
    for metric in metrics:
        before = [run[metric] for run in baseline]
        after = [run[metric] for run in modified]
        paired = [reduction(left, right) for left, right in zip(before, after)]
        before_median = statistics.median(before)
        after_median = statistics.median(after)
        lines.extend(
            [
                f"{metric}_baseline={format_values(before)}",
                f"{metric}_modified={format_values(after)}",
                f"{metric}_paired_reduction_pct={format_values(paired)}",
                f"{metric}_paired_improved={sum(value > 0 for value in paired)}/{len(paired)}",
                f"{metric}_baseline_median={before_median:.3f}",
                f"{metric}_modified_median={after_median:.3f}",
                f"{metric}_median_reduction_pct={reduction(before_median, after_median):.3f}",
            ]
        )
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", nargs="+", type=Path, required=True)
    parser.add_argument("--modified", nargs="+", type=Path, required=True)
    parser.add_argument("--vcpu-id", type=int, default=1)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    result = summarize(args.baseline, args.modified, args.vcpu_id)
    if args.output:
        args.output.write_text(result)
    print(result, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
