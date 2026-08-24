#!/usr/bin/env python3
"""Regression tests for P1 attempt reliability summaries."""

import unittest

from importlib.machinery import SourceFileLoader
from pathlib import Path


SCRIPT = Path(__file__).with_name("summarize-p1-attempts.py")
MODULE = SourceFileLoader("summarize_p1_attempts", str(SCRIPT)).load_module()


class SummarizeP1AttemptsTest(unittest.TestCase):
    def test_counts_retries_separately_from_accepted_runs(self):
        log = "\n".join(
            (
                "P1_ATTEMPT_START label=baseline/run-01 attempt=1",
                "P1_ATTEMPT_RETRY label=baseline/run-01 attempt=1",
                "P1_ATTEMPT_START label=baseline/run-01 attempt=2",
                "P1_ATTEMPT_ACCEPTED label=baseline/run-01 attempt=2",
                "P1_ATTEMPT_START label=modified/run-01 attempt=1",
                "P1_ATTEMPT_ACCEPTED label=modified/run-01 attempt=1",
            )
        )

        summary = MODULE.summarize(log)

        self.assertIn("baseline_total_attempts=2\n", summary)
        self.assertIn("baseline_attempt_acceptance_rate=0.500000\n", summary)
        self.assertIn("modified_total_attempts=1\n", summary)
        self.assertIn("total_failed_attempts=1\n", summary)

    def test_rejects_an_incomplete_batch(self):
        with self.assertRaisesRegex(ValueError, "incomplete runs"):
            MODULE.summarize(
                "P1_ATTEMPT_START label=baseline/run-01 attempt=1\n"
                "P1_ATTEMPT_ACCEPTED label=baseline/run-01 attempt=1\n"
                "P1_ATTEMPT_START label=modified/run-01 attempt=1\n"
            )


if __name__ == "__main__":
    unittest.main()
