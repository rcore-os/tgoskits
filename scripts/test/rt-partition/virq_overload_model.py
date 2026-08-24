#!/usr/bin/env python3
"""Replay one overload schedule against old unbounded and current bounded vIRQ queues."""

from __future__ import annotations

import argparse
import csv
import re
import subprocess
from collections import deque
from pathlib import Path
from typing import NamedTuple


ROOT = Path(__file__).resolve().parents[3]
QUEUE_PATH = "virtualization/axvm/src/runtime/queue.rs"
CAPACITY_PATH = ROOT / "virtualization/axvm/src/runtime/mod.rs"
DEFAULT_BASELINE = "f298ee57b^"


class Delivery(NamedTuple):
    event_id: int
    arrival_us: int
    result: str
    service_us: int | None
    latency_us: int | None


class ReplayResult(NamedTuple):
    arrivals: int
    accepted: int
    overflow: int
    max_queue_depth: int
    drain_end_us: int
    deliveries: list[Delivery]
    depth_samples: list[tuple[int, int]]


def replay(
    *,
    capacity: int | None,
    arrival_interval_us: int,
    service_interval_us: int,
    duration_us: int,
) -> ReplayResult:
    if arrival_interval_us <= 0 or service_interval_us <= 0 or duration_us <= 0:
        raise ValueError("all replay timing values must be positive")
    if capacity is not None and capacity <= 0:
        raise ValueError("capacity must be positive or None")

    arrival_times = list(range(0, duration_us, arrival_interval_us))
    queue: deque[tuple[int, int]] = deque()
    deliveries: list[Delivery] = []
    depth_samples: list[tuple[int, int]] = [(0, 0)]
    next_service_us = service_interval_us
    max_depth = 0
    overflow = 0

    def service_until(limit_us: int) -> None:
        nonlocal next_service_us
        while next_service_us <= limit_us:
            if queue:
                event_id, arrival_us = queue.popleft()
                deliveries.append(
                    Delivery(
                        event_id,
                        arrival_us,
                        "delivered",
                        next_service_us,
                        next_service_us - arrival_us,
                    )
                )
            depth_samples.append((next_service_us, len(queue)))
            next_service_us += service_interval_us

    for event_id, arrival_us in enumerate(arrival_times):
        service_until(arrival_us)
        if capacity is not None and len(queue) >= capacity:
            overflow += 1
            deliveries.append(Delivery(event_id, arrival_us, "overflow", None, None))
        else:
            queue.append((event_id, arrival_us))
            max_depth = max(max_depth, len(queue))
        depth_samples.append((arrival_us, len(queue)))

    while queue:
        service_until(next_service_us)

    deliveries.sort(key=lambda delivery: delivery.event_id)
    accepted = len(arrival_times) - overflow
    drain_end_us = max(
        (delivery.service_us or 0 for delivery in deliveries),
        default=0,
    )
    return ReplayResult(
        arrivals=len(arrival_times),
        accepted=accepted,
        overflow=overflow,
        max_queue_depth=max_depth,
        drain_end_us=drain_end_us,
        deliveries=deliveries,
        depth_samples=depth_samples,
    )


def percentile(values: list[int], fraction: float) -> int:
    ordered = sorted(values)
    if not ordered:
        return 0
    index = min(len(ordered) - 1, max(0, int(len(ordered) * fraction + 0.999999) - 1))
    return ordered[index]


def load_source_contract(baseline_rev: str) -> tuple[str, str, int]:
    old_source = subprocess.run(
        ["git", "show", f"{baseline_rev}:{QUEUE_PATH}"],
        cwd=ROOT,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    ).stdout
    current_source = (ROOT / QUEUE_PATH).read_text()
    capacity_source = CAPACITY_PATH.read_text()
    match = re.search(r"VCPU_INTERRUPT_QUEUE_CAPACITY:\s*usize\s*=\s*(\d+)", capacity_source)
    if match is None:
        raise RuntimeError("cannot find current vIRQ queue capacity")
    capacity = int(match.group(1))
    if ".push(interrupt);" not in old_source or "VCPU_INTERRUPT_QUEUE_CAPACITY" in old_source:
        raise RuntimeError("baseline revision is not the expected unbounded queue")
    if "try_push" not in current_source or "VCPU_INTERRUPT_QUEUE_CAPACITY" not in current_source:
        raise RuntimeError("current source is not the expected bounded queue")
    return old_source, current_source, capacity


def write_plot(
    output: Path,
    unbounded: ReplayResult,
    bounded: ReplayResult,
) -> None:
    import matplotlib.pyplot as plt

    figure, axes = plt.subplots(1, 2, figsize=(11, 4.2))
    for label, result, color in [
        ("old unbounded", unbounded, "#b42318"),
        ("current bounded", bounded, "#175cd3"),
    ]:
        x = [time_us / 1000 for time_us, _ in result.depth_samples]
        y = [depth for _, depth in result.depth_samples]
        axes[0].plot(x, y, label=label, color=color, linewidth=1.4)
        latencies = sorted(
            delivery.latency_us
            for delivery in result.deliveries
            if delivery.latency_us is not None
        )
        axes[1].plot(
            range(1, len(latencies) + 1),
            [latency / 1000 for latency in latencies],
            label=label,
            color=color,
            linewidth=1.4,
        )
    axes[0].set_title("Queue depth during and after overload")
    axes[0].set_xlabel("replay time (ms)")
    axes[0].set_ylabel("pending edges")
    axes[1].set_title("Delivered-edge queue latency")
    axes[1].set_xlabel("accepted edge rank")
    axes[1].set_ylabel("latency (ms)")
    for axis in axes:
        axis.grid(True, alpha=0.25)
        axis.legend()
    figure.tight_layout()
    figure.savefig(output, dpi=160)
    plt.close(figure)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline-rev", default=DEFAULT_BASELINE)
    parser.add_argument("--arrival-us", type=int, default=50)
    parser.add_argument("--service-us", type=int, default=1000)
    parser.add_argument("--duration-us", type=int, default=100000)
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=ROOT / "results/task1/overload",
    )
    args = parser.parse_args()

    old_source, current_source, capacity = load_source_contract(args.baseline_rev)
    unbounded = replay(
        capacity=None,
        arrival_interval_us=args.arrival_us,
        service_interval_us=args.service_us,
        duration_us=args.duration_us,
    )
    bounded = replay(
        capacity=capacity,
        arrival_interval_us=args.arrival_us,
        service_interval_us=args.service_us,
        duration_us=args.duration_us,
    )

    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    (output / "unbounded-queue.rs").write_text(old_source)
    (output / "bounded-queue.rs").write_text(current_source)

    with (output / "events.csv").open("w", newline="") as stream:
        writer = csv.writer(stream)
        writer.writerow(["model", "event_id", "arrival_us", "result", "service_us", "latency_us"])
        for model, result in [("unbounded", unbounded), ("bounded", bounded)]:
            for delivery in result.deliveries:
                writer.writerow([model, *delivery])

    summaries = []
    for model, result in [("unbounded", unbounded), ("bounded", bounded)]:
        latencies = [
            delivery.latency_us
            for delivery in result.deliveries
            if delivery.latency_us is not None
        ]
        summaries.append(
            {
                "model": model,
                "arrivals": result.arrivals,
                "accepted": result.accepted,
                "overflow": result.overflow,
                "max_queue_depth": result.max_queue_depth,
                "p99_latency_us": percentile(latencies, 0.99),
                "max_latency_us": max(latencies, default=0),
                "drain_end_us": result.drain_end_us,
            }
        )
    with (output / "summary.csv").open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(summaries[0]))
        writer.writeheader()
        writer.writerows(summaries)

    write_plot(output / "comparison.png", unbounded, bounded)
    commit = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=ROOT,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    ).stdout.strip()
    (output / "meta.txt").write_text(
        f"git_commit={commit}\n"
        f"baseline_rev={args.baseline_rev}\n"
        f"queue_capacity={capacity}\n"
        f"arrival_interval_us={args.arrival_us}\n"
        f"service_interval_us={args.service_us}\n"
        f"arrival_duration_us={args.duration_us}\n"
        "method=deterministic source-contract replay\n"
        "claim_scope=queue growth and overflow semantics only; not QEMU or hardware WCET\n"
    )
    bounded_summary = summaries[1]
    unbounded_summary = summaries[0]
    (output / "README.md").write_text(
        "# T3.1 vIRQ Overload Replay\n\n"
        "This deterministic replay applies one arrival/service schedule to the old Git "
        "revision's unbounded queue contract and the current capacity-64 contract. It is "
        "a queue-bound proof, not an end-to-end QEMU latency measurement.\n\n"
        "| model | accepted | overflow | max depth | p99 latency (us) | max latency (us) |\n"
        "|---|---:|---:|---:|---:|---:|\n"
        f"| old unbounded | {unbounded_summary['accepted']} | {unbounded_summary['overflow']} | "
        f"{unbounded_summary['max_queue_depth']} | {unbounded_summary['p99_latency_us']} | "
        f"{unbounded_summary['max_latency_us']} |\n"
        f"| current bounded | {bounded_summary['accepted']} | {bounded_summary['overflow']} | "
        f"{bounded_summary['max_queue_depth']} | {bounded_summary['p99_latency_us']} | "
        f"{bounded_summary['max_latency_us']} |\n\n"
        "The bounded implementation turns excess load into an explicit error and keeps "
        "resident queue depth at 64. The unbounded implementation accepts every edge and "
        "lets backlog and drain time grow with overload duration.\n"
    )

    evidence = [
        "README.md",
        "events.csv",
        "summary.csv",
        "comparison.png",
        "meta.txt",
        "unbounded-queue.rs",
        "bounded-queue.rs",
    ]
    hashes = []
    import hashlib

    for name in evidence:
        digest = hashlib.sha256((output / name).read_bytes()).hexdigest()
        hashes.append(f"{digest}  {name}\n")
    (output / "sha256sums").write_text("".join(hashes))
    print(f"accepted overload replay: {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
