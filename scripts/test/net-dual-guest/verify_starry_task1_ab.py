#!/usr/bin/env python3
"""Verify the StarryOS/Zephyr RR versus bounded FP-RR Task 1 run."""

from __future__ import annotations

import argparse
import math
import re
import statistics
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path

from verify_pcap import analyze
from verify_starry_task23 import (
    ANSI_RE,
    KIND_CONTROL,
    KIND_STATUS,
    STARRY_IP,
    ZEPHYR_IP,
    matching,
    task2_frames,
)


SCHEDULER_FEATURES = {"rr-scheduler", "fp-rr-scheduler"}
GUEST_ARTIFACT_SUFFIXES = {
    "rootfs": "rootfs-aarch64-alpine.img",
    "endpoint": "starryos-task2-endpoint",
    "yolo_param": "yolo11n.ncnn.param",
    "yolo_model": "yolo11n.ncnn.bin",
    "yolo_input": "input.ppm",
    "starry": "starryos.bin",
}
RTT_RE = re.compile(r"STARRY_T2N1_STATUS_DELIVERED[^\n]*\brtt_ms=(\d+)")
LOWER_SERVICE_RE = re.compile(r"\blower_priority_services=(\d+)")


@dataclass(frozen=True)
class RttSummary:
    count: int
    minimum: int
    median: float
    p95: int
    maximum: int


@dataclass(frozen=True)
class ArmEvidence:
    label: str
    git_head: str
    controls: int
    statuses: int
    rtt: RttSummary
    lower_priority_services: int | None
    artifact_hashes: dict[str, str]


def summarize_rtts(samples: list[int]) -> RttSummary:
    if not samples:
        raise ValueError("no protocol RTT samples")
    ordered = sorted(samples)
    p95_index = math.ceil(0.95 * len(ordered)) - 1
    return RttSummary(
        count=len(ordered),
        minimum=ordered[0],
        median=statistics.median(ordered),
        p95=ordered[p95_index],
        maximum=ordered[-1],
    )


def verify_host_config_pair(rr_path: Path, fp_rr_path: Path) -> list[str]:
    rr = load_toml(rr_path)
    fp_rr = load_toml(fp_rr_path)
    rr_features = set(rr.pop("features", []))
    fp_rr_features = set(fp_rr.pop("features", []))
    failures: list[str] = []
    if rr != fp_rr:
        failures.append("host configs differ outside the scheduler feature")
    if rr_features & SCHEDULER_FEATURES != {"rr-scheduler"}:
        failures.append(
            f"RR host config scheduler features are invalid: {sorted(rr_features)}"
        )
    if fp_rr_features & SCHEDULER_FEATURES != {"fp-rr-scheduler"}:
        failures.append(
            f"FP-RR host config scheduler features are invalid: {sorted(fp_rr_features)}"
        )
    if rr_features - SCHEDULER_FEATURES != fp_rr_features - SCHEDULER_FEATURES:
        failures.append("host configs have different auxiliary features")
    return failures


def verify_scheduler_log(label: str, log: str) -> tuple[list[str], int | None]:
    if label == "rr":
        failures = require_patterns(log, (r"use Round-robin scheduler\.",))
        if "FP-RR scheduler counters:" in log:
            failures.append("RR arm unexpectedly exposed FP-RR counters")
        return failures, None

    failures = require_patterns(
        log,
        (
            r"use Fixed-priority round-robin scheduler\.",
            r"FP-RR scheduler counters:",
        ),
    )
    match = LOWER_SERVICE_RE.search(log)
    lower_priority_services = int(match.group(1)) if match else None
    if lower_priority_services is None:
        failures.append("FP-RR arm has no lower_priority_services counter")
    elif lower_priority_services == 0:
        failures.append("FP-RR bounded lower-priority service path was not exercised")
    return failures, lower_priority_services


def verify_guest_artifact_equivalence(
    rr_hashes: dict[str, str], fp_rr_hashes: dict[str, str], rtos_name: str
) -> list[str]:
    failures: list[str] = []
    suffixes = dict(GUEST_ARTIFACT_SUFFIXES)
    suffixes[rtos_name] = f"{rtos_name}-task2.bin"
    for role in suffixes:
        rr_hash = rr_hashes.get(role)
        fp_rr_hash = fp_rr_hashes.get(role)
        if rr_hash is None or fp_rr_hash is None:
            failures.append(f"missing {role} artifact hash in one or both arms")
        elif rr_hash != fp_rr_hash:
            failures.append(f"{role} artifact differs between scheduler arms")
    return failures


def verify_shared_pcpu_configs(
    starry_path: Path, rtos_path: Path, rtos_name: str
) -> list[str]:
    starry = load_toml(starry_path).get("base", {})
    rtos = load_toml(rtos_path).get("base", {})
    failures: list[str] = []
    rtos_label = "RT-Thread" if rtos_name == "rtthread" else "Zephyr"
    expectations = (
        ("StarryOS", starry, 89),
        (rtos_label, rtos, 90),
    )
    for guest, base, priority in expectations:
        if not isinstance(base, dict):
            failures.append(f"{guest} config has no base table")
            continue
        if base.get("cpu_num") != 1 or base.get("phys_cpu_ids") != [1]:
            failures.append(f"{guest} is not a one-vCPU Guest pinned to pCPU1")
        if base.get("host_sched_priority") != priority:
            failures.append(f"{guest} host priority is not {priority}")
    return failures


def verify_rootfs_content_hashes(path: Path) -> list[str]:
    hashes = read_key_values(path)
    failures: list[str] = []
    for artifact in ("endpoint", "script", "yolo_param", "yolo_model", "yolo_input"):
        host = hashes.get(f"host_{artifact}_sha256")
        rootfs = hashes.get(f"rootfs_{artifact}_sha256")
        if host is None or rootfs is None:
            failures.append(f"rootfs evidence is missing the {artifact} hash pair")
        elif host != rootfs:
            failures.append(f"rootfs {artifact} does not match the current host artifact")
    return failures


def verify_ab(
    rr_dir: Path, fp_rr_dir: Path, rtos_name: str
) -> tuple[list[str], ArmEvidence, ArmEvidence]:
    rr, rr_failures = load_arm(rr_dir, "rr", rtos_name)
    fp_rr, fp_rr_failures = load_arm(fp_rr_dir, "fp-rr", rtos_name)
    failures = rr_failures + fp_rr_failures
    failures.extend(
        verify_host_config_pair(
            rr_dir / "host-config.toml", fp_rr_dir / "host-config.toml"
        )
    )
    for name in ("qemu.toml", "vm-starry.toml", f"vm-{rtos_name}.toml"):
        if (rr_dir / name).read_bytes() != (fp_rr_dir / name).read_bytes():
            failures.append(f"{name} differs between scheduler arms")
    failures.extend(
        verify_shared_pcpu_configs(
            rr_dir / "vm-starry.toml", rr_dir / f"vm-{rtos_name}.toml", rtos_name
        )
    )
    if rr.git_head != fp_rr.git_head:
        failures.append(
            f"scheduler arms used different Git revisions: {rr.git_head}/{fp_rr.git_head}"
        )
    failures.extend(
        verify_guest_artifact_equivalence(
            rr.artifact_hashes, fp_rr.artifact_hashes, rtos_name
        )
    )
    return failures, rr, fp_rr


def load_arm(
    directory: Path, label: str, rtos_name: str
) -> tuple[ArmEvidence, list[str]]:
    log = ANSI_RE.sub("", (directory / "run.log").read_text(errors="replace"))
    command = read_key_values(directory / "command.txt")
    frames = task2_frames(directory / "starry.pcap")
    starry_report = analyze(directory / "starry.pcap", None)
    rtos_report = analyze(directory / f"{rtos_name}.pcap", None)
    controls = len(
        matching(frames, src=STARRY_IP, dst=ZEPHYR_IP, kind=KIND_CONTROL)
    )
    statuses = len(
        matching(frames, src=ZEPHYR_IP, dst=STARRY_IP, kind=KIND_STATUS)
    )
    rtt = summarize_rtts([int(value) for value in RTT_RE.findall(log)])
    scheduler_failures, lower_priority_services = verify_scheduler_log(label, log)
    failures = scheduler_failures
    git_head = command.get("git_head", "")
    if not re.fullmatch(r"[0-9a-f]{40}", git_head):
        failures.append(f"{label} arm has no full Git revision")
    failures.extend(verify_rootfs_content_hashes(directory / "rootfs-content-hashes.txt"))
    failures.extend(
        require_patterns(
            log,
            (
                r"TASK2_READY\b",
                r"TASK3_MODEL_READY model=yolo11n\.ncnn runtime=ncnn[^\n]*mode=in-guest",
                r"TASK3_INFER model=yolo11n\.ncnn[^\n]*request=3\b",
                r"TASK3_DETECTION model=yolo11n\.ncnn[^\n]*request=3\b",
                r"STARRY_T2N1_PASS\b",
                r"STARRY_T2N1_STATUS_DELIVERED[^\n]*request=3\b",
                r"RT vCPU wait counters:",
            ),
        )
    )
    if controls < 3 or statuses < 3:
        failures.append(f"{label} arm has only {controls}/{statuses} CONTROL/STATUS frames")
    if rtt.count < 3:
        failures.append(f"{label} arm has only {rtt.count} RTT samples")
    if starry_report["task2_signature"] != rtos_report["task2_signature"]:
        failures.append(f"{label} arm dual-ended T2N1 ledgers differ")
    for verifier_log in ("verify-pcap.log", "verify-scenario.log"):
        if "PASS" not in (directory / verifier_log).read_text(errors="replace"):
            failures.append(f"{label} arm {verifier_log} is not a PASS")
    return (
        ArmEvidence(
            label=label,
            git_head=git_head,
            controls=controls,
            statuses=statuses,
            rtt=rtt,
            lower_priority_services=lower_priority_services,
            artifact_hashes=read_artifact_hashes(
                directory / "artifact-hashes.txt", rtos_name
            ),
        ),
        failures,
    )


def load_toml(path: Path) -> dict[str, object]:
    with path.open("rb") as stream:
        return tomllib.load(stream)


def read_key_values(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    for line in path.read_text(errors="replace").splitlines():
        key, separator, value = line.partition("=")
        if separator:
            values[key] = value
    return values


def read_artifact_hashes(path: Path, rtos_name: str) -> dict[str, str]:
    hashes: dict[str, str] = {}
    suffixes = dict(GUEST_ARTIFACT_SUFFIXES)
    suffixes[rtos_name] = f"{rtos_name}-task2.bin"
    for line in path.read_text(errors="replace").splitlines():
        digest, separator, artifact = line.partition("  ")
        if not separator:
            continue
        for role, suffix in suffixes.items():
            if artifact.endswith(suffix):
                hashes[role] = digest
    return hashes


def require_patterns(log: str, patterns: tuple[str, ...]) -> list[str]:
    return [
        f"runtime log missing {pattern!r}"
        for pattern in patterns
        if not re.search(pattern, log)
    ]


def format_summary(rr: ArmEvidence, fp_rr: ArmEvidence, rtos_name: str) -> str:
    def row(arm: ArmEvidence) -> str:
        rtt = arm.rtt
        service = (
            "n/a"
            if arm.lower_priority_services is None
            else str(arm.lower_priority_services)
        )
        return (
            f"| {arm.label} | {arm.controls} | {arm.statuses} | {rtt.count} | "
            f"{rtt.minimum} | {rtt.median:g} | {rtt.p95} | {rtt.maximum} | {service} |"
        )

    return "\n".join(
        (
            "# StarryOS Task 1 scheduler A/B",
            "",
            f"Git revision: `{rr.git_head}`",
            "",
            f"Both arms use the same StarryOS rootfs/endpoint/kernel, {rtos_name} image, "
            "QEMU topology and shared-pCPU Guest configs. Only the AxVisor scheduler "
            "feature changes. Results are QEMU software-in-the-loop observations, not "
            "physical-board WCET bounds.",
            "",
            "| Scheduler | CONTROL | STATUS | RTT count | min ms | median ms | "
            "p95 ms | max ms | lower-priority services |",
            "|---|---:|---:|---:|---:|---:|---:|---:|---:|",
            row(rr),
            row(fp_rr),
            "",
            "The p95 value uses the nearest-rank definition. Scheduler timing is "
            "reported as an observation only; the acceptance claim is that both Guests "
            "remain live and complete the same T2N1/ncnn/YOLO workload, and that the bounded "
            "FP-RR service path is exercised.",
            "",
        )
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rr-dir", type=Path, required=True)
    parser.add_argument("--fp-rr-dir", type=Path, required=True)
    parser.add_argument(
        "--rtos-name", choices=("zephyr", "rtthread"), default="zephyr"
    )
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        failures, rr, fp_rr = verify_ab(
            args.rr_dir, args.fp_rr_dir, args.rtos_name
        )
    except (OSError, ValueError, tomllib.TOMLDecodeError) as error:
        print(f"FAIL: {error}")
        return 1
    print(
        f"rr controls={rr.controls} statuses={rr.statuses} rtt={rr.rtt} "
        f"fp-rr controls={fp_rr.controls} statuses={fp_rr.statuses} rtt={fp_rr.rtt} "
        f"lower_priority_services={fp_rr.lower_priority_services}"
    )
    if failures:
        print("FAIL")
        print("\n".join(f"- {failure}" for failure in failures))
        return 1
    args.output.write_text(format_summary(rr, fp_rr, args.rtos_name))
    print(f"PASS: StarryOS Task 1 scheduler A/B; summary={args.output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
