#!/usr/bin/env python3

import importlib.util
import unittest
from pathlib import Path

MODULE_PATH = Path(__file__).with_name("ci_perf_report.py")
SPEC = importlib.util.spec_from_file_location("ci_perf_report", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
ci_perf_report = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci_perf_report)


class PerformanceReportTests(unittest.TestCase):
    def test_performance_report_renders_supported_axvisor_results(self):
        log_text = "\n".join(
            [
                "[VM 1] VCPU_PERF_SAMPLE index=0 blocks=1099483 "
                "elapsed_ns=3000001123 timer_wakes=3011 checksum=841832",
                "[VM 1] VCPU_PERF_SAMPLE index=1 blocks=1100123 "
                "elapsed_ns=3000000987 timer_wakes=3010 checksum=841890",
                "[VM 1] VCPU_PERF_RESULT blocks_per_second=365334.20 "
                "baseline=364822.00 threshold=328339.80 samples=[364474.50,365334.20]",
                "[VM 1] VCPU_PERF_PASS",
            ]
        )
        report = ci_perf_report.render_report(
            "test-axvisor-self-hosted-board-orangepi-5-plus-vcpu-perf",
            "Board OrangePi 5 Plus · Single ArceOS guest performance",
            log_text,
        )

        self.assertIn("#### vCPU samples (per window)", report)
        self.assertIn(
            "| index | blocks | elapsed_ns | timer_wakes | checksum |", report
        )
        self.assertIn("| 1 | 1100123 | 3000000987 | 3010 | 841890 |", report)
        self.assertIn("#### vCPU throughput result", report)
        self.assertIn(
            "| blocks_per_second | baseline | threshold | samples |", report
        )
        self.assertIn(
            "| 365334.20 | 364822.00 | 328339.80 | [364474.50,365334.20] |", report
        )
        self.assertEqual(
            ci_perf_report.render_benchmarks(log_text),
            [
                {
                    "name": "vcpu-perf/blocks_per_second",
                    "unit": "blocks/s",
                    "value": 365334.20,
                }
            ],
        )

    def test_performance_report_renders_axivc_benchmark_result(self):
        log_text = "\n".join(
            [
                "[test_output] ========================================",
                "[test_output] average sendBandwidth = 2263.10 MB/s, "
                "average receiveBandwidth = 1505.03 MB/s, "
                "testTime = 100, datasize = 262144",
                "[test_output] average sendBandwidth = 2287.42 MB/s, "
                "average receiveBandwidth = 2045.21 MB/s, "
                "testTime = 100, datasize = 1048576",
                "AXVISOR_IVC_BENCH_RESULT=PASS cases=4 testTime=100 "
                "bytes=1232076800 chunks=400",
            ]
        )
        report = ci_perf_report.render_report(
            "test-axvisor-self-hosted-board-orangepi-5-plus-ivc-benchmark",
            "Board OrangePi 5 Plus · AXIVC Zephyr-Starry benchmark",
            log_text,
        )

        self.assertIn("#### AXIVC benchmark per-case bandwidth", report)
        self.assertIn(
            "| datasize | sendBandwidth (MB/s) | receiveBandwidth (MB/s) | testTime |",
            report,
        )
        self.assertIn("| 262144 (256 KiB) | 2263.10 | 1505.03 | 100 |", report)
        self.assertIn("| 1048576 (1 MiB) | 2287.42 | 2045.21 | 100 |", report)
        self.assertIn("#### AXIVC benchmark result", report)
        self.assertIn("| status | cases | testTime | bytes | chunks |", report)
        self.assertIn("| PASS | 4 | 100 | 1232076800 | 400 |", report)
        self.assertEqual(
            ci_perf_report.render_benchmarks(log_text),
            [
                {"name": "ivc-bench/send/256KiB", "unit": "MB/s", "value": 2263.10},
                {"name": "ivc-bench/receive/256KiB", "unit": "MB/s", "value": 1505.03},
                {"name": "ivc-bench/send/1MiB", "unit": "MB/s", "value": 2287.42},
                {"name": "ivc-bench/receive/1MiB", "unit": "MB/s", "value": 2045.21},
            ],
        )

    def test_performance_report_renders_starry_result_lines(self):
        log_text = "\n".join(
            [
                "BLOCK_BENCH_RESULT op=read round=5 mib_s=123.4",
                "LTP_NETSTRESS_RESULT case=tcp-rr median_ms=2",
                'WAKEUP_LATENCY_RESULT {"case":"foo","policy":"other","p50_ns":9}',
                "STARRY_IPERF3_BENCH_RESULT case=T01 direction=tx median_mbps=100.5",
                "uvc-fps: done avg_fps=30 avg_throughput_mib_s=4.5",
            ]
        )
        report = ci_perf_report.render_report(
            "starry-performance",
            "Starry performance",
            log_text,
        )
        self.assertIn("#### Starry benchmark metrics", report)
        self.assertIn("| block-io/read | MiB/s | 123.4 |", report)
        metrics = ci_perf_report.render_benchmarks(log_text)
        self.assertEqual(
            metrics,
            [
                {"name": "block-io/read", "unit": "MiB/s", "value": 123.4},
                {"name": "netstress/tcp-rr", "unit": "ms", "value": 2.0},
                {"name": "wakeup/foo/other/p50", "unit": "ns", "value": 9.0},
                {"name": "iperf3/T01/tx", "unit": "Mbps", "value": 100.5},
                {"name": "uvc/avg_fps", "unit": "frames/s", "value": 30.0},
                {"name": "uvc/avg_throughput_mib_s", "unit": "MiB/s", "value": 4.5},
            ],
        )

    def test_performance_report_renders_task_switch_groups(self):
        lines = ["AXVISOR_TASK_SWITCH_BENCH_BEGIN groups=10"]
        for index in range(10):
            prefix = "[VM 1] " if index % 2 else ""
            lines.append(
                f"{prefix}AXVISOR_TASK_SWITCH_GROUP_SUMMARY index={index} "
                f"samples_per_direction=1000000 avg_cycles={1700 + index} "
                "min_cycles=1632 max_cycles=15879"
            )
        lines.append("AXVISOR_TASK_SWITCH_BENCH_DONE")
        report = ci_perf_report.render_report("task-switch", "Task switch", "\n".join(lines))
        self.assertIn("#### Task switch cycles (per group)", report)
        self.assertIn(
            "| index | samples_per_direction | avg_cycles | min_cycles | max_cycles |",
            report,
        )
        for index in range(10):
            self.assertIn(
                f"| {index} | 1000000 | {1700 + index} | 1632 | 15879 |", report
            )
        self.assertEqual(
            ci_perf_report.render_benchmarks("\n".join(lines)),
            [
                {
                    "name": f"task-switch/avg_cycles/index-{index}",
                    "unit": "cycles",
                    "value": float(1700 + index),
                }
                for index in range(10)
            ],
        )
        with self.assertRaises(ValueError):
            ci_perf_report.render_report(
                "task-switch", "Task switch", "AXVISOR_TASK_SWITCH_BENCH_BEGIN groups=10"
            )

if __name__ == "__main__":
    unittest.main()
