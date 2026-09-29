#!/usr/bin/env python3
"""Recheck the rejected Fair wake timing full20 screening from raw logs."""

import hashlib
import json
import re
from pathlib import Path
from statistics import median

ROOT = Path(__file__).resolve().parent
BASELINE = ROOT.parent.parent / "review-2477/full-pgo-2026-09-23/linux-rt-baseline.json"
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"


def tagged_json(text, prefix):
    return [json.loads(line[len(prefix) :]) for line in text.splitlines() if line.startswith(prefix)]


def row_key(row):
    return row["policy"], row["case"]


def main():
    linux = json.loads(BASELINE.read_text())
    archived = json.loads((ROOT / "full20/results.json").read_text())
    status = json.loads((ROOT / "status.json").read_text())
    assert linux["bench_sha256"] == archived["bench_sha256"] == status["benchmark_sha256"] == BENCH_SHA
    assert archived["board_id"] == status["board"] == "OrangePi-5-Plus-1"
    assert [item["tag"] for item in archived["rounds"]] == status["full20_order"]
    assert archived["image_sha256"] == {
        "A": status["historical_control_image_sha256"],
        "B": status["candidate_image_sha256"],
    }
    baseline = {row_key(row): row for row in linux["results"]}
    valid = {}
    invalid = {}

    for item in archived["rounds"]:
        tag = item["tag"]
        raw = (ROOT / f"full20/{tag}-full.log").read_bytes()
        assert hashlib.sha256(raw).hexdigest() == item["raw_log_sha256"]
        assert (ROOT / f"full20/{tag}.sha256").read_text().split()[0] == BENCH_SHA
        text = raw.decode()
        metadata = tagged_json(text, "WAKEUP_LATENCY_METADATA ")
        assert len(metadata) == 1
        assert {k: v for k, v in metadata[0].items() if k != "clock_pair_min_ns"} == {
            k: v for k, v in linux["metadata"][0].items() if k != "clock_pair_min_ns"
        }
        rows = tagged_json(text, "WAKEUP_LATENCY_RESULT ")
        assert len(rows) == len(baseline) == 20
        assert {row_key(row) for row in rows} == set(baseline)
        assert len(re.findall(r"^WAKEUP_LATENCY_CASE_START ", text, re.M)) == 20
        assert len(re.findall(r"^WAKEUP_LATENCY_CASE_DONE ", text, re.M)) == 20
        assert "WAKEUP_LATENCY_PASSED" in text and "WAKEUP_LATENCY_FAILED" not in text
        assert all(sum(row["histogram_counts"]) == row["samples"] for row in rows)
        bad = [row for row in rows if row["samples"] != row["attempted"] or row["not_parked"] or row["missed_deadlines"]]
        if bad:
            assert tag == "A2" and not item["valid"] and len(bad) == 1
            assert row_key(bad[0]) == ("other", "thread_futex_same_cpu")
            assert (bad[0]["samples"], bad[0]["attempted"], bad[0]["not_parked"]) == (19999, 20000, 1)
            invalid[tag] = bad[0]
        else:
            assert item["valid"] and rows == item["rows"]
            assert sum(row["samples"] for row in rows) == 380000
            valid[tag] = {row_key(row): row for row in rows}

    assert set(valid) == {"A1", "B1", "B2"} and set(invalid) == {"A2"}
    ratios = {
        key: baseline[key]["p50_ns"] / median([valid["B1"][key]["p50_ns"], valid["B2"][key]["p50_ns"]])
        for key in baseline
    }
    assert sum(ratio >= 0.9 for ratio in ratios.values()) == status["candidate_90pct_rows_two_boot_median"] == 10
    worst = min(ratios, key=ratios.get)
    assert worst == ("other", "thread_futex_same_cpu")
    assert median([valid["B1"][worst]["p50_ns"], valid["B2"][worst]["p50_ns"]]) == 26396
    assert round(100 * ratios[worst], 3) == status["candidate_worst_rt_over_candidate_pct"] == 32.043
    assert valid["A1"][worst]["p50_ns"] == 27125

    screening_risks = [
        (key, metric)
        for key in baseline
        for metric in ("p50_ns", "p99_ns", "p999_ns")
        if median([valid["B1"][key][metric], valid["B2"][key][metric]]) / valid["A1"][key][metric] >= 1.03
    ]
    assert len(screening_risks) == 6
    print("valid=A1,B1,B2 invalid=A2 rows_90pct=10/20 worst=OTHER/thread_futex_same_cpu rt_over_B=32.043%")
    print("A1_vs_B_screening_regressions_gte_3pct=6; formal_two_boot_A_gate=unproven")


if __name__ == "__main__":
    main()
