#!/usr/bin/env python3
"""Summarize StarryOS net-bench run logs into per-test mean/stddev metrics.

The guest bench core (net-bench-common.sh) wraps each iperf3 -J measurement in
markers:

    NET_BENCH_BEGIN test=<id> iter=<n> warmup=<0|1>
    <iperf3 JSON>
    NET_BENCH_END test=<id> iter=<n>

This script extracts those blocks, parses the iperf3 JSON, drops warmup
iterations, and reports mean +/- stddev across the measured iterations for each
test id. It only depends on the Python standard library (no jq), so it runs in
the minimal WSL2 host environment.

Per methodology §3.4: data points need >=5 iterations with mean+stddev, and a
relative stddev above ~10% is flagged as noisy and not trustworthy.

Usage:
    summarize.py RUN_LOG [RUN_LOG ...]
    summarize.py --json RUN_LOG          # machine-readable output
"""

from __future__ import annotations

import argparse
import json
import math
import re
import sys
from dataclasses import dataclass, field

BEGIN_RE = re.compile(
    r"^NET_BENCH_BEGIN\s+test=(\S+)\s+iter=(\d+)\s+warmup=([01])\s*$"
)
END_RE = re.compile(r"^NET_BENCH_END\s+test=(\S+)\s+iter=(\d+)\s*$")

# net_stats eBPF marker pattern
NETSTATS_BEGIN_RE = re.compile(r"^NET_STATS_BEGIN\s*$")
NETSTATS_END_RE = re.compile(r"^NET_STATS_END\s*$")
NETSTATS_KV_RE = re.compile(r"(\w+)=(\d+)")

# Relative stddev (stddev/mean) above this fraction is flagged as noisy.
NOISE_THRESHOLD = 0.10

# Human-friendly descriptions, also fixes report ordering.
TEST_ORDER = ["tcp1", "tcp4", "tcp1r", "udp1g", "udp64"]
TEST_LABELS = {
    "tcp1": "TCP 1-stream (uplink)",
    "tcp4": "TCP 4-stream (uplink)",
    "tcp1r": "TCP 1-stream (reverse/downlink)",
    "udp1g": "UDP 1G target (large packets)",
    "udp64": "UDP 64B small-packet PPS",
}


@dataclass
class Sample:
    """One measured metric from a single iteration."""

    mbps: float
    pps: float | None = None
    lost_percent: float | None = None
    retransmits: int | None = None


@dataclass
class TestStats:
    test_id: str
    measured: list[Sample] = field(default_factory=list)
    warmup_count: int = 0
    parse_errors: int = 0


def _extract_metric(doc: dict) -> Sample:
    """Pull throughput / PPS / loss / retransmits out of one iperf3 JSON doc."""
    end = doc.get("end", {})
    # UDP results live under sum; TCP under sum_received (fallback sum_sent).
    summary = end.get("sum_received") or end.get("sum") or end.get("sum_sent")
    if not summary:
        raise ValueError("no sum/sum_received/sum_sent block")

    mbps = float(summary.get("bits_per_second", 0.0)) / 1e6

    pps = None
    lost_percent = None
    retransmits = None

    seconds = float(summary.get("seconds", 0.0)) or None
    packets = summary.get("packets")
    if packets is not None and seconds:
        pps = float(packets) / seconds
    if "lost_percent" in summary:
        lost_percent = float(summary["lost_percent"])
    if "retransmits" in summary:
        retransmits = int(summary["retransmits"])
    # TCP retransmits also appear under sum_sent.
    elif "sum_sent" in end and "retransmits" in end["sum_sent"]:
        retransmits = int(end["sum_sent"]["retransmits"])

    return Sample(
        mbps=mbps, pps=pps, lost_percent=lost_percent, retransmits=retransmits
    )


@dataclass
class NetStatsSnapshot:
    """One NET_STATS_BEGIN/END block parsed from a log."""

    tcp_tx_pkts: int = 0
    tcp_tx_bytes: int = 0
    tcp_rx_pkts: int = 0
    tcp_rx_bytes: int = 0
    udp_tx_pkts: int = 0
    udp_tx_bytes: int = 0
    udp_rx_pkts: int = 0
    udp_rx_bytes: int = 0


def parse_netstats(text: str) -> list[NetStatsSnapshot]:
    """Extract all NET_STATS_BEGIN/END blocks and return parsed snapshots."""
    snapshots: list[NetStatsSnapshot] = []
    lines = text.splitlines()
    i = 0
    while i < len(lines):
        if not NETSTATS_BEGIN_RE.match(lines[i]):
            i += 1
            continue
        i += 1
        snap = NetStatsSnapshot()
        while i < len(lines) and not NETSTATS_END_RE.match(lines[i]):
            for m in NETSTATS_KV_RE.finditer(lines[i]):
                key, val = m.group(1), int(m.group(2))
                if hasattr(snap, key):
                    setattr(snap, key, val)
            i += 1
        if i < len(lines):
            i += 1  # consume END
        snapshots.append(snap)
    return snapshots


def render_netstats(snapshots: list[NetStatsSnapshot]) -> str:
    if not snapshots:
        return ""
    out = ["## eBPF net_stats (ax-net kernel counters)"]
    for idx, s in enumerate(snapshots):
        label = "before" if idx == 0 else ("after" if idx == 1 else f"sample-{idx}")
        out.append(
            f"  [{label}]  tcp tx={s.tcp_tx_pkts}pkts/{s.tcp_tx_bytes}B"
            f"  rx={s.tcp_rx_pkts}pkts/{s.tcp_rx_bytes}B"
            f"  udp tx={s.udp_tx_pkts}pkts/{s.udp_tx_bytes}B"
            f"  rx={s.udp_rx_pkts}pkts/{s.udp_rx_bytes}B"
        )
    if len(snapshots) >= 2:
        b, a = snapshots[0], snapshots[-1]
        out.append(
            f"  [delta]    tcp tx={a.tcp_tx_pkts - b.tcp_tx_pkts}pkts/{a.tcp_tx_bytes - b.tcp_tx_bytes}B"
            f"  rx={a.tcp_rx_pkts - b.tcp_rx_pkts}pkts/{a.tcp_rx_bytes - b.tcp_rx_bytes}B"
            f"  udp tx={a.udp_tx_pkts - b.udp_tx_pkts}pkts/{a.udp_tx_bytes - b.udp_tx_bytes}B"
            f"  rx={a.udp_rx_pkts - b.udp_rx_pkts}pkts/{a.udp_rx_bytes - b.udp_rx_bytes}B"
        )
    out.append("")
    return "\n".join(out)


def parse_log(text: str) -> dict[str, TestStats]:
    """Parse a run log into {test_id: TestStats}."""
    lines = text.splitlines()
    stats: dict[str, TestStats] = {}
    i = 0
    n = len(lines)
    while i < n:
        m = BEGIN_RE.match(lines[i])
        if not m:
            i += 1
            continue
        test_id, _iter, warmup = m.group(1), int(m.group(2)), m.group(3) == "1"
        # Collect JSON lines until the matching END marker.
        body: list[str] = []
        i += 1
        while i < n and not END_RE.match(lines[i]):
            # A stray BEGIN means the END was lost; bail out of this block.
            if BEGIN_RE.match(lines[i]):
                break
            body.append(lines[i])
            i += 1
        if i < n and END_RE.match(lines[i]):
            i += 1  # consume END

        ts = stats.setdefault(test_id, TestStats(test_id=test_id))
        if warmup:
            ts.warmup_count += 1
            continue
        try:
            doc = json.loads("\n".join(body))
            ts.measured.append(_extract_metric(doc))
        except (ValueError, json.JSONDecodeError):
            ts.parse_errors += 1
    return stats


def _mean_std(values: list[float]) -> tuple[float, float]:
    if not values:
        return (0.0, 0.0)
    mean = sum(values) / len(values)
    if len(values) < 2:
        return (mean, 0.0)
    var = sum((v - mean) ** 2 for v in values) / (len(values) - 1)
    return (mean, math.sqrt(var))


def _fmt_mbps(mean: float, std: float) -> str:
    rel = (std / mean) if mean else 0.0
    flag = "  [NOISY >10%]" if rel > NOISE_THRESHOLD else ""
    if mean >= 1000:
        return f"{mean / 1000:.2f} +/- {std / 1000:.2f} Gbit/s{flag}"
    return f"{mean:.2f} +/- {std:.2f} Mbit/s{flag}"


def render_text(stats: dict[str, TestStats]) -> str:
    out: list[str] = []
    ordered = [t for t in TEST_ORDER if t in stats]
    ordered += [t for t in stats if t not in TEST_ORDER]
    for test_id in ordered:
        ts = stats[test_id]
        label = TEST_LABELS.get(test_id, test_id)
        out.append(f"## {label}  (test={test_id})")
        if not ts.measured:
            out.append(
                f"  no measured iterations "
                f"(warmup={ts.warmup_count}, parse_errors={ts.parse_errors})"
            )
            out.append("")
            continue
        mbps_mean, mbps_std = _mean_std([s.mbps for s in ts.measured])
        out.append(
            f"  throughput : {_fmt_mbps(mbps_mean, mbps_std)} "
            f"(n={len(ts.measured)}, warmup={ts.warmup_count})"
        )
        pps_vals = [s.pps for s in ts.measured if s.pps is not None]
        if pps_vals:
            pps_mean, pps_std = _mean_std(pps_vals)
            out.append(f"  pps        : {pps_mean:.0f} +/- {pps_std:.0f} pkt/s")
        loss_vals = [
            s.lost_percent for s in ts.measured if s.lost_percent is not None
        ]
        if loss_vals:
            loss_mean, loss_std = _mean_std(loss_vals)
            out.append(f"  udp loss   : {loss_mean:.2f} +/- {loss_std:.2f} %")
        retr_vals = [
            s.retransmits for s in ts.measured if s.retransmits is not None
        ]
        if retr_vals:
            retr_mean, retr_std = _mean_std([float(v) for v in retr_vals])
            out.append(f"  retransmits: {retr_mean:.1f} +/- {retr_std:.1f}")
        if ts.parse_errors:
            out.append(f"  parse_errors: {ts.parse_errors}")
        out.append("")
    return "\n".join(out)


def render_json(stats: dict[str, TestStats]) -> str:
    payload: dict[str, dict] = {}
    for test_id, ts in stats.items():
        mbps_mean, mbps_std = _mean_std([s.mbps for s in ts.measured])
        pps_vals = [s.pps for s in ts.measured if s.pps is not None]
        loss_vals = [
            s.lost_percent for s in ts.measured if s.lost_percent is not None
        ]
        retr_vals = [
            float(s.retransmits)
            for s in ts.measured
            if s.retransmits is not None
        ]
        entry: dict = {
            "label": TEST_LABELS.get(test_id, test_id),
            "iterations": len(ts.measured),
            "warmup": ts.warmup_count,
            "parse_errors": ts.parse_errors,
            "throughput_mbps_mean": mbps_mean,
            "throughput_mbps_std": mbps_std,
        }
        if pps_vals:
            pm, ps = _mean_std(pps_vals)
            entry["pps_mean"], entry["pps_std"] = pm, ps
        if loss_vals:
            lm, ls = _mean_std(loss_vals)
            entry["loss_percent_mean"], entry["loss_percent_std"] = lm, ls
        if retr_vals:
            rm, rs = _mean_std(retr_vals)
            entry["retransmits_mean"], entry["retransmits_std"] = rm, rs
        payload[test_id] = entry
    return json.dumps(payload, indent=2)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("logs", nargs="+", help="run log file(s) to summarize")
    parser.add_argument(
        "--json", action="store_true", help="emit machine-readable JSON"
    )
    args = parser.parse_args(argv)

    combined: dict[str, TestStats] = {}
    all_netstats: list[NetStatsSnapshot] = []
    for path in args.logs:
        try:
            with open(path, "r", errors="replace") as fh:
                text = fh.read()
        except OSError as exc:
            print(f"error: cannot read {path}: {exc}", file=sys.stderr)
            return 1
        for test_id, ts in parse_log(text).items():
            agg = combined.setdefault(test_id, TestStats(test_id=test_id))
            agg.measured.extend(ts.measured)
            agg.warmup_count += ts.warmup_count
            agg.parse_errors += ts.parse_errors
        all_netstats.extend(parse_netstats(text))

    if not combined:
        print(
            "warning: no NET_BENCH_BEGIN/END blocks found; "
            "is this a current net-bench run log?",
            file=sys.stderr,
        )
        return 2

    if args.json:
        print(render_json(combined))
    else:
        ns_text = render_netstats(all_netstats)
        if ns_text:
            print(ns_text)
        print(render_text(combined))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
