#!/usr/bin/env python3
"""Validate frozen Linux RT PMU counts and compare diagnostics only."""

import json
import hashlib
import statistics
from pathlib import Path

ROOT = Path(__file__).resolve().parent
STARRY = ROOT.parent / "resume808-current-pmu/analysis.json"
EVENTS = {"cycles", "instructions", "l1i_refill", "l1d_refill"}


def one_json(lines, prefix):
    rows = [json.loads(line[len(prefix):]) for line in lines if line.startswith(prefix)]
    assert len(rows) == 1, (prefix, len(rows))
    return rows[0]


def main():
    status = json.loads((ROOT / "status.json").read_text())
    assert status["board_released"] is True and status["failures"] == 0
    assert status["pll_verified"] is True and status["diagnostic_only"] is True
    assert hashlib.sha256((ROOT / "boot.log").read_bytes()).hexdigest() == status["boot_sha256"]
    assert hashlib.sha256((ROOT / "init.c").read_bytes()).hexdigest() == status["init_source_sha256"]
    text = (ROOT / "boot.log").read_text(errors="replace")
    assert "PREEMPT_RT" in text and "RESUME809_INIT_DONE failures=0" in text
    cursor = 0
    rows = []
    for round_number in (1, 2, 3):
        for policy in ("other", "fifo"):
            start = f"RESUME809_CASE_START policy={policy} round={round_number}"
            done = f"RESUME809_CASE_DONE policy={policy} round={round_number} exit=0"
            begin = text.index(start, cursor)
            end = text.index(done, begin)
            segment = text[begin:end]
            cursor = end + len(done)
            lines = segment.splitlines()
            assert lines.count("WAKEUP_LATENCY_PASSED") == 1
            assert "PMU_COUNT_DONE windows=1 active=0 child_status=0" in lines
            metadata = one_json(lines, "WAKEUP_LATENCY_METADATA ")
            assert metadata["warmup"] == 1000 and metadata["handoff_samples"] == 20000
            result = one_json(lines, "WAKEUP_LATENCY_RESULT ")
            assert result["case"] == "thread_futex_same_cpu" and result["policy"] == policy
            assert result["samples"] == result["attempted"] == 20000
            assert result["not_parked"] == result["missed_deadlines"] == 0
            assert sum(result["histogram_counts"]) == 20000
            counts = {}
            for line in lines:
                if not line.startswith("PMU_COUNT {"):
                    continue
                count = json.loads(line[10:])
                event = count["event"]
                assert event in EVENTS and event not in counts
                assert count["window"] == 1
                assert count["enabled_ns"] == count["running_ns"] > 0
                assert count["value"] < 0xffff0000
                counts[event] = count["value"]
            assert set(counts) == EVENTS
            instructions = counts["instructions"]
            rows.append({
                "policy": policy,
                "round": round_number,
                "p50_ns_diagnostic_only": result["p50_ns"],
                "counts": counts,
                "instructions_per_attempt": instructions / 20000,
                "cycles_per_instruction": counts["cycles"] / instructions,
                "l1i_refill_per_1000_instructions": counts["l1i_refill"] * 1000 / instructions,
                "l1d_refill_per_1000_instructions": counts["l1d_refill"] * 1000 / instructions,
            })
    linux = {}
    starry = json.loads(STARRY.read_text())["group_medians"]
    comparison = {}
    for policy in ("other", "fifo"):
        subset = [row for row in rows if row["policy"] == policy]
        linux[policy] = {
            field: statistics.median(row[field] for row in subset)
            for field in (
                "p50_ns_diagnostic_only", "instructions_per_attempt",
                "cycles_per_instruction", "l1i_refill_per_1000_instructions",
                "l1d_refill_per_1000_instructions",
            )
        }
        comparison[policy] = {
            field: starry[f"F:{policy}"][field] / linux[policy][field]
            for field in linux[policy]
        }
    output = {
        "diagnostic_only": True,
        "limitations": [
            "whole-case CPU0 EL1 counts include child startup, reverse handoffs and background",
            "Linux initramfs and Starry userspace service background differ",
            "counts are sequential rather than atomic group reads",
            "not exclusive forward latency or full20 acceptance",
        ],
        "linux_image_sha256": status["linux_image_sha256"],
        "benchmark_sha256": status["benchmark_sha256"],
        "collector_sha256": status["collector_sha256"],
        "board_id": status["board_id"],
        "runs": rows,
        "linux_medians": linux,
        "starry_f_over_linux_ratios": comparison,
    }
    (ROOT / "analysis.json").write_text(json.dumps(output, indent=2) + "\n")
    print(json.dumps({"linux_medians": linux, "starry_f_over_linux_ratios": comparison}, indent=2))


if __name__ == "__main__":
    main()
