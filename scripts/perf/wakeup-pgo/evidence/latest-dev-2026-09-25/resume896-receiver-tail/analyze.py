#!/usr/bin/env python3
"""Validate raw receiver-tail rounds and identify exploratory valid subrounds."""

import hashlib
import gzip
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent
SOURCE = "b292a098bb60ef604e7677c37cd95d926ff08200"
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"
MODES = ("fifo", "other", "other", "fifo", "fifo", "other")
STAGES = ("hook", "unwind", "total")
ERRORS = ("overlap", "repeated_switch", "generation_mismatch", "cancelled",
          "missing_switch", "invalid_timestamp")


def sha(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def sha_gzip(path):
    digest = hashlib.sha256()
    with gzip.open(path, "rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def metrics(path):
    result = {}
    for line in path.read_text().splitlines():
        key, value = line.split()
        assert key not in result
        result[key] = int(value)
    return result


def percentile_bin(delta, prefix, percentile):
    count = delta[f"{prefix}_count"]
    threshold = (count * percentile + 99) // 100
    total = 0
    for index in range(64):
        total += delta[f"{prefix}_bin_{index}"]
        if total >= threshold:
            return index * 250
    raise AssertionError("histogram count below expected threshold")


def main():
    report = {"experiment": "resume896", "source_head": SOURCE,
              "benchmark_sha256": BENCH_SHA, "boots": [], "valid_rounds": []}
    for run_name in ("run1", "run2"):
        directory = ROOT / run_name
        stored = json.loads((directory / "results.json").read_text())
        assert stored["source_head"] == SOURCE
        assert stored["benchmark_sha256"] == BENCH_SHA
        assert stored["board_id"] == "OrangePi-5-Plus-1"
        assert stored["board_released"] and stored["pll_verified"]
        assert stored["image_sha256"] == (ROOT / "image.sha256").read_text().split()[0]
        assert stored["source_patch_sha256"] == sha_gzip(ROOT / "source.patch.gz")
        assert stored["build_config_sha256"] == sha(ROOT / "build.toml")
        assert stored["serial_sha256"] == sha(directory / "serial.log")
        assert len(stored["rounds"]) == len(MODES)
        rounds = []
        for index, (entry, mode) in enumerate(zip(stored["rounds"], MODES), 1):
            assert entry["round"] == index and entry["mode"] == mode
            prefix = f"round-{index}"
            assert entry["log_sha256"] == sha(directory / f"{prefix}.log")
            assert entry["before_sha256"] == sha(directory / f"{prefix}-before")
            assert entry["after_sha256"] == sha(directory / f"{prefix}-after")
            before = metrics(directory / f"{prefix}-before")
            after = metrics(directory / f"{prefix}-after")
            assert before.keys() == after.keys()
            assert before["bin_width_ns"] == after["bin_width_ns"] == 250
            delta = {key: after[key] - before[key] for key in before}
            assert all(value >= 0 for value in delta.values())
            assert delta["bin_width_ns"] == 0
            row = entry["rows"][0]
            assert row["policy"] == mode and row["case"] == "thread_futex_same_cpu"
            assert row["attempted"] == 20000 and row["missed_deadlines"] == 0
            count = delta[f"{mode}_total_count"]
            other = "other" if mode == "fifo" else "fifo"
            probe_valid = (delta["armed"] == count and 20000 <= count <= 21000
                           and delta[f"{other}_total_count"] == 0
                           and all(delta[name] == 0 for name in ERRORS))
            stages = {}
            for stage in STAGES:
                key = f"{mode}_{stage}"
                assert sum(delta[f"{key}_bin_{bucket}"] for bucket in range(64)) == count
                assert delta[f"{key}_count"] == count
                stages[stage] = {"count": count,
                                 "mean_ns": round(delta[f"{key}_total_ns"] / count, 3),
                                 "p50_bin_lower_ns": percentile_bin(delta, key, 50),
                                 "p99_bin_lower_ns": percentile_bin(delta, key, 99)}
            bench_valid = (row["samples"] == row["attempted"] == 20000
                           and row["not_parked"] == 0 and entry["valid_bench"])
            assert entry["valid_probe"] == probe_valid
            assert entry["valid"] == (probe_valid and bench_valid)
            summary = {"round": index, "policy": mode, "valid": entry["valid"],
                       "samples": row["samples"], "not_parked": row["not_parked"],
                       "probe_armed": delta["armed"], "errors": {name: delta[name] for name in ERRORS},
                       "stages": stages}
            rounds.append(summary)
            if entry["valid"]:
                report["valid_rounds"].append({"boot": run_name, **summary})
        report["boots"].append({"name": run_name, "group_valid": all(row["valid"] for row in rounds),
                                "rounds": rounds})
    assert len([row for row in report["valid_rounds"] if row["policy"] == "other"]) == 2
    assert len([row for row in report["valid_rounds"] if row["policy"] == "fifo"]) == 6
    print(json.dumps(report, ensure_ascii=True, indent=2))


if __name__ == "__main__":
    main()
