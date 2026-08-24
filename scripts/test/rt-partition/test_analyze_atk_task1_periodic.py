#!/usr/bin/env python3
"""Tests for sustained ATK Task 1 result validation."""

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("analyze-atk-task1-periodic.py")
SPEC = importlib.util.spec_from_file_location("analyze_atk_task1_periodic", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
ANALYZER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = ANALYZER
SPEC.loader.exec_module(ANALYZER)


class AnalyzeAtkTask1PeriodicTest(unittest.TestCase):
    def test_parse_accepts_contiguous_sustained_run(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "task1-yolo-fp-rr-01.log"
            log.write_text(valid_log())

            run = ANALYZER.parse(
                log,
                expected_samples=3,
                min_inferences=2,
                min_runtime_seconds=0.03,
            )

            self.assertEqual(run.scheduler, "fp-rr")
            self.assertEqual(len(run.rows), 3)
            self.assertEqual(run.periodic_duration_seconds, 0.03)
            self.assertEqual([sample.infer_us for sample in run.inferences], [1500, 1700])
            self.assertEqual(run.inference_duration_seconds, 0.04)

    def test_parse_rejects_a_periodic_sequence_gap(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "task1-yolo-rr-01.log"
            log.write_text(valid_log().replace("1,20000000", "2,20000000"))

            with self.assertRaisesRegex(ValueError, "non-contiguous"):
                ANALYZER.parse(log)

    def test_parse_rejects_missing_sustained_yolo_overlap(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "task1-yolo-rr-01.log"
            log.write_text(valid_log().replace("elapsed_ms=40", "elapsed_ms=20"))

            with self.assertRaisesRegex(ValueError, "sustained overlap is not proven"):
                ANALYZER.parse(log, min_inferences=2, min_runtime_seconds=0.03)

    def test_parse_ignores_numeric_host_diagnostics_before_csv_header(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "task1-yolo-fp-rr-01.log"
            log.write_text(
                valid_log().replace(
                    "sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns",
                    "PERIODIC LATENCY SAMPLING COMPLETE samples=3\n"
                    "0,99999999,1,99999999,99999998\n"
                    "sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns",
                )
            )

            run = ANALYZER.parse(log, expected_samples=3)

            self.assertEqual([row[0] for row in run.rows], [0, 1, 2])

    def test_group_summary_uses_median_of_runs(self):
        runs = [
            {
                "scheduler": "rr",
                "samples": 3,
                "runtime_seconds": 1,
                "mean_ns": 10,
                "p50_ns": 10,
                "p99_ns": value,
                "p99_9_ns": value,
                "max_ns": value,
                "over_1ms": 0,
                "over_10ms": 0,
                "deadline_misses": 0,
                "inference_samples": 2,
                "inference_mean_us": 10,
                "inference_p95_us": 10,
                "inference_p99_us": 10,
                "inference_max_us": 10,
                "inferences_per_minute": 2,
            }
            for value in (30, 10, 20)
        ]

        groups = ANALYZER.summarize_groups(runs)

        self.assertEqual(groups[0]["median_p99_ns"], 20)

    def test_report_exposes_yolo_tail_latency(self):
        group = {
            "scheduler": "fp-rr",
            "runs": 3,
            "median_p99_ns": 500_000,
            "median_p99_9_ns": 700_000,
            "median_max_ns": 900_000,
            "median_inference_mean_us": 2_000_000,
            "median_inference_p99_us": 80_000_000,
            "median_inferences_per_minute": 12,
        }
        rr_group = {**group, "scheduler": "rr"}
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / "SUMMARY.md"

            ANALYZER.write_report(report, [rr_group, group], 6)

            text = report.read_text()
            self.assertIn("YOLO P99", text)
            self.assertIn("80000.000 ms", text)
            self.assertIn("tail-latency/throughput trade-offs", text)


def valid_log() -> str:
    return """\
TASK1_RUNNER scheduler=fp-rr model_mode=model-loop
TASK3_INFER elapsed_ms=20 sample=1 output=1 infer_us=1500 model=yolo11n.ncnn target=1
PERIODIC LATENCY START
sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns
0,10000000,9000000,10000000,1000000
1,20000000,19000000,20000000,1000000
2,30000000,29000000,30000000,1000000
PERIODIC LATENCY COMPLETE samples=3
TASK3_INFER elapsed_ms=40 sample=2 output=1 infer_us=1700 model=yolo11n.ncnn target=1
output_dropped=0
"""


if __name__ == "__main__":
    unittest.main()
