#!/usr/bin/env python3
"""Regression checks for the deterministic vIRQ overload replay."""

import importlib.util
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
MODEL_PATH = ROOT / "scripts/test/rt-partition/virq_overload_model.py"


def load_model():
    spec = importlib.util.spec_from_file_location("virq_overload_model", MODEL_PATH)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


class VirqOverloadModelTest(unittest.TestCase):
    def test_bounded_queue_reports_overflow_and_caps_depth(self):
        model = load_model()
        bounded = model.replay(
            capacity=4,
            arrival_interval_us=10,
            service_interval_us=100,
            duration_us=100,
        )
        unbounded = model.replay(
            capacity=None,
            arrival_interval_us=10,
            service_interval_us=100,
            duration_us=100,
        )

        self.assertLessEqual(bounded.max_queue_depth, 4)
        self.assertGreater(bounded.overflow, 0)
        self.assertEqual(unbounded.overflow, 0)
        self.assertEqual(unbounded.accepted, unbounded.arrivals)
        self.assertGreater(unbounded.max_queue_depth, bounded.max_queue_depth)


if __name__ == "__main__":
    unittest.main()
