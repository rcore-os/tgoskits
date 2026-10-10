#!/usr/bin/env python3
"""Collect performance report metrics into the benchmark bridge artifact.

The benchmark matrices upload one JSON array per check.  This helper owns the
small amount of aggregation needed before the docs workflow consumes those
arrays: reports are read in a deterministic order, flattened per source and
written as ``<source>.json``.  Missing report directories are treated as an
empty source because the workflow already gates inclusion on the corresponding
matrix result and artifact downloads are allowed to be absent.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path


@dataclass(frozen=True)
class SourceResult:
    """The number of metrics collected for one benchmark source."""

    source: str
    count: int
    output: Path | None


def _source_name(value: str) -> str:
    """Validate a source name used as a bridge-artifact filename."""
    name = value.strip()
    allowed = set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-")
    if not name or name in {".", ".."} or any(char not in allowed for char in name):
        raise ValueError(f"invalid source name: {value!r}")
    return name


def load_report_metrics(reports_dir: Path) -> list[dict[str, object]]:
    """Load and flatten the report arrays in ``reports_dir``.

    Only direct JSON files are considered.  This matches the flattened layout
    produced by ``actions/download-artifact`` with ``merge-multiple`` and
    avoids accidentally consuming unrelated files from the runner temp tree.
    Every report must contain a JSON array of metric objects; malformed input
    fails closed so a partial bridge artifact cannot be published.
    """
    if not reports_dir.is_dir():
        return []

    metrics: list[dict[str, object]] = []
    for path in sorted(reports_dir.glob("*.json"), key=lambda item: item.name):
        try:
            payload = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise ValueError(f"invalid performance report {path}: {error}") from error
        if not isinstance(payload, list):
            raise ValueError(f"performance report {path} must contain a JSON array")
        for metric in payload:
            if not isinstance(metric, dict):
                raise ValueError(f"performance report {path} contains a non-object metric")
            name = metric.get("name")
            unit = metric.get("unit")
            value = metric.get("value")
            if not isinstance(name, str) or not name:
                raise ValueError(f"performance report {path} has an invalid metric name")
            if not isinstance(unit, str) or not unit:
                raise ValueError(f"performance report {path} has an invalid metric unit")
            if not isinstance(value, (int, float)) or isinstance(value, bool):
                raise ValueError(f"performance report {path} has a non-numeric metric value")
            metrics.append(metric)
    return metrics


def collect_updates(
    sources: list[tuple[str, Path]],
    output_dir: Path,
) -> list[SourceResult]:
    """Collect each source into ``output_dir`` and return deterministic counts."""
    output_dir.mkdir(parents=True, exist_ok=True)
    seen: set[str] = set()
    results: list[SourceResult] = []
    for raw_name, reports_dir in sources:
        source = _source_name(raw_name)
        if source in seen:
            raise ValueError(f"duplicate source: {source}")
        seen.add(source)

        metrics = load_report_metrics(reports_dir)
        output_path = output_dir / f"{source}.json"
        output = output_path if metrics else None
        if output is not None:
            output.write_text(json.dumps(metrics, indent=2) + "\n", encoding="utf-8")
        elif output_path.exists():
            output_path.unlink()
        results.append(SourceResult(source=source, count=len(metrics), output=output))
    return results


def _write_outputs(results: list[SourceResult], output_file: Path) -> None:
    """Write machine-readable outputs for a composite/workflow step."""
    output_file.parent.mkdir(parents=True, exist_ok=True)
    counts = {result.source: result.count for result in results}
    lines = [
        f"has_updates={'true' if any(counts.values()) else 'false'}",
        f"date={datetime.now(timezone.utc).date().isoformat()}",
        f"counts={json.dumps(counts, sort_keys=True, separators=(',', ':'))}",
    ]
    with output_file.open("a", encoding="utf-8") as stream:
        stream.write("\n".join(lines) + "\n")


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument(
        "--source",
        action="append",
        default=[],
        metavar="NAME=DIRECTORY",
        help="Source report directory; may be specified more than once.",
    )
    parser.add_argument(
        "--output-file",
        type=Path,
        default=None,
        help="Optional GitHub output file (defaults to GITHUB_OUTPUT).",
    )
    return parser.parse_args(argv)


def parse_sources(entries: list[str]) -> list[tuple[str, Path]]:
    sources: list[tuple[str, Path]] = []
    for entry in entries:
        name, separator, directory = entry.partition("=")
        if not separator or not name.strip() or not directory.strip():
            raise ValueError(f"source must use the form 'name=directory': {entry}")
        sources.append((name, Path(directory)))
    return sources


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    output_file = args.output_file
    if output_file is None:
        output_path = os.environ.get("GITHUB_OUTPUT")
        output_file = Path(output_path) if output_path else None
    try:
        results = collect_updates(parse_sources(args.source), args.output_dir)
        if output_file is not None:
            _write_outputs(results, output_file)
    except (OSError, ValueError) as error:
        print(f"::error::Collecting benchmark updates failed: {error}", file=sys.stderr)
        return 1

    for result in results:
        print(f"{result.source} metrics: {result.count}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
