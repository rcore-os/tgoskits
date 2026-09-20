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


def load_history(path: Path) -> list[dict[str, object]]:
    if not path.exists():
        return []
    history = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(history, list):
        raise ValueError(f"{path} must contain a JSON array")
    return history


def update_history(
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


def render_chart_section(
    index: int,
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
<div class="chart-wrap"><canvas id="chart-{index}"></canvas></div>
<script>
new Chart(document.getElementById('chart-{index}'), {payload});
</script>
</section>"""


def render_dashboard(
    title: str, history: list[dict[str, object]], window: int = 7
) -> str:
    if not history:
        raise ValueError("cannot render an empty history")
    # The JSON keeps every nightly entry; charts show the most recent window.
    visible = history[-window:] if window > 0 else history
    sections = [
        render_chart_section(index, prefix, unit, names, visible)
        for index, ((prefix, unit), names) in enumerate(collect_groups(visible).items())
    ]
    latest = visible[-1]
    window_note = (
        f" · showing last {len(visible)} of {len(history)} nightly entries"
        if len(visible) < len(history)
        else ""
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
section {{ margin: 2.5rem 0; }}
.chart-wrap {{ position: relative; height: 420px; }}
code {{ background: #f1f5f9; padding: 0.1rem 0.3rem; border-radius: 4px; }}
</style>
</head>
<body>
<h1>{title}</h1>
<p>Last nightly: {latest['date']} · revision <code>{latest['revision']}</code>{window_note}</p>
{''.join(sections)}
</body>
</html>
"""


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Render CI performance dashboard")
    parser.add_argument("--metrics", type=Path, required=True)
    parser.add_argument("--history", type=Path, required=True)
    parser.add_argument("--date", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--title", default="Performance Benchmarks")
    parser.add_argument(
        "--window",
        type=int,
        default=7,
        help="Number of most recent nightly entries to chart; 0 charts all",
    )
    parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        metrics = load_metrics(args.metrics)
        history = update_history(
            load_history(args.history), args.date, args.revision, metrics
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
