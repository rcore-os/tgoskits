#!/usr/bin/env python3
"""Record provenance for one observed AxVisor real-time benchmark capture."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import platform
import re
import subprocess
import sys
from pathlib import Path
from typing import Iterator, Sequence


GUEST_PROFILES: dict[str, dict[str, object]] = {
    "partitioned": {
        "axvisor_config": "docs/realtime/axvisor-qemu-aarch64-partition.toml",
        "dedicated_host_cpu_ids": [2, 3],
        "vm_config": (
            "os/axvisor/configs/vms/qemu/aarch64/"
            "linux-smp2-dedicated.toml"
        ),
    },
    "shared": {
        "axvisor_config": "docs/realtime/axvisor-qemu-aarch64-shared.toml",
        "dedicated_host_cpu_ids": [],
        "vm_config": "os/axvisor/configs/vms/qemu/aarch64/linux-smp2-shared.toml",
    },
}

PRUNED_DIRECTORY_NAMES = {
    ".git",
    "target",
    "tmp",
    "build",
    "__pycache__",
}
GENERATED_EVIDENCE_DIRECTORIES = (
    Path("competition/results"),
    Path("docs/competition"),
)
GIT_COMMIT_PATTERN = re.compile(r"[0-9a-f]{40}(?:[0-9a-f]{24})?\Z")


class MetadataError(ValueError):
    """Raised when immutable capture provenance cannot be recorded safely."""


def main(argv: Sequence[str] | None = None) -> int:
    try:
        args = parse_args(argv)
        if args.command == "start":
            write_metadata_atomic(args.output, build_running_metadata(args))
        else:
            running = read_metadata(args.metadata)
            completed = finalize_metadata(
                running,
                finished_at=args.finished_at,
                exit_code=args.exit_code,
                raw_log=args.raw_log,
            )
            write_metadata_atomic(args.metadata, completed)
    except (MetadataError, OSError, json.JSONDecodeError) as error:
        print(f"record_metadata: {error}", file=sys.stderr)
        return 2
    return 0


def build_running_metadata(args: argparse.Namespace) -> dict[str, object]:
    guest_profile = GUEST_PROFILES[args.profile]
    axvisor_config = args.workspace / str(guest_profile["axvisor_config"])
    vm_config = args.workspace / str(guest_profile["vm_config"])
    qemu_config = args.workspace / "scripts/benchmark/axvisor-rt/qemu-aarch64.toml"
    return {
        "schema_version": 1,
        "run_id": args.run_id,
        "status": "capture_running",
        "started_at": args.started_at,
        "finished_at": None,
        "repository": repository_state(args.workspace),
        "host": {
            "system": platform.system(),
            "release": platform.release(),
            "machine": platform.machine(),
        },
        "qemu": {
            "binary": args.qemu_binary,
            "version": command_output([args.qemu_binary, "--version"], "unknown"),
            "acceleration": "tcg",
            "machine": "virt,virtualization=on,gic-version=3",
            "cpu": "cortex-a72",
            "host_cpu_count": 4,
            "exit_code": None,
        },
        "guest": {
            "architecture": "aarch64",
            "vcpu_count": 2,
            "profile": args.profile,
            "dedicated_host_cpu_ids": guest_profile["dedicated_host_cpu_ids"],
            "vm_config": guest_profile["vm_config"],
        },
        "benchmark": {
            "iterations": args.iterations,
            "warmup_iterations": args.warmup,
            "period_ns": args.period_us * 1000,
            "guest_cpu": args.cpu,
            "fifo_priority": args.fifo_priority,
            "workload": args.workload,
            "metrics": [
                "periodic_jitter",
                "dispatch_latency",
                "emulated_irq_response",
            ],
        },
        "artifacts": {
            "input_rootfs": artifact_record(args.rootfs),
            "injected_rootfs_pre_run": artifact_record(args.injected_rootfs),
            "probe": artifact_record(args.probe),
            "guest_runner": artifact_record(args.guest_runner),
            "axvisor_config": artifact_record(axvisor_config),
            "vm_config": artifact_record(vm_config),
            "qemu_config": artifact_record(qemu_config),
        },
    }


def finalize_metadata(
    metadata: dict[str, object],
    *,
    finished_at: str,
    exit_code: int,
    raw_log: Path,
) -> dict[str, object]:
    if metadata.get("status") != "capture_running":
        raise MetadataError("metadata status must be capture_running before finalization")
    qemu = metadata.get("qemu")
    artifacts = metadata.get("artifacts")
    if not isinstance(qemu, dict) or qemu.get("exit_code") is not None:
        raise MetadataError("capture_running metadata must have a null QEMU exit code")
    if not isinstance(artifacts, dict) or "raw_log" in artifacts:
        raise MetadataError("capture_running metadata must not contain a raw-log record")

    finalized = copy.deepcopy(metadata)
    finalized_qemu = finalized["qemu"]
    finalized_artifacts = finalized["artifacts"]
    assert isinstance(finalized_qemu, dict)
    assert isinstance(finalized_artifacts, dict)
    finalized["finished_at"] = finished_at
    finalized["status"] = "capture_complete" if exit_code == 0 else "failed"
    finalized_qemu["exit_code"] = exit_code
    finalized_artifacts["raw_log"] = artifact_record(raw_log)
    return finalized


def repository_state(workspace: Path) -> dict[str, object]:
    commit_bytes = git_output_required(workspace, ["rev-parse", "--verify", "HEAD"])
    status = git_output_required(
        workspace,
        ["status", "--porcelain=v1", "-z", "--untracked-files=normal"],
    )
    tracked_diff = git_output_required(workspace, ["diff", "--binary", "HEAD", "--"])
    try:
        commit = commit_bytes.decode("ascii", errors="strict").strip()
    except UnicodeDecodeError as error:
        raise MetadataError("git returned a non-ASCII commit identifier") from error
    if GIT_COMMIT_PATTERN.fullmatch(commit) is None:
        raise MetadataError("git returned an invalid commit identifier")

    manifest_entries = untracked_source_manifest(workspace, status)
    manifest = b"".join(manifest_entries)
    snapshot = b"\0".join((commit.encode("ascii"), tracked_diff, manifest))
    return {
        "commit": commit,
        "dirty": bool(status),
        "source_snapshot_sha256": hashlib.sha256(snapshot).hexdigest(),
        "tracked_diff_sha256": hashlib.sha256(tracked_diff).hexdigest(),
        "untracked_source_file_count": len(manifest_entries),
        "untracked_source_manifest_sha256": hashlib.sha256(manifest).hexdigest(),
    }


def untracked_source_manifest(workspace: Path, status: bytes) -> list[bytes]:
    untracked_roots = {
        Path(os.fsdecode(entry.removeprefix(b"?? ")))
        for entry in status.split(b"\0")
        if entry.startswith(b"?? ")
    }
    source_nodes = sorted(
        (
            node
            for root in untracked_roots
            for node in enumerate_untracked_source_nodes(workspace, root)
        ),
        key=lambda node: os.fsencode(node.as_posix()),
    )
    return [source_manifest_entry(workspace, node) for node in source_nodes]


def enumerate_untracked_source_nodes(
    workspace: Path, relative_path: Path
) -> Iterator[Path]:
    relative_path = Path(relative_path)
    if not relative_path.parts or is_generated_evidence_path(relative_path):
        return

    source_path = workspace / relative_path
    if source_path.is_symlink() or source_path.is_file():
        yield relative_path
        return
    if not source_path.is_dir():
        raise MetadataError(f"untracked git path is unavailable: {relative_path}")
    if is_pruned_directory(relative_path):
        return

    try:
        entries = sorted(
            os.scandir(source_path),
            key=lambda entry: os.fsencode(entry.name),
        )
    except OSError as error:
        raise MetadataError(f"cannot enumerate untracked path {relative_path}: {error}") from error
    for entry in entries:
        child = relative_path / entry.name
        if entry.is_symlink():
            if not is_generated_evidence_path(child):
                yield child
        elif entry.is_dir(follow_symlinks=False):
            yield from enumerate_untracked_source_nodes(workspace, child)
        elif entry.is_file(follow_symlinks=False):
            if not is_generated_evidence_path(child):
                yield child
        else:
            raise MetadataError(f"unsupported untracked filesystem node: {child}")


def is_pruned_directory(relative_path: Path) -> bool:
    name = relative_path.name
    return name in PRUNED_DIRECTORY_NAMES or name.startswith("build-")


def is_generated_evidence_path(relative_path: Path) -> bool:
    return any(
        relative_path == evidence_directory
        or evidence_directory in relative_path.parents
        for evidence_directory in GENERATED_EVIDENCE_DIRECTORIES
    )


def source_manifest_entry(workspace: Path, relative_path: Path) -> bytes:
    source_path = workspace / relative_path
    relative_bytes = os.fsencode(relative_path.as_posix())
    try:
        if source_path.is_symlink():
            kind = b"symlink"
            payload = os.fsencode(os.readlink(source_path))
            digest = hashlib.sha256(payload).hexdigest().encode("ascii")
        else:
            kind = b"file"
            digest = sha256_file(source_path).encode("ascii")
    except OSError as error:
        raise MetadataError(f"cannot hash untracked path {relative_path}: {error}") from error
    return b"\0".join((kind, digest, relative_bytes, b""))


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def artifact_record(path: Path) -> dict[str, str]:
    return {"path": str(path.resolve()), "sha256": sha256_file(path)}


def command_output(command: Sequence[str], fallback: str) -> str:
    try:
        result = subprocess.run(
            command,
            check=True,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return fallback
    return result.stdout.strip().splitlines()[0] if result.stdout.strip() else fallback


def git_output_required(workspace: Path, arguments: Sequence[str]) -> bytes:
    try:
        return subprocess.run(
            ["git", "-C", str(workspace), *arguments],
            check=True,
            capture_output=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError) as error:
        command = " ".join(("git", *arguments))
        raise MetadataError(f"git provenance command failed: {command}") from error


def read_metadata(path: Path) -> dict[str, object]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise MetadataError("metadata document must be a JSON object")
    return value


def write_metadata_atomic(path: Path, metadata: dict[str, object]) -> None:
    temporary = path.with_name(f".{path.name}.tmp")
    temporary.write_text(
        json.dumps(metadata, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    os.replace(temporary, path)


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    start = subparsers.add_parser("start", help="record immutable pre-run provenance")
    start.add_argument("--output", type=Path, required=True)
    start.add_argument("--workspace", type=Path, required=True)
    start.add_argument("--run-id", required=True)
    start.add_argument("--started-at", required=True)
    start.add_argument("--qemu-binary", default="qemu-system-aarch64")
    start.add_argument("--rootfs", type=Path, required=True)
    start.add_argument("--injected-rootfs", type=Path, required=True)
    start.add_argument("--probe", type=Path, required=True)
    start.add_argument("--guest-runner", type=Path, required=True)
    start.add_argument("--iterations", type=int, required=True)
    start.add_argument("--warmup", type=int, required=True)
    start.add_argument("--period-us", type=int, required=True)
    start.add_argument("--cpu", type=int, required=True)
    start.add_argument("--fifo-priority", type=int, required=True)
    start.add_argument("--workload", required=True)
    start.add_argument("--profile", choices=tuple(GUEST_PROFILES), required=True)

    finalize = subparsers.add_parser("finalize", help="record post-run status and log")
    finalize.add_argument("--metadata", type=Path, required=True)
    finalize.add_argument("--finished-at", required=True)
    finalize.add_argument("--exit-code", type=int, required=True)
    finalize.add_argument("--raw-log", type=Path, required=True)
    return parser.parse_args(argv)


if __name__ == "__main__":
    raise SystemExit(main())
