#!/usr/bin/env python3
"""Regression tests for Linux timerlat summary generation."""

import csv
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
SCRIPT = ROOT / "scripts/test/rt-partition/linux-timerlat-latency.py"


SAMPLE = """\
 <idle>-0 [001] d.h1 10.0: #7 context irq timer_latency 1000 ns
 timerlat/1-8 [001] .... 10.1: #7 context thread timer_latency 5000 ns
 <idle>-0 [001] d.h1 10.2: #8 context irq timer_latency 2000 ns
 timerlat/1-8 [001] .... 10.3: #8 context thread timer_latency 9000 ns
 timerlat/1-8 [001] .... 10.4: #9 context thread timer_latency 12000 ns
"""


class LinuxTimerlatLatencyTest(unittest.TestCase):
    def run_parser(self, text: str):
        directory = tempfile.TemporaryDirectory()
        temp = Path(directory.name)
        trace = temp / "trace.txt"
        summary = temp / "summary.txt"
        samples = temp / "samples.csv"
        trace.write_text(text)
        result = subprocess.run(
            [
                "python3",
                str(SCRIPT),
                str(trace),
                str(summary),
                "--csv",
                str(samples),
            ],
            text=True,
            capture_output=True,
            check=False,
        )
        return directory, result, summary, samples

    def test_pairs_irq_and_thread_records_by_activation(self):
        directory, result, summary_path, samples_path = self.run_parser(SAMPLE)
        with directory:
            self.assertEqual(result.returncode, 0, result.stderr)
            summary = dict(
                line.split("=", 1) for line in summary_path.read_text().splitlines()
            )
            self.assertEqual(summary["complete_activations"], "2")
            self.assertEqual(summary["unmatched_thread_records"], "1")
            self.assertEqual(summary["irq_latency_ns_p99"], "2000")
            self.assertEqual(summary["thread_latency_ns_p99"], "9000")
            self.assertEqual(summary["irq_to_thread_ns_p99"], "7000")
            with samples_path.open(newline="") as stream:
                rows = list(csv.DictReader(stream))
            self.assertEqual([row["activation"] for row in rows], ["7", "8"])

    def test_rejects_a_trace_without_complete_activations(self):
        directory, result, _, _ = self.run_parser(
            "<idle>-0 [001] d.h1 10.0: #7 context irq timer_latency 1000 ns\n"
        )
        with directory:
            self.assertEqual(result.returncode, 2)
            self.assertIn("no complete timerlat", result.stderr)


if __name__ == "__main__":
    unittest.main()
