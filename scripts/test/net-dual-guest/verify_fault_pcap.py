#!/usr/bin/env python3
"""Verify a bounded ACK-loss/retransmission P3 capture pair.

Normal P2 verification requires identical ledgers.  An injected loss
intentionally violates that invariant for exactly the dropped frame, so this
verifier checks the expected one-sided delta and independently requires
retransmission/duplicate evidence from the Guest and proxy logs.
"""

from __future__ import annotations

import argparse
from collections import Counter
from pathlib import Path
import re
import sys

from verify_pcap import analyze


KIND_NAMES = {
    "control": 1,
    "status": 2,
    "error": 3,
    "ack": 4,
    "heartbeat": 5,
}
SIGNATURE = tuple[str, str, int, int, int]
ANSI_RE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
VM_PREFIX_RE = re.compile(r"^\[VM (\d+)\] ?(.*)$")


def check_expected_fault_delta(
    delivered: Counter[SIGNATURE],
    source_capture: Counter[SIGNATURE],
    source_ip: str,
    destination_ip: str,
    kind: int,
    acknowledgement: int,
    drop_count: int,
) -> list[str]:
    """Check that only the intentionally dropped directed frames differ."""
    expected = Counter(
        {
            (source_ip, destination_ip, kind, 0, acknowledgement): drop_count
        }
    )
    extra = source_capture - delivered
    missing = delivered - source_capture
    failures: list[str] = []
    if extra != expected:
        failures.append(f"unexpected source-capture delta: expected={dict(expected)} actual={dict(extra)}")
    if missing:
        failures.append(f"delivered capture has frames absent from source capture: {dict(missing)}")
    return failures


def require_log_patterns(path: Path, patterns: tuple[str, ...]) -> list[str]:
    """Require each runtime evidence pattern in one log artifact."""
    try:
        content = ANSI_RE.sub("", path.read_text(encoding="utf-8", errors="replace"))
    except OSError as error:
        return [f"cannot read {path}: {error}"]
    guest_fragments: dict[int, list[str]] = {}
    for line in content.splitlines():
        match = VM_PREFIX_RE.match(line)
        if match:
            guest_fragments.setdefault(int(match.group(1)), []).append(match.group(2))
    searchable = content + "\n" + "\n".join(
        "".join(fragments) for fragments in guest_fragments.values()
    )
    return [
        f"{path}: missing evidence {pattern!r}"
        for pattern in patterns
        if not re.search(pattern, searchable)
    ]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("delivered_pcap", type=Path, help="capture at the receiving QEMU netdev")
    parser.add_argument("source_pcap", type=Path, help="capture at the sending QEMU netdev")
    parser.add_argument("--drop-src", required=True)
    parser.add_argument("--drop-dst", required=True)
    parser.add_argument("--drop-kind", choices=tuple(KIND_NAMES), default="ack")
    parser.add_argument("--drop-ack", type=int, required=True)
    parser.add_argument("--drop-count", type=int, default=1)
    parser.add_argument("--min-udp", type=int, default=3)
    parser.add_argument("--guest-log", type=Path, required=True)
    parser.add_argument("--proxy-log", type=Path, required=True)
    args = parser.parse_args()
    if args.drop_count < 1:
        parser.error("--drop-count must be positive")

    try:
        delivered = analyze(args.delivered_pcap, None)
        source = analyze(args.source_pcap, None)
    except (OSError, ValueError) as error:
        print(f"FAIL: {error}")
        return 1

    failures: list[str] = []
    for report in (delivered, source):
        stats = report["stats"]
        if stats["packets"] == 0:
            failures.append(f"{report['path']}: no packets captured")
        if stats["udp"] < args.min_udp:
            failures.append(f"{report['path']}: UDP packets={stats['udp']} < {args.min_udp}")
        if not report["task2_kinds"]:
            failures.append(f"{report['path']}: no T2N1 frames found")

    delivered_signature = delivered["task2_signature"]
    source_signature = source["task2_signature"]
    failures.extend(
        check_expected_fault_delta(
            delivered_signature,
            source_signature,
            args.drop_src,
            args.drop_dst,
            KIND_NAMES[args.drop_kind],
            args.drop_ack,
            args.drop_count,
        )
    )

    reliable_retransmitted = any(
        kind in (KIND_NAMES["control"], KIND_NAMES["status"]) and count >= 2
        for (_, _, kind, _, _), count in source_signature.items()
    )
    if not reliable_retransmitted:
        failures.append("source capture has no repeated reliable frame for retransmission")

    failures.extend(
        require_log_patterns(
            args.guest_log,
            (
                # AxVisor can interleave a host log line between fragments of
                # one Guest serial record (for example RETRANS + MIT).
                r"TASK2_RETRANS[\s\S]{0,512}MIT\b",
                r"TASK2_DUPLICATE\b",
                r"TASK2_ACK\b",
            ),
        )
    )
    failures.extend(
        require_log_patterns(
            args.proxy_log,
            (r"PROXY_DROP .*kind=ack .*ack=", r"PROXY_SUMMARY dropped="),
        )
    )

    for report in (delivered, source):
        stats = report["stats"]
        print(
            f"{report['path']}: packets={stats['packets']} udp={stats['udp']} "
            f"task2={stats['task2_frames']} kinds={dict(report['task2_kinds'])}"
        )
    if failures:
        print("FAIL")
        print("\n".join(f"- {failure}" for failure in failures))
        return 1
    print("PASS: bounded ACK loss caused real retransmission and duplicate handling")
    return 0


if __name__ == "__main__":
    sys.exit(main())
