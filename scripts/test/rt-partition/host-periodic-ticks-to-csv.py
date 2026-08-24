#!/usr/bin/env python3
"""Extract host periodic scheduler tick snapshots and enforce CPU silence."""

import argparse
import csv
import re
from pathlib import Path


HEADER = "Host periodic scheduler ticks (event-driven timer IRQs excluded):"
TIMESTAMP_PREFIX = re.compile(r"^\[host_monotonic_s=[0-9.]+\] ")
ANSI_ESCAPE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
CPU_ROW = re.compile(r"^\s*cpu\s+(\d+):\s+(\d+)(?:\s+\([^)]*\))?\s*$")


def clean_line(line: str) -> str:
    line = ANSI_ESCAPE.sub("", line.rstrip("\r\n"))
    return TIMESTAMP_PREFIX.sub("", line)


def parse_snapshots(log: str) -> list[dict[int, int]]:
    snapshots: list[dict[int, int]] = []
    current: dict[int, int] | None = None

    for raw_line in log.splitlines():
        line = clean_line(raw_line)
        if line == HEADER:
            current = {}
            snapshots.append(current)
            continue
        if current is None:
            continue
        match = CPU_ROW.fullmatch(line)
        if match:
            cpu, count = map(int, match.groups())
            if cpu in current:
                raise ValueError(f"duplicate pCPU{cpu} in periodic scheduler tick snapshot")
            current[cpu] = count

    if len(snapshots) < 3:
        raise ValueError(
            f"expected at least three periodic scheduler tick snapshots, found {len(snapshots)}"
        )
    for index, snapshot in enumerate(snapshots):
        if not snapshot:
            raise ValueError(f"periodic scheduler tick snapshot {index} has no CPU rows")
    return snapshots


def snapshot_name(index: int, total: int) -> str:
    if index == 0:
        return "before"
    if index == 1:
        return "zephyr-after"
    if index == total - 1:
        return "linux-final"
    return f"intermediate-{index}"


def write_csv(path: Path, snapshots: list[dict[int, int]]) -> list[dict[str, int | str]]:
    rows: list[dict[str, int | str]] = []
    previous: dict[int, int] = {}
    for index, snapshot in enumerate(snapshots):
        for cpu, count in sorted(snapshot.items()):
            old_count = previous.get(cpu, 0)
            if count < old_count:
                raise ValueError(
                    f"pCPU{cpu} periodic scheduler tick count decreased: {old_count} -> {count}"
                )
            rows.append(
                {
                    "snapshot": snapshot_name(index, len(snapshots)),
                    "cpu": cpu,
                    "count": count,
                    "delta": count - old_count,
                }
            )
        previous = snapshot

    with path.open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=["snapshot", "cpu", "count", "delta"])
        writer.writeheader()
        writer.writerows(rows)
    return rows


def require_zero_cpu(rows: list[dict[str, int | str]], cpu: int) -> None:
    cpu_rows = [row for row in rows if row["cpu"] == cpu]
    if not cpu_rows:
        raise ValueError(f"pCPU{cpu} is absent from periodic scheduler tick snapshots")
    failures = [
        row for row in cpu_rows if int(row["count"]) != 0 or int(row["delta"]) != 0
    ]
    if failures:
        details = ", ".join(
            f"{row['snapshot']}: count={row['count']} delta={row['delta']}"
            for row in failures
        )
        raise ValueError(f"pCPU{cpu} received host periodic scheduler ticks: {details}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("log", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--require-zero-cpu", type=int, action="append", default=[])
    args = parser.parse_args()

    try:
        snapshots = parse_snapshots(args.log.read_text(errors="replace"))
        rows = write_csv(args.output, snapshots)
        for cpu in args.require_zero_cpu:
            require_zero_cpu(rows, cpu)
    except ValueError as error:
        raise SystemExit(f"error: {error}") from error


if __name__ == "__main__":
    main()
