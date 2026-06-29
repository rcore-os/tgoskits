#!/usr/bin/env python3
"""
apps/starry/net-bench/compare-baseline.py — 对比 Starry 与 Linux 基线性能

用法:
  python3 compare-baseline.py <starry_summary.txt> <linux_summary.txt>

输出三方对比报告：
  - Starry 吞吐/PPS
  - Linux 基线吞吐/PPS
  - Starry 达到 Linux 的百分比

对齐 methodology §4.3 "呈现方式" 要求。
"""

import sys
import re
from typing import Dict, Optional, Tuple


def parse_summary(file_path: str) -> Dict[str, Tuple[float, float]]:
    """
    解析 summarize.py 输出的 summary 文件

    返回: {test_id: (mean, stddev)}
    """
    results = {}

    with open(file_path, 'r') as f:
        content = f.read()

    # 匹配格式：TCP 1-stream (uplink):    93.1 ± 2.4 Mbit/s
    # 或：      UDP 64B (PPS):           12345 ± 678 pkt/s
    pattern = r'(\w+(?:\s+\w+)*?)\s*:\s*([\d.]+)\s*±\s*([\d.]+)\s*(\w+/s)'

    for match in re.finditer(pattern, content):
        label = match.group(1).strip()
        mean = float(match.group(2))
        stddev = float(match.group(3))
        unit = match.group(4)

        # 标准化单位到 Mbit/s 或 pkt/s
        if 'Gbit' in unit:
            mean *= 1000
            stddev *= 1000
            unit = 'Mbit/s'
        elif 'Kbit' in unit:
            mean /= 1000
            stddev /= 1000
            unit = 'Mbit/s'

        results[label] = (mean, stddev, unit)

    return results


def compute_percentage(starry_val: float, linux_val: float) -> float:
    """计算 Starry 达到 Linux 的百分比"""
    if linux_val == 0:
        return 0.0
    return (starry_val / linux_val) * 100.0


def print_comparison(starry_results: Dict, linux_results: Dict):
    """打印三方对比表格"""

    print("=" * 100)
    print("Starry vs Linux Baseline Performance Comparison")
    print("=" * 100)
    print()

    # 标准化 test 名称映射
    test_labels = {
        'TCP 1-stream (uplink)': 'tcp1',
        'TCP 4-stream (uplink)': 'tcp4',
        'TCP 1-stream (downlink)': 'tcp1r',
        'UDP 1Gbit target': 'udp1g',
        'UDP 64B (PPS)': 'udp64',
    }

    print(f"{'Test':<30} {'Starry':<25} {'Linux Baseline':<25} {'Starry/Linux':<15}")
    print("-" * 100)

    for label, test_id in test_labels.items():
        starry_data = starry_results.get(label)
        linux_data = linux_results.get(label)

        if starry_data and linux_data:
            s_mean, s_std, s_unit = starry_data
            l_mean, l_std, l_unit = linux_data

            percentage = compute_percentage(s_mean, l_mean)

            starry_str = f"{s_mean:.1f} ± {s_std:.1f} {s_unit}"
            linux_str = f"{l_mean:.1f} ± {l_std:.1f} {l_unit}"
            pct_str = f"{percentage:.1f}%"

            print(f"{label:<30} {starry_str:<25} {linux_str:<25} {pct_str:<15}")
        elif starry_data:
            s_mean, s_std, s_unit = starry_data
            starry_str = f"{s_mean:.1f} ± {s_std:.1f} {s_unit}"
            print(f"{label:<30} {starry_str:<25} {'N/A':<25} {'N/A':<15}")
        elif linux_data:
            l_mean, l_std, l_unit = linux_data
            linux_str = f"{l_mean:.1f} ± {l_std:.1f} {l_unit}"
            print(f"{label:<30} {'N/A':<25} {linux_str:<25} {'N/A':<15}")

    print("-" * 100)
    print()

    # 计算平均达成率
    percentages = []
    for label in test_labels.keys():
        starry_data = starry_results.get(label)
        linux_data = linux_results.get(label)
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

    for label, test_id in test_labels.items():
        starry_data = starry_results.get(label)
        linux_data = linux_results.get(label)

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

    print_comparison(starry_results, linux_results)


if __name__ == '__main__':
    main()
