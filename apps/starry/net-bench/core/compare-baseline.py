#!/usr/bin/env python3
"""
apps/starry/net-bench/compare-baseline.py — 对比 Starry 与 Linux 基线性能

用法:
  python3 compare-baseline.py <starry_summary.txt> <linux_summary.txt>

输出三方对比报告：
  - Starry 吞吐/PPS
  - Linux 基线吞吐/PPS
  - Starry 达到 Linux 的百分比
  - /proc/net/dev L2 计数器对比（含 errors/drops）

对齐 methodology §4.3 "呈现方式" 要求。
"""

import sys
import re
from typing import Dict, Optional, Tuple

from constants import (  # noqa: E402 — shared with summarize.py
    TEST_LABELS,
    TEST_ORDER,
    LABEL_TO_ID,
    NetDevRow,
    format_bytes,
    parse_bytes,
)


def parse_summary(file_path: str) -> Dict[str, Tuple[float, float, str]]:
    """
    Parse summarize.py output into {label: (mean, stddev, unit)}.

    Handles both legacy "±" and current "+/-" separators, and skips
    non-matching header sections (/proc/net/dev, Protocol Overhead,
    CPU Efficiency) that precede the per-test data.
    """
    results = {}

    with open(file_path, 'r', errors="replace") as f:
        content = f.read()

    # Match section headers and throughput lines.
    # Section:  "## TCP 1-stream (uplink)  (test=tcp1)"
    # Metric:   "  throughput : 93.2 +/- 2.4 Mbit/s (n=5, warmup=1)"
    _SECTION_RE = re.compile(r"^##\s+(.+?)\s+\(test=(\S+)\)\s*$")
    _METRIC_RE = re.compile(
        r"^\s+throughput\s*:\s*([\d.]+)\s*(?:±|\+/-)\s*([\d.]+)\s*(\S+/s)"
    )

    current_test_id = None
    for line in content.splitlines():
        sec = _SECTION_RE.match(line)
        if sec:
            current_test_id = sec.group(2)
            label = sec.group(1).strip()
            continue
        if current_test_id is None:
            continue
        m = _METRIC_RE.match(line)
        if m:
            mean = float(m.group(1))
            stddev = float(m.group(2))
            unit = m.group(3)

            # Normalize to Mbit/s or pkt/s
            if 'Gbit' in unit:
                mean *= 1000
                stddev *= 1000
                unit = 'Mbit/s'
            elif 'Kbit' in unit:
                mean /= 1000
                stddev /= 1000
                unit = 'Mbit/s'

            results[current_test_id] = (mean, stddev, unit)
            current_test_id = None  # consume the section

    return results


# ---- /proc/net/dev section parsing -----------------------------------------

# Parses the per-interface per-test markdown table emitted by summarize.py.
# Example row:
#   | TCP 1-stream (uplink) | 193.89 MB |  134668 |  6.94 MB |  134674 | ...
#   | **total** | 655.98 MB |  553251 | 488.13 MB |  651681 |      0 |  ...
#
# Byte columns use format_bytes() output: "193.89 MB", "4.69 KB", "0 B".
# Packet and error columns are plain integers.

# Split a markdown table row on | boundaries (skipping leading/trailing pipes).
_TABLE_COL_RE = re.compile(r"\|\s*([^|]*?)\s*(?=\|)")


def _split_table_columns(line: str) -> list[str]:
    """Extract cell values from a markdown table row."""
    cols: list[str] = []
    for m in _TABLE_COL_RE.finditer(line):
        cols.append(m.group(1).strip())
    return cols


# /proc/net/dev interface sub-section header: "### eth0"
_NETDEV_IFACE_RE = re.compile(r"^###\s+(\S+)")


def parse_netdev_section(file_path: str) -> Dict[str, Dict[str, NetDevRow]]:
    """Parse the /proc/net/dev per-interface table from a summary file.

    Returns ``{iface: {test_id: NetDevRow}}`` where test_id is one of
    ``tcp1``, ``tcp4``, ``tcp1r``, ``udp1g``, ``udp64``, or ``**total**``.
    """
    result: Dict[str, Dict[str, NetDevRow]] = {}
    in_netdev = False
    found_netdev_section = False
    lines_after_iface = 0  # tracks lines consumed after "### iface" heading
    current_iface: Optional[str] = None
    skipped_rows = 0
    parse_errors = 0

    with open(file_path, 'r', errors="replace") as f:
        lines = f.readlines()

    for line in lines:
        # Detect start of netdev section.
        if line.startswith("## /proc/net/dev"):
            in_netdev = True
            found_netdev_section = True
            continue
        if not in_netdev:
            continue
        # Detect end of netdev section (next ## heading that isn't ###).
        if line.startswith("## ") and not line.startswith("### "):
            in_netdev = False
            current_iface = None
            lines_after_iface = 0
            continue

        # Interface sub-section header: "### eth0"
        iface_m = _NETDEV_IFACE_RE.match(line)
        if iface_m:
            current_iface = iface_m.group(1)
            if current_iface not in result:
                result[current_iface] = {}
            # The markdown table emitted by render_netstats() places the
            # two-line preamble (column header + separator) immediately
            # after the ``### iface`` heading with no blank lines.
            lines_after_iface = 0
            continue

        # Skip the two-line markdown table preamble after ``### iface``.
        if current_iface is not None and lines_after_iface < 2:
            lines_after_iface += 1
            continue

        # Parse data rows.
        if not line.startswith("|"):
            continue
        if current_iface is None:
            continue

        cols = _split_table_columns(line)
        if len(cols) != 9:
            skipped_rows += 1
            continue

        raw_label = cols[0].strip()
        # Map display label → short test_id via shared LABEL_TO_ID.
        test_id = LABEL_TO_ID.get(raw_label, raw_label)
        if test_id == "unknown" or test_id.startswith("*"):
            test_id = "**total**" if "total" in raw_label.lower() else raw_label

        try:
            row = NetDevRow(
                test_label=test_id,
                tx_bytes=parse_bytes(cols[1]),
                tx_pkts=int(cols[2]),
                rx_bytes=parse_bytes(cols[3]),
                rx_pkts=int(cols[4]),
                tx_err=int(cols[5]),
                tx_drop=int(cols[6]),
                rx_err=int(cols[7]),
                rx_drop=int(cols[8]),
            )
        except (ValueError, IndexError):
            parse_errors += 1
            continue
        result[current_iface][test_id] = row

    if skipped_rows:
        print(
            f"warning: skipped {skipped_rows} row(s) with unexpected column "
            f"count in /proc/net/dev table of {file_path}",
            file=sys.stderr,
        )
    if parse_errors:
        print(
            f"warning: {parse_errors} parse error(s) in /proc/net/dev table "
            f"of {file_path}",
            file=sys.stderr,
        )
    if found_netdev_section and not result:
        print(
            f"note: /proc/net/dev section found in {file_path} but no "
            f"parseable per-test table rows — summary may use an older format",
            file=sys.stderr,
        )

    return result


def format_bytes(n: int) -> str:
    """Format a byte count in human-readable form."""
    if n >= 1 << 30:
        return f"{n / (1 << 30):.2f} GB"
    if n >= 1 << 20:
        return f"{n / (1 << 20):.2f} MB"
    if n >= 1 << 10:
        return f"{n / (1 << 10):.2f} KB"
    return f"{n} B"


def compute_percentage(starry_val: float, linux_val: float) -> float:
    """计算 Starry 达到 Linux 的百分比"""
    if linux_val == 0:
        return 0.0
    return (starry_val / linux_val) * 100.0


def print_netdev_comparison(
    starry_netdev: Dict[str, Dict[str, "NetDevRow"]],
    linux_netdev: Dict[str, Dict[str, "NetDevRow"]],
):
    """Print /proc/net/dev L2 counter comparison between Starry and Linux."""
    all_test_ids = TEST_ORDER + ["**total**"]

    all_ifaces = sorted(set(starry_netdev.keys()) | set(linux_netdev.keys()))
    if not all_ifaces:
        return

    print("=" * 100)
    print("/proc/net/dev L2 Counter Comparison (Starry vs Linux)")
    print("=" * 100)
    print()

    for iface in all_ifaces:
        s_iface = starry_netdev.get(iface, {})
        l_iface = linux_netdev.get(iface, {})

        print(f"### {iface}")
        print()

        # Header.
        print(f"{'Test':<27} {'':>10} {'Starry':>18} {'Linux':>18} {'S/L':>6}  {'[err/drop]':>15}")
        print("-" * 100)

        for tid in all_test_ids:
            if tid == "**total**" and not (
                tid in s_iface or tid in l_iface
            ):
                continue
            if tid not in s_iface and tid not in l_iface:
                continue

            label = "**TOTAL**" if tid == "**total**" else TEST_LABELS.get(tid, tid)
            s_row = s_iface.get(tid)
            l_row = l_iface.get(tid)

            if tid == "**total**":
                # Summary line: compare aggregate tx/rx bytes + pkt counts.
                if s_row and l_row:
                    # Number of packets (choose direction with more traffic).
                    stx_pkts = s_row.tx_pkts + s_row.rx_pkts
                    ltx_pkts = l_row.tx_pkts + l_row.rx_pkts
                    pkt_pct = compute_percentage(stx_pkts, ltx_pkts)
                    s_total = s_row.tx_bytes + s_row.rx_bytes
                    l_total = l_row.tx_bytes + l_row.rx_bytes
                    byte_pct = compute_percentage(s_total, l_total)
                    s_str = f"{format_bytes(s_total)}"
                    l_str = f"{format_bytes(l_total)}"

                    # Error/drop comparison.
                    s_errs = s_row.tx_err + s_row.tx_drop + s_row.rx_err + s_row.rx_drop
                    l_errs = l_row.tx_err + l_row.tx_drop + l_row.rx_err + l_row.rx_drop
                    err_str = f"S:{s_errs} L:{l_errs}"
                    if s_errs > l_errs:
                        err_str += " ⚠️"
                    elif s_errs == 0 and l_errs == 0:
                        err_str += " ✓"

                    print(
                        f"{label:<27} {'L2 total':>10} {s_str:>18} {l_str:>18} {byte_pct:>5.0f}%  {err_str:>15}"
                    )
                elif s_row:
                    s_total = s_row.tx_bytes + s_row.rx_bytes
                    print(f"{label:<27} {'L2 total':>10} {format_bytes(s_total):>18} {'N/A':>18} {'N/A':>6}")
                elif l_row:
                    l_total = l_row.tx_bytes + l_row.rx_bytes
                    print(f"{label:<27} {'L2 total':>10} {'N/A':>18} {format_bytes(l_total):>18} {'N/A':>6}")
            else:
                # Per-test row: show pkts comparison for the dominant direction.
                if s_row and l_row:
                    s_pkts = max(s_row.tx_pkts, s_row.rx_pkts)
                    l_pkts = max(l_row.tx_pkts, l_row.rx_pkts)
                    if l_pkts > 0:
                        pkt_pct = compute_percentage(s_pkts, l_pkts)
                        pkt_str = f"{pkt_pct:.0f}%"
                    else:
                        pkt_str = "—"
                    s_str = f"{s_pkts} pkts"
                    l_str = f"{l_pkts} pkts"

                    # Error/drop per test.
                    s_errs = s_row.tx_err + s_row.tx_drop + s_row.rx_err + s_row.rx_drop
                    l_errs = l_row.tx_err + l_row.tx_drop + l_row.rx_err + l_row.rx_drop
                    if s_errs > 0 or l_errs > 0:
                        err_str = f"S:{s_errs} L:{l_errs}"
                        if s_errs > l_errs:
                            err_str += " ⚠️"
                    else:
                        err_str = "✓"

                    print(
                        f"{label:<27} {'max pkts':>10} {s_str:>18} {l_str:>18} {pkt_str:>6}  {err_str:>15}"
                    )
                elif s_row:
                    s_pkts = max(s_row.tx_pkts, s_row.rx_pkts)
                    print(f"{label:<27} {'max pkts':>10} {f'{s_pkts} pkts':>18} {'N/A':>18} {'N/A':>6}")
                elif l_row:
                    l_pkts = max(l_row.tx_pkts, l_row.rx_pkts)
                    print(f"{label:<27} {'max pkts':>10} {'N/A':>18} {f'{l_pkts} pkts':>18} {'N/A':>6}")

        print()

        # Error/drop rate summary.
        if "**total**" in s_iface or "**total**" in l_iface:
            s_total = s_iface.get("**total**")
            l_total = l_iface.get("**total**")
            print("  Error/drop rates:")
            for field, label in [
                ("tx_err", "TX errors"), ("tx_drop", "TX drops"),
                ("rx_err", "RX errors"), ("rx_drop", "RX drops"),
            ]:
                s_val = getattr(s_total, field, 0) if s_total else 0
                l_val = getattr(l_total, field, 0) if l_total else 0
                flag = ""
                if s_val > l_val:
                    flag = "  ⚠️  Starry higher"
                elif s_val > 0 and l_val == 0:
                    flag = "  ⚠️  Starry only"
                if s_val > 0 or l_val > 0:
                    print(f"    {label:<10}  Starry={s_val:<6}  Linux={l_val:<6}{flag}")
            print()

    print("-" * 100)
    print()


def print_comparison(starry_results: Dict, linux_results: Dict):
    """打印三方对比表格"""

    print("=" * 100)
    print("Starry vs Linux Baseline Performance Comparison")
    print("=" * 100)
    print()

    # Map test_id to display label (from shared constants module).
    print(f"{'Test':<30} {'Starry':<25} {'Linux Baseline':<25} {'Starry/Linux':<15}")
    print("-" * 100)

    for test_id, label in TEST_LABELS.items():
        starry_data = starry_results.get(test_id)
        linux_data = linux_results.get(test_id)

        if starry_data and linux_data:
            s_mean, s_std, s_unit = starry_data
            l_mean, l_std, l_unit = linux_data
            percentage = compute_percentage(s_mean, l_mean)
            starry_str = f"{s_mean:.1f} +/- {s_std:.1f} {s_unit}"
            linux_str = f"{l_mean:.1f} +/- {l_std:.1f} {l_unit}"
            pct_str = f"{percentage:.1f}%"
            print(f"{label:<30} {starry_str:<25} {linux_str:<25} {pct_str:<15}")
        elif starry_data:
            s_mean, s_std, s_unit = starry_data
            starry_str = f"{s_mean:.1f} +/- {s_std:.1f} {s_unit}"
            print(f"{label:<30} {starry_str:<25} {'N/A':<25} {'N/A':<15}")
        elif linux_data:
            l_mean, l_std, l_unit = linux_data
            linux_str = f"{l_mean:.1f} +/- {l_std:.1f} {l_unit}"
            print(f"{label:<30} {'N/A':<25} {linux_str:<25} {'N/A':<15}")

    print("-" * 100)
    print()

    # 计算平均达成率
    percentages = []
    for test_id in TEST_LABELS:
        starry_data = starry_results.get(test_id)
        linux_data = linux_results.get(test_id)
        if starry_data and linux_data:
            pct = compute_percentage(starry_data[0], linux_data[0])
            percentages.append(pct)

    if percentages:
        avg_pct = sum(percentages) / len(percentages)
        print(f"Average Starry/Linux ratio: {avg_pct:.1f}%")
        print()

    # 关键差距分析
    print("Key Gaps (methodology §6.2):")
    print()

    for test_id, label in TEST_LABELS.items():
        starry_data = starry_results.get(test_id)
        linux_data = linux_results.get(test_id)

        if starry_data and linux_data:
            s_mean = starry_data[0]
            l_mean = linux_data[0]
            percentage = compute_percentage(s_mean, l_mean)

            if percentage < 50:
                gap = l_mean - s_mean
                print(f"  ❌ {label}: Starry {percentage:.1f}% of Linux (gap: {gap:.1f} {starry_data[2]})")
            elif percentage < 80:
                gap = l_mean - s_mean
                print(f"  ⚠️  {label}: Starry {percentage:.1f}% of Linux (gap: {gap:.1f} {starry_data[2]})")
            else:
                print(f"  ✅ {label}: Starry {percentage:.1f}% of Linux")

    print()
    print("=" * 100)


def main():
    if len(sys.argv) != 3:
        print("usage: python3 compare-baseline.py <starry_summary.txt> <linux_summary.txt>", file=sys.stderr)
        sys.exit(1)

    starry_file = sys.argv[1]
    linux_file = sys.argv[2]

    try:
        starry_results = parse_summary(starry_file)
        linux_results = parse_summary(linux_file)
    except FileNotFoundError as e:
        print(f"error: {e}", file=sys.stderr)
        sys.exit(1)
    except Exception as e:
        print(f"error parsing summary files: {e}", file=sys.stderr)
        sys.exit(1)

    if not starry_results:
        print(f"error: no results found in {starry_file}", file=sys.stderr)
        sys.exit(1)

    if not linux_results:
        print(f"error: no results found in {linux_file}", file=sys.stderr)
        sys.exit(1)

    # Throughput comparison.
    print_comparison(starry_results, linux_results)

    # ---- /proc/net/dev comparison ------------------------------------------
    try:
        starry_netdev = parse_netdev_section(starry_file)
        linux_netdev = parse_netdev_section(linux_file)
    except Exception as e:
        print(f"note: could not parse /proc/net/dev section: {e}", file=sys.stderr)
        starry_netdev = {}
        linux_netdev = {}

    if starry_netdev or linux_netdev:
        print_netdev_comparison(starry_netdev, linux_netdev)


if __name__ == '__main__':
    main()
