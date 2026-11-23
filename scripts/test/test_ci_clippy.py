"""Validate routing facts, never reimplement Rust package selection."""
import copy
import unittest

from ci_clippy import result_succeeded, selection_outputs


class ClippyModeTests(unittest.TestCase):
    def test_selection_json_rejects_wrong_identity_and_contradictory_modes(self):
        value = {"schema_version": 1, "mode": "incremental", "tested_sha": "a" * 40,
                 "diff_base_sha": "b" * 40, "selection_digest": "c" * 64, "checks": ["fixture"]}
        self.assertEqual(selection_outputs(value, "a" * 40)["mode"], "incremental")
        for field, bad in [("schema_version", 99), ("mode", "unknown"),
                           ("tested_sha", "d" * 40), ("diff_base_sha", "branch"),
                           ("selection_digest", "bad"), ("checks", [])]:
            candidate = copy.deepcopy(value)
            candidate[field] = bad
            with self.subTest(field=field), self.assertRaises(ValueError):
                selection_outputs(candidate, "a" * 40)
        value["mode"] = "empty"
        with self.assertRaises(ValueError):
            selection_outputs(value, "a" * 40)
        value["checks"] = []
        self.assertEqual(selection_outputs(value, "a" * 40)["mode"], "empty")
        value["mode"] = "full"
        value["diff_base_sha"] = None
        self.assertEqual(selection_outputs(value, "a" * 40)["base"], "")

    def test_final_gate_requires_the_actual_path_to_finish_successfully(self):
        for mode in ["incremental", "empty", "full", "unknown", ""]:
            for prepare in ["success", "failure", "cancelled", "skipped"]:
                for checks in ["success", "failure", "cancelled", "skipped"]:
                    expected = prepare == "success" and ((mode in {"incremental", "empty"} and checks == "skipped") or (mode == "full" and checks == "success"))
                    with self.subTest(mode=mode, prepare=prepare, checks=checks):
                        self.assertEqual(result_succeeded(mode, prepare, checks), expected)
