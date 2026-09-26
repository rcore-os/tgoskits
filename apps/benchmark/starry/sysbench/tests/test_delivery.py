#!/usr/bin/env python3
"""A corrupt downloaded archive must fail the board application command."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class DeliveryTests(unittest.TestCase):
    def test_corrupt_archive_propagates_failure(self):
        script = Path(__file__).resolve().parents[1] / "init.sh"
        with tempfile.TemporaryDirectory() as directory:
            wget = Path(directory) / "wget"
            wget.write_text('#!/bin/sh\nprintf corrupt > "$4"\n')
            wget.chmod(0o755)
            env = dict(os.environ, PATH=directory + os.pathsep + os.environ["PATH"])
            result = subprocess.run(["sh", str(script)], env=env, text=True,
                                    capture_output=True, timeout=5, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("SYSBENCH_BOARD_FAILED", result.stdout)
        self.assertNotIn("SYSBENCH_DONE", result.stdout)


if __name__ == "__main__":
    unittest.main()
