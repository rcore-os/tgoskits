#!/usr/bin/env python3
"""Regression tests for host periodic scheduler tick evidence."""

import csv
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
SCRIPT = ROOT / "scripts/test/rt-partition/host-periodic-ticks-to-csv.py"


SAMPLE_LOG = """\
[host_monotonic_s=10.0] VM-exit counters per physical CPU (13 reasons):
[host_monotonic_s=10.1] Host periodic scheduler ticks (event-driven timer IRQs excluded):
[host_monotonic_s=10.2]   cpu   0:        100
[host_monotonic_s=10.3]   cpu   1:          0
[host_monotonic_s=10.4]   cpu   2:        101
[host_monotonic_s=10.5]   cpu   3:          0
[host_monotonic_s=20.0] VM-exit counters per physical CPU (13 reasons):
[host_monotonic_s=20.1] Host periodic scheduler ticks (event-driven timer IRQs excluded):
[host_monotonic_s=20.2]   cpu   0:        900 (80.000/s)
[host_monotonic_s=20.3]   cpu   1:          0
[host_monotonic_s=20.4]   cpu   2:        901 (80.000/s)
[host_monotonic_s=20.5]   cpu   3:          0
[host_monotonic_s=30.0] VM-exit counters per physical CPU (13 reasons):
[host_monotonic_s=30.1] Host periodic scheduler ticks (event-driven timer IRQs excluded):
[host_monotonic_s=30.2]   cpu   0:       1700 (80.000/s)
[host_monotonic_s=30.3]   cpu   1:          0
[host_monotonic_s=30.4]   cpu   2:       1701 (80.000/s)
[host_monotonic_s=30.5]   cpu   3:          0
"""


class HostPeriodicTicksTest(unittest.TestCase):
    def run_parser(self, log: str, *extra_args: str):
        with tempfile.TemporaryDirectory() as directory:
            temp = Path(directory)
            log_path = temp / "run.log"
            csv_path = temp / "ticks.csv"
            log_path.write_text(log)
            result = subprocess.run(
                ["python3", str(SCRIPT), str(log_path), str(csv_path), *extra_args],
                text=True,
                capture_output=True,
                check=False,
            )
            rows = []
            if csv_path.exists():
                with csv_path.open(newline="") as stream:
                    rows = list(csv.DictReader(stream))
            return result, rows

    def test_extracts_cumulative_and_delta_counts_from_timestamped_log(self):
        result, rows = self.run_parser(SAMPLE_LOG)
        self.assertEqual(result.returncode, 0, result.stderr)
        cpu1 = [row for row in rows if row["cpu"] == "1"]
        self.assertEqual([row["count"] for row in cpu1], ["0", "0", "0"])
        self.assertEqual([row["delta"] for row in cpu1], ["0", "0", "0"])
        cpu0 = [row for row in rows if row["cpu"] == "0"]
        self.assertEqual([row["delta"] for row in cpu0], ["100", "800", "800"])

    def test_require_zero_cpu_rejects_any_periodic_tick(self):
        bad_log = SAMPLE_LOG.replace("cpu   1:          0", "cpu   1:          1", 1)
        result, _ = self.run_parser(bad_log, "--require-zero-cpu", "1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("pCPU1", result.stderr)

    def test_require_zero_cpu_accepts_all_zero_snapshots(self):
        result, _ = self.run_parser(SAMPLE_LOG, "--require-zero-cpu", "1")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_require_zero_cpu_accepts_multiple_zero_cpus(self):
        result, _ = self.run_parser(
            SAMPLE_LOG,
            "--require-zero-cpu",
            "1",
            "--require-zero-cpu",
            "3",
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_missing_tick_snapshots(self):
        result, _ = self.run_parser("VM-exit counters per physical CPU\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("periodic scheduler tick snapshots", result.stderr)


if __name__ == "__main__":
    unittest.main()
