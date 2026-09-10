#!/usr/bin/env python3
"""Aggregate GIPC client metrics from a multi-request guest log."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


METRIC = re.compile(
    r"GIPC_STARRY_METRIC .*?requests=(?P<requests>\d+) success=(?P<success>\d+) .*?"
    r"errors=(?P<errors>\d+) timeouts=(?P<timeouts>\d+) attempts=(?P<attempts>\d+) "
    r"reconnects=(?P<reconnects>\d+) recovery=(?P<recovery>\d+) rtt_ns=(?P<rtt>\d+) throughput_bps=(?P<throughput>\d+)"
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
    rtts = [int(sample["rtt"]) for sample in samples if int(sample["rtt"]) > 0]
    successes = sum(int(sample["success"]) for sample in samples)
    requests = sum(int(sample["requests"]) for sample in samples)
    errors = sum(int(sample["errors"]) for sample in samples)
    timeouts = sum(int(sample["timeouts"]) for sample in samples)
    reconnects = sum(int(sample["reconnects"]) for sample in samples)
    recoveries = sum(int(sample["recovery"]) for sample in samples)
    throughputs = [int(sample["throughput"]) for sample in samples]
    total = requests
    print(
        "GIPC_AGGREGATE "
        f"requests={total} success={successes} success_rate={successes / total:.6f} "
        f"app_errors={errors} timeouts={timeouts} reconnects={reconnects} recoveries={recoveries} "
        f"rtt_p50_ns={percentile(rtts, 0.50) if rtts else 0} "
        f"rtt_p95_ns={percentile(rtts, 0.95) if rtts else 0} "
        f"throughput_avg_bps={sum(throughputs) // len(throughputs)}"
    )
    return 0 if successes == total and errors == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
