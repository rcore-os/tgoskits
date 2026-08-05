"""Derive disclosed first-cycle and post-first-cycle RKNN deadline evidence."""

from __future__ import annotations

import statistics
from pathlib import Path
from typing import TypedDict

try:
    import analyze_board as board_analysis
except ModuleNotFoundError:
    from competition.ivc import analyze_board as board_analysis


class DeadlineAnalysisError(ValueError):
    """Raised when raw evidence cannot support a deadline partition."""


class OverallRecord(TypedDict):
    sample_count: int
    deadline_misses: int


class FirstCycleRecord(TypedDict):
    sequence: int
    deadline_miss: bool
    full_loop_us: int
    pre_send_us: int
    transport_us: int
    rknn_device_us: int
    rknn_wall_ns: int


class PostFirstCycleRecord(TypedDict):
    first_sequence: int
    sample_count: int
    deadline_misses: int
    full_loop_p99_us: int
    full_loop_max_us: int
    pre_send_p99_us: int
    pre_send_max_us: int
    transport_p99_us: int
    transport_max_us: int
    rknn_device_p99_us: int
    rknn_device_max_us: int
    rknn_wall_p99_ns: int
    rknn_wall_max_ns: int


class RunDeadlinePartition(TypedDict):
    threshold_us: int
    overall: OverallRecord
    first_cycle: FirstCycleRecord
    post_first_cycle: PostFirstCycleRecord


def analyze_run(
    raw_path: Path,
    rknn_path: Path,
    expected_count: int,
    controller: dict[str, object],
    rknn_summary: dict[str, object],
    period_ms: int = 100,
) -> RunDeadlinePartition:
    """Validate raw timing evidence and partition the disclosed first cycle."""
    if expected_count < 2:
        raise DeadlineAnalysisError("deadline partition requires at least two samples")
    if period_ms <= 0:
        raise DeadlineAnalysisError("control period must be positive")
    try:
        raw_rows = board_analysis.read_raw_rows(raw_path, expected_count)
        rknn_rows = board_analysis.read_rknn_rows(rknn_path, expected_count)
        all_metrics = board_analysis.derive_raw_metrics(raw_rows, period_ms)
        post_metrics = board_analysis.derive_raw_metrics(raw_rows[1:], period_ms)
        board_analysis.cross_check_raw_metrics(controller, all_metrics)
    except (OSError, board_analysis.AnalysisError) as error:
        raise DeadlineAnalysisError(str(error)) from error

    for raw_row, rknn_row in zip(raw_rows, rknn_rows, strict=True):
        if raw_row["command_actuator_permille"] != rknn_row["actuator_permille"]:
            raise DeadlineAnalysisError(
                "RKNN actuator does not match the controller raw CSV"
            )

    all_deadline_misses = _required_metric(all_metrics, "deadline_misses")
    if _required_integer(controller, "deadline_misses", "controller") != all_deadline_misses:
        raise DeadlineAnalysisError("controller deadline misses do not match raw CSV")
    _cross_check_rknn_summary(rknn_summary, rknn_rows)

    threshold_us = period_ms * 1_000
    first_raw = raw_rows[0]
    first_rknn = rknn_rows[0]
    post_device_times = sorted(row["device_us"] for row in rknn_rows[1:])
    post_wall_times = sorted(row["wall_ns"] for row in rknn_rows[1:])
    return {
        "threshold_us": threshold_us,
        "overall": {
            "sample_count": len(raw_rows),
            "deadline_misses": all_deadline_misses,
        },
        "first_cycle": {
            "sequence": first_raw["sequence"],
            "deadline_miss": first_raw["full_loop_us"] > threshold_us,
            "full_loop_us": first_raw["full_loop_us"],
            "pre_send_us": first_raw["pre_send_us"],
            "transport_us": first_raw["transport_us"],
            "rknn_device_us": first_rknn["device_us"],
            "rknn_wall_ns": first_rknn["wall_ns"],
        },
        "post_first_cycle": {
            "first_sequence": raw_rows[1]["sequence"],
            "sample_count": len(raw_rows) - 1,
            "deadline_misses": _required_metric(post_metrics, "deadline_misses"),
            "full_loop_p99_us": _required_metric(post_metrics, "full_loop_p99_us"),
            "full_loop_max_us": _required_metric(post_metrics, "full_loop_max_us"),
            "pre_send_p99_us": _required_metric(post_metrics, "pre_send_p99_us"),
            "pre_send_max_us": _required_metric(post_metrics, "pre_send_max_us"),
            "transport_p99_us": _required_metric(post_metrics, "transport_p99_us"),
            "transport_max_us": _required_metric(post_metrics, "transport_max_us"),
            "rknn_device_p99_us": board_analysis.percentile(post_device_times, 99),
            "rknn_device_max_us": post_device_times[-1],
            "rknn_wall_p99_ns": board_analysis.percentile(post_wall_times, 99),
            "rknn_wall_max_ns": post_wall_times[-1],
        },
    }


def aggregate_runs(partitions: list[RunDeadlinePartition]) -> dict[str, object]:
    """Aggregate per-run partitions without removing first cycles from totals."""
    if not partitions:
        raise DeadlineAnalysisError("deadline campaign has no runs")
    thresholds = {partition["threshold_us"] for partition in partitions}
    if len(thresholds) != 1:
        raise DeadlineAnalysisError("deadline campaign mixes control periods")

    first_cycles = [partition["first_cycle"] for partition in partitions]
    post_cycles = [partition["post_first_cycle"] for partition in partitions]
    return {
        "threshold_us": thresholds.pop(),
        "interpretation": (
            "overall metrics retain every measured cycle; the post-first-cycle "
            "partition is descriptive and does not erase cold-start misses"
        ),
        "overall": {
            "sample_count": sum(
                partition["overall"]["sample_count"] for partition in partitions
            ),
            "deadline_misses": sum(
                partition["overall"]["deadline_misses"] for partition in partitions
            ),
        },
        "first_cycle": {
            "sample_count": len(first_cycles),
            "deadline_misses": sum(cycle["deadline_miss"] for cycle in first_cycles),
            "full_loop_us": _describe(
                [cycle["full_loop_us"] for cycle in first_cycles]
            ),
            "pre_send_us": _describe(
                [cycle["pre_send_us"] for cycle in first_cycles]
            ),
            "transport_us": _describe(
                [cycle["transport_us"] for cycle in first_cycles]
            ),
            "rknn_device_us": _describe(
                [cycle["rknn_device_us"] for cycle in first_cycles]
            ),
            "rknn_wall_ns": _describe(
                [cycle["rknn_wall_ns"] for cycle in first_cycles]
            ),
        },
        "post_first_cycle": {
            "sample_count": sum(cycle["sample_count"] for cycle in post_cycles),
            "deadline_misses": sum(cycle["deadline_misses"] for cycle in post_cycles),
            **{
                metric: _describe([cycle[metric] for cycle in post_cycles])
                for metric in (
                    "full_loop_p99_us",
                    "full_loop_max_us",
                    "pre_send_p99_us",
                    "pre_send_max_us",
                    "transport_p99_us",
                    "transport_max_us",
                    "rknn_device_p99_us",
                    "rknn_device_max_us",
                    "rknn_wall_p99_ns",
                    "rknn_wall_max_ns",
                )
            },
        },
    }


def _cross_check_rknn_summary(
    summary: dict[str, object], rows: list[dict[str, int]]
) -> None:
    device_times = sorted(row["device_us"] for row in rows)
    wall_times = sorted(row["wall_ns"] for row in rows)
    expected = {
        "sample_count": len(rows),
        "positive_device_times": len(rows),
        "actuator_matches": len(rows),
        "device_p50_us": board_analysis.percentile(device_times, 50),
        "device_p99_us": board_analysis.percentile(device_times, 99),
        "device_max_us": device_times[-1],
        "wall_p50_ns": board_analysis.percentile(wall_times, 50),
        "wall_p99_ns": board_analysis.percentile(wall_times, 99),
        "wall_max_ns": wall_times[-1],
    }
    for field, expected_value in expected.items():
        if _required_integer(summary, field, "RKNN summary") != expected_value:
            raise DeadlineAnalysisError(f"RKNN summary {field} does not match CSV")


def _required_integer(parent: dict[str, object], key: str, label: str) -> int:
    value = parent.get(key)
    if isinstance(value, bool) or not isinstance(value, int):
        raise DeadlineAnalysisError(f"{label} {key} must be an integer")
    return value


def _required_metric(metrics: dict[str, object], key: str) -> int:
    return _required_integer(metrics, key, "derived raw metrics")


def _describe(values: list[int]) -> dict[str, int | float | list[int]]:
    return {
        "values": values,
        "min": min(values),
        "median": statistics.median(values),
        "max": max(values),
        "range": max(values) - min(values),
    }
