#!/usr/bin/env python3
"""Host regression for conda notices failure propagation."""

import os
import sys
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))

from conda_notices import (  # noqa: E402
    CONDA_OFFLINE_ADAPTER_MARKER,
    CONDA_OFFLINE_DIAGNOSTIC,
    invoke_inproc,
    notices_result_ok,
    run_offline,
    verify_notices,
)


class CondaNoticesResultTest(unittest.TestCase):
    def test_success_and_explicit_offline_refusal_are_accepted(self):
        self.assertTrue(notices_result_ok(0, ""))

        def offline_run(args, timeout):
            self.assertEqual(args, ["notices"])
            self.assertEqual(timeout, 300)
            self.assertEqual(os.environ.get("CONDA_OFFLINE"), "1")
            return 1, (
                "OfflineError: %s https://example.invalid/notices.json.\n%s"
                % (CONDA_OFFLINE_ADAPTER_MARKER, CONDA_OFFLINE_DIAGNOSTIC)
            )

        previous = os.environ.pop("CONDA_OFFLINE", None)
        try:
            rc, output = run_offline(offline_run, ["notices"])
            self.assertNotIn("CONDA_OFFLINE", os.environ)
        finally:
            if previous is not None:
                os.environ["CONDA_OFFLINE"] = previous

        self.assertTrue(notices_result_ok(rc, output))

    def test_inproc_and_unexpected_nonzero_results_are_rejected(self):
        self.assertFalse(
            notices_result_ok(1, "INPROC ERROR: ValueError: invalid configuration")
        )
        self.assertFalse(notices_result_ok(1, "CondaError: invalid channel"))
        self.assertFalse(notices_result_ok(1, CONDA_OFFLINE_DIAGNOSTIC))
        self.assertFalse(notices_result_ok(2, CONDA_OFFLINE_DIAGNOSTIC))
        self.assertFalse(notices_result_ok(124, "TIMEOUT: conda notices exceeded 300s"))

    def test_system_exit_codes_are_normalized(self):
        cases = (
            (None, (0, "", "")),
            (0, (0, "", "")),
            (3, (3, "", "")),
            ("fatal", (1, "", "")),
        )
        for exit_code, expected in cases:
            with self.subTest(exit_code=exit_code):
                def exiting_main(*args, exit_code=exit_code):
                    raise SystemExit(exit_code)

                self.assertEqual(
                    invoke_inproc(exiting_main, ["notices"]), expected
                )

    def test_timeout_exception_is_rethrown(self):
        class InprocTimeout(Exception):
            pass

        def timing_out_main(*args):
            raise InprocTimeout("deadline")

        with self.assertRaises(InprocTimeout):
            invoke_inproc(
                timing_out_main, ["notices"], timeout_error=(InprocTimeout,)
            )

    def test_inproc_value_error_reaches_the_notices_check_as_failure(self):
        def crashing_main(*args):
            raise ValueError("invalid configuration")

        def crashing_run(args, timeout):
            self.assertEqual(args, ["notices"])
            self.assertEqual(timeout, 300)
            rc, stdout, stderr = invoke_inproc(crashing_main, args)
            self.assertEqual(rc, 1)
            self.assertEqual(stdout, "")
            self.assertEqual(
                stderr, "INPROC ERROR: ValueError: invalid configuration"
            )
            return rc, stdout + stderr

        checked = []

        passed = verify_notices(
            crashing_run, lambda condition, label: checked.append((condition, label))
        )

        self.assertFalse(passed)
        self.assertEqual(len(checked), 1)
        self.assertFalse(checked[0][0])


if __name__ == "__main__":
    unittest.main()
