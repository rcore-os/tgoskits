#!/usr/bin/env python3
"""Summarize accepted and failed attempts from a P1 interleaved batch log."""

from __future__ import annotations

import argparse
import re
import sys
from collections import defaultdict
from pathlib import Path


START = re.compile(
    r"P1_ATTEMPT_START label=(baseline|modified)/(run-\d+) attempt=(\d+)"
)
ACCEPTED = re.compile(
    r"P1_ATTEMPT_ACCEPTED label=(baseline|modified)/(run-\d+) attempt=(\d+)"
)


def summarize(log: str) -> str:
    attempts: dict[tuple[str, str], list[int]] = defaultdict(list)
    accepted: dict[tuple[str, str], int] = {}
    for match in START.finditer(log):
        variant, run_id, attempt = match.groups()
        attempts[(variant, run_id)].append(int(attempt))
    for match in ACCEPTED.finditer(log):
        variant, run_id, attempt = match.groups()
        key = (variant, run_id)
        if key in accepted:
            raise ValueError(f"duplicate accepted marker for {variant}/{run_id}")
        accepted[key] = int(attempt)

    if not accepted:
        raise ValueError("batch log contains no accepted P1 runs")
    if set(attempts) != set(accepted):
        incomplete = sorted(set(attempts) - set(accepted))
        raise ValueError(f"batch log contains incomplete runs: {incomplete}")

    lines: list[str] = []
    total_runs = 0
    total_attempts = 0
    for variant in ("baseline", "modified"):
        keys = sorted(key for key in attempts if key[0] == variant)
        variant_runs = len(keys)
        variant_attempts = sum(len(attempts[key]) for key in keys)
        total_runs += variant_runs
        total_attempts += variant_attempts
        lines.extend(
            (
                f"{variant}_accepted_runs={variant_runs}",
                f"{variant}_total_attempts={variant_attempts}",
                f"{variant}_failed_attempts={variant_attempts - variant_runs}",
                f"{variant}_attempt_acceptance_rate={variant_runs / variant_attempts:.6f}",
            )
        )
        for key in keys:
            lines.append(f"{variant}_{key[1]}_accepted_attempt={accepted[key]}")
    lines.extend(
        (
            f"total_accepted_runs={total_runs}",
            f"total_attempts={total_attempts}",
            f"total_failed_attempts={total_attempts - total_runs}",
            f"total_attempt_acceptance_rate={total_runs / total_attempts:.6f}",
        )
    )
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("log", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    try:
        summary = summarize(args.log.read_text(errors="replace"))
        if args.output is None:
            sys.stdout.write(summary)
        else:
            args.output.write_text(summary)
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    return 0



if __name__ == "__main__":
    raise SystemExit(main())
