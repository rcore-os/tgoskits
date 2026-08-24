from __future__ import annotations

import copy
import hashlib
import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


BENCHMARK_DIR = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "axvisor_rt_record_metadata", BENCHMARK_DIR / "record_metadata.py"
)
assert SPEC is not None and SPEC.loader is not None
record_metadata = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = record_metadata
SPEC.loader.exec_module(record_metadata)


class RepositoryStateTests(unittest.TestCase):
    def test_fingerprints_tracked_and_untracked_source_without_result_artifacts(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory)
            self.run_git(workspace, "init", "-q")
            self.run_git(workspace, "config", "user.email", "benchmark@example.invalid")
            self.run_git(workspace, "config", "user.name", "Benchmark Test")
            (workspace / "tracked.txt").write_text("base\n", encoding="utf-8")
            self.run_git(workspace, "add", "tracked.txt")
            self.run_git(workspace, "commit", "-q", "-m", "base")

            clean = record_metadata.repository_state(workspace)
            self.assertFalse(clean["dirty"])
            self.assertEqual(clean["untracked_source_file_count"], 0)

            (workspace / "tracked.txt").write_text("changed\n", encoding="utf-8")
            (workspace / "source.py").write_text("VALUE = 1\n", encoding="utf-8")
            (workspace / "source-tree").mkdir()
            (workspace / "source-tree" / "main.py").write_text(
                "VALUE = 2\n", encoding="utf-8"
            )
            for index in range(128):
                generated = workspace / "source-tree" / "target" / f"artifact-{index}.o"
                generated.parent.mkdir(parents=True, exist_ok=True)
                generated.write_bytes(b"generated\n")
            for relative_path in (
                "source-tree/tmp/capture.log",
                "source-tree/build/output.bin",
                "source-tree/build-debug/output.bin",
                "source-tree/__pycache__/main.pyc",
            ):
                generated = workspace / relative_path
                generated.parent.mkdir(parents=True, exist_ok=True)
                generated.write_bytes(b"generated\n")

            result = workspace / "competition" / "results" / "capture.json"
            weekly = workspace / "docs" / "competition" / "weekly.md"
            result.parent.mkdir(parents=True)
            weekly.parent.mkdir(parents=True)
            result.write_text("measurement\n", encoding="utf-8")
            weekly.write_text("unrelated user note\n", encoding="utf-8")

            changed = record_metadata.repository_state(workspace)
            self.assertTrue(changed["dirty"])
            self.assertEqual(changed["untracked_source_file_count"], 2)
            self.assertNotEqual(
                changed["source_snapshot_sha256"], clean["source_snapshot_sha256"]
            )

            excluded = record_metadata.repository_state(workspace)
            self.assertEqual(
                excluded["source_snapshot_sha256"], changed["source_snapshot_sha256"]
            )
            self.assertEqual(excluded["untracked_source_file_count"], 2)

            (workspace / "source.py").write_text("VALUE = 2\n", encoding="utf-8")
            updated = record_metadata.repository_state(workspace)
            self.assertNotEqual(
                updated["source_snapshot_sha256"], changed["source_snapshot_sha256"]
            )

    def test_repository_fingerprint_fails_closed_outside_git(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(record_metadata.MetadataError, "git"):
                record_metadata.repository_state(Path(directory))

    def test_finalize_preserves_pre_run_provenance(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            raw_log = Path(directory) / "raw-console.log"
            raw_log.write_bytes(b"complete capture\n")
            running = {
                "schema_version": 1,
                "status": "capture_running",
                "finished_at": None,
                "repository": {"source_snapshot_sha256": "1" * 64},
                "qemu": {"exit_code": None},
                "artifacts": {
                    "injected_rootfs_pre_run": {
                        "path": "/capture/rootfs.img",
                        "sha256": "2" * 64,
                    }
                },
            }
            original = copy.deepcopy(running)

            finalized = record_metadata.finalize_metadata(
                running,
                finished_at="2026-07-31T00:01:00Z",
                exit_code=0,
                raw_log=raw_log,
            )

            self.assertEqual(running, original)
            self.assertEqual(finalized["status"], "capture_complete")
            self.assertEqual(finalized["finished_at"], "2026-07-31T00:01:00Z")
            self.assertEqual(finalized["qemu"]["exit_code"], 0)
            self.assertEqual(
                finalized["artifacts"]["raw_log"]["sha256"],
                hashlib.sha256(raw_log.read_bytes()).hexdigest(),
            )
            self.assertEqual(finalized["repository"], original["repository"])
            self.assertEqual(
                finalized["artifacts"]["injected_rootfs_pre_run"],
                original["artifacts"]["injected_rootfs_pre_run"],
            )

    def test_finalize_rejects_non_running_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            raw_log = Path(directory) / "raw-console.log"
            raw_log.write_bytes(b"capture\n")

            with self.assertRaisesRegex(record_metadata.MetadataError, "capture_running"):
                record_metadata.finalize_metadata(
                    {
                        "status": "planned",
                        "qemu": {"exit_code": None},
                        "artifacts": {},
                    },
                    finished_at="2026-07-31T00:01:00Z",
                    exit_code=0,
                    raw_log=raw_log,
                )

    @staticmethod
    def run_git(workspace: Path, *arguments: str) -> None:
        subprocess.run(
            ["git", "-C", str(workspace), *arguments],
            check=True,
            capture_output=True,
        )


if __name__ == "__main__":
    unittest.main()
