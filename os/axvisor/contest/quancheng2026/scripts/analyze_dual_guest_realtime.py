#!/usr/bin/env python3
"""Build a compact latency/reliability report from dual-guest evidence."""

from __future__ import annotations

import argparse
import json
import math
import re
from pathlib import Path
from typing import Iterable


ANSI_RE = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")
KEY_VALUE_RE = re.compile(r"(QC_[A-Z0-9_]+)=([^\r\n]*)")
UDP_RE = re.compile(r"QC_UDP_SEQUENCE=(\d+).*?STATUS=([A-Z]+).*?RTT_US=(\d+)")
QCZ1_RE = re.compile(
    r"QC_QCZ1_RELIABLE_ACK seq=(\d+).*?attempts=(\d+).*?result=([A-Z]+)"
    r".*?status=(\d+).*?duplicate=(\d+).*?latency_us=(\d+)"
)
DUP_RE = re.compile(
    r"QC_QCZ1_DUPLICATE_ACK seq=(\d+).*?result=([A-Z]+).*?status=(\d+)"
    r".*?duplicate=(\d+).*?latency_us=(\d+)"
)
AI_RE = re.compile(
    r"QC_AI(?:_SEQ=|.*?\bSEQ=)(\d+).*?infer_us=(\d+).*?e2e_us=(\d+)"
    r".*?output_milli=(-?\d+).*?result=([A-Z]+)"
)
TCPDUMP_SUMMARY_RE = re.compile(r"(\d+) packets (captured|received by filter|dropped by kernel)")


def strip_ansi(line: str) -> str:
    return ANSI_RE.sub("", line)


def percentile(values: list[int], percent: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    if len(ordered) == 1:
        return float(ordered[0])
    rank = (len(ordered) - 1) * (percent / 100.0)
    low = math.floor(rank)
    high = math.ceil(rank)
    if low == high:
        return float(ordered[low])
    weight = rank - low
    return ordered[low] * (1.0 - weight) + ordered[high] * weight


def stats(values: Iterable[int]) -> dict[str, float | int | None]:
    vals = list(values)
    if not vals:
        return {
            "count": 0,
            "min": None,
            "mean": None,
            "p50": None,
            "p95": None,
            "p99": None,
            "max": None,
        }
    return {
        "count": len(vals),
        "min": min(vals),
        "mean": sum(vals) / len(vals),
        "p50": percentile(vals, 50),
        "p95": percentile(vals, 95),
        "p99": percentile(vals, 99),
        "max": max(vals),
    }


def metric_float(metrics: dict[str, str], key: str) -> float | None:
    value = metrics.get(key)
    if value is None:
        return None
    try:
        return float(value)
    except ValueError:
        return None


def apply_summary_stats(
    parsed_stats: dict[str, float | int | None],
    metrics: dict[str, str],
    count: int,
    *,
    min_key: str | None = None,
    mean_key: str | None = None,
    max_key: str | None = None,
) -> dict[str, float | int | None]:
    """Use final summary fields when serial log interleaving hides samples."""
    merged = dict(parsed_stats)
    if count:
        merged["count"] = count
    for field, key in (("min", min_key), ("mean", mean_key), ("max", max_key)):
        if key is None:
            continue
        value = metric_float(metrics, key)
        if value is not None:
            merged[field] = value
    return merged


def fmt_us(value: float | int | None) -> str:
    if value is None:
        return "n/a"
    return f"{value:.3f} us"


def fmt_tps(value: float | int | None) -> str:
    if value is None:
        return "n/a"
    return f"{value:.2f} tx/s"


def serial_throughput(latency_stats: dict[str, dict[str, float | int | None]]) -> dict[str, dict[str, float | int | None]]:
    """Estimate serialized application throughput from observed request latency."""
    result: dict[str, dict[str, float | int | None]] = {}
    for key in ("plain_udp_rtt", "qcz1_ack", "ai_end_to_end"):
        item = latency_stats.get(key, {})
        count = item.get("count")
        mean_us = item.get("mean")
        if not count or mean_us is None:
            result[key] = {
                "successful_transactions": count or 0,
                "active_seconds": None,
                "transactions_per_second": None,
            }
            continue
        active_seconds = float(mean_us) * int(count) / 1_000_000.0
        result[key] = {
            "successful_transactions": int(count),
            "active_seconds": active_seconds,
            "transactions_per_second": int(count) / active_seconds if active_seconds > 0 else None,
        }
    return result


def parse_qemu_log(path: Path) -> dict[str, object]:
    metrics: dict[str, str] = {}
    udp_rtt: list[int] = []
    qcz1_latency: list[int] = []
    dup_latency: list[int] = []
    ai_infer: list[int] = []
    ai_e2e: list[int] = []
    udp_pass = 0
    udp_fail = 0
    qcz1_ack = 0
    qcz1_fail = 0
    ai_pass = 0
    ai_fail = 0
    markers: set[str] = set()
    final_result = "UNKNOWN"

    with path.open("r", encoding="utf-8", errors="replace") as handle:
        for raw in handle:
            line = strip_ansi(raw)
            for key, value in KEY_VALUE_RE.findall(line):
                metrics[key] = value.strip()
                if value.strip() == "PASS":
                    markers.add(f"{key}=PASS")

            if line.startswith("result="):
                final_result = line.strip().split("=", 1)[1]

            udp = UDP_RE.search(line)
            if udp:
                status = udp.group(2)
                if status == "PASS":
                    udp_pass += 1
                    udp_rtt.append(int(udp.group(3)))
                else:
                    udp_fail += 1

            qcz1 = QCZ1_RE.search(line)
            if qcz1:
                if qcz1.group(3) == "ACK" and qcz1.group(4) == "0":
                    qcz1_ack += 1
                    qcz1_latency.append(int(qcz1.group(6)))
                else:
                    qcz1_fail += 1

            dup = DUP_RE.search(line)
            if dup:
                dup_latency.append(int(dup.group(5)))

            ai = AI_RE.search(line)
            if ai:
                if ai.group(5) == "PASS":
                    ai_pass += 1
                    ai_infer.append(int(ai.group(2)))
                    ai_e2e.append(int(ai.group(3)))
                else:
                    ai_fail += 1

    def metric_int(key: str, fallback: int) -> int:
        value = metrics.get(key)
        if value is None:
            return fallback
        try:
            return int(value)
        except ValueError:
            return fallback

    # Serial console output can interleave debug text into per-request marker
    # lines. Prefer the final summary counters and summary latency fields when
    # they are present.
    counts = {
        "udp_pass": metric_int("QC_UDP_SUCCESSES", udp_pass),
        "udp_fail": metric_int("QC_UDP_FAILURES", udp_fail),
        "qcz1_ack": metric_int("QC_QCZ1_RELIABLE_SUCCESSES", qcz1_ack),
        "qcz1_fail": metric_int("QC_QCZ1_RELIABLE_FAILURES", qcz1_fail),
        "ai_pass": metric_int("QC_AI_SUCCESSES", ai_pass),
        "ai_fail": metric_int("QC_AI_FAILURES", ai_fail),
        "duplicate_ack_samples": metric_int("QC_QCZ1_DUPLICATE_ACKS", len(dup_latency)),
    }
    return {
        "metrics": metrics,
        "final_result": final_result,
        "markers": sorted(markers),
        "counts": counts,
        "latency_us": {
            "plain_udp_rtt": apply_summary_stats(
                stats(udp_rtt),
                metrics,
                counts["udp_pass"],
                min_key="QC_UDP_RTT_MIN_US",
                mean_key="QC_UDP_RTT_MEAN_US",
                max_key="QC_UDP_RTT_MAX_US",
            ),
            "qcz1_ack": apply_summary_stats(
                stats(qcz1_latency),
                metrics,
                counts["qcz1_ack"],
                min_key="QC_QCZ1_LATENCY_MIN_US",
                mean_key="QC_QCZ1_LATENCY_MEAN_US",
                max_key="QC_QCZ1_LATENCY_MAX_US",
            ),
            "qcz1_duplicate_ack": stats(dup_latency),
            "ai_infer": apply_summary_stats(
                stats(ai_infer),
                metrics,
                counts["ai_pass"],
                mean_key="QC_AI_INFER_MEAN_US",
            ),
            "ai_end_to_end": apply_summary_stats(
                stats(ai_e2e),
                metrics,
                counts["ai_pass"],
                mean_key="QC_AI_E2E_MEAN_US",
                max_key="QC_AI_E2E_MAX_US",
            ),
        },
    }


def parse_tcpdump(path: Path) -> dict[str, int | None]:
    summary = {
        "packets_captured": None,
        "packets_received_by_filter": None,
        "packets_dropped_by_kernel": None,
    }
    if not path.exists():
        return summary

    with path.open("r", encoding="utf-8", errors="replace") as handle:
        for line in handle:
            match = TCPDUMP_SUMMARY_RE.search(line)
            if not match:
                continue
            count = int(match.group(1))
            label = match.group(2).replace(" ", "_")
            summary[f"packets_{label}"] = count
    return summary


def parse_bridge_info(path: Path) -> dict[str, str]:
    info = {
        "net_mode": "n/a",
        "bridge": "n/a",
        "tap_linux": "n/a",
        "tap_rtos": "n/a",
    }
    if not path.exists():
        return info

    with path.open("r", encoding="utf-8", errors="replace") as handle:
        for raw in handle:
            if "=" not in raw:
                continue
            key, value = raw.strip().split("=", 1)
            if key in info:
                info[key] = value
    return info


def read_final_result(evidence_dir: Path, fallback: str) -> str:
    for name in ("runner.log", "summary.txt", "qemu.log"):
        path = evidence_dir / name
        if not path.exists():
            continue
        with path.open("r", encoding="utf-8", errors="replace") as handle:
            for raw in handle:
                line = strip_ansi(raw).strip()
                if line.startswith("result="):
                    return line.split("=", 1)[1]
    return fallback


def write_markdown(report: dict[str, object], path: Path) -> None:
    latency = report["latency_us"]  # type: ignore[index]
    throughput = report["throughput_tps"]  # type: ignore[index]
    counts = report["counts"]  # type: ignore[index]
    metrics = report["metrics"]  # type: ignore[index]
    tcpdump = report["tcpdump"]  # type: ignore[index]
    bridge = report["bridge"]  # type: ignore[index]

    lines = [
        "# AxVisor Dual-Guest Latency and Reliability Report",
        "",
        f"- Evidence directory: `{report['evidence_dir']}`",
        f"- Final result: `{report['final_result']}`",
        f"- Linux guest CPUs online: `{metrics.get('QC_CPU_ONLINE', 'n/a')}`",
        f"- Linux guest processor count: `{metrics.get('QC_CPUINFO_PROCESSORS', 'n/a')}`",
        f"- RTOS endpoint: Zephyr e1000 at `192.0.2.20:4242`",
        f"- Linux guest periodic samples: `{metrics.get('QC_RT_PERIOD_SAMPLES', 'n/a')}` at `{metrics.get('QC_RT_PERIOD_NS', 'n/a')}` ns period",
        f"- Network mode: `{bridge.get('net_mode', 'n/a')}`",
        f"- Host network object: bridge `{bridge.get('bridge', 'n/a')}`, Linux TAP `{bridge.get('tap_linux', 'n/a')}`, RTOS TAP `{bridge.get('tap_rtos', 'n/a')}`",
        f"- RTOS guest periodic samples: `{metrics.get('QC_RTOS_PERIOD_SAMPLES', 'n/a')}` at `{metrics.get('QC_RTOS_PERIOD_NS', 'n/a')}` ns period",
        "",
        "## Reliability",
        "",
        "| Channel | Success | Failure | Notes |",
        "|---|---:|---:|---|",
        f"| Plain UDP echo | {counts['udp_pass']} | {counts['udp_fail']} | byte-exact payload check |",
        f"| QCZ1 reliable control | {counts['qcz1_ack']} | {counts['qcz1_fail']} | duplicate ACK samples: {counts['duplicate_ack_samples']} |",
        f"| AI control closed loop | {counts['ai_pass']} | {counts['ai_fail']} | fixed-point MLP inference in Linux guest |",
        f"| tcpdump | {tcpdump.get('packets_captured')} captured | {tcpdump.get('packets_dropped_by_kernel')} kernel drops | bridge `{bridge.get('bridge', 'n/a')}` |",
        "",
        "## Latency",
        "",
        "| Measurement | Count | Min | Mean | P95 | P99 | Max |",
        "|---|---:|---:|---:|---:|---:|---:|",
    ]

    for name, key in [
        ("Plain UDP RTT", "plain_udp_rtt"),
        ("QCZ1 ACK latency", "qcz1_ack"),
        ("QCZ1 duplicate ACK latency", "qcz1_duplicate_ack"),
        ("AI inference time", "ai_infer"),
        ("AI end-to-end latency", "ai_end_to_end"),
    ]:
        item = latency[key]  # type: ignore[index]
        lines.append(
            f"| {name} | {item['count']} | {fmt_us(item['min'])} | "
            f"{fmt_us(item['mean'])} | {fmt_us(item['p95'])} | "
            f"{fmt_us(item['p99'])} | {fmt_us(item['max'])} |"
        )

    lines.extend(
        [
            "",
            "## Effective Application Throughput",
            "",
            "This is a serialized request/response estimate derived from successful transactions and observed latency. It is intended as a conservative application-level throughput metric for the contest communication path, not a raw link-capacity benchmark.",
            "",
            "| Channel | Successful transactions | Active latency window | Effective throughput |",
            "|---|---:|---:|---:|",
        ]
    )

    for name, key in [
        ("Plain UDP echo", "plain_udp_rtt"),
        ("QCZ1 reliable control", "qcz1_ack"),
        ("AI control closed loop", "ai_end_to_end"),
    ]:
        item = throughput[key]  # type: ignore[index]
        active = item["active_seconds"]
        active_text = "n/a" if active is None else f"{active:.6f} s"
        lines.append(
            f"| {name} | {item['successful_transactions']} | {active_text} | "
            f"{fmt_tps(item['transactions_per_second'])} |"
        )

    lines.extend(
        [
            "",
            "## Linux Guest Periodic Task Latency",
            "",
            "| Measurement | Value |",
            "|---|---:|",
            f"| Samples | `{metrics.get('QC_RT_PERIOD_SAMPLES', 'n/a')}` |",
            f"| Period | `{metrics.get('QC_RT_PERIOD_NS', 'n/a')} ns` |",
            f"| Min lateness | `{metrics.get('QC_RT_LATENCY_MIN_NS', 'n/a')} ns` |",
            f"| Mean lateness | `{metrics.get('QC_RT_LATENCY_MEAN_NS', 'n/a')} ns` |",
            f"| P50 lateness | `{metrics.get('QC_RT_LATENCY_P50_NS', 'n/a')} ns` |",
            f"| P95 lateness | `{metrics.get('QC_RT_LATENCY_P95_NS', 'n/a')} ns` |",
            f"| P99 lateness | `{metrics.get('QC_RT_LATENCY_P99_NS', 'n/a')} ns` |",
            f"| Max lateness | `{metrics.get('QC_RT_LATENCY_MAX_NS', 'n/a')} ns` |",
            f"| >100 us overruns | `{metrics.get('QC_RT_OVERRUN_GT_100US', 'n/a')}` |",
            f"| >500 us overruns | `{metrics.get('QC_RT_OVERRUN_GT_500US', 'n/a')}` |",
            f"| >1000 us overruns | `{metrics.get('QC_RT_OVERRUN_GT_1000US', 'n/a')}` |",
            f"| Result | `{metrics.get('QC_RT_PERIODIC_RESULT', 'n/a')}` |",
        ]
    )

    lines.extend(
        [
            "",
            "## RTOS Guest Periodic Task Latency",
            "",
            "| Measurement | Value |",
            "|---|---:|",
            f"| Samples | `{metrics.get('QC_RTOS_PERIOD_SAMPLES', 'n/a')}` |",
            f"| Period | `{metrics.get('QC_RTOS_PERIOD_NS', 'n/a')} ns` |",
            f"| Min lateness | `{metrics.get('QC_RTOS_LATENCY_MIN_NS', 'n/a')} ns` |",
            f"| Mean lateness | `{metrics.get('QC_RTOS_LATENCY_MEAN_NS', 'n/a')} ns` |",
            f"| P50 lateness | `{metrics.get('QC_RTOS_LATENCY_P50_NS', 'n/a')} ns` |",
            f"| P95 lateness | `{metrics.get('QC_RTOS_LATENCY_P95_NS', 'n/a')} ns` |",
            f"| P99 lateness | `{metrics.get('QC_RTOS_LATENCY_P99_NS', 'n/a')} ns` |",
            f"| Max lateness | `{metrics.get('QC_RTOS_LATENCY_MAX_NS', 'n/a')} ns` |",
            f"| >100 us overruns | `{metrics.get('QC_RTOS_OVERRUN_GT_100US', 'n/a')}` |",
            f"| >500 us overruns | `{metrics.get('QC_RTOS_OVERRUN_GT_500US', 'n/a')}` |",
            f"| >1000 us overruns | `{metrics.get('QC_RTOS_OVERRUN_GT_1000US', 'n/a')}` |",
            f"| Result | `{metrics.get('QC_RTOS_PERIODIC_RESULT', 'n/a')}` |",
        ]
    )

    lines.extend(
        [
            "",
            "## Coverage Notes",
            "",
            "- This report covers the AxVisor dual-guest device/network service path: Linux guest, virtual network, Zephyr RTOS guest, QCZ1 protocol, and AI control loop.",
                "- It is useful evidence for request-response latency, maximum observed delay, error recovery, duplicate handling, and integrated stability of the task-two/task-three path.",
                "- When run with increased periodic samples and Linux guest stress workers, the same evidence also supports the task-one AxVisor-hosted dual-guest realtime campaign. Compare it with the native Zephyr baseline when writing the final task-one evaluation.",
            "",
        ]
    )
    path.write_text("\n".join(lines), encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence_dir", type=Path)
    parser.add_argument("--json-out", type=Path)
    parser.add_argument("--md-out", type=Path)
    parser.add_argument("--fail-on-missing", action="store_true")
    args = parser.parse_args()

    evidence_dir = args.evidence_dir.resolve()
    qemu_log = evidence_dir / "qemu.log"
    if not qemu_log.exists():
        raise SystemExit(f"missing qemu log: {qemu_log}")

    report = parse_qemu_log(qemu_log)
    report["evidence_dir"] = str(evidence_dir)
    report["throughput_tps"] = serial_throughput(report["latency_us"])  # type: ignore[arg-type]
    report["tcpdump"] = parse_tcpdump(evidence_dir / "tcpdump.log")
    report["bridge"] = parse_bridge_info(evidence_dir / "bridge.txt")

    required = {
        "QC_RT_PERIODIC_RESULT=PASS",
        "QC_RTOS_PERIODIC_RESULT=PASS",
        "QC_DUAL_GUEST_UDP_ECHO_RESULT=PASS",
        "QC_QCZ1_RELIABLE_RESULT=PASS",
        "QC_AI_CONTROL_RESULT=PASS",
        "QC_QCZ1_GUEST_DEMO=PASS",
        "QC_DUAL_GUEST_LINUX_INIT=PASS",
    }
    observed = set(report["markers"])  # type: ignore[arg-type]
    missing = sorted(required - observed)
    report["missing_required_markers"] = missing
    report["analysis_result"] = "PASS" if not missing else "FAIL"
    report["final_result"] = read_final_result(evidence_dir, str(report["analysis_result"]))

    json_out = args.json_out or evidence_dir / "realtime-summary.json"
    md_out = args.md_out or evidence_dir / "realtime-report.md"
    json_out.write_text(json.dumps(report, indent=2, sort_keys=True), encoding="utf-8")
    write_markdown(report, md_out)

    print(f"analysis_result={report['analysis_result']}")
    print(f"json_out={json_out}")
    print(f"md_out={md_out}")
    if missing:
        print("missing_required_markers=" + ",".join(missing))
    return 1 if args.fail_on_missing and missing else 0


if __name__ == "__main__":
    raise SystemExit(main())
