#!/usr/bin/env python3
"""Validate the current-source A/F PMU diagnostic without acceptance claims."""

import hashlib
import json
import statistics
from pathlib import Path

ROOT = Path(__file__).resolve().parent
EXPECTED_EVENTS = {"cycles", "instructions", "l1i_refill", "l1d_refill"}
EXPECTED_POLICIES = ("other", "fifo")
EXPECTED_TAGS = ("A1", "F1", "F2", "A2")


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def parse_line(lines, prefix):
    found = [line[len(prefix):] for line in lines if line.startswith(prefix)]
    assert len(found) == 1, (prefix, len(found))
    return json.loads(found[0])


def main():
    status = json.loads((ROOT / "status.json").read_text())
    assert status["board_released"] is True
    assert status["order"] == list(EXPECTED_TAGS)
    assert [record["tag"] for record in status["rounds"]] == list(EXPECTED_TAGS)
    assert status["diagnostic_only"] is True
    summary = []
    for record in status["rounds"]:
        tag = record["tag"]
        assert record["guest_exit"] == 0
        checksums = (ROOT / f"{tag}-sha256").read_text().splitlines()
        assert sha(ROOT / f"{tag}-sha256") == record["logs"]["sha256"]["sha256"]
        assert len(checksums) == 2
        assert status["benchmark_sha256"] in checksums[0]
        assert status["collector_sha256"] in checksums[1]
        for policy in EXPECTED_POLICIES:
            for round_number in (1, 2, 3):
                name = f"{policy}-{round_number}.log"
                path = ROOT / f"{tag}-{name}"
                assert sha(path) == record["logs"][name]["sha256"]
                lines = path.read_text().splitlines()
                assert lines[0].startswith("PMU_SCOPE cpu=0 observer_cpu=2 exclude_user=1")
                assert "fixed_slots=1" in lines[0] and "marker_gated=0" in lines[0]
                assert lines[-1] == "DIAGNOSTIC_EXIT 0"
                assert "PMU_COUNT_DONE windows=1 active=0 child_status=0" in lines
                assert "WAKEUP_LATENCY_PASSED" in lines
                assert f"WAKEUP_LATENCY_CASE_START case=thread_futex_same_cpu policy={policy}" in lines
                assert f"WAKEUP_LATENCY_CASE_DONE case=thread_futex_same_cpu policy={policy}" in lines
                metadata = parse_line(lines, "WAKEUP_LATENCY_METADATA ")
                assert metadata["warmup"] == 1000 and metadata["handoff_samples"] == 20000
                assert metadata["sender_cpu"] == 0 and metadata["fifo_priority"] == 80
                result = parse_line(lines, "WAKEUP_LATENCY_RESULT ")
                assert result["case"] == "thread_futex_same_cpu" and result["policy"] == policy
                assert result["attempted"] == 20000
                assert result["samples"] + result["not_parked"] == 20000
                assert sum(result["histogram_counts"]) == result["samples"]
                valid = (result["samples"] == 20000 and result["not_parked"] == 0
                         and result["missed_deadlines"] == 0)
                count_lines = [line[10:] for line in lines if line.startswith("PMU_COUNT {")]
                assert len(count_lines) == 4
                events = {}
                for item in count_lines:
                    count = json.loads(item)
                    event = count["event"]
                    assert event in EXPECTED_EVENTS and event not in events
                    assert count["window"] == 1
                    assert count["value"] < 0xffff0000
                    assert count["enabled_ns"] == count["running_ns"] > 0
                    events[event] = count["value"]
                assert set(events) == EXPECTED_EVENTS
                instructions = events["instructions"]
                row = {
                    "tag": tag,
                    "policy": policy,
                    "round": round_number,
                    "valid": valid,
                    "samples": result["samples"],
                    "not_parked": result["not_parked"],
                    "missed_deadlines": result["missed_deadlines"],
                    "p50_ns_diagnostic_only": result["p50_ns"],
                    "counts": events,
                    "instructions_per_attempt": instructions / 20000,
                    "cycles_per_instruction": events["cycles"] / instructions,
                    "l1i_refill_per_1000_instructions": events["l1i_refill"] * 1000 / instructions,
                    "l1d_refill_per_1000_instructions": events["l1d_refill"] * 1000 / instructions,
                }
                summary.append(row)
    grouped = {}
    for kind in ("A", "F"):
        for policy in EXPECTED_POLICIES:
            rows = [row for row in summary if row["tag"].startswith(kind)
                    and row["policy"] == policy and row["valid"]]
            assert len(rows) >= 4, (kind, policy, len(rows))
            grouped[f"{kind}:{policy}"] = {
                field: statistics.median(row[field] for row in rows)
                for field in (
                    "p50_ns_diagnostic_only",
                    "instructions_per_attempt",
                    "cycles_per_instruction",
                    "l1i_refill_per_1000_instructions",
                    "l1d_refill_per_1000_instructions",
                )
            }
            grouped[f"{kind}:{policy}"]["valid_runs"] = len(rows)
    output = {
        "diagnostic_only": True,
        "limitations": status["limitations"],
        "source_commit": status["source_commit"],
        "board_id": status["board_id"],
        "order": status["order"],
        "runs": summary,
        "invalid_runs": [
            {key: row[key] for key in ("tag", "policy", "round", "samples", "not_parked", "missed_deadlines")}
            for row in summary if not row["valid"]
        ],
        "group_medians": grouped,
    }
    (ROOT / "analysis.json").write_text(json.dumps(output, indent=2) + "\n")
    print(json.dumps(grouped, indent=2))


if __name__ == "__main__":
    main()
