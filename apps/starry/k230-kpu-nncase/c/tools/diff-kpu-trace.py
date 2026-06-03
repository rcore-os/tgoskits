#!/usr/bin/env python3
import argparse
import re
from pathlib import Path
from typing import Optional


START_RE = re.compile(r"^k230_kpu_start .* start (0x[0-9a-fA-F]+) end (0x[0-9a-fA-F]+)")
EVENT_RE = re.compile(r"^(k230_kpu_[a-z0-9_]+) ")


def parse_runs(path: Path) -> list[list[str]]:
    runs: list[list[str]] = []
    current: Optional[list[str]] = None
    for raw in path.read_text(errors="replace").splitlines():
        line = raw.strip()
        if not line:
            continue
        if START_RE.match(line):
            if current is not None:
                runs.append(current)
            current = [line]
        elif current is not None and EVENT_RE.match(line):
            current.append(line)
    if current is not None:
        runs.append(current)
    return runs


def event_signature(line: str) -> str:
    if line.startswith("k230_kpu_start "):
        match = START_RE.match(line)
        return f"start:{match.group(1)}:{match.group(2)}" if match else line
    if line.startswith("k230_kpu_runtime_arg_table "):
        words = re.search(r" words (.*)$", line)
        return f"arg_table:{words.group(1)}" if words else line
    if line.startswith("k230_kpu_l2_load "):
        match = re.search(
            r"source (0x[0-9a-fA-F]+) logical (0x[0-9a-fA-F]+) bytes ([0-9]+) head (0x[0-9a-fA-F]+)",
            line,
        )
        return f"l2_load:{match.group(1)}:{match.group(2)}:{match.group(3)}:{match.group(4)}" if match else line
    if line.startswith("k230_kpu_l2_load_hash "):
        match = re.search(r"source_hash (0x[0-9a-fA-F]+)", line)
        return f"l2_load_hash:{match.group(1)}" if match else line
    if line.startswith("k230_kpu_l2_store "):
        match = re.search(
            r"logical (0x[0-9a-fA-F]+) physical (0x[0-9a-fA-F]+) bytes ([0-9]+)",
            line,
        )
        return f"l2_store:{match.group(1)}:{match.group(2)}:{match.group(3)}" if match else line
    if line.startswith("k230_kpu_l2_store_hash "):
        match = re.search(
            r"source_hash (0x[0-9a-fA-F]+) dest_hash (0x[0-9a-fA-F]+)",
            line,
        )
        return f"l2_store_hash:{match.group(1)}:{match.group(2)}" if match else line
    if line.startswith("k230_kpu_gnne_summary "):
        match = re.search(
            r"instructions ([0-9]+) l2_load ([0-9]+) l2_load_w ([0-9]+) l2_store ([0-9]+) bytes ([0-9]+) unknown ([0-9]+)",
            line,
        )
        return "summary:{}:{}:{}:{}:{}:{}".format(*match.groups()) if match else line
    if line.startswith("k230_kpu_gnne_compute_summary "):
        return "compute:" + line.split(" k230-kpu ", 1)[-1]
    return line


def run_signature(run: list[str]) -> list[str]:
    return [event_signature(line) for line in run]


def choose_candidate_window(reference_count: int, candidate_count: int, offset: Optional[int]) -> int:
    if offset is not None:
        return offset
    if candidate_count >= reference_count * 2:
        return candidate_count - reference_count
    return 0


def print_run(label: str, run: list[str], max_lines: int) -> None:
    print(f"{label}:")
    for line in run[:max_lines]:
        print(f"  {line}")
    if len(run) > max_lines:
        print(f"  ... ({len(run) - max_lines} more)")


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Compare K230 KPU trace runs and locate the first semantic split."
    )
    parser.add_argument("--reference", required=True, type=Path)
    parser.add_argument("--candidate", required=True, type=Path)
    parser.add_argument("--candidate-offset", type=int)
    parser.add_argument("--max-context", type=int, default=14)
    args = parser.parse_args()

    reference_runs = parse_runs(args.reference)
    candidate_runs_all = parse_runs(args.candidate)
    if not reference_runs:
        raise SystemExit(f"no KPU runs found in reference trace: {args.reference}")
    if not candidate_runs_all:
        raise SystemExit(f"no KPU runs found in candidate trace: {args.candidate}")

    offset = choose_candidate_window(
        len(reference_runs), len(candidate_runs_all), args.candidate_offset
    )
    candidate_runs = candidate_runs_all[offset : offset + len(reference_runs)]
    print(f"reference_runs={len(reference_runs)} candidate_runs={len(candidate_runs_all)} compare_offset={offset}")
    if len(candidate_runs) < len(reference_runs):
        print(f"candidate window too short: {len(candidate_runs)} < {len(reference_runs)}")
        return 2

    first_mismatch: Optional[int] = None
    for index, (ref_run, cand_run) in enumerate(zip(reference_runs, candidate_runs), start=1):
        if run_signature(ref_run) != run_signature(cand_run):
            first_mismatch = index
            print(f"first_mismatch_run={index}")
            print_run("reference", ref_run, args.max_context)
            print_run("candidate", cand_run, args.max_context)
            break

    if first_mismatch is None:
        print("trace signatures match")
        return 0
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
