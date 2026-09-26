#!/usr/bin/env python3
"""Validate same-binary stratified PMU diagnostics, not full20 acceptance."""

import hashlib
import json
import statistics
from pathlib import Path


ROOT = Path(__file__).resolve().parent
EVENTS = ("instructions", "cycles", "l1i_refill")
POLICIES = ("other", "fifo")
TAGS = ("A1", "F1", "F2", "A2")


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def one_json(lines, prefix):
    rows = [json.loads(line[len(prefix):]) for line in lines
            if line.startswith(prefix)]
    assert len(rows) == 1, (prefix, len(rows))
    return rows[0]


def parse_case(lines, system, boot, event, policy, round_number):
    metadata = one_json(lines, "WAKEUP_LATENCY_METADATA ")
    result = one_json(lines, "WAKEUP_LATENCY_RESULT ")
    window = one_json(lines, "WAKEUP_PMU_WINDOW ")
    strata = [json.loads(line[len("WAKEUP_PMU_STRATUM "):]) for line in lines
              if line.startswith("WAKEUP_PMU_STRATUM ")]
    assert metadata["warmup"] == 1000 and metadata["handoff_samples"] == 20000
    assert metadata["sender_cpu"] == 0 and metadata["fifo_priority"] == 80
    assert result["case"] == "thread_futex_same_cpu"
    assert result["policy"] == policy and result["attempted"] == 20000
    assert sum(result["histogram_counts"]) == result["samples"]
    assert window["event"] == event and window["samples"] == result["samples"]
    assert window["adjusted_p50"] == max(0, window["raw_p50"] - window["read_p50"])
    assert len(strata) == 2 and {row["stratum"] for row in strata} == {"before", "after"}
    assert all(row["event"] == event for row in strata)
    assert sum(row["samples"] for row in strata) == result["samples"]
    for row in strata:
        if row["samples"]:
            assert row["adjusted_p50"] == max(0, row["raw_p50"] - row["read_p50"])
            assert row["p50_ns"] >= 0
        else:
            assert "raw_p50" not in row
    assert "WAKEUP_LATENCY_PASSED" in lines
    valid = (result["samples"] == 20000 and result["not_parked"] == 0
             and result["missed_deadlines"] == 0)
    return {
        "system": system, "boot": boot, "event": event,
        "policy": policy, "round": round_number, "valid": valid,
        "samples": result["samples"], "not_parked": result["not_parked"],
        "missed_deadlines": result["missed_deadlines"],
        "p50_ns_diagnostic_only": result["p50_ns"],
        "window": window, "strata": strata,
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
    for event in EVENTS:
        for round_number in (1, 2):
            for policy in POLICIES:
                start = f"RESUME813_CASE_START event={event} policy={policy} round={round_number}"
                done = f"RESUME813_CASE_DONE event={event} policy={policy} round={round_number} exit=0"
                begin = content.index(start, cursor)
                end = content.index(done, begin)
                rows.append(parse_case(content[begin:end].splitlines(),
                                       "Linux RT", "L1", event, policy, round_number))
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
        for event in EVENTS:
            for round_number in (1, 2):
                for policy in POLICIES:
                    name = f"{event}-{policy}-{round_number}.log"
                    path = ROOT / f"{tag}-{name}"
                    assert sha(path) == record["logs"][name]["sha256"]
                    lines = path.read_text().splitlines()
                    assert lines[-1] == "DIAGNOSTIC_EXIT 0"
                    rows.append(parse_case(lines, tag[0], tag, event,
                                           policy, round_number))
    return rows


def main():
    linux = json.loads((ROOT / "linux-status.json").read_text())
    starry = json.loads((ROOT / "starry-status.json").read_text())
    assert linux["benchmark_sha256"] == starry["benchmark_sha256"]
    rows = linux_rows(linux) + starry_rows(starry)
    grouped = {}
    for system in ("Linux RT", "A", "F"):
        for event in EVENTS:
            for policy in POLICIES:
                valid = [row for row in rows if row["system"] == system
                         and row["event"] == event and row["policy"] == policy
                         and row["valid"]]
                for stratum in ("before", "after"):
                    strata = [next(part for part in row["strata"]
                                   if part["stratum"] == stratum)
                              for row in valid]
                    nonempty = [part for part in strata if part["samples"]]
                    grouped[f"{system}:{event}:{policy}:{stratum}"] = {
                        "valid_runs": len(valid),
                        "nonempty_runs": len(nonempty),
                        "sample_count_median": statistics.median(
                            part["samples"] for part in strata),
                        "adjusted_p50_median": statistics.median(
                            part["adjusted_p50"] for part in nonempty)
                            if nonempty else None,
                        "p50_ns_diagnostic_only": statistics.median(
                            part["p50_ns"] for part in nonempty)
                            if nonempty else None,
                    }
    output = {
        "diagnostic_only": True,
        "benchmark_sha256": linux["benchmark_sha256"],
        "board_id": linux["board_id"],
        "limitations": [
            "diagnostic benchmark perturbs cache state and differs from frozen full20",
            "adjacent PMU read is an approximate, not exact, calibration",
            "sender marker does not distinguish in-syscall from return-boundary preemption",
            "raw per-sample PMU deltas are not exported, only stratum summaries",
            "Linux and Starry background activity differs",
        ],
        "rows": rows, "group_medians": grouped,
        "invalid_runs": [row for row in rows if not row["valid"]],
    }
    (ROOT / "analysis.json").write_text(json.dumps(output, indent=2) + "\n")
    print(json.dumps({"group_medians": grouped,
                      "invalid_runs": output["invalid_runs"]}, indent=2))


if __name__ == "__main__":
    main()
