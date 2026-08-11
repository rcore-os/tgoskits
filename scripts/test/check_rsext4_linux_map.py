#!/usr/bin/env python3
"""Audit the frozen Linux 7.1 ext4/JBD2 source-interval map."""

from __future__ import annotations

import argparse
import json
import pathlib
import subprocess
import sys
from dataclasses import dataclass
from typing import Any


LINUX_COMMIT = "8cd9520d35a6c38db6567e97dd93b1f11f185dc6"
SOURCE_ROOTS = ("fs/ext4", "fs/jbd2")
CLASSIFICATIONS = {"core", "capability", "glue", "not-applicable"}
REVIEW_STATUSES = {"coarse", "reviewed"}
MANIFEST_PATH = pathlib.Path("scripts/test/data/rsext4-linux-7.1-map.json")


@dataclass(frozen=True)
class AuditSummary:
    files: int
    lines: int
    reviewed_files: int


class MapAuditError(ValueError):
    """A stable, user-facing manifest audit failure."""


def workspace_root() -> pathlib.Path:
    return pathlib.Path(__file__).resolve().parents[2]


def load_manifest(path: pathlib.Path) -> dict[str, Any]:
    try:
        contents = path.read_text(encoding="utf-8")
        manifest = json.loads(contents)
    except (OSError, json.JSONDecodeError) as error:
        raise MapAuditError(f"cannot load {path}: {error}") from error
    if not isinstance(manifest, dict):
        raise MapAuditError("manifest root must be an object")
    return manifest


def require_nonempty_string(value: Any, field: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise MapAuditError(f"{field} must be a non-empty string")
    return value


def validate_segment(path: str, segment: Any, expected_start: int) -> int:
    if not isinstance(segment, dict):
        raise MapAuditError(f"{path}: segment must be an object")

    start = segment.get("start")
    end = segment.get("end")
    if not isinstance(start, int) or not isinstance(end, int):
        raise MapAuditError(f"{path}: segment bounds must be integers")
    if start != expected_start:
        raise MapAuditError(
            f"{path}: expected next segment at line {expected_start}, got {start}"
        )
    if end < start:
        raise MapAuditError(f"{path}:{start}-{end}: invalid segment range")

    classification = segment.get("classification")
    if classification not in CLASSIFICATIONS:
        raise MapAuditError(
            f"{path}:{start}-{end}: invalid classification {classification!r}"
        )
    for field in ("symbol", "state_machine", "invariant", "rust_owner", "reason"):
        require_nonempty_string(segment.get(field), f"{path}:{start}-{end}.{field}")

    test_ids = segment.get("test_ids")
    if (
        not isinstance(test_ids, list)
        or not test_ids
        or any(not isinstance(test_id, str) or not test_id for test_id in test_ids)
    ):
        raise MapAuditError(f"{path}:{start}-{end}.test_ids must be non-empty")
    return end + 1


def validate_manifest(
    manifest: dict[str, Any], *, require_reviewed: bool = False
) -> AuditSummary:
    if manifest.get("schema") != 1:
        raise MapAuditError("unsupported or missing manifest schema")
    if manifest.get("linux_commit") != LINUX_COMMIT:
        raise MapAuditError("manifest Linux commit does not match the frozen baseline")
    if manifest.get("source_roots") != list(SOURCE_ROOTS):
        raise MapAuditError("manifest source_roots do not match fs/ext4 and fs/jbd2")

    files = manifest.get("files")
    if not isinstance(files, list) or not files:
        raise MapAuditError("manifest files must be a non-empty array")

    paths: list[str] = []
    line_total = 0
    reviewed_files = 0
    for file_entry in files:
        if not isinstance(file_entry, dict):
            raise MapAuditError("file entry must be an object")
        path = require_nonempty_string(file_entry.get("path"), "file.path")
        if not any(path == root or path.startswith(f"{root}/") for root in SOURCE_ROOTS):
            raise MapAuditError(f"{path}: outside the audited source roots")
        paths.append(path)

        blob = file_entry.get("blob")
        if (
            not isinstance(blob, str)
            or len(blob) != 40
            or any(character not in "0123456789abcdef" for character in blob)
        ):
            raise MapAuditError(f"{path}: blob must be a lowercase 40-hex Git object ID")

        line_count = file_entry.get("line_count")
        if not isinstance(line_count, int) or line_count <= 0:
            raise MapAuditError(f"{path}: line_count must be positive")
        line_total += line_count

        review_status = file_entry.get("review_status")
        if review_status not in REVIEW_STATUSES:
            raise MapAuditError(f"{path}: invalid review_status {review_status!r}")
        if review_status == "reviewed":
            reviewed_files += 1
        elif require_reviewed:
            raise MapAuditError(f"{path}: source intervals remain coarse")

        segments = file_entry.get("segments")
        if not isinstance(segments, list) or not segments:
            raise MapAuditError(f"{path}: segments must be a non-empty array")
        next_line = 1
        for segment in segments:
            next_line = validate_segment(path, segment, next_line)
        if next_line != line_count + 1:
            raise MapAuditError(
                f"{path}: segments end at line {next_line - 1}, expected {line_count}"
            )

    if paths != sorted(paths):
        raise MapAuditError("manifest files must be sorted by path")
    if len(paths) != len(set(paths)):
        raise MapAuditError("manifest contains duplicate file paths")

    return AuditSummary(len(paths), line_total, reviewed_files)


def git_output(linux_src: pathlib.Path, *arguments: str) -> str:
    try:
        return subprocess.check_output(
            ["git", "-C", str(linux_src), *arguments],
            text=True,
            stderr=subprocess.STDOUT,
        ).strip()
    except subprocess.CalledProcessError as error:
        detail = error.output.strip()
        raise MapAuditError(f"git {' '.join(arguments)} failed: {detail}") from error


def source_line_count(contents: bytes) -> int:
    if not contents:
        return 0
    return contents.count(b"\n") + (0 if contents.endswith(b"\n") else 1)


def validate_linux_source(linux_src: pathlib.Path, manifest: dict[str, Any]) -> None:
    if not linux_src.is_dir():
        raise MapAuditError(f"Linux source directory does not exist: {linux_src}")

    head = git_output(linux_src, "rev-parse", "HEAD")
    if head != LINUX_COMMIT:
        raise MapAuditError(f"Linux HEAD is {head}, expected {LINUX_COMMIT}")

    actual_paths = git_output(
        linux_src,
        "ls-tree",
        "-r",
        "--name-only",
        LINUX_COMMIT,
        *SOURCE_ROOTS,
    ).splitlines()
    manifest_paths = [entry["path"] for entry in manifest["files"]]
    if actual_paths != manifest_paths:
        missing = sorted(set(actual_paths) - set(manifest_paths))
        extra = sorted(set(manifest_paths) - set(actual_paths))
        raise MapAuditError(f"Linux source file set differs: missing={missing}, extra={extra}")

    for file_entry in manifest["files"]:
        path = file_entry["path"]
        blob = git_output(linux_src, "rev-parse", f"{LINUX_COMMIT}:{path}")
        if blob != file_entry["blob"]:
            raise MapAuditError(f"{path}: blob {blob} differs from manifest")
        try:
            contents = subprocess.check_output(
                ["git", "-C", str(linux_src), "show", f"{LINUX_COMMIT}:{path}"],
                stderr=subprocess.STDOUT,
            )
        except subprocess.CalledProcessError as error:
            detail = error.output.decode(errors="replace").strip()
            raise MapAuditError(f"cannot read {path} from Linux commit: {detail}") from error
        lines = source_line_count(contents)
        if lines != file_entry["line_count"]:
            raise MapAuditError(
                f"{path}: source has {lines} lines, manifest has {file_entry['line_count']}"
            )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--linux-src",
        type=pathlib.Path,
        help="also verify the manifest against this exact Linux checkout",
    )
    parser.add_argument(
        "--require-reviewed",
        action="store_true",
        help="reject file maps that are still marked as coarse",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    manifest_path = workspace_root() / MANIFEST_PATH
    try:
        manifest = load_manifest(manifest_path)
        summary = validate_manifest(manifest, require_reviewed=args.require_reviewed)
        if args.linux_src is not None:
            validate_linux_source(args.linux_src.expanduser().resolve(), manifest)
    except MapAuditError as error:
        print("RSEXT4_LINUX_MAP_FAILED")
        print(f"- {error}")
        return 1

    print(
        "RSEXT4_LINUX_MAP_PASSED "
        f"commit={LINUX_COMMIT} files={summary.files} lines={summary.lines} "
        f"reviewed_files={summary.reviewed_files}"
    )
    if args.linux_src is not None:
        print("RSEXT4_LINUX_MAP_SOURCE_PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
