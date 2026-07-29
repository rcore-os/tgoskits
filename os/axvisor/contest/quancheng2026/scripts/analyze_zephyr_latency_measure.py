#!/usr/bin/env python3
"""Parse Zephyr latency_measure output into JSON and Markdown reports."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path


ANSI_RE = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")
METRIC_RE = re.compile(
    r"^(?P<metric>[A-Za-z0-9_.+]+)\s+-\s+"
    r"(?P<description>.*?):\s+"
    r"(?P<cycles>\d+)\s+cycles\s+,\s+"
    r"(?P<nanoseconds>\d+)\s+ns"
)

SELECTED_METRICS = [
    "thread.yield.preemptive.ctx.k_to_k",
    "thread.yield.cooperative.ctx.k_to_k",
    "isr.resume.interrupted.thread.kernel",
    "isr.resume.different.thread.kernel",
    "semaphore.take.blocking.k_to_k",
    "semaphore.give.wake+ctx.k_to_k",
    "events.wait.blocking.k_to_k",
    "events.set.wake+ctx.k_to_k",
    "mutex.lock.immediate.recursive.kernel",
    "mutex.unlock.immediate.recursive.kernel",
    "heap.malloc.immediate",
    "heap.free.immediate",
]


def strip_ansi(line: str) -> str:
    return ANSI_RE.sub("", line).strip()


def parse_log(path: Path) -> dict[str, object]:
    metrics: list[dict[str, object]] = []
    success_marker = False
    boot_line = ""

    with path.open("r", encoding="utf-8", errors="replace") as handle:
        for raw in handle:
            line = strip_ansi(raw)
            if "Booting Zephyr OS build" in line:
                boot_line = line
            if "PROJECT EXECUTION SUCCESSFUL" in line:
                success_marker = True

            match = METRIC_RE.search(line)
            if not match:
                continue
            metrics.append(
                {
                    "metric": match.group("metric"),
                    "description": match.group("description").strip(),
                    "cycles": int(match.group("cycles")),
                    "nanoseconds": int(match.group("nanoseconds")),
                }
            )

    by_metric = {str(item["metric"]): item for item in metrics}
    selected = [by_metric[name] for name in SELECTED_METRICS if name in by_metric]
    ns_values = [int(item["nanoseconds"]) for item in metrics]

    return {
        "run_log": str(path),
        "success_marker": success_marker,
        "boot_line": boot_line,
        "metric_count": len(metrics),
        "nanoseconds": {
            "min": min(ns_values) if ns_values else None,
            "max": max(ns_values) if ns_values else None,
            "mean": (sum(ns_values) / len(ns_values)) if ns_values else None,
        },
        "selected_metrics": selected,
        "metrics": metrics,
    }


def write_markdown(report: dict[str, object], path: Path) -> None:
    selected = report["selected_metrics"]  # type: ignore[index]
    metrics = report["metrics"]  # type: ignore[index]
    ns = report["nanoseconds"]  # type: ignore[index]

    lines = [
        "# Zephyr Native Latency Baseline Report",
        "",
        f"- Run log: `{report['run_log']}`",
        f"- Success marker: `{report['success_marker']}`",
        f"- Boot line: `{report.get('boot_line') or 'n/a'}`",
        f"- Metric count: `{report['metric_count']}`",
        f"- Minimum observed metric: `{ns['min']} ns`",
        f"- Mean across reported metrics: `{ns['mean']:.3f} ns`" if ns["mean"] is not None else "- Mean across reported metrics: `n/a`",
        f"- Maximum observed metric: `{ns['max']} ns`",
        "",
        "## Selected Realtime Metrics",
        "",
        "| Metric | Description | Cycles | Nanoseconds |",
        "|---|---|---:|---:|",
    ]
    for item in selected:
        lines.append(
            f"| `{item['metric']}` | {item['description']} | "
            f"{item['cycles']} | {item['nanoseconds']} |"
        )

    lines.extend(
        [
            "",
            "## All Metrics",
            "",
            "| Metric | Cycles | Nanoseconds |",
            "|---|---:|---:|",
        ]
    )
    for item in metrics:
        lines.append(f"| `{item['metric']}` | {item['cycles']} | {item['nanoseconds']} |")

    lines.extend(
        [
            "",
            "## Coverage Notes",
            "",
            "- This is the native Zephyr RTOS baseline on QEMU `qemu_cortex_a53`, outside AxVisor.",
            "- It covers kernel primitive latency, including context switch and ISR return paths.",
            "- It should be compared with the AxVisor-hosted RTOS guest and with periodic jitter data collected under the contest workload.",
            "",
        ]
    )
    path.write_text("\n".join(lines), encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run_log", type=Path)
    parser.add_argument("--json-out", type=Path)
    parser.add_argument("--md-out", type=Path)
    parser.add_argument("--fail-on-missing", action="store_true")
    args = parser.parse_args()

    run_log = args.run_log.resolve()
    if not run_log.exists():
        raise SystemExit(f"missing run log: {run_log}")

    report = parse_log(run_log)
    json_out = args.json_out or run_log.parent / "latency-summary.json"
    md_out = args.md_out or run_log.parent / "latency-report.md"
    json_out.write_text(json.dumps(report, indent=2, sort_keys=True), encoding="utf-8")
    write_markdown(report, md_out)

    analysis_result = "PASS" if report["success_marker"] and report["metric_count"] else "FAIL"
    print(f"analysis_result={analysis_result}")
    print(f"metric_count={report['metric_count']}")
    print(f"json_out={json_out}")
    print(f"md_out={md_out}")
    if args.fail_on_missing and analysis_result != "PASS":
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
