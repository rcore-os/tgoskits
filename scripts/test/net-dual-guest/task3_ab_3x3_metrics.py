#!/usr/bin/env python3
"""Aggregate three physical Task-3 manual/YOLO A/B repetitions."""

from __future__ import annotations

import argparse
import csv
import json
import math
import statistics
from collections.abc import Iterable
from dataclasses import asdict
from pathlib import Path

from task3_ab_metrics import Run, parse_path, summarize, verify_comparable


RUN_COUNT = 3


def main() -> None:
    args = parse_arguments()
    manual_runs = [parse_path(path, "manual") for path in args.manual]
    yolo_runs = [parse_path(path, "yolo") for path in args.yolo]
    verify_repetitions(manual_runs, yolo_runs)

    args.out_dir.mkdir(parents=True, exist_ok=True)
    write_sample_csvs(args.out_dir, manual_runs, yolo_runs)
    per_run = summarize_repetitions(manual_runs, yolo_runs)
    aggregate = aggregate_repetitions(manual_runs, yolo_runs)
    write_dict_rows(args.out_dir / "per-run-summary.csv", per_run)
    write_dict_rows(args.out_dir / "summary.csv", aggregate)
    (args.out_dir / "summary.json").write_text(
        json.dumps(aggregate, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    print(f"PASS: Task-3 physical 3+3 metrics written to {args.out_dir}")


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manual", required=True, action="append", type=Path)
    parser.add_argument("--yolo", required=True, action="append", type=Path)
    parser.add_argument("--out-dir", required=True, type=Path)
    args = parser.parse_args()
    if len(args.manual) != RUN_COUNT or len(args.yolo) != RUN_COUNT:
        parser.error(
            f"expected exactly {RUN_COUNT} --manual and {RUN_COUNT} --yolo logs"
        )
    return args


def verify_repetitions(manual_runs: list[Run], yolo_runs: list[Run]) -> None:
    if len(manual_runs) != RUN_COUNT or len(yolo_runs) != RUN_COUNT:
        raise ValueError(f"expected exactly {RUN_COUNT} manual and YOLO runs")
    reference = manual_runs[0]
    for run in (*manual_runs[1:], *yolo_runs):
        verify_comparable(reference, run)


def write_sample_csvs(
    out_dir: Path, manual_runs: list[Run], yolo_runs: list[Run]
) -> None:
    for mode_runs in (manual_runs, yolo_runs):
        for run_index, run in enumerate(mode_runs, start=1):
            path = out_dir / f"{run.mode}-{run_index}.csv"
            with path.open("w", newline="", encoding="utf-8") as stream:
                fieldnames = ["run", *asdict(run.samples[0])]
                writer = csv.DictWriter(stream, fieldnames=fieldnames)
                writer.writeheader()
                writer.writerows(
                    {"run": run_index, **asdict(sample)} for sample in run.samples
                )


def summarize_repetitions(
    manual_runs: list[Run], yolo_runs: list[Run]
) -> list[dict[str, int | float | None | str]]:
    rows: list[dict[str, int | float | None | str]] = []
    for mode_runs in (manual_runs, yolo_runs):
        for run_index, run in enumerate(mode_runs, start=1):
            rows.append({"run": run_index, **summarize(run)})
    return rows


def aggregate_repetitions(
    manual_runs: list[Run], yolo_runs: list[Run]
) -> list[dict[str, int | float | None | str]]:
    return [aggregate_mode(runs) for runs in (manual_runs, yolo_runs)]


def aggregate_mode(runs: list[Run]) -> dict[str, int | float | None | str]:
    combined = Run(
        mode=runs[0].mode,
        samples=[sample for run in runs for sample in run.samples],
        declared_samples=sum(run.declared_samples for run in runs),
    )
    base = summarize(combined)
    rtts = [
        sample.rtt_ms
        for sample in combined.samples
        if sample.rtt_ms is not None
    ]
    inference = [
        sample.infer_us
        for sample in combined.samples
        if sample.infer_us is not None
    ]
    run_summaries = [summarize(run) for run in runs]
    return {
        "mode": combined.mode,
        "runs": len(runs),
        **{key: value for key, value in base.items() if key != "mode"},
        "p50_rtt_ms": nearest_rank(rtts, 0.50),
        "p95_rtt_ms": nearest_rank(rtts, 0.95),
        "p99_rtt_ms": nearest_rank(rtts, 0.99),
        "p50_infer_us": nearest_rank(inference, 0.50),
        "p95_infer_us": nearest_rank(inference, 0.95),
        "p99_infer_us": nearest_rank(inference, 0.99),
        "max_infer_us": max(inference) if inference else None,
        "run_mean_rtt_stddev_ms": sample_stddev(
            summary["mean_rtt_ms"] for summary in run_summaries
        ),
        "run_mean_infer_stddev_us": sample_stddev(
            summary["mean_infer_us"] for summary in run_summaries
        ),
    }


def nearest_rank(values: list[int], quantile: float) -> int | None:
    if not values:
        return None
    ordered = sorted(values)
    rank = max(1, math.ceil(quantile * len(ordered)))
    return ordered[rank - 1]


def sample_stddev(values: Iterable[int | float | None]) -> float | None:
    numeric = [value for value in values if value is not None]
    return statistics.stdev(numeric) if len(numeric) >= 2 else None


def write_dict_rows(path: Path, rows: list[dict[str, object]]) -> None:
    with path.open("w", newline="", encoding="utf-8") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


if __name__ == "__main__":
    main()
