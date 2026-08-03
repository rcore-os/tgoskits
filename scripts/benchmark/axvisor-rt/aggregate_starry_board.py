#!/usr/bin/env python3
"""Aggregate the frozen five-pair StarryOS board latency campaign."""

from __future__ import annotations

import argparse
import json
import re
import statistics
import sys
from pathlib import Path
from typing import Sequence


EXPECTED_PAIR_COUNT = 5
EXPECTED_METRICS = (
    "periodic_jitter",
    "dispatch_latency",
    "emulated_irq_response",
    "virtual_timer_injection_to_guest_irq",
)
PRIMARY_GUEST_METRICS = ("periodic_jitter", "dispatch_latency")
DIRECT_IRQ_METRIC = "virtual_timer_injection_to_guest_irq"
P99_NON_REGRESSION_LIMIT_PERCENT = 5.0
MAX_IMPROVEMENT_TARGET_PERCENT = 10.0
DIRECT_IRQ_MAX_REQUIRED_PAIRS = 4
SOAK_ITERATIONS_PER_METRIC = 10_000
SOAK_PERIOD_US = 90_000
SOAK_MINIMUM_DURATION_SECONDS = 1_800
SOAK_HOST_NOISE_MAX_DURATION_MS = 3_600_000
SHA256_PATTERN = re.compile(r"[0-9a-f]{64}")


class AggregationError(ValueError):
    """Raised when pair comparisons cannot form an authoritative campaign."""


def require_object(parent: dict[str, object], key: str, label: str) -> dict[str, object]:
    value = parent.get(key)
    if not isinstance(value, dict):
        raise AggregationError(f"{label} {key} must be an object")
    return value


def require_number(parent: dict[str, object], key: str, label: str) -> int | float:
    value = parent.get(key)
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise AggregationError(f"{label} {key} must be a number")
    return value


def improvement_percent(shared_ns: int | float, partitioned_ns: int | float) -> float:
    if shared_ns <= 0 or partitioned_ns < 0:
        raise AggregationError("latencies must be positive shared/nonnegative partitioned")
    return round(100.0 * (shared_ns - partitioned_ns) / shared_ns, 3)


def validate_raw(
    pair: dict[str, object], key: str, label: str, observed_hashes: set[str]
) -> dict[str, str]:
    raw = require_object(pair, key, label)
    path = raw.get("path")
    sha256 = raw.get("sha256")
    if not isinstance(path, str) or not path:
        raise AggregationError(f"{label} {key} path must be nonempty")
    if not isinstance(sha256, str) or SHA256_PATTERN.fullmatch(sha256) is None:
        raise AggregationError(f"{label} {key} must contain a lowercase raw SHA-256")
    if sha256 in observed_hashes:
        raise AggregationError(f"{label} reuses a raw SHA-256")
    observed_hashes.add(sha256)
    return {"path": path, "sha256": sha256}


def validate_metric(
    metric: dict[str, object], label: str
) -> dict[str, dict[str, int | float | bool]]:
    validated: dict[str, dict[str, int | float | bool]] = {}
    for statistic in ("p99", "max"):
        values = require_object(metric, statistic, label)
        shared_ns = require_number(values, "shared_ns", f"{label} {statistic}")
        partitioned_ns = require_number(
            values, "partitioned_ns", f"{label} {statistic}"
        )
        improvement = improvement_percent(shared_ns, partitioned_ns)
        reported = require_number(
            values, "improvement_percent", f"{label} {statistic}"
        )
        if abs(reported - improvement) > 0.001:
            raise AggregationError(
                f"{label} {statistic} improvement does not match raw values"
            )
        result: dict[str, int | float | bool] = {
            "shared_ns": shared_ns,
            "partitioned_ns": partitioned_ns,
            "improvement_percent": improvement,
        }
        if statistic == "p99":
            result["within_non_regression_limit"] = (
                improvement >= -P99_NON_REGRESSION_LIMIT_PERCENT
            )
        else:
            result["improved"] = improvement > 0
            result["meets_ten_percent_target"] = (
                improvement >= MAX_IMPROVEMENT_TARGET_PERCENT
            )
        validated[statistic] = result
    return validated


def validate_soak_summary(
    summary: dict[str, object], profile: str, observed_hashes: set[str]
) -> dict[str, object]:
    """Validate one lossless, at-least-30-minute controlled-interference soak."""
    label = f"{profile} soak"
    if summary.get("schema_version") != 1:
        raise AggregationError(f"{label} must use summary schema version 1")

    capture = require_object(summary, "capture", label)
    expected_capture = {
        "profile": profile,
        "workload": "idle",
        "iterations_per_metric": SOAK_ITERATIONS_PER_METRIC,
        "period_us": SOAK_PERIOD_US,
        "sample_count": SOAK_ITERATIONS_PER_METRIC * 3,
        "vcpu_count": 2,
    }
    for key, expected in expected_capture.items():
        if capture.get(key) != expected:
            raise AggregationError(f"{label} {key} differs from the soak contract")

    raw = require_object(summary, "input", label)
    if raw.get("snapshot_filesystem_state") != "clean":
        raise AggregationError(f"{label} snapshot filesystem must be clean")
    raw_identity = validate_raw({"input": raw}, "input", label, observed_hashes)

    profile_contract = require_object(summary, "profile_contract", label)
    expected_vm_config = (
        "scripts/benchmark/axvisor-rt/config/"
        f"starry-orangepi-5-plus-smp2-soak-{profile}.toml"
    )
    expected_dedicated = profile == "partitioned"
    if profile_contract.get("dedicated_cpus") is not expected_dedicated:
        raise AggregationError(f"{label} dedicated_cpus does not match its profile")
    if profile_contract.get("phys_cpu_sets") != ["0x2", "0x4"]:
        raise AggregationError(f"{label} vCPU placement differs from the formal matrix")
    if profile_contract.get("vm_config") != expected_vm_config:
        raise AggregationError(f"{label} does not use its soak VM config")

    noise = require_object(summary, "host_noise", label)
    requested_pcpu = 1 if profile == "shared" else 3
    expected_mask = 1 << requested_pcpu
    exact_noise = {
        "requested_pcpu": requested_pcpu,
        "affinity_mask": expected_mask,
        "observed_pcpu_mask": expected_mask,
        "max_duration_ms": SOAK_HOST_NOISE_MAX_DURATION_MS,
        "stop_reason": "guest-complete",
        "intensity": "busy-loop",
        "covers_host_trace": True,
        "status": "collected",
    }
    for key, expected in exact_noise.items():
        if noise.get(key) != expected:
            raise AggregationError(f"{label} host-noise {key} is invalid")
    elapsed_ns = require_number(noise, "elapsed_ns", f"{label} host-noise")
    if elapsed_ns < SOAK_MINIMUM_DURATION_SECONDS * 1_000_000_000:
        raise AggregationError(f"{label} does not cover the required 30-minute window")
    if require_number(noise, "loop_iterations", f"{label} host-noise") <= 0:
        raise AggregationError(f"{label} host-noise loop must execute")

    direct = require_object(summary, "direct_irq_trace", label)
    lossless = require_object(direct, "lossless", label)
    record_counts: dict[str, int | float] = {}
    for side in ("host", "guest"):
        counters = require_object(lossless, side, f"{label} lossless")
        records = require_number(counters, "records", f"{label} {side} trace")
        if records <= 0:
            raise AggregationError(f"{label} {side} trace must contain records")
        record_counts[side] = records
        for key in (
            "dropped",
            "incomplete",
            "failed_injections",
            "unowned_virtual_timer_irqs",
            "counter_frequency_mismatches",
        ):
            if require_number(counters, key, f"{label} {side} trace") != 0:
                raise AggregationError(f"{label} {side} trace reports {key}")
    pairing = require_object(direct, "pairing", label)
    pair_count = require_number(pairing, "pair_count", f"{label} direct IRQ")
    if pair_count <= 0 or pair_count > min(record_counts.values()):
        raise AggregationError(f"{label} direct IRQ pair count is invalid")

    trace_inputs = require_object(direct, "inputs", label)
    trace_identities: dict[str, dict[str, str]] = {}
    for side in ("host", "guest"):
        trace_identities[side] = validate_raw(
            {side: require_object(trace_inputs, side, f"{label} trace inputs")},
            side,
            f"{label} trace",
            observed_hashes,
        )

    return {
        "profile": profile,
        "raw": raw_identity,
        "trace_inputs": trace_identities,
        "elapsed_ns": elapsed_ns,
        "elapsed_seconds": round(elapsed_ns / 1_000_000_000, 3),
        "host_records": record_counts["host"],
        "guest_records": record_counts["guest"],
        "direct_irq_pair_count": pair_count,
        "requested_pcpu": requested_pcpu,
        "observed_pcpu_mask": expected_mask,
    }


def aggregate_statistic(
    pairs: list[dict[str, dict[str, int | float | bool]]], statistic: str
) -> dict[str, object]:
    values = [pair[statistic] for pair in pairs]
    improvements = [float(value["improvement_percent"]) for value in values]
    shared_worst = max(float(value["shared_ns"]) for value in values)
    partitioned_worst = max(float(value["partitioned_ns"]) for value in values)
    worst_improvement = improvement_percent(shared_worst, partitioned_worst)
    result: dict[str, object] = {
        "pair_improvement_percent": improvements,
        "minimum_improvement_percent": min(improvements),
        "median_improvement_percent": round(statistics.median(improvements), 3),
        "maximum_improvement_percent": max(improvements),
        "shared_worst_of_runs_ns": shared_worst,
        "partitioned_worst_of_runs_ns": partitioned_worst,
        "worst_of_runs_improvement_percent": worst_improvement,
    }
    if statistic == "p99":
        result["non_regression_pair_count"] = sum(
            improvement >= -P99_NON_REGRESSION_LIMIT_PERCENT
            for improvement in improvements
        )
    else:
        result["improved_pair_count"] = sum(
            improvement > 0 for improvement in improvements
        )
        result["target_pair_count"] = sum(
            improvement >= MAX_IMPROVEMENT_TARGET_PERCENT
            for improvement in improvements
        )
        result["worst_of_runs_meets_ten_percent_target"] = (
            worst_improvement >= MAX_IMPROVEMENT_TARGET_PERCENT
        )
    return result


def aggregate_comparisons(
    comparisons: Sequence[dict[str, object]],
    soak_summaries: dict[str, dict[str, object]] | None = None,
) -> dict[str, object]:
    """Validate and aggregate exactly five orthogonal pair comparisons."""
    if len(comparisons) != EXPECTED_PAIR_COUNT:
        raise AggregationError(
            f"campaign must contain exactly {EXPECTED_PAIR_COUNT} pair comparisons"
        )

    expected_contract: tuple[object, ...] | None = None
    observed_hashes: set[str] = set()
    inputs: list[dict[str, object]] = []
    metric_pairs: dict[str, list[dict[str, dict[str, int | float | bool]]]] = {
        name: [] for name in EXPECTED_METRICS
    }

    for index, comparison in enumerate(comparisons, start=1):
        label = f"pair {index}"
        if comparison.get("schema_version") != 1:
            raise AggregationError(f"{label} must use comparison schema version 1")
        pair = require_object(comparison, "pair", label)
        thresholds = require_object(comparison, "thresholds", label)
        if thresholds != {
            "p99_non_regression_limit_percent": P99_NON_REGRESSION_LIMIT_PERCENT,
            "max_improvement_target_percent": MAX_IMPROVEMENT_TARGET_PERCENT,
        }:
            raise AggregationError(f"{label} thresholds differ from the frozen contract")
        controlled = require_object(pair, "controlled_interference", label)
        contract = (
            pair.get("workload"),
            pair.get("iterations_per_metric"),
            controlled.get("implementation"),
            controlled.get("max_duration_ms"),
            controlled.get("shared_requested_pcpu"),
            controlled.get("partitioned_requested_pcpu"),
        )
        if expected_contract is None:
            expected_contract = contract
        elif contract != expected_contract:
            raise AggregationError(f"{label} differs from the frozen capture contract")
        if pair.get("workload") != "idle" or pair.get("iterations_per_metric") != 10_000:
            raise AggregationError(f"{label} must be the formal idle 10k capture")
        if controlled.get("placement_and_coverage_validated") is not True:
            raise AggregationError(f"{label} did not validate host-noise placement")
        if contract[2:] != ("busy-loop", 600_000, 1, 3):
            raise AggregationError(f"{label} controlled interference is not formal")

        scope = require_object(comparison, "scope", label)
        for field in (
            "direct_irq_latency_collected",
            "host_pcpu_accounting_collected",
            "controlled_interference_collected",
        ):
            if scope.get(field) is not True:
                raise AggregationError(f"{label} scope is missing {field}")

        shared_raw = validate_raw(pair, "shared_raw", label, observed_hashes)
        partitioned_raw = validate_raw(
            pair, "partitioned_raw", label, observed_hashes
        )
        inputs.append(
            {"pair": index, "shared_raw": shared_raw, "partitioned_raw": partitioned_raw}
        )

        metrics = require_object(comparison, "metrics", label)
        if set(metrics) != set(EXPECTED_METRICS):
            raise AggregationError(f"{label} metric set differs from the formal contract")
        for name in EXPECTED_METRICS:
            metric = metrics[name]
            if not isinstance(metric, dict):
                raise AggregationError(f"{label} metric {name} must be an object")
            metric_pairs[name].append(validate_metric(metric, f"{label} {name}"))

    metrics = {
        name: {
            "p99": aggregate_statistic(pairs, "p99"),
            "max": aggregate_statistic(pairs, "max"),
        }
        for name, pairs in metric_pairs.items()
    }
    primary_p99_pass = all(
        metrics[name]["p99"]["non_regression_pair_count"] == EXPECTED_PAIR_COUNT
        for name in PRIMARY_GUEST_METRICS
    )
    primary_max_direction_pass = all(
        metrics[name]["max"]["improved_pair_count"] >= DIRECT_IRQ_MAX_REQUIRED_PAIRS
        for name in PRIMARY_GUEST_METRICS
    )
    primary_worst_pass = all(
        metrics[name]["max"]["worst_of_runs_meets_ten_percent_target"]
        for name in PRIMARY_GUEST_METRICS
    )
    direct_p99_pass = (
        metrics[DIRECT_IRQ_METRIC]["p99"]["non_regression_pair_count"]
        == EXPECTED_PAIR_COUNT
    )
    direct_max_target_pairs = metrics[DIRECT_IRQ_METRIC]["max"]["target_pair_count"]
    direct_max_direction_pass = (
        direct_max_target_pairs >= DIRECT_IRQ_MAX_REQUIRED_PAIRS
    )
    direct_worst_pass = metrics[DIRECT_IRQ_METRIC]["max"][
        "worst_of_runs_meets_ten_percent_target"
    ]
    matrix_gate_met = all(
        (
            primary_p99_pass,
            primary_max_direction_pass,
            primary_worst_pass,
            direct_p99_pass,
            direct_max_direction_pass,
            direct_worst_pass,
        )
    )

    soak: dict[str, object] | None = None
    soak_gate_met = False
    if soak_summaries is not None:
        if set(soak_summaries) != {"shared", "partitioned"}:
            raise AggregationError("soak evidence must contain shared and partitioned summaries")
        validated_soaks = {
            profile: validate_soak_summary(
                soak_summaries[profile], profile, observed_hashes
            )
            for profile in ("shared", "partitioned")
        }
        soak = {
            "minimum_duration_seconds": SOAK_MINIMUM_DURATION_SECONDS,
            "iterations_per_metric": SOAK_ITERATIONS_PER_METRIC,
            "period_us": SOAK_PERIOD_US,
            "profiles": validated_soaks,
        }
        soak_gate_met = True

    m2_exit_gate_met = matrix_gate_met and soak_gate_met
    if m2_exit_gate_met:
        reason = "five-pair matrix and required shared/partitioned soak evidence passed"
    elif matrix_gate_met:
        reason = "five-pair matrix passed; required shared/partitioned soak evidence is missing"
    else:
        reason = "five-pair matrix thresholds were not all satisfied"

    result = {
        "schema_version": 1,
        "campaign": {
            "pair_count": EXPECTED_PAIR_COUNT,
            "workload": expected_contract[0],
            "iterations_per_metric": expected_contract[1],
            "inputs": inputs,
        },
        "thresholds": {
            "p99_non_regression_limit_percent": P99_NON_REGRESSION_LIMIT_PERCENT,
            "max_improvement_target_percent": MAX_IMPROVEMENT_TARGET_PERCENT,
            "direct_irq_max_required_pairs": DIRECT_IRQ_MAX_REQUIRED_PAIRS,
        },
        "metrics": metrics,
        "assessment": {
            "primary_guest_p99_all_pairs": primary_p99_pass,
            "primary_guest_max_direction_gate_met": primary_max_direction_pass,
            "primary_guest_worst_of_runs_gate_met": primary_worst_pass,
            "direct_irq_p99_all_pairs": direct_p99_pass,
            "direct_irq_max_target_pairs": direct_max_target_pairs,
            "direct_irq_max_direction_gate_met": direct_max_direction_pass,
            "direct_irq_worst_of_runs_gate_met": direct_worst_pass,
            "five_pair_matrix_gate_met": matrix_gate_met,
            "soak_evidence_collected": soak_gate_met,
            "m2_exit_gate_met": m2_exit_gate_met,
            "reason": reason,
        },
    }
    if soak is not None:
        result["soak"] = soak
    return result


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "comparisons",
        nargs="+",
        type=Path,
        help="five pair comparison JSON files in registered run order",
    )
    parser.add_argument("--output", type=Path, help="campaign JSON; defaults to stdout")
    parser.add_argument("--shared-soak", type=Path, help="validated shared soak summary")
    parser.add_argument(
        "--partitioned-soak", type=Path, help="validated partitioned soak summary"
    )
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        comparisons = []
        for path in args.comparisons:
            value = json.loads(path.read_text(encoding="utf-8"))
            if not isinstance(value, dict):
                raise AggregationError(f"comparison {path} must be a JSON object")
            comparisons.append(value)
        if (args.shared_soak is None) != (args.partitioned_soak is None):
            raise AggregationError("both shared and partitioned soak summaries are required")
        soak_summaries = None
        if args.shared_soak is not None:
            soak_summaries = {}
            for profile, path in (
                ("shared", args.shared_soak),
                ("partitioned", args.partitioned_soak),
            ):
                value = json.loads(path.read_text(encoding="utf-8"))
                if not isinstance(value, dict):
                    raise AggregationError(f"{profile} soak summary must be a JSON object")
                soak_summaries[profile] = value
        result = aggregate_comparisons(comparisons, soak_summaries)
    except (AggregationError, OSError, json.JSONDecodeError) as error:
        print(f"StarryOS board campaign aggregation failed: {error}", file=sys.stderr)
        return 2

    rendered = json.dumps(result, indent=2, sort_keys=True, allow_nan=False) + "\n"
    if args.output is None:
        sys.stdout.write(rendered)
    else:
        args.output.write_bytes(rendered.encode("utf-8"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
