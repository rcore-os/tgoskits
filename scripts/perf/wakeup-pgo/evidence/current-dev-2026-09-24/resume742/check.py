#!/usr/bin/env python3
"""Verify resume742 full20 logs and recompute the two-boot comparisons."""

import gzip
import hashlib
import json
import re
from pathlib import Path
from statistics import median

HERE = Path(__file__).resolve().parent
CURRENT = HERE.parent
FROZEN = CURRENT.parent / "review-2477/full-pgo-2026-09-23/linux-rt-baseline.json"
METRICS = ("p50_ns", "p99_ns", "p999_ns")
VALID = ("A1", "A2", "B1", "B3")
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read_log(path, baseline, allow_invalid=False):
    log = path.read_text()
    rows = [json.loads(line.split(" ", 1)[1]) for line in log.splitlines()
            if line.startswith("WAKEUP_LATENCY_RESULT ")]
    metas = [json.loads(line.split(" ", 1)[1]) for line in log.splitlines()
             if line.startswith("WAKEUP_LATENCY_METADATA ")]
    expected = {(row["policy"], row["case"]) for row in baseline["results"]}
    assert len(rows) == 20 and len(metas) == 1, path
    assert len({(row["policy"], row["case"]) for row in rows}) == 20, path
    assert {(row["policy"], row["case"]) for row in rows} == expected, path
    assert {key: value for key, value in metas[0].items() if key != "clock_pair_min_ns"} == \
           {key: value for key, value in baseline["metadata"][0].items()
            if key != "clock_pair_min_ns"}, path
    assert metas[0]["clock_pair_min_ns"] > 0, path
    starts = re.findall(r"^WAKEUP_LATENCY_CASE_START case=(\S+) policy=(\S+)$", log, re.M)
    dones = re.findall(r"^WAKEUP_LATENCY_CASE_DONE case=(\S+) policy=(\S+)$", log, re.M)
    assert len(starts) == len(dones) == 20, path
    assert {(policy, case) for case, policy in starts} == expected, path
    assert {(policy, case) for case, policy in dones} == expected, path
    markers = ("WAKEUP_LATENCY_PROFILE_START", "WAKEUP_LATENCY_PROFILE_DONE",
               "WAKEUP_LATENCY_PASSED")
    assert all(marker in log for marker in markers), path
    assert log.index(markers[0]) < log.index(markers[1]) < log.index(markers[2]), path
    assert "WAKEUP_LATENCY_FAILED" not in log and "panicked at" not in log, path
    assert all(sum(row["histogram_counts"]) == row["samples"] for row in rows), path
    invalid = [row for row in rows if row["samples"] != row["attempted"] or
               row["not_parked"] or row["missed_deadlines"]]
    if allow_invalid:
        assert len(invalid) == 1, path
        row = invalid[0]
        assert (row["policy"], row["case"], row["samples"], row["attempted"],
                row["not_parked"], row["missed_deadlines"]) == \
               ("other", "thread_futex_same_cpu", 19999, 20000, 1, 0), path
    else:
        assert not invalid and sum(row["samples"] for row in rows) == 380000, path
    return {(row["policy"], row["case"]): row for row in rows}


def compare(control, candidate):
    changes = []
    for key in control[0]:
        for metric in METRICS:
            before = median(row[key][metric] for row in control)
            after = median(row[key][metric] for row in candidate)
            changes.append({"policy": key[0], "case": key[1], "metric": metric,
                            "control_ns": before, "candidate_ns": after,
                            "regression_pct": round(100 * (after / before - 1), 4)})
    return sorted(changes, key=lambda item: item["regression_pct"], reverse=True)


def main():
    baseline = json.loads(FROZEN.read_text())
    assert baseline["bench_sha256"] == BENCH_SHA
    status = json.loads((HERE / "status.json").read_text())
    assert status["source_head"] == "b59894344a23786d7b5ba57b56eabeb716eb1ba6"
    patch = gzip.decompress((HERE / "candidate.patch.gz").read_bytes())
    assert status["source_patch_sha256"] == hashlib.sha256(patch).hexdigest()
    assert status["bench_sha256"] == BENCH_SHA
    assert status["board_id"] == baseline["board"] == "OrangePi-5-Plus-1"
    assert status["images"]["A"] != status["images"]["B"]
    assert [session["sequence"] for session in status["sessions"]] == \
           [["A1", "B1", "B2"], ["A2", "B3"]]
    for index, session in enumerate(status["sessions"], start=1):
        name = "session.json" if index == 1 else f"session{index}.json"
        assert session["id"] == json.loads((HERE / name).read_text())["session_id"]
        assert session["released"]
    assert set(status["logs"]) == {*VALID, "B2"}
    for tag, entry in status["logs"].items():
        assert entry["sha256"] == sha(HERE / f"{tag}-full.log")
        assert entry["valid"] == (tag in VALID)

    logs = {}
    for tag in (*VALID, "B2"):
        assert (HERE / f"{tag}.sha256").read_text().split()[0] == BENCH_SHA
        logs[tag] = read_log(HERE / f"{tag}-full.log", baseline, allow_invalid=tag == "B2")
    for tag, log_name in (("F1", "resume711-F1-full.log"),
                          ("F2", "resume712-F1-full.log")):
        logs[tag] = read_log(CURRENT / log_name, baseline)

    ab = compare([logs["A1"], logs["A2"]], [logs["B1"], logs["B3"]])
    af = compare([logs["A1"], logs["A2"]], [logs["F1"], logs["F2"]])
    focus = ("other", "thread_futex_same_cpu")
    assert median(logs[tag][focus]["p50_ns"] for tag in ("A1", "A2")) == 18812.5
    assert median(logs[tag][focus]["p50_ns"] for tag in ("B1", "B3")) == 18229.5
    assert ab[0]["case"] == "process_futex_cross_cpu" and ab[0]["metric"] == "p999_ns"
    assert ab[0]["regression_pct"] == 25.0701
    assert af[0]["case"] == "absolute_timer_same_cpu" and af[0]["metric"] == "p999_ns"
    assert af[0]["regression_pct"] == 36.9558
    print(json.dumps({"valid_full20": list(VALID), "invalid_full20": ["B2"],
                      "ordinary_candidate_regressions_at_least_3pct":
                      [row for row in ab if row["regression_pct"] >= 3],
                      "pgo_vs_new_control_regressions_at_least_3pct":
                      [row for row in af if row["regression_pct"] >= 3]}, indent=2))


if __name__ == "__main__":
    main()
