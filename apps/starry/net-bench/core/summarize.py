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

from constants import (  # noqa: E402 — shared with compare-baseline.py
    TEST_LABELS,
    TEST_ORDER,
    _REVERSE_TEST_IDS,
    format_bytes,
)

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


@dataclass
class Sample:
    """One measured metric from a single iteration."""

    mbps: float
    pps: float | None = None
    lost_percent: float | None = None
    jitter_ms: float | None = None  # UDP jitter from iperf3 end.sum.jitter_ms
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
    # UDP jitter (ms) — present in iperf3 -u JSON end.sum.jitter_ms.
    jitter_ms = None
    if "jitter_ms" in summary:
        jitter_ms = float(summary["jitter_ms"])
    if "retransmits" in summary:
        retransmits = int(summary["retransmits"])
    # TCP retransmits also appear under sum_sent.
    elif "sum_sent" in end and "retransmits" in end["sum_sent"]:
        retransmits = int(end["sum_sent"]["retransmits"])

    return Sample(
        mbps=mbps, pps=pps, lost_percent=lost_percent, jitter_ms=jitter_ms,
        retransmits=retransmits,
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
    test_id: str = "unknown"  # test identifier from nearest NET_BENCH_BEGIN


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
    """Extract NET_STATS_BEGIN/END blocks containing /proc/net/dev output.

    Each snapshot is tagged with the active test_id from the nearest preceding
    NET_BENCH_BEGIN marker so that per-test L2 deltas can be computed.
    """
    snapshots: list[NetDevSnapshot] = []
    lines = text.splitlines()
    i = 0
    skipped_lines = 0
    empty_blocks = 0
    current_test_id = "unknown"
    while i < len(lines):
        line = lines[i]

        # Track test context from NET_BENCH_BEGIN markers.
        begin_m = BEGIN_RE.match(line)
        if begin_m:
            current_test_id = begin_m.group(1)
            i += 1
            continue

        m = NETSTATS_BEGIN_RE.match(line)
        if not m:
            i += 1
            continue
        warmup = m.group(1) == "1"
        i += 1
        snap = NetDevSnapshot(warmup=warmup, test_id=current_test_id)
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
        else:
            empty_blocks += 1
    if empty_blocks:
        print(
            f"warning: {empty_blocks} NET_STATS block(s) contained no "
            f"parseable /proc/net/dev rows — snapshot pairing may be affected "
            f"if gaps exist",
            file=sys.stderr,
        )
    if skipped_lines:
        print(
            f"warning: skipped {skipped_lines} unparseable line(s) "
            f"inside NET_STATS_BEGIN/END blocks",
            file=sys.stderr,
        )
    return snapshots


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

    When *skip_warmup* is True, snapshot pairs whose ``before`` snapshot
    is tagged warmup are excluded so protocol-overhead analysis only
    compares measured-iteration L2 traffic against application-layer bytes.

    Pairing invariant: consecutive snapshots at (j, j+1) must share the
    same ``warmup`` flag and ``test_id``.  A mismatch indicates a dropped
    or reordered snapshot and produces a loud assertion failure so the
    user can investigate rather than getting silently wrong deltas.
    """
    if len(snapshots) % 2 != 0:
        print(
            f"warning: odd number of NET_STATS snapshots ({len(snapshots)}), "
            f"last snapshot will be ignored",
            file=sys.stderr,
        )
    deltas: list[dict[str, dict[str, int]]] = []
    tx_total = 0
    rx_total = 0
    for j in range(0, len(snapshots) - 1, 2):
        before, after = snapshots[j], snapshots[j + 1]
        assert before.warmup == after.warmup, (
            f"warmup mismatch in snapshot pair ({j}, {j+1}): "
            f"{before.warmup} != {after.warmup}"
        )
        assert before.test_id == after.test_id, (
            f"test_id mismatch in snapshot pair ({j}, {j+1}): "
            f"{before.test_id!r} != {after.test_id!r}"
        )
        if skip_warmup and before.warmup:
            continue
        d = _netdev_delta(before, after)
        if d:
            deltas.append(d)
            for fields in d.values():
                tx_total += fields.get("tx_bytes", 0)
                rx_total += fields.get("rx_bytes", 0)
    return deltas, tx_total, rx_total


def _compute_per_test_breakdown(
    snapshots: list[NetDevSnapshot],
) -> dict[str, dict[str, dict[str, int]]]:
    """Group consecutive paired-snapshot deltas by test_id.

    Returns ``{test_id: {iface: {field: delta}}}`` where deltas from measured
    (non-warmup) iterations are summed per test and per interface.  Warmup
    pairs whose *before* snapshot is tagged ``warmup=True`` are excluded.
    """
    breakdown: dict[str, dict[str, dict[str, int]]] = {}
    for j in range(0, len(snapshots) - 1, 2):
        before, after = snapshots[j], snapshots[j + 1]
        # Paired snapshots must agree on warmup status and test identity.
        # A mismatch means a snapshot was dropped or reordered.
        assert before.warmup == after.warmup, (
            f"warmup mismatch in per-test pair ({j}, {j+1}): "
            f"{before.warmup} != {after.warmup}"
        )
        assert before.test_id == after.test_id, (
            f"test_id mismatch in per-test pair ({j}, {j+1}): "
            f"{before.test_id!r} != {after.test_id!r}"
        )
        if before.warmup:
            continue
        d = _netdev_delta(before, after)
        if not d:
            continue
        tid = before.test_id
        if tid not in breakdown:
            breakdown[tid] = {}
        for iface, fields in d.items():
            if iface not in breakdown[tid]:
                breakdown[tid][iface] = {k: 0 for k in _IFACE_FIELDS}
            for k, v in fields.items():
                breakdown[tid][iface][k] += v
    return breakdown


def _fmt_rate(part: int, total: int) -> str:
    """Format a part/total fraction as a percentage or ratio."""
    if total == 0:
        return "—" if part == 0 else f"{part}/0"
    pct = part / total * 100
    if pct < 0.01:
        return "<0.01%"
    if pct < 1:
        return f"{pct:.2f}%"
    return f"{pct:.1f}%"


def render_netstats(snapshots: list[NetDevSnapshot]) -> str:
    """Render /proc/net/dev L2 counter deltas with per-test breakdown.

    Consecutive snapshots are paired (before, after).  Warmup-tagged pairs
    are excluded.  Output includes a per-test table showing what each test
    contributed to each interface, plus aggregate totals with error/drop
    rates.
    """
    if len(snapshots) < 2:
        return ""

    # ---- aggregate totals ------------------------------------------------
    deltas, _, _ = _pair_deltas(snapshots, skip_warmup=True)
    if not deltas:
        return ""
    totals = _sum_deltas(deltas)

    # ---- per-test breakdown -----------------------------------------------
    per_test = _compute_per_test_breakdown(snapshots)

    out = ["## /proc/net/dev (kernel interface counters)", ""]

    # Per-interface per-test table.
    all_ifaces = sorted(
        set(totals.keys()) | {iface for td in per_test.values() for iface in td}
    )
    for iface in all_ifaces:
        tf = totals.get(iface, {k: 0 for k in _IFACE_FIELDS})
        out.append(f"### {iface}")
        out.append("| test   | tx_bytes | tx_pkts | rx_bytes | rx_pkts | tx_err | tx_drop | rx_err | rx_drop |")
        out.append("|--------|----------|---------|----------|---------|--------|---------|--------|---------|")

        # Ordered by test label so output is deterministic.
        test_order = [t for t in TEST_ORDER if t in per_test]
        test_order += [t for t in per_test if t not in test_order]
        for tid in test_order:
            label = TEST_LABELS.get(tid, tid)
            td = per_test[tid].get(iface, {k: 0 for k in _IFACE_FIELDS})
            ttx_b = format_bytes(td.get("tx_bytes", 0))
            ttx_p = td.get("tx_packets", 0)
            trx_b = format_bytes(td.get("rx_bytes", 0))
            trx_p = td.get("rx_packets", 0)
            ttx_e = td.get("tx_errors", 0)
            ttx_d = td.get("tx_dropped", 0)
            trx_e = td.get("rx_errors", 0)
            trx_d = td.get("rx_dropped", 0)
            out.append(
                f"| {label:<6} | {ttx_b:>8} | {ttx_p:>7} | "
                f"{trx_b:>8} | {trx_p:>7} | "
                f"{ttx_e:>6} | {ttx_d:>7} | {trx_e:>6} | {trx_d:>7} |"
            )

        # Total row.
        ttx_b = format_bytes(tf.get("tx_bytes", 0))
        ttx_p = tf.get("tx_packets", 0)
        trx_b = format_bytes(tf.get("rx_bytes", 0))
        trx_p = tf.get("rx_packets", 0)
        ttx_e = tf.get("tx_errors", 0)
        ttx_d = tf.get("tx_dropped", 0)
        trx_e = tf.get("rx_errors", 0)
        trx_d = tf.get("rx_dropped", 0)
        out.append(
            f"| **total** | {ttx_b:>8} | {ttx_p:>7} | "
            f"{trx_b:>8} | {trx_p:>7} | "
            f"{ttx_e:>6} | {ttx_d:>7} | {trx_e:>6} | {trx_d:>7} |"
        )

        # Error/drop rate summary for this interface.
        tx_total_pkts = tf.get("tx_packets", 0)
        rx_total_pkts = tf.get("rx_packets", 0)
        tx_err_rate = _fmt_rate(ttx_e, tx_total_pkts + ttx_e)
        tx_drop_rate = _fmt_rate(ttx_d, tx_total_pkts + ttx_d)
        rx_err_rate = _fmt_rate(trx_e, rx_total_pkts + trx_e)
        rx_drop_rate = _fmt_rate(trx_d, rx_total_pkts + trx_d)
        out.append(
            f"  *rates:* tx_err={tx_err_rate}  tx_drop={tx_drop_rate}  "
            f"rx_err={rx_err_rate}  rx_drop={rx_drop_rate}"
        )
        out.append("")

    # Diagnostic for non-zero errors/drops.
    flags = []
    for iface in all_ifaces:
        tf = totals.get(iface, {})
        for field, label in [
            ("tx_errors", "TX errors"), ("tx_dropped", "TX drops"),
            ("rx_errors", "RX errors"), ("rx_dropped", "RX drops"),
        ]:
            if tf.get(field, 0) > 0:
                flags.append(f"    [{iface}] {label}: {tf[field]}")
    if flags:
        out.append("### ⚠️  Non-zero error/drop counters")
        out.append("")
        out.extend(flags)
        out.append("")
        out.append(
            "These counters indicate packets lost inside the kernel network "
            "stack.  Drops may be caused by queue overflow (RX buffer full, "
            "TX queue full), no-route (IP layer), or loopback injection "
            "failure.  Errors originate from device-level deferred counters "
            "(e.g. send failures)."
        )
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
    deltas, l2_tx_total, l2_rx_total = _pair_deltas(snapshots, skip_warmup=True)
    totals = _sum_deltas(deltas) if deltas else {}

    app_tx_total = 0
    app_rx_total = 0
    for test_id in ordered:
        ts = stats[test_id]
        for s in ts.measured:
            if test_id in _REVERSE_TEST_IDS:
                app_rx_total += s.app_bytes
            else:
                app_tx_total += s.app_bytes

    # Compute aggregate error/drop totals from all interfaces for context.
    l2_tx_errs = sum(f.get("tx_errors", 0) for f in totals.values())
    l2_tx_drops = sum(f.get("tx_dropped", 0) for f in totals.values())
    l2_rx_errs = sum(f.get("rx_errors", 0) for f in totals.values())
    l2_rx_drops = sum(f.get("rx_dropped", 0) for f in totals.values())

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
                f"  TX  L2={format_bytes(l2_tx_total)}  "
                f"app={format_bytes(app_tx_total)}  "
                f"overhead={overhead}"
            )
        if l2_rx_total > 0:
            if app_rx_total > 0:
                ratio = (l2_rx_total - app_rx_total) / app_rx_total * 100
                overhead = f"{ratio:.1f}%"
            else:
                overhead = "N/A"
            out.append(
                f"  RX  L2={format_bytes(l2_rx_total)}  "
                f"app={format_bytes(app_rx_total)}  "
                f"overhead={overhead}"
            )
        # Error/drop context: non-zero values mean some L2 bytes were dropped
        # before reaching the application, inflating the apparent overhead.
        l2_has_loss = l2_tx_errs or l2_tx_drops or l2_rx_errs or l2_rx_drops
        if l2_has_loss:
            out.append("")
            out.append("  ⚠️  L2 error/drop counters are non-zero — protocol overhead")
            out.append("  may be skewed because dropped packets contribute to L2 byte")
            out.append("  counters but never reach iperf3's application-layer totals.")
            parts = []
            if l2_tx_errs:
                parts.append(f"tx_err={l2_tx_errs}")
            if l2_tx_drops:
                parts.append(f"tx_drop={l2_tx_drops}")
            if l2_rx_errs:
                parts.append(f"rx_err={l2_rx_errs}")
            if l2_rx_drops:
                parts.append(f"rx_drop={l2_rx_drops}")
            out.append(f"  totals: {', '.join(parts)}")
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
        jitter_vals = [
            s.jitter_ms for s in ts.measured if s.jitter_ms is not None
        ]
        if jitter_vals:
            jitter_mean, jitter_std = _mean_std(jitter_vals)
            out.append(f"  udp jitter : {jitter_mean:.3f} +/- {jitter_std:.3f} ms")
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
        jitter_vals = [
            s.jitter_ms for s in ts.measured if s.jitter_ms is not None
        ]
        if jitter_vals:
            jm, js = _mean_std(jitter_vals)
            entry["jitter_ms_mean"], entry["jitter_ms_std"] = jm, js
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
