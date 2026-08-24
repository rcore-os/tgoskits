#!/usr/bin/env python3
"""Summarize periodic real-time samples from CSV or standard input.

Expected columns:
    sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns
"""

from __future__ import annotations

import argparse
import csv
import math
import sys
from pathlib import Path


def percentile(values: list[int], fraction: float) -> int:
    if not values:
        return 0
    ordered = sorted(values)
    index = min(len(ordered) - 1, math.ceil(fraction * len(ordered)) - 1)
    return ordered[index]


def read_samples(stream, tolerance_ns: int) -> tuple[list[int], int, int]:
    reader = csv.DictReader(
        line for line in stream if line.strip() and not line.lstrip().startswith("#")
    )
    required = {"jitter_ns", "deadline_ns", "actual_ns"}
    missing = required - set(reader.fieldnames or ())
    if missing:
        raise ValueError(f"missing columns: {', '.join(sorted(missing))}")

    jitter: list[int] = []
    zero_tolerance_misses = 0
    tolerance_misses = 0
    for row in reader:
        jitter_ns = int(row["jitter_ns"])
        jitter.append(jitter_ns)
        actual_ns = int(row["actual_ns"])
        deadline_ns = int(row["deadline_ns"])
        if actual_ns > deadline_ns:
            zero_tolerance_misses += 1
        if actual_ns > deadline_ns + tolerance_ns:
            tolerance_misses += 1
    return jitter, zero_tolerance_misses, tolerance_misses


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--tolerance-ns",
        type=int,
        default=1_000_000,
        help="allowed lateness for the configured deadline miss count (default: 1 ms)",
    )
    parser.add_argument("csv", nargs="?", type=Path, help="CSV file; read stdin when omitted")
    args = parser.parse_args()

    if args.tolerance_ns < 0:
        parser.error("tolerance must be non-negative")

    try:
        if args.csv is None:
            samples, zero_misses, tolerance_misses = read_samples(
                sys.stdin, args.tolerance_ns
            )
        else:
            with args.csv.open(newline="") as stream:
                samples, zero_misses, tolerance_misses = read_samples(
                    stream, args.tolerance_ns
                )
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2

    if not samples:
        print("samples=0")
        return 0

    print(f"samples={len(samples)}")
    print(f"mean_jitter_ns={sum(samples) / len(samples):.2f}")
    print(f"p99_jitter_ns={percentile(samples, 0.99)}")
    print(f"p99_9_jitter_ns={percentile(samples, 0.999)}")
    print(f"max_jitter_ns={max(samples)}")
    print(f"deadline_tolerance_ns={args.tolerance_ns}")
    print(f"deadline_misses={zero_misses}")
    print(f"deadline_misses_zero_tolerance={zero_misses}")
    print(f"deadline_misses_tolerance={tolerance_misses}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
