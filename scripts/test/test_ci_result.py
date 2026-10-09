#!/usr/bin/env python3

import tempfile
import unittest
from pathlib import Path

import ci_result


class CiResultTests(unittest.TestCase):
    def test_parse_stages_accepts_workflow_separators(self) -> None:
        self.assertEqual(
            ci_result.parse_stages("Plan=success\nChecks=success"),
            [("Plan", "success"), ("Checks", "success")],
        )
        self.assertEqual(
            ci_result.required_stage_labels("Plan,Checks", [("Plan", "success"), ("Checks", "success")]),
            {"Plan", "Checks"},
        )

    def test_required_stages_default_to_all_and_fail_closed(self) -> None:
        stages = [("Plan", "success"), ("Checks", "skipped")]
        self.assertEqual(ci_result.required_stage_labels("", stages), {"Plan", "Checks"})
        with self.assertRaisesRegex(ValueError, "missing"):
            ci_result.required_stage_labels("Unknown", stages)
        with self.assertRaisesRegex(ValueError, "duplicate"):
            ci_result.parse_stages("Plan=success;Plan=failure")

    def test_render_summary_escapes_table_values(self) -> None:
        summary = ci_result.render_summary(
            "CI | Result", "abc", "tested revision", [("Plan", "success|ok")]
        )
        self.assertIn("## CI | Result", summary)
        self.assertIn("| Plan | success\\|ok |", summary)

    def test_main_returns_failure_for_required_non_success(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "summary"
            status = ci_result.main(
                [
                    "--title",
                    "CI",
                    "--stages",
                    "Plan=success,Checks=cancelled",
                    "--summary-file",
                    str(output),
                ]
            )
            self.assertEqual(status, 1)
            self.assertIn("Checks", output.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
