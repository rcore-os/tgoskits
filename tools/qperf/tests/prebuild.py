#!/usr/bin/env python3
"""Exercise the local qperf entrypoint without downloading a harness checkout."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[3]
PREBUILD = ROOT / "apps/qperf/prebuild.sh"


class PrebuildTests(unittest.TestCase):
    def run_prebuild(self, *args):
        with tempfile.TemporaryDirectory(prefix="qperf-entrypoint-") as directory:
            env = os.environ.copy()
            # An external checkout must neither be validated nor fetched.
            env["TGOSKIT_HARNESS_KIT_DIR"] = str(Path(directory) / "absent")
            return subprocess.run(
                [str(PREBUILD), *args], cwd=directory, env=env,
                capture_output=True, text=True, timeout=10,
            )

    def test_prints_local_root_from_another_directory(self):
        result = self.run_prebuild()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, str(ROOT) + "\n")

    def test_runs_relative_commands_at_local_root(self):
        result = self.run_prebuild(
            "sh", "-c", 'test -f tools/qperf/Cargo.toml && printf "%s" "$1"',
            "qperf-test", "argument with spaces",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "argument with spaces")

    def test_propagates_command_failure(self):
        result = self.run_prebuild("sh", "-c", "exit 23")
        self.assertEqual(result.returncode, 23, result.stderr)


if __name__ == "__main__":
    unittest.main()
