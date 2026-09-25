#!/usr/bin/env python3
"""Check archived full20 integrity and the rejected single-pair PGO screen."""

import gzip
import hashlib
import json
from pathlib import Path
from statistics import median


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


def read_counter_snapshot(path):
    entries = [line.split() for line in path.read_text().splitlines()]
    assert all(len(entry) == 2 for entry in entries)
    counters = {key: int(value) for key, value in entries}
    assert len(counters) == len(entries)
    return counters


def check_diagnostic(name, baseline, case, valid_rounds):
    directory = ROOT / name
    result = json.loads((directory / "results.json").read_text())
    assert result["diagnostic_only"] is True
    assert result["board_id"] == "OrangePi-5-Plus-1"
    assert result["bench_sha256"] == baseline["bench_sha256"]
    assert result["session_id"] == json.loads((directory / "session.json").read_text())["session_id"]
    if name == "resume799":
        patch = gzip.decompress((directory / "probe.patch.gz").read_bytes())
        assert hashlib.sha256(patch).hexdigest() == result["source_patch_sha256"]
        assert result["source_head"] == "69a33650763538692fafea27c869870ed0313642"
    else:
        assert result["source_head"] == "05175ca38823b631a73777b0130226ddfa558439"

    assert len(result["rounds"]) == 4
    assert [(row["policy"], row["round"]) for row in result["rounds"]] == [
        ("fifo", 1), ("fifo", 2), ("other", 1), ("other", 2)
    ]
    for row in result["rounds"]:
        policy, number = row["policy"], row["round"]
        assert row["case"] == case
        assert row["valid"] == ((policy, number) in valid_rounds)
        prefix = f"{policy}-{case}-{number}"
        raw_log = directory / f"{prefix}.log"
        assert sha256(raw_log) == row["raw_log_sha256"]
        lines = raw_log.read_text().splitlines()
        assert lines.count("WAKEUP_LATENCY_PASSED") == 1
        metadata = [json.loads(line.split(" ", 1)[1]) for line in lines
                    if line.startswith("WAKEUP_LATENCY_METADATA ")]
        assert len(metadata) == 1
        assert {key: value for key, value in metadata[0].items()
                if key != "clock_pair_min_ns"} == {
                    key: value for key, value in baseline["metadata"][0].items()
                    if key != "clock_pair_min_ns"}
        rows = [json.loads(line.split(" ", 1)[1]) for line in lines
                if line.startswith("WAKEUP_LATENCY_RESULT ")]
        assert rows == [row["benchmark"]]
        bench = row["benchmark"]
        assert (bench["policy"], bench["case"]) == (policy, case)
        assert bench["attempted"] == 20000
        assert sum(bench["histogram_counts"]) == bench["samples"]
        assert row["valid"] == (bench["samples"] == bench["attempted"]
                                and bench["not_parked"] == 0
                                and bench["missed_deadlines"] == 0)

        before = read_counter_snapshot(directory / f"{prefix}-before")
        after = read_counter_snapshot(directory / f"{prefix}-after")
        assert before.keys() == after.keys() == row["delta"].keys()
        assert {key: after[key] - before[key] for key in before} == row["delta"]
    return result["rounds"]


def check_recent_path_diagnostics(baseline):
    root = ROOT / "resume829-833-path-diagnostics"
    source = "69a33650763538692fafea27c869870ed0313642"
    cases = {"830": ("absolute_timer_same_cpu", 10000),
             "831": ("absolute_timer_same_cpu", 10000),
             "833": ("thread_futex_cross_cpu", 20000)}
    images = {
        "830": "bfc70a0df2ea0d3c1991b9ac81474f9dbb7e76535fdab3e087f15ac8da27b21e",
        "831": "4b9173ee3ecc34cf2c0930166999fe0d31d6eabda60e732c85c0957cae8e7500",
        "833": "68ff5e32dead62e21fa96c67572fcf8acf3c6b4a591f9c228ac534fb58196ad1",
    }
    for identifier, (case, samples) in cases.items():
        directory = root / f"resume{identifier}"
        run = directory / "run1"
        result = json.loads((run / "results.json").read_text())
        assert result["diagnostic_only"] is True
        assert result["board_id"] == "OrangePi-5-Plus-2"
        assert result["source_head"] == source
        assert result["bench_sha256"] == baseline["bench_sha256"]
        assert result["image_sha256"] == images[identifier]
        assert result["session_id"] == json.loads((run / "session.json").read_text())["session_id"]
        patch = gzip.decompress((directory / "probe.patch.gz").read_bytes())
        if identifier == "833":
            assert hashlib.sha256(patch).hexdigest() == result["source_patch_sha256"]
        assert [(row["policy"], row["round"]) for row in result["rounds"]] == [
            ("fifo", 1), ("fifo", 2), ("other", 1), ("other", 2)]
        for row in result["rounds"]:
            policy, number = row["policy"], row["round"]
            bench = row["benchmark"]
            assert row["valid"] is True
            assert (bench["policy"], bench["case"]) == (policy, case)
            assert (bench["samples"], bench["attempted"], bench["not_parked"],
                    bench["missed_deadlines"]) == (samples, samples, 0, 0)
            assert sum(bench["histogram_counts"]) == samples
            prefix = f"{policy}-{case}-{number}"
            log = run / f"{prefix}.log"
            assert sha256(log) == row["raw_log_sha256"]
            lines = log.read_text().splitlines()
            assert lines.count("WAKEUP_LATENCY_PASSED") == 1
            metadata = [json.loads(line.split(" ", 1)[1]) for line in lines
                        if line.startswith("WAKEUP_LATENCY_METADATA ")]
            comparable = lambda item: {key: value for key, value in item.items()
                                       if key != "clock_pair_min_ns"}
            assert len(metadata) == 1
            assert comparable(metadata[0]) == comparable(baseline["metadata"][0])
            assert [json.loads(line.split(" ", 1)[1]) for line in lines
                    if line.startswith("WAKEUP_LATENCY_RESULT ")] == [bench]
            before = read_counter_snapshot(run / f"{prefix}-before")
            after = read_counter_snapshot(run / f"{prefix}-after")
            assert before.keys() == after.keys() == row["delta"].keys()
            assert {key: after[key] - before[key] for key in before} == row["delta"]
            if identifier == "833":
                delta = row["delta"]
                count = delta["ipi_dispatch_count"]
                assert count == delta["ipi_handler_count"]
                assert 21000 < count < 22000
                assert sum(delta[f"ipi_dispatch_bucket_{i}"] for i in range(64)) == count
                assert sum(delta[f"ipi_handler_bucket_{i}"] for i in range(64)) == count
                excess = (delta["ipi_dispatch_total_ns"] - delta["ipi_handler_total_ns"]) / count
                assert 1180 < excess < 1250
        print(f"resume{identifier}: four valid instrumented rounds; diagnostic only")


def check_sgi_diagnostic(baseline):
    directory = ROOT / "resume834-836-sgi-path/resume836"
    run = directory / "run1"
    result = json.loads((run / "results.json").read_text())
    assert result["diagnostic_only"] is True
    assert result["source_head"] == "69a33650763538692fafea27c869870ed0313642"
    assert result["board_id"] == "OrangePi-5-Plus-2"
    assert result["session_id"] == json.loads((run / "session.json").read_text())["session_id"]
    assert result["bench_sha256"] == baseline["bench_sha256"]
    assert result["image_sha256"] == "e803441a0d600b9d02d11899d1f028fb9c1c620e516901a6ed3b05877bf903ff"
    assert result["build_config_sha256"] == sha256(directory / "build.toml")
    patch = gzip.decompress((directory / "probe.patch.gz").read_bytes())
    assert hashlib.sha256(patch).hexdigest() == result["source_patch_sha256"]
    assert (run / "sha256").read_text().split()[0] == baseline["bench_sha256"]
    assert [(row["policy"], row["round"]) for row in result["rounds"]] == [
        ("fifo", 1), ("fifo", 2), ("other", 1), ("other", 2)
    ]
    expected = ((21138, 21765, 11), (21182, 22084, 11),
                (21014, 21375, 12), (21001, 21360, 12))
    for row, (cpu0_pairs, ipis, median_bucket) in zip(result["rounds"], expected):
        assert row["valid"] is True
        assert row["case"] == "thread_futex_cross_cpu"
        policy, number = row["policy"], row["round"]
        prefix = f"{policy}-thread_futex_cross_cpu-{number}"
        raw_log = run / f"{prefix}.log"
        assert sha256(raw_log) == row["raw_log_sha256"]
        lines = raw_log.read_text().splitlines()
        assert lines.count("WAKEUP_LATENCY_PASSED") == 1
        bench = row["benchmark"]
        assert [json.loads(line.split(" ", 1)[1]) for line in lines
                if line.startswith("WAKEUP_LATENCY_RESULT ")] == [bench]
        assert (bench["policy"], bench["case"]) == (policy, row["case"])
        assert (bench["samples"], bench["attempted"], bench["not_parked"],
                bench["missed_deadlines"]) == (20000, 20000, 0, 0)
        assert sum(bench["histogram_counts"]) == 20000
        before = read_counter_snapshot(run / f"{prefix}-before")
        after = read_counter_snapshot(run / f"{prefix}-after")
        assert before.keys() == after.keys() == row["delta"].keys()
        delta = row["delta"]
        assert {key: after[key] - before[key] for key in before} == delta
        assert (delta["ipi_issue_count"], delta["ipi_entry_count"],
                delta["ipi_pair_count"]) == (ipis, ipis, ipis)
        assert (delta["ipi_overwrite_count"], delta["ipi_unmatched_count"],
                delta["ipi_backward_count"]) == (0, 0, 0)
        assert delta["ipi_dispatch_count"] == delta["ipi_handler_count"] == ipis
        assert delta["ipi_cpu0_pair_count"] == cpu0_pairs
        buckets = [delta[f"ipi_cpu0_pair_bucket_{index}"] for index in range(64)]
        assert sum(buckets) == cpu0_pairs
        assert next(index for index in range(64)
                    if sum(buckets[:index + 1]) >= cpu0_pairs / 2) == median_bucket
    print("resume836: four valid instrumented SGI rounds; diagnostic only")


def check_weighted_pgo(baseline, rt, ordinary, previous):
    directory = ROOT / "resume817-819-weighted-pgo"
    training = json.loads((directory / "training-result.json").read_text())
    assert training["diagnostic_only"] is True
    assert training["bench_sha256"] == baseline["bench_sha256"]
    assert training["board_id"] == "OrangePi-5-Plus-1"
    assert training["session_id"] == json.loads(
        (directory / "training-session.json").read_text())["session_id"]
    assert training["workload_log_sha256"] == sha256(directory / "training-workload.log")
    training_rows = [json.loads(line.split(" ", 1)[1])
                     for line in (directory / "training-workload.log").read_text().splitlines()
                     if line.startswith("WAKEUP_LATENCY_RESULT ")]
    assert len(training_rows) == training["workload_rows"] == 27
    assert [[row["policy"], row["case"]] for row in training_rows] == training["policies_cases"]
    assert sum(row["not_parked"] for row in training_rows) == training["not_parked_total"] == 17

    wrapper = [json.loads(line) for line in (directory / "wrapper.jsonl").read_text().splitlines()]
    assert {row["crate"] for row in wrapper if row["profiled"]} == {
        "ax_sched", "ax_task", "ax_runtime", "starry_kernel"}
    assert {row["crate"] for row in wrapper if not row["profiled"]} == {"starryos"}
    assert "starry bin refresh" in gzip.decompress(
        (directory / "build.log.gz").read_bytes()).decode()

    runs = {}
    image = "9e9847a433cd99808d7f511372d454eb2cf5412a94e42187bcab64780ebfb8a3"
    for label in ("G1", "G2"):
        status = json.loads((directory / f"{label}-results.json").read_text())
        assert status["source_head"] == "69a33650763538692fafea27c869870ed0313642"
        assert status["source_patch_sha256"] == "d7c1388dec5a849b1bec41e49b51d1f4cea6e2e853f1b4b2ce58d195a71d2b78"
        assert status["bench_sha256"] == baseline["bench_sha256"]
        assert status["board_id"] == "OrangePi-5-Plus-1"
        assert status["session_id"] == json.loads(
            (directory / f"{label}-session.json").read_text())["session_id"]
        assert status["image_sha256"]["G"] == image
        assert len(status["rounds"]) == 1
        run = status["rounds"][0]
        assert run["tag"] == label and run["valid"] and run["error"] is None
        assert run["image_sha256"] == image
        path = directory / f"{label}-full.log"
        assert sha256(path) == run["raw_log_sha256"]
        rows = read_full20(path, baseline)
        assert rows == {(row["policy"], row["case"]): row for row in run["rows"]}
        assert set(rows) == set(rt)
        runs[label] = rows

    analysis = json.loads((directory / "analysis.json").read_text())
    assert analysis["candidate"] == "G1/G2" and analysis["screening_only"] is True
    assert analysis["both_full20_valid"] is True and analysis["image_sha256"] == image
    assert len(analysis["comparison"]) == 20
    assert {(row["policy"], row["case"]) for row in analysis["comparison"]} == set(rt)
    assert analysis["accepted_rows_90_percent"] == sum(
        rt[key]["p50_ns"] * 10 >= median(runs[label][key]["p50_ns"] for label in runs) * 9
        for key in rt) == 11
    regressions = []
    for row in analysis["comparison"]:
        key = row["policy"], row["case"]
        candidate_p50 = median(runs[label][key]["p50_ns"] for label in runs)
        assert row["linux_rt_p50_ns"] == rt[key]["p50_ns"]
        assert row["rt_over_G_p50_percent"] == round(
            100 * rt[key]["p50_ns"] / candidate_p50, 2)
        assert row["G_passes_90_percent"] == (rt[key]["p50_ns"] * 10 >= candidate_p50 * 9)
        for metric in METRICS:
            values = row["metrics"][metric]
            g1, g2 = (runs[label][key][metric] for label in runs)
            g = median((g1, g2))
            vs_a = round(100 * (g / ordinary[key][metric] - 1), 2)
            vs_f = round(100 * (g / previous[key][metric] - 1), 2)
            assert values == {"A1": ordinary[key][metric], "F2": previous[key][metric],
                              "G1": g1, "G2": g2, "G_median": g,
                              "G_vs_A_percent": vs_a, "G_vs_F_percent": vs_f}
            if g >= ordinary[key][metric] * 1.03:
                regressions.append({"policy": key[0], "case": key[1], "metric": metric,
                                    "G_vs_A_percent": vs_a})
    assert analysis["regressions_ge_3_percent_vs_single_A1"] == regressions == []
    worst = min(analysis["comparison"], key=lambda row: row["rt_over_G_p50_percent"])
    assert (worst["policy"], worst["case"]) == ("other", "thread_futex_same_cpu")
    assert analysis["worst_row"] == worst["case"]
    assert median(runs[label]["other", "thread_futex_same_cpu"]["p50_ns"]
                  for label in runs) == 14583.5
    print("resume817-819: two valid G full20 boots, 11/20 at 90%; weighted PGO rejected")


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

    check_weighted_pgo(baseline, rt, a, f)

    trial = json.loads((ROOT / "resume795/results.json").read_text())
    assert trial["source_head"] == "f52d905a4765d84c08ff36fa6298a7cf9659520f"
    assert trial["source_patch_sha256"] == candidate["temporary_exporter_patch_sha256"]
    assert trial["bench_sha256"] == baseline["bench_sha256"]
    assert trial["board_id"] == "OrangePi-5-Plus-1"
    assert trial["image_sha256"]["A"] == ordinary["bin_sha256"]
    assert [item["tag"] for item in trial["rounds"]] == ["A1", "B1"]
    assert (ROOT / "resume795/0001-perf-irq-framework-reuse-dispatch-descriptor-index.patch") \
        .read_text().startswith(f"From {trial['source_head']} ")

    trial_runs = {}
    for item in trial["rounds"]:
        label = item["tag"]
        path = ROOT / f"resume795/{label}-full.log"
        assert item["valid"] and item["error"] is None
        assert item["image_sha256"] == trial["image_sha256"][label[0]]
        assert sha256(path) == item["raw_log_sha256"]
        rows = read_full20(path, baseline)
        assert {(row["policy"], row["case"]): row for row in item["rows"]} == rows
        assert set(rows) == set(rt)
        trial_runs[label] = rows

    trial_a, trial_b = trial_runs["A1"], trial_runs["B1"]
    assert sum(rt[key]["p50_ns"] / trial_b[key]["p50_ns"] >= 0.9 for key in rt) == 10
    assert (trial_a[worst]["p50_ns"], trial_b[worst]["p50_ns"]) == (27417, 26834)
    regressions = {(metric, key) for metric in METRICS for key in rt
                   if trial_b[key][metric] >= trial_a[key][metric] * 1.03}
    assert regressions == {
        ("p999_ns", ("fifo", "sched_yield_handoff")),
        ("p999_ns", ("other", "absolute_timer_same_cpu")),
        ("p50_ns", ("other", "sched_yield_no_peer")),
    }
    print("resume795: valid A1/B1, 10/20 at 90%, three >=3% regressions; rejected")

    ipi = check_diagnostic(
        "resume799", baseline, "thread_futex_cross_cpu",
        {("fifo", 1), ("fifo", 2), ("other", 1), ("other", 2)})
    sent_means = [row["delta"]["switch_scheduler_detail_ipi_sent_total_ns"] /
                  row["delta"]["switch_scheduler_detail_ipi_sent_count"] for row in ipi]
    assert all(1469 < value < 1475 for value in sent_means[:2])
    assert all(1701 < value < 1712 for value in sent_means[2:])
    print("resume799: four valid focused qperf rounds; IPI timing is diagnostic only")

    same_cpu = check_diagnostic(
        "resume801", baseline, "thread_futex_same_cpu",
        {("fifo", 1), ("fifo", 2), ("other", 2)})
    invalid = same_cpu[2]["benchmark"]
    assert (invalid["samples"], invalid["attempted"], invalid["not_parked"]) == (19999, 20000, 1)
    fifo_switches = [row["delta"]["context_switches"] / 20000 for row in same_cpu[:2]]
    other_switches = same_cpu[3]["delta"]["context_switches"] / 20000
    fifo_rq = [row["delta"]["owner_rq_scheduler_transactions"] / 20000
               for row in same_cpu[:2]]
    other_rq = same_cpu[3]["delta"]["owner_rq_scheduler_transactions"] / 20000
    assert 2.10 < min(fifo_switches) <= max(fifo_switches) < 2.14
    assert 2.19 < other_switches < 2.20
    assert 3.16 < min(fifo_rq) <= max(fifo_rq) < 3.21
    assert 3.25 < other_rq < 3.26
    assert 1.05 < same_cpu[3]["delta"]["direct_wake_preemptions"] / 20000 < 1.06
    print("resume801: three valid focused qperf rounds; one OTHER round invalid")

    check_recent_path_diagnostics(baseline)
    check_sgi_diagnostic(baseline)


if __name__ == "__main__":
    main()
