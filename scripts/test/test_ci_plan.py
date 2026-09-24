#!/usr/bin/env python3

import importlib.util
import re
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path
from typing import Any

MODULE_PATH = Path(__file__).with_name("ci_plan.py")
sys.path.insert(0, str(MODULE_PATH.parent))
SPEC = importlib.util.spec_from_file_location("ci_plan", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
ci_plan = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci_plan)
PERF_REPORT_SPEC = importlib.util.spec_from_file_location(
    "ci_perf_report", MODULE_PATH.with_name("ci_perf_report.py")
)
assert PERF_REPORT_SPEC is not None and PERF_REPORT_SPEC.loader is not None
ci_perf_report = importlib.util.module_from_spec(PERF_REPORT_SPEC)
PERF_REPORT_SPEC.loader.exec_module(ci_perf_report)
PERF_DASHBOARD_SPEC = importlib.util.spec_from_file_location(
    "ci_perf_dashboard", MODULE_PATH.with_name("ci_perf_dashboard.py")
)
assert PERF_DASHBOARD_SPEC is not None and PERF_DASHBOARD_SPEC.loader is not None
ci_perf_dashboard = importlib.util.module_from_spec(PERF_DASHBOARD_SPEC)
PERF_DASHBOARD_SPEC.loader.exec_module(ci_perf_dashboard)

MAIN_TEST_PREFIXES = ("workspace", "arceos", "starry", "axvisor")
MAIN_TEST_GROUPS = ("Workspace", "ArceOS", "Starry", "AxVisor")


def main_test_rows(plan: dict) -> list[dict]:
    return [
        row
        for prefix in MAIN_TEST_PREFIXES
        for row in plan[f"{prefix}_matrix"]["include"]
    ]


def perf_chart_labels(html: str) -> str:
    """Return the concatenated x-axis labels of every chart on the dashboard."""
    return " ".join(
        match.group(1) for match in re.finditer(r'"labels": (\[[^\]]*\])', html)
    )


def perf_table_group(html: str, source: str, prefix: str, unit: str) -> str:
    """Return one chart group's table section ('' if the group is absent)."""
    match = re.search(
        rf'<section class="table-group" data-source="{re.escape(source)}"'
        rf' data-group="{re.escape(prefix)}" data-unit="{re.escape(unit)}"'
        rf"[^>]*>.*?</section>",
        html,
        re.DOTALL,
    )
    return match.group(0) if match else ""


def perf_group_date_body(group: str, date: str) -> str:
    """Return the tbody of one date inside a table group ('' if absent)."""
    match = re.search(
        rf'data-date="{re.escape(date)}"[^>]*>(.*?)</tbody>',
        group,
        re.DOTALL,
    )
    return match.group(1) if match else ""


class CiPlanTests(unittest.TestCase):
    def test_starry_apps_nightly_splits_performance_matrix(self):
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="workflow_dispatch",
        )
        plan = ci_plan.build_starry_apps_plan(context)
        app_ids = {row["id"] for row in plan["starry_apps_matrix"]["include"]}
        performance_ids = {
            row["id"] for row in plan["starry_performance_matrix"]["include"]
        }
        self.assertTrue(app_ids)
        self.assertEqual(
            performance_ids,
            {
                "starry-performance-block-io-x86-64",
                "starry-performance-compile-sim",
                "starry-performance-ltp-hackbench",
                "starry-performance-ltp-netstress",
                "starry-performance-wakeup-latency",
                "starry-performance-sysbench",
            },
        )
        self.assertEqual(
            {
                row["id"]
                for row in plan["starry_board_performance_matrix"]["include"]
            },
            {
                "starry-performance-block-rw-orangepi-5-plus",
                "starry-performance-iperf3-orangepi-5-plus",
                "starry-performance-uvc-orangepi-5-plus",
                "starry-performance-uvc-rknn-orangepi-5-plus",
                "starry-performance-tennis-yolo-aka-00-sg2002",
            },
        )
        self.assertTrue(all(row["download_xtask_bin_artifact"] for row in plan["starry_performance_matrix"]["include"]))

        performance_commands = "\n".join(
            row["command"] for row in plan["starry_performance_matrix"]["include"]
        )
        self.assertIn("-t benchmark/block-io-bench", performance_commands)
        self.assertIn("-t benchmark/wakeup-latency-bench", performance_commands)
        self.assertIn(
            "-t benchmark/qemu/compile-sim-bench", performance_commands
        )
        self.assertIn(
            "--qemu-config qemu-x86_64-benchmark.toml", performance_commands
        )
        self.assertIn("--qemu-config qemu-aarch64-matrix.toml", performance_commands)

        board_commands = "\n".join(
            row["command"]
            for row in plan["starry_board_performance_matrix"]["include"]
        )
        self.assertIn("-t benchmark/block-rw-bench", board_commands)
        self.assertIn("-t benchmark/iperf3", board_commands)
        self.assertIn("-t benchmark/orangepi-5-plus-uvc-rknn", board_commands)
        self.assertIn("--board-config board-orangepi-5-plus.toml", board_commands)
        self.assertIn(
            "--board-config configs/board-orangepi-5-plus-bench.toml", board_commands
        )

    def test_axvisor_nightly_runs_all_registered_checks_with_artifact_producer(self):
        catalog = ci_plan.load_catalog(ci_plan.MAIN_MANIFESTS)
        expected = {check["id"] for check in catalog if check["group"] == "AxVisor"}
        for event in ("schedule", "workflow_dispatch"):
            with self.subTest(event=event):
                context = ci_plan.PlanContext(
                    repository="rcore-os/tgoskits",
                    repository_owner="rcore-os",
                    event_name=event,
                )
                plan = ci_plan.build_axvisor_nightly_plan(context)
                rows = plan["axvisor_matrix"]["include"]
                self.assertEqual({row["id"] for row in rows}, expected)
                self.assertEqual(len(rows), len(expected))
                self.assertTrue(any(
                    "--board orangepi-5-plus-linux --test-case ping" in row["command"]
                    for row in rows
                ))
                producer, = plan["prepare_matrix"]["include"]
                self.assertTrue(producer["upload_xtask_bin_artifact"])
                self.assertEqual(producer["command"], "cargo build -p tg-xtask")
                for row in rows:
                    if row["download_xtask_bin_artifact"]:
                        self.assertEqual(
                            row["xtask_bin_artifact_name"], producer["xtask_bin_artifact_name"]
                        )
                main = ci_plan.build_main_plan(context)
                nightly_ids = {
                    check["id"] for check in catalog if check.get("nightly_only", False)
                }
                self.assertEqual(
                    [row for row in rows if row["id"] not in nightly_ids],
                    main["axvisor_matrix"]["include"],
                )

    def test_axvisor_nightly_maps_only_performance_checks_to_report_rows(self):
        checks = [
            {
                "id": "xtask-producer",
                "name": "producer",
                "group": "Workspace",
                "phase": "static",
                "runs_on": ["ubuntu-latest"],
                "environment": "host",
                "command": "build-xtask",
                "source": "synthetic.toml",
                "upload_xtask_bin_artifact": True,
            },
            {
                "id": "axvisor-performance",
                "name": "performance",
                "group": "AxVisor",
                "phase": "test",
                "runs_on": ["ubuntu-latest"],
                "environment": "host",
                "command": "run-performance",
                "source": "synthetic.toml",
                "performance_report": True,
            },
            {
                "id": "axvisor-functional",
                "name": "functional",
                "group": "AxVisor",
                "phase": "test",
                "runs_on": ["ubuntu-latest"],
                "environment": "host",
                "command": "run-functional",
                "source": "synthetic.toml",
            },
        ]
        context = ci_plan.PlanContext(
            repository="example/tgoskits",
            repository_owner="example",
            event_name="workflow_dispatch",
            include_nightly=True,
        )

        rows = ci_plan._build_axvisor_nightly_plan(checks, context)[
            "axvisor_matrix"
        ]["include"]

        self.assertEqual(
            {row["id"] for row in rows if row["performance_report"]},
            {"axvisor-performance"},
        )
        self.assertFalse(
            next(row for row in rows if row["id"] == "axvisor-functional")[
                "performance_report"
            ]
        )

    def test_main_ci_never_runs_axvisor_nightly_only_cases(self):
        for event in ("pull_request", "push", "workflow_dispatch", "schedule"):
            with self.subTest(event=event):
                context = ci_plan.replace(self.upstream, event_name=event)
                rows = ci_plan.build_main_plan(context)["axvisor_matrix"]["include"]
                commands = "\n".join(row["command"] for row in rows)
                self.assertNotIn("timer-stress", commands)
                self.assertNotIn("ivc-benchmark", commands)
                self.assertNotIn("orangepi-5-plus-vcpu-perf", commands)
                self.assertNotIn("--test-case ping", commands)
                self.assertIn("--board orangepi-5-plus-linux --test-case smoke", commands)
                self.assertNotIn("--board orangepi-5-plus-linux\n", commands)
                self.assertIn("--test-case qemu-ivc", commands)
                self.assertIn("--board orangepi-5-plus-starry", commands)

    def test_benchmark_suite_path_resolves_to_registered_axvisor_check(self) -> None:
        path = (
            "apps/benchmark/axvisor/normal/board-orangepi-5-plus/vcpu-perf/"
            "performance/board-orangepi-5-plus-vcpu-perf.toml"
        )

        selections = ci_plan.resolve_suite_selections(
            ci_plan.WORKSPACE_ROOT,
            ci_plan.load_catalog(ci_plan.MAIN_MANIFESTS),
            [path],
        )

        self.assertEqual(len(selections), 1)
        self.assertEqual(
            selections[0].template_id,
            "test-axvisor-self-hosted-board-orangepi-5-plus-vcpu-perf",
        )
        self.assertEqual(
            selections[0].command,
            "cargo xtask axvisor test board --test-group normal "
            "--test-case performance --board orangepi-5-plus-vcpu-perf",
        )

    def test_nightly_only_suite_changes_keep_static_checks_without_running_board(self):
        for path in (
            "test-suit/axvisor/normal/qemu-timer-stress/gicv3-timer-stress/qemu-aarch64.toml",
            "apps/benchmark/axvisor/normal/board-orangepi-5-plus/ivc-benchmark/benchmark/board-orangepi-5-plus-ivc-benchmark.toml",
            "test-suit/axvisor/normal/board-orangepi-5-plus/pci-network/ping/board-orangepi-5-plus-linux.toml",
            "apps/benchmark/axvisor/normal/board-orangepi-5-plus/vcpu-perf/performance/board-orangepi-5-plus-vcpu-perf.toml",
            "apps/benchmark/axvisor/normal/board-orangepi-5-plus/task-switch-overhead/board-orangepi-5-plus-task-switch-overhead.toml",
            "apps/benchmark/starry/block-rw-bench/board-orangepi-5-plus.toml",
            "apps/benchmark/starry/qemu/ltp-hackbench/qemu-x86_64-benchmark.toml",
        ):
            with self.subTest(path=path):
                context = ci_plan.replace(
                    self.upstream,
                    impact=ci_plan.CiImpact(
                        full=False, reason="fixture", changed_paths=(path,),
                        test_suite_paths=(path,), exclusive=True,
                    ),
                )
                plan = ci_plan.build_main_plan(context)
                self.assertTrue(plan["static_required"])
                self.assertFalse(main_test_rows(plan))
                self.assertFalse(plan["axvisor_required"])
                self.assertFalse(plan["starry_required"])

    def test_benchmark_starry_path_resolves_to_registered_nightly_app_check(self):
        cases = {
            "apps/benchmark/starry/block-rw-bench/board-orangepi-5-plus.toml": (
                "starry-performance-block-rw-orangepi-5-plus",
                "-t benchmark/block-rw-bench",
            ),
            "apps/benchmark/starry/qemu/ltp-hackbench/qemu-x86_64-benchmark.toml": (
                "starry-performance-ltp-hackbench",
                "--qemu-config qemu-x86_64-benchmark.toml",
            ),
            "apps/benchmark/starry/orangepi-5-plus-uvc-rknn/configs/board-orangepi-5-plus-bench.toml": (
                "starry-performance-uvc-rknn-orangepi-5-plus",
                "--board-config configs/board-orangepi-5-plus-bench.toml",
            ),
        }
        for path, (template_id, fragment) in cases.items():
            with self.subTest(path=path):
                selections = ci_plan.resolve_suite_selections(
                    ci_plan.WORKSPACE_ROOT,
                    ci_plan.load_catalog(ci_plan.MAIN_PLAN_MANIFESTS),
                    [path],
                )
                self.assertEqual(len(selections), 1)
                self.assertEqual(selections[0].template_id, template_id)
                self.assertIn(fragment, selections[0].command)

    def test_functional_smoke_path_does_not_route_to_the_nightly_benchmark_check(
        self,
    ):
        path = "apps/starry/qemu/compile-sim-bench/qemu-x86_64.toml"
        context = ci_plan.replace(
            self.upstream,
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=(path,),
                ignored_apps=(path,),
            ),
        )

        plan = ci_plan.build_main_plan(context)

        self.assertEqual(plan["starry_matrix"]["include"], [])
        self.assertFalse(plan["starry_required"])

    def test_axvisor_nightly_rejects_incremental_pr_mode(self):
        with self.assertRaises(ci_plan.PlanError):
            ci_plan.build_axvisor_nightly_plan(self.upstream)

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

    def test_perf_dashboard_groups_by_test_case_with_date_axis_lines(self):
        metrics = [
            {"name": "vcpu-perf/blocks_per_second", "unit": "blocks/s", "value": 368008.45},
            {"name": "ivc-bench/send/256KiB", "unit": "MB/s", "value": 2263.10},
            {"name": "ivc-bench/receive/256KiB", "unit": "MB/s", "value": 1505.03},
        ]
        history = ci_perf_dashboard.update_history([], "2026-09-17", "rev1", metrics)
        partial = [
            {"name": "ivc-bench/send/256KiB", "unit": "MB/s", "value": 2287.42},
        ]
        history = ci_perf_dashboard.update_history(history, "2026-09-18", "rev2", partial)
        # Re-running the same nightly replaces instead of appending.
        history = ci_perf_dashboard.update_history(history, "2026-09-18", "rev2", partial)
        self.assertEqual(
            [entry["date"] for entry in history], ["2026-09-17", "2026-09-18"]
        )

        html = ci_perf_dashboard.render_dashboard("AxVisor Nightly Benchmarks", history)
        self.assertIn("<h2>vcpu-perf</h2>", html)
        self.assertIn(
            '<p class="chart-description">AxVisor 中 ArceOS guest 的 vCPU 工作吞吐量。</p>',
            html,
        )
        # Send and receive bandwidth get separate charts.
        self.assertIn("<h2>ivc-bench/send</h2>", html)
        self.assertIn("<h2>ivc-bench/receive</h2>", html)
        self.assertNotIn("<h2>ivc-bench</h2>", html)
        self.assertIn('"label": "256KiB"', html)
        self.assertIn('"labels": ["2026-09-17", "2026-09-18"]', html)
        self.assertIn('"fill": false', html)
        self.assertNotIn('"fill": true', html)
        self.assertIn('"text": "blocks/s"', html)
        self.assertIn('"text": "MB/s"', html)
        # A metric missing on a later day renders as a gap, not a zero.
        self.assertIn('"data": [368008.45, null]', html)

    def test_perf_dashboard_charts_only_the_most_recent_window(self):
        metrics = [
            {"name": "vcpu-perf/blocks_per_second", "unit": "blocks/s", "value": 1.0},
        ]
        history: list[dict[str, object]] = []
        for day in range(1, 13):
            history = ci_perf_dashboard.update_history(
                history, f"2026-09-{day:02d}", f"rev{day}", metrics
            )

        html = ci_perf_dashboard.render_dashboard("Benchmarks", history)
        labels = perf_chart_labels(html)
        self.assertNotIn('"2026-09-01"', labels)
        self.assertNotIn('"2026-09-02"', labels)
        self.assertIn('"2026-09-03"', labels)
        self.assertIn('"2026-09-12"', labels)
        self.assertIn("showing last 10 of 12 nightly entries", html)
        # The table keeps every date even though the chart only windows ten.
        self.assertIn('data-date="2026-09-01"', html)
        # window=0 charts the full history for manual inspection.
        self.assertIn(
            '"2026-09-01"',
            perf_chart_labels(
                ci_perf_dashboard.render_dashboard("Benchmarks", history, window=0)
            ),
        )

    def test_perf_dashboard_keeps_axvisor_and_starry_sources(self):
        history = ci_perf_dashboard.update_history(
            {},
            "2026-09-20",
            "ax-rev",
            [{"name": "vcpu-perf/blocks", "unit": "blocks/s", "value": 1.0}],
            "axvisor",
        )
        history = ci_perf_dashboard.update_history(
            history,
            "2026-09-20",
            "starry-rev",
            [{"name": "wakeup/p50", "unit": "ns", "value": 2.0}],
            "starry",
        )
        html = ci_perf_dashboard.render_dashboard("Benchmarks", history)
        self.assertIn(
            '<option value="axvisor" selected>AxVisor</option>', html
        )
        self.assertIn('<option value="starry">Starry</option>', html)
        self.assertIn('data-source="axvisor"', html)
        self.assertIn('data-source="starry"', html)
        self.assertIn(
            '<p class="chart-description">StarryOS 回环 TCP/UDP 请求响应耗时。</p>',
            ci_perf_dashboard.render_dashboard(
                "Benchmarks",
                {
                    "starry": [
                        {
                            "date": "2026-09-20",
                            "revision": "rev",
                            "metrics": [
                                {
                                    "name": "netstress/tcp-rr",
                                    "unit": "ms",
                                    "value": 1.0,
                                }
                            ],
                        }
                    ]
                },
            ),
        )

    def test_perf_dashboard_defaults_to_chart_view(self):
        history = [
            {
                "date": "2026-09-20",
                "revision": "rev",
                "metrics": [
                    {"name": "vcpu-perf/blocks", "unit": "blocks/s", "value": 1.0},
                ],
            }
        ]
        html = ci_perf_dashboard.render_dashboard("Benchmarks", history)

        # The selected source panel is visible, the other source hides, and
        # the table view starts hidden while charts are the default.
        self.assertIn(
            '<div class="dashboard-source" data-source="axvisor">',
            html,
        )
        self.assertIn('<div class="chart-view">', html)
        self.assertIn('<div class="table-view" hidden>', html)
        self.assertNotIn(
            '<div class="dashboard-source" data-source="axvisor" hidden>', html
        )
        self.assertIn('<select id="benchmark-source">', html)
        self.assertIn(
            '<button type="button" id="show-charts" aria-pressed="true">Charts</button>',
            html,
        )
        self.assertIn(
            '<button type="button" id="show-table" aria-pressed="false">Table</button>',
            html,
        )
        # The date picker only shows in table view, so it starts hidden.
        self.assertIn(
            '<label id="table-date-label" for="table-date" hidden>Date:',
            html,
        )
        self.assertIn('<select id="table-date"></select>', html)

    def test_perf_dashboard_table_links_source_and_date(self):
        history = ci_perf_dashboard.update_history(
            {},
            "2026-09-18",
            "ax-1",
            [{"name": "vcpu-perf/blocks", "unit": "blocks/s", "value": 1.0}],
            "axvisor",
        )
        history = ci_perf_dashboard.update_history(
            history,
            "2026-09-19",
            "ax-2",
            [{"name": "vcpu-perf/blocks", "unit": "blocks/s", "value": 2.0}],
            "axvisor",
        )
        history = ci_perf_dashboard.update_history(
            history,
            "2026-09-21",
            "starry-1",
            [{"name": "wakeup/p50", "unit": "ns", "value": 5.0}],
            "starry",
        )
        html = ci_perf_dashboard.render_dashboard("Benchmarks", history)

        # Only the default source panel is visible initially; the other hides.
        self.assertIn('<div class="dashboard-source" data-source="axvisor">', html)
        self.assertIn(
            '<div class="dashboard-source" data-source="starry" hidden>', html
        )
        # Every source keeps its own chart group tables and newest date body.
        axvisor = perf_table_group(html, "axvisor", "vcpu-perf", "blocks/s")
        self.assertIn('data-source="axvisor" data-date="2026-09-19">', axvisor)
        self.assertIn('data-source="axvisor" data-date="2026-09-18" hidden>', axvisor)
        self.assertIn(
            "<td>blocks</td><td><code>ax-2</code></td>"
            '<td class="metric-value">2</td><td>blocks/s</td>',
            perf_group_date_body(axvisor, "2026-09-19"),
        )
        starry = perf_table_group(html, "starry", "wakeup", "ns")
        self.assertIn(
            "<td>p50</td><td><code>starry-1</code></td>"
            '<td class="metric-value">5</td><td>ns</td>',
            perf_group_date_body(starry, "2026-09-21"),
        )

    def test_perf_dashboard_table_has_one_table_per_chart_group(self):
        history = [
            {
                "date": "2026-09-19",
                "revision": "rev-1",
                "metrics": [
                    {"name": "vcpu-perf/blocks", "unit": "blocks/s", "value": 1.0},
                    {"name": "ivc-bench/send/256KiB", "unit": "MB/s", "value": 2263.1},
                ],
            },
            {
                "date": "2026-09-20",
                "revision": "rev-2",
                "metrics": [
                    {"name": "vcpu-perf/blocks", "unit": "blocks/s", "value": 2.0},
                    {"name": "ivc-bench/send/256KiB", "unit": "MB/s", "value": 2287.42},
                ],
            },
        ]
        html = ci_perf_dashboard.render_dashboard("Benchmarks", history)

        # Each chart group gets its own titled table, matching the chart.
        self.assertIn("<h3>vcpu-perf</h3>", html)
        self.assertIn("<h3>ivc-bench/send</h3>", html)
        vcpu = perf_table_group(html, "axvisor", "vcpu-perf", "blocks/s")
        send = perf_table_group(html, "axvisor", "ivc-bench/send", "MB/s")
        self.assertIn(
            '<p class="chart-description">'
            "AxVisor 中 ArceOS guest 的 vCPU 工作吞吐量。</p>",
            vcpu,
        )
        self.assertIn(
            '<p class="chart-description">'
            "AxVisor IVC 通道向 guest 发送数据的带宽。</p>",
            send,
        )
        # Exactly one table per group, with per-group columns instead of a
        # shared "Test case" column.
        self.assertEqual(vcpu.count("<table"), 1)
        self.assertEqual(send.count("<table"), 1)
        self.assertIn(
            "<thead><tr><th>Metric</th><th>Revision</th><th>Value</th>"
            "<th>Unit</th></tr></thead>",
            vcpu,
        )
        self.assertNotIn("Test case", html)
        # The two group tables never mix each other's metrics.
        self.assertIn("<td>blocks</td>", vcpu)
        self.assertNotIn("256KiB", vcpu)
        self.assertIn("<td>256KiB</td>", send)
        self.assertNotIn("blocks</td>", send)
        # Both group tables show the newest date and hide the older one.
        self.assertIn('data-date="2026-09-20">', vcpu)
        self.assertIn('data-date="2026-09-19" hidden>', vcpu)

    def test_perf_dashboard_table_isolates_groups_by_unit(self):
        history = [
            {
                "date": "2026-09-20",
                "revision": "rev",
                "metrics": [
                    {"name": "vcpu-perf/blocks", "unit": "blocks/s", "value": 1.0},
                    {"name": "vcpu-perf/cycles", "unit": "cycles", "value": 7.0},
                ],
            }
        ]
        html = ci_perf_dashboard.render_dashboard("Benchmarks", history)

        # The same prefix with a different unit is a separate chart group, so it
        # becomes its own table rather than one merged table.
        blocks = perf_table_group(html, "axvisor", "vcpu-perf", "blocks/s")
        cycles = perf_table_group(html, "axvisor", "vcpu-perf", "cycles")
        self.assertIn("<td>blocks</td>", blocks)
        self.assertNotIn("cycles</td>", blocks)
        self.assertIn("<td>cycles</td>", cycles)
        self.assertNotIn("blocks</td>", cycles)
        self.assertEqual(html.count('class="table-group"'), 2)

    def test_perf_dashboard_table_keeps_same_day_revisions(self):
        history = ci_perf_dashboard.update_history(
            [],
            "2026-09-20",
            "rev-a",
            [
                {"name": "vcpu-perf/blocks", "unit": "blocks/s", "value": 1.0},
                {"name": "vcpu-perf/cycles", "unit": "cycles", "value": 7.0},
            ],
        )
        history = ci_perf_dashboard.update_history(
            history,
            "2026-09-20",
            "rev-b",
            [{"name": "vcpu-perf/blocks", "unit": "blocks/s", "value": 2.0}],
        )
        html = ci_perf_dashboard.render_dashboard("Benchmarks", history)

        body = perf_group_date_body(
            perf_table_group(html, "axvisor", "vcpu-perf", "blocks/s"),
            "2026-09-20",
        )
        # Two runs of the same day keep their own measurements, newer first.
        self.assertIn(
            "<td>blocks</td><td><code>rev-b</code></td>"
            '<td class="metric-value">2</td><td>blocks/s</td>',
            body,
        )
        self.assertIn(
            "<td>blocks</td><td><code>rev-a</code></td>"
            '<td class="metric-value">1</td><td>blocks/s</td>',
            body,
        )
        # The date heading plus one row per run.
        self.assertEqual(body.count("<tr"), 3)
        # rev-b did not measure cycles, so that group only has the rev-a row.
        cycles = perf_group_date_body(
            perf_table_group(html, "axvisor", "vcpu-perf", "cycles"),
            "2026-09-20",
        )
        self.assertIn("<td><code>rev-a</code></td>", cycles)
        self.assertNotIn("<code>rev-b</code>", cycles)

    def test_perf_dashboard_table_hides_group_without_the_date(self):
        history = [
            {
                "date": "2026-09-18",
                "revision": "rev-a",
                "metrics": [
                    {"name": "vcpu-perf/blocks", "unit": "blocks/s", "value": 1.0},
                    {"name": "vcpu-perf/cycles", "unit": "cycles", "value": 2.0},
                ],
            },
            {
                "date": "2026-09-20",
                "revision": "rev-b",
                "metrics": [
                    {"name": "vcpu-perf/blocks", "unit": "blocks/s", "value": 3.0},
                ],
            },
        ]
        html = ci_perf_dashboard.render_dashboard("Benchmarks", history)

        # 2026-09-19 has no data, so no date body exists for it.
        self.assertNotIn('data-date="2026-09-19"', html)
        blocks = perf_table_group(html, "axvisor", "vcpu-perf", "blocks/s")
        cycles = perf_table_group(html, "axvisor", "vcpu-perf", "cycles")
        # blocks measures both dates; the default newest body shows.
        self.assertIn('data-unit="blocks/s">', blocks)
        self.assertIn('data-date="2026-09-20">', blocks)
        self.assertIn("<td>blocks</td>", perf_group_date_body(blocks, "2026-09-20"))
        # cycles has no record on the newest date, so it has no body for it and
        # the whole group is hidden until a date it measured is selected.
        self.assertNotIn('data-date="2026-09-20"', cycles)
        self.assertIn('data-unit="cycles" hidden>', cycles)
        self.assertIn("<td>cycles</td>", perf_group_date_body(cycles, "2026-09-18"))

    def test_perf_dashboard_table_keeps_full_value_precision(self):
        history = [
            {
                "date": "2026-09-20",
                "revision": "rev",
                "metrics": [
                    {"name": "vcpu-perf/decimal", "unit": "ns", "value": 1.23456789},
                    {"name": "vcpu-perf/large", "unit": "ns", "value": 1234567.89},
                    {"name": "vcpu-perf/whole", "unit": "ns", "value": 368008.0},
                ],
            }
        ]
        html = ci_perf_dashboard.render_dashboard("Benchmarks", history)
        body = perf_group_date_body(
            perf_table_group(html, "axvisor", "vcpu-perf", "ns"), "2026-09-20"
        )

        # Long decimals and large magnitudes keep every stored digit.
        self.assertIn('<td class="metric-value">1.23456789</td>', body)
        self.assertIn('<td class="metric-value">1234567.89</td>', body)
        # Integral floats stay in their plain form.
        self.assertIn('<td class="metric-value">368008</td>', body)
        # The previous ':g' formatting truncated to six significant digits.
        self.assertNotIn("1.23457", body)
        self.assertNotIn("1.23457e+06", body)

    def test_perf_dashboard_table_keeps_every_date_beyond_window(self):
        metrics = [
            {"name": "vcpu-perf/blocks", "unit": "blocks/s", "value": 1.0},
        ]
        history: list[dict[str, object]] = []
        for day in range(1, 13):
            history = ci_perf_dashboard.update_history(
                history, f"2026-09-{day:02d}", f"rev{day}", metrics
            )
        html = ci_perf_dashboard.render_dashboard("Benchmarks", history, window=3)

        # The chart keeps only the window, but the table exposes every date.
        labels = perf_chart_labels(html)
        self.assertNotIn('"2026-09-09"', labels)
        self.assertNotIn('"2026-09-01"', labels)
        self.assertIn('"2026-09-12"', labels)
        self.assertIn("showing last 3 of 12 nightly entries", html)
        for day in (1, 2, 5, 10, 12):
            self.assertIn(f'data-date="2026-09-{day:02d}"', html)
        # Newest date is the visible table body, the rest hide.
        self.assertIn(
            '<tbody class="table-date-body" data-source="axvisor" data-date="2026-09-12">',
            html,
        )
        self.assertIn(
            '<tbody class="table-date-body" data-source="axvisor" data-date="2026-09-01" hidden>',
            html,
        )

    def test_perf_dashboard_escapes_dynamic_strings(self):
        source = 'a"<b>&'
        html = ci_perf_dashboard.render_dashboard(
            "<b>t</b>",
            {
                source: [
                    {
                        "date": "2026-09-20",
                        "revision": 'rev</script>"x',
                        "metrics": [
                            {"name": 'm"<x/y&z', "unit": "u<v", "value": 1.0},
                        ],
                    }
                ]
            },
        )

        # The page title is escaped everywhere it appears.
        self.assertIn("&lt;b&gt;t&lt;/b&gt;", html)
        self.assertNotIn("<b>t</b>", html)
        # The source survives as escaped data attribute, option value and text.
        escaped_source = 'a&quot;&lt;b&gt;&amp;'
        self.assertIn(f'data-source="{escaped_source}"', html)
        self.assertIn(f'<option value="{escaped_source}" selected>', html)
        self.assertIn(f">{escaped_source}</option>", html)
        # The chart heading and description never emit raw log text.
        self.assertIn('<h2>m&quot;&lt;x</h2>', html)
        self.assertNotIn('m"<x', html)
        # Table cells, the revision and the unit are escaped too.
        self.assertIn("<td><code>rev&lt;/script&gt;&quot;x</code></td>", html)
        self.assertNotIn("rev</script>", html)
        self.assertIn("y&amp;z", html)
        self.assertIn("u&lt;v", html)

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

    def test_axvisor_nightly_preserves_runner_owner_restrictions(self):
        context = ci_plan.PlanContext(
            repository="example/tgoskits",
            repository_owner="example",
            event_name="workflow_dispatch",
        )
        rows = ci_plan.build_axvisor_nightly_plan(context)["axvisor_matrix"]["include"]
        self.assertTrue(rows)
        self.assertTrue(all("self-hosted" not in row["runs_on"] for row in rows))

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

    def setUp(self) -> None:
        self.upstream = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            base_ref="dev",
        )

    def test_main_plan_has_unique_checks_in_required_groups(
        self,
    ) -> None:
        plan = ci_plan.build_main_plan(self.upstream)

        self.assertTrue(plan["static_required"])
        static_rows = self.assert_unique_ids(plan["static_matrix"]["include"])
        test_rows = self.assert_unique_ids(main_test_rows(plan))
        self.assertNotIn("test_matrix", plan)
        self.assertTrue(static_rows.keys().isdisjoint(test_rows))
        for prefix, group in zip(MAIN_TEST_PREFIXES, MAIN_TEST_GROUPS, strict=True):
            group_rows = plan[f"{prefix}_matrix"]["include"]
            self.assertTrue(plan[f"{prefix}_required"])
            self.assertTrue(group_rows)
            self.assertTrue(all(row["group"] == group for row in group_rows))
        self.assertTrue(
            all(
                not row["name"].startswith(f"{row['group']} / ")
                for row in test_rows.values()
            )
        )

    def test_pull_request_crate_impact_selects_every_check_for_matching_os(
        self,
    ) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            base_ref="dev",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=("os/arceos/modules/axhal/src/lib.rs",),
                changed_packages=("ax-hal",),
                affected_packages=("ax-hal",),
                affected_oses=("arceos",),
                targets=tuple(
                    f"arceos:{arch}"
                    for arch in ("aarch64", "x86_64", "riscv64", "loongarch64")
                ),
            ),
        )

        plan = ci_plan.build_main_plan(context)
        rows = self.assert_unique_ids(main_test_rows(plan))

        self.assert_selects_full_group(rows, "Workspace")
        self.assert_selects_full_group(rows, "ArceOS")
        self.assertEqual(
            {row["group"] for row in rows.values()},
            {"Workspace", "ArceOS"},
        )
        self.assertTrue(plan["workspace_required"])
        self.assertTrue(plan["arceos_required"])
        self.assertFalse(plan["starry_required"])
        self.assertFalse(plan["axvisor_required"])

    def test_pull_request_impact_package_selects_standalone_check(self) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=("bootloader/axloader/src/main.rs",),
                changed_packages=("axloader",),
                affected_packages=("axloader",),
            ),
        )

        plan = ci_plan.build_main_plan(context)
        ids = {row["id"] for row in main_test_rows(plan)}

        self.assertIn("test-axloader-http-smoke", ids)
        self.assertFalse(any(check_id.startswith("test-arceos-") for check_id in ids))
        self.assertFalse(any(check_id.startswith("test-starry-") for check_id in ids))

    def test_incremental_pr_uses_std_since_but_full_pr_does_not(self) -> None:
        incremental = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=("components/shared/src/lib.rs",),
            ),
        )
        full = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            impact=ci_plan.CiImpact.full_selection("fixture"),
        )

        incremental_rows = self.assert_unique_ids(
            main_test_rows(ci_plan.build_main_plan(incremental))
        )
        full_rows = self.assert_unique_ids(
            main_test_rows(ci_plan.build_main_plan(full))
        )

        self.assertIn(
            '--since "$SINCE_REF"', incremental_rows["test-with-std"]["command"]
        )
        self.assertNotIn("--since", full_rows["test-with-std"]["command"])

    def test_app_only_impact_does_not_select_runtime_checks(self) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            base_ref="dev",
            impact=ci_plan.CiImpact(
                full=False,
                reason="only ignored app paths changed",
                changed_paths=("apps/starry/demo/main.c",),
                ignored_apps=("apps/starry/demo/main.c",),
            ),
        )

        plan = ci_plan.build_main_plan(context)
        rows = self.assert_unique_ids(main_test_rows(plan))

        self.assert_selects_full_group(rows, "Workspace")
        self.assertEqual({row["group"] for row in rows.values()}, {"Workspace"})
        self.assertTrue(plan["workspace_required"])
        for prefix in ("arceos", "starry", "axvisor"):
            self.assertFalse(plan[f"{prefix}_required"])
            self.assertEqual(plan[f"{prefix}_matrix"]["include"], [])

    def test_non_pr_events_ignore_impact_and_preserve_full_matrix(self) -> None:
        impact = ci_plan.CiImpact(
            full=False,
            reason="must be ignored outside pull requests",
            changed_paths=("components/axcpu/src/arch/aarch64/mod.rs",),
            targets=("axvisor:aarch64",),
        )
        for event_name in ("push", "workflow_dispatch"):
            with self.subTest(event=event_name):
                baseline = ci_plan.build_main_plan(
                    ci_plan.PlanContext(
                        repository="rcore-os/tgoskits",
                        repository_owner="rcore-os",
                        event_name=event_name,
                    )
                )
                with_impact = ci_plan.build_main_plan(
                    ci_plan.PlanContext(
                        repository="rcore-os/tgoskits",
                        repository_owner="rcore-os",
                        event_name=event_name,
                        impact=impact,
                    )
                )

                self.assertEqual(with_impact, baseline)
                self.assert_unique_ids(main_test_rows(with_impact))

    def test_pure_test_suite_change_runs_only_the_exact_registered_case(self) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            base_ref="dev",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=("test-suit/starryos/qemu/system/qemu-aarch64.toml",),
                test_suite_paths=("test-suit/starryos/qemu/system/qemu-aarch64.toml",),
                exclusive=True,
            ),
        )

        plan = ci_plan.build_main_plan(context)

        self.assertFalse(plan["static_required"])
        self.assertEqual(plan["static_matrix"]["include"], [])
        self.assertFalse(plan["workspace_required"])
        self.assertFalse(plan["arceos_required"])
        self.assertTrue(plan["starry_required"])
        self.assertFalse(plan["axvisor_required"])
        self.assertEqual(
            [row["name"] for row in plan["starry_matrix"]["include"]],
            ["QEMU aarch64 · qemu/system"],
        )
        self.assertEqual(
            plan["starry_matrix"]["include"][0]["command"],
            "cargo xtask starry test qemu --arch aarch64 --test-case qemu/system",
        )

    def test_pure_board_suite_change_runs_only_the_exact_board_case(self) -> None:
        path = (
            "test-suit/starryos/board-orangepi-5-plus/"
            "native-hardware-smoke/board-orangepi-5-plus.toml"
        )
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            base_ref="dev",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=(path,),
                test_suite_paths=(path,),
                exclusive=True,
            ),
        )

        plan = ci_plan.build_main_plan(context)

        self.assertFalse(plan["static_required"])
        self.assertEqual(
            [row["name"] for row in plan["starry_matrix"]["include"]],
            ["Board OrangePi 5 Plus · native-hardware-smoke"],
        )
        self.assertEqual(
            plan["starry_matrix"]["include"][0]["command"],
            "cargo xtask starry test board --test-case native-hardware-smoke "
            "--board orangepi-5-plus",
        )

    def test_cpu_vmx_suite_routes_to_the_registered_cpu_case(self) -> None:
        path = "test-suit/arceos/cpu/guest-entry/qemu-x86_64-vmx.toml"
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            base_ref="dev",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=(path,),
                test_suite_paths=(path,),
                exclusive=True,
            ),
        )
        plan = ci_plan.build_main_plan(context)
        rows = plan["arceos_matrix"]["include"]
        self.assertEqual(len(rows), 1)
        self.assertIn("intel", rows[0]["runs_on"])
        self.assertEqual(
            rows[0]["command"],
            "cargo xtask arceos test qemu --arch x86_64 "
            "--test-group cpu --test-case guest-entry-vmx",
        )

    def test_cpu_pmu_board_routes_to_its_actual_case(self) -> None:
        path = "test-suit/arceos/board-orangepi-5-plus/pmu/board-orangepi-5-plus.toml"
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            base_ref="dev",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=(path,),
                test_suite_paths=(path,),
                exclusive=True,
            ),
        )
        plan = ci_plan.build_main_plan(context)
        rows = plan["arceos_matrix"]["include"]
        self.assertEqual(len(rows), 1)
        self.assertIn("board", rows[0]["runs_on"])
        self.assertEqual(
            rows[0]["command"],
            "cargo xtask arceos test board --test-case pmu --board orangepi-5-plus",
        )

    def test_unregistered_test_suite_fails_planning(self) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=(
                    "test-suit/starryos/board-rock-4d/boot/board-rock-4d.toml",
                ),
                test_suite_paths=(
                    "test-suit/starryos/board-rock-4d/boot/board-rock-4d.toml",
                ),
                exclusive=True,
            ),
        )

        with self.assertRaisesRegex(ci_plan.PlanError, "not registered"):
            ci_plan.build_main_plan(context)

    def test_unsupported_test_group_fails_planning(self) -> None:
        with self.assertRaisesRegex(
            ci_plan.PlanError,
            "unsupported group 'Future OS'",
        ):
            ci_plan._build_test_group_outputs(
                [{"id": "future-os-check", "group": "Future OS"}]
            )

    def test_manifest_rejects_unsupported_test_group(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            manifest = Path(temp_dir) / "future-os.toml"
            manifest.write_text(
                """\
schema_version = 3
phase = "test"
group = "Future OS"

[[check]]
id = "future-os-check"
name = "Future OS check"
command = "true"
""",
                encoding="utf-8",
            )

            with self.assertRaisesRegex(
                ci_plan.PlanError,
                "unsupported test group 'Future OS'",
            ):
                ci_plan._load_manifest(manifest)

    def test_multiple_test_suite_changes_form_a_stable_exact_union(self) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=(
                    "test-suit/axvisor/normal/qemu-acpi/direct-acpi/qemu-x86_64-vmx.toml",
                    "test-suit/starryos/qemu/system/qemu-aarch64.toml",
                ),
                test_suite_paths=(
                    "test-suit/axvisor/normal/qemu-acpi/direct-acpi/qemu-x86_64-vmx.toml",
                    "test-suit/starryos/qemu/system/qemu-aarch64.toml",
                ),
                exclusive=True,
            ),
        )

        plan = ci_plan.build_main_plan(context)
        axvisor_rows = plan["axvisor_matrix"]["include"]
        starry_rows = plan["starry_matrix"]["include"]

        self.assertEqual(
            [row["name"] for row in axvisor_rows],
            ["VMX x86_64 · direct-acpi-vmx"],
        )
        self.assertEqual(
            [row["name"] for row in starry_rows],
            ["QEMU aarch64 · qemu/system"],
        )
        self.assertTrue(
            all(
                not row["download_xtask_bin_artifact"]
                for row in axvisor_rows + starry_rows
            )
        )

    def test_starry_grouped_subcase_runs_only_that_subcase_on_registered_arches(
        self,
    ) -> None:
        path = "test-suit/starryos/qemu/system/test-pivot-root/src/main.c"
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=(path,),
                test_suite_paths=(path,),
                exclusive=True,
            ),
        )

        rows = ci_plan.build_main_plan(context)["starry_matrix"]["include"]

        self.assertEqual(len(rows), 4)
        self.assertEqual(
            {row["name"] for row in rows},
            {
                f"QEMU {arch} · qemu/test-pivot-root"
                for arch in ("aarch64", "loongarch64", "riscv64", "x86_64")
            },
        )
        self.assertTrue(
            all("--test-case qemu/test-pivot-root" in row["command"] for row in rows)
        )

    def test_precise_board_input_does_not_select_same_arch_qemu(self) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=("os/StarryOS/configs/board/visionfive2.toml",),
                input_selections=("starry:board:visionfive2",),
                targets=("starry:riscv64",),
            ),
        )

        ids = {
            row["id"]
            for row in main_test_rows(ci_plan.build_main_plan(context))
        }

        self.assertIn("test-starry-self-hosted-board-visionfive2", ids)
        self.assertNotIn("test-starry-riscv64-qemu", ids)

    def test_suite_plus_os_wide_crate_uses_the_broader_os_checks(self) -> None:
        path = "test-suit/starryos/qemu/system/qemu-aarch64.toml"
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=(path, "components/shared/src/lib.rs"),
                changed_packages=("shared",),
                affected_packages=("shared", "starryos"),
                affected_oses=("starry",),
                test_suite_paths=(path,),
                targets=tuple(
                    f"starry:{arch}"
                    for arch in ("aarch64", "x86_64", "riscv64", "loongarch64")
                ),
            ),
        )

        plan = ci_plan.build_main_plan(context)
        rows = self.assert_unique_ids(main_test_rows(plan))

        self.assertTrue(plan["static_required"])
        self.assert_selects_full_group(rows, "Starry")
        self.assertFalse(any(check_id.startswith("suite-") for check_id in rows))

    def test_unmatched_known_os_input_falls_back_to_that_os(self) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="rcore-os/tgoskits",
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=("os/StarryOS/configs/board/future-board.toml",),
                input_selections=("starry:board:future-board",),
            ),
        )

        plan = ci_plan.build_main_plan(context)
        rows = self.assert_unique_ids(main_test_rows(plan))

        self.assert_selects_full_group(rows, "Starry")
        self.assertFalse(
            any(row["group"] in {"ArceOS", "AxVisor"} for row in rows.values())
        )
        self.assertEqual(plan["arceos_matrix"]["include"], [])
        self.assertEqual(plan["axvisor_matrix"]["include"], [])

    def test_dualguest_robot_board_is_not_scheduled(self) -> None:
        rows = self.assert_unique_ids(
            ci_plan.build_main_plan(self.upstream)["axvisor_matrix"]["include"]
        )
        self.assertNotIn("test-orangepi-5-plus-dualguest-robot", rows)
        nightly_rows = self.assert_unique_ids(
            ci_plan.build_axvisor_nightly_plan(
                ci_plan.replace(self.upstream, event_name="schedule")
            )["axvisor_matrix"]["include"]
        )
        self.assertNotIn("test-orangepi-5-plus-dualguest-robot", nightly_rows)
        self.assertIn(
            "test-axvisor-self-hosted-board-orangepi-5-plus-ivc-benchmark", nightly_rows
        )

    def test_dualguest_robot_board_markers_cannot_match_command_echo(self) -> None:
        root = MODULE_PATH.parents[2]
        configs = (
            root
            / "test-suit/axvisor/normal/board-orangepi-5-plus/dual-linux-zephyr"
            / "board-orangepi-5-plus-dualguest-robot.toml",
            root
            / "test-suit/axvisor/normal/board-orangepi-5-plus/dual-starry-zephyr"
            / "board-orangepi-5-plus-dualguest-robot.toml",
        )

        for path in configs:
            with self.subTest(config=path):
                config = tomllib.loads(path.read_text())
                self.assertEqual(
                    config["board_type"], "OrangePi-5-Plus-DualGuest-robot"
                )
                step = config["shell_check_steps"][-1]
                for pattern in step["success_regex"] + step["fail_regex"]:
                    self.assertIsNone(re.search(pattern, step["shell_cmd"]))
                guest = "linux-zephyr" if "dual-linux" in str(path) else "starry-zephyr"
                self.assertTrue(any(re.search(pattern, f"DUAL_PICK_CI_PASS guest={guest}\n")
                                    for pattern in step["success_regex"]))
                self.assertTrue(any(re.search(pattern, f"DUAL_PICK_CI_FAIL guest={guest} status=1\n")
                                    for pattern in step["fail_regex"]))

    def test_fork_repository_filters_owner_checks_and_falls_back_from_qcs(
        self,
    ) -> None:
        context = ci_plan.PlanContext(
            repository="contributor/tgoskits",
            repository_owner="contributor",
            event_name="push",
        )
        plan = ci_plan.build_main_plan(context)
        static_rows = self.assert_unique_ids(plan["static_matrix"]["include"])
        test_rows = self.assert_unique_ids(main_test_rows(plan))

        self.assertTrue(
            {
                "run-clippy",
                "test-with-std",
                "test-arceos-aarch64-qemu-app-suites",
                "test-axvisor-aarch64-qemu-http-control-plane-browser-console-ivc",
                "test-starry-aarch64-qemu",
            }.issubset(test_rows)
        )
        self.assertFalse(any("board" in row["id"] for row in test_rows.values()))
        self.assertTrue(
            all(
                row["runs_on"] == ["ubuntu-latest"]
                for row in (*static_rows.values(), *test_rows.values())
            )
        )
        self.assertEqual(static_rows["check-formatting"]["runs_on"], ["ubuntu-latest"])
        self.assertEqual(
            static_rows["check-formatting"]["container_image"],
            "ghcr.io/contributor/tgoskits-container:latest",
        )
        self.assertFalse(static_rows["check-formatting"]["download_xtask_bin_artifact"])
        clippy = test_rows["run-clippy"]
        self.assertEqual(clippy["runs_on"], ["ubuntu-latest"])
        self.assertEqual(clippy["fetch_depth"], "100")
        self.assertTrue(clippy["download_xtask_bin_artifact"])

    def test_fork_pull_request_never_allocates_self_hosted_runners(self) -> None:
        context = ci_plan.PlanContext(
            repository="rcore-os/tgoskits",
            repository_owner="rcore-os",
            event_name="pull_request",
            head_repository="contributor/tgoskits",
            base_ref="dev",
        )

        plan = ci_plan.build_main_plan(context)
        rows = self.assert_unique_ids(
            plan["static_matrix"]["include"] + main_test_rows(plan)
        )

        self.assertTrue(rows)
        self.assertTrue(
            all("self-hosted" not in row["runs_on"] for row in rows.values())
        )
        catalog = ci_plan.load_catalog(ci_plan.MAIN_MANIFESTS)
        self_hosted_only_ids = {
            check["id"]
            for check in catalog
            if "self-hosted" in check["runs_on"]
            and "fallback_environment" not in check
        }
        self.assertTrue(self_hosted_only_ids.isdisjoint(rows))
        self.assertEqual(rows["check-formatting"]["runs_on"], ["ubuntu-latest"])
        self.assertEqual(rows["run-clippy"]["runs_on"], ["ubuntu-latest"])

        board_path = (
            "test-suit/arceos/board-orangepi-5-plus/pmu/"
            "board-orangepi-5-plus.toml"
        )
        board_only = ci_plan.replace(
            context,
            impact=ci_plan.CiImpact(
                full=False,
                reason="fixture",
                changed_paths=(board_path,),
                test_suite_paths=(board_path,),
                exclusive=True,
            ),
        )
        board_plan = ci_plan.build_main_plan(board_only)
        board_rows = board_plan["static_matrix"]["include"] + main_test_rows(
            board_plan
        )
        self.assertTrue(board_rows)
        self.assertTrue(
            all("self-hosted" not in row["runs_on"] for row in board_rows)
        )

    def test_same_repository_pull_request_keeps_self_hosted_runners(self) -> None:
        plan = ci_plan.build_main_plan(self.upstream)
        rows = self.assert_unique_ids(
            plan["static_matrix"]["include"] + main_test_rows(plan)
        )

        self.assertTrue(any("self-hosted" in row["runs_on"] for row in rows.values()))

    def test_event_and_boolean_input_select_checks_independently(self) -> None:
        check = {"events": ["schedule"], "enable_boolean_input": "run_optional"}
        for event, enabled, expected in (
            ("schedule", frozenset(), True),
            ("workflow_dispatch", frozenset(), False),
            ("workflow_dispatch", frozenset({"run_optional"}), True),
            ("workflow_dispatch", frozenset({"other_input"}), False),
        ):
            with self.subTest(event=event, enabled=enabled):
                context = ci_plan.PlanContext(
                    repository="example/project",
                    repository_owner="example",
                    event_name=event,
                    enabled_boolean_inputs=enabled,
                )
                self.assertEqual(ci_plan._is_enabled(check, context), expected)

    def assert_unique_ids(
        self, rows: list[dict[str, Any]]
    ) -> dict[str, dict[str, Any]]:
        ids = [row["id"] for row in rows]
        self.assertEqual(
            len(ids),
            len(set(ids)),
            f"matrix check IDs must be unique: {ids}",
        )
        return {row["id"]: row for row in rows}

    def assert_selects_full_group(
        self, rows: dict[str, dict[str, Any]], group: str
    ) -> None:
        full_rows = self.assert_unique_ids(
            main_test_rows(ci_plan.build_main_plan(self.upstream))
        )
        selected_ids = {
            check_id for check_id, row in rows.items() if row["group"] == group
        }
        full_ids = {
            check_id for check_id, row in full_rows.items() if row["group"] == group
        }
        self.assertEqual(selected_ids, full_ids)


if __name__ == "__main__":
    unittest.main()
