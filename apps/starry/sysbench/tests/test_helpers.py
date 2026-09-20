#!/usr/bin/env python3
"""Exercise the real probe executables, optionally through a qemu-user runner."""
import argparse
import os
from pathlib import Path
import subprocess
import unittest

parser = argparse.ArgumentParser()
parser.add_argument('--bin-dir', type=Path, required=True)
parser.add_argument('--runner', nargs='*', default=[])
args, unittest_args = parser.parse_known_args()


class ProbeContract(unittest.TestCase):
    def run_probe(self, name, *arguments, cpu=None):
        pin = None if cpu is None else lambda: os.sched_setaffinity(0, {cpu})
        return subprocess.run(
            [*args.runner, str(args.bin_dir / name), *arguments],
            capture_output=True, text=True, timeout=10, preexec_fn=pin,
        )

    def test_invalid_measurements_are_rejected(self):
        unavailable = next(cpu for cpu in range(1024) if cpu not in os.sched_getaffinity(0))
        for name, arguments in [
            ('cpuprobe', [str(unavailable)]),
            ('cpuprobe', ['not-a-cpu']),
            ('membw', [str(unavailable), '1']),
            ('membw', [str(min(os.sched_getaffinity(0))), '0']),
        ]:
            with self.subTest(name=name, arguments=arguments):
                result = self.run_probe(name, *arguments)
                self.assertNotEqual(result.returncode, 0, result.stdout)

    def test_measurements_run_on_the_requested_cpu(self):
        cpu = min(os.sched_getaffinity(0))
        for name, arguments in [('cpuprobe', [str(cpu)]), ('membw', [str(cpu), '1'])]:
            with self.subTest(name=name):
                result = self.run_probe(name, *arguments)
                self.assertEqual(result.returncode, 0, result.stderr)
                fields = dict(item.split('=', 1) for item in result.stdout.split() if '=' in item)
                self.assertEqual(int(fields['landed']), cpu)
                self.assertGreater(float(fields['sec' if name == 'cpuprobe' else 'memcpy_GBps']), 0)

    def test_cpu_discovery_respects_the_callers_affinity(self):
        cpu = min(os.sched_getaffinity(0))
        result = self.run_probe('cpuprobe', '--list', cpu=cpu)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(cpu))


if __name__ == '__main__':
    unittest.main(argv=[__file__, *unittest_args])
