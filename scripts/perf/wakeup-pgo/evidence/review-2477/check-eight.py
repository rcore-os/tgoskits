#!/usr/bin/env python3
"""Verify archived eight-boot metrics without discarding either valid block."""

import hashlib
import json
import statistics
from pathlib import Path

ROOT = Path(__file__).resolve().parent
BLOCKS = ((ROOT, "resume653"), (ROOT, "resume654"))
OUT = ROOT / "joint-eight.json"
TAGS = ("A1", "B1", "B2", "A2")


def read_json(path):
    return json.loads(path.read_text())


def load_block(folder, prefix):
    status = read_json(folder / f"{prefix}-abba-status.json")
    report = read_json(folder / f"{prefix}-abba-comparison.json")
    assert status["stage"] == "board_collection_complete"
    assert status["board_released"] is True
    assert report["all_runs_valid"] is True
    assert [run["tag"] for run in status["runs"]] == list(TAGS)
    assert report["thresholds"] == {"improvement_target": 0.1, "regression_limit": -0.03}
    parsed = {}
    for run in status["runs"]:
        assert run["valid"] is True
        tag = run["tag"]
        log = folder / f"{prefix}-{tag}-full.log"
        assert hashlib.sha256(log.read_bytes()).hexdigest() == run["guest_log_sha256"]
        rows = [json.loads(line.split(" ", 1)[1]) for line in log.read_text().splitlines()
                if line.startswith("WAKEUP_LATENCY_RESULT ")]
        assert len(rows) == 20
        assert all(row["samples"] == row["attempted"] and
                   row["not_parked"] == row["missed_deadlines"] == 0 for row in rows)
        keyed = {(row["policy"], row["case"]): row for row in rows}
        assert len(keyed) == 20
        parsed[tag] = keyed
    return status, report, parsed


def main():
    blocks = [load_block(*block) for block in BLOCKS]
    first, second = (block[0] for block in blocks)
    for field in ("source_head", "board_id", "images", "profile_sha256", "bench"):
        assert first[field] == second[field], field
    assert all(block[0]["sequence"] == list(TAGS) for block in blocks)
    keys = sorted(blocks[0][2]["A1"])
    assert all(set(run) == set(keys) for _, _, parsed in blocks for run in parsed.values())
    rows = []
    for policy, case in keys:
        a_runs = [parsed[tag][(policy, case)] for _, _, parsed in blocks
                  for tag in ("A1", "A2")]
        b_runs = [parsed[tag][(policy, case)] for _, _, parsed in blocks
                  for tag in ("B1", "B2")]
        medians = {field: {"A": statistics.median(row[field] for row in a_runs),
                           "B": statistics.median(row[field] for row in b_runs)}
                   for field in ("p50_ns", "p99_ns", "p999_ns")}
        change = {field: (pair["A"] - pair["B"]) / pair["A"]
                  for field, pair in medians.items()}
        rows.append({"policy": policy, "case": case, "medians_ns": medians,
                     "improvement": change})
    regressions = [{"policy": row["policy"], "case": row["case"], "metric": field,
                    "improvement": row["improvement"][field]}
                   for row in rows for field in ("p50_ns", "p99_ns", "p999_ns")
                   if row["improvement"][field] <= -0.03]
    focus = next(row for row in rows if row["policy"] == "other" and
                 row["case"] == "thread_futex_same_cpu")
    result = {
        "protocol": "predeclared in PLAN.md after first valid block failed tail guardrail",
        "source_head": first["source_head"],
        "board_id": first["board_id"],
        "block_verdicts": [block[1]["verdict"]["passed"] for block in blocks],
        "runs_per_side": 4,
        "focus_p50_improvement": focus["improvement"]["p50_ns"],
        "p50_improved_at_least_ten_percent": sum(
            row["improvement"]["p50_ns"] >= 0.1 for row in rows),
        "regressions_at_least_three_percent": regressions,
        "joint_guardrails_pass": not regressions and any(
            row["improvement"]["p50_ns"] >= 0.1 for row in rows),
        "rows": rows,
    }
    assert result == read_json(OUT), "archived joint metrics differ from raw logs"
    print("BLOCK_VERDICTS", result["block_verdicts"])
    print("JOINT_FOCUS_P50_IMPROVEMENT", round(result["focus_p50_improvement"] * 100, 2))
    print("JOINT_REGRESSIONS", regressions)
    print("JOINT_GUARDRAILS_PASS", result["joint_guardrails_pass"])


if __name__ == "__main__":
    main()
