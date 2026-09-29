#!/usr/bin/env python3
"""Recheck resume889 raw process logs and qperf counter snapshots."""

import hashlib
import json
import os
from pathlib import Path
from statistics import median


ROOT = Path(__file__).resolve().parent
RUN_NAME = os.environ.get("RESUME889_RUN", "run1")
require_run_name = RUN_NAME in ("run1", "run2")
if not require_run_name:
    raise SystemExit("invalid run name")
RUN = ROOT / RUN_NAME
MODES = ("fifo", "other", "other", "fifo", "fifo", "other")
STAGES = ("park_block", "park_pick")
SOURCE = "b292a098bb60ef604e7677c37cd95d926ff08200"
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"


def require(condition, message):
    if not condition:
        raise SystemExit(message)


def sha(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def metrics(path):
    result = {}
    for line in path.read_text().splitlines():
        fields = line.split()
        if len(fields) == 2 and fields[1].isdigit():
            result[fields[0]] = int(fields[1])
    return result


def row_from_log(text):
    rows = [json.loads(line.split(" ", 1)[1]) for line in text.splitlines()
            if line.startswith("WAKEUP_LATENCY_RESULT ")]
    require(len(rows) == 1, "one result per process")
    return rows[0]


status = json.loads((RUN / "results.json").read_text())
session = json.loads((RUN / "session.json").read_text())
require(status["state"] == "collected" and status["guest_exit"] == 0,
        "group not collected")
require(status["source_head"] == SOURCE and status["board_id"] ==
        session["board_id"] == "OrangePi-5-Plus-1", "source or board identity")
require(status["board_released"] and status["pll_verified"],
        "board or PLL not verified")
require(status["benchmark_sha256"] == BENCH_SHA, "benchmark identity")
require(sha(ROOT / "image.bin") == status["image_sha256"], "image hash")
require(sha(ROOT / "build.toml") == status["build_config_sha256"], "config hash")
require(sha(ROOT / "source.patch") == status["source_patch_sha256"], "patch hash")
require(sha(RUN / "serial.log") == status["serial_sha256"], "serial hash")
require((RUN / "sha256").read_text().split()[0] == BENCH_SHA,
        "guest benchmark hash")
build = (ROOT / "build.log").read_text()
require("starry build package=starryos" in build and
        "ax-driver/rk3588-cpufreq" not in build and "qperf-metrics" in build,
        "build config mismatch")
require(len(status["rounds"]) == len(MODES), "six process rounds")

by_mode = {mode: {stage: [] for stage in STAGES} for mode in ("fifo", "other")}
for number, mode in enumerate(MODES, 1):
    result = status["rounds"][number - 1]
    require(result["round"] == number and result["mode"] == mode and result["valid"],
            f"invalid process {number}")
    prefix = f"round-{number}"
    log_path = RUN / f"{prefix}.log"
    before_path = RUN / f"{prefix}-before"
    after_path = RUN / f"{prefix}-after"
    require(sha(log_path) == result["log_sha256"] and
            sha(before_path) == result["before_sha256"] and
            sha(after_path) == result["after_sha256"], f"raw hash {number}")
    log = log_path.read_text()
    row = row_from_log(log)
    require([row] == result["rows"] and row["policy"] == mode and
            row["case"] == "thread_futex_same_cpu" and
            row["samples"] == row["attempted"] == 20000 and
            row["not_parked"] == row["missed_deadlines"] == 0 and
            "WAKEUP_LATENCY_PASSED" in log and "DIAGNOSTIC_EXIT 0" in log,
            f"sample or exit {number}")
    before = metrics(before_path)
    after = metrics(after_path)
    require(before.keys() == after.keys(), f"counter keys {number}")
    delta = {key: after[key] - before[key] for key in before}
    require(delta == result["delta"] and all(value >= 0 for value in delta.values()),
            f"counter delta {number}")
    counts = [delta[f"switch_scheduler_detail_{stage}_count"] for stage in STAGES]
    require(counts == result["counts"] and all(
        count >= 20000 and abs(count - counts[0]) <= counts[0] * 0.03
        for count in counts), f"stage coverage {number}")
    for stage, count in zip(STAGES, counts):
        total = delta[f"switch_scheduler_detail_{stage}_total_ns"]
        by_mode[mode][stage].append(total / count)
    print(f"round {number} {mode}: p50={row['p50_ns']} ns, stage count={counts[0]}")

stage_medians = {mode: {stage: median(values) for stage, values in stages.items()}
                 for mode, stages in by_mode.items()}
for stage in STAGES:
    fifo = stage_medians["fifo"][stage]
    other = stage_medians["other"][stage]
    print(f"{stage}: FIFO={fifo:.2f} ns, OTHER={other:.2f} ns, "
          f"difference={other - fifo:.2f} ns/event")
print("resume889 diagnostic logs and stage coverage: OK")
