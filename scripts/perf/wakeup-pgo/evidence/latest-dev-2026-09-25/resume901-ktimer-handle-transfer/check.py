#!/usr/bin/env python3
"""Recalculate the ktimer handle-transfer screening from raw full20 logs."""

import hashlib
import json
import re
from pathlib import Path
from statistics import median

ROOT = Path(__file__).resolve().parent
BASELINE = ROOT.parent.parent / "review-2477/full-pgo-2026-09-23/linux-rt-baseline.json"
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"
PATCH_SHA = "31d2a41caaae729ddd3d2003e970b44386efaffd09c8df3a9befda6a793e66dc"
BUILD_SHA = "97a5d39843b42bca2e965ce83ebfc6458918dd1566eee46db73232f65a3208b5"


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def tagged_json(text, prefix):
    return [json.loads(line[len(prefix) :]) for line in text.splitlines() if line.startswith(prefix)]


def row_key(row):
    return row["policy"], row["case"]


def main():
    linux = json.loads(BASELINE.read_text())
    result = json.loads((ROOT / "full20/results.json").read_text())
    session = json.loads((ROOT / "full20/session.json").read_text())
    assert linux["bench_sha256"] == result["bench_sha256"] == BENCH_SHA
    assert result["board_id"] == session["board_id"] == "OrangePi-5-Plus-1"
    assert result["session_id"] == session["session_id"]
    assert result["source_head"] == "b292a098bb60ef604e7677c37cd95d926ff08200"
    assert result["source_patch_sha256"] == sha(ROOT / "source.patch") == PATCH_SHA
    assert result["build_sha256"] == sha(ROOT / "ordinary.toml") == BUILD_SHA
    assert result["image_sha256"] == {
        "A": "31d68c9c739af52722d8596cdafd52796b571ee26d690a8b2a174aeb04bc1ca9",
        "B": "7cd0b108614985db8df6d98ecc5bd6da3b2d0f7ad7acefab5dfa547f22101be2",
    }

    baseline = {row_key(row): row for row in linux["results"]}
    assert len(baseline) == 20
    valid = {}
    assert [item["tag"] for item in result["rounds"]] == ["A1", "B1", "B2", "A2"]
    for item in result["rounds"]:
        tag = item["tag"]
        raw = ROOT / f"full20/{tag}-full.log"
        assert sha(raw) == item["raw_log_sha256"]
        assert (ROOT / f"full20/{tag}.sha256").read_text().split()[0] == BENCH_SHA
        text = raw.read_text()
        metadata = tagged_json(text, "WAKEUP_LATENCY_METADATA ")
        assert len(metadata) == 1
        assert {k: v for k, v in metadata[0].items() if k != "clock_pair_min_ns"} == {
            k: v for k, v in linux["metadata"][0].items() if k != "clock_pair_min_ns"
        }
        rows = tagged_json(text, "WAKEUP_LATENCY_RESULT ")
        assert len(rows) == len({row_key(row) for row in rows}) == 20
        assert {row_key(row) for row in rows} == set(baseline)
        assert len(re.findall(r"^WAKEUP_LATENCY_CASE_START ", text, re.M)) == 20
        assert len(re.findall(r"^WAKEUP_LATENCY_CASE_DONE ", text, re.M)) == 20
        assert "WAKEUP_LATENCY_PASSED" in text and "WAKEUP_LATENCY_FAILED" not in text
        assert all(sum(row["histogram_counts"]) == row["samples"] for row in rows)
        bad = [row for row in rows if row["samples"] != row["attempted"]
               or row["not_parked"] or row["missed_deadlines"]]
        if tag == "B1":
            assert not item["valid"] and not item["rows"] and len(bad) == 1
            assert row_key(bad[0]) == ("other", "thread_futex_same_cpu")
            assert (bad[0]["samples"], bad[0]["attempted"], bad[0]["not_parked"],
                    bad[0]["missed_deadlines"]) == (19999, 20000, 1, 0)
            assert "samples!=attempted" in item["error"]
        else:
            assert item["valid"] and not bad and rows == item["rows"]
            assert sum(row["samples"] for row in rows) == 380000
            valid[tag] = {row_key(row): row for row in rows}

    assert set(valid) == {"A1", "B2", "A2"}
    candidate = valid["B2"]
    ratios = {key: baseline[key]["p50_ns"] / row["p50_ns"]
              for key, row in candidate.items()}
    assert sum(10 * baseline[key]["p50_ns"] >= 9 * row["p50_ns"]
               for key, row in candidate.items()) == 10
    worst = min(ratios, key=ratios.get)
    assert worst == ("other", "thread_futex_same_cpu")
    assert (baseline[worst]["p50_ns"], candidate[worst]["p50_ns"]) == (8458, 28000)
    timer = ("other", "absolute_timer_same_cpu")
    control_p50 = median([valid[tag][timer]["p50_ns"] for tag in ("A1", "A2")])
    assert control_p50 == 54500
    assert candidate[timer]["p50_ns"] == 56166
    assert round(100 * (candidate[timer]["p50_ns"] / control_p50 - 1), 3) == 3.057

    regressions = {
        (key, metric): 100 * (candidate[key][metric] /
                              median([valid[tag][key][metric] for tag in ("A1", "A2")]) - 1)
        for key in baseline for metric in ("p50_ns", "p99_ns", "p999_ns")
    }
    risks = {key: value for key, value in regressions.items() if value >= 3}
    assert len(risks) == 8
    assert round(risks[("other", "absolute_timer_same_cpu"), "p50_ns"], 3) == 3.057
    assert round(risks[("fifo", "absolute_timer_same_cpu"), "p999_ns"], 3) == 78.593
    assert round(risks[("fifo", "futex_wait_mismatch"), "p99_ns"], 3) == 16.686
    print("valid=A1,B2,A2 invalid=B1; B2_90pct=10/20; worst_rt_over_B=30.207%")
    print("OTHER_timer_A_median=54500 B2=56166 exploratory_delta=+3.057%")
    print("single_B_screening_regressions_gte_3pct=8; formal_two_boot_B_gate=unproven")


if __name__ == "__main__":
    main()
