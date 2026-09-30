#!/usr/bin/env python3

"""Accumulate nightly benchmark history and render a static Chart.js dashboard.

The dashboard groups metrics by test case (the prefix of the metric name),
uses the nightly date as the x-axis, and draws plain lines without filling.
Beside the chart view it offers a table view that shows every measurement of a
single nightly date; the table is plain HTML, so it stays usable even when the
Chart.js CDN is unreachable.
"""

import argparse
import html
import json
import sys
from pathlib import Path

COLORS = (
    "#2563eb",
    "#dc2626",
    "#16a34a",
    "#9333ea",
    "#ea580c",
    "#0891b2",
    "#be185d",
    "#65a30d",
    "#475569",
    "#ca8a04",
)
CHART_JS = "https://cdn.jsdelivr.net/npm/chart.js@4.4.9/dist/chart.umd.min.js"

CHART_DESCRIPTIONS = {
    "axvisor": {
        "vcpu-perf": "AxVisor 中 ArceOS guest 的 vCPU 工作吞吐量。",
        "ivc-bench/send": "AxVisor IVC 通道向 guest 发送数据的带宽。",
        "ivc-bench/receive": "AxVisor IVC 通道从 guest 接收数据的带宽。",
        "task-switch/avg_cycles": "AxVisor 任务切换的平均 CPU cycles。",
    },
    "starry": {
        "sysbench": "StarryOS 的 CPU、线程同步和内存工作负载吞吐量。",
        "block-io": "StarryOS 文件系统块设备的读写与 fsync 吞吐量。",
        "block-rw": "StarryOS 不同 I/O 大小及多任务并发读写性能。",
        "compile-sim": "模拟多进程编译依赖图，比较不同并行度下的构建耗时。",
        "hackbench": "调度器、进程/线程和 pipe IPC 工作负载的耗时。",
        "netstress": "StarryOS 回环 TCP/UDP 请求响应耗时。",
        "wakeup": "futex、timer 和调度 yield 的唤醒延迟分布。",
        "scheduler": "StarryOS 内核线程创建、唤醒和线程切换延迟。",
        "iperf3": "OrangePi 上 StarryOS 真实网络链路的 TCP 吞吐量。",
        "uvc": "StarryOS UVC 摄像头采集帧率和数据吞吐量。",
        "uvc-rknn": "UVC 采集与 RKNN 推理流水线的帧率、吞吐量和延迟。",
        "tennis-yolo": "StarryOS TPU/YOLO 固定图片推理的分阶段耗时。",
    },
}


def load_metrics(path: Path) -> list[dict[str, object]]:
    metrics = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(metrics, list) or not metrics:
        raise ValueError(f"{path} must contain a non-empty metrics array")
    for metric in metrics:
        name = metric.get("name")
        unit = metric.get("unit")
        value = metric.get("value")
        if not isinstance(name, str) or not name or not isinstance(unit, str):
            raise ValueError(f"invalid metric entry in {path}: {metric!r}")
        if not isinstance(value, (int, float)) or isinstance(value, bool):
            raise ValueError(f"metric '{name}' has a non-numeric value: {value!r}")
    return metrics


def load_history(path: Path) -> dict[str, list[dict[str, object]]]:
    if not path.exists():
        return {}
    history = json.loads(path.read_text(encoding="utf-8"))
    if isinstance(history, list):
        # The original dashboard stored only AxVisor history. Keep old
        # perf-data branches readable when the first Starry update lands.
        return {"axvisor": history}
    if not isinstance(history, dict):
        raise ValueError(f"{path} must contain a source history object")
    for source, entries in history.items():
        if not isinstance(source, str) or not isinstance(entries, list):
            raise ValueError(f"invalid history source in {path}: {source!r}")
    return history


def _update_source_history(
    history: list[dict[str, object]],
    date: str,
    revision: str,
    metrics: list[dict[str, object]],
) -> list[dict[str, object]]:
    entry = {"date": date, "revision": revision, "metrics": metrics}
    # Re-running the same nightly (same date and revision) replaces its entry.
    updated = [
        item
        for item in history
        if (item.get("date"), item.get("revision")) != (date, revision)
    ]
    updated.append(entry)
    updated.sort(key=lambda item: (str(item.get("date", "")), str(item.get("revision", ""))))
    return updated


def update_history(
    history: list[dict[str, object]] | dict[str, list[dict[str, object]]],
    date: str,
    revision: str,
    metrics: list[dict[str, object]],
    source: str | None = None,
) -> list[dict[str, object]] | dict[str, list[dict[str, object]]]:
    """Update one source while retaining the other source's history.

    ``source=None`` preserves the old list-in/list-out API used by callers
    that render a single dashboard in isolation.
    """
    if source is None:
        if not isinstance(history, list):
            raise ValueError("source is required for multi-source history")
        return _update_source_history(history, date, revision, metrics)
    if not source:
        raise ValueError("source must not be empty")
    if isinstance(history, list):
        history = {"axvisor": history}
    updated = dict(history)
    updated[source] = _update_source_history(
        list(updated.get(source, [])), date, revision, metrics
    )
    return updated


def entry_value(entry: dict[str, object], name: str) -> float | None:
    for metric in entry["metrics"]:
        if metric["name"] == name:
            return metric["value"]
    return None


def collect_groups(
    history: list[dict[str, object]],
) -> dict[tuple[str, str], list[str]]:
    # The last path segment of a metric name is the series label; everything
    # before it is the chart group (e.g. "ivc-bench/send" vs "ivc-bench/receive").
    groups: dict[tuple[str, str], list[str]] = {}
    for entry in history:
        for metric in entry["metrics"]:
            name = str(metric["name"])
            group = name.rpartition("/")[0] or name
            key = (group, str(metric["unit"]))
            groups.setdefault(key, [])
            if metric["name"] not in groups[key]:
                groups[key].append(name)
    return groups


def chart_description(source: str, prefix: str) -> str:
    descriptions = CHART_DESCRIPTIONS.get(source, {})
    if prefix in descriptions:
        return descriptions[prefix]
    root = prefix.partition("/")[0]
    return descriptions.get(root, "该图表展示此性能测例的 nightly 测量结果。")


def render_chart_section(
    index: int,
    source: str,
    prefix: str,
    unit: str,
    names: list[str],
    history: list[dict[str, object]],
) -> str:
    labels = [str(entry["date"]) for entry in history]
    datasets = []
    for position, name in enumerate(names):
        color = COLORS[position % len(COLORS)]
        datasets.append(
            {
                "label": name.rpartition("/")[2] or name,
                "data": [entry_value(entry, name) for entry in history],
                "borderColor": color,
                "backgroundColor": color,
                "fill": False,
                "tension": 0.15,
                "pointRadius": 3,
                "spanGaps": True,
            }
        )
    config = {
        "type": "line",
        "data": {"labels": labels, "datasets": datasets},
        "options": {
            "responsive": True,
            "maintainAspectRatio": False,
            "scales": {"y": {"title": {"display": True, "text": unit}}},
            "plugins": {"legend": {"position": "bottom"}},
        },
    }
    payload = json.dumps(config).replace("</", "<\\/")
    return f"""<section>
<h2>{html.escape(prefix)}</h2>
<p class="chart-description">{html.escape(chart_description(source, prefix))}</p>
<div class="chart-wrap"><canvas id="chart-{index}"></canvas></div>
<script>
new Chart(document.getElementById('chart-{index}'), {payload});
</script>
</section>"""


def _source_title(source: str) -> str:
    return {"axvisor": "AxVisor", "starry": "Starry"}.get(source, source)


def _history_by_date(
    history: list[dict[str, object]],
) -> dict[str, list[dict[str, object]]]:
    """Group history entries by their nightly date, keeping every revision.

    Entries are bucketed by the ``date`` field; each bucket keeps all runs of
    that date (one per revision) sorted by revision so the table shows the most
    recent revision first.
    """
    grouped: dict[str, list[dict[str, object]]] = {}
    for entry in history:
        date = str(entry["date"])
        grouped.setdefault(date, []).append(entry)
    for date, entries in grouped.items():
        entries.sort(key=lambda item: str(item.get("revision", "")), reverse=True)
    return grouped


def _sorted_dates(by_date: dict[str, list[dict[str, object]]]) -> list[str]:
    # Newest calendar date first; the table itself keeps each date's runs.
    return sorted(by_date, reverse=True)


def _chart_groups(history: list[dict[str, object]]) -> list[tuple[str, str, list[str]]]:
    """Return ``(prefix, unit, names)`` sections in chart order.

    The order matches ``collect_groups`` (first appearance in the charted
    history), so the chart headings stay exactly as before.
    """
    return [
        (prefix, unit, list(names))
        for (prefix, unit), names in collect_groups(history).items()
    ]


def format_metric_value(value: int | float) -> str:
    """Render a metric value without losing stored precision.

    ``str.format`` with a ``g`` conversion keeps only six significant digits,
    which silently truncates longer measurements such as ``1.23456789`` or
    ``1234567.89``. Integers and integral floats keep their plain form without a
    trailing ``.0``; every other value uses ``repr`` so the full precision that
    was written to the history survives.
    """
    if isinstance(value, float):
        if value.is_integer():
            return str(int(value))
        return repr(value)
    return str(value)


def render_metric_table_rows(
    entry: dict[str, object],
    unit: str,
    names: list[str],
    revision: str,
) -> str:
    """Render one revision's rows inside a single chart group table.

    Only the metrics of this group that ``entry`` measured produce rows, so a
    skipped metric stays absent instead of showing a fabricated zero. The group
    heading already names the test case, so a row only carries the metric, its
    revision, the value and the unit.
    """
    rows = []
    for name in names:
        value = entry_value(entry, name)
        if value is None:
            continue
        label = name.rpartition("/")[2] or name
        rows.append(
            "<tr>"
            f"<td>{html.escape(label)}</td>"
            f"<td><code>{html.escape(revision)}</code></td>"
            f'<td class="metric-value">{format_metric_value(value)}</td>'
            f"<td>{html.escape(unit)}</td>"
            "</tr>"
        )
    return "".join(rows)


def render_group_table(
    source: str,
    prefix: str,
    unit: str,
    names: list[str],
    by_date: dict[str, list[dict[str, object]]],
    default_date: str,
) -> str:
    """Render one chart group as its own titled table.

    The ``(prefix, unit)`` group maps to one chart, so it gets one table with the
    chart's title and description. Each measured date is a ``tbody`` so
    JavaScript can keep every group on the selected date; a group without a
    record on that date hides entirely instead of showing an empty table.
    """
    rows_by_date: dict[str, str] = {}
    for date in _sorted_dates(by_date):
        rows = "".join(
            render_metric_table_rows(
                entry, unit, names, str(entry.get("revision", ""))
            )
            for entry in by_date[date]
        )
        if rows:
            rows_by_date[date] = rows
    if not rows_by_date:
        return ""
    bodies = []
    for date, rows in rows_by_date.items():
        hidden = "" if date == default_date else " hidden"
        bodies.append(
            f'<tbody class="table-date-body" data-source="{html.escape(source)}"'
            f' data-date="{html.escape(date)}"{hidden}>\n'
            f'<tr class="table-date-heading"><th colspan="4">{html.escape(date)}</th></tr>\n'
            f"{rows}</tbody>"
        )
    group_hidden = "" if default_date in rows_by_date else " hidden"
    return (
        f'<section class="table-group" data-source="{html.escape(source)}"'
        f' data-group="{html.escape(prefix)}" data-unit="{html.escape(unit)}"'
        f"{group_hidden}>\n"
        f"<h3>{html.escape(prefix)}</h3>\n"
        f'<p class="chart-description">'
        f"{html.escape(chart_description(source, prefix))}</p>\n"
        '<div class="table-scroll">\n'
        '<table class="dashboard-table">\n'
        "<thead><tr><th>Metric</th><th>Revision</th><th>Value</th>"
        "<th>Unit</th></tr></thead>\n"
        f"{''.join(bodies)}</table>\n"
        "</div>\n"
        "</section>"
    )


def render_table_view(
    source: str,
    groups: list[tuple[str, str, list[str]]],
    by_date: dict[str, list[dict[str, object]]],
) -> str:
    """Render one titled table per chart group for a single source.

    The table view mirrors the charts one to one; every ``(prefix, unit)`` group
    gets its own table so rows never mix groups, and all of them share the same
    date selection.
    """
    dates = _sorted_dates(by_date)
    default_date = dates[0] if dates else ""
    return "".join(
        render_group_table(source, prefix, unit, names, by_date, default_date)
        for prefix, unit, names in groups
    )


_DASHBOARD_CSS = """
body { font-family: system-ui, sans-serif; max-width: 960px; margin: 2rem auto; padding: 0 1rem; }
h1 { font-size: 1.5rem; }
h2 { font-size: 1.15rem; margin-bottom: 0.5rem; }
h3 { font-size: 1rem; margin-bottom: 0.5rem; }
.chart-description { color: #475569; margin-top: 0; }
section { margin: 2.5rem 0; }
.table-group { margin: 2rem 0; }
.chart-wrap { position: relative; height: 420px; }
.dashboard-controls { display: flex; flex-wrap: wrap; gap: 0.75rem 1.5rem; margin: 1rem 0 2rem; }
.dashboard-controls label { display: inline-flex; align-items: center; gap: 0.5rem; }
.view-toggle { display: inline-flex; gap: 0.25rem; }
.dashboard-controls select, .dashboard-controls button { font: inherit; padding: 0.35rem 0.6rem; }
.view-toggle button[aria-pressed="true"] { background: #2563eb; color: #ffffff; border-color: #2563eb; }
[hidden] { display: none !important; }
.table-scroll { overflow-x: auto; margin-top: 0.5rem; }
table { width: 100%; border-collapse: collapse; margin-top: 0.5rem; }
th, td { text-align: left; padding: 0.35rem 0.5rem; border-bottom: 1px solid #e2e8f0; font-variant-numeric: tabular-nums; }
.table-date-body th { color: #475569; background: #f8fafc; font-weight: 600; text-align: left; }
td.metric-value { text-align: right; }
code { background: #f1f5f9; padding: 0.1rem 0.3rem; border-radius: 4px; }
"""

_DASHBOARD_SCRIPT = """
const sourceSelect = document.getElementById('benchmark-source');
const dateSelect = document.getElementById('table-date');
const dateLabel = document.getElementById('table-date-label');
const showChartsButton = document.getElementById('show-charts');
const showTableButton = document.getElementById('show-table');
const sourcePanels = [...document.querySelectorAll('.dashboard-source')];
const dateBodies = [...document.querySelectorAll('.table-date-body')];
let currentView = 'chart';
let sourceHasDates = false;

function datesForSource(source) {
  const dates = new Set();
  dateBodies.forEach((body) => {
    if (body.dataset.source === source) {
      dates.add(body.dataset.date);
    }
  });
  return [...dates].sort().reverse();
}

function defaultDate(source) {
  const dates = datesForSource(source);
  return dates.length > 0 ? dates[0] : '';
}

function updateDateControl() {
  // The date picker only matters in table view and only when the selected
  // source has measured dates.
  const visible = currentView === 'table' && sourceHasDates;
  dateSelect.hidden = !visible;
  if (dateLabel) {
    dateLabel.hidden = !visible;
  }
}

function populateDates(source) {
  const dates = datesForSource(source);
  dateSelect.replaceChildren();
  for (const date of dates) {
    const option = document.createElement('option');
    option.value = date;
    option.textContent = date;
    dateSelect.appendChild(option);
  }
  sourceHasDates = dates.length > 0;
  updateDateControl();
}

function showDate(source, date) {
  dateBodies.forEach((body) => {
    body.hidden = !(body.dataset.source === source && body.dataset.date === date);
  });
  // A group without a record on the selected date hides whole instead of
  // leaving an empty table behind.
  document.querySelectorAll('.table-group').forEach((group) => {
    if (group.dataset.source !== source) {
      return;
    }
    const hasDate = [...group.querySelectorAll('.table-date-body')].some(
      (body) => body.dataset.date === date
    );
    group.hidden = !hasDate;
  });
}

function showSource(source) {
  sourcePanels.forEach((panel) => {
    panel.hidden = panel.dataset.source !== source;
  });
  populateDates(source);
  // Switching sources jumps to that source's newest measured date.
  dateSelect.value = defaultDate(source);
  showDate(source, dateSelect.value);
}

function setView(view) {
  currentView = view;
  const showTable = view === 'table';
  sourcePanels.forEach((panel) => {
    const chartView = panel.querySelector('.chart-view');
    const tableView = panel.querySelector('.table-view');
    if (chartView) {
      chartView.hidden = showTable;
    }
    if (tableView) {
      tableView.hidden = !showTable;
    }
  });
  if (showTable) {
    // Keep the table on a measured date when the view becomes visible.
    const targetDate = dateSelect.value || defaultDate(sourceSelect.value);
    showDate(sourceSelect.value, targetDate);
  }
  updateDateControl();
  showChartsButton.setAttribute('aria-pressed', String(!showTable));
  showTableButton.setAttribute('aria-pressed', String(showTable));
}

if (sourceSelect) {
  sourceSelect.addEventListener('change', () => {
    showSource(sourceSelect.value);
  });
  showSource(sourceSelect.value);
}

if (dateSelect) {
  dateSelect.addEventListener('change', () => {
    showDate(sourceSelect.value, dateSelect.value);
  });
}

if (showChartsButton) {
  showChartsButton.addEventListener('click', () => setView('chart'));
}

if (showTableButton) {
  showTableButton.addEventListener('click', () => setView('table'));
}
"""


def render_dashboard(
    title: str,
    history: list[dict[str, object]] | dict[str, list[dict[str, object]]],
    window: int = 10,
) -> str:
    histories = {"axvisor": history} if isinstance(history, list) else history
    histories = {
        source: entries for source, entries in histories.items() if entries
    }
    if not histories:
        raise ValueError("cannot render an empty history")
    source_names = list(histories)
    default_source = "axvisor" if "axvisor" in histories else source_names[0]
    # The JSON keeps every nightly entry. Each source's charts show its most
    # recent window, while the table keeps every measured date so older runs
    # stay reachable even when a window hides them from the charts. A single
    # page then switches source, view and date without a reload.
    panels = []
    chart_index = 0
    for source, source_history in histories.items():
        visible = source_history[-window:] if window > 0 else source_history
        table_groups = _chart_groups(source_history)
        by_date = _history_by_date(source_history)
        sections = []
        for prefix, unit, names in _chart_groups(visible):
            sections.append(
                render_chart_section(
                    chart_index,
                    source,
                    prefix,
                    unit,
                    names,
                    visible,
                )
            )
            chart_index += 1
        latest = visible[-1]
        window_note = (
            f" · showing last {len(visible)} of {len(source_history)} nightly entries"
            if len(visible) < len(source_history)
            else ""
        )
        panels.append(
            f'<div class="dashboard-source" data-source="{html.escape(source)}"'
            f'{"" if source == default_source else " hidden"}>\n'
            f'<div class="chart-view">\n'
            f"<p>Last nightly: {html.escape(str(latest['date']))} · revision "
            f"<code>{html.escape(str(latest['revision']))}</code>{window_note}</p>\n"
            f"{''.join(sections)}</div>\n"
            f'<div class="table-view" hidden>\n'
            f"{render_table_view(source, table_groups, by_date)}"
            "</div>\n"
            "</div>\n"
        )
    options = "".join(
        f'<option value="{html.escape(source)}"'
        f'{" selected" if source == default_source else ""}>'
        f'{html.escape(_source_title(source))}</option>'
        for source in source_names
    )
    return f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{html.escape(title)}</title>
<script src="{CHART_JS}"></script>
<style>
{_DASHBOARD_CSS}
</style>
</head>
<body>
<h1>{html.escape(title)}</h1>
<div class="dashboard-controls">
<label for="benchmark-source">Benchmark source: <select id="benchmark-source">{options}</select></label>
<label id="table-date-label" for="table-date" hidden>Date: <select id="table-date"></select></label>
<span class="view-toggle">
<button type="button" id="show-charts" aria-pressed="true">Charts</button>
<button type="button" id="show-table" aria-pressed="false">Table</button>
</span>
</div>
{''.join(panels)}
<script>
{_DASHBOARD_SCRIPT}
</script>
</body>
</html>
"""


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Render CI performance dashboard")
    parser.add_argument("--metrics", type=Path, required=True)
    parser.add_argument("--history", type=Path, required=True)
    parser.add_argument("--date", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument(
        "--source",
        default="axvisor",
        help="History source to update (for example: axvisor or starry)",
    )
    parser.add_argument("--title", default="Performance Benchmarks")
    parser.add_argument(
        "--window",
        type=int,
        default=10,
        help="Number of most recent nightly entries to chart; 0 charts all",
    )
    parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        metrics = load_metrics(args.metrics)
        history = update_history(
            load_history(args.history), args.date, args.revision, metrics, args.source
        )
        args.history.parent.mkdir(parents=True, exist_ok=True)
        args.history.write_text(
            json.dumps(history, indent=2) + "\n", encoding="utf-8"
        )
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(
            render_dashboard(args.title, history, args.window), encoding="utf-8"
        )
        print(f"dashboard covers {len(history)} nightly entries")
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"performance dashboard failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
