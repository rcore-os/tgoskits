"""Behavioral tests of Clippy gating and its authoritative shard matrix."""
import tempfile
import subprocess
import unittest
from pathlib import Path

import ci_clippy


class ClippyPlanningTests(unittest.TestCase):
    def test_only_known_static_documentation_can_skip_full_clippy(self):
        self.assertFalse(ci_clippy.needs_clippy(["docs/docs/topic.md", "docs/blog/post.md"]))
        for path in ["README.md", "components/example/README.md", "tools/helper.py", ".github/workflows/ci.yml", "Cargo.lock", "crate/src/main.rs", "crate/tests/flow.rs", "unrecognized.bin"]:
            with self.subTest(path=path):
                self.assertTrue(ci_clippy.needs_clippy(["docs/docs/topic.md", path]))
        with tempfile.TemporaryDirectory() as directory:
            self.assertTrue(ci_clippy.classify(Path(directory), "missing-history")[0])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def git(*arguments):
                return subprocess.check_output(["git", "-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid", *arguments], cwd=root, text=True, stderr=subprocess.PIPE).strip()
            git("init", "-b", "fixture-base")
            page = root / "docs/docs/topic.md"
            page.parent.mkdir(parents=True)
            page.write_text("first version")
            git("add", ".")
            git("commit", "-m", "initial")
            base = git("rev-parse", "HEAD")
            page.write_text("documentation only")
            git("commit", "-am", "documentation")
            self.assertFalse(ci_clippy.classify(root, base)[0])
            # A fallback base at HEAD does not prove the real change was irrelevant.
            self.assertTrue(ci_clippy.classify(root, "HEAD")[0])
            source = root / "src/lib.rs"
            source.parent.mkdir()
            source.write_text("pub fn operation() {}")
            git("add", ".")
            git("commit", "-m", "code")
            self.assertTrue(ci_clippy.classify(root, base)[0])
            # A ref ahead of HEAD can also produce a merge base equal to HEAD.
            ahead = git("rev-parse", "HEAD")
            git("checkout", "HEAD~1")
            self.assertTrue(ci_clippy.classify(root, ahead)[0])

    def test_matrix_uses_every_planned_check_once_and_rejects_partial_plans(self):
        plan = {"schema_version": 1, "checks": ["a", "b", "c"], "shards": [[0, 2], [1]]}
        self.assertEqual(ci_clippy.matrix(plan), {"include": [{"shard": 1}, {"shard": 2}]})
        for shards in [[[0], [1]], [[0, 1], [1, 2]], [[0, 1, 2], []], [[0, 1, 2, 3]]]:
            with self.subTest(shards=shards), self.assertRaises(ValueError):
                ci_clippy.matrix(dict(plan, shards=shards))


if __name__ == "__main__":
    unittest.main()
