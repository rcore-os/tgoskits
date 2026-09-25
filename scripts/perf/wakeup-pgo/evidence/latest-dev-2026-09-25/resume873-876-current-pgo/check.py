#!/usr/bin/env python3
"""Recheck the rejected current-source PGO full20 screening from raw logs."""

import hashlib
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent
BASELINE = ROOT.parent.parent / "review-2477/full-pgo-2026-09-23/linux-rt-baseline.json"
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"


def rows_in(text, prefix):
    return [json.loads(line[len(prefix) :]) for line in text.splitlines() if line.startswith(prefix)]


def row_key(row):
    return row["policy"], row["case"]


def main():
    linux = json.loads(BASELINE.read_text())
    assert linux["bench_sha256"] == BENCH_SHA
    baseline_rows = {row_key(row): row for row in linux["results"]}
    archived = json.loads((ROOT / "full20/results.json").read_text())
    assert archived["bench_sha256"] == BENCH_SHA
    assert [item["tag"] for item in archived["rounds"]] == ["A1", "F1", "F2", "A2"]
    assert archived["board_id"] == "OrangePi-5-Plus-1"
    assert archived["image_sha256"] == {
        "A": "ad77bbb56abeeafedb90b767ff7f226cf99df757b838e97526b031c34757e747",
        "F": "4a872387073a948bf1416479164f7e6269e25d40eec8590ac26eba07cc8467d8",
    }

    valid = {}
    invalid = {}
    for item in archived["rounds"]:
        tag = item["tag"]
        data = (ROOT / f"full20/{tag}-full.log").read_bytes()
        assert hashlib.sha256(data).hexdigest() == item["raw_log_sha256"]
        assert (ROOT / f"full20/{tag}.sha256").read_text().split()[0] == BENCH_SHA
        text = data.decode()
        rows = rows_in(text, "WAKEUP_LATENCY_RESULT ")
        metadata = rows_in(text, "WAKEUP_LATENCY_METADATA ")
        assert len(metadata) == 1
        assert {key: value for key, value in metadata[0].items() if key != "clock_pair_min_ns"} == {
            key: value for key, value in linux["metadata"][0].items() if key != "clock_pair_min_ns"
        }
        assert len(rows) == len(baseline_rows) == 20
        assert {row_key(row) for row in rows} == set(baseline_rows)
        assert len(re.findall(r"^WAKEUP_LATENCY_CASE_START ", text, re.M)) == 20
        assert len(re.findall(r"^WAKEUP_LATENCY_CASE_DONE ", text, re.M)) == 20
        assert "WAKEUP_LATENCY_PASSED" in text and "WAKEUP_LATENCY_FAILED" not in text
        assert all(sum(row["histogram_counts"]) == row["samples"] for row in rows)
        bad = [row for row in rows if row["samples"] != row["attempted"] or row["not_parked"] or row["missed_deadlines"]]
        if bad:
            assert tag == "A2" and not item["valid"]
            assert len(bad) == 1
            assert row_key(bad[0]) == ("other", "thread_futex_same_cpu")
            assert (bad[0]["samples"], bad[0]["attempted"], bad[0]["not_parked"]) == (19999, 20000, 1)
            invalid[tag] = bad[0]
        else:
            assert item["valid"] and rows == item["rows"]
            assert sum(row["samples"] for row in rows) == 380000
            valid[tag] = {row_key(row): row for row in rows}

    assert set(valid) == {"A1", "F1", "F2"} and set(invalid) == {"A2"}
    passed = 0
    worst = (float("inf"), None)
    for key, rt in baseline_rows.items():
        f_sum = valid["F1"][key]["p50_ns"] + valid["F2"][key]["p50_ns"]
        passed += rt["p50_ns"] * 20 >= f_sum * 9
        ratio = 200 * rt["p50_ns"] / f_sum
        if ratio < worst[0]:
            worst = ratio, key
    assert passed == 10
    assert worst[1] == ("other", "thread_futex_same_cpu")
    assert valid["F1"][worst[1]]["p50_ns"] == valid["F2"][worst[1]]["p50_ns"] == 14875
    assert round(worst[0], 3) == 56.861
    assert (ROOT / "full20-analysis.json").exists()
    print("valid=A1,F1,F2 invalid=A2 rows_90pct=10/20 worst=OTHER/thread_futex_same_cpu rt_over_F=56.861%")


if __name__ == "__main__":
    main()
