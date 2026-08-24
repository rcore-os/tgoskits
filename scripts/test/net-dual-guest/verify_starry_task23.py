#!/usr/bin/env python3
"""Verify StarryOS/Zephyr Task-2 and Task-3 scenario evidence."""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path

from verify_pcap import analyze, parse_task2_frame, parse_udp, read_pcap


KIND_CONTROL = 1
KIND_STATUS = 2
KIND_ERROR = 3
KIND_ACK = 4
KIND_HEARTBEAT = 5
STARRY_IP = "10.0.42.15"
ZEPHYR_IP = "10.0.42.2"
ANSI_RE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
YOLO_READY_PATTERN = (
    r"TASK3_MODEL_READY model=yolo11n\.ncnn runtime=ncnn"
    r"(?:[^\n]*\n){0,16}[^\n]*mode=in-guest"
)


@dataclass(frozen=True)
class WireFrame:
    src: str
    dst: str
    kind: int
    sequence: int
    acknowledgement: int
    error_code: int
    body: bytes


def task2_frames(path: Path) -> list[WireFrame]:
    _, packets = read_pcap(path)
    frames: list[WireFrame] = []
    for packet in packets:
        udp = parse_udp(packet)
        if udp is None:
            continue
        src, dst, _, _, payload = udp
        parsed = parse_task2_frame(payload)
        if parsed is None:
            continue
        kind, sequence, acknowledgement, body = parsed
        frames.append(
            WireFrame(
                src=src,
                dst=dst,
                kind=kind,
                sequence=sequence,
                acknowledgement=acknowledgement,
                error_code=int.from_bytes(payload[22:24], "big"),
                body=body,
            )
        )
    return frames


def require_patterns(log: str, patterns: tuple[str, ...]) -> list[str]:
    return [
        f"runtime log missing {pattern!r}"
        for pattern in patterns
        if not re.search(pattern, log)
    ]


def require_order(log: str, markers: tuple[str, ...]) -> list[str]:
    position = 0
    for marker in markers:
        position = log.find(marker, position)
        if position < 0:
            return [f"runtime marker order is invalid: {markers}"]
        position += len(marker)
    return []


def matching(
    frames: list[WireFrame],
    *,
    src: str | None = None,
    dst: str | None = None,
    kind: int | None = None,
    sequence: int | None = None,
    acknowledgement: int | None = None,
    error_code: int | None = None,
) -> list[WireFrame]:
    return [
        frame
        for frame in frames
        if (src is None or frame.src == src)
        and (dst is None or frame.dst == dst)
        and (kind is None or frame.kind == kind)
        and (sequence is None or frame.sequence == sequence)
        and (acknowledgement is None or frame.acknowledgement == acknowledgement)
        and (error_code is None or frame.error_code == error_code)
    ]


def verify_normal(frames: list[WireFrame], log: str) -> list[str]:
    failures = require_patterns(
        log,
        (
            r"STARRY_T2N1_PASS\b",
            r"STARRY_T2N1_STATUS_DELIVERED[^\n]*request=3\b",
            r"TASK3_INFER model=yolo11n\.ncnn[^\n]*request=3\b",
            r"TASK3_DETECTION model=yolo11n\.ncnn[^\n]*request=3\b",
        ),
    )
    controls = matching(frames, src=STARRY_IP, dst=ZEPHYR_IP, kind=KIND_CONTROL)
    statuses = matching(frames, src=ZEPHYR_IP, dst=STARRY_IP, kind=KIND_STATUS)
    if len(controls) < 3 or len(statuses) < 3:
        failures.append(
            f"persistent loop needs at least three CONTROL/STATUS frames, got {len(controls)}/{len(statuses)}"
        )
    return failures


def verify_drop_ack(frames: list[WireFrame], log: str) -> list[str]:
    failures = require_patterns(
        log,
        (
            r"TASK2_FAULT_DROP_ACK seq=1\b",
            r"STARRY_T2N1_RETRANSMIT seq=1 attempt=1\b",
            r"TASK2_FAULT_DROP_ACK_RECOVERED duplicate_seq=1\b",
            r"STARRY_T2N1_ACK seq=1\b",
            r"STARRY_T2N1_PASS\b",
        ),
    )
    controls = matching(
        frames,
        src=STARRY_IP,
        dst=ZEPHYR_IP,
        kind=KIND_CONTROL,
        sequence=1,
    )
    acknowledgements = matching(
        frames,
        src=ZEPHYR_IP,
        dst=STARRY_IP,
        kind=KIND_ACK,
        acknowledgement=1,
    )
    if len(controls) < 2:
        failures.append(f"CONTROL sequence 1 was not retransmitted: count={len(controls)}")
    if len(acknowledgements) != 1:
        failures.append(f"expected one eventual ACK for sequence 1, got {len(acknowledgements)}")
    return failures


def verify_retry_exhausted(frames: list[WireFrame], log: str) -> list[str]:
    failures = require_patterns(
        log,
        (
            r"TASK2_FAULT_MODE mode=drop-ack-always",
            r"TASK2_FAULT_DROP_ACK_ALWAYS seq=1\b",
            r"STARRY_T2N1_RETRANSMIT seq=1 attempt=5\b",
            r"STARRY_T2N1_SAFE source=protocol reason=RetryExhausted",
            r"STARRY_T2N1_RECOVERED state=Active",
        ),
    )
    failures.extend(
        require_order(
            log,
            (
                "TASK2_FAULT_DROP_ACK_ALWAYS seq=1",
                "STARRY_T2N1_RETRANSMIT seq=1 attempt=5",
                "STARRY_T2N1_SAFE source=protocol reason=RetryExhausted",
                "STARRY_T2N1_RECOVERED state=Active",
            ),
        )
    )
    controls = matching(
        frames,
        src=STARRY_IP,
        dst=ZEPHYR_IP,
        kind=KIND_CONTROL,
        sequence=1,
    )
    acknowledgements = matching(
        frames,
        src=ZEPHYR_IP,
        dst=STARRY_IP,
        kind=KIND_ACK,
        acknowledgement=1,
    )
    if len(controls) < 6:
        failures.append(f"retry exhaustion needs initial CONTROL plus five retries, got {len(controls)}")
    if acknowledgements:
        failures.append(f"retry-exhausted capture contains {len(acknowledgements)} CONTROL ACK(s)")
    return failures


def verify_out_of_order(frames: list[WireFrame], log: str) -> list[str]:
    failures = require_patterns(
        log,
        (
            r"TASK2_PROTOCOL_ERROR out_of_order=2 expected=1\b",
            r"STARRY_T2N1_FAULT_RECOVERY_COMPLETE mode=out-of-order[^\n]*safe_observed=true recovered=true",
            r"STARRY_T2N1_PASS\b",
        ),
    )
    injected = matching(
        frames,
        src=STARRY_IP,
        dst=ZEPHYR_IP,
        kind=KIND_CONTROL,
        sequence=2,
    )
    errors = matching(
        frames,
        src=ZEPHYR_IP,
        dst=STARRY_IP,
        kind=KIND_ERROR,
        acknowledgement=2,
        error_code=2,
    )
    if not injected:
        failures.append("wire capture has no out-of-order CONTROL sequence 2")
    if not errors:
        failures.append("wire capture has no OutOfOrder ERROR acknowledging sequence 2")
    return failures


def verify_invalid_parameter(frames: list[WireFrame], log: str) -> list[str]:
    failures = require_patterns(
        log,
        (
            r"TASK2_PROTOCOL_ERROR invalid_parameter seq=1\b",
            r"STARRY_T2N1_FAULT_RECOVERY_COMPLETE mode=invalid-parameter[^\n]*safe_observed=true recovered=true",
            r"STARRY_T2N1_PASS\b",
        ),
    )
    invalid_controls = [
        frame
        for frame in matching(
            frames,
            src=STARRY_IP,
            dst=ZEPHYR_IP,
            kind=KIND_CONTROL,
            sequence=1,
        )
        if len(frame.body) == 12
        and int.from_bytes(frame.body[4:8], "big", signed=True) == 1001
    ]
    errors = matching(
        frames,
        src=ZEPHYR_IP,
        dst=STARRY_IP,
        kind=KIND_ERROR,
        acknowledgement=1,
        error_code=1,
    )
    if not invalid_controls:
        failures.append("wire capture has no checksum-valid CONTROL value 1001")
    if not errors:
        failures.append("wire capture has no InvalidParameter ERROR acknowledging sequence 1")
    return failures


def verify_blackout(frames: list[WireFrame], log: str) -> list[str]:
    failures = require_patterns(
        log,
        (
            r"virtnet: blackout ON",
            r"STARRY_T2N1_SAFE source=protocol",
            r"TASK2_SAFE state=Safe event=HeartbeatTimeout",
            r"virtnet: blackout OFF",
            r"STARRY_T2N1_RECOVERED state=Active",
            r"STARRY_T2N1_FAULT_RECOVERY_COMPLETE mode=normal[^\n]*safe_observed=true recovered=true",
            r"TASK2_CONTROL_RECEIVED[^\n]*request=",
        ),
    )
    failures.extend(
        require_order(
            log,
            (
                "virtnet: blackout ON",
                "STARRY_T2N1_SAFE source=protocol",
                "TASK2_SAFE state=Safe event=HeartbeatTimeout",
                "virtnet: blackout OFF",
                "STARRY_T2N1_RECOVERED state=Active",
                "STARRY_T2N1_FAULT_RECOVERY_COMPLETE mode=normal",
            ),
        )
    )
    recovery_marker = "STARRY_T2N1_FAULT_RECOVERY_COMPLETE mode=normal"
    recovery_position = log.find(recovery_marker)
    recovered_control_position = log.find(
        "TASK2_CONTROL_RECEIVED", recovery_position + len(recovery_marker)
    )
    if recovery_position < 0 or recovered_control_position < 0:
        failures.append("runtime log has no Zephyr CONTROL after completed recovery")
    recovered_infer_position = log.find(
        "TASK3_INFER model=yolo11n.ncnn", log.find("STARRY_T2N1_RECOVERED state=Active")
    )
    if recovered_infer_position < 0:
        failures.append("runtime log has no real YOLO inference after blackout recovery")
    recovered_controls = matching(
        frames,
        src=STARRY_IP,
        dst=ZEPHYR_IP,
        kind=KIND_CONTROL,
        sequence=1,
    )
    recovered_statuses = matching(
        frames,
        src=ZEPHYR_IP,
        dst=STARRY_IP,
        kind=KIND_STATUS,
        sequence=1,
    )
    if len(recovered_controls) < 2 or len(recovered_statuses) < 2:
        failures.append(
            "wire capture does not prove sequence-one CONTROL/STATUS before and after recovery: "
            f"{len(recovered_controls)}/{len(recovered_statuses)}"
        )
    return failures


def verify_model_rejected(frames: list[WireFrame], log: str) -> list[str]:
    failures = require_patterns(
        log,
        (
            r"TASK3_MODEL_READY(?:[^\n]*\n){0,16}[^\n]*run_mode=model-rejected",
            r"TASK3_MODEL_REJECTED[^\n]*reason=InjectedInvalidOutput",
            r"STARRY_T2N1_SAFE source=model reason=InjectedInvalidOutput",
        ),
    )
    controls = matching(frames, src=STARRY_IP, dst=ZEPHYR_IP, kind=KIND_CONTROL)
    if controls:
        failures.append(f"model rejection emitted {len(controls)} unsafe CONTROL frame(s)")
    if "STARRY_T2N1_CONTROL_SENT" in log:
        failures.append("model rejection log contains a CONTROL send marker")
    if "TASK3_DETECTION" in log:
        failures.append("model rejection log contains an accepted detection marker")
    if not matching(frames, kind=KIND_HEARTBEAT):
        failures.append("model rejection capture has no T2N1 heartbeat liveness traffic")
    return failures


VERIFY_SCENARIO = {
    "normal": verify_normal,
    "drop-ack": verify_drop_ack,
    "retry-exhausted": verify_retry_exhausted,
    "out-of-order": verify_out_of_order,
    "invalid-parameter": verify_invalid_parameter,
    "blackout": verify_blackout,
    "model-rejected": verify_model_rejected,
}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--scenario", choices=tuple(VERIFY_SCENARIO), required=True)
    parser.add_argument("--starry-pcap", type=Path, required=True)
    parser.add_argument("--zephyr-pcap", type=Path, required=True)
    parser.add_argument("--run-log", type=Path, required=True)
    args = parser.parse_args()

    try:
        starry_report = analyze(args.starry_pcap, None)
        zephyr_report = analyze(args.zephyr_pcap, None)
        frames = task2_frames(args.starry_pcap)
        log = ANSI_RE.sub("", args.run_log.read_text(encoding="utf-8", errors="replace"))
    except (OSError, ValueError) as error:
        print(f"FAIL: {error}")
        return 1

    failures: list[str] = []
    failures.extend(require_patterns(log, (YOLO_READY_PATTERN,)))
    if "embedded:fixture-replay" in log or "model=cnn" in log:
        failures.append("runtime log contains a fixture or CNN path instead of real ncnn/YOLO")
    if args.scenario != "model-rejected":
        failures.extend(
            require_patterns(
                log,
                (
                    r"TASK3_INFER model=yolo11n\.ncnn",
                    r"TASK3_DETECTION model=yolo11n\.ncnn",
                ),
            )
        )
    if starry_report["task2_signature"] != zephyr_report["task2_signature"]:
        failures.append("StarryOS and Zephyr captures have different T2N1 ledgers")
    failures.extend(VERIFY_SCENARIO[args.scenario](frames, log))

    print(
        f"scenario={args.scenario} frames={len(frames)} "
        f"kinds={dict(starry_report['task2_kinds'])}"
    )
    if failures:
        print("FAIL")
        print("\n".join(f"- {failure}" for failure in failures))
        return 1
    print(f"PASS: StarryOS/Zephyr scenario {args.scenario}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
