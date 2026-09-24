#!/usr/bin/env python3
"""Check archived full20 integrity and the rejected single-pair PGO screen."""

import hashlib
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parent
BASELINE = ROOT.parent / "review-2477/full-pgo-2026-09-23/linux-rt-baseline.json"
METRICS = ("p50_ns", "p99_ns", "p999_ns")


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read_full20(path, baseline):
    lines = path.read_text().splitlines()
    assert sum(line.startswith("WAKEUP_LATENCY_CASE_START ") for line in lines) == 20
    assert sum(line.startswith("WAKEUP_LATENCY_CASE_DONE ") for line in lines) == 20
    assert lines.count("WAKEUP_LATENCY_PASSED") == 1
    assert not any("WAKEUP_LATENCY_FAILED" in line for line in lines)
    metadata = [json.loads(line.split(" ", 1)[1]) for line in lines
                if line.startswith("WAKEUP_LATENCY_METADATA ")]
    assert len(metadata) == 1
    comparable = lambda item: {key: value for key, value in item.items()
                               if key != "clock_pair_min_ns"}
    assert comparable(metadata[0]) == comparable(baseline["metadata"][0])
    rows = [json.loads(line.split(" ", 1)[1]) for line in lines
            if line.startswith("WAKEUP_LATENCY_RESULT ")]
    keys = {(row["policy"], row["case"]) for row in rows}
    assert len(rows) == len(keys) == 20
    assert sum(row["samples"] for row in rows) == 380000
    assert all(row["samples"] == row["attempted"]
               and row["not_parked"] == row["missed_deadlines"] == 0
               and sum(row["histogram_counts"]) == row["samples"] for row in rows)
    for marker in ("WAKEUP_LATENCY_CASE_START ", "WAKEUP_LATENCY_CASE_DONE "):
        marked = [dict(part.split("=", 1) for part in line[len(marker):].split())
                  for line in lines if line.startswith(marker)]
        assert {(row["policy"], row["case"]) for row in marked} == keys
    return {(row["policy"], row["case"]): row for row in rows}


def main():
    baseline = json.loads(BASELINE.read_text())
    rt = {(row["policy"], row["case"]): row for row in baseline["results"]}
    assert len(rt) == 20
    old = json.loads((ROOT / "resume771/status.json").read_text())
    new = json.loads((ROOT / "resume773/status.json").read_text())
    assert old["source_head"] == "c346962754ff7a96359a928f07ffaedbcfeb9ca5"
    assert new["source_head"] == "05175ca38823b631a73777b0130226ddfa558439"
    assert old["benchmark_sha256"] == new["benchmark_sha256"] == baseline["bench_sha256"]
    assert old["board"] == new["board"] == "OrangePi-5-Plus-1"

    a_path = ROOT / "resume771/A1-full.log"
    f_path = ROOT / "resume771/F1-full.log"
    dev_path = ROOT / "resume773/A1-full.log"
    assert sha256(a_path) == old["ordinary_log_sha256"]
    assert sha256(f_path) == old["candidate_log_sha256"]
    assert sha256(dev_path) == new["full20_log_sha256"]
    a, f, dev = (read_full20(path, baseline) for path in (a_path, f_path, dev_path))
    assert set(a) == set(f) == set(dev) == set(rt)

    analysis = json.loads((ROOT / "resume771/analysis.json").read_text())
    assert sha256(ROOT / "resume771/analysis.json") == old["analysis_sha256"]
    assert analysis["source_head"] == old["source_head"]
    assert {(row["policy"], row["case"]) for row in analysis["table"]} == set(rt)
    assert analysis["p50_pass90"] == sum(rt[key]["p50_ns"] / f[key]["p50_ns"] >= 0.9
                                        for key in rt) == 0
    assert [len(analysis["regressions_ge_3_percent"][metric])
            for metric in METRICS] == [11, 11, 10]
    for row in analysis["table"]:
        key = row["policy"], row["case"]
        assert (row["linux_rt_p50"], row["ordinary_p50"], row["candidate_p50"]) == (
            rt[key]["p50_ns"], a[key]["p50_ns"], f[key]["p50_ns"])
        assert abs(row["linux_rt_over_candidate_percent"] -
                   100 * rt[key]["p50_ns"] / f[key]["p50_ns"]) < 1e-9
        for metric in METRICS:
            comparison = row["comparison"][metric]
            assert comparison["ordinary"] == a[key][metric]
            assert comparison["candidate"] == f[key][metric]
            assert abs(comparison["regression_percent"] -
                       100 * (f[key][metric] / a[key][metric] - 1)) < 1e-9
    for metric in METRICS:
        expected = {key: 100 * (f[key][metric] / a[key][metric] - 1)
                    for key in rt if f[key][metric] / a[key][metric] - 1 >= 0.03}
        actual = {(row["policy"], row["case"]): row["regression_percent"]
                  for row in analysis["regressions_ge_3_percent"][metric]}
        assert actual.keys() == expected.keys()
        assert all(abs(actual[key] - expected[key]) < 1e-9 for key in expected)
    assert dev["other", "thread_futex_same_cpu"]["p50_ns"] == new["other_thread_futex_same_cpu_p50_ns"]
    print("resume771: valid A1/F1, 0/20 at 90%, rejected")
    print("resume773: valid latest-dev ordinary A1, no candidate comparison")


if __name__ == "__main__":
    main()
