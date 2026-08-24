#!/usr/bin/env python3
"""Strictly validate and summarize one hybrid-topology latency capture."""

import argparse
import csv
import math
import re
import statistics
from pathlib import Path


ROW = re.compile(r"^(\d+),(-?\d+),(-?\d+),(-?\d+),(-?\d+)$")


def main() -> int:
    args = parse_arguments()
    text = normalize_capture(args.capture)
    rows, activity = parse_capture(
        text, args.samples, args.idle, args.timer_frequency_hz
    )
    write_analysis(args.output, rows, activity, args.timer_frequency_hz)
    return 0


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("capture", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=300)
    parser.add_argument("--timer-frequency-hz", type=int, default=24_000_000)
    parser.add_argument("--idle", action="store_true")
    args = parser.parse_args()
    if args.samples <= 0:
        raise ValueError("--samples must be positive")
    if args.timer_frequency_hz <= 0:
        raise ValueError("--timer-frequency-hz must be positive")
    return args


def normalize_capture(capture: Path) -> str:
    return capture.read_text(encoding="utf-8", errors="strict").replace(
        "\r\n", "\n"
    ).replace("\r", "\n")


def parse_capture(
    text: str, samples: int, idle: bool, timer_frequency_hz: int = 24_000_000
) -> tuple[list[tuple[int, int, int, int, int]], tuple[int, int, int]]:
    ready_marker = (
        f"PERIODIC LATENCY READY frequency_hz={timer_frequency_hz} "
        f"period_ms=10 samples={samples}\n"
    )
    ready = text.index(ready_marker)
    start_marker = "PERIODIC LATENCY START\n"
    start = text.index(start_marker, ready + len(ready_marker)) + len(start_marker)
    completion = re.compile(
        rf"PERIODIC LATENCY SAMPLING COMPLETE samples={samples} "
        r"controls=(\d+) statuses=(\d+) heartbeats=(\d+)"
    ).search(text, start)
    if completion is None:
        raise ValueError("missing strict sampling-complete telemetry marker")
    if text[start : completion.start()].strip():
        raise ValueError("UART output occurred inside the real-time sampling window")

    activity = tuple(map(int, completion.groups()))
    validate_activity(activity, idle)
    rows = parse_csv_rows(text, completion.end(), samples)
    validate_rows(rows, samples)
    return rows, activity


def validate_activity(activity: tuple[int, int, int], idle: bool) -> None:
    controls, statuses, _heartbeats = activity
    if idle and (controls != 0 or statuses != 0):
        raise ValueError(f"idle window had controls={controls}, statuses={statuses}")
    if not idle and (controls == 0 or statuses == 0):
        raise ValueError(f"stress window had controls={controls}, statuses={statuses}")


def parse_csv_rows(
    text: str, completion_end: int, samples: int
) -> list[tuple[int, int, int, int, int]]:
    header = "sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns\n"
    csv_start = text.index(header, completion_end) + len(header)
    csv_end = text.index(f"PERIODIC LATENCY COMPLETE samples={samples}", csv_start)
    lines = text[csv_start:csv_end].splitlines()
    if len(lines) != samples:
        raise ValueError(f"expected {samples} CSV lines, found {len(lines)}")

    rows = []
    for line_number, line in enumerate(lines, start=1):
        match = ROW.fullmatch(line)
        if match is None:
            raise ValueError(f"invalid CSV line {line_number}: {line!r}")
        rows.append(tuple(map(int, match.groups())))
    return rows


def validate_rows(rows: list[tuple[int, int, int, int, int]], samples: int) -> None:
    if [row[0] for row in rows] != list(range(samples)):
        raise ValueError(f"sample sequence is not contiguous 0..{samples - 1}")
    if any(row[3] - row[2] != row[4] for row in rows):
        raise ValueError("actual/deadline/jitter arithmetic is inconsistent")
    deadlines = [row[2] for row in rows]
    if any(now - before != 10_000_000 for before, now in zip(deadlines, deadlines[1:])):
        raise ValueError("deadline sequence is not a contiguous 10 ms series")


def write_analysis(
    output: Path,
    rows: list[tuple[int, int, int, int, int]],
    activity: tuple[int, int, int],
    timer_frequency_hz: int,
) -> None:
    output.mkdir(parents=True, exist_ok=True)
    with (output / "periodic.csv").open("w", newline="", encoding="utf-8") as stream:
        writer = csv.writer(stream)
        writer.writerow(("sequence", "timestamp_ns", "deadline_ns", "actual_ns", "jitter_ns"))
        writer.writerows(rows)

    with (output / "spikes-over-3ms.csv").open(
        "w", newline="", encoding="utf-8"
    ) as stream:
        writer = csv.writer(stream)
        writer.writerow(("sequence", "timestamp_ns", "deadline_ns", "actual_ns", "jitter_ns"))
        writer.writerows(row for row in rows if row[4] > 3_000_000)

    jitter = [row[4] for row in rows]
    controls, statuses, heartbeats = activity
    fields = {
        "samples": len(rows),
        "timer_frequency_hz": timer_frequency_hz,
        "period_ns": 10_000_000,
        "min_ns": min(jitter),
        "mean_ns": f"{statistics.mean(jitter):.2f}",
        "stddev_ns": f"{statistics.pstdev(jitter):.2f}",
        "p50_ns": percentile(jitter, 0.50),
        "p90_ns": percentile(jitter, 0.90),
        "p95_ns": percentile(jitter, 0.95),
        "p99_ns": percentile(jitter, 0.99),
        "p99_9_ns": percentile(jitter, 0.999),
        "max_ns": max(jitter),
        "max_sequence": max(rows, key=lambda row: row[4])[0],
        "over_500us": sum(value > 500_000 for value in jitter),
        "over_1ms": sum(value > 1_000_000 for value in jitter),
        "over_2ms": sum(value > 2_000_000 for value in jitter),
        "over_3ms": sum(value > 3_000_000 for value in jitter),
        "over_10ms": sum(value > 10_000_000 for value in jitter),
        "deadline_misses": sum(value > 10_000_000 for value in jitter),
        "controls_during_sampling": controls,
        "statuses_during_sampling": statuses,
        "heartbeats_during_sampling": heartbeats,
        "sampling_window_uart_lines": 0,
        "serial_interleave_repairs": 0,
    }
    summary = "".join(f"{key}={value}\n" for key, value in fields.items())
    (output / "summary.txt").write_text(summary, encoding="utf-8")
    print(summary, end="")


def percentile(values: list[int], fraction: float) -> int:
    ordered = sorted(values)
    return ordered[math.ceil(fraction * len(ordered)) - 1]


if __name__ == "__main__":
    raise SystemExit(main())
