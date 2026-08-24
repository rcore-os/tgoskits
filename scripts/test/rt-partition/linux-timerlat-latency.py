#!/usr/bin/env python3
"""Summarize Linux timerlat IRQ and thread latency records."""

from __future__ import annotations

import argparse
import csv
import math
import re
import sys
from pathlib import Path


TIMERLAT_RECORD = re.compile(
    r"#(?P<activation>\d+)\s+context\s+"
    r"(?P<context>irq|thread)\s+timer_latency\s+"
    r"(?P<latency>\d+)\s+ns"
)


def nearest_rank(values: list[int], percentile: float) -> int:
    ordered = sorted(values)
    rank = max(1, math.ceil(percentile * len(ordered)))
    return ordered[rank - 1]


def parse_records(text: str) -> tuple[list[dict[str, int]], int, int]:
    irq_by_activation: dict[int, int] = {}
    rows: list[dict[str, int]] = []
    irq_records = 0
    thread_records = 0
    for match in TIMERLAT_RECORD.finditer(text):
        activation = int(match.group("activation"))
        latency_ns = int(match.group("latency"))
        if match.group("context") == "irq":
            irq_records += 1
            irq_by_activation[activation] = latency_ns
            continue
        thread_records += 1
        irq_latency_ns = irq_by_activation.pop(activation, None)
        if irq_latency_ns is None:
            continue
        rows.append(
            {
                "activation": activation,
                "irq_latency_ns": irq_latency_ns,
                "thread_latency_ns": latency_ns,
                "irq_to_thread_ns": max(0, latency_ns - irq_latency_ns),
            }
        )
    return rows, irq_records, thread_records


def build_summary(
    rows: list[dict[str, int]], irq_records: int, thread_records: int
) -> str:
    if not rows:
        raise ValueError("no complete timerlat IRQ/thread activations were found")
    lines = [
        f"irq_records={irq_records}",
        f"thread_records={thread_records}",
        f"complete_activations={len(rows)}",
        f"unmatched_irq_records={irq_records - len(rows)}",
        f"unmatched_thread_records={thread_records - len(rows)}",
    ]
    for name in ("irq_latency_ns", "thread_latency_ns", "irq_to_thread_ns"):
        values = [row[name] for row in rows]
        lines.extend(
            (
                f"{name}_p50={nearest_rank(values, 0.50)}",
                f"{name}_p90={nearest_rank(values, 0.90)}",
                f"{name}_p99={nearest_rank(values, 0.99)}",
                f"{name}_p99_9={nearest_rank(values, 0.999)}",
                f"{name}_max={max(values)}",
            )
        )
    return "\n".join(lines) + "\n"


def write_csv(path: Path, rows: list[dict[str, int]]) -> None:
    with path.open("w", newline="") as stream:
        writer = csv.DictWriter(
            stream,
            fieldnames=(
                "activation",
                "irq_latency_ns",
                "thread_latency_ns",
                "irq_to_thread_ns",
            ),
        )
        writer.writeheader()
        writer.writerows(rows)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trace", type=Path)
    parser.add_argument("summary", type=Path)
    parser.add_argument("--csv", type=Path)
    args = parser.parse_args()

    try:
        rows, irq_records, thread_records = parse_records(
            args.trace.read_text(errors="replace")
        )
        args.summary.write_text(build_summary(rows, irq_records, thread_records))
        if args.csv is not None:
            write_csv(args.csv, rows)
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
