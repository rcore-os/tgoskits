#!/usr/bin/env python3
"""Render a workflow stage summary and enforce required stage success."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path


def _split_entries(value: str) -> list[str]:
    """Split newline/comma/semicolon separated values while preserving labels."""
    entries: list[str] = []
    for line in value.replace(";", "\n").replace(",", "\n").splitlines():
        entry = line.strip()
        if entry:
            entries.append(entry)
    return entries


def parse_stages(value: str) -> list[tuple[str, str]]:
    """Parse ``label=result`` entries and reject malformed or duplicate labels."""
    stages: list[tuple[str, str]] = []
    seen: set[str] = set()
    for entry in _split_entries(value):
        label, separator, result = entry.partition("=")
        label = label.strip()
        result = result.strip()
        if not separator or not label or not result:
            raise ValueError(
                "stage entries must use the form 'label=result': " + entry
            )
        if label in seen:
            raise ValueError(f"duplicate stage label: {label}")
        seen.add(label)
        stages.append((label, result))
    if not stages:
        raise ValueError("at least one stage is required")
    return stages


def required_stage_labels(value: str, stages: list[tuple[str, str]]) -> set[str]:
    """Resolve required labels, defaulting to every reported stage."""
    labels = set(_split_entries(value))
    if not labels:
        return {label for label, _ in stages}
    known = {label for label, _ in stages}
    unknown = labels - known
    if unknown:
        raise ValueError("required stage is missing from stages: " + ", ".join(sorted(unknown)))
    return labels


def render_summary(
    title: str,
    revision: str,
    revision_label: str,
    stages: list[tuple[str, str]],
) -> str:
    """Return the stable GitHub Actions summary for a workflow result."""
    lines = [f"## {title}", ""]
    if revision:
        lines.extend((f"{revision_label}: {revision}", ""))
    lines.extend(("| Stage | Result |", "| --- | --- |"))
    lines.extend(f"| {_escape(label)} | {_escape(result)} |" for label, result in stages)
    return "\n".join(lines) + "\n"


def _escape(value: str) -> str:
    return value.replace("|", "\\|").replace("\n", " ")


def _parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--title", required=True)
    parser.add_argument("--revision", default="")
    parser.add_argument("--revision-label", default="tested revision")
    parser.add_argument("--stages", required=True)
    parser.add_argument("--required-stages", default="")
    parser.add_argument("--summary-file", type=Path)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(argv)
    try:
        stages = parse_stages(args.stages)
        required = required_stage_labels(args.required_stages, stages)
    except ValueError as error:
        print(f"CI result reporting failed: {error}", file=sys.stderr)
        return 2

    summary = render_summary(args.title, args.revision, args.revision_label, stages)
    if args.summary_file is not None:
        with args.summary_file.open("a", encoding="utf-8") as output:
            output.write(summary)
    else:
        sys.stdout.write(summary)

    failed = [label for label, result in stages if label in required and result != "success"]
    if failed:
        print(
            "Required CI stages did not succeed: " + ", ".join(failed),
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
