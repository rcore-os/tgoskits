"""Validate routing facts, never reimplement Rust package selection."""
import copy
from pathlib import Path
import subprocess
import tempfile
import unittest

from ci_clippy import classify, result_succeeded, selection_outputs


class ClippyModeTests(unittest.TestCase):
    def test_classification_requires_planning_when_comparison_is_uncertain(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            def git(*args):
                return subprocess.check_output(
                    ["git", "-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid", *args],
                    cwd=root, text=True, stderr=subprocess.PIPE,
                ).strip()

            git("init", "-b", "main")
            (root / "source.rs").write_text("pub fn operation() {}\n")
            git("add", ".")
            git("commit", "-m", "base")
            base = git("rev-parse", "HEAD")
            self.assertTrue(classify(root, base)[0])
            git("commit", "--allow-empty", "-m", "identical inputs")
            self.assertFalse(classify(root, base)[0])
            docs = root / "docs/docs/guide.md"
            docs.parent.mkdir(parents=True)
            docs.write_text("static documentation\n")
            git("add", ".")
            git("commit", "-m", "docs")
            self.assertFalse(classify(root, base)[0])
            (root / "future-input.cfg").write_text("unknown input\n")
            git("add", ".")
            git("commit", "-m", "unknown input")
            self.assertTrue(classify(root, base)[0])
            for since in ["HEAD", "missing-ref", "1" * 40]:
                with self.subTest(since=since):
                    self.assertTrue(classify(root, since)[0])

    def test_selection_json_rejects_wrong_identity_and_contradictory_modes(self):
        value = {"schema_version": 1, "mode": "incremental", "tested_sha": "a" * 40,
                 "diff_base_sha": "b" * 40, "selection_digest": "c" * 64, "checks": ["fixture"]}
        self.assertEqual(selection_outputs(value, "a" * 40)["mode"], "incremental")
        for field, bad in [("schema_version", 99), ("mode", "unknown"),
                           ("tested_sha", "d" * 40), ("diff_base_sha", "branch"),
                           ("diff_base_sha", "a" * 40),
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
        value["diff_base_sha"] = "a" * 40
        with self.assertRaises(ValueError):
            selection_outputs(value, "a" * 40)
        value["mode"] = "full"
        value["diff_base_sha"] = None
        with self.assertRaises(ValueError):
            selection_outputs(value, "a" * 40)
        value["checks"] = ["fixture"]
        self.assertEqual(selection_outputs(value, "a" * 40)["base"], "")

    def test_final_gate_requires_the_actual_path_to_finish_successfully(self):
        for mode in ["incremental", "empty", "full", "unknown", ""]:
            for prepare in ["success", "failure", "cancelled", "skipped"]:
                for checks in ["success", "failure", "cancelled", "skipped"]:
                    expected = prepare == "success" and ((mode in {"incremental", "empty"} and checks == "skipped") or (mode == "full" and checks == "success"))
                    with self.subTest(mode=mode, prepare=prepare, checks=checks):
                        self.assertEqual(result_succeeded(mode, prepare, checks), expected)
