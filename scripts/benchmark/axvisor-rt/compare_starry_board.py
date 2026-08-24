#!/usr/bin/env python3
"""Compare one orthogonal shared/partitioned StarryOS board capture pair."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Sequence


EXPECTED_METRICS = (
    "periodic_jitter",
    "dispatch_latency",
    "emulated_irq_response",
)
DIRECT_IRQ_METRIC = "virtual_timer_injection_to_guest_irq"
PRIMARY_GUEST_METRICS = ("periodic_jitter", "dispatch_latency")
COMPARABLE_CAPTURE_FIELDS = (
    "platform",
    "os",
    "workload",
    "vcpu_count",
    "iterations_per_metric",
    "sample_count",
    "warmup_iterations",
    "period_us",
    "measurement_cpu",
    "stress_cpu",
    "fifo_priority",
)
P99_NON_REGRESSION_LIMIT_PERCENT = 5.0
MAX_IMPROVEMENT_TARGET_PERCENT = 10.0


class ComparisonError(ValueError):
    """Raised when summaries cannot form an authoritative paired comparison."""


def require_object(parent: dict[str, object], key: str) -> dict[str, object]:
    value = parent.get(key)
    if not isinstance(value, dict):
        raise ComparisonError(f"summary {key} must be an object")
    return value


def require_profile(summary: dict[str, object], expected: str, label: str) -> None:
    if summary.get("schema_version") != 1:
        raise ComparisonError(f"{label} must use summary schema version 1")
    capture = require_object(summary, "capture")
    if capture.get("profile") != expected:
        raise ComparisonError(f"{label} must declare profile={expected}")


def validate_orthogonal_pair(
    shared: dict[str, object], partitioned: dict[str, object]
) -> tuple[dict[str, object], dict[str, object]]:
    require_profile(shared, "shared", "shared input")
    require_profile(partitioned, "partitioned", "partitioned input")
    shared_capture = require_object(shared, "capture")
    partitioned_capture = require_object(partitioned, "capture")
    for field in COMPARABLE_CAPTURE_FIELDS:
        if field not in shared_capture or field not in partitioned_capture:
            raise ComparisonError(f"capture comparison is missing {field}")
        if shared_capture[field] != partitioned_capture[field]:
            raise ComparisonError(
                f"capture field {field} differs between shared and partitioned"
            )

    iterations = shared_capture["iterations_per_metric"]
    if type(iterations) is not int or iterations <= 0:
        raise ComparisonError("iterations_per_metric must be a positive integer")
    expected_total = iterations * len(EXPECTED_METRICS)
    if shared_capture.get("sample_count") != expected_total:
        raise ComparisonError("shared total sample count is incomplete")
    if partitioned_capture.get("sample_count") != expected_total:
        raise ComparisonError("partitioned total sample count is incomplete")
    return shared_capture, partitioned_capture


def validate_controlled_interference(
    shared: dict[str, object], partitioned: dict[str, object]
) -> dict[str, object] | None:
    shared_noise = optional_host_noise(shared)
    partitioned_noise = optional_host_noise(partitioned)
    statuses = (shared_noise.get("status"), partitioned_noise.get("status"))
    if statuses == ("not-configured", "not-configured"):
        return None
    if statuses != ("collected", "collected"):
        raise ComparisonError("controlled interference must be collected on both sides")

    placements = (
        shared_noise.get("requested_pcpu"),
        partitioned_noise.get("requested_pcpu"),
    )
    if placements != (1, 3):
        raise ComparisonError(
            "controlled interference must use shared pCPU1 and partitioned pCPU3"
        )
    for label, noise in (("shared", shared_noise), ("partitioned", partitioned_noise)):
        requested_pcpu = noise["requested_pcpu"]
        expected_mask = 1 << requested_pcpu
        if noise.get("affinity_mask") != expected_mask:
            raise ComparisonError(f"{label} host noise has the wrong singleton affinity")
        if noise.get("observed_pcpu_mask") != expected_mask:
            raise ComparisonError(f"{label} host noise escaped its requested placement")
        if noise.get("stop_reason") != "guest-complete" or not noise.get(
            "covers_host_trace"
        ):
            raise ComparisonError(f"{label} host noise does not cover the capture")
    for field in ("max_duration_ms", "intensity"):
        if shared_noise.get(field) != partitioned_noise.get(field):
            raise ComparisonError(f"controlled interference field {field} differs")

    return {
        "implementation": shared_noise["intensity"],
        "max_duration_ms": shared_noise["max_duration_ms"],
        "shared_requested_pcpu": shared_noise["requested_pcpu"],
        "partitioned_requested_pcpu": partitioned_noise["requested_pcpu"],
        "shared_elapsed_ticks": shared_noise.get("elapsed_ticks"),
        "partitioned_elapsed_ticks": partitioned_noise.get("elapsed_ticks"),
        "placement_and_coverage_validated": True,
    }


def optional_host_noise(summary: dict[str, object]) -> dict[str, object]:
    if "host_noise" not in summary:
        return {"status": "not-configured"}
    return require_object(summary, "host_noise")


def metric_change(
    shared_value: int | float, partitioned_value: int | float
) -> dict[str, int | float | bool]:
    if (
        isinstance(shared_value, bool)
        or not isinstance(shared_value, (int, float))
        or shared_value <= 0
    ):
        raise ComparisonError("shared latency must be a positive number")
    if (
        isinstance(partitioned_value, bool)
        or not isinstance(partitioned_value, (int, float))
        or partitioned_value < 0
    ):
        raise ComparisonError("partitioned latency must be a nonnegative number")
    change = partitioned_value - shared_value
    improvement = 100.0 * (shared_value - partitioned_value) / shared_value
    return {
        "shared_ns": shared_value,
        "partitioned_ns": partitioned_value,
        "change_ns": change,
        "improvement_percent": round(improvement, 3),
    }


def compare_metric(
    name: str,
    shared: dict[str, object],
    partitioned: dict[str, object],
    expected_count: int | None,
) -> dict[str, object]:
    for label, metric in (("shared", shared), ("partitioned", partitioned)):
        if metric.get("unit") != "ns":
            raise ComparisonError(f"{label} {name} must use nanoseconds")
        count = metric.get("count")
        if type(count) is not int or count <= 0:
            raise ComparisonError(f"{label} {name} sample count must be positive")
        if expected_count is not None and count != expected_count:
            raise ComparisonError(f"{label} {name} sample count does not match capture")
    p99 = metric_change(shared.get("p99_ns"), partitioned.get("p99_ns"))
    maximum = metric_change(shared.get("max_ns"), partitioned.get("max_ns"))
    p99["within_non_regression_limit"] = (
        p99["improvement_percent"] >= -P99_NON_REGRESSION_LIMIT_PERCENT
    )
    maximum["improved"] = maximum["improvement_percent"] > 0
    maximum["meets_ten_percent_target"] = (
        maximum["improvement_percent"] >= MAX_IMPROVEMENT_TARGET_PERCENT
    )
    return {"p99": p99, "max": maximum}


def compare_summaries(
    shared: dict[str, object], partitioned: dict[str, object]
) -> dict[str, object]:
    """Validate an orthogonal pair and report signed latency improvements."""
    shared_capture, _ = validate_orthogonal_pair(shared, partitioned)
    controlled_interference = validate_controlled_interference(shared, partitioned)
    iterations = shared_capture["iterations_per_metric"]
    shared_metrics = require_object(shared, "metrics")
    partitioned_metrics = require_object(partitioned, "metrics")
    shared_metric_names = set(shared_metrics)
    partitioned_metric_names = set(partitioned_metrics)
    allowed_metric_sets = (
        set(EXPECTED_METRICS),
        {*EXPECTED_METRICS, DIRECT_IRQ_METRIC},
    )
    if shared_metric_names not in allowed_metric_sets:
        raise ComparisonError("shared summary metric set does not match the contract")
    if partitioned_metric_names != shared_metric_names:
        raise ComparisonError("shared and partitioned metric sets differ")

    metrics: dict[str, object] = {}
    metric_names = list(EXPECTED_METRICS)
    if DIRECT_IRQ_METRIC in shared_metric_names:
        metric_names.append(DIRECT_IRQ_METRIC)
    for name in metric_names:
        shared_metric = shared_metrics[name]
        partitioned_metric = partitioned_metrics[name]
        if not isinstance(shared_metric, dict) or not isinstance(partitioned_metric, dict):
            raise ComparisonError(f"metric {name} must be an object")
        metrics[name] = compare_metric(
            name,
            shared_metric,
            partitioned_metric,
            iterations if name in EXPECTED_METRICS else None,
        )

    shared_input = require_object(shared, "input")
    partitioned_input = require_object(partitioned, "input")
    host_accounting_collected = all(
        require_object(summary, "host_pcpu_accounting").get("status") == "collected"
        for summary in (shared, partitioned)
    )
    direct_irq_collected = DIRECT_IRQ_METRIC in metrics
    primary_p99_within_limit = all(
        metrics[name]["p99"]["within_non_regression_limit"]
        for name in PRIMARY_GUEST_METRICS
    )
    primary_max_improved = all(
        metrics[name]["max"]["improved"] for name in PRIMARY_GUEST_METRICS
    )
    direct_irq_max_improved = (
        metrics[DIRECT_IRQ_METRIC]["max"]["improved"]
        if direct_irq_collected
        else False
    )
    return {
        "schema_version": 1,
        "pair": {
            "workload": shared_capture["workload"],
            "iterations_per_metric": iterations,
            "shared_raw": {
                "path": shared_input.get("path"),
                "sha256": shared_input.get("sha256"),
            },
            "partitioned_raw": {
                "path": partitioned_input.get("path"),
                "sha256": partitioned_input.get("sha256"),
            },
            "controlled_interference": controlled_interference,
        },
        "sign_convention": "positive improvement_percent means partitioned is lower",
        "thresholds": {
            "p99_non_regression_limit_percent": P99_NON_REGRESSION_LIMIT_PERCENT,
            "max_improvement_target_percent": MAX_IMPROVEMENT_TARGET_PERCENT,
        },
        "metrics": metrics,
        "assessment": {
            "primary_guest_p99_within_non_regression_limit": primary_p99_within_limit,
            "primary_guest_max_improved_in_this_pair": primary_max_improved,
            "direct_irq_max_improved_in_this_pair": direct_irq_max_improved,
            "m2_exit_gate_met": False,
            "reason": (
                "one pair cannot satisfy the required five-pair shared/partitioned matrix"
                if direct_irq_collected and host_accounting_collected
                else "direct IRQ latency and independent host accounting are incomplete"
            ),
        },
        "scope": {
            "direct_irq_latency_collected": direct_irq_collected,
            "host_pcpu_accounting_collected": host_accounting_collected,
            "controlled_interference_collected": controlled_interference is not None,
            "timerfd_metric_is_proxy": True,
        },
    }


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("shared", type=Path, help="shared summary JSON")
    parser.add_argument("partitioned", type=Path, help="partitioned summary JSON")
    parser.add_argument("--output", type=Path, help="comparison JSON; defaults to stdout")
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        shared = json.loads(args.shared.read_text(encoding="utf-8"))
        partitioned = json.loads(args.partitioned.read_text(encoding="utf-8"))
        if not isinstance(shared, dict) or not isinstance(partitioned, dict):
            raise ComparisonError("summaries must be JSON objects")
        result = compare_summaries(shared, partitioned)
    except (ComparisonError, OSError, json.JSONDecodeError) as error:
        print(f"StarryOS board comparison failed: {error}", file=sys.stderr)
        return 2

    rendered = json.dumps(result, indent=2, sort_keys=True, allow_nan=False) + "\n"
    if args.output is None:
        sys.stdout.write(rendered)
    else:
        args.output.write_text(rendered, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
