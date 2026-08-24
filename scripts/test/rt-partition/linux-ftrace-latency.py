#!/usr/bin/env python3
"""Summarize the Linux cyclictest wakeup path from an ftrace event dump."""

from __future__ import annotations

import argparse
import csv
import math
import re
import sys
from dataclasses import dataclass
from decimal import Decimal
from pathlib import Path


ANSI_ESCAPE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
TRACE_EVENT = re.compile(
    r"(?P<task>.+?)\s+\[(?P<cpu>\d+)\]\s+\S+\s+"
    r"(?P<timestamp>\d+\.\d+):\s+"
    r"(?P<event>[a-zA-Z0-9_]+):\s*(?P<details>.*)$"
)


@dataclass(frozen=True)
class TraceEvent:
    cpu: int
    current_pid: int | None
    timestamp_ns: int
    name: str
    details: str


@dataclass
class WakeupSample:
    cpu: int
    pid: int
    irq_entry_ns: int
    hrtimer_entry_ns: int
    sched_wakeup_ns: int
    hrtimer_exit_ns: int | None = None
    irq_exit_ns: int | None = None
    sched_switch_ns: int | None = None


def parse_timestamp_ns(value: str) -> int:
    return int(Decimal(value) * Decimal(1_000_000_000))


def parse_events(text: str) -> list[TraceEvent]:
    events = []
    for raw_line in text.splitlines():
        line = ANSI_ESCAPE.sub("", raw_line)
        match = TRACE_EVENT.search(line)
        if match is None:
            continue
        events.append(
            TraceEvent(
                cpu=int(match.group("cpu")),
                current_pid=(
                    int(pid_match.group(1))
                    if (pid_match := re.search(r"-(\d+)\s*$", match.group("task")))
                    else None
                ),
                timestamp_ns=parse_timestamp_ns(match.group("timestamp")),
                name=match.group("event"),
                details=match.group("details"),
            )
        )
    return events


def detail_int(details: str, name: str) -> int | None:
    match = re.search(rf"(?:^|\s){re.escape(name)}=(-?\d+)(?:\s|$)", details)
    return None if match is None else int(match.group(1))


def detail_text(details: str, name: str) -> str | None:
    match = re.search(rf"(?:^|\s){re.escape(name)}=([^\s]+)", details)
    return None if match is None else match.group(1)


def collect_samples(
    events: list[TraceEvent], target_comm: str, target_kernel_prio: int
) -> tuple[list[WakeupSample], int, int]:
    irq_entry_by_cpu: dict[int, int] = {}
    hrtimer_entry_by_cpu: dict[int, int] = {}
    irq_samples_by_cpu: dict[int, list[WakeupSample]] = {}
    callback_sample_by_cpu: dict[int, WakeupSample] = {}
    pending_by_pid: dict[int, WakeupSample] = {}
    samples: list[WakeupSample] = []
    target_wakeups = 0
    self_wakeups_skipped = 0

    for event in events:
        if event.name == "irq_handler_entry":
            if detail_text(event.details, "name") == "arch_timer":
                irq_entry_by_cpu[event.cpu] = event.timestamp_ns
                irq_samples_by_cpu[event.cpu] = []
            continue

        if event.name == "hrtimer_expire_entry":
            if detail_text(event.details, "function") == "hrtimer_wakeup":
                hrtimer_entry_by_cpu[event.cpu] = event.timestamp_ns
            else:
                hrtimer_entry_by_cpu.pop(event.cpu, None)
            continue

        if event.name == "sched_wakeup":
            comm = detail_text(event.details, "comm")
            priority = detail_int(event.details, "prio")
            if comm != target_comm or priority != target_kernel_prio:
                continue
            target_wakeups += 1
            pid = detail_int(event.details, "pid")
            if pid is not None and event.current_pid == pid:
                self_wakeups_skipped += 1
                continue
            irq_entry_ns = irq_entry_by_cpu.get(event.cpu)
            hrtimer_entry_ns = hrtimer_entry_by_cpu.get(event.cpu)
            if pid is None or irq_entry_ns is None or hrtimer_entry_ns is None:
                continue
            if not irq_entry_ns <= hrtimer_entry_ns <= event.timestamp_ns:
                continue
            sample = WakeupSample(
                cpu=event.cpu,
                pid=pid,
                irq_entry_ns=irq_entry_ns,
                hrtimer_entry_ns=hrtimer_entry_ns,
                sched_wakeup_ns=event.timestamp_ns,
            )
            samples.append(sample)
            callback_sample_by_cpu[event.cpu] = sample
            irq_samples_by_cpu.setdefault(event.cpu, []).append(sample)
            pending_by_pid[pid] = sample
            continue

        if event.name == "hrtimer_expire_exit":
            sample = callback_sample_by_cpu.pop(event.cpu, None)
            if sample is not None and event.timestamp_ns >= sample.sched_wakeup_ns:
                sample.hrtimer_exit_ns = event.timestamp_ns
            hrtimer_entry_by_cpu.pop(event.cpu, None)
            continue

        if event.name == "irq_handler_exit":
            for sample in irq_samples_by_cpu.pop(event.cpu, []):
                if event.timestamp_ns >= sample.sched_wakeup_ns:
                    sample.irq_exit_ns = event.timestamp_ns
            irq_entry_by_cpu.pop(event.cpu, None)
            hrtimer_entry_by_cpu.pop(event.cpu, None)
            callback_sample_by_cpu.pop(event.cpu, None)
            continue

        if event.name == "sched_switch":
            next_pid = detail_int(event.details, "next_pid")
            if next_pid is None:
                continue
            sample = pending_by_pid.pop(next_pid, None)
            if sample is None:
                continue
            if event.timestamp_ns >= sample.sched_wakeup_ns:
                sample.sched_switch_ns = event.timestamp_ns

    return samples, target_wakeups, self_wakeups_skipped


def nearest_rank(values: list[int], percentile: float) -> int:
    ordered = sorted(values)
    rank = max(1, math.ceil(percentile * len(ordered)))
    return ordered[rank - 1]


def sample_metrics(sample: WakeupSample) -> dict[str, int] | None:
    if sample.sched_switch_ns is None:
        return None
    metrics = {
        "irq_to_hrtimer_ns": sample.hrtimer_entry_ns - sample.irq_entry_ns,
        "hrtimer_to_wakeup_ns": sample.sched_wakeup_ns - sample.hrtimer_entry_ns,
        "wakeup_to_switch_ns": sample.sched_switch_ns - sample.sched_wakeup_ns,
        "irq_to_switch_ns": sample.sched_switch_ns - sample.irq_entry_ns,
    }
    if sample.hrtimer_exit_ns is not None:
        metrics["hrtimer_callback_ns"] = (
            sample.hrtimer_exit_ns - sample.hrtimer_entry_ns
        )
    if sample.irq_exit_ns is not None:
        metrics["irq_handler_ns"] = sample.irq_exit_ns - sample.irq_entry_ns
    if any(value < 0 for value in metrics.values()):
        return None
    return metrics


def build_summary(
    events: list[TraceEvent],
    samples: list[WakeupSample],
    target_wakeups: int,
    self_wakeups_skipped: int,
) -> str:
    complete = [(sample, sample_metrics(sample)) for sample in samples]
    complete = [(sample, metrics) for sample, metrics in complete if metrics is not None]
    if not complete:
        raise ValueError("no complete cyclictest wakeup chains were found")

    lines = [
        f"parsed_events={len(events)}",
        f"target_sched_wakeups={target_wakeups}",
        f"self_wakeups_skipped={self_wakeups_skipped}",
        f"matched_hrtimer_wakeups={len(samples)}",
        f"complete_wakeup_chains={len(complete)}",
        f"incomplete_wakeup_chains={len(samples) - len(complete)}",
    ]
    metric_names = (
        "irq_to_hrtimer_ns",
        "hrtimer_to_wakeup_ns",
        "wakeup_to_switch_ns",
        "irq_to_switch_ns",
        "hrtimer_callback_ns",
        "irq_handler_ns",
    )
    for name in metric_names:
        values = [metrics[name] for _, metrics in complete if name in metrics]
        if not values:
            continue
        lines.extend(
            (
                f"{name}_samples={len(values)}",
                f"{name}_p50={nearest_rank(values, 0.50)}",
                f"{name}_p90={nearest_rank(values, 0.90)}",
                f"{name}_p99={nearest_rank(values, 0.99)}",
                f"{name}_p99_9={nearest_rank(values, 0.999)}",
                f"{name}_max={max(values)}",
            )
        )
    return "\n".join(lines) + "\n"


def write_csv(path: Path, samples: list[WakeupSample]) -> None:
    fieldnames = [
        "cpu",
        "pid",
        "irq_entry_ns",
        "hrtimer_entry_ns",
        "sched_wakeup_ns",
        "hrtimer_exit_ns",
        "irq_exit_ns",
        "sched_switch_ns",
        "irq_to_hrtimer_ns",
        "hrtimer_to_wakeup_ns",
        "wakeup_to_switch_ns",
        "irq_to_switch_ns",
        "hrtimer_callback_ns",
        "irq_handler_ns",
    ]
    with path.open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=fieldnames)
        writer.writeheader()
        for sample in samples:
            metrics = sample_metrics(sample)
            if metrics is None:
                continue
            writer.writerow(
                {
                    "cpu": sample.cpu,
                    "pid": sample.pid,
                    "irq_entry_ns": sample.irq_entry_ns,
                    "hrtimer_entry_ns": sample.hrtimer_entry_ns,
                    "sched_wakeup_ns": sample.sched_wakeup_ns,
                    "hrtimer_exit_ns": sample.hrtimer_exit_ns or "",
                    "irq_exit_ns": sample.irq_exit_ns or "",
                    "sched_switch_ns": sample.sched_switch_ns,
                    **metrics,
                }
            )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trace", type=Path)
    parser.add_argument("summary", type=Path)
    parser.add_argument("--csv", type=Path)
    parser.add_argument("--comm", default="cyclictest")
    parser.add_argument("--kernel-prio", type=int, default=9)
    args = parser.parse_args()

    try:
        events = parse_events(args.trace.read_text(errors="replace"))
        samples, target_wakeups, self_wakeups_skipped = collect_samples(
            events, args.comm, args.kernel_prio
        )
        summary = build_summary(
            events, samples, target_wakeups, self_wakeups_skipped
        )
        args.summary.write_text(summary)
        if args.csv is not None:
            write_csv(args.csv, samples)
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
