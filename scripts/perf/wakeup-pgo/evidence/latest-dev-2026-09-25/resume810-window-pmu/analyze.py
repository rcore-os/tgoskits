#!/usr/bin/env python3
"""Audit forward-window PMU diagnostics without treating them as acceptance."""

import hashlib
import json
import statistics
from pathlib import Path

ROOT = Path(__file__).resolve().parent
EVENTS = ("instructions", "cycles", "l1i_refill", "l1d_refill")
POLICIES = ("other", "fifo")
FIELDS = ("raw_p50", "read_p50", "adjusted_p50", "raw_p99", "read_p99")


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def single_json(lines, prefix):
    found = [json.loads(line[len(prefix):]) for line in lines if line.startswith(prefix)]
    assert len(found) == 1, (prefix, len(found))
    return found[0]


def parse_case(lines, system, event, policy, round_number, boot):
    row = {"system": system, "event": event, "policy": policy,
           "round": round_number, "boot": boot}
    try:
        metadata = single_json(lines, "WAKEUP_LATENCY_METADATA ")
        result = single_json(lines, "WAKEUP_LATENCY_RESULT ")
        window = single_json(lines, "WAKEUP_PMU_WINDOW ")
        assert metadata["warmup"] == 1000 and metadata["handoff_samples"] == 20000
        assert metadata["sender_cpu"] == 0 and metadata["fifo_priority"] == 80
        assert result["case"] == "thread_futex_same_cpu" and result["policy"] == policy
        assert window["event"] == event and window["samples"] == result["samples"]
        assert result["attempted"] == 20000
        assert sum(result["histogram_counts"]) == result["samples"]
        assert window["adjusted_p50"] == max(0, window["raw_p50"] - window["read_p50"])
        valid = (result["samples"] == 20000 and result["not_parked"] == 0
                 and result["missed_deadlines"] == 0)
        row.update({"valid": valid, "samples": result["samples"],
                    "not_parked": result["not_parked"],
                    "missed_deadlines": result["missed_deadlines"],
                    "p50_ns_diagnostic_only": result["p50_ns"], **window})
    except (AssertionError, KeyError, ValueError) as error:
        row.update({"valid": False, "error": repr(error)})
    return row


def linux_rows(status):
    assert status["board_released"] is True and status["failures"] == 0
    assert status["pll_verified"] is True and status["diagnostic_only"] is True
    log = ROOT / "linux-boot.log"
    assert sha(log) == status["boot_sha256"]
    content = log.read_text(errors="replace")
    assert "PREEMPT_RT" in content
    cursor = 0
    rows = []
    for event in EVENTS:
        for round_number in (1, 2):
            for policy in POLICIES:
                start = f"RESUME810_CASE_START event={event} policy={policy} round={round_number}"
                done = f"RESUME810_CASE_DONE event={event} policy={policy} round={round_number} exit=0"
                begin = content.index(start, cursor)
                end = content.index(done, begin)
                lines = content[begin:end].splitlines()
                assert "WAKEUP_LATENCY_PASSED" in lines
                rows.append(parse_case(lines, "Linux RT", event, policy, round_number, "L1"))
                cursor = end + len(done)
    return rows


def starry_rows(status):
    assert status["board_released"] is True and status["diagnostic_only"] is True
    assert status["order"] == ["A1", "F1", "F2", "A2"]
    assert [record["tag"] for record in status["rounds"]] == status["order"]
    rows = []
    for record in status["rounds"]:
        tag = record["tag"]
        sha_file = ROOT / f"{tag}-sha256"
        assert sha(sha_file) == record["logs"]["sha256"]["sha256"]
        assert status["benchmark_sha256"] in sha_file.read_text()
        for event in EVENTS:
            for round_number in (1, 2):
                for policy in POLICIES:
                    name = f"{event}-{policy}-{round_number}.log"
                    path = ROOT / f"{tag}-{name}"
                    assert sha(path) == record["logs"][name]["sha256"]
                    lines = path.read_text().splitlines()
                    if lines[-1] != "DIAGNOSTIC_EXIT 0":
                        rows.append({"system": tag[0], "event": event,
                                     "policy": policy, "round": round_number,
                                     "boot": tag, "valid": False,
                                     "error": lines[-1]})
                    else:
                        rows.append(parse_case(lines, tag[0], event, policy,
                                               round_number, tag))
    return rows


def main():
    linux_status = json.loads((ROOT / "linux-status.json").read_text())
    starry_status = json.loads((ROOT / "starry-status.json").read_text())
    assert linux_status["benchmark_sha256"] == starry_status["benchmark_sha256"]
    assert linux_status["board_id"] == starry_status["board_id"]
    rows = linux_rows(linux_status) + starry_rows(starry_status)
    grouped = {}
    for system in ("Linux RT", "A", "F"):
        for event in EVENTS:
            for policy in POLICIES:
                key = f"{system}:{event}:{policy}"
                valid = [row for row in rows if row["system"] == system and
                         row["event"] == event and row["policy"] == policy and row["valid"]]
                if not valid:
                    grouped[key] = {"valid_runs": 0}
                    continue
                grouped[key] = {field: statistics.median(row[field] for row in valid)
                                for field in FIELDS + ("p50_ns_diagnostic_only",)}
                grouped[key]["valid_runs"] = len(valid)
    output = {
        "diagnostic_only": True,
        "limitations": [
            "diagnostic benchmark differs from frozen benchmark and perturbs cache state",
            "end PMU read syscall lies inside the raw counter delta; adjacent read is only a calibration",
            "raw per-sample PMU deltas were not exported, only computed p50/p99",
            "one event is counted per run and Linux/Starry background activity differs",
            "not full20 acceptance or an exclusive source-function cost",
        ],
        "benchmark_sha256": linux_status["benchmark_sha256"],
        "board_id": linux_status["board_id"],
        "rows": rows,
        "group_medians": grouped,
        "invalid_runs": [row for row in rows if not row["valid"]],
    }
    (ROOT / "analysis.json").write_text(json.dumps(output, indent=2) + "\n")
    print(json.dumps({"group_medians": grouped,
                      "invalid_runs": output["invalid_runs"]}, indent=2))


if __name__ == "__main__":
    main()
