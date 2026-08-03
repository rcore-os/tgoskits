#!/usr/bin/env python3
"""Write reproducible metadata for one Orange Pi competition run."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
from pathlib import Path


class MetadataError(ValueError):
    """The run metadata cannot be produced from the supplied evidence."""


def build_metadata(arguments: argparse.Namespace) -> dict[str, object]:
    workspace = arguments.workspace.resolve()
    git = inspect_git_worktree(workspace)
    if arguments.require_clean and git["dirty"]:
        raise MetadataError("formal board evidence requires a clean Git worktree")

    summary = optional_file_record(arguments.summary, workspace)
    raw_csv = optional_file_record(arguments.raw_csv, workspace)
    summary_document = read_optional_json(arguments.summary)
    return {
        "schema_version": 1,
        "run": {
            "profile": arguments.profile,
            "run_id": arguments.run_id,
            "run_number": arguments.run_number,
            "repeat_count": arguments.repeat_count,
            "execution_order": arguments.run_number,
            "board_type": arguments.board_type,
            "started_at": arguments.started_at,
            "finished_at": arguments.finished_at,
            "exit_status": arguments.exit_status,
        },
        "source": git,
        "inputs": {
            "build_config": required_file_record(arguments.build_config, workspace),
            "board_config": required_file_record(arguments.board_config, workspace),
            "starry_kernel": optional_file_record(arguments.starry_kernel, workspace),
            "starry_dtb": optional_file_record(arguments.starry_dtb, workspace),
            "rootfs": optional_file_record(arguments.rootfs, workspace),
        },
        "board": {
            "type": arguments.board_type,
            "id": nested_value(summary_document, "board", "board_id"),
            "hostname": nested_value(summary_document, "board", "hostname"),
            "cpu_temp_milli_c": nested_value(
                summary_document, "board", "cpu_temp_milli_c"
            ),
        },
        "model": {
            "id": arguments.model_id,
            "backend": arguments.inference_backend,
            "runtime_version": arguments.runtime_version,
            "artifact": required_file_record(arguments.model_artifact, workspace),
        },
        "outputs": {
            "console_log": required_file_record(arguments.console_log, workspace),
            "raw_csv": raw_csv,
            "summary": summary,
        },
        "result": {
            "validated": (
                summary_document is not None
                and raw_csv is not None
                and arguments.exit_status == 0
            ),
            "controller_policy": nested_value(
                summary_document, "controller", "policy"
            ),
            "sample_count": nested_value(
                summary_document, "controller", "acknowledged"
            ),
            "dropped_samples": nested_value(
                summary_document, "raw_samples", "dropped_samples"
            ),
            "deadline_misses": nested_value(
                summary_document, "raw_samples", "deadline_misses"
            ),
            "successful_marker": nested_value(
                summary_document, "lifecycle", "starry_done"
            ),
        },
    }


def inspect_git_worktree(workspace: Path) -> dict[str, object]:
    commit = run_git(workspace, "rev-parse", "HEAD")
    branch = run_git(workspace, "branch", "--show-current") or None
    tracked = run_git(
        workspace,
        "status",
        "--porcelain=v1",
        "--untracked-files=no",
        "--ignore-submodules=all",
    ).splitlines()
    untracked = run_git(
        workspace, "ls-files", "--others", "--exclude-standard"
    ).splitlines()
    return {
        "commit": commit,
        "branch": branch,
        "dirty": bool(tracked or untracked),
        "tracked_change_count": len(tracked),
        "untracked_file_count": len(untracked),
    }


def run_git(workspace: Path, *arguments: str) -> str:
    completed = subprocess.run(
        ["git", "-C", str(workspace), *arguments],
        check=False,
        capture_output=True,
        text=True,
    )
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip()
        raise MetadataError(f"git {' '.join(arguments)} failed: {detail}")
    return completed.stdout.strip()


def required_file_record(path: Path, workspace: Path) -> dict[str, object]:
    resolved = resolve_input_path(path, workspace)
    if not resolved.is_file():
        raise MetadataError(f"required evidence file does not exist: {resolved}")
    return file_record(resolved, workspace)


def optional_file_record(
    path: Path, workspace: Path
) -> dict[str, object] | None:
    resolved = resolve_input_path(path, workspace)
    if not resolved.is_file():
        return None
    return file_record(resolved, workspace)


def resolve_input_path(path: Path, workspace: Path) -> Path:
    if path.is_absolute():
        return path.resolve()
    return (workspace / path).resolve()


def file_record(path: Path, workspace: Path) -> dict[str, object]:
    try:
        display_path = str(path.relative_to(workspace))
    except ValueError:
        display_path = str(path)
    return {
        "path": display_path,
        "sha256": sha256_file(path),
        "size_bytes": path.stat().st_size,
    }


def read_optional_json(path: Path) -> dict[str, object] | None:
    if not path.is_file():
        return None
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise MetadataError(f"cannot read result summary {path}: {error}") from error
    if not isinstance(document, dict):
        raise MetadataError(f"result summary must contain a JSON object: {path}")
    return document


def nested_value(
    document: dict[str, object] | None, *keys: str
) -> object | None:
    value: object = document
    for key in keys:
        if not isinstance(value, dict) or key not in value:
            return None
        value = value[key]
    return value


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--profile", required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--run-number", type=int, required=True)
    parser.add_argument("--repeat-count", type=int, required=True)
    parser.add_argument("--board-type", required=True)
    parser.add_argument("--started-at", required=True)
    parser.add_argument("--finished-at", required=True)
    parser.add_argument("--exit-status", type=int, required=True)
    parser.add_argument("--console-log", type=Path, required=True)
    parser.add_argument("--raw-csv", type=Path, required=True)
    parser.add_argument("--summary", type=Path, required=True)
    parser.add_argument("--build-config", type=Path, required=True)
    parser.add_argument("--board-config", type=Path, required=True)
    parser.add_argument("--starry-kernel", type=Path, required=True)
    parser.add_argument("--starry-dtb", type=Path, required=True)
    parser.add_argument("--rootfs", type=Path, required=True)
    parser.add_argument("--model-id", required=True)
    parser.add_argument("--model-artifact", type=Path, required=True)
    parser.add_argument("--inference-backend", required=True)
    parser.add_argument("--runtime-version", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--require-clean", action="store_true")
    return parser.parse_args()


def main() -> int:
    arguments = parse_arguments()
    if arguments.run_number <= 0:
        print("board metadata failed: run number must be positive", file=sys.stderr)
        return 2
    if arguments.repeat_count <= 0 or arguments.run_number > arguments.repeat_count:
        print("board metadata failed: invalid repeat count or run order", file=sys.stderr)
        return 2
    try:
        metadata = build_metadata(arguments)
        arguments.output.parent.mkdir(parents=True, exist_ok=True)
        arguments.output.write_text(
            json.dumps(metadata, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
    except (MetadataError, OSError) as error:
        print(f"board metadata failed: {error}", file=sys.stderr)
        return 1
    print(f"BOARD_METADATA_WRITTEN path={arguments.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
