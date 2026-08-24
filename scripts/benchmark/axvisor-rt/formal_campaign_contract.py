"""Immutable contract and evidence validation for a StarryOS RT campaign."""

from __future__ import annotations

import hashlib
import json
import re
import subprocess
from datetime import datetime, timezone
from pathlib import Path
from typing import Mapping, NamedTuple


COMMIT_PATTERN = re.compile(r"[0-9a-f]{40}")
SHA256_PATTERN = re.compile(r"[0-9a-f]{64}")
BOARD_IDENTITY_PATTERN = re.compile(
    r"^AXVISOR_RT_BOARD_IDENTITY "
    r"board_id=([A-Za-z0-9._:-]+) "
    r"hostname=([A-Za-z0-9._-]+) "
    r"cpu_temp_milli_c=(-?[0-9]+)$",
    re.MULTILINE,
)
BOARD_SERVICE_PATTERN = re.compile(
    r"^AXVISOR_RT_BOARD_SERVICE_ID "
    r"board_type=([A-Za-z0-9._:-]+) board_id=([A-Za-z0-9._:-]+)$",
    re.MULTILINE,
)
FORBIDDEN_CONSOLE_PATTERNS = (
    "Unhandled IRQ",
    "ESR_EL2:",
    "panicked at",
    "AXVM_NESTED_VCPU_OPERATION",
    "Unhandled synchronous exception",
    "AXVISOR_RT_STARRY_CAPTURE_FAILED",
)
ZERO_COUNTERS = (
    "dropped",
    "incomplete",
    "failed_injections",
    "unowned_virtual_timer_irqs",
    "counter_frequency_mismatches",
)
PAIR_ORDER = (
    ("shared", "partitioned"),
    ("partitioned", "shared"),
    ("shared", "partitioned"),
    ("partitioned", "shared"),
    ("shared", "partitioned"),
)
SOAK_ORDER = ("shared", "partitioned")
ARTIFACT_NAMES = (
    "base_rootfs",
    "host_toolchain",
    "probe",
    "pair_kernel",
    "pair_rootfs",
    "soak_kernel",
    "soak_rootfs",
    "guest_dtb",
)
FROZEN_SOURCE_PATHS = (
    "competition/ivc/orangepi/board-runner.sh",
    "competition/ivc/orangepi/prepare-service-dtb.sh",
    "competition/ivc/orangepi/restore-linux.sh",
    "competition/ivc/starry/build-guest-dtb.sh",
    "competition/ivc/starry/orangepi-5-plus.dts",
    "scripts/benchmark/axvisor-rt/aggregate_starry_board.py",
    "scripts/benchmark/axvisor-rt/analyze_irq_trace.py",
    "scripts/benchmark/axvisor-rt/analyze_starry_board.py",
    "scripts/benchmark/axvisor-rt/build-starry-kernel.sh",
    "scripts/benchmark/axvisor-rt/build-starry-rootfs.sh",
    "scripts/benchmark/axvisor-rt/compare_starry_board.py",
    "scripts/benchmark/axvisor-rt/formal_campaign.py",
    "scripts/benchmark/axvisor-rt/formal_campaign_contract.py",
    "scripts/benchmark/axvisor-rt/formal_campaign_receipt.py",
    "scripts/benchmark/axvisor-rt/guest/starry_rt_capture_run.sh",
    "scripts/benchmark/axvisor-rt/harvest-starry-board.sh",
    "scripts/benchmark/axvisor-rt/prepare-starry-soak.sh",
    "scripts/benchmark/axvisor-rt/prepare-freestanding-c-toolchain.sh",
    "scripts/benchmark/axvisor-rt/run-formal-campaign.sh",
    "scripts/benchmark/axvisor-rt/stage-starry-board.sh",
    "scripts/benchmark/axvisor-rt/config/starry-aarch64-rt.toml",
    "scripts/benchmark/axvisor-rt/config/starry-aarch64-rt-soak.toml",
    "scripts/benchmark/axvisor-rt/config/axvisor-orangepi-5-plus-starry-host-noise-formal-shared.toml",
    "scripts/benchmark/axvisor-rt/config/axvisor-orangepi-5-plus-starry-host-noise-formal-partitioned.toml",
    "scripts/benchmark/axvisor-rt/config/axvisor-orangepi-5-plus-starry-host-noise-soak-shared.toml",
    "scripts/benchmark/axvisor-rt/config/axvisor-orangepi-5-plus-starry-host-noise-soak-partitioned.toml",
    "scripts/benchmark/axvisor-rt/config/board-orangepi-5-plus-starry-host-noise-formal-shared.toml",
    "scripts/benchmark/axvisor-rt/config/board-orangepi-5-plus-starry-host-noise-formal-partitioned.toml",
    "scripts/benchmark/axvisor-rt/config/board-orangepi-5-plus-starry-host-noise-soak-shared.toml",
    "scripts/benchmark/axvisor-rt/config/board-orangepi-5-plus-starry-host-noise-soak-partitioned.toml",
    "scripts/benchmark/axvisor-rt/config/starry-orangepi-5-plus-smp2-shared.toml",
    "scripts/benchmark/axvisor-rt/config/starry-orangepi-5-plus-smp2-partitioned.toml",
    "scripts/benchmark/axvisor-rt/config/starry-orangepi-5-plus-smp2-soak-shared.toml",
    "scripts/benchmark/axvisor-rt/config/starry-orangepi-5-plus-smp2-soak-partitioned.toml",
)


class ContractError(ValueError):
    """The formal campaign contract or evidence is inconsistent."""


class Slot(NamedTuple):
    """One immutable position in the formal campaign execution order."""

    phase: str
    pair: int | None
    profile: str


def git_output(workspace: Path, *arguments: str) -> str:
    completed = subprocess.run(
        ["git", "-C", str(workspace), *arguments],
        check=False,
        capture_output=True,
        text=True,
    )
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip()
        raise ContractError(f"git {' '.join(arguments)} failed: {detail}")
    return completed.stdout.strip()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def resolve_record_path(path: str, workspace: Path) -> Path:
    candidate = Path(path)
    if candidate.is_absolute():
        return candidate.resolve()
    return (workspace / candidate).resolve()


def artifact_record(path: Path, workspace: Path) -> dict[str, object]:
    resolved = path.resolve()
    if not resolved.is_file() or resolved.stat().st_size <= 0:
        raise ContractError(f"formal campaign input is missing or empty: {resolved}")
    try:
        display_path = resolved.relative_to(workspace.resolve()).as_posix()
    except ValueError:
        display_path = str(resolved)
    return {
        "path": display_path,
        "sha256": sha256_file(resolved),
        "size_bytes": resolved.stat().st_size,
    }


def pair_order_document() -> list[dict[str, object]]:
    return [
        {"pair": pair, "order": list(order)}
        for pair, order in enumerate(PAIR_ORDER, start=1)
    ]


def measurement_contract(*, soak: bool) -> dict[str, object]:
    return {
        "workload": "idle",
        "iterations_per_metric": 10_000,
        "warmup_iterations": 100,
        "period_us": 90_000 if soak else 1_000,
        "guest_vcpus": 2,
        "measurement_cpu": 0,
        "stress_cpu": 1,
        "fifo_priority": 80,
        "host_noise": {
            "implementation": "busy-loop",
            "shared_pcpu": 1,
            "partitioned_pcpu": 3,
            "max_duration_ms": 3_600_000 if soak else 600_000,
        },
        **({"minimum_elapsed_ns": 1_800_000_000_000} if soak else {}),
    }


def acceptance_contract() -> dict[str, object]:
    return {
        "pair_count": 5,
        "direct_irq_p99_non_regression_limit_percent": 5.0,
        "direct_irq_max_improvement_target_percent": 10.0,
        "direct_irq_max_must_improve_in_at_least_pairs": 4,
        "snapshot_fsck": "clean",
        "linux_restore_required": True,
        "harvest_each_half_before_next_boot": True,
        "required_zero_counters": list(ZERO_COUNTERS),
        "required_console_absence": list(FORBIDDEN_CONSOLE_PATTERNS),
    }


def build_preregistration(
    workspace: Path,
    expected_commit: str,
    source_ref: str,
    board_type: str,
    service_id: str,
    hardware_id: str,
    hostname: str,
    artifacts: Mapping[str, Path],
    created_at: datetime | None = None,
    pair_timeout_seconds: int = 900,
    soak_timeout_seconds: int = 4500,
) -> dict[str, object]:
    """Build the immutable contract before the first measured board boot."""

    workspace = workspace.resolve()
    if COMMIT_PATTERN.fullmatch(expected_commit) is None:
        raise ContractError("expected commit must be a full lowercase Git SHA")
    if git_output(workspace, "rev-parse", "HEAD") != expected_commit:
        raise ContractError("workspace HEAD differs from the expected commit")
    if git_output(workspace, "status", "--porcelain=v1"):
        raise ContractError("formal campaign requires a clean Git worktree")
    if not source_ref or not board_type or not service_id or not hardware_id or not hostname:
        raise ContractError("source and board identity fields must be nonempty")
    if set(artifacts) != set(ARTIFACT_NAMES):
        raise ContractError("formal campaign artifact set differs from the contract")
    if pair_timeout_seconds <= 0 or soak_timeout_seconds <= 0:
        raise ContractError("board timeouts must be positive")
    timestamp = created_at or datetime.now(timezone.utc)
    if timestamp.tzinfo is None:
        raise ContractError("preregistration time must include a timezone")
    timestamp = timestamp.astimezone(timezone.utc).replace(microsecond=0)
    source_inputs = {
        relative: artifact_record(workspace / relative, workspace)
        for relative in FROZEN_SOURCE_PATHS
    }
    artifact_records = {
        name: artifact_record(artifacts[name], workspace)
        for name in ARTIFACT_NAMES
    }
    validate_host_toolchain_manifest(artifact_records["host_toolchain"], workspace)
    return {
        "schema_version": 2,
        "status": "preregistered",
        "created_at_utc": timestamp.isoformat().replace("+00:00", "Z"),
        "source": {
            "ref": source_ref,
            "commit": expected_commit,
            "tree": git_output(workspace, "show", "-s", "--format=%T", "HEAD"),
            "clean_worktree_required": True,
        },
        "board": {
            "type": board_type,
            "service_id": service_id,
            "hardware_id": hardware_id,
            "hostname": hostname,
        },
        "artifacts": artifact_records,
        "source_inputs": source_inputs,
        "pair_order": pair_order_document(),
        "soak_order": list(SOAK_ORDER),
        "measurement": {
            "pair": measurement_contract(soak=False),
            "soak": measurement_contract(soak=True),
        },
        "timeouts_seconds": {
            "pair": pair_timeout_seconds,
            "soak": soak_timeout_seconds,
        },
        "acceptance": acceptance_contract(),
    }


def require_object(
    parent: Mapping[str, object], key: str, label: str
) -> dict[str, object]:
    value = parent.get(key)
    if not isinstance(value, dict):
        raise ContractError(f"{label} {key} must be an object")
    return value


def require_string(parent: Mapping[str, object], key: str, label: str) -> str:
    value = parent.get(key)
    if not isinstance(value, str) or not value:
        raise ContractError(f"{label} {key} must be a nonempty string")
    return value


def validate_record(
    name: str, record: Mapping[str, object], workspace: Path
) -> None:
    if set(record) != {"path", "sha256", "size_bytes"}:
        raise ContractError(f"{name} artifact record has the wrong fields")
    display_path = require_string(record, "path", name)
    expected_digest = require_string(record, "sha256", name)
    if SHA256_PATTERN.fullmatch(expected_digest) is None:
        raise ContractError(f"{name} SHA-256 is malformed")
    size = record.get("size_bytes")
    if isinstance(size, bool) or not isinstance(size, int) or size <= 0:
        raise ContractError(f"{name} size_bytes must be a positive integer")
    path = resolve_record_path(display_path, workspace)
    if not path.is_file():
        raise ContractError(f"{name} artifact is missing: {path}")
    if path.stat().st_size != size:
        raise ContractError(f"{name} byte length differs from the preregistration")
    if sha256_file(path) != expected_digest:
        raise ContractError(f"{name} SHA-256 differs from the preregistration")


def command_output(path: Path, *arguments: str) -> str:
    try:
        completed = subprocess.run(
            [str(path), *arguments],
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ContractError(f"cannot execute host tool {path}: {error}") from error
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip()
        raise ContractError(f"host tool {path} failed: {detail}")
    return completed.stdout.strip()


def validate_host_toolchain_manifest(
    record: Mapping[str, object], workspace: Path
) -> None:
    manifest_path = resolve_record_path(
        require_string(record, "path", "host_toolchain"), workspace
    )
    manifest = read_json(manifest_path, "host toolchain manifest")
    if manifest.get("schema_version") != 1:
        raise ContractError("host toolchain manifest schema is invalid")
    if manifest.get("purpose") != "StarryOS freestanding C objects and bindings":
        raise ContractError("host toolchain purpose differs from the contract")
    target = require_object(manifest, "target", "host toolchain")
    if set(target) != {"machine", "sysroot"}:
        raise ContractError("host toolchain target has the wrong fields")
    machine = require_string(target, "machine", "host toolchain target")
    if machine != "aarch64-linux-gnu":
        raise ContractError("host toolchain target machine differs from the contract")
    sysroot = Path(require_string(target, "sysroot", "host toolchain target"))
    if not sysroot.is_absolute() or not sysroot.is_dir():
        raise ContractError("host toolchain sysroot is missing or not absolute")

    tools: dict[str, tuple[Path, str]] = {}
    for name in ("compiler", "archiver"):
        tool = require_object(manifest, name, "host toolchain")
        if set(tool) != {"path", "sha256", "size_bytes", "version"}:
            raise ContractError(f"host toolchain {name} has the wrong fields")
        validate_record(
            name,
            {key: tool[key] for key in ("path", "sha256", "size_bytes")},
            workspace,
        )
        tools[name] = (
            resolve_record_path(require_string(tool, "path", name), workspace),
            require_string(tool, "version", name),
        )

    wrappers = require_object(manifest, "wrappers", "host toolchain")
    if set(wrappers) != {"compiler", "archiver"}:
        raise ContractError("host toolchain wrapper set differs from the contract")
    wrapper_paths: dict[str, Path] = {}
    for name in ("compiler", "archiver"):
        wrapper = require_object(wrappers, name, "host toolchain wrappers")
        validate_record(f"{name} wrapper", wrapper, workspace)
        wrapper_paths[name] = resolve_record_path(
            require_string(wrapper, "path", f"{name} wrapper"), workspace
        )

    compiler, compiler_version = tools["compiler"]
    archiver, archiver_version = tools["archiver"]
    if command_output(compiler, "-dumpmachine") != machine:
        raise ContractError("host compiler target differs from the manifest")
    if command_output(compiler, "-dumpfullversion", "-dumpversion") != compiler_version:
        raise ContractError("host compiler version differs from the manifest")
    if command_output(archiver, "--version").splitlines()[:1] != [archiver_version]:
        raise ContractError("host archiver version differs from the manifest")
    if command_output(wrapper_paths["compiler"], "-print-sysroot") != str(sysroot):
        raise ContractError("host compiler wrapper sysroot differs from the manifest")
    if command_output(wrapper_paths["compiler"], "-dumpmachine") != machine:
        raise ContractError("host compiler wrapper target differs from the manifest")
    wrapper_archiver_version = command_output(
        wrapper_paths["archiver"], "--version"
    ).splitlines()[:1]
    if wrapper_archiver_version != [archiver_version]:
        raise ContractError("host archiver wrapper differs from the manifest")


def validate_preregistration(
    document: Mapping[str, object], workspace: Path, require_clean: bool = True
) -> None:
    """Revalidate every frozen source and runtime input before a campaign step."""

    workspace = workspace.resolve()
    if document.get("schema_version") != 2 or document.get("status") != "preregistered":
        raise ContractError("formal preregistration schema or status is invalid")
    source = require_object(document, "source", "preregistration")
    commit = require_string(source, "commit", "source")
    tree = require_string(source, "tree", "source")
    if COMMIT_PATTERN.fullmatch(commit) is None or COMMIT_PATTERN.fullmatch(tree) is None:
        raise ContractError("source commit or tree is malformed")
    if git_output(workspace, "rev-parse", "HEAD") != commit:
        raise ContractError("workspace HEAD differs from the preregistered commit")
    if git_output(workspace, "show", "-s", "--format=%T", "HEAD") != tree:
        raise ContractError("workspace tree differs from the preregistered tree")
    if require_clean and git_output(workspace, "status", "--porcelain=v1"):
        raise ContractError("formal campaign requires a clean Git worktree")

    source_inputs = require_object(document, "source_inputs", "preregistration")
    if set(source_inputs) != set(FROZEN_SOURCE_PATHS):
        raise ContractError("preregistered source input set differs from the contract")
    for relative in FROZEN_SOURCE_PATHS:
        record = require_object(source_inputs, relative, "source_inputs")
        if record.get("path") != relative:
            raise ContractError(f"source input path differs for {relative}")
        validate_record(relative, record, workspace)

    artifacts = require_object(document, "artifacts", "preregistration")
    if set(artifacts) != set(ARTIFACT_NAMES):
        raise ContractError("preregistered runtime artifact set differs from the contract")
    for name in ARTIFACT_NAMES:
        validate_record(name, require_object(artifacts, name, "artifacts"), workspace)
    validate_host_toolchain_manifest(
        require_object(artifacts, "host_toolchain", "artifacts"), workspace
    )

    if document.get("pair_order") != pair_order_document():
        raise ContractError("pair order differs from the frozen AB/BA contract")
    if document.get("soak_order") != list(SOAK_ORDER):
        raise ContractError("soak order differs from the frozen contract")
    measurement = require_object(document, "measurement", "preregistration")
    if measurement.get("pair") != measurement_contract(soak=False):
        raise ContractError("pair measurement contract was modified")
    if measurement.get("soak") != measurement_contract(soak=True):
        raise ContractError("soak measurement contract was modified")
    timeouts = require_object(document, "timeouts_seconds", "preregistration")
    if set(timeouts) != {"pair", "soak"}:
        raise ContractError("preregistered timeout set differs from the contract")
    for phase in ("pair", "soak"):
        timeout = timeouts.get(phase)
        if isinstance(timeout, bool) or not isinstance(timeout, int) or timeout <= 0:
            raise ContractError(f"{phase} timeout must be a positive integer")
    if document.get("acceptance") != acceptance_contract():
        raise ContractError("formal acceptance contract was modified")


def validate_stage_log(
    document: Mapping[str, object], text: str
) -> dict[str, object]:
    """Bind one staging operation to the preregistered physical board."""

    board = require_object(document, "board", "preregistration")
    service_matches = list(BOARD_SERVICE_PATTERN.finditer(text))
    if len(service_matches) != 1:
        raise ContractError("stage log must contain exactly one board-service identity")
    board_type, service_id = service_matches[0].groups()
    if board_type != board.get("type"):
        raise ContractError("stage board type differs from the preregistration")
    if service_id != board.get("service_id"):
        raise ContractError("stage board-service ID differs from the preregistration")
    matches = list(BOARD_IDENTITY_PATTERN.finditer(text))
    if len(matches) != 1:
        raise ContractError("stage log must contain exactly one board identity")
    board_id, hostname, temperature_text = matches[0].groups()
    if board_id != board.get("hardware_id"):
        raise ContractError("stage hardware ID differs from the preregistration")
    if hostname != board.get("hostname"):
        raise ContractError("stage hostname differs from the preregistration")
    temperature = int(temperature_text)
    if not -40_000 <= temperature <= 150_000:
        raise ContractError("stage CPU temperature is outside the valid range")
    if "AXVISOR_RT_BOARD_STAGE_PASS" not in text:
        raise ContractError("stage completion proof is missing")
    if "AXVISOR_RT_BOARD_STAGE_COMPLETE " not in text:
        raise ContractError("stage final marker is missing")
    linux_root_ok = False
    for line in text.splitlines():
        fields = line.split()
        if len(fields) >= 3 and fields[0].startswith("/dev/") and fields[1] == "ext4":
            if "rw" in fields[2].split(","):
                linux_root_ok = True
                break
    if not linux_root_ok:
        raise ContractError("stage log lacks Linux ext4 read-write evidence")
    return {
        "type": board_type,
        "service_id": service_id,
        "board_id": board_id,
        "hostname": hostname,
        "cpu_temp_milli_c": temperature,
    }


def validate_harvest_identity(
    document: Mapping[str, object], text: str, stage_identity: Mapping[str, object]
) -> dict[str, object]:
    """Require the post-run Linux harvest to use the same physical board."""

    matches = list(BOARD_IDENTITY_PATTERN.finditer(text))
    if len(matches) != 1:
        raise ContractError("harvest log must contain exactly one board identity")
    board_id, hostname, temperature_text = matches[0].groups()
    board = require_object(document, "board", "preregistration")
    if board_id != board.get("hardware_id") or board_id != stage_identity.get("board_id"):
        raise ContractError("harvest hardware ID differs from stage or preregistration")
    if hostname != board.get("hostname") or hostname != stage_identity.get("hostname"):
        raise ContractError("harvest hostname differs from stage or preregistration")
    temperature = int(temperature_text)
    if not -40_000 <= temperature <= 150_000:
        raise ContractError("harvest CPU temperature is outside the valid range")
    return {
        "board_id": board_id,
        "hostname": hostname,
        "cpu_temp_milli_c": temperature,
    }


def expected_vm_config(profile: str, *, soak: bool) -> str:
    infix = "-soak" if soak else ""
    return (
        "scripts/benchmark/axvisor-rt/config/"
        f"starry-orangepi-5-plus-smp2{infix}-{profile}.toml"
    )


def validate_console_log(
    document: Mapping[str, object], text: str, profile: str, *, soak: bool
) -> None:
    """Require board-session, guest completion, snapshot, and Linux restore proof."""

    if profile not in SOAK_ORDER:
        raise ContractError(f"unsupported RT profile {profile!r}")
    board = require_object(document, "board", "preregistration")
    service_id = require_string(board, "service_id", "board")
    service_matches = re.findall(r"^[ ]+board_id: ([A-Za-z0-9._:-]+)$", text, re.MULTILINE)
    if service_matches != [service_id]:
        raise ContractError("board-service ID differs from the preregistration")
    required_fragments = (
        "AXVISOR_SNAPSHOT_SYNC_OK",
        "AXVISOR_RT_STARRY_CAPTURE_COMPLETE schema=1 workload=idle",
        "BOARD_LINUX_RESTORED",
    )
    for fragment in required_fragments:
        if fragment not in text:
            raise ContractError(f"console is missing required marker {fragment}")
    expected_config = (
        "axvisor-orangepi-5-plus-starry-host-noise-"
        f"{'soak' if soak else 'formal'}-{profile}.toml"
    )
    if expected_config not in text:
        raise ContractError("console does not identify the frozen AxVisor config")
    linux_root_ok = False
    for line in text.splitlines():
        fields = line.split()
        if len(fields) >= 3 and fields[0] == "/dev/mmcblk1p2" and fields[1] == "ext4":
            if "rw" in fields[2].split(","):
                linux_root_ok = True
                break
    if not linux_root_ok:
        raise ContractError("console lacks the restored /dev/mmcblk1p2 ext4 rw gate")
    for forbidden in FORBIDDEN_CONSOLE_PATTERNS:
        if forbidden in text:
            raise ContractError(f"console contains forbidden marker {forbidden}")


def validate_lossless_endpoint(endpoint: Mapping[str, object], label: str) -> None:
    records = endpoint.get("records")
    if isinstance(records, bool) or not isinstance(records, int) or records <= 0:
        raise ContractError(f"{label} lossless trace must contain records")
    for counter in ZERO_COUNTERS:
        if endpoint.get(counter) != 0:
            raise ContractError(f"{label} direct IRQ trace reports {counter}")


def validate_summary(
    document: Mapping[str, object],
    summary: Mapping[str, object],
    profile: str,
    *,
    soak: bool,
) -> None:
    """Validate one analyzed half against the preregistered measurement contract."""

    if summary.get("schema_version") != 1:
        raise ContractError("RT summary must use schema version 1")
    if profile not in SOAK_ORDER:
        raise ContractError(f"unsupported RT profile {profile!r}")
    measurement = require_object(document, "measurement", "preregistration")
    contract = require_object(measurement, "soak" if soak else "pair", "measurement")
    capture = require_object(summary, "capture", "summary")
    expected_capture = {
        "os": "starryos",
        "platform": "OrangePi-5-Plus",
        "profile": profile,
        "workload": contract["workload"],
        "vcpu_count": contract["guest_vcpus"],
        "iterations_per_metric": contract["iterations_per_metric"],
        "sample_count": int(contract["iterations_per_metric"]) * 3,
        "warmup_iterations": contract["warmup_iterations"],
        "period_us": contract["period_us"],
        "measurement_cpu": contract["measurement_cpu"],
        "stress_cpu": contract["stress_cpu"],
        "fifo_priority": contract["fifo_priority"],
    }
    for key, expected in expected_capture.items():
        if capture.get(key) != expected:
            raise ContractError(f"summary capture {key} differs from the contract")

    raw_input = require_object(summary, "input", "summary")
    if raw_input.get("snapshot_filesystem_state") != "clean":
        raise ContractError("authoritative snapshot filesystem is not clean")
    raw_digest = raw_input.get("sha256")
    if not isinstance(raw_digest, str) or SHA256_PATTERN.fullmatch(raw_digest) is None:
        raise ContractError("summary raw input SHA-256 is malformed")

    profile_contract = require_object(summary, "profile_contract", "summary")
    if profile_contract.get("dedicated_cpus") != (profile == "partitioned"):
        raise ContractError("summary dedicated CPU policy differs from the profile")
    if profile_contract.get("phys_cpu_sets") != ["0x2", "0x4"]:
        raise ContractError("summary vCPU placement differs from the contract")
    if profile_contract.get("soak") is not soak:
        raise ContractError("summary soak flag differs from the requested phase")
    if profile_contract.get("vm_config") != expected_vm_config(profile, soak=soak):
        raise ContractError("summary VM config differs from the frozen profile")

    noise_contract = require_object(contract, "host_noise", "measurement")
    noise = require_object(summary, "host_noise", "summary")
    expected_pcpu = noise_contract[f"{profile}_pcpu"]
    expected_mask = 1 << int(expected_pcpu)
    if noise.get("status") != "collected":
        raise ContractError("controlled host interference was not collected")
    if noise.get("requested_pcpu") != expected_pcpu:
        raise ContractError("host-noise requested pCPU differs from the contract")
    if noise.get("observed_pcpu_mask") != expected_mask:
        raise ContractError("host-noise observed pCPU mask differs from the contract")
    if noise.get("affinity_mask") != expected_mask:
        raise ContractError("host-noise affinity mask differs from the contract")
    pcpu_records = noise.get("pcpus")
    if not isinstance(pcpu_records, list) or len(pcpu_records) != 1:
        raise ContractError("host-noise pCPU observation is incomplete")
    pcpu_record = pcpu_records[0]
    if not isinstance(pcpu_record, dict) or pcpu_record.get("pcpu") != expected_pcpu:
        raise ContractError("host-noise pCPU observation differs from the contract")
    wall_ticks = pcpu_record.get("observed_wall_ticks")
    if isinstance(wall_ticks, bool) or not isinstance(wall_ticks, int) or wall_ticks <= 0:
        raise ContractError("host-noise observed wall time is invalid")
    if noise.get("covers_host_trace") is not True:
        raise ContractError("host noise does not cover the independent host trace")
    if noise.get("max_duration_ms") != noise_contract["max_duration_ms"]:
        raise ContractError("host-noise maximum duration differs from the contract")
    if noise.get("stop_reason") != "guest-complete":
        raise ContractError("host noise did not stop because the guest completed")
    elapsed_ns = noise.get("elapsed_ns")
    if isinstance(elapsed_ns, bool) or not isinstance(elapsed_ns, int) or elapsed_ns <= 0:
        raise ContractError("host-noise elapsed duration is invalid")
    if soak and elapsed_ns < 1_800_000_000_000:
        raise ContractError("soak host interference covered less than 1,800 seconds")

    accounting = require_object(summary, "host_pcpu_accounting", "summary")
    if accounting.get("status") != "collected":
        raise ContractError("host vCPU accounting was not collected")
    vcpus = accounting.get("vcpus")
    if not isinstance(vcpus, list):
        raise ContractError("host vCPU accounting list is missing")
    placement = sorted(
        (entry.get("vm"), entry.get("vcpu"), entry.get("pcpu_mask"), entry.get("migrations"))
        for entry in vcpus
        if isinstance(entry, dict)
    )
    if placement != [(1, 0, 2, 0), (1, 1, 4, 0)]:
        raise ContractError("host vCPU placement or migration evidence differs")

    irq = require_object(summary, "direct_irq_trace", "summary")
    pairing = require_object(irq, "pairing", "direct_irq_trace")
    pair_count = pairing.get("pair_count")
    if isinstance(pair_count, bool) or not isinstance(pair_count, int) or pair_count <= 0:
        raise ContractError("direct IRQ trace contains no paired events")
    lossless = require_object(irq, "lossless", "direct_irq_trace")
    for endpoint in ("host", "guest"):
        validate_lossless_endpoint(
            require_object(lossless, endpoint, "direct_irq_trace lossless"), endpoint
        )


def read_json(path: Path, label: str) -> dict[str, object]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ContractError(f"cannot read {label} {path}: {error}") from error
    if not isinstance(value, dict):
        raise ContractError(f"{label} must contain a JSON object")
    return value


def write_json_exclusive(path: Path, value: object) -> None:
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
