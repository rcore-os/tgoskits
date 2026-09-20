#!/usr/bin/env python3

import argparse
import json
import re
import sys
from pathlib import Path


VCPU_SAMPLE_PATTERN = re.compile(r"VCPU_PERF_SAMPLE\s+(?P<fields>.+)")
VCPU_RESULT_PATTERN = re.compile(r"VCPU_PERF_RESULT\s+(?P<fields>.+)")
IVC_RESULT_PATTERN = re.compile(r"AXVISOR_IVC_BENCH_RESULT=(?P<status>\S+)\s*(?P<fields>.*)")
IVC_CASE_PATTERN = re.compile(
    r"average\s+sendBandwidth\s*=\s*(?P<send>[\d.]+)\s*MB/s\s*,\s*"
    r"average\s+receiveBandwidth\s*=\s*(?P<receive>[\d.]+)\s*MB/s\s*,\s*"
    r"testTime\s*=\s*(?P<test_time>\d+)\s*,\s*"
    r"datasize\s*=\s*(?P<datasize>\d+)"
)
FIELD_PATTERN = re.compile(r"(?P<key>[A-Za-z_][A-Za-z0-9_]*)=(?P<value>\[[^\]]*\]|\S+)")

LINE_PREFIXES = ("[VM 1] ", "[test_output] ")

COLUMN_ORDER = (
    "status",
    "index",
    "blocks",
    "elapsed_ns",
    "timer_wakes",
    "checksum",
    "blocks_per_second",
    "baseline",
    "threshold",
    "samples",
    "cases",
    "testTime",
    "bytes",
    "chunks",
)


def parse_fields(text: str) -> dict[str, str]:
    return {match.group("key"): match.group("value") for match in FIELD_PATTERN.finditer(text)}


def strip_line_prefixes(line: str) -> str:
    for prefix in LINE_PREFIXES:
        line = line.removeprefix(prefix)
    return line.strip()


def markdown_table(title: str, headers: list[str], rows: list[list[str]]) -> str:
    lines = [
        f"#### {title}",
        "",
        f"| {' | '.join(headers)} |",
        f"| {' | '.join('---' for _ in headers)} |",
    ]
    for row in rows:
        lines.append(f"| {' | '.join(row)} |")
    return "\n".join(lines)


def key_value_table(title: str, rows: list[dict[str, str]]) -> str:
    keys = [key for key in COLUMN_ORDER if any(key in row for row in rows)]
    keys.extend(sorted({key for row in rows for key in row} - set(keys)))
    return markdown_table(title, keys, [[row.get(key, "") for key in keys] for row in rows])


def human_datasize(value: int) -> str:
    for unit, factor in (("MiB", 1 << 20), ("KiB", 1 << 10)):
        if value >= factor and value % factor == 0:
            return f"{value // factor} {unit}"
    return str(value)


def ivc_case_table(cases: list[dict[str, str]]) -> str:
    headers = ["datasize", "sendBandwidth (MB/s)", "receiveBandwidth (MB/s)", "testTime"]
    rows = [
        [
            f"{case['datasize']} ({human_datasize(int(case['datasize']))})",
            case["send"],
            case["receive"],
            case["test_time"],
        ]
        for case in sorted(cases, key=lambda case: int(case["datasize"]))
    ]
    return markdown_table("AXIVC benchmark per-case bandwidth", headers, rows)


def render_report(check_id: str, check_name: str, log_text: str) -> str:
    vcpu_samples, vcpu_results, ivc_cases, ivc_results = parse_log(log_text)

    sections = [
        section
        for section in (
            key_value_table("vCPU samples (per window)", vcpu_samples)
            if vcpu_samples
            else "",
            key_value_table("vCPU throughput result", vcpu_results)
            if vcpu_results
            else "",
            ivc_case_table(ivc_cases) if ivc_cases else "",
            key_value_table("AXIVC benchmark result", ivc_results)
            if ivc_results
            else "",
        )
        if section
    ]
    if not sections:
        raise ValueError("no supported performance result lines found")

    return "\n".join(
        [
            f"### {check_name}",
            "",
            f"`{check_id}`",
            "",
            "\n\n".join(sections),
            "",
        ]
    )


def parse_log(log_text: str) -> tuple[
    list[dict[str, str]], list[dict[str, str]],
    list[dict[str, str]], list[dict[str, str]],
]:
    vcpu_samples: list[dict[str, str]] = []
    vcpu_results: list[dict[str, str]] = []
    ivc_cases: list[dict[str, str]] = []
    ivc_results: list[dict[str, str]] = []
    for raw_line in log_text.splitlines():
        line = strip_line_prefixes(raw_line)
        if match := VCPU_SAMPLE_PATTERN.search(line):
            vcpu_samples.append(parse_fields(match.group("fields")))
        if match := VCPU_RESULT_PATTERN.search(line):
            vcpu_results.append(parse_fields(match.group("fields")))
        if match := IVC_CASE_PATTERN.search(line):
            ivc_cases.append(
                {
                    "datasize": match.group("datasize"),
                    "send": match.group("send"),
                    "receive": match.group("receive"),
                    "test_time": match.group("test_time"),
                }
            )
        if match := IVC_RESULT_PATTERN.search(line):
            fields = {"status": match.group("status")}
            fields.update(parse_fields(match.group("fields")))
            ivc_results.append(fields)
    return vcpu_samples, vcpu_results, ivc_cases, ivc_results


def metric_datasize(datasize: str) -> str:
    return human_datasize(int(datasize)).replace(" ", "")


def render_benchmarks(log_text: str) -> list[dict[str, object]]:
    """Metrics in github-action-benchmark's customBiggerIsBetter JSON format."""
    _, vcpu_results, ivc_cases, _ = parse_log(log_text)
    benchmarks: list[dict[str, object]] = []
    for result in vcpu_results:
        if "blocks_per_second" in result:
            benchmarks.append(
                {
                    "name": "vcpu-perf/blocks_per_second",
                    "unit": "blocks/s",
                    "value": float(result["blocks_per_second"]),
                }
            )
    for case in sorted(ivc_cases, key=lambda case: int(case["datasize"])):
        label = metric_datasize(case["datasize"])
        benchmarks.append(
            {"name": f"ivc-bench/send/{label}", "unit": "MB/s", "value": float(case["send"])}
        )
        benchmarks.append(
            {
                "name": f"ivc-bench/receive/{label}",
                "unit": "MB/s",
                "value": float(case["receive"]),
            }
        )
    return benchmarks


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Render CI performance results")
    parser.add_argument("--check-id", required=True)
    parser.add_argument("--check-name", required=True)
    parser.add_argument("--log", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--json-output",
        type=Path,
        help="Write github-action-benchmark metrics to this JSON file",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        log_text = args.log.read_text(encoding="utf-8", errors="replace")
        report = render_report(args.check_id, args.check_name, log_text)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(report, encoding="utf-8")
        if args.json_output is not None:
            benchmarks = render_benchmarks(log_text)
            args.json_output.parent.mkdir(parents=True, exist_ok=True)
            args.json_output.write_text(
                json.dumps(benchmarks, indent=2), encoding="utf-8"
            )
    except (OSError, ValueError) as error:
        print(f"performance report failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
