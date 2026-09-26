#!/usr/bin/env python3
"""Audit both invalid diagnostic boots without pooling their valid rows."""

import hashlib
import json
from pathlib import Path
from statistics import median

ROOT = Path(__file__).resolve().parent
MODES = ("fifo", "other", "other", "fifo", "fifo", "other")
STAGES = ("park_block", "park_pick")
SOURCE = "b292a098bb60ef604e7677c37cd95d926ff08200"
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"
IMAGE_SHA = "89cd85a42930c422c1f5f1206f38d5cc126f9658d4f7c511352e72d3e7c015c0"
BUILD = (ROOT / "build.log").read_text()
assert "starry build package=starryos" in BUILD
assert "qperf-metrics" in BUILD and "ax-driver/rk3588-cpufreq" not in BUILD


def sha(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def metrics(path):
    values = {}
    for line in path.read_text().splitlines():
        fields = line.split()
        if len(fields) == 2 and fields[1].isdigit():
            values[fields[0]] = int(fields[1])
    return values


for run_name in ("run1", "run2"):
    run = ROOT / run_name
    status = json.loads((run / "results.json").read_text())
    session = json.loads((run / "session.json").read_text())
    assert status["state"] == "invalid" and status["guest_exit"] == 0
    assert status["source_head"] == SOURCE
    assert status["board_id"] == session["board_id"] == "OrangePi-5-Plus-1"
    assert status["board_released"] and status["pll_verified"]
    assert status["benchmark_sha256"] == BENCH_SHA
    assert status["image_sha256"] == IMAGE_SHA
    assert (run / "sha256").read_text().split()[0] == BENCH_SHA
    image = ROOT / "image.bin"
    if image.is_file():
        assert sha(image) == IMAGE_SHA
    for name, path in (("source_patch_sha256", ROOT / "source.patch"),
                       ("build_config_sha256", ROOT / "build.toml"),
                       ("serial_sha256", run / "serial.log")):
        assert sha(path) == status[name], (run_name, name)
    assert len(status["rounds"]) == len(MODES)
    stages_by_mode = {mode: {stage: [] for stage in STAGES}
                      for mode in ("fifo", "other")}
    valid_counts = {"fifo": 0, "other": 0}
    for number, mode in enumerate(MODES, 1):
        result = status["rounds"][number - 1]
        assert result["round"] == number and result["mode"] == mode
        prefix = f"round-{number}"
        log_path = run / f"{prefix}.log"
        before_path = run / f"{prefix}-before"
        after_path = run / f"{prefix}-after"
        for path, key in ((log_path, "log_sha256"),
                          (before_path, "before_sha256"),
                          (after_path, "after_sha256")):
            assert sha(path) == result[key], (run_name, number, key)
        log = log_path.read_text()
        rows = [json.loads(line.split(" ", 1)[1]) for line in log.splitlines()
                if line.startswith("WAKEUP_LATENCY_RESULT ")]
        assert len(rows) == 1 and rows == result["rows"]
        row = rows[0]
        assert row["policy"] == mode and row["case"] == "thread_futex_same_cpu"
        assert row["attempted"] == 20000 and row["missed_deadlines"] == 0
        assert sum(row["histogram_counts"]) == row["samples"]
        assert "DIAGNOSTIC_EXIT 0" in log
        before, after = metrics(before_path), metrics(after_path)
        assert before.keys() == after.keys()
        delta = {key: after[key] - before[key] for key in before}
        assert delta == result["delta"] and min(delta.values()) >= 0
        counts = [delta[f"switch_scheduler_detail_{stage}_count"]
                  for stage in STAGES]
        assert counts == result["counts"] and counts[0] == counts[1]
        valid = (row["samples"] == 20000 and row["not_parked"] == 0
                 and "WAKEUP_LATENCY_PASSED" in log and counts[0] >= 20000)
        assert valid == result["valid"]
        if valid:
            valid_counts[mode] += 1
            for stage, count in zip(STAGES, counts):
                total = delta[f"switch_scheduler_detail_{stage}_total_ns"]
                stages_by_mode[mode][stage].append(total / count)
        print(run_name, number, mode, "VALID" if valid else "INVALID",
              row["samples"], row["not_parked"], counts[0])

    print(run_name, "valid", valid_counts, "complete_group", all(
        count == 3 for count in valid_counts.values()))
    for stage in STAGES:
        fifo = median(stages_by_mode["fifo"][stage])
        other = median(stages_by_mode["other"][stage])
        print(run_name, stage, f"fifo={fifo:.2f}", f"other={other:.2f}",
              f"other_minus_fifo={other - fifo:.2f}")

print("resume889 raw evidence and invalid-run accounting: OK")
