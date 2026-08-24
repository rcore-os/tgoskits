#!/usr/bin/env python3
"""Regression tests for Linux ftrace wakeup-path analysis."""

import csv
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
SCRIPT = ROOT / "scripts/test/rt-partition/linux-ftrace-latency.py"


SAMPLE_TRACE = """\
 <idle>-0 [001] d.h1. 10.000000: irq_handler_entry: irq=11 name=arch_timer
 <idle>-0 [001] d.h1. 10.000010: hrtimer_expire_entry: hrtimer=x function=tick_nohz_handler now=1
 <idle>-0 [001] d.h1. 10.000020: hrtimer_expire_exit: hrtimer=x
 <idle>-0 [001] d.h1. 10.000030: hrtimer_expire_entry: hrtimer=y function=hrtimer_wakeup now=1
 <idle>-0 [001] dNh4. 10.000050: sched_wakeup: comm=cyclictest pid=85 prio=9 target_cpu=001
 <idle>-0 [001] dNh1. 10.000060: hrtimer_expire_exit: hrtimer=y
 <idle>-0 [001] dNh1. 10.000070: irq_handler_exit: irq=11 ret=handled
 <idle>-0 [001] d..2. 10.000250: sched_switch: prev_comm=swapper/1 prev_pid=0 prev_prio=120 prev_state=R ==> next_comm=cyclictest next_pid=85 next_prio=9
 cyclictest-85 [001] d.h1. 10.001000: irq_handler_entry: irq=11 name=arch_timer
 cyclictest-85 [001] d.h1. 10.001010: hrtimer_expire_entry: hrtimer=z function=hrtimer_wakeup now=2
 cyclictest-85 [001] dNh4. 10.001020: sched_wakeup: comm=cyclictest pid=83 prio=120 target_cpu=001
 cyclictest-85 [001] dNh1. 10.001030: hrtimer_expire_exit: hrtimer=z
 cyclictest-85 [001] dNh1. 10.001040: irq_handler_exit: irq=11 ret=handled
 cyclictest-85 [001] d.h1. 10.002000: irq_handler_entry: irq=11 name=arch_timer
 cyclictest-85 [001] d.h1. 10.002010: hrtimer_expire_entry: hrtimer=q function=hrtimer_wakeup now=3
 cyclictest-85 [001] dNh4. 10.002020: sched_wakeup: comm=cyclictest pid=85 prio=9 target_cpu=001
 cyclictest-85 [001] dNh1. 10.002030: hrtimer_expire_exit: hrtimer=q
 cyclictest-85 [001] dNh1. 10.002040: irq_handler_exit: irq=11 ret=handled
"""


def read_key_values(path: Path) -> dict[str, int]:
    return {
        name: int(value)
        for name, value in (
            line.split("=", 1) for line in path.read_text().splitlines()
        )
    }


class LinuxFtraceLatencyTest(unittest.TestCase):
    def run_parser(self, trace: str):
        directory = tempfile.TemporaryDirectory()
        temp = Path(directory.name)
        trace_path = temp / "trace.txt"
        summary_path = temp / "summary.txt"
        csv_path = temp / "samples.csv"
        trace_path.write_text(trace)
        result = subprocess.run(
            [
                "python3",
                str(SCRIPT),
                str(trace_path),
                str(summary_path),
                "--csv",
                str(csv_path),
            ],
            text=True,
            capture_output=True,
            check=False,
        )
        return directory, result, summary_path, csv_path

    def test_extracts_the_rt_cyclictest_wakeup_chain(self):
        directory, result, summary_path, csv_path = self.run_parser(SAMPLE_TRACE)
        with directory:
            self.assertEqual(result.returncode, 0, result.stderr)
            summary = read_key_values(summary_path)
            self.assertEqual(summary["target_sched_wakeups"], 2)
            self.assertEqual(summary["self_wakeups_skipped"], 1)
            self.assertEqual(summary["complete_wakeup_chains"], 1)
            self.assertEqual(summary["irq_to_hrtimer_ns_p99"], 30_000)
            self.assertEqual(summary["hrtimer_to_wakeup_ns_p99"], 20_000)
            self.assertEqual(summary["wakeup_to_switch_ns_p99"], 200_000)
            self.assertEqual(summary["irq_to_switch_ns_p99"], 250_000)
            self.assertEqual(summary["hrtimer_callback_ns_p99"], 30_000)
            self.assertEqual(summary["irq_handler_ns_p99"], 70_000)
            with csv_path.open(newline="") as stream:
                rows = list(csv.DictReader(stream))
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]["pid"], "85")

    def test_rejects_a_trace_without_complete_target_chains(self):
        directory, result, _, _ = self.run_parser(
            "<idle>-0 [001] d.h1. 10.0: irq_handler_entry: irq=11 name=arch_timer\n"
        )
        with directory:
            self.assertEqual(result.returncode, 2)
            self.assertIn("no complete cyclictest wakeup chains", result.stderr)


if __name__ == "__main__":
    unittest.main()
