#!/usr/bin/env python3
"""Aggregate GIPC client metrics from a multi-request guest log."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


METRIC = re.compile(
    r"GIPC_STARRY_METRIC .*?success=(?P<success>\d+) .*?"
    r"timeouts=(?P<timeouts>\d+) rtt_ns=(?P<rtt>\d+) throughput_bps=(?P<throughput>\d+)"
)


def percentile(values: list[int], rank: float) -> int:
    values = sorted(values)
    index = min(len(values) - 1, int((len(values) - 1) * rank))
    return values[index]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("log", type=Path)
    args = parser.parse_args()
    samples = [match.groupdict() for match in METRIC.finditer(args.log.read_text(encoding="utf-8"))]
    if not samples:
        print("no GIPC metrics found", file=sys.stderr)
        return 1
    rtts = [int(sample["rtt"]) for sample in samples]
    successes = sum(int(sample["success"]) == 1 for sample in samples)
    timeouts = sum(int(sample["timeouts"]) for sample in samples)
    throughputs = [int(sample["throughput"]) for sample in samples]
    total = len(samples)
    print(
        "GIPC_AGGREGATE "
        f"requests={total} success={successes} success_rate={successes / total:.6f} "
        f"timeouts={timeouts} rtt_p50_ns={percentile(rtts, 0.50)} "
        f"rtt_p95_ns={percentile(rtts, 0.95)} throughput_avg_bps={sum(throughputs) // total}"
    )
    return 0 if successes == total else 1


if __name__ == "__main__":
    raise SystemExit(main())
