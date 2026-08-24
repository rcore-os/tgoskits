#!/usr/bin/env python3
"""Regression tests for repeated RT run comparison summaries."""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
SCRIPT = ROOT / "scripts/test/rt-partition/compare-rt-runs.py"


def write_run(
    path: Path, commit: str, p99_ns: int, misses: int, linux_p99_us: int
) -> None:
    path.mkdir(parents=True)
    (path / "meta.txt").write_text(f"git_commit={commit}\n")
    (path / "zephyr-stats.txt").write_text(
        "samples=300\n"
        f"mean_jitter_ns={p99_ns / 2:.2f}\n"
        f"p99_jitter_ns={p99_ns}\n"
        f"p99_9_jitter_ns={p99_ns + 100}\n"
        f"max_jitter_ns={p99_ns + 200}\n"
        "deadline_tolerance_ns=1000000\n"
        f"deadline_misses_tolerance={misses}\n"
    )
    (path / "cyclictest-summary.txt").write_text(
        "min_latency_us=10\n"
        f"avg_latency_us={linux_p99_us // 2}\n"
        f"max_latency_us={linux_p99_us + 200}\n"
        f"p99_latency_us={linux_p99_us}\n"
        f"p99_9_latency_us={linux_p99_us + 100}\n"
        "bucket_samples=1000\n"
        "overflow_samples=3\n"
        "total_samples=1003\n"
    )


class CompareRtRunsTest(unittest.TestCase):
    def test_reports_group_ranges_and_paired_p99_improvement(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            baseline = [root / f"baseline-{index}" for index in range(3)]
            modified = [root / f"modified-{index}" for index in range(3)]
            for path, value, linux_p99 in zip(
                baseline,
                (10_000_000, 12_000_000, 11_000_000),
                (1000, 1100, 1200),
            ):
                write_run(path, "baseline-sha", value, 200, linux_p99)
            for path, value, linux_p99 in zip(
                modified,
                (800_000, 900_000, 1_000_000),
                (1250, 1300, 1350),
            ):
                write_run(path, "modified-sha", value, 0, linux_p99)
            output = root / "comparison.txt"

            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--baseline-label",
                    "upstream-dev",
                    "--baseline",
                    *(str(path) for path in baseline),
                    "--modified-label",
                    "rt-partition",
                    "--modified",
                    *(str(path) for path in modified),
                    "--output",
                    str(output),
                ],
                check=True,
                text=True,
                capture_output=True,
            )

            summary = output.read_text()
            self.assertIn("baseline_git_commit=baseline-sha\n", summary)
            self.assertIn("modified_git_commit=modified-sha\n", summary)
            self.assertIn("baseline_p99_jitter_ns_median=11000000\n", summary)
            self.assertIn("modified_p99_jitter_ns_median=900000\n", summary)
            self.assertIn("p99_improvement_ratio=12.222222\n", summary)
            self.assertIn("paired_p99_improvement_ratio_median=12.500000\n", summary)
            self.assertIn("baseline_zephyr_p99_jitter_ns_median=11000000\n", summary)
            self.assertIn("zephyr_p99_improvement_ratio=12.222222\n", summary)
            self.assertIn("baseline_linux_p99_latency_us_median=1100\n", summary)
            self.assertIn("modified_linux_p99_latency_us_median=1300\n", summary)
            self.assertIn("linux_p99_improvement_ratio=0.846154\n", summary)

    def test_rejects_mixed_commits_within_one_group(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            baseline_a = root / "baseline-a"
            baseline_b = root / "baseline-b"
            modified = root / "modified"
            write_run(baseline_a, "sha-a", 10_000_000, 1, 1000)
            write_run(baseline_b, "sha-b", 10_000_000, 1, 1000)
            write_run(modified, "sha-c", 1_000_000, 0, 1000)

            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--baseline",
                    str(baseline_a),
                    str(baseline_b),
                    "--modified",
                    str(modified),
                ],
                text=True,
                capture_output=True,
            )

            self.assertEqual(result.returncode, 2)
            self.assertIn("baseline runs use multiple git commits", result.stderr)


if __name__ == "__main__":
    unittest.main()
