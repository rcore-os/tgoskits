from __future__ import annotations

import hashlib
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path


BENCHMARK_DIR = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "axvisor_rt_aggregate_starry_stress",
    BENCHMARK_DIR / "aggregate_starry_stress.py",
)
assert SPEC is not None and SPEC.loader is not None
stress = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = stress
SPEC.loader.exec_module(stress)


METRICS = (
    "periodic_jitter",
    "dispatch_latency",
    "emulated_irq_response",
    "virtual_timer_injection_to_guest_irq",
)


def identity(label: str) -> str:
    return hashlib.sha256(label.encode()).hexdigest()


def metric(shared_ns: int, partitioned_ns: int) -> dict[str, object]:
    improvement = round(100.0 * (shared_ns - partitioned_ns) / shared_ns, 3)
    return {
        statistic: {
            "shared_ns": shared_ns,
            "partitioned_ns": partitioned_ns,
            "change_ns": partitioned_ns - shared_ns,
            "improvement_percent": improvement,
            **(
                {"within_non_regression_limit": improvement >= -5.0}
                if statistic == "p99"
                else {
                    "improved": improvement > 0,
                    "meets_ten_percent_target": improvement >= 10.0,
                }
            ),
        }
        for statistic in ("p99", "max")
    }


def comparison(index: int) -> dict[str, object]:
    return {
        "schema_version": 1,
        "pair": {
            "workload": "cpu-stress",
            "iterations_per_metric": 10_000,
            "shared_raw": {
                "path": f"/campaign/pair-{index}/shared/raw.log",
                "sha256": identity(f"pair-{index}-shared-raw"),
            },
            "partitioned_raw": {
                "path": f"/campaign/pair-{index}/partitioned/raw.log",
                "sha256": identity(f"pair-{index}-partitioned-raw"),
            },
        },
        "scope": {
            "direct_irq_latency_collected": True,
            "host_pcpu_accounting_collected": True,
            "timerfd_metric_is_proxy": True,
        },
        "thresholds": {
            "p99_non_regression_limit_percent": 5.0,
            "max_improvement_target_percent": 10.0,
        },
        "metrics": {
            name: metric(100_000 + offset, 101_000 + offset)
            for offset, name in enumerate(METRICS)
        },
    }


def summary(index: int, profile: str) -> dict[str, object]:
    trace_counters = {
        "records": 500,
        "dropped": 0,
        "incomplete": 0,
        "failed_injections": 0,
        "unowned_virtual_timer_irqs": 0,
        "counter_frequency_mismatches": 0,
    }
    return {
        "schema_version": 1,
        "capture": {
            "os": "starryos",
            "profile": profile,
            "workload": "cpu-stress",
            "vcpu_count": 2,
            "iterations_per_metric": 10_000,
            "sample_count": 30_000,
            "warmup_iterations": 100,
            "period_us": 1_000,
            "measurement_cpu": 0,
            "stress_cpu": 1,
            "fifo_priority": 80,
        },
        "input": {
            "path": f"/campaign/pair-{index}/{profile}/raw.log",
            "sha256": identity(f"pair-{index}-{profile}-raw"),
            "snapshot_filesystem_state": "clean",
        },
        "profile_contract": {
            "dedicated_cpus": profile == "partitioned",
            "phys_cpu_sets": ["0x2", "0x4"],
            "soak": False,
            "vm_config": (
                "scripts/benchmark/axvisor-rt/config/"
                f"starry-orangepi-5-plus-smp2-{profile}.toml"
            ),
        },
        "host_noise": {"status": "not-configured"},
        "host_pcpu_accounting": {
            "status": "collected",
            "pcpus": [],
            "vcpus": [
                {"vm": 1, "vcpu": 0, "pcpu_mask": 0x2, "migrations": 0},
                {"vm": 1, "vcpu": 1, "pcpu_mask": 0x4, "migrations": 0},
            ],
        },
        "direct_irq_trace": {
            "lossless": {
                "host": dict(trace_counters),
                "guest": dict(trace_counters),
            },
            "pairing": {"pair_count": 500},
            "inputs": {
                side: {
                    "path": f"/campaign/pair-{index}/{profile}/{side}.log",
                    "sha256": identity(f"pair-{index}-{profile}-{side}"),
                }
                for side in ("host", "guest")
            },
        },
    }


def campaign_inputs() -> tuple[list[dict[str, object]], list[dict[str, object]]]:
    comparisons = [comparison(index) for index in range(1, 6)]
    summaries = [
        {
            "shared": summary(index, "shared"),
            "partitioned": summary(index, "partitioned"),
        }
        for index in range(1, 6)
    ]
    return comparisons, summaries


class StarryStressCampaignTests(unittest.TestCase):
    def test_five_lossless_pairs_complete_formal_stress_coverage(self) -> None:
        comparisons, summaries = campaign_inputs()

        result = stress.aggregate_stress_campaign(comparisons, summaries)

        self.assertTrue(result["assessment"]["formal_stress_coverage_met"])
        self.assertFalse(result["assessment"]["isolation_claim_allowed"])
        self.assertEqual(result["campaign"]["registered_order"], ["AB", "BA", "AB", "BA", "AB"])
        self.assertEqual(result["campaign"]["capture_count"], 10)
        self.assertEqual(
            result["campaign"]["inputs"][0]["profiles"]["shared"][
                "vcpu_migrations"
            ],
            0,
        )

    def test_cli_verifies_evidence_files_and_writes_campaign(self) -> None:
        comparisons, summaries = campaign_inputs()
        with tempfile.TemporaryDirectory() as temporary:
            campaign_dir = Path(temporary)
            comparison_paths: list[Path] = []
            for index, (pair_comparison, pair_summaries) in enumerate(
                zip(comparisons, summaries, strict=True), start=1
            ):
                pair_dir = campaign_dir / f"pair-{index}"
                for profile in ("shared", "partitioned"):
                    profile_dir = pair_dir / profile
                    profile_dir.mkdir(parents=True)
                    (profile_dir / "raw.log").write_text(
                        f"pair-{index}-{profile}-raw", encoding="utf-8"
                    )
                    (profile_dir / "host.log").write_text(
                        f"pair-{index}-{profile}-host", encoding="utf-8"
                    )
                    (profile_dir / "guest-irq.log.gz").write_text(
                        f"pair-{index}-{profile}-guest", encoding="utf-8"
                    )
                    (profile_dir / "summary.json").write_text(
                        json.dumps(pair_summaries[profile]), encoding="utf-8"
                    )
                comparison_path = pair_dir / "comparison.json"
                comparison_path.write_text(
                    json.dumps(pair_comparison), encoding="utf-8"
                )
                comparison_paths.append(comparison_path)

            output_path = campaign_dir / "campaign-summary.json"
            result = stress.main(
                [*(str(path) for path in comparison_paths), "--output", str(output_path)]
            )

            self.assertEqual(result, 0)
            campaign = json.loads(output_path.read_text(encoding="utf-8"))
            self.assertTrue(campaign["assessment"]["formal_stress_coverage_met"])

    def test_rejects_non_stress_comparison(self) -> None:
        comparisons, summaries = campaign_inputs()
        comparisons[0]["pair"]["workload"] = "idle"

        with self.assertRaisesRegex(stress.AggregationError, "cpu-stress"):
            stress.aggregate_stress_campaign(comparisons, summaries)

    def test_rejects_lossy_trace(self) -> None:
        comparisons, summaries = campaign_inputs()
        summaries[2]["shared"]["direct_irq_trace"]["lossless"]["guest"][
            "dropped"
        ] = 1

        with self.assertRaisesRegex(stress.AggregationError, "dropped"):
            stress.aggregate_stress_campaign(comparisons, summaries)

    def test_rejects_non_proxy_timerfd_scope(self) -> None:
        comparisons, summaries = campaign_inputs()
        comparisons[1]["scope"]["timerfd_metric_is_proxy"] = False

        with self.assertRaisesRegex(stress.AggregationError, "timerfd_metric_is_proxy"):
            stress.aggregate_stress_campaign(comparisons, summaries)


if __name__ == "__main__":
    unittest.main()
