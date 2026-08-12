#!/usr/bin/env python3
"""Verify a real-wire out-of-order or invalid-parameter injection run."""

from __future__ import annotations

import argparse
from pathlib import Path
import re
import sys

from verify_pcap import analyze


INJECTION_KINDS = {
    "out-of-order": (1, 99),
    "invalid-parameter": (1, 2),
}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("injected_side_pcap", type=Path)
    parser.add_argument("--mode", choices=tuple(INJECTION_KINDS), required=True)
    parser.add_argument("--guest-log", type=Path, required=True)
    parser.add_argument("--proxy-log", type=Path, required=True)
    args = parser.parse_args()

    try:
        report = analyze(args.injected_side_pcap, None)
        guest_log = args.guest_log.read_text(encoding="utf-8", errors="replace")
        proxy_log = args.proxy_log.read_text(encoding="utf-8", errors="replace")
    except (OSError, ValueError) as error:
        print(f"FAIL: {error}")
        return 1

    kind, sequence = INJECTION_KINDS[args.mode]
    signature = report["task2_signature"]
    failures: list[str] = []
    if signature[("10.0.42.15", "10.0.42.2", kind, sequence, 0)] < 1:
        failures.append(
            f"injected frame missing from {args.injected_side_pcap}: "
            f"kind={kind} sequence={sequence}"
        )
    if not re.search(rf"PROXY_INJECT mode={re.escape(args.mode)}\b", proxy_log):
        failures.append(f"proxy log does not prove {args.mode} injection")
    if args.mode == "out-of-order":
        required = (r"TASK2_PROTOCOL_ERROR out_of_order=99", r"TASK2_REMOTE_ERROR code=OutOfOrder")
    else:
        required = (r"TASK2_PROTOCOL_ERROR invalid_payload=", r"TASK2_REMOTE_ERROR code=InvalidParameter")
    failures.extend(
        f"guest log missing {pattern!r}"
        for pattern in required
        if not re.search(pattern, guest_log)
    )

    stats = report["stats"]
    print(
        f"{args.injected_side_pcap}: packets={stats['packets']} udp={stats['udp']} "
        f"task2={stats['task2_frames']} kinds={dict(report['task2_kinds'])}"
    )
    if failures:
        print("FAIL")
        print("\n".join(f"- {failure}" for failure in failures))
        return 1
    print(f"PASS: {args.mode} was injected on the QEMU wire and rejected")
    return 0


if __name__ == "__main__":
    sys.exit(main())
