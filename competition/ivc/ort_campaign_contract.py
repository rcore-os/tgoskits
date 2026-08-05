#!/usr/bin/env python3
"""Create and validate the frozen ONNX Runtime full-campaign contract."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Mapping, Sequence


DEFAULT_MODEL_SHA256 = (
    "3582869baf9b8cec722208d06f66acd680a64128b52875d22e7f0e43f2ed7887"
)
DEFAULT_RUNTIME_VERSION = "1.25.0"
EXPECTED_PROVIDER = "CPUExecutionProvider"
EXPECTED_RUNS = 5
EXPECTED_COUNT = 1_800
PERIOD_US = 100_000
MAX_DEADLINE_MISSES_PER_RUN = 1
MAX_FULL_LOOP_P99_US = 25_000
MAX_FULL_LOOP_US = 200_000
MIN_THROUGHPUT_MSG_S = 9.5
MAX_INITIALIZATION_US = 500_000
MAX_ORT_WALL_P99_NS = 1_000_000
MAX_ORT_WALL_NS = 25_000_000
COMMIT_PATTERN = re.compile(r"[0-9a-f]{40}")
SHA256_PATTERN = re.compile(r"[0-9a-f]{64}")
INPUT_NAMES = (
    "board_config",
    "build_config",
    "rootfs",
    "starry_dtb",
    "starry_kernel",
    "zephyr_guest",
)


class ContractError(ValueError):
    """Raised when the frozen campaign contract is missing or inconsistent."""


@dataclass(frozen=True)
class ValidatedPreregistration:
    """Identity fields needed to bind later runs to their preregistration."""

    created_at: datetime
    branch: str
    input_identity: str


def frozen_thresholds() -> dict[str, int | float]:
    """Return the immutable full-campaign acceptance thresholds."""

    return {
        "max_deadline_misses_per_run": MAX_DEADLINE_MISSES_PER_RUN,
        "post_first_deadline_misses": 0,
        "max_full_loop_p99_us": MAX_FULL_LOOP_P99_US,
        "max_full_loop_us": MAX_FULL_LOOP_US,
        "min_throughput_msg_s": MIN_THROUGHPUT_MSG_S,
        "max_initialization_us": MAX_INITIALIZATION_US,
        "max_ort_wall_p99_ns": MAX_ORT_WALL_P99_NS,
        "max_ort_wall_ns": MAX_ORT_WALL_NS,
    }


def build_preregistration(
    workspace: Path,
    expected_commit: str,
    branch: str,
    inputs: Mapping[str, Path],
    model_artifact: Path,
    created_at: datetime | None = None,
    run_count: int = EXPECTED_RUNS,
    samples_per_run: int = EXPECTED_COUNT,
) -> dict[str, object]:
    """Build a preregistration document from the exact campaign artifacts."""

    if COMMIT_PATTERN.fullmatch(expected_commit) is None:
        raise ContractError("expected commit must be a full Git SHA")
    if not branch:
        raise ContractError("source branch must not be empty")
    if run_count <= 0 or samples_per_run <= 1:
        raise ContractError("campaign run and sample counts must be positive")
    if set(inputs) != set(INPUT_NAMES):
        raise ContractError("campaign input set does not match the frozen contract")
    workspace = workspace.resolve()
    input_records = {
        name: artifact_record(path, workspace) for name, path in inputs.items()
    }
    model_record = artifact_record(model_artifact, workspace)
    if model_record["sha256"] != DEFAULT_MODEL_SHA256:
        raise ContractError("ORT model digest differs from the frozen model")
    timestamp = created_at or datetime.now(timezone.utc)
    if timestamp.tzinfo is None:
        raise ContractError("preregistration timestamp must include a timezone")
    timestamp = timestamp.astimezone(timezone.utc).replace(microsecond=0)
    return {
        "schema_version": 1,
        "created_at": timestamp.isoformat().replace("+00:00", "Z"),
        "source": {
            "branch": branch,
            "commit": expected_commit,
            "dirty": False,
        },
        "campaign": {
            "profile": "ort-full",
            "run_count": run_count,
            "samples_per_run": samples_per_run,
            "period_ms": 100,
            "execution_order": [
                f"run-{number:03d}" for number in range(1, run_count + 1)
            ],
            "replacement_runs_allowed": False,
            "startup_semantics": "fresh-board-reboot-and-new-ort-session",
        },
        "board": {"type": "OrangePi-5-Plus"},
        "model": {
            "id": "thermal-4x6x1-v1",
            "backend": "onnxruntime",
            "runtime_version": DEFAULT_RUNTIME_VERSION,
            "provider": EXPECTED_PROVIDER,
            "artifact": model_record,
        },
        "inputs": input_records,
        "frozen_thresholds": frozen_thresholds(),
    }


def validate_preregistration(
    document: dict[str, object],
    expected_commit: str,
    expected_runs: int,
    expected_count: int,
    expected_model_sha256: str,
    expected_runtime_version: str,
) -> ValidatedPreregistration:
    """Validate a preregistration and return its cross-run identity."""

    require_equal(document, "schema_version", 1, "preregistration")
    created_at = parse_timestamp(
        require_string(document, "created_at", "preregistration"),
        "preregistration created_at",
    )
    source = require_object(document, "source", "preregistration")
    require_equal(source, "commit", expected_commit, "preregistration source")
    require_equal(source, "dirty", False, "preregistration source")
    branch = require_string(source, "branch", "preregistration source")

    campaign = require_object(document, "campaign", "preregistration")
    require_equal(campaign, "profile", "ort-full", "preregistration campaign")
    require_equal(campaign, "run_count", expected_runs, "preregistration campaign")
    require_equal(
        campaign,
        "samples_per_run",
        expected_count,
        "preregistration campaign",
    )
    require_equal(campaign, "period_ms", 100, "preregistration campaign")
    require_equal(
        campaign,
        "execution_order",
        [f"run-{number:03d}" for number in range(1, expected_runs + 1)],
        "preregistration campaign",
    )
    require_equal(
        campaign,
        "replacement_runs_allowed",
        False,
        "preregistration campaign",
    )
    require_equal(
        campaign,
        "startup_semantics",
        "fresh-board-reboot-and-new-ort-session",
        "preregistration campaign",
    )

    board = require_object(document, "board", "preregistration")
    require_equal(board, "type", "OrangePi-5-Plus", "preregistration board")
    model = require_object(document, "model", "preregistration")
    require_equal(model, "id", "thermal-4x6x1-v1", "preregistration model")
    require_equal(model, "backend", "onnxruntime", "preregistration model")
    require_equal(
        model,
        "runtime_version",
        expected_runtime_version,
        "preregistration model",
    )
    require_equal(model, "provider", EXPECTED_PROVIDER, "preregistration model")
    model_artifact = require_object(model, "artifact", "preregistration model")
    validate_artifact_record(model_artifact, "preregistration model artifact")
    require_equal(
        model_artifact,
        "sha256",
        expected_model_sha256,
        "preregistration model artifact",
    )

    inputs = require_object(document, "inputs", "preregistration")
    if set(inputs) != set(INPUT_NAMES):
        raise ContractError("preregistration has the wrong input artifact set")
    for name in INPUT_NAMES:
        validate_artifact_record(
            require_object(inputs, name, "preregistration inputs"),
            f"preregistration input {name}",
        )
    require_equal(
        document,
        "frozen_thresholds",
        frozen_thresholds(),
        "preregistration",
    )
    return ValidatedPreregistration(
        created_at=created_at,
        branch=branch,
        input_identity=json.dumps(
            {"inputs": inputs, "model": without_provider(model)},
            sort_keys=True,
        ),
    )


def load_preregistration_evidence(
    campaign_root: Path,
    expected_commit: str,
    expected_runs: int,
    expected_count: int,
    expected_model_sha256: str,
    expected_runtime_version: str,
) -> tuple[ValidatedPreregistration, str]:
    """Load a preregistration and verify its pre-run digest seal."""

    preregistration_path = campaign_root / "preregistration.json"
    digest_path = campaign_root / "preregistration.sha256"
    try:
        contents = preregistration_path.read_bytes()
        document = json.loads(contents)
        digest_lines = digest_path.read_text(encoding="utf-8").splitlines()
    except (OSError, json.JSONDecodeError) as error:
        raise ContractError(f"cannot read preregistration evidence: {error}") from error
    if not isinstance(document, dict):
        raise ContractError("preregistration must be a JSON object")
    digest = hashlib.sha256(contents).hexdigest()
    if digest_lines != [f"{digest}  preregistration.json"]:
        raise ContractError("preregistration checksum does not match its contents")
    validated = validate_preregistration(
        document,
        expected_commit,
        expected_runs,
        expected_count,
        expected_model_sha256,
        expected_runtime_version,
    )
    return validated, digest


def artifact_record(path: Path, workspace: Path) -> dict[str, object]:
    resolved = path.resolve() if path.is_absolute() else (workspace / path).resolve()
    if not resolved.is_file():
        raise ContractError(f"required campaign artifact does not exist: {resolved}")
    try:
        display_path = str(resolved.relative_to(workspace))
    except ValueError:
        display_path = str(resolved)
    try:
        contents = resolved.read_bytes()
    except OSError as error:
        raise ContractError(f"cannot read campaign artifact {resolved}: {error}") from error
    return {
        "path": display_path,
        "sha256": hashlib.sha256(contents).hexdigest(),
        "size_bytes": len(contents),
    }


def validate_artifact_record(record: dict[str, object], label: str) -> None:
    if set(record) != {"path", "sha256", "size_bytes"}:
        raise ContractError(f"{label} has the wrong fields")
    require_string(record, "path", label)
    digest = require_string(record, "sha256", label)
    if SHA256_PATTERN.fullmatch(digest) is None:
        raise ContractError(f"{label} sha256 is malformed")
    size = record.get("size_bytes")
    if isinstance(size, bool) or not isinstance(size, int) or size <= 0:
        raise ContractError(f"{label} size_bytes must be a positive integer")


def without_provider(model: dict[str, object]) -> dict[str, object]:
    return {key: value for key, value in model.items() if key != "provider"}


def require_object(
    parent: dict[str, object], key: str, label: str
) -> dict[str, object]:
    value = parent.get(key)
    if not isinstance(value, dict):
        raise ContractError(f"{label} {key} must be an object")
    return value


def require_string(parent: dict[str, object], key: str, label: str) -> str:
    value = parent.get(key)
    if not isinstance(value, str) or not value:
        raise ContractError(f"{label} {key} must be a nonempty string")
    return value


def require_equal(
    parent: dict[str, object], key: str, expected: object, label: str
) -> None:
    if parent.get(key) != expected:
        raise ContractError(
            f"{label} {key} must be {expected!r}, got {parent.get(key)!r}"
        )


def parse_timestamp(value: str, label: str) -> datetime:
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise ContractError(f"{label} is not an ISO-8601 timestamp") from error
    if parsed.tzinfo is None:
        raise ContractError(f"{label} must include a timezone")
    return parsed


def write_json(path: Path, value: object) -> None:
    if path.exists():
        raise ContractError(f"refusing to overwrite existing output {path}")
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            json.dumps(value, indent=2, sort_keys=True, allow_nan=False) + "\n",
            encoding="utf-8",
            newline="\n",
        )
    except OSError as error:
        raise ContractError(f"cannot write {path}: {error}") from error


def parse_arguments(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Write the frozen StarryOS ONNX Runtime campaign preregistration."
    )
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--expected-commit", required=True)
    parser.add_argument("--branch", required=True)
    parser.add_argument("--board-config", type=Path, required=True)
    parser.add_argument("--build-config", type=Path, required=True)
    parser.add_argument("--rootfs", type=Path, required=True)
    parser.add_argument("--starry-dtb", type=Path, required=True)
    parser.add_argument("--starry-kernel", type=Path, required=True)
    parser.add_argument("--zephyr-guest", type=Path, required=True)
    parser.add_argument("--model-artifact", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    arguments = parse_arguments(sys.argv[1:] if argv is None else argv)
    try:
        document = build_preregistration(
            arguments.workspace,
            arguments.expected_commit,
            arguments.branch,
            {
                "board_config": arguments.board_config,
                "build_config": arguments.build_config,
                "rootfs": arguments.rootfs,
                "starry_dtb": arguments.starry_dtb,
                "starry_kernel": arguments.starry_kernel,
                "zephyr_guest": arguments.zephyr_guest,
            },
            arguments.model_artifact,
        )
        write_json(arguments.output, document)
        print(f"ORT campaign preregistration written: {arguments.output}")
    except ContractError as error:
        print(f"ORT campaign preregistration failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
