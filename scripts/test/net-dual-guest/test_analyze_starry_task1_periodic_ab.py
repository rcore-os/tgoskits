from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("analyze_starry_task1_periodic_ab.py")
SPEC = importlib.util.spec_from_file_location("analyze_starry_task1_periodic_ab", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class AnalyzeStarryTask1PeriodicTests(unittest.TestCase):
    def test_accepts_inference_marker_with_additional_stable_fields(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            run = Path(directory)
            samples = "".join(
                f"{sequence},{sequence},{sequence},{sequence},100\n"
                for sequence in range(300)
            )
            (run / "run.log").write_text(
                "use Round-robin scheduler.\n"
                "TASK3_MODEL_READY model=yolo11n.ncnn runtime=ncnn\n"
                "TASK3_INFER_STARTED model=yolo11n.ncnn request=1 phase=startup\n"
                "PERIODIC LATENCY START\n"
                + samples
                + "PERIODIC LATENCY COMPLETE samples=300\n"
                "TASK3_INFER model=yolo11n.ncnn source=yolo infer_us=20804665 "
                "request=1 elapsed_ms=0\n"
            )

            metrics = MODULE.read_run(run, "rr", "zephyr")

            self.assertEqual(metrics.infer_us, 20_804_665)


if __name__ == "__main__":
    unittest.main()
