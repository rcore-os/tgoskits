#!/usr/bin/env python3
"""Recalculate the scheduler-pass diagnostic from archived board output."""

import hashlib
import gzip
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parent
RUN = ROOT / "run1"
BASELINE = ROOT.parent.parent / "review-2477/full-pgo-2026-09-23/linux-rt-baseline.json"
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"
SOURCE_SHA = "844c9af98ca3fab3673138e6af33802741d8576e44d315d8bb10998e20d3d27a"
BUILD_SHA = "e2c45ac5d73454937923cce62b86298c96f0fa9310880ae2020ed93436ac8255"
IMAGE_SHA = "f6087021e4cd4a2c49e563d1a988099fc289ab1b7ea248cee6b47baa32b19bca"


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def snapshot(path):
    return {key: int(value) for key, value in
            (line.split() for line in path.read_text().splitlines())}


def tagged_json(raw, prefix):
    return [json.loads(line[len(prefix):]) for line in raw.splitlines()
            if line.startswith(prefix)]


def main():
    result = json.loads((RUN / "results.json").read_text())
    session = json.loads((RUN / "session.json").read_text())
    linux = json.loads(BASELINE.read_text())
    assert result["experiment"] == "resume904"
    assert result["source_head"] == "b292a098bb60ef604e7677c37cd95d926ff08200"
    assert result["board_id"] == session["board_id"] == "OrangePi-5-Plus-1"
    assert result["session_id"] == session["session_id"]
    assert result["benchmark_sha256"] == linux["bench_sha256"] == BENCH_SHA
    assert (RUN / "sha256").read_text().split()[0] == BENCH_SHA
    assert result["source_patch_sha256"] == hashlib.sha256(gzip.decompress(
        (ROOT / "source.patch.gz").read_bytes())).hexdigest() == SOURCE_SHA
    assert result["build_config_sha256"] == sha(ROOT / "build.toml") == BUILD_SHA
    assert result["image_sha256"] == IMAGE_SHA
    assert result["serial_sha256"] == sha(RUN / "serial.log")
    assert result["guest_exit"] == 0 and result["pll_verified"]
    assert result["board_released"] and result["state"] == "collected"

    modes = ("fifo", "other", "other", "fifo", "fifo", "other")
    assert len(result["rounds"]) == len(modes)
    for index, (round_, mode) in enumerate(zip(result["rounds"], modes), 1):
        assert round_["round"] == index and round_["mode"] == mode
        assert round_["valid"]
        raw_path = RUN / f"round-{index}.log"
        before_path = RUN / f"round-{index}-before"
        after_path = RUN / f"round-{index}-after"
        assert sha(raw_path) == round_["log_sha256"]
        assert sha(before_path) == round_["before_sha256"]
        assert sha(after_path) == round_["after_sha256"]
        raw = raw_path.read_text()
        metadata = tagged_json(raw, "WAKEUP_LATENCY_METADATA ")
        rows = tagged_json(raw, "WAKEUP_LATENCY_RESULT ")
        assert len(metadata) == len(rows) == 1
        assert {key: value for key, value in metadata[0].items()
                if key != "clock_pair_min_ns"} == {
                    key: value for key, value in linux["metadata"][0].items()
                    if key != "clock_pair_min_ns"}
        assert rows == round_["rows"]
        row = rows[0]
        assert (row["policy"], row["case"]) == (mode, "thread_futex_same_cpu")
        assert row["samples"] == row["attempted"] == 20000
        assert row["not_parked"] == row["missed_deadlines"] == 0
        assert sum(row["histogram_counts"]) == row["samples"]
        assert "WAKEUP_LATENCY_PASSED" in raw and "WAKEUP_LATENCY_FAILED" not in raw
        before, after = snapshot(before_path), snapshot(after_path)
        assert before.keys() == after.keys() == round_["delta"].keys()
        assert all(after[key] - before[key] == value
                   for key, value in round_["delta"].items())
        counts = round_["delta"]
        assert counts["preempt_schedule_passes"] == sum(counts[key] for key in (
            "preempt_schedule_switches", "preempt_schedule_rq_no_switch",
            "preempt_schedule_early_no_switch"))
        if mode == "other":
            assert counts["owner_rq_scheduler_transactions"] - counts["context_switches"] > 21000
            assert counts["preempt_schedule_rq_no_switch"] < 600
        print(f"round={index} mode={mode} valid=1 samples=20000 "
              f"rq_surplus={counts['owner_rq_scheduler_transactions'] - counts['context_switches']} "
              f"preempt_rq_no_switch={counts['preempt_schedule_rq_no_switch']}")

    print("qperf-only: no native full20 or production performance claim")


if __name__ == "__main__":
    main()
