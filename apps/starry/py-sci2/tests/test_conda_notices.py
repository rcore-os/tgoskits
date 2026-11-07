#!/usr/bin/env python3
"""Host regression for conda in-process failure propagation and diagnostics."""

import json
import os
import sys
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))

from conda_notices import (  # noqa: E402
    CONDA_OFFLINE_ADAPTER_MARKER,
    CONDA_OFFLINE_DIAGNOSTIC,
    describe_output,
    invoke_inproc,
    notices_result_ok,
    run_offline,
    temporary_env,
    verify_notices,
)


class CondaNoticesResultTest(unittest.TestCase):
    def test_offline_result_and_environment_restoration(self):
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

            self.assertTrue(notices_result_ok(rc, output))

            def failing_run(args, timeout):
                raise RuntimeError("fixture failure")

            with self.assertRaises(RuntimeError):
                run_offline(failing_run, ["notices"])
            self.assertNotIn("CONDA_OFFLINE", os.environ)

            os.environ["CONDA_OFFLINE"] = "previous"
            with self.assertRaises(RuntimeError):
                run_offline(failing_run, ["notices"])
            self.assertEqual(os.environ.get("CONDA_OFFLINE"), "previous")
        finally:
            if previous is None:
                os.environ.pop("CONDA_OFFLINE", None)
            else:
                os.environ["CONDA_OFFLINE"] = previous

        previous_pkgs = os.environ.get("CONDA_PKGS_DIRS")
        os.environ["CONDA_PKGS_DIRS"] = "/previous/pkgs"
        try:
            with self.assertRaises(RuntimeError):
                with temporary_env("CONDA_PKGS_DIRS", "/isolated/pkgs"):
                    self.assertEqual(
                        os.environ.get("CONDA_PKGS_DIRS"), "/isolated/pkgs"
                    )
                    raise RuntimeError("fixture failure")
            self.assertEqual(
                os.environ.get("CONDA_PKGS_DIRS"), "/previous/pkgs"
            )
        finally:
            if previous_pkgs is not None:
                os.environ["CONDA_PKGS_DIRS"] = previous_pkgs
            else:
                os.environ.pop("CONDA_PKGS_DIRS", None)

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

    def test_swallowed_timeout_is_reported_as_timeout(self):
        """Component boundary: caller records the alarm; not kernel proof."""
        class InprocTimeout(Exception):
            pass

        class TimeoutState:
            expired = False

        state = TimeoutState()
        timeout_message = (
            "TIMEOUT: conda clean --all --dry-run --json exceeded 300s"
        )

        def swallowed_timeout_main(*args):
            try:
                state.expired = True
                raise InprocTimeout("deadline")
            except InprocTimeout:
                return 1

        self.assertEqual(
            invoke_inproc(
                swallowed_timeout_main,
                ["clean", "--all", "--dry-run", "--json"],
                timeout_state=state,
                timeout_message=timeout_message,
            ),
            (124, "", timeout_message),
        )

    def test_failed_json_report_exposes_locator_fields(self):
        report = json.dumps(
            {
                "error": "TimeoutError()",
                "exception_name": "_InprocTimeout",
                "exception_type": "<class '_InprocTimeout'>",
                "command": "conda clean --all --dry-run --json",
                "traceback": "File main_clean.py\\n_InprocTimeout",
            }
        )

        diagnostic = describe_output(1, report, "")

        for expected in (
            '"_InprocTimeout"',
            '"TimeoutError()"',
            "main_clean.py",
            '"conda clean --all --dry-run --json"',
        ):
            self.assertIn(expected, diagnostic)

        self.assertEqual(
            describe_output(0, '{\n  "success": true\n}', ""),
            "rc=0 stdout='{' stderr='<no output>'",
        )

    def test_inproc_value_error_reaches_the_notices_check_as_failure_and_recovers(self):
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

        def succeeding_main(*args):
            return 0

        self.assertEqual(invoke_inproc(succeeding_main, ["notices"]), (0, "", ""))


if __name__ == "__main__":
    unittest.main()
