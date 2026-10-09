#!/usr/bin/env python3

import importlib.util
import re
import unittest
from pathlib import Path

MODULE_PATH = Path(__file__).with_name("ci_perf_dashboard.py")
SPEC = importlib.util.spec_from_file_location("ci_perf_dashboard", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
ci_perf_dashboard = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci_perf_dashboard)


def perf_chart_labels(html: str) -> str:
    """Return concatenated x-axis labels from every rendered chart."""
    return " ".join(
        match.group(1) for match in re.finditer(r'"labels": (\[[^\]]*\])', html)
    )


def perf_table_group(html: str, source: str, prefix: str, unit: str) -> str:
    """Return one chart group's table section ('' if absent)."""
    match = re.search(
        rf'<section class="table-group" data-source="{re.escape(source)}"'
        rf' data-group="{re.escape(prefix)}" data-unit="{re.escape(unit)}"'
        rf"[^>]*>.*?</section>",
        html, re.DOTALL,
    )
    return match.group(0) if match else ""


def perf_group_date_body(group: str, date: str) -> str:
    """Return tbody for one date inside a table group ('' if absent)."""
    match = re.search(rf'data-date="{re.escape(date)}"[^>]*>(.*?)</tbody>', group, re.DOTALL)
    return match.group(1) if match else ""


class PerformanceDashboardTests(unittest.TestCase):
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
    def test_perf_dashboard_bootstraps_legacy_axvisor_history_for_starry(self):
        legacy_entry = {
            "date": "2026-09-16",
            "revision": "legacy-rev",
            "metrics": [
                {"name": "vcpu-perf/blocks", "unit": "blocks/s", "value": 1.0},
            ],
        }
        starry_metrics = [
            {"name": "wakeup/p50", "unit": "ns", "value": 2.0},
        ]

        history = ci_perf_dashboard.update_history(
            [legacy_entry],
            "2026-09-20",
            "starry-rev",
            starry_metrics,
            "starry",
        )

        self.assertIsInstance(history, dict)
        self.assertEqual(
            history["axvisor"],
            [
                {
                    "date": "2026-09-16",
                    "revision": "legacy-rev",
                    "metrics": [
                        {
                            "name": "vcpu-perf/blocks",
                            "unit": "blocks/s",
                            "value": 1.0,
                        },
                    ],
                }
            ],
        )
        self.assertEqual(
            history["starry"],
            [
                {
                    "date": "2026-09-20",
                    "revision": "starry-rev",
                    "metrics": starry_metrics,
                }
            ],
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


if __name__ == "__main__":
    unittest.main()
