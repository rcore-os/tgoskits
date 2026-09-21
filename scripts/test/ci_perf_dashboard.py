#!/usr/bin/env python3

"""Accumulate nightly benchmark history and render a static Chart.js dashboard.

The dashboard groups metrics by test case (the prefix of the metric name),
uses the nightly date as the x-axis, and draws plain lines without filling.
"""

import argparse
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
<h2>{prefix}</h2>
<p class="chart-description">{chart_description(source, prefix)}</p>
<div class="chart-wrap"><canvas id="chart-{index}"></canvas></div>
<script>
new Chart(document.getElementById('chart-{index}'), {payload});
</script>
</section>"""


def _source_title(source: str) -> str:
    return {"axvisor": "AxVisor", "starry": "Starry"}.get(source, source)


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
    # The JSON keeps every nightly entry; each source's charts show its most
    # recent window. A single page then switches sources without a reload.
    source_sections = []
    chart_index = 0
    for source, source_history in histories.items():
        visible = source_history[-window:] if window > 0 else source_history
        sections = []
        for (prefix, unit), names in collect_groups(visible).items():
            sections.append(
                render_chart_section(
                    chart_index, source, prefix, unit, names, visible
                )
            )
            chart_index += 1
        latest = visible[-1]
        window_note = (
            f" · showing last {len(visible)} of {len(source_history)} nightly entries"
            if len(visible) < len(source_history)
            else ""
        )
        source_sections.append(
            f'''<div class="dashboard-source" data-source="{source}">
<p>Last nightly: {latest['date']} · revision <code>{latest['revision']}</code>{window_note}</p>
{''.join(sections)}
</div>'''
        )
    source_names = list(histories)
    default_source = "axvisor" if "axvisor" in histories else source_names[0]
    options = "".join(
        f'<option value="{source}"{' selected' if source == default_source else ''}>'
        f'{_source_title(source)}</option>'
        for source in source_names
    )
    return f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<script src="{CHART_JS}"></script>
<style>
body {{ font-family: system-ui, sans-serif; max-width: 960px; margin: 2rem auto; padding: 0 1rem; }}
h1 {{ font-size: 1.5rem; }}
h2 {{ font-size: 1.15rem; margin-bottom: 0.5rem; }}
.chart-description {{ color: #475569; margin-top: 0; }}
section {{ margin: 2.5rem 0; }}
.chart-wrap {{ position: relative; height: 420px; }}
.source-picker {{ margin: 1rem 0 2rem; }}
.source-picker select {{ font: inherit; padding: 0.35rem 0.6rem; }}
code {{ background: #f1f5f9; padding: 0.1rem 0.3rem; border-radius: 4px; }}
</style>
</head>
<body>
<h1>{title}</h1>
<label class="source-picker" for="benchmark-source">Benchmark source: </label>
<select id="benchmark-source" class="source-picker">{options}</select>
{''.join(source_sections)}
<script>
const sourceSelect = document.getElementById('benchmark-source');
const sourcePanels = [...document.querySelectorAll('.dashboard-source')];
function showSource(source) {{
  sourcePanels.forEach((panel) => {{
    panel.hidden = panel.dataset.source !== source;
  }});
}}
sourceSelect.addEventListener('change', () => showSource(sourceSelect.value));
showSource(sourceSelect.value);
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
