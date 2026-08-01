#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0

"""Deterministic contract tests for native Zephyr evidence analysis."""

from __future__ import annotations

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPT_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPT_DIR))

import analyze  # noqa: E402


class CompletionValidationTests(unittest.TestCase):
    """Check the exact measured and warm-up miss bounds."""

    @staticmethod
    def completion(timer_misses: int, warmup_timer_misses: int) -> dict[str, str]:
        return {
            "schema": "1",
            "workload": "idle",
            "status": "pass",
            "timer_misses": str(timer_misses),
            "warmup_timer_misses": str(warmup_timer_misses),
            "early_wakes": "0",
        }

    def test_accepts_full_measured_window_boundary(self) -> None:
        completion = analyze.validate_complete(
            self.completion(timer_misses=10_000, warmup_timer_misses=99), "idle"
        )

        self.assertEqual(completion["timer_misses"], 10_000)
        self.assertEqual(completion["warmup_timer_misses"], 99)

    def test_rejects_more_misses_than_either_window_can_contain(self) -> None:
        invalid_counts = ((10_001, 0), (0, 100))

        for timer_misses, warmup_timer_misses in invalid_counts:
            with self.subTest(
                timer_misses=timer_misses,
                warmup_timer_misses=warmup_timer_misses,
            ):
                with self.assertRaisesRegex(
                    analyze.AnalysisError, "invalid timer miss count"
                ):
                    analyze.validate_complete(
                        self.completion(timer_misses, warmup_timer_misses), "idle"
                    )


class SourceProvenanceTests(unittest.TestCase):
    """Check that recorded provenance describes the named Git index exactly."""

    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary_directory.name)
        self.zephyr_base = self.root / "zephyr"
        self.zephyr_base.mkdir()
        self.index_path = self.root / "index"
        self.index_path.write_bytes(b"deterministic git index fixture\n")
        self.raw_log = self.root / "qemu.log"
        self.raw_log.write_text("measured run\n", encoding="utf-8")
        self.provenance_path = self.root / "source-provenance.json"

    def tearDown(self) -> None:
        self.temporary_directory.cleanup()

    def write_provenance(self, git_index: dict[str, object]) -> None:
        provenance = {
            "schema_version": 1,
            "zephyr_base": str(self.zephyr_base.resolve()),
            "source": {
                **analyze.EXPECTED_SOURCE,
                "worktree": "clean",
                "core_autocrlf": "unset",
            },
            "git_index": git_index,
        }
        self.provenance_path.write_text(
            json.dumps(provenance), encoding="utf-8"
        )
        os.utime(self.raw_log, (1_000, 1_000))
        os.utime(self.provenance_path, (1_001, 1_001))

    def load(self) -> tuple[dict[str, str], dict[str, object]]:
        with (
            mock.patch.object(
                analyze, "source_identity", return_value=analyze.EXPECTED_SOURCE
            ),
            mock.patch.object(
                analyze, "git_index_path", return_value=self.index_path
            ),
        ):
            return analyze.load_source_provenance(
                self.provenance_path, self.zephyr_base, self.raw_log
            )

    def test_accepts_exact_index_artifact(self) -> None:
        self.write_provenance(analyze.artifact(self.index_path, ".git/index"))

        source, provenance_artifact = self.load()

        self.assertEqual(source["worktree"], "clean")
        self.assertEqual(provenance_artifact["path"], "source-provenance.json")

    def test_rejects_index_metadata_for_a_differently_named_file(self) -> None:
        git_index = analyze.artifact(self.index_path, ".git/not-the-index")
        self.write_provenance(git_index)

        with self.assertRaisesRegex(
            analyze.AnalysisError, "index metadata does not match"
        ):
            self.load()

    def test_rejects_incorrect_index_byte_count(self) -> None:
        git_index = analyze.artifact(self.index_path, ".git/index")
        git_index["bytes"] = int(git_index["bytes"]) + 1
        self.write_provenance(git_index)

        with self.assertRaisesRegex(
            analyze.AnalysisError, "index metadata does not match"
        ):
            self.load()


if __name__ == "__main__":
    unittest.main()
