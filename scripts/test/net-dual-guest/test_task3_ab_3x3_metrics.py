#!/usr/bin/env python3
"""Regression tests for physical Task-3 3+3 aggregation."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("task3_ab_3x3_metrics.py")
sys.path.insert(0, str(MODULE_PATH.parent))
SPEC = importlib.util.spec_from_file_location("task3_ab_3x3_metrics", MODULE_PATH)
assert SPEC and SPEC.loader
METRICS = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = METRICS
SPEC.loader.exec_module(METRICS)
BASE_METRICS = sys.modules["task3_ab_metrics"]


class Task3Ab3x3MetricsTests(unittest.TestCase):
    def test_aggregates_sample_and_cross_run_latency_statistics(self) -> None:
        manual_runs = [manual_run(rtt) for rtt in (20, 30, 40)]
        yolo_runs = [
            yolo_run(rtt, infer)
            for rtt, infer in ((10, 100), (20, 200), (30, 300))
        ]

        METRICS.verify_repetitions(manual_runs, yolo_runs)
        manual, yolo = METRICS.aggregate_repetitions(manual_runs, yolo_runs)

        self.assertEqual(manual["runs"], 3)
        self.assertEqual(manual["samples"], 3)
        self.assertEqual(manual["mean_rtt_ms"], 30.0)
        self.assertEqual(manual["p95_rtt_ms"], 40)
        self.assertEqual(manual["run_mean_rtt_stddev_ms"], 10.0)
        self.assertEqual(yolo["mean_infer_us"], 200.0)
        self.assertEqual(yolo["p50_infer_us"], 200)
        self.assertEqual(yolo["p95_infer_us"], 300)

    def test_rejects_incomplete_repetition_set_or_changed_manifest(self) -> None:
        manual_runs = [manual_run(20) for _ in range(3)]
        yolo_runs = [yolo_run(10, 100) for _ in range(3)]

        with self.assertRaisesRegex(ValueError, "exactly 3"):
            METRICS.verify_repetitions(manual_runs[:2], yolo_runs)

        yolo_runs[2].samples[0].image_sha256 = "changed"
        with self.assertRaisesRegex(ValueError, "same frozen image manifest"):
            METRICS.verify_repetitions(manual_runs, yolo_runs)


def manual_run(rtt_ms: int):
    return parse("manual", rtt_ms, None, sent_target=500, state_after=355)


def yolo_run(rtt_ms: int, infer_us: int):
    return parse("yolo", rtt_ms, infer_us, sent_target=400, state_after=320)


def parse(
    mode: str,
    rtt_ms: int,
    infer_us: int | None,
    sent_target: int,
    state_after: int,
):
    inference = ""
    if infer_us is not None:
        inference = (
            "TASK3_INFER model=yolo11n.ncnn source=yolo "
            f"infer_us={infer_us} request=1 elapsed_ms=1\n"
            "TASK3_DETECTION model=yolo11n.ncnn class=75 confidence_milli=828 "
            "center_x_milli=321 area_milli=63 target=400 request=1\n"
        )
    log = (
        "TASK3_SAMPLE sample=1 image_id=vase-left image_sha256=abc "
        f"truth_target=321 expected=accept source={mode} outcome=accepted "
        "request=1 elapsed_ms=0\n"
        f"{inference}"
        "TASK3_CONTROL_SENT elapsed_ms=1 sample=1 image_id=vase-left "
        f"source={mode} value={sent_target} truth_target=321 state=300 "
        "request=1 seq=1\n"
        "TASK3_STATUS_RECEIVED elapsed_ms=2 sample=1 image_id=vase-left request=1 "
        f"value={sent_target} state_before=300 state_after={state_after} "
        f"rtt_ms={rtt_ms}\n"
        f"TASK3_EXPERIMENT_COMPLETE run_mode={mode} samples=1 elapsed_ms=2"
    )
    return BASE_METRICS.parse_log(log, mode)


if __name__ == "__main__":
    unittest.main()
