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


def read_full20(path, baseline, invalid_key=None):
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
    assert sum(row["attempted"] for row in rows) == 380000
    invalid = {(row["policy"], row["case"]) for row in rows
               if row["samples"] != row["attempted"] or row["not_parked"]
               or row["missed_deadlines"]}
    assert invalid == ({invalid_key} if invalid_key else set())
    assert all(sum(row["histogram_counts"]) == row["samples"] for row in rows)
    if invalid_key:
        assert sum(row["samples"] for row in rows) == 379999
        row = next(row for row in rows if (row["policy"], row["case"]) == invalid_key)
        assert (row["samples"], row["attempted"], row["not_parked"],
                row["missed_deadlines"]) == (19999, 20000, 1, 0)
    else:
        assert sum(row["samples"] for row in rows) == 380000
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

    ordinary = json.loads((ROOT / "resume786/status.json").read_text())
    candidate = json.loads((ROOT / "resume785/status.json").read_text())
    first = json.loads((ROOT / "resume787/status.json").read_text())
    repeat = json.loads((ROOT / "resume788/status.json").read_text())
    assert ordinary["bin_sha256"] == first["ordinary_image_sha256"]
    assert candidate["bin_sha256"] == first["pgo_image_sha256"] == repeat["pgo_image_sha256"]
    assert candidate["profile_sha256"] == "7d71a724fd5ce66e4faf15280995602392fc6ff8e6d41fc197cdbc0eb8e67156"
    assert ordinary["temporary_exporter_patch_sha256"] == candidate["temporary_exporter_patch_sha256"]
    assert first["source_patch_sha256"] == repeat["source_patch_sha256"] == candidate["temporary_exporter_patch_sha256"]
    assert first["source_commit"] == repeat["source_commit"] == "69a33650763538692fafea27c869870ed0313642"
    assert first["benchmark_sha256"] == repeat["benchmark_sha256"] == baseline["bench_sha256"]
    assert first["board"] == repeat["board"] == "OrangePi-5-Plus-1"

    paths = {"A1": ROOT / "resume787/A1-full.log",
             "F1": ROOT / "resume787/F1-full.log",
             "F2": ROOT / "resume788/F2-full.log"}
    assert sha256(paths["A1"]) == first["raw_log_sha256"]["A1"]
    assert sha256(paths["F1"]) == first["raw_log_sha256"]["F1"]
    assert sha256(paths["F2"]) == repeat["raw_log_sha256"]
    runs = {label: read_full20(path, baseline,
                               ("other", "thread_futex_same_cpu") if label == "F1" else None)
            for label, path in paths.items()}
    assert all(set(rows) == set(rt) for rows in runs.values())

    for label, evidence in (("F1", "resume787"), ("F2", "resume788")):
        analysis = json.loads((ROOT / evidence / "analysis.json").read_text())
        assert analysis["both_runs_valid"] == (label == "F2")
        assert bool(analysis["invalid_rows"]) == (label == "F1")
        assert len(analysis["comparison_diagnostic_only"]) == 20
        assert not analysis["complete_row_regressions_ge_3_percent"]
        for item in analysis["comparison_diagnostic_only"]:
            key = item["policy"], item["case"]
            assert item["linux_rt_p50_ns"] == rt[key]["p50_ns"]
            assert abs(item["rt_over_f_p50_percent"] -
                       round(100 * rt[key]["p50_ns"] / runs[label][key]["p50_ns"], 2)) < 0.01
            assert item["valid"] == (label == "F2" or key != ("other", "thread_futex_same_cpu"))
            for metric in METRICS:
                comparison = item["metrics"][metric]
                a_value = runs["A1"][key][metric]
                f_value = runs[label][key][metric]
                assert (comparison["A1"], comparison[label]) == (a_value, f_value)
                assert abs(comparison["delta_percent"] -
                           round(100 * (f_value / a_value - 1), 2)) < 0.01

    a = runs["A1"]
    f = runs["F2"]
    assert all(f[key][metric] < a[key][metric] * 1.03
               for key in rt for metric in METRICS)
    assert sum(rt[key]["p50_ns"] / f[key]["p50_ns"] >= 0.9 for key in rt) == 11
    worst = ("other", "thread_futex_same_cpu")
    assert (rt[worst]["p50_ns"], a[worst]["p50_ns"], f[worst]["p50_ns"]) == (8458, 27417, 16625)
    assert not (rt[worst]["p50_ns"] / f[worst]["p50_ns"] >= 0.9)
    print("resume787: valid A1, invalid F1 (one not_parked sample)")
    print("resume788: valid F2, A1/F2 diagnostic 11/20 at 90%, no >=3% regression")


if __name__ == "__main__":
    main()
