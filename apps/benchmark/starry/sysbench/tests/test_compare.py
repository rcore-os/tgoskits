#!/usr/bin/env python3
"""Exercise the public comparison CLI with complete and corrupted run logs."""
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "harness/compare.py"


def sample(rate=10):
    lines = ["SYSBENCH_BEGIN schema=1 mode=board seconds=3 prime=20000 cpus=1",
             "ENV affinity=0", "ENV bundle_sha256=" + "a" * 64]
    cases = {
        "probe-0": f"CPUPROBE req=0 landed=0 ips={rate}",
        "membw-0": "MEMBW core=0 landed=0 firsttouch_s=0.01 memcpy_GBps=1 read_GBps=2",
        "memory": "256 MiB transferred (100.00 MiB/sec)",
    }
    for name in ("pinned-0", "cpu-1", "threads", "mutex"):
        cases[name] = f"total number of events: {rate * 3}\ntotal time: 3.0000s"
    for name, body in cases.items():
        lines.extend([f"CASE_BEGIN {name}", body, f"CASE_END {name}"])
    return "\n".join([*lines, "SYSBENCH_DONE", ""])


class ComparisonTests(unittest.TestCase):
    def invoke(self, left, right):
        with tempfile.TemporaryDirectory() as directory:
            paths = [Path(directory) / name for name in ("linux", "starry")]
            for path, text in zip(paths, (left, right)):
                path.write_text(text)
            return subprocess.run(["python3", str(SCRIPT), *map(str, paths)],
                                  text=True, capture_output=True, check=False)

    def test_reports_measured_ratio_and_rejects_untrustworthy_runs(self):
        original = sample()
        result = self.invoke(original, sample(5))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("cpu-1\tevents_per_second\t10\t5\t0.5000", result.stdout)
        corruptions = [
            original.replace("SYSBENCH_DONE", ""),
            original.replace("landed=0", "landed=1"),
            original.replace("ips=10", "ips=nan"),
            original.replace("a" * 64, "b" * 64),
            original.replace("seconds=3", "seconds=5"),
            original.replace("CASE_END cpu-1", "CASE_END cpu-2"),
            original + "SYSBENCH_FAILED case=memory\n",
            original + original,
        ]
        for damaged in corruptions:
            with self.subTest(log=damaged):
                self.assertNotEqual(self.invoke(original, damaged).returncode, 0)


if __name__ == "__main__":
    unittest.main()
