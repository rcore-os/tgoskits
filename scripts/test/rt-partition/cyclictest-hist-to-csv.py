#!/usr/bin/env python3
"""Extract the cyclictest histogram from a console log and write CSV.

Input: a serial log that contains quiet cyclictest histogram output. Some
rt-tests versions omit the optional ``# Histogram`` heading:
    # Histogram                 (optional)
    000000 000005
    000001 000003
    # Min Latencies: ...

Output CSV columns: bucket_us,count. A separate summary file records latency
statistics and samples above the histogram ceiling.

Usage: cyclictest-hist-to-csv.py <log> <out.csv> <summary.txt>
"""

from __future__ import annotations

import argparse
import math
import re
import sys
from pathlib import Path

ANSI_ESCAPE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
HOST_TIMESTAMP_PREFIX = re.compile(r"^\[host_monotonic_s=[0-9.]+\]\s*")
VM_PREFIX = re.compile(r"^\[VM \d+\]\s*")


def normalize_console_line(line: str) -> str:
    line = ANSI_ESCAPE.sub("", line).strip()
    line = HOST_TIMESTAMP_PREFIX.sub("", line)
    return VM_PREFIX.sub("", line)


def extract_histogram(text: str) -> list[tuple[int, int]]:
    lines = [normalize_console_line(line) for line in text.splitlines()]
    summary_indexes = [
        index for index, line in enumerate(lines) if line.startswith("# Min Latencies:")
    ]
    if not summary_indexes:
        raise ValueError("cyclictest minimum latency summary not found in log")

    # The bucket rows are the contiguous numeric block immediately preceding
    # the summary. Anchoring from the summary avoids confusing earlier numeric
    # VM-exit tables with cyclictest output when the heading is omitted.
    buckets: list[tuple[int, int]] = []
    for line in reversed(lines[: summary_indexes[-1]]):
        match = re.fullmatch(r"\s*(\d+)\s+((?:\d+\s*)+)", line)
        if match is None:
            if buckets:
                break
            if not line:
                continue
            raise ValueError("cyclictest histogram contains no bucket rows")
        bucket = int(match.group(1))
        counts = [int(value) for value in match.group(2).split()]
        buckets.append((bucket, sum(counts)))

    if not buckets:
        raise ValueError("cyclictest histogram contains no bucket rows")
    buckets.reverse()
    return buckets


def histogram_percentile(
    buckets: list[tuple[int, int]], overflow_samples: int, fraction: float
) -> tuple[int, int]:
    total_samples = sum(count for _, count in buckets) + overflow_samples
    if total_samples == 0:
        return 0, 0

    rank = math.ceil(fraction * total_samples)
    cumulative = 0
    for bucket, count in buckets:
        cumulative += count
        if rank <= cumulative:
            return bucket, 0

    # Histogram overflow values are known only to exceed the last bucket.
    return buckets[-1][0] + 1, 1


def extract_summary(text: str, buckets: list[tuple[int, int]]) -> dict[str, int]:
    lines = [normalize_console_line(line) for line in text.splitlines()]
    summary_indexes = [
        index for index, line in enumerate(lines) if line.startswith("# Min Latencies:")
    ]
    if not summary_indexes:
        raise ValueError("cyclictest minimum latency summary not found in log")

    metrics: dict[str, int] = {}
    patterns = {
        "min_latency_us": re.compile(r"# Min Latencies:\s+(\d+)\s*$"),
        "avg_latency_us": re.compile(r"# Avg Latencies:\s+(\d+)\s*$"),
        "max_latency_us": re.compile(r"# Max Latencies:\s+(\d+)\s*$"),
        "overflow_samples": re.compile(r"# Histogram Overflows:\s+(\d+)\s*$"),
    }
    for line in lines[summary_indexes[-1] :]:
        for name, pattern in patterns.items():
            match = pattern.fullmatch(line)
            if match is not None:
                metrics[name] = int(match.group(1))
        if line == "RT_CYCLICTEST_COMPLETE":
            break

    missing = [name for name in patterns if name not in metrics]
    if missing:
        raise ValueError("cyclictest summary is missing: " + ", ".join(missing))
    metrics["bucket_samples"] = sum(count for _, count in buckets)
    metrics["total_samples"] = metrics["bucket_samples"] + metrics["overflow_samples"]
    for name, fraction in (
        ("p90", 0.90),
        ("p95", 0.95),
        ("p99", 0.99),
        ("p99_9", 0.999),
    ):
        value, censored = histogram_percentile(
            buckets, metrics["overflow_samples"], fraction
        )
        metrics[f"{name}_latency_us"] = value
        metrics[f"{name}_latency_censored"] = censored
    return metrics


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("log", type=Path)
    parser.add_argument("out", type=Path)
    parser.add_argument("summary", type=Path)
    args = parser.parse_args()

    try:
        text = args.log.read_text()
        buckets = extract_histogram(text)
        summary = extract_summary(text, buckets)
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2

    with args.out.open("w", newline="") as stream:
        stream.write("bucket_us,count\n")
        for bucket, count in buckets:
            stream.write(f"{bucket},{count}\n")
    with args.summary.open("w") as stream:
        for name in (
            "min_latency_us",
            "avg_latency_us",
            "max_latency_us",
            "p90_latency_us",
            "p90_latency_censored",
            "p95_latency_us",
            "p95_latency_censored",
            "p99_latency_us",
            "p99_latency_censored",
            "p99_9_latency_us",
            "p99_9_latency_censored",
            "bucket_samples",
            "overflow_samples",
            "total_samples",
        ):
            stream.write(f"{name}={summary[name]}\n")
    print(
        f"buckets={len(buckets)} bucket_samples={summary['bucket_samples']} "
        f"overflow_samples={summary['overflow_samples']} "
        f"total_samples={summary['total_samples']}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
