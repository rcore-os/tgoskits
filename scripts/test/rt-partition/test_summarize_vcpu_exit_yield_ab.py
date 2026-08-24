#!/usr/bin/env python3
import importlib.util
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
SCRIPT = ROOT / "scripts/test/rt-partition/summarize-vcpu-exit-yield-ab.py"
SPEC = importlib.util.spec_from_file_location("summarize_exit_yield", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


def write_run(path: Path, yields: int, direct_p99_ns: int, linux_p99_us: int) -> None:
    path.mkdir(parents=True)
    (path / "vmexit-stat.txt").write_text(
        "  vcpu=1 "
        f"post_vmexit_yields={yields} direct_overlaps=0 "
        "callback_to_run_dispatch_p50_ns=60000 "
        "callback_to_run_dispatch_p99_ns=100000 "
        "callback_to_run_dispatch_p99_9_ns=150000 "
        "direct_to_run_dispatch_p50_ns=10000 "
        f"direct_to_run_dispatch_p99_ns={direct_p99_ns} "
        "direct_to_run_dispatch_p99_9_ns=40000 "
        "callback_to_guest_entry_p50_ns=80000 "
        "callback_to_guest_entry_p99_ns=140000 "
        "callback_to_guest_entry_p99_9_ns=190000 "
        "direct_to_guest_entry_p50_ns=30000 "
        "direct_to_guest_entry_p99_ns=60000 "
        "direct_to_guest_entry_p99_9_ns=80000\n"
    )
    (path / "linux-timerlat-latency-summary.txt").write_text(
        "irq_latency_ns_p99=500000\nirq_latency_ns_p99_9=700000\n"
        "thread_latency_ns_p99=1200000\nthread_latency_ns_p99_9=1600000\n"
        "irq_to_thread_ns_p99=800000\nirq_to_thread_ns_p99_9=1000000\n"
    )
    (path / "cyclictest-summary.txt").write_text(
        f"p99_latency_us={linux_p99_us}\np99_9_latency_us=2000\nmax_latency_us=300000\n"
    )
    (path / "zephyr-stats.txt").write_text(
        "p99_jitter_ns=700000 p99_9_jitter_ns=800000\n"
    )


class SummarizeExitYieldTests(unittest.TestCase):
    def test_reports_paired_internal_and_guest_metrics(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            baseline = [root / "baseline-1", root / "baseline-2"]
            modified = [root / "modified-1", root / "modified-2"]
            write_run(baseline[0], 100, 40000, 1600)
            write_run(baseline[1], 120, 30000, 1400)
            write_run(modified[0], 0, 20000, 1500)
            write_run(modified[1], 0, 20000, 1500)

            result = MODULE.summarize(baseline, modified, 1)

            self.assertIn("baseline_post_vmexit_yields=100.000,120.000", result)
            self.assertIn("modified_post_vmexit_yields=0.000,0.000", result)
            self.assertIn("direct_dispatch_p99_us_paired_improved=2/2", result)
            self.assertIn("direct_dispatch_p99_us_median_reduction_pct=42.857", result)
            self.assertIn("direct_guest_entry_p99_us_baseline=60.000,60.000", result)
            self.assertIn("cyclictest_p99_us_paired_improved=1/2", result)

    def test_rejects_modified_runs_that_still_yield(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            baseline = root / "baseline"
            modified = root / "modified"
            write_run(baseline, 100, 40000, 1600)
            write_run(modified, 1, 20000, 1500)

            with self.assertRaisesRegex(ValueError, "must be zero"):
                MODULE.summarize([baseline], [modified], 1)


if __name__ == "__main__":
    unittest.main()
