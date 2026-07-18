#!/usr/bin/env python3
"""Summarize StarryOS net-bench run logs into per-test mean/stddev metrics.

The guest bench core (net-bench-common.sh) wraps each iperf3 -J measurement in
markers:

    NET_BENCH_BEGIN test=<id> iter=<n> warmup=<0|1>
    <iperf3 JSON>
    NET_BENCH_END test=<id> iter=<n>

/proc/net/dev snapshots are embedded between NET_STATS_BEGIN/END markers
(with optional `warmup=<0|1>` on the BEGIN line so this parser can exclude
warmup traffic from protocol-overhead aggregation).

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

# NET_STATS_BEGIN may carry an optional warmup=<0|1> flag emitted by the
# guest-side shell scripts so the protocol-overhead section can exclude
# warmup iterations.
NETSTATS_BEGIN_RE = re.compile(
    r"^NET_STATS_BEGIN(?:\s+warmup=([01]))?\s*$"
)
NETSTATS_END_RE = re.compile(r"^NET_STATS_END\s*$")
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

# Test IDs whose traffic direction is reverse (downlink: host -> guest).
# Used for protocol-overhead direction attribution.
_REVERSE_TEST_IDS: frozenset[str] = frozenset({"tcp1r"})


@dataclass
class Sample:
    """One measured metric from a single iteration."""

    mbps: float
    pps: float | None = None
    lost_percent: float | None = None
    retransmits: int | None = None
    app_bytes: int = 0  # application-layer bytes from iperf3 sum_received/sum


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
    app_bytes = int(summary.get("bytes", 0))

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
        mbps=mbps, pps=pps, lost_percent=lost_percent, retransmits=retransmits,
        app_bytes=app_bytes,
    )


@dataclass
class NetDevSnapshot:
    """One /proc/net/dev snapshot parsed from a NET_STATS_BEGIN/END block.

    Each key is an interface name (e.g. 'eth0', 'lo'). Values are dicts
    with keys matching /proc/net/dev columns: rx_bytes, rx_packets,
    rx_errors, rx_dropped, tx_bytes, tx_packets, tx_errors, tx_dropped.
    """

    interfaces: dict[str, dict[str, int]] = field(default_factory=dict)
    warmup: bool = False  # True if this snapshot belongs to a warmup iteration


# /proc/net/dev row parser: extracts interface name + 16 column values.
# Format: "  iface:  rx_bytes rx_pkts ... | tx_bytes tx_pkts ..."
_IFACE_RE = re.compile(
    r"^\s*(\S+):"  # interface name (stripped, colon-separated)
    r"\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)"  # RX 8 cols
    r"\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)"  # TX 8 cols
)

# Column index -> /proc/net/dev field name mapping.
_IFACE_FIELDS = [
    "rx_bytes", "rx_packets", "rx_errors", "rx_dropped",
    "rx_fifo", "rx_frame", "rx_compressed", "rx_multicast",
    "tx_bytes", "tx_packets", "tx_errors", "tx_dropped",
    "tx_fifo", "tx_colls", "tx_carrier", "tx_compressed",
]

# /proc/net/dev header and blank-line pattern — skip before regex parse.
_NETDEV_HEADER_RE = re.compile(r"^\s*(Inter-\||face\s+\||$)")


def _parse_proc_net_dev_line(line: str) -> tuple[str, dict[str, int]]:
    """Parse one /proc/net/dev data row into (iface_name, fields_dict)."""
    m = _IFACE_RE.match(line)
    if not m:
        raise ValueError(f"cannot parse /proc/net/dev line: {line!r}")
    name = m.group(1)
    vals = [int(m.group(i + 2)) for i in range(16)]
    fields = dict(zip(_IFACE_FIELDS, vals))
    return name, fields


def parse_netstats(text: str) -> list[NetDevSnapshot]:
    """Extract NET_STATS_BEGIN/END blocks containing /proc/net/dev output."""
    snapshots: list[NetDevSnapshot] = []
    lines = text.splitlines()
    i = 0
    skipped_lines = 0
    while i < len(lines):
        m = NETSTATS_BEGIN_RE.match(lines[i])
        if not m:
            i += 1
            continue
        warmup = m.group(1) == "1"
        i += 1
        snap = NetDevSnapshot(warmup=warmup)
        while i < len(lines) and not NETSTATS_END_RE.match(lines[i]):
            if _NETDEV_HEADER_RE.match(lines[i]):
                i += 1
                continue
            try:
                name, fields = _parse_proc_net_dev_line(lines[i])
                snap.interfaces[name] = fields
            except ValueError:
                skipped_lines += 1
            i += 1
        if i < len(lines):
            i += 1  # consume END
        if snap.interfaces:
            snapshots.append(snap)
    if skipped_lines:
        print(
            f"warning: skipped {skipped_lines} unparseable line(s) "
            f"inside NET_STATS_BEGIN/END blocks",
            file=sys.stderr,
        )
    return snapshots


def _fmt_bytes(n: int) -> str:
    """Format a byte count in human-readable form."""
    if n >= 1 << 30:
        return f"{n / (1 << 30):.2f} GB"
    if n >= 1 << 20:
        return f"{n / (1 << 20):.2f} MB"
    if n >= 1 << 10:
        return f"{n / (1 << 10):.2f} KB"
    return f"{n} B"


def _netdev_delta(
    before: NetDevSnapshot, after: NetDevSnapshot
) -> dict[str, dict[str, int]]:
    """Compute per-interface counter deltas between two snapshots."""
    delta: dict[str, dict[str, int]] = {}
    all_ifaces = set(before.interfaces.keys()) | set(after.interfaces.keys())
    for iface in all_ifaces:
        b = before.interfaces.get(iface, {})
        a = after.interfaces.get(iface, {})
        d = {}
        for key in _IFACE_FIELDS:
            d[key] = a.get(key, 0) - b.get(key, 0)
        if any(v != 0 for v in d.values()):
            delta[iface] = d
    return delta


def _sum_deltas(
    deltas: list[dict[str, dict[str, int]]]
) -> dict[str, dict[str, int]]:
    """Sum multiple per-interface deltas into a single accumulator."""
    total: dict[str, dict[str, int]] = {}
    for d in deltas:
        for iface, fields in d.items():
            if iface not in total:
                total[iface] = {k: 0 for k in _IFACE_FIELDS}
            for k, v in fields.items():
                total[iface][k] += v
    return total


def _pair_deltas(
    snapshots: list[NetDevSnapshot],
    skip_warmup: bool = False,
) -> tuple[list[dict[str, dict[str, int]]], int, int]:
    """Pair consecutive snapshots and return (deltas, tx_total, rx_total).

    When *skip_warmup* is True, snapshot pairs whose `before` snapshot
    is tagged warmup are excluded so protocol-overhead analysis only
    compares measured-iteration L2 traffic against application-layer bytes.
    """
    deltas: list[dict[str, dict[str, int]]] = []
    tx_total = 0
    rx_total = 0
    for j in range(0, len(snapshots) - 1, 2):
        before, after = snapshots[j], snapshots[j + 1]
        if skip_warmup and before.warmup:
            continue
        d = _netdev_delta(before, after)
        if d:
            deltas.append(d)
            for fields in d.values():
                tx_total += fields.get("tx_bytes", 0)
                rx_total += fields.get("rx_bytes", 0)
    return deltas, tx_total, rx_total


def render_netstats(snapshots: list[NetDevSnapshot]) -> str:
    """Render /proc/net/dev L2 counter deltas across all measured iterations.

    Consecutive snapshots are paired (before, after) and accumulated.
    Warmup-tagged snapshots are excluded.
    """
    if len(snapshots) < 2:
        return ""
    deltas, _, _ = _pair_deltas(snapshots, skip_warmup=True)
    if not deltas:
        return ""
    total = _sum_deltas(deltas)
    out = ["## /proc/net/dev (kernel interface counters)"]
    for iface in sorted(total.keys()):
        f = total[iface]
        tx_b = f.get("tx_bytes", 0)
        rx_b = f.get("rx_bytes", 0)
        tx_p = f.get("tx_packets", 0)
        rx_p = f.get("rx_packets", 0)
        tx_e = f.get("tx_errors", 0)
        rx_e = f.get("rx_errors", 0)
        tx_d = f.get("tx_dropped", 0)
        rx_d = f.get("rx_dropped", 0)
        parts = [f"  [{iface}]"]
        parts.append(f"tx={_fmt_bytes(tx_b)}/{tx_p}pkts")
        parts.append(f"rx={_fmt_bytes(rx_b)}/{rx_p}pkts")
        if tx_e or rx_e or tx_d or rx_d:
            parts.append(f"tx_err={tx_e} tx_drop={tx_d} rx_err={rx_e} rx_drop={rx_d}")
        out.append("  ".join(parts))
    out.append("")
    return "\n".join(out)


# perf stat counter names accepted by parse_perf_stat.
_PERF_COUNTERS: frozenset[str] = frozenset({
    "cycles", "instructions", "cache-references", "cache-misses",
})

# perf stat output line pattern: optional commas in number, counter name.
_PERF_STAT_RE = re.compile(r"^\s*([0-9,]+)\s+(\S+)")


def parse_perf_stat(text: str) -> dict[str, int]:
    """Extract counter values from `perf stat` output."""
    counters: dict[str, int] = {}
    for line in text.splitlines():
        m = _PERF_STAT_RE.match(line)
        if m:
            raw_val = m.group(1).replace(",", "")
            name = m.group(2)
            if name in _PERF_COUNTERS:
                counters[name] = int(raw_val)
    return counters


def render_perf(counters: dict[str, int]) -> str:
    """Render perf stat counters as a markdown section."""
    if not counters:
        return ""
    out = ["## CPU Efficiency (perf stat)"]
    cycles = counters.get("cycles")
    instructions = counters.get("instructions")
    if cycles:
        out.append(f"  cycles         : {cycles:,}")
    if instructions:
        out.append(f"  instructions   : {instructions:,}")
    if cycles and instructions:
        ipc = instructions / cycles if cycles > 0 else 0.0
        out.append(f"  IPC            : {ipc:.2f}")
    crefs = counters.get("cache-references")
    cmiss = counters.get("cache-misses")
    if crefs:
        out.append(f"  cache-refs     : {crefs:,}")
    if cmiss:
        out.append(f"  cache-misses   : {cmiss:,}")
    if crefs and cmiss and crefs > 0:
        miss_rate = cmiss / crefs * 100
        out.append(f"  cache-miss-rate: {miss_rate:.1f}%")
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
        # NET_STATS_BEGIN/END blocks (containing /proc/net/dev snapshots)
        # that appear between BEGIN and the iperf3 JSON are skipped so
        # json.loads() receives a clean payload.
        body: list[str] = []
        in_netstats = False
        i += 1
        while i < n and not END_RE.match(lines[i]):
            # A stray BEGIN means the END was lost; bail out of this block.
            if BEGIN_RE.match(lines[i]):
                break
            if NETSTATS_BEGIN_RE.match(lines[i]):
                in_netstats = True
                i += 1
                continue
            if in_netstats and NETSTATS_END_RE.match(lines[i]):
                in_netstats = False
                i += 1
                continue
            if not in_netstats:
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


def render_text(stats: dict[str, TestStats], snapshots: list[NetDevSnapshot]) -> str:
    out: list[str] = []
    ordered = [t for t in TEST_ORDER if t in stats]
    ordered += [t for t in stats if t not in TEST_ORDER]

    # Compute aggregate L2 and application-layer byte totals for overhead
    # comparison.  Warmup-tagged snapshots are excluded so the L2 and
    # application-layer totals are comparable.
    _, l2_tx_total, l2_rx_total = _pair_deltas(snapshots, skip_warmup=True)

    app_tx_total = 0
    app_rx_total = 0
    for test_id in ordered:
        ts = stats[test_id]
        for s in ts.measured:
            if test_id in _REVERSE_TEST_IDS:
                app_rx_total += s.app_bytes
            else:
                app_tx_total += s.app_bytes

    # Show aggregate L2-vs-app overview.
    if l2_tx_total or l2_rx_total:
        out.append("## Protocol Overhead (L2 vs Application)")
        if l2_tx_total > 0:
            if app_tx_total > 0:
                ratio = (l2_tx_total - app_tx_total) / app_tx_total * 100
                overhead = f"{ratio:.1f}%"
            else:
                overhead = "N/A"
            out.append(
                f"  TX  L2={_fmt_bytes(l2_tx_total)}  "
                f"app={_fmt_bytes(app_tx_total)}  "
                f"overhead={overhead}"
            )
        if l2_rx_total > 0:
            if app_rx_total > 0:
                ratio = (l2_rx_total - app_rx_total) / app_rx_total * 100
                overhead = f"{ratio:.1f}%"
            else:
                overhead = "N/A"
            out.append(
                f"  RX  L2={_fmt_bytes(l2_rx_total)}  "
                f"app={_fmt_bytes(app_rx_total)}  "
                f"overhead={overhead}"
            )
        out.append("")
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
    parser.add_argument(
        "--perf",
        action="append",
        default=[],
        help="perf stat output file(s) for CPU efficiency section",
    )
    args = parser.parse_args(argv)

    combined: dict[str, TestStats] = {}
    all_netstats: list[NetDevSnapshot] = []
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
        # Perf stat section (if available).
        if args.perf:
            all_perf: dict[str, int] = {}
            for perf_path in args.perf:
                try:
                    with open(perf_path, "r", errors="replace") as fh:
                        all_perf.update(parse_perf_stat(fh.read()))
                except OSError as exc:
                    print(
                        f"warning: cannot read perf file {perf_path}: {exc}",
                        file=sys.stderr,
                    )
            perf_text = render_perf(all_perf)
            if perf_text:
                print(perf_text)
        ns_text = render_netstats(all_netstats)
        if ns_text:
            print(ns_text)
        print(render_text(combined, all_netstats))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
