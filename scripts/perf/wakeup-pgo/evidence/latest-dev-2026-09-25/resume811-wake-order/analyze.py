#!/usr/bin/env python3
"""Validate the diagnostic wake-order runs without full20 claims."""

import hashlib
import json
import statistics
from pathlib import Path


ROOT = Path(__file__).resolve().parent
POLICIES = ("other", "fifo")
TAGS = ("A1", "F1", "F2", "A2")


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def one_json(lines, prefix):
    rows = [json.loads(line[len(prefix):]) for line in lines
            if line.startswith(prefix)]
    assert len(rows) == 1, (prefix, len(rows))
    return rows[0]


def parse_case(lines, system, boot, policy, round_number):
    metadata = one_json(lines, "WAKEUP_LATENCY_METADATA ")
    result = one_json(lines, "WAKEUP_LATENCY_RESULT ")
    order = one_json(lines, "WAKEUP_ORDER ")
    assert metadata["warmup"] == 1000 and metadata["handoff_samples"] == 20000
    assert metadata["sender_cpu"] == 0 and metadata["fifo_priority"] == 80
    assert result["case"] == "thread_futex_same_cpu"
    assert result["policy"] == order["policy"] == policy
    assert result["attempted"] == 20000
    assert sum(result["histogram_counts"]) == result["samples"]
    assert order["samples"] == result["samples"]
    before = order["receiver_before_user_marker"]
    after = order["receiver_after_user_marker"]
    assert before + after == result["samples"]
    assert "WAKEUP_LATENCY_PASSED" in lines
    valid = (result["samples"] == 20000 and result["not_parked"] == 0
             and result["missed_deadlines"] == 0)
    return {
        "system": system, "boot": boot, "policy": policy,
        "round": round_number, "valid": valid,
        "samples": result["samples"], "attempted": result["attempted"],
        "not_parked": result["not_parked"],
        "missed_deadlines": result["missed_deadlines"],
        "receiver_before_user_marker": before,
        "receiver_after_user_marker": after,
        "before_percent": 100 * before / result["samples"],
        "p50_ns_diagnostic_only": result["p50_ns"],
    }


def linux_rows(status):
    assert status["board_id"] == "OrangePi-5-Plus-1"
    assert status["board_released"] is True and status["pll_verified"] is True
    assert status["failures"] == 0 and status["diagnostic_only"] is True
    log = ROOT / "linux-boot.log"
    assert sha(log) == status["boot_sha256"]
    content = log.read_text(errors="replace")
    assert "PREEMPT_RT" in content
    rows = []
    cursor = 0
    for round_number in (1, 2, 3):
        for policy in POLICIES:
            start = f"RESUME811_CASE_START policy={policy} round={round_number}"
            done = f"RESUME811_CASE_DONE policy={policy} round={round_number} exit=0"
            begin = content.index(start, cursor)
            end = content.index(done, begin)
            rows.append(parse_case(content[begin:end].splitlines(),
                                   "Linux RT", "L1", policy, round_number))
            cursor = end + len(done)
    return rows


def starry_rows(status):
    assert status["board_id"] == "OrangePi-5-Plus-1"
    assert status["board_released"] is True and status["diagnostic_only"] is True
    assert status["order"] == list(TAGS)
    assert [record["tag"] for record in status["rounds"]] == list(TAGS)
    rows = []
    for record in status["rounds"]:
        assert record["guest_exit"] == 0
        tag = record["tag"]
        checksum = ROOT / f"{tag}-sha256"
        assert sha(checksum) == record["logs"]["sha256"]["sha256"]
        assert status["benchmark_sha256"] in checksum.read_text()
        for round_number in (1, 2, 3):
            for policy in POLICIES:
                name = f"{policy}-{round_number}.log"
                path = ROOT / f"{tag}-{name}"
                assert sha(path) == record["logs"][name]["sha256"]
                lines = path.read_text().splitlines()
                assert lines[-1] == "DIAGNOSTIC_EXIT 0"
                rows.append(parse_case(lines, tag[0], tag, policy,
                                       round_number))
    return rows


def main():
    linux = json.loads((ROOT / "linux-status.json").read_text())
    starry = json.loads((ROOT / "starry-status.json").read_text())
    assert linux["benchmark_sha256"] == starry["benchmark_sha256"]
    rows = linux_rows(linux) + starry_rows(starry)
    grouped = {}
    for system in ("Linux RT", "A", "F"):
        for policy in POLICIES:
            valid = [row for row in rows if row["system"] == system
                     and row["policy"] == policy and row["valid"]]
            grouped[f"{system}:{policy}"] = {
                "valid_runs": len(valid),
                "before_percent_median": statistics.median(
                    row["before_percent"] for row in valid),
                "p50_ns_diagnostic_only": statistics.median(
                    row["p50_ns_diagnostic_only"] for row in valid),
            }
    output = {
        "diagnostic_only": True,
        "benchmark_sha256": linux["benchmark_sha256"],
        "board_id": linux["board_id"],
        "limitations": [
            "marker is the first C statement after futex_wake returns; a post-return preemption before it is indistinguishable",
            "diagnostic benchmark changes code layout and is not frozen full20",
            "one Linux boot and four Starry boots with different background tasks",
        ],
        "rows": rows,
        "group_medians": grouped,
        "invalid_runs": [row for row in rows if not row["valid"]],
    }
    (ROOT / "analysis.json").write_text(json.dumps(output, indent=2) + "\n")
    print(json.dumps({"group_medians": grouped,
                      "invalid_runs": output["invalid_runs"]}, indent=2))


if __name__ == "__main__":
    main()
