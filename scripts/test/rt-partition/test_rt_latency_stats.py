#!/usr/bin/env python3
"""Regression tests for Zephyr periodic latency summaries."""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
SCRIPT = ROOT / "scripts/test/rt_latency_stats.py"


class RtLatencyStatsTest(unittest.TestCase):
    def test_reports_zero_and_configured_deadline_miss_counts(self):
        samples = """\
sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns
0,100,1000,1001,1
1,200,2000,2500,500
2,300,3000,4001,1001
"""
        with tempfile.TemporaryDirectory() as temp_dir:
            csv_path = Path(temp_dir) / "samples.csv"
            csv_path.write_text(samples)

            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--tolerance-ns",
                    "1000",
                    str(csv_path),
                ],
                check=True,
                text=True,
                capture_output=True,
            )

            self.assertIn("deadline_tolerance_ns=1000\n", result.stdout)
            self.assertIn("deadline_misses=3\n", result.stdout)
            self.assertIn("deadline_misses_zero_tolerance=3\n", result.stdout)
            self.assertIn("deadline_misses_tolerance=1\n", result.stdout)

    def test_rejects_negative_tolerance(self):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--tolerance-ns", "-1"],
            input="sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns\n",
            text=True,
            capture_output=True,
        )

        self.assertEqual(result.returncode, 2)
        self.assertIn("tolerance must be non-negative", result.stderr)


if __name__ == "__main__":
    unittest.main()
