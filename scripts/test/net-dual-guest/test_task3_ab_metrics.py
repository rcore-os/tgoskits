#!/usr/bin/env python3
"""Regression tests for the integrated Task-3 manual/YOLO metrics."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("task3_ab_metrics.py")
SPEC = importlib.util.spec_from_file_location("task3_ab_metrics", MODULE_PATH)
assert SPEC and SPEC.loader
METRICS = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = METRICS
SPEC.loader.exec_module(METRICS)


class Task3AbMetricsTests(unittest.TestCase):
    def test_parses_manual_and_yolo_through_the_same_control_status_chain(self) -> None:
        manual = "\n".join(
            (
                "TASK3_SAMPLE sample=1 image_id=vase-left image_sha256=abc "
                "truth_target=321 expected=accept source=manual outcome=accepted request=1 elapsed_ms=0",
                "TASK3_CONTROL_SENT elapsed_ms=0 sample=1 image_id=vase-left source=manual "
                "request=1 value=500 truth_target=321 state=300 seq=1",
                "TASK3_STATUS_RECEIVED elapsed_ms=20 sample=1 image_id=vase-left request=1 "
                "value=500 state_before=300 state_after=355 rtt_ms=20",
                "TASK3_EXPERIMENT_COMPLETE run_mode=manual samples=1 elapsed_ms=20",
            )
        )
        yolo = "\n".join(
            (
                "TASK3_SAMPLE sample=1 image_id=vase-left image_sha256=abc "
                "truth_target=321 expected=accept source=yolo outcome=accepted request=1 elapsed_ms=1500",
                "TASK3_INFER model=yolo11n.ncnn source=yolo infer_us=1499000 request=1 elapsed_ms=1500",
                "TASK3_DETECTION model=yolo11n.ncnn class=75 confidence_milli=828 "
                "center_x_milli=321 area_milli=63 target=400 request=1",
                "TASK3_CONTROL_SENT elapsed_ms=1500 sample=1 image_id=vase-left source=yolo "
                "request=1 value=400 truth_target=321 state=300 seq=1",
                "TASK3_STATUS_RECEIVED elapsed_ms=1518 sample=1 image_id=vase-left request=1 "
                "value=400 state_before=300 state_after=320 rtt_ms=18",
                "TASK3_EXPERIMENT_COMPLETE run_mode=yolo samples=1 elapsed_ms=1518",
            )
        )

        manual_run = METRICS.parse_log(manual, "manual")
        yolo_run = METRICS.parse_log(yolo, "yolo")

        self.assertEqual(manual_run.samples[0].sent_target, 500)
        self.assertEqual(manual_run.samples[0].state_after, 355)
        self.assertEqual(yolo_run.samples[0].detected_target, 321)
        self.assertEqual(yolo_run.samples[0].infer_us, 1_499_000)
        self.assertEqual(METRICS.summarize(yolo_run)["perception_mae"], 0.0)
        self.assertEqual(METRICS.summarize(yolo_run)["target_mae"], 79.0)

    def test_counts_expected_safe_rejection_without_requiring_control(self) -> None:
        log = "\n".join(
            (
                "TASK3_SAMPLE sample=4 image_id=no-target image_sha256=def truth_target=none "
                "expected=reject source=yolo outcome=rejected request=4 elapsed_ms=7000",
                "TASK3_INFER model=yolo11n.ncnn source=yolo infer_us=1499000 "
                "request=4 elapsed_ms=7000",
                "TASK3_MODEL_REJECTED model=yolo11n.ncnn reason=Perception(LowConfidence) "
                "action=safe request=4 elapsed_ms=7000",
                "TASK3_EXPERIMENT_COMPLETE run_mode=yolo samples=1 elapsed_ms=7000",
            )
        )

        run = METRICS.parse_log(log, "yolo")
        summary = METRICS.summarize(run)

        self.assertIsNone(run.samples[0].sent_target)
        self.assertEqual(summary["safe_rejection_rate"], 1.0)
        self.assertEqual(summary["expected_behavior_accuracy"], 1.0)

    def test_manual_run_does_not_claim_model_acceptance_or_rejection_accuracy(self) -> None:
        log = "\n".join(
            (
                "TASK3_SAMPLE sample=1 image_id=no-target image_sha256=def truth_target=none "
                "expected=reject source=manual outcome=accepted request=1 elapsed_ms=0",
                "TASK3_CONTROL_SENT elapsed_ms=0 sample=1 image_id=no-target source=manual "
                "request=1 value=500 truth_target=none state=300 seq=1",
                "TASK3_STATUS_RECEIVED elapsed_ms=20 sample=1 image_id=no-target request=1 "
                "value=500 state_before=300 state_after=355 rtt_ms=20",
                "TASK3_EXPERIMENT_COMPLETE run_mode=manual samples=1 elapsed_ms=20",
            )
        )

        summary = METRICS.summarize(METRICS.parse_log(log, "manual"))

        self.assertIsNone(summary["expected_behavior_accuracy"])
        self.assertIsNone(summary["safe_rejection_rate"])

    def test_parses_uart_lines_prefixed_by_ansi_reset(self) -> None:
        log = "\n".join(
            (
                "\x1b[mTASK3_SAMPLE sample=1 image_id=vase-left image_sha256=abc "
                "truth_target=321 expected=accept source=manual outcome=accepted "
                "request=1 elapsed_ms=0\r",
                "\x1b[mTASK3_CONTROL_SENT elapsed_ms=0 sample=1 image_id=vase-left "
                "source=manual request=1 value=500 truth_target=321 state=300 seq=1\r",
                "\x1b[mTASK3_STATUS_RECEIVED elapsed_ms=20 sample=1 image_id=vase-left "
                "request=1 value=500 state_before=300 state_after=355 rtt_ms=20\r",
                "\x1b[mTASK3_EXPERIMENT_COMPLETE run_mode=manual samples=1 elapsed_ms=20\r",
            )
        )

        run = METRICS.parse_log(log, "manual")

        self.assertEqual(run.samples[0].image_id, "vase-left")
        self.assertEqual(run.samples[0].rtt_ms, 20)

    def test_rejects_incomplete_or_cross_mode_sample_identity(self) -> None:
        incomplete = "TASK3_SAMPLE sample=1 image_id=vase-left image_sha256=abc truth_target=321 expected=accept source=manual outcome=accepted request=1 elapsed_ms=0"
        with self.assertRaises(ValueError):
            METRICS.parse_log(incomplete, "manual")

        manual = METRICS.parse_log(
            incomplete
            + "\nTASK3_CONTROL_SENT elapsed_ms=0 sample=1 image_id=vase-left "
            "source=manual request=1 value=500 truth_target=321 state=300 seq=1"
            + "\nTASK3_STATUS_RECEIVED elapsed_ms=1 sample=1 image_id=vase-left "
            "request=1 value=500 state_before=300 state_after=355 rtt_ms=1"
            + "\nTASK3_EXPERIMENT_COMPLETE run_mode=manual samples=1 elapsed_ms=1",
            "manual",
        )
        yolo = METRICS.parse_log(
            incomplete.replace("source=manual", "source=yolo").replace(
                "image_sha256=abc", "image_sha256=changed"
            )
            + "\nTASK3_INFER model=yolo11n.ncnn source=yolo infer_us=1499000 "
            "request=1 elapsed_ms=1"
            + "\nTASK3_DETECTION model=yolo11n.ncnn class=75 confidence_milli=828 "
            "center_x_milli=321 area_milli=63 target=400 request=1"
            + "\nTASK3_CONTROL_SENT elapsed_ms=1 sample=1 image_id=vase-left "
            "source=yolo request=1 value=400 truth_target=321 state=300 seq=1"
            + "\nTASK3_STATUS_RECEIVED elapsed_ms=2 sample=1 image_id=vase-left "
            "request=1 value=400 state_before=300 state_after=320 rtt_ms=1"
            + "\nTASK3_EXPERIMENT_COMPLETE run_mode=yolo samples=1 elapsed_ms=1",
            "yolo",
        )
        with self.assertRaises(ValueError):
            METRICS.verify_comparable(manual, yolo)

    def test_rejects_control_or_status_for_another_request(self) -> None:
        base = "\n".join(
            (
                "TASK3_SAMPLE sample=1 image_id=vase-left image_sha256=abc "
                "truth_target=321 expected=accept source=manual outcome=accepted request=1 elapsed_ms=0",
                "TASK3_CONTROL_SENT elapsed_ms=0 sample=1 image_id=vase-left source=manual "
                "request=1 value=500 truth_target=321 state=300 seq=1",
                "TASK3_STATUS_RECEIVED elapsed_ms=20 sample=1 image_id=vase-left request=1 "
                "value=500 state_before=300 state_after=355 rtt_ms=20",
                "TASK3_EXPERIMENT_COMPLETE run_mode=manual samples=1 elapsed_ms=20",
            )
        )

        with self.assertRaisesRegex(ValueError, "CONTROL request mismatch"):
            METRICS.parse_log(
                base.replace("source=manual request=1", "source=manual request=2"),
                "manual",
            )
        with self.assertRaisesRegex(ValueError, "STATUS request mismatch"):
            METRICS.parse_log(
                base.replace(
                    "image_id=vase-left request=1 value=500 state_before",
                    "image_id=vase-left request=2 value=500 state_before",
                ),
                "manual",
            )


if __name__ == "__main__":
    unittest.main()
