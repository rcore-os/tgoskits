from __future__ import annotations

import importlib.util
import sys
import unittest
from copy import deepcopy
from pathlib import Path


BENCHMARK_DIR = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "axvisor_rt_compare_starry_board",
    BENCHMARK_DIR / "compare_starry_board.py",
)
assert SPEC is not None and SPEC.loader is not None
compare = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = compare
SPEC.loader.exec_module(compare)


def summary(profile: str) -> dict[str, object]:
    return {
        "schema_version": 1,
        "capture": {
            "platform": "OrangePi-5-Plus",
            "os": "starryos",
            "profile": profile,
            "profile_authority": "test",
            "workload": "idle",
            "vcpu_count": 2,
            "iterations_per_metric": 100,
            "sample_count": 300,
            "warmup_iterations": 10,
            "period_us": 1000,
            "measurement_cpu": 0,
            "stress_cpu": 1,
            "fifo_priority": 80,
        },
        "input": {
            "path": f"/{profile}/raw.log",
            "sha256": ("a" if profile == "shared" else "b") * 64,
            "line_count": 311,
            "snapshot_filesystem_state": "clean",
        },
        "metrics": {
            "periodic_jitter": {
                "unit": "ns",
                "count": 100,
                "p99_ns": 100,
                "max_ns": 200,
            },
            "dispatch_latency": {
                "unit": "ns",
                "count": 100,
                "p99_ns": 200,
                "max_ns": 300,
            },
            "emulated_irq_response": {
                "unit": "ns",
                "count": 100,
                "p99_ns": 300,
                "max_ns": 400,
            },
        },
        "metric_semantics": {
            "periodic_jitter": {"authority": "direct-guest-observation"},
            "dispatch_latency": {"authority": "direct-guest-observation"},
            "emulated_irq_response": {"authority": "proxy"},
        },
        "profile_contract": {"dedicated_cpus": profile == "partitioned"},
        "host_pcpu_accounting": {"status": "not-collected"},
        "host_noise": {"status": "not-configured"},
    }


def host_noise(cpu: int, *, elapsed_ticks: int = 900) -> dict[str, object]:
    mask = 1 << cpu
    return {
        "status": "collected",
        "requested_pcpu": cpu,
        "affinity_mask": mask,
        "observed_pcpu_mask": mask,
        "max_duration_ms": 180_000,
        "elapsed_ticks": elapsed_ticks,
        "stop_reason": "guest-complete",
        "intensity": "busy-loop",
        "covers_host_trace": True,
    }


class StarryBoardComparisonTests(unittest.TestCase):
    def test_reports_positive_improvement_when_partitioned_is_lower(self) -> None:
        shared = summary("shared")
        partitioned = summary("partitioned")
        partitioned["metrics"]["periodic_jitter"]["p99_ns"] = 80
        partitioned["metrics"]["periodic_jitter"]["max_ns"] = 150

        result = compare.compare_summaries(shared, partitioned)

        metric = result["metrics"]["periodic_jitter"]
        self.assertEqual(metric["p99"]["change_ns"], -20)
        self.assertEqual(metric["p99"]["improvement_percent"], 20.0)
        self.assertEqual(metric["max"]["improvement_percent"], 25.0)
        self.assertTrue(metric["p99"]["within_non_regression_limit"])
        self.assertFalse(result["scope"]["direct_irq_latency_collected"])
        self.assertFalse(result["scope"]["host_pcpu_accounting_collected"])
        self.assertFalse(result["scope"]["controlled_interference_collected"])

    def test_validates_controlled_interference_placement_and_strength(self) -> None:
        shared = summary("shared")
        partitioned = summary("partitioned")
        shared["host_noise"] = host_noise(1, elapsed_ticks=900)
        partitioned["host_noise"] = host_noise(3, elapsed_ticks=880)

        result = compare.compare_summaries(shared, partitioned)

        interference = result["pair"]["controlled_interference"]
        self.assertEqual(interference["implementation"], "busy-loop")
        self.assertEqual(interference["shared_requested_pcpu"], 1)
        self.assertEqual(interference["partitioned_requested_pcpu"], 3)
        self.assertTrue(result["scope"]["controlled_interference_collected"])

    def test_rejects_one_sided_controlled_interference(self) -> None:
        shared = summary("shared")
        partitioned = summary("partitioned")
        shared["host_noise"] = host_noise(1)

        with self.assertRaisesRegex(compare.ComparisonError, "on both sides"):
            compare.compare_summaries(shared, partitioned)

    def test_compares_direct_irq_metric_with_host_accounting(self) -> None:
        shared = summary("shared")
        partitioned = summary("partitioned")
        shared["metrics"]["virtual_timer_injection_to_guest_irq"] = {
            "unit": "ns",
            "count": 249,
            "p99_ns": 2_000,
            "max_ns": 4_000,
        }
        partitioned["metrics"]["virtual_timer_injection_to_guest_irq"] = {
            "unit": "ns",
            "count": 249,
            "p99_ns": 1_800,
            "max_ns": 3_000,
        }
        shared["host_pcpu_accounting"] = {"status": "collected"}
        partitioned["host_pcpu_accounting"] = {"status": "collected"}

        result = compare.compare_summaries(shared, partitioned)

        direct = result["metrics"]["virtual_timer_injection_to_guest_irq"]
        self.assertEqual(direct["p99"]["improvement_percent"], 10.0)
        self.assertEqual(direct["max"]["improvement_percent"], 25.0)
        self.assertTrue(result["scope"]["direct_irq_latency_collected"])
        self.assertTrue(result["scope"]["host_pcpu_accounting_collected"])
        self.assertTrue(
            result["assessment"]["direct_irq_max_improved_in_this_pair"]
        )
        self.assertFalse(result["assessment"]["m2_exit_gate_met"])

    def test_marks_more_than_five_percent_p99_regression(self) -> None:
        shared = summary("shared")
        partitioned = summary("partitioned")
        partitioned["metrics"]["dispatch_latency"]["p99_ns"] = 211

        result = compare.compare_summaries(shared, partitioned)

        self.assertFalse(
            result["metrics"]["dispatch_latency"]["p99"][
                "within_non_regression_limit"
            ]
        )

    def test_rejects_non_orthogonal_capture_settings(self) -> None:
        shared = summary("shared")
        partitioned = summary("partitioned")
        partitioned["capture"]["period_us"] = 2000

        with self.assertRaisesRegex(compare.ComparisonError, "period_us"):
            compare.compare_summaries(shared, partitioned)

    def test_rejects_reversed_profile_roles(self) -> None:
        with self.assertRaisesRegex(compare.ComparisonError, "shared input"):
            compare.compare_summaries(
                summary("partitioned"), summary("shared")
            )

    def test_rejects_incomplete_metric_count(self) -> None:
        shared = summary("shared")
        partitioned = deepcopy(summary("partitioned"))
        partitioned["metrics"]["periodic_jitter"]["count"] = 99

        with self.assertRaisesRegex(compare.ComparisonError, "sample count"):
            compare.compare_summaries(shared, partitioned)


if __name__ == "__main__":
    unittest.main()
