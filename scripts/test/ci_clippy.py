#!/usr/bin/env python3
"""Coarse Clippy gating and conversion of its authoritative plan to a matrix."""
import argparse
import json
import subprocess
from pathlib import Path


def needs_clippy(paths):
    # Only the static documentation tree is a known non-build input. README
    # files elsewhere may be included by Rust or packaging and are not ignored.
    return any(not path.startswith(("docs/docs/", "docs/blog/")) for path in paths)


def classify(root, since):
    try:
        def git(*arguments):
            return subprocess.check_output(["git", *arguments], cwd=root, text=True, stderr=subprocess.PIPE).strip()
        base = git("merge-base", since, "HEAD")
        head = git("rev-parse", "HEAD")
        if base == head:
            return True, "comparison base is HEAD; use the full Clippy matrix"
        changed = git("diff", "--name-only", "--no-renames", f"{base}..{head}", "--").splitlines()
    except (subprocess.CalledProcessError, OSError):
        return True, "comparison unavailable; use the full Clippy matrix"
    if not changed:
        return False, "no changed files between distinct commits"
    return needs_clippy(changed), "code/configuration or unknown input" if needs_clippy(changed) else "static documentation only"


def matrix(plan):
    if plan.get("schema_version") != 1 or not plan.get("checks") or not plan.get("shards"):
        raise ValueError("missing or unsupported Clippy plan")
    seen = []
    rows = []
    for i, shard in enumerate(plan["shards"]):
        if not shard:
            raise ValueError("empty Clippy shard")
        seen.extend(shard)
        rows.append({"shard": i + 1})
    if sorted(seen) != list(range(len(plan["checks"]))):
        raise ValueError("Clippy plan omits or duplicates checks")
    return {"include": rows}


def selection_outputs(selection, expected_sha):
    """Translate the Rust selector's facts, without implementing package selection."""
    import re
    mode = selection.get("mode")
    if selection.get("schema_version") != 1 or mode not in {"full", "incremental", "empty"}:
        raise ValueError("unsupported Clippy selection")
    if selection.get("tested_sha") != expected_sha or not re.fullmatch(r"[0-9a-f]{40,64}", expected_sha):
        raise ValueError("Clippy selection is not for this checkout")
    digest = selection.get("selection_digest", "")
    if not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise ValueError("invalid Clippy selection digest")
    base = selection.get("diff_base_sha")
    if mode in {"incremental", "empty"} and not re.fullmatch(r"[0-9a-f]{40,64}", base or ""):
        raise ValueError("Clippy comparison base is not immutable")
    if mode != "full" and base == expected_sha:
        raise ValueError("Clippy comparison base is HEAD; a full selection is required")
    if mode in {"full", "incremental"} and not selection.get("checks"):
        raise ValueError("non-empty selection has no checks")
    if mode == "empty" and selection.get("checks"):
        raise ValueError("empty selection contains checks")
    return {"mode": mode, "digest": digest, "base": base or ""}


def result_succeeded(mode, prepare, checks):
    if prepare != "success":
        return False
    if mode in {"incremental", "empty"}:
        return checks == "skipped"
    return mode == "full" and checks == "success"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="mode", required=True)
    gate = sub.add_parser("classify")
    gate.add_argument("--since", required=True)
    gate.add_argument("--workspace", type=Path, default=Path(__file__).resolve().parents[2])
    plan_parser = sub.add_parser("matrix")
    plan_parser.add_argument("plan", type=Path)
    selection_parser = sub.add_parser("selection")
    selection_parser.add_argument("document", type=Path)
    selection_parser.add_argument("--sha", required=True)
    result_parser = sub.add_parser("result")
    result_parser.add_argument("--selection-mode", required=True)
    result_parser.add_argument("--prepare", required=True)
    result_parser.add_argument("--checks", required=True)
    args = parser.parse_args()
    if args.mode == "selection":
        for key, value in selection_outputs(json.loads(args.document.read_text()), args.sha).items():
            print(f"{key}={value}")
    elif args.mode == "result":
        if not result_succeeded(args.selection_mode, args.prepare, args.checks):
            raise SystemExit("Clippy validation failed, was cancelled, or lacks a valid mode/result")
    elif args.mode == "classify":
        required, reason = classify(args.workspace, args.since)
        print(f"required={str(required).lower()}")
        print(f"reason={reason}")
    else:
        print("matrix=" + json.dumps(matrix(json.loads(args.plan.read_text()))))


if __name__ == "__main__":
    main()
