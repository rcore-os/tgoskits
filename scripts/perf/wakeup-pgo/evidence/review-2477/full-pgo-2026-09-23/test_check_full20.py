"""Regression tests for archived full20 evidence identity checks."""

import importlib.util
import json
import shutil
import tempfile
import unittest
from pathlib import Path


HERE = Path(__file__).resolve().parent
CHECKER = HERE / "check-full20.py"


def load_checker(root):
    spec = importlib.util.spec_from_file_location("check_full20", CHECKER)
    checker = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(checker)
    checker.ROOT = root
    checker.BASELINE = root / "linux-rt-baseline.json"
    checker.OUTPUT = root / "resume668-comparison.json"
    return checker


class Full20ImageIdentityTests(unittest.TestCase):
    def test_rejects_run_with_wrong_tftp_image(self):
        for filename in ("resume666-a-status.json", "resume668-abba-status.json"):
            with self.subTest(filename=filename), tempfile.TemporaryDirectory() as directory:
                root = Path(directory) / "evidence"
                shutil.copytree(HERE, root, ignore=shutil.ignore_patterns("__pycache__"))
                checker = load_checker(root)
                checker.main()

                status_file = root / filename
                status = json.loads(status_file.read_text())
                status["runs"][0]["tftp_image_sha256"] = "0" * 64
                status_file.write_text(json.dumps(status))

                with self.assertRaises(AssertionError):
                    checker.main()


if __name__ == "__main__":
    unittest.main()
