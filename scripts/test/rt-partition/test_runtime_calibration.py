#!/usr/bin/env python3
"""Regression tests for per-scenario TCG runtime calibration."""

import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
SCRIPT = ROOT / "scripts/test/rt-partition/calibrate-runtime-scale.py"
CALIBRATION_RUNNER = ROOT / "scripts/test/rt-partition/run-runtime-calibration.sh"


class RuntimeCalibrationTest(unittest.TestCase):
    def run_calibration(self, progress: str, *extra_args: str):
        with tempfile.TemporaryDirectory() as directory:
            temp = Path(directory)
            progress_path = temp / "progress.txt"
            output_path = temp / "runtime-scales.env"
            progress_path.write_text(progress)
            result = subprocess.run(
                [
                    "python3",
                    str(SCRIPT),
                    "--output",
                    str(output_path),
                    *extra_args,
                    f"stress-rt={progress_path}",
                ],
                text=True,
                capture_output=True,
                check=False,
            )
            output = output_path.read_text() if output_path.exists() else ""
            return result, output

    def test_adds_safety_margin_and_rounds_up(self):
        result, output = self.run_calibration(
            "markers=12\nhost_elapsed_s=240.0\nguest_elapsed_s=110.0\n"
            "guest_wall_ratio=0.458333333\n",
            "--minimum-guest-seconds",
            "100",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("stress-rt=3", output)

    def test_rejects_too_short_calibration_window(self):
        result, _ = self.run_calibration(
            "markers=3\nhost_elapsed_s=20.0\nguest_elapsed_s=20.0\n"
            "guest_wall_ratio=1.0\n",
            "--minimum-guest-seconds",
            "100",
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("too short", result.stderr)

    def test_calibration_runner_uses_120_guest_seconds_for_all_scenarios(self):
        script = CALIBRATION_RUNNER.read_text()
        self.assertIn('calibration_duration_sec="${RT_CALIBRATION_DURATION_SEC:-120}"', script)
        self.assertIn("idle stress-noiso stress-dedicated stress-rt", script)
        self.assertIn("calibrate-runtime-scale.py", script)


if __name__ == "__main__":
    unittest.main()
