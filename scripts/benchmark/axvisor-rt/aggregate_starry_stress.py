#!/usr/bin/env python3
"""Validate the formal five-pair StarryOS guest CPU-stress campaign."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import sys
from pathlib import Path
from types import ModuleType
from typing import Sequence


def _load_matrix_aggregator() -> ModuleType:
    path = Path(__file__).with_name("aggregate_starry_board.py")
    spec = importlib.util.spec_from_file_location(
        "axvisor_rt_aggregate_starry_board_base", path
    )
    if spec is None or spec.loader is None:
        raise ImportError(f"cannot load StarryOS matrix aggregator from {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


base = _load_matrix_aggregator()
AggregationError = base.AggregationError
EXPECTED_PAIR_COUNT = 5
REGISTERED_ORDER = ("AB", "BA", "AB", "BA", "AB")
EXPECTED_ITERATIONS = 10_000
EXPECTED_METRICS = tuple(base.EXPECTED_METRICS)
EXPECTED_CAPTURE_METRICS = tuple(
    name for name in EXPECTED_METRICS if name != base.DIRECT_IRQ_METRIC
)
EXPECTED_SAMPLE_COUNT = EXPECTED_ITERATIONS * len(EXPECTED_CAPTURE_METRICS)
ZERO_TRACE_COUNTERS = (
    "dropped",
    "incomplete",
    "failed_injections",
    "unowned_virtual_timer_irqs",
    "counter_frequency_mismatches",
)


def require_list(parent: dict[str, object], key: str, label: str) -> list[object]:
    value = parent.get(key)
    if not isinstance(value, list):
        raise AggregationError(f"{label} {key} must be an array")
    return value


def validate_identity(
    parent: dict[str, object], label: str, observed_hashes: set[str]
) -> dict[str, str]:
    path = parent.get("path")
    sha256 = parent.get("sha256")
    if not isinstance(path, str) or not path:
        raise AggregationError(f"{label} path must be nonempty")
    if not isinstance(sha256, str) or base.SHA256_PATTERN.fullmatch(sha256) is None:
        raise AggregationError(f"{label} must contain a lowercase SHA-256")
    if sha256 in observed_hashes:
        raise AggregationError(f"{label} reuses an evidence SHA-256")
    observed_hashes.add(sha256)
    return {"path": path, "sha256": sha256}


def validate_host_accounting(summary: dict[str, object], label: str) -> None:
    accounting = base.require_object(summary, "host_pcpu_accounting", label)
    if accounting.get("status") != "collected":
        raise AggregationError(f"{label} host pCPU accounting was not collected")
    vcpus = require_list(accounting, "vcpus", f"{label} host accounting")
    expected = {0: 0x2, 1: 0x4}
    observed: dict[int, int] = {}
    for value in vcpus:
        if not isinstance(value, dict):
            raise AggregationError(f"{label} vCPU accounting entry must be an object")
        vm = value.get("vm")
        vcpu = value.get("vcpu")
        pcpu_mask = value.get("pcpu_mask")
        migrations = value.get("migrations")
        if vm != 1 or not isinstance(vcpu, int) or isinstance(vcpu, bool):
            raise AggregationError(f"{label} vCPU accounting identity is invalid")
        if not isinstance(pcpu_mask, int) or isinstance(pcpu_mask, bool):
            raise AggregationError(f"{label} vCPU pCPU mask is invalid")
        if migrations != 0:
            raise AggregationError(f"{label} vCPU migration count is nonzero")
        if vcpu in observed:
            raise AggregationError(f"{label} repeats vCPU {vcpu} accounting")
        observed[vcpu] = pcpu_mask
    if observed != expected:
        raise AggregationError(f"{label} vCPU placement differs from the frozen contract")


def validate_stress_summary(
    summary: dict[str, object],
    profile: str,
    expected_raw: dict[str, str],
    label: str,
    observed_trace_hashes: set[str],
) -> dict[str, object]:
    if summary.get("schema_version") != 1:
        raise AggregationError(f"{label} must use summary schema version 1")

    capture = base.require_object(summary, "capture", label)
    expected_capture = {
        "os": "starryos",
        "profile": profile,
        "workload": "cpu-stress",
        "vcpu_count": 2,
        "iterations_per_metric": EXPECTED_ITERATIONS,
        "sample_count": EXPECTED_SAMPLE_COUNT,
        "warmup_iterations": 100,
        "period_us": 1_000,
        "measurement_cpu": 0,
        "stress_cpu": 1,
        "fifo_priority": 80,
    }
    for key, expected in expected_capture.items():
        if capture.get(key) != expected:
            raise AggregationError(f"{label} {key} differs from the stress contract")

    raw = base.require_object(summary, "input", label)
    if raw.get("snapshot_filesystem_state") != "clean":
        raise AggregationError(f"{label} snapshot filesystem must be clean")
    if raw.get("sha256") != expected_raw["sha256"]:
        raise AggregationError(f"{label} raw SHA-256 differs from its comparison")

    contract = base.require_object(summary, "profile_contract", label)
    expected_vm_config = (
        "scripts/benchmark/axvisor-rt/config/"
        f"starry-orangepi-5-plus-smp2-{profile}.toml"
    )
    if contract.get("dedicated_cpus") is not (profile == "partitioned"):
        raise AggregationError(f"{label} dedicated_cpus differs from its profile")
    if contract.get("phys_cpu_sets") != ["0x2", "0x4"]:
        raise AggregationError(f"{label} vCPU sets differ from the frozen contract")
    if contract.get("vm_config") != expected_vm_config or contract.get("soak") is not False:
        raise AggregationError(f"{label} must use the standard non-soak VM config")

    host_noise = base.require_object(summary, "host_noise", label)
    if host_noise != {"status": "not-configured"}:
        raise AggregationError(f"{label} must not enable controlled host interference")
    validate_host_accounting(summary, label)

    direct = base.require_object(summary, "direct_irq_trace", label)
    lossless = base.require_object(direct, "lossless", label)
    records: dict[str, int | float] = {}
    for side in ("host", "guest"):
        counters = base.require_object(lossless, side, f"{label} lossless")
        count = base.require_number(counters, "records", f"{label} {side} trace")
        if count <= 0:
            raise AggregationError(f"{label} {side} trace must contain records")
        records[side] = count
        for key in ZERO_TRACE_COUNTERS:
            if base.require_number(counters, key, f"{label} {side} trace") != 0:
                raise AggregationError(f"{label} {side} trace reports {key}")

    pairing = base.require_object(direct, "pairing", label)
    pair_count = base.require_number(pairing, "pair_count", f"{label} direct IRQ")
    if pair_count <= 0 or pair_count > min(records.values()):
        raise AggregationError(f"{label} direct IRQ pair count is invalid")

    inputs = base.require_object(direct, "inputs", label)
    trace_inputs = {
        side: validate_identity(
            base.require_object(inputs, side, f"{label} trace inputs"),
            f"{label} {side} trace",
            observed_trace_hashes,
        )
        for side in ("host", "guest")
    }
    return {
        "profile": profile,
        "raw": expected_raw,
        "trace_inputs": trace_inputs,
        "host_records": records["host"],
        "guest_records": records["guest"],
        "direct_irq_pair_count": pair_count,
        "snapshot_filesystem_state": "clean",
        "vcpu_masks": ["0x2", "0x4"],
        "vcpu_migrations": 0,
    }


def aggregate_stress_campaign(
    comparisons: Sequence[dict[str, object]],
    summary_pairs: Sequence[dict[str, object]],
) -> dict[str, object]:
    """Validate five lossless stress pairs without making an isolation claim."""
    if len(comparisons) != EXPECTED_PAIR_COUNT:
        raise AggregationError(
            f"stress campaign must contain exactly {EXPECTED_PAIR_COUNT} comparisons"
        )
    if len(summary_pairs) != EXPECTED_PAIR_COUNT:
        raise AggregationError(
            f"stress campaign must contain exactly {EXPECTED_PAIR_COUNT} summary pairs"
        )

    observed_raw_hashes: set[str] = set()
    observed_trace_hashes: set[str] = set()
    metric_pairs: dict[str, list[dict[str, dict[str, int | float | bool]]]] = {
        name: [] for name in EXPECTED_METRICS
    }
    inputs: list[dict[str, object]] = []

    for index, (comparison, summaries) in enumerate(
        zip(comparisons, summary_pairs, strict=True), start=1
    ):
        label = f"pair {index}"
        if comparison.get("schema_version") != 1:
            raise AggregationError(f"{label} must use comparison schema version 1")
        pair = base.require_object(comparison, "pair", label)
        if pair.get("workload") != "cpu-stress":
            raise AggregationError(f"{label} must use the cpu-stress workload")
        if pair.get("iterations_per_metric") != EXPECTED_ITERATIONS:
            raise AggregationError(f"{label} must contain 10,000 samples per metric")
        if pair.get("controlled_interference") is not None:
            raise AggregationError(f"{label} must not contain controlled host interference")

        thresholds = base.require_object(comparison, "thresholds", label)
        if thresholds != {
            "p99_non_regression_limit_percent": base.P99_NON_REGRESSION_LIMIT_PERCENT,
            "max_improvement_target_percent": base.MAX_IMPROVEMENT_TARGET_PERCENT,
        }:
            raise AggregationError(f"{label} comparison thresholds are invalid")
        scope = base.require_object(comparison, "scope", label)
        for field in (
            "direct_irq_latency_collected",
            "host_pcpu_accounting_collected",
            "timerfd_metric_is_proxy",
        ):
            if scope.get(field) is not True:
                raise AggregationError(f"{label} scope is missing {field}")
        if scope.get("controlled_interference_collected") is True:
            raise AggregationError(f"{label} unexpectedly collected host interference")

        shared_raw = base.validate_raw(
            pair, "shared_raw", label, observed_raw_hashes
        )
        partitioned_raw = base.validate_raw(
            pair, "partitioned_raw", label, observed_raw_hashes
        )
        if not isinstance(summaries, dict) or set(summaries) != {
            "shared",
            "partitioned",
        }:
            raise AggregationError(f"{label} summaries must contain both profiles")
        profiles = {
            "shared": validate_stress_summary(
                summaries["shared"],
                "shared",
                shared_raw,
                f"{label} shared",
                observed_trace_hashes,
            ),
            "partitioned": validate_stress_summary(
                summaries["partitioned"],
                "partitioned",
                partitioned_raw,
                f"{label} partitioned",
                observed_trace_hashes,
            ),
        }

        metrics = base.require_object(comparison, "metrics", label)
        if set(metrics) != set(EXPECTED_METRICS):
            raise AggregationError(f"{label} metric set differs from the stress contract")
        for name in EXPECTED_METRICS:
            metric = metrics[name]
            if not isinstance(metric, dict):
                raise AggregationError(f"{label} metric {name} must be an object")
            metric_pairs[name].append(base.validate_metric(metric, f"{label} {name}"))
        inputs.append(
            {
                "pair": index,
                "run_order": REGISTERED_ORDER[index - 1],
                "profiles": profiles,
            }
        )

    metrics = {
        name: {
            "p99": base.aggregate_statistic(pairs, "p99"),
            "max": base.aggregate_statistic(pairs, "max"),
        }
        for name, pairs in metric_pairs.items()
    }
    return {
        "schema_version": 1,
        "campaign": {
            "pair_count": EXPECTED_PAIR_COUNT,
            "capture_count": EXPECTED_PAIR_COUNT * 2,
            "registered_order": list(REGISTERED_ORDER),
            "workload": "cpu-stress",
            "iterations_per_metric": EXPECTED_ITERATIONS,
            "inputs": inputs,
        },
        "metrics": metrics,
        "assessment": {
            "formal_stress_coverage_met": True,
            "all_captures_lossless": True,
            "fixed_vcpu_placement_met": True,
            "isolation_claim_allowed": False,
            "reason": (
                "five lossless guest CPU1 stress pairs passed the frozen stability "
                "contract; this same-VM workload is not an isolation treatment"
            ),
        },
    }


def read_json(path: Path) -> dict[str, object]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise AggregationError(f"{path} must contain a JSON object")
    return value


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def verify_pair_files(
    pair_dir: Path,
    comparison: dict[str, object],
    summaries: dict[str, object],
) -> None:
    pair = base.require_object(comparison, "pair", str(pair_dir))
    for profile in ("shared", "partitioned"):
        summary = summaries[profile]
        if not isinstance(summary, dict):
            raise AggregationError(f"{pair_dir} {profile} summary must be an object")
        raw = pair_dir / profile / "raw.log"
        expected_raw = base.require_object(pair, f"{profile}_raw", str(pair_dir))
        if sha256_file(raw) != expected_raw.get("sha256"):
            raise AggregationError(f"{pair_dir} {profile} raw file SHA-256 is invalid")
        direct = base.require_object(summary, "direct_irq_trace", str(pair_dir))
        inputs = base.require_object(direct, "inputs", str(pair_dir))
        for side, filename in (("host", "host.log"), ("guest", "guest-irq.log.gz")):
            identity = base.require_object(inputs, side, str(pair_dir))
            if sha256_file(pair_dir / profile / filename) != identity.get("sha256"):
                raise AggregationError(
                    f"{pair_dir} {profile} {side} trace file SHA-256 is invalid"
                )


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "comparisons",
        nargs="+",
        type=Path,
        help="five comparison JSON files in preregistered run order",
    )
    parser.add_argument("--output", type=Path, help="campaign JSON; defaults to stdout")
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        comparisons: list[dict[str, object]] = []
        summaries: list[dict[str, object]] = []
        for comparison_path in args.comparisons:
            comparison = read_json(comparison_path)
            pair_dir = comparison_path.parent
            summary_pair = {
                profile: read_json(pair_dir / profile / "summary.json")
                for profile in ("shared", "partitioned")
            }
            verify_pair_files(pair_dir, comparison, summary_pair)
            comparisons.append(comparison)
            summaries.append(summary_pair)
        result = aggregate_stress_campaign(comparisons, summaries)
    except (AggregationError, json.JSONDecodeError, OSError) as error:
        print(f"StarryOS stress campaign aggregation failed: {error}", file=sys.stderr)
        return 2

    rendered = json.dumps(result, indent=2, sort_keys=True, allow_nan=False) + "\n"
    if args.output is None:
        sys.stdout.write(rendered)
    else:
        args.output.write_text(rendered, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
