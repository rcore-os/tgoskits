from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import unittest
from copy import deepcopy
from pathlib import Path


BENCHMARK_DIR = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "axvisor_rt_aggregate_starry_board",
    BENCHMARK_DIR / "aggregate_starry_board.py",
)
assert SPEC is not None and SPEC.loader is not None
aggregate = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = aggregate
SPEC.loader.exec_module(aggregate)


METRICS = (
    "periodic_jitter",
    "dispatch_latency",
    "emulated_irq_response",
    "virtual_timer_injection_to_guest_irq",
)


def metric(shared: int, partitioned: int) -> dict[str, object]:
    improvement = round(100.0 * (shared - partitioned) / shared, 3)
    return {
        "p99": {
            "shared_ns": shared,
            "partitioned_ns": partitioned,
            "change_ns": partitioned - shared,
            "improvement_percent": improvement,
            "within_non_regression_limit": improvement >= -5.0,
        },
        "max": {
            "shared_ns": shared,
            "partitioned_ns": partitioned,
            "change_ns": partitioned - shared,
            "improvement_percent": improvement,
            "improved": improvement > 0,
            "meets_ten_percent_target": improvement >= 10.0,
        },
    }


def comparison(index: int) -> dict[str, object]:
    metrics = {name: metric(1_000_000 + index, 100_000 + index) for name in METRICS}
    return {
        "schema_version": 1,
        "pair": {
            "workload": "idle",
            "iterations_per_metric": 10_000,
            "shared_raw": {
                "path": f"/pair-{index}/shared/raw.log",
                "sha256": f"{index:064x}",
            },
            "partitioned_raw": {
                "path": f"/pair-{index}/partitioned/raw.log",
                "sha256": f"{index + 10:064x}",
            },
            "controlled_interference": {
                "implementation": "busy-loop",
                "max_duration_ms": 600_000,
                "shared_requested_pcpu": 1,
                "partitioned_requested_pcpu": 3,
                "placement_and_coverage_validated": True,
            },
        },
        "thresholds": {
            "p99_non_regression_limit_percent": 5.0,
            "max_improvement_target_percent": 10.0,
        },
        "metrics": metrics,
        "assessment": {
            "primary_guest_p99_within_non_regression_limit": True,
            "primary_guest_max_improved_in_this_pair": True,
            "direct_irq_max_improved_in_this_pair": True,
            "m2_exit_gate_met": False,
        },
        "scope": {
            "direct_irq_latency_collected": True,
            "host_pcpu_accounting_collected": True,
            "controlled_interference_collected": True,
            "timerfd_metric_is_proxy": True,
        },
    }


class StarryBoardCampaignAggregationTests(unittest.TestCase):
    def test_five_passing_pairs_meet_matrix_gate_but_not_soak_gate(self) -> None:
        result = aggregate.aggregate_comparisons(
            [comparison(index) for index in range(1, 6)]
        )

        self.assertTrue(result["assessment"]["five_pair_matrix_gate_met"])
        self.assertFalse(result["assessment"]["m2_exit_gate_met"])
        self.assertEqual(result["assessment"]["direct_irq_max_target_pairs"], 5)
        direct = result["metrics"]["virtual_timer_injection_to_guest_irq"]
        self.assertEqual(direct["max"]["target_pair_count"], 5)
        self.assertGreater(direct["max"]["worst_of_runs_improvement_percent"], 10)

    def test_requires_exactly_five_pairs(self) -> None:
        with self.assertRaisesRegex(aggregate.AggregationError, "exactly 5"):
            aggregate.aggregate_comparisons(
                [comparison(index) for index in range(1, 5)]
            )

    def test_requires_direct_irq_max_target_in_four_pairs(self) -> None:
        pairs = [comparison(index) for index in range(1, 6)]
        for pair in pairs[3:]:
            direct = pair["metrics"]["virtual_timer_injection_to_guest_irq"]
            direct["max"] = metric(1_000_000, 950_000)["max"]
            pair["assessment"]["direct_irq_max_improved_in_this_pair"] = True

        result = aggregate.aggregate_comparisons(pairs)

        self.assertEqual(result["assessment"]["direct_irq_max_target_pairs"], 3)
        self.assertFalse(result["assessment"]["five_pair_matrix_gate_met"])

    def test_rejects_reused_raw_capture(self) -> None:
        pairs = [comparison(index) for index in range(1, 6)]
        pairs[4]["pair"]["shared_raw"] = deepcopy(pairs[0]["pair"]["shared_raw"])

        with self.assertRaisesRegex(aggregate.AggregationError, "raw SHA-256"):
            aggregate.aggregate_comparisons(pairs)

    def test_cli_writes_platform_independent_lf_json(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            inputs = []
            for index in range(1, 6):
                path = root / f"pair-{index}.json"
                path.write_text(json.dumps(comparison(index)), encoding="utf-8")
                inputs.append(str(path))
            output = root / "campaign.json"

            status = aggregate.main([*inputs, "--output", str(output)])

            self.assertEqual(status, 0)
            self.assertNotIn(b"\r\n", output.read_bytes())


if __name__ == "__main__":
    unittest.main()
