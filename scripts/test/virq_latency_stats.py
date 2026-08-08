#!/usr/bin/env python3
"""Summarize one OpenRace software-vIRQ serial log."""

from __future__ import annotations

import argparse
import json
import re
import statistics
from pathlib import Path


INJECT_RE = re.compile(
    r"VIRQ_INJECT sequence=(?P<sequence>\d+) .*?vector=(?P<vector>\d+) "
    r".*?requested_ns=(?P<requested>\d+) "
    r"completed_ns=(?P<completed>\d+) status=(?P<status>\w+)"
)
GUEST_RE = re.compile(
    r"^(?:(?P<vector>\d+),)?(?P<sequence>\d+),(?P<timestamp>\d+)$"
)
TRACE_OVERFLOW_RE = re.compile(
    r"VIRQ_TRACE seq=(?P<sequence>\d+) .*event=queue_overflow"
)
ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")


def percentile(values: list[int], fraction: float) -> int:
    if not values:
        return 0
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, int(fraction * len(ordered) + 0.999999) - 1))
    return ordered[index]


def match_guest_samples(
    injections: dict[tuple[int, int], tuple[int, int, str]],
    guests: dict[tuple[int, int], int],
) -> list[tuple[tuple[int, int], int]]:
    """Match guest ISR timestamps to the latest preceding host request.

    The guest sequence counts received interrupts, so it no longer identifies
    the corresponding host sequence after a dropped interrupt. Monotonic host
    and guest timestamps let us skip the missing request without shifting every
    later sample by one or more periods.
    """
    matched: list[tuple[tuple[int, int], int]] = []
    vectors = {vector for vector, _ in injections} | {vector for vector, _ in guests}
    for vector in sorted(vectors):
        successful = [
            ((vector, sequence), requested)
            for (injection_vector, sequence), (requested, _, status) in sorted(
                injections.items()
            )
            if injection_vector == vector and status == "ok"
        ]
        injection_index = 0
        vector_guests = sorted(
            (key, timestamp)
            for key, timestamp in guests.items()
            if key[0] == vector
        )
        for _, guest_timestamp in vector_guests:
            if injection_index >= len(successful):
                continue
            while (
                injection_index + 1 < len(successful)
                and successful[injection_index + 1][1] <= guest_timestamp
            ):
                injection_index += 1
            if successful[injection_index][1] > guest_timestamp:
                continue
            key, requested = successful[injection_index]
            matched.append((key, guest_timestamp - requested))
            injection_index += 1
    return matched


def summarize(path: Path, period_ns: int) -> dict[str, int | float]:
    injections: dict[tuple[int, int], tuple[int, int, str]] = {}
    guests: dict[tuple[int, int], int] = {}
    overflow_sequences: set[int] = set()
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = ANSI_RE.sub("", line)
        if match := INJECT_RE.search(line):
            sequence = int(match["sequence"])
            vector = int(match["vector"])
            injections[(vector, sequence)] = (
                int(match["requested"]),
                int(match["completed"]),
                match["status"],
            )
            continue
        if match := TRACE_OVERFLOW_RE.search(line):
            overflow_sequences.add(int(match["sequence"]))
            continue
        if match := GUEST_RE.match(line.strip()):
            vector = int(match["vector"] or 48)
            guests[(vector, int(match["sequence"]))] = int(match["timestamp"])

    matched_samples = match_guest_samples(injections, guests)
    response_ns = [latency for _, latency in matched_samples]
    overrun_ns = [max(0, value - period_ns) for value in response_ns]
    matched = len(response_ns)
    successful_injections = sum(status == "ok" for _, _, status in injections.values())
    lost_irq = max(0, successful_injections - matched)
    errors = sum(status != "ok" for _, _, status in injections.values())
    return {
        "injected": len(injections),
        "guest_received": len(guests),
        "matched": matched,
        "lost_irq": lost_irq,
        "inject_errors": errors,
        "queue_overflow": len(overflow_sequences),
        "mean_ns": round(statistics.mean(response_ns)) if response_ns else 0,
        "p99_ns": percentile(response_ns, 0.99),
        "p99_9_ns": percentile(response_ns, 0.999),
        "max_ns": max(response_ns, default=0),
        "overrun_max_ns": max(overrun_ns, default=0),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("log", type=Path)
    parser.add_argument("--period-ns", type=int, default=2_000_000)
    args = parser.parse_args()
    result = summarize(args.log, args.period_ns)
    print(json.dumps(result, sort_keys=True))
    print(
        "p99={p99_ns}ns p99.9={p99_9_ns}ns max={max_ns}ns "
        "overrun_max={overrun_max_ns}ns lost={lost_irq} overflow={queue_overflow}".format(
            **result
        )
    )


if __name__ == "__main__":
    main()
