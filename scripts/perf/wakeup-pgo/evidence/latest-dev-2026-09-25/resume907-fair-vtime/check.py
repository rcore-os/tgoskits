#!/usr/bin/env python3
"""Recalculate the rejected Fair virtual-time full20 screening."""

import hashlib
import json
import re
from pathlib import Path
from statistics import median

ROOT = Path(__file__).resolve().parent
BASELINE = ROOT.parent.parent / "review-2477/full-pgo-2026-09-23/linux-rt-baseline.json"
SOURCE_HEAD = "b292a098bb60ef604e7677c37cd95d926ff08200"
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"
PATCH_SHA = "b824173a37744537d972c168b61178a21031f86ce21a21903e28ea134c729a50"
BUILD_SHA = "97a5d39843b42bca2e965ce83ebfc6458918dd1566eee46db73232f65a3208b5"
IMAGES = {
    "A": "31d68c9c739af52722d8596cdafd52796b571ee26d690a8b2a174aeb04bc1ca9",
    "B": "448a418b0ef54a3d1b88dbf4af5ee434ba1199a7f83a00363e0f7f7ca7ce940a",
}
METRICS = ("p50_ns", "p99_ns", "p999_ns")


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
    assert result["source_head"] == SOURCE_HEAD
    assert result["source_patch_sha256"] == sha(ROOT / "source.patch") == PATCH_SHA
    assert result["build_sha256"] == sha(ROOT / "ordinary.toml") == BUILD_SHA
    assert result["image_sha256"] == IMAGES

    baseline = {row_key(row): row for row in linux["results"]}
    assert len(baseline) == 20
    assert [item["tag"] for item in result["rounds"]] == ["A1", "B1", "B2", "A2"]
    rounds = {}
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
        assert all(row["samples"] == row["attempted"] and not row["not_parked"]
                   and not row["missed_deadlines"] for row in rows)
        assert sum(row["samples"] for row in rows) == 380000
        assert item["valid"] and item["error"] is None and item["rows"] == rows
        assert item["image_sha256"] == IMAGES[tag[0]]
        rounds[tag] = {row_key(row): row for row in rows}

    control = {key: {metric: median(rounds[tag][key][metric] for tag in ("A1", "A2"))
                     for metric in METRICS} for key in baseline}
    candidate = {key: {metric: median(rounds[tag][key][metric] for tag in ("B1", "B2"))
                       for metric in METRICS} for key in baseline}
    passed = sum(10 * baseline[key]["p50_ns"] >= 9 * candidate[key]["p50_ns"]
                 for key in baseline)
    worst = min(baseline, key=lambda key: baseline[key]["p50_ns"] / candidate[key]["p50_ns"])
    regressions = sorted((100 * (candidate[key][metric] / control[key][metric] - 1), key, metric)
                         for key in baseline for metric in METRICS)
    assert passed == 10 and worst == ("other", "thread_futex_same_cpu")
    assert (baseline[worst]["p50_ns"], control[worst]["p50_ns"],
            candidate[worst]["p50_ns"]) == (8458, 28000, 27708.5)
    assert sum(delta >= 3 for delta, _, _ in regressions) == 8
    print("valid=A1,B1,B2,A2 samples=380000/380000 per boot missed=0 not_parked=0")
    print(f"90pct={passed}/20 worst={worst} RT/candidate="
          f"{100 * baseline[worst]['p50_ns'] / candidate[worst]['p50_ns']:.3f}%")
    print("policy case RT_p50 A_p50 B_p50 RT/B_pct")
    for key in sorted(baseline):
        print(*key, baseline[key]["p50_ns"], control[key]["p50_ns"],
              candidate[key]["p50_ns"],
              f"{100 * baseline[key]['p50_ns'] / candidate[key]['p50_ns']:.3f}%")
    print("regressions_gte_3pct=8 (A1/A2 median to B1/B2 median)")
    for delta, key, metric in reversed(regressions):
        if delta < 3:
            break
        print(*key, metric, control[key][metric], candidate[key][metric], f"+{delta:.3f}%")


if __name__ == "__main__":
    main()
