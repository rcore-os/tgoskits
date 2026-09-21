#!/usr/bin/env python3

import argparse
import json
import re
import sys
from pathlib import Path


VCPU_SAMPLE_PATTERN = re.compile(r"VCPU_PERF_SAMPLE\s+(?P<fields>.+)")
VCPU_RESULT_PATTERN = re.compile(r"VCPU_PERF_RESULT\s+(?P<fields>.+)")
TASK_SWITCH_PATTERN = re.compile(r"AXVISOR_TASK_SWITCH_GROUP_SUMMARY\s+(?P<fields>.+)")
IVC_RESULT_PATTERN = re.compile(r"AXVISOR_IVC_BENCH_RESULT=(?P<status>\S+)\s*(?P<fields>.*)")
IVC_CASE_PATTERN = re.compile(
    r"average\s+sendBandwidth\s*=\s*(?P<send>[\d.]+)\s*MB/s\s*,\s*"
    r"average\s+receiveBandwidth\s*=\s*(?P<receive>[\d.]+)\s*MB/s\s*,\s*"
    r"testTime\s*=\s*(?P<test_time>\d+)\s*,\s*"
    r"datasize\s*=\s*(?P<datasize>\d+)"
)
STARRY_SYSBENCH_PATTERN = re.compile(
    r"METRIC\s+(?P<case>\S+)\s+events_per_second\s+(?P<value>[-+]?\d+(?:\.\d+)?)"
)
STARRY_BLOCK_IO_PATTERN = re.compile(
    r"BLOCK_BENCH_RESULT\s+op=(?P<case>\S+).*?mib_s=(?P<value>[-+]?\d+(?:\.\d+)?)"
)
STARRY_BLOCK_RW_PATTERN = re.compile(
    r"block-rw-bench:\s+case=(?P<case>\S+)(?P<fields>.*)"
)
STARRY_COMPILE_PATTERN = re.compile(
    r"COMPILE_SIM_RESULT\s+jobs=(?P<jobs>\d+).*?median_us=(?P<value>\d+)"
)
STARRY_COMPILE_SPEEDUP_PATTERN = re.compile(
    r"COMPILE_SIM_SPEEDUP\s+.*?speedup_milli=(?P<value>\d+)"
)
STARRY_HACKBENCH_PATTERN = re.compile(
    r"LTP_HACKBENCH_RESULT\s+mode=(?P<mode>\S+)\s+cpus=(?P<cpus>\d+).*?median_us=(?P<value>\d+)"
)
STARRY_HACKBENCH_SPEEDUP_PATTERN = re.compile(
    r"LTP_HACKBENCH_SPEEDUP\s+mode=(?P<mode>\S+).*?speedup_milli=(?P<value>\d+)"
)
STARRY_NETSTRESS_PATTERN = re.compile(
    r"LTP_NETSTRESS_RESULT\s+case=(?P<case>\S+)\s+median_ms=(?P<value>[-+]?\d+(?:\.\d+)?)"
)
STARRY_SCHEDULER_PATTERN = re.compile(
    r"(?P<case>kernel_thread_[a-z0-9_]+)\s+p50_ns=(?P<value>\d+)"
)
STARRY_WAKEUP_PATTERN = re.compile(
    r"WAKEUP_LATENCY_RESULT\s+(?P<payload>\{.*\})"
)
STARRY_UVC_PATTERN = re.compile(
    r"UVC_RKNN_BENCH_RESULT\s+(?P<fields>.*)"
)
STARRY_TENNIS_PATTERN = re.compile(
    r"AKARS_TENNIS_BENCH_RESULT\s+(?P<fields>.*)"
)
STARRY_IPERF_PATTERN = re.compile(
    r"STARRY_IPERF3_BENCH_RESULT\s+case=(?P<case>\S+)\s+direction=(?P<direction>\S+)\s+median_mbps=(?P<value>[-+]?\d+(?:\.\d+)?)"
)
STARRY_UVC_FPS_PATTERN = re.compile(r"uvc-fps:\s+done\s+(?P<fields>.*)")
FIELD_PATTERN = re.compile(r"(?P<key>[A-Za-z_][A-Za-z0-9_]*)=(?P<value>\[[^\]]*\]|\S+)")

LINE_PREFIXES = ("[VM 1] ", "[test_output] ")

COLUMN_ORDER = (
    "status",
    "index",
    "samples_per_direction",
    "avg_cycles",
    "min_cycles",
    "max_cycles",
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
    vcpu_samples, vcpu_results, ivc_cases, ivc_results, task_switch_groups = parse_log(
        log_text
    )
    starry_metrics = render_starry_benchmarks(log_text)

    sections = [
        section
        for section in (
            key_value_table("Task switch cycles (per group)", task_switch_groups)
            if task_switch_groups
            else "",
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
            markdown_table(
                "Starry benchmark metrics",
                ["name", "unit", "value"],
                [[str(item["name"]), str(item["unit"]), str(item["value"])] for item in starry_metrics],
            )
            if starry_metrics
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
    list[dict[str, str]],
]:
    vcpu_samples: list[dict[str, str]] = []
    vcpu_results: list[dict[str, str]] = []
    ivc_cases: list[dict[str, str]] = []
    ivc_results: list[dict[str, str]] = []
    task_switch_groups: list[dict[str, str]] = []
    for raw_line in log_text.splitlines():
        line = strip_line_prefixes(raw_line)
        if match := TASK_SWITCH_PATTERN.search(line):
            task_switch_groups.append(parse_fields(match.group("fields")))
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
    return vcpu_samples, vcpu_results, ivc_cases, ivc_results, task_switch_groups


def metric_datasize(datasize: str) -> str:
    return human_datasize(int(datasize)).replace(" ", "")


def render_benchmarks(log_text: str) -> list[dict[str, object]]:
    """Metrics in github-action-benchmark's customBiggerIsBetter JSON format."""
    _, vcpu_results, ivc_cases, _, task_switch_groups = parse_log(log_text)
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
    for group in sorted(task_switch_groups, key=lambda group: int(group["index"])):
        benchmarks.append(
            {
                "name": f"task-switch/avg_cycles/index-{group['index']}",
                "unit": "cycles",
                "value": float(group["avg_cycles"]),
            }
        )
    benchmarks.extend(render_starry_benchmarks(log_text))
    return benchmarks


def _append_metric(
    metrics: list[dict[str, object]], name: str, unit: str, value: str | int | float
) -> None:
    try:
        number = float(value)
    except (TypeError, ValueError):
        return
    if not number == number or number in (float("inf"), float("-inf")):
        return
    metrics.append({"name": name, "unit": unit, "value": number})


def render_starry_benchmarks(log_text: str) -> list[dict[str, object]]:
    """Extract stable result lines emitted by Starry performance apps.

    The app-specific result lines deliberately remain the source of truth; the
    CI layer only maps their numeric fields into the common dashboard format.
    """
    metrics: list[dict[str, object]] = []
    for raw_line in log_text.splitlines():
        line = strip_line_prefixes(raw_line)

        if match := STARRY_SYSBENCH_PATTERN.search(line):
            _append_metric(metrics, f"sysbench/{match['case']}", "events/s", match["value"])

        if match := STARRY_BLOCK_IO_PATTERN.search(line):
            _append_metric(metrics, f"block-io/{match['case']}", "MiB/s", match["value"])

        if match := STARRY_BLOCK_RW_PATTERN.search(line):
            fields = parse_fields(match["fields"])
            case = match["case"]
            for field, unit in (("write_mib_s", "MiB/s"), ("read_mib_s", "MiB/s")):
                if field in fields:
                    _append_metric(metrics, f"block-rw/{case}/{field}", unit, fields[field])
            if "elapsed_ms" in fields:
                _append_metric(metrics, f"block-rw/{case}/elapsed", "ms", fields["elapsed_ms"])

        if match := STARRY_COMPILE_PATTERN.search(line):
            _append_metric(metrics, f"compile-sim/jobs-{match['jobs']}", "us", match["value"])
        if match := STARRY_COMPILE_SPEEDUP_PATTERN.search(line):
            _append_metric(metrics, "compile-sim/speedup", "x1000", match["value"])

        if match := STARRY_HACKBENCH_PATTERN.search(line):
            _append_metric(
                metrics,
                f"hackbench/{match['mode']}/cpus-{match['cpus']}",
                "us",
                match["value"],
            )
        if match := STARRY_HACKBENCH_SPEEDUP_PATTERN.search(line):
            _append_metric(metrics, f"hackbench/{match['mode']}/speedup", "x1000", match["value"])

        if match := STARRY_NETSTRESS_PATTERN.search(line):
            _append_metric(metrics, f"netstress/{match['case']}", "ms", match["value"])

        if match := STARRY_SCHEDULER_PATTERN.search(line):
            _append_metric(metrics, f"scheduler/{match['case']}", "ns", match["value"])

        if match := STARRY_WAKEUP_PATTERN.search(line):
            try:
                payload = json.loads(match["payload"])
            except json.JSONDecodeError:
                payload = {}
            case = payload.get("case")
            policy = payload.get("policy", "default")
            if isinstance(case, str) and isinstance(policy, str):
                for field in ("p50_ns", "p95_ns", "p99_ns", "p999_ns"):
                    if field in payload:
                        _append_metric(
                            metrics,
                            f"wakeup/{case}/{policy}/{field.removesuffix('_ns')}",
                            "ns",
                            payload[field],
                        )

        if match := STARRY_UVC_PATTERN.search(line):
            fields = parse_fields(match["fields"])
            for field, unit in (
                ("capture_fps", "frames/s"),
                ("infer_fps", "inferences/s"),
                ("throughput_mib_s", "MiB/s"),
                ("decode_ms_p50", "ms"),
                ("infer_ms_p50", "ms"),
            ):
                if field in fields:
                    _append_metric(metrics, f"uvc-rknn/{field}", unit, fields[field])

        if match := STARRY_TENNIS_PATTERN.search(line):
            fields = parse_fields(match["fields"])
            for field in ("decode_us_p50", "resize_us_p50", "preprocess_us_p50", "forward_us_p50", "postprocess_us_p50", "total_us_p50"):
                if field in fields:
                    _append_metric(metrics, f"tennis-yolo/{field}", "us", fields[field])

        if match := STARRY_IPERF_PATTERN.search(line):
            _append_metric(
                metrics,
                f"iperf3/{match['case']}/{match['direction']}",
                "Mbps",
                match["value"],
            )

        if match := STARRY_UVC_FPS_PATTERN.search(line):
            fields = parse_fields(match["fields"])
            for field, unit in (("avg_fps", "frames/s"), ("avg_throughput_mib_s", "MiB/s")):
                if field in fields:
                    _append_metric(metrics, f"uvc/{field}", unit, fields[field])
    return metrics


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
