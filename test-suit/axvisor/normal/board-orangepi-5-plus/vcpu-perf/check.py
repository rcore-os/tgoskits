#!/usr/bin/env python3
"""Reject incomplete workloads before comparing throughput to a frozen baseline."""
import argparse
import math
import re
import statistics
from pathlib import Path
import tomllib


def evaluate(text, config, measure=False):
    baseline = config["baseline_blocks_per_second"]
    limit = config["max_regression_percent"]
    timer_floor = config["min_timer_wakes_per_second"]
    if not all(
        isinstance(x, (int, float)) and not isinstance(x, bool) and math.isfinite(x)
        for x in (baseline, limit, timer_floor)
    ):
        raise ValueError("performance configuration must contain finite numbers")
    if baseline < 0 or not 0 < limit < 100 or timer_floor <= 0:
        raise ValueError("invalid baseline or regression/load budget")
    text = re.sub(r"\x1b\[[0-9;]*[A-Za-z]", "", text)
    text = re.sub(r"(?m)^\[VM 1\] ", "", text)
    pattern = (
        r"(?m)^VCPU_PERF_SAMPLE index=(\d+) blocks=(\d+) elapsed_ns=(\d+) "
        r"timer_wakes=(\d+) checksum=(\d+)\s*$"
    )
    rows = [tuple(map(int, match)) for match in re.findall(pattern, text)]
    if "VCPU_PERF_LOAD_READY cpu=0" not in text:
        raise ValueError("host background load did not start")
    if (
        "VCPU_PERF_LOAD_STOPPED" in text
        and text.index("VCPU_PERF_LOAD_STOPPED") < text.rfind("VCPU_PERF_SAMPLE")
    ):
        raise ValueError("host background load stopped before measurement completed")
    if [r[0] for r in rows] != list(range(6)):
        raise ValueError("missing, duplicated, or reordered performance samples")
    completed = list(re.finditer(r"(?m)^VCPU_PERF_DONE windows=5\s*$", text))
    if len(completed) != 1 or completed[0].start() < text.rfind("VCPU_PERF_SAMPLE"):
        raise ValueError("guest did not finish")
    if "VCPU_PERF_FAIL" in text or re.search(r"(?i)\bpanic(?:ked)?\b", text):
        raise ValueError("guest/host failure")
    scores = []
    for index, blocks, ns, wakes, checksum in rows[1:]:
        if blocks == 0 or checksum == 0 or not 3_000_000_000 <= ns <= 6_000_000_000:
            raise ValueError("invalid work or measurement duration")
        wake_rate = wakes / (ns / 1e9)
        if wake_rate < timer_floor:
            raise ValueError(
                f"timer load missing or stalled: sample={index} "
                f"wakes_per_second={wake_rate:.2f} minimum={timer_floor}"
            )
        scores.append(blocks * 1e9 / ns)
    score = statistics.median(scores)
    if not measure and baseline <= 0:
        raise ValueError("performance baseline has not been qualified")
    threshold = baseline * (1 - limit / 100)
    print(
        f"VCPU_PERF_RESULT blocks_per_second={score:.2f} baseline={baseline:.2f} "
        f"threshold={threshold:.2f} samples={scores}"
    )
    if not measure and score < threshold:
        raise ValueError("throughput regressed beyond configured budget")
    return score


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("log", type=Path)
    parser.add_argument("baseline", type=Path)
    parser.add_argument(
        "--measure", action="store_true", help="collect data only; never emit CI PASS"
    )
    args = parser.parse_args()
    try:
        evaluate(
            args.log.read_text(errors="replace"),
            tomllib.loads(args.baseline.read_text()),
            args.measure,
        )
    except (ValueError, KeyError, OSError) as error:
        print(f"VCPU_PERF_FAIL {error}")
        raise SystemExit(1)
    print("VCPU_PERF_MEASURED" if args.measure else "VCPU_PERF_PASS")
