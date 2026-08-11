#!/usr/bin/env python3
"""Deterministic regression tests for the rsext4 Linux map audit."""

from __future__ import annotations

import copy
import importlib.util
import pathlib
import sys
import unittest


SCRIPT = pathlib.Path(__file__).with_name("check_rsext4_linux_map.py")
SPEC = importlib.util.spec_from_file_location("check_rsext4_linux_map", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
AUDIT = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = AUDIT
SPEC.loader.exec_module(AUDIT)


def valid_manifest() -> dict:
    return {
        "schema": 1,
        "linux_commit": AUDIT.LINUX_COMMIT,
        "source_roots": list(AUDIT.SOURCE_ROOTS),
        "files": [
            {
                "path": "fs/ext4/a.c",
                "blob": "0" * 40,
                "line_count": 4,
                "review_status": "reviewed",
                "segments": [
                    {
                        "start": 1,
                        "end": 4,
                        "symbol": "file",
                        "classification": "core",
                        "state_machine": "none",
                        "invariant": "all lines are classified",
                        "rust_owner": "rsext4::test",
                        "reason": "test fixture",
                        "test_ids": ["linux-map-complete"],
                    }
                ],
            }
        ],
    }


class ManifestAuditTests(unittest.TestCase):
    def test_valid_manifest_covers_every_line(self) -> None:
        summary = AUDIT.validate_manifest(valid_manifest(), require_reviewed=True)
        self.assertEqual(summary.files, 1)
        self.assertEqual(summary.lines, 4)

    def test_gap_is_rejected(self) -> None:
        manifest = valid_manifest()
        segment = copy.deepcopy(manifest["files"][0]["segments"][0])
        manifest["files"][0]["segments"] = [
            {**segment, "end": 2},
            {**segment, "start": 4},
        ]
        with self.assertRaisesRegex(AUDIT.MapAuditError, "expected next segment"):
            AUDIT.validate_manifest(manifest)

    def test_overlap_is_rejected(self) -> None:
        manifest = valid_manifest()
        segment = copy.deepcopy(manifest["files"][0]["segments"][0])
        manifest["files"][0]["segments"] = [
            {**segment, "end": 2},
            {**segment, "start": 2},
        ]
        with self.assertRaisesRegex(AUDIT.MapAuditError, "expected next segment"):
            AUDIT.validate_manifest(manifest)

    def test_coarse_file_is_rejected_by_final_gate(self) -> None:
        manifest = valid_manifest()
        manifest["files"][0]["review_status"] = "coarse"
        with self.assertRaisesRegex(AUDIT.MapAuditError, "remain coarse"):
            AUDIT.validate_manifest(manifest, require_reviewed=True)

    def test_wrong_commit_is_rejected(self) -> None:
        manifest = valid_manifest()
        manifest["linux_commit"] = "1" * 40
        with self.assertRaisesRegex(AUDIT.MapAuditError, "frozen baseline"):
            AUDIT.validate_manifest(manifest)


if __name__ == "__main__":
    unittest.main()
