#!/usr/bin/env python3
"""Validate and recompute the resume867 focused diagnostic."""

import hashlib
import gzip
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RUN = ROOT / "run1"
SOURCE_DIR = ROOT.parent / "resume866-user-return"
IMAGE = SOURCE_DIR / "image.bin"
IMAGE_SHA = "54ee6a87398275a571f917ac55526947ea5eb2f440c75e6aad55fbf8af39537f"
SOURCE_ARCHIVE_SHA = "74851e4473cc54f3754256dac82b95a63ffeb96350f351b69dd0222d4ca5f755"
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"
SOURCE = "b292a098bb60ef604e7677c37cd95d926ff08200"
ROUNDS = ("other-1", "fifo-1", "other-2", "fifo-2", "other-3")
FIELDS = {
    "clear_count", "clear_observe_total_ns", "clear_exit_total_ns",
    "pending_count", "pending_observe_total_ns", "pending_unmask_total_ns",
}


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def metrics(path):
    result = {}
    for line in path.read_text().splitlines():
        key, value = line.split()
        assert key in FIELDS and key not in result and value.isdigit(), line
        result[key] = int(value)
    assert result.keys() == FIELDS
    return result


def main():
    result = json.loads((RUN / "results.json").read_text())
    session = json.loads((RUN / "session.json").read_text())
    assert result["diagnostic_only"] is True
    assert result["source_head"] == SOURCE
    assert result["board_id"] == session["board_id"] == "OrangePi-5-Plus-1"
    assert result["session_id"] == session["session_id"]
    assert result["image_sha256"] == IMAGE_SHA
    assert sha(SOURCE_DIR / "probe-source.tgz") == SOURCE_ARCHIVE_SHA
    if IMAGE.is_file():
        assert sha(IMAGE) == IMAGE_SHA
    assert result["bench_sha256"] == (RUN / "bench.sha256").read_text().split()[0] == BENCH_SHA
    with gzip.open(RUN / "serial.log.gz", "rb") as serial_file:
        serial = serial_file.read()
    assert b"fd818040: 00000110 00000082 00000000" in serial
    assert b"fd818280: 00000001" in serial
    assert b"RESUME866_DONE 0" in serial
    assert [row["sequence"] for row in result["rounds"]] == list(ROUNDS)

    summary = []
    for row in result["rounds"]:
        sequence = row["sequence"]
        policy = sequence.split("-", 1)[0]
        prefix = f"{sequence}-thread_futex_same_cpu"
        log_path = RUN / f"{prefix}.log"
        log = log_path.read_text()
        rows = [json.loads(line.split(" ", 1)[1]) for line in log.splitlines()
                if line.startswith("WAKEUP_LATENCY_RESULT ")]
        assert len(rows) == 1
        benchmark = rows[0]
        assert row["benchmark"] == benchmark
        assert row["raw_log_sha256"] == sha(log_path)
        assert (benchmark["case"], benchmark["policy"]) == (
            "thread_futex_same_cpu", policy)
        assert benchmark["attempted"] == 20000
        assert benchmark["missed_deadlines"] == 0
        assert sum(benchmark["histogram_counts"]) == benchmark["samples"]
        assert "WAKEUP_LATENCY_PASSED" in log and "DIAGNOSTIC_EXIT 0" in log
        valid = benchmark["samples"] == 20000 and benchmark["not_parked"] == 0
        assert row["valid"] == valid
        before = metrics(RUN / f"{prefix}-before")
        after = metrics(RUN / f"{prefix}-after")
        delta = {key: after[key] - before[key] for key in FIELDS}
        assert all(value >= 0 for value in delta.values())
        assert row["delta"] == delta
        assert delta["pending_count"] > 0 and delta["clear_count"] > 0
        summary.append({
            "sequence": sequence,
            "valid": valid,
            "samples": benchmark["samples"],
            "not_parked": benchmark["not_parked"],
            "pending_count": delta["pending_count"],
            "pending_observe_mean_ns": round(
                delta["pending_observe_total_ns"] / delta["pending_count"], 2),
            "pending_unmask_mean_ns": round(
                delta["pending_unmask_total_ns"] / delta["pending_count"], 2),
            "clear_count": delta["clear_count"],
            "clear_observe_mean_ns": round(
                delta["clear_observe_total_ns"] / delta["clear_count"], 2),
        })
    assert sum(item["valid"] for item in summary) == 4
    assert summary[0]["not_parked"] == 2
    print(json.dumps({"diagnostic_only": True, "valid_rounds": 4,
                      "invalid_rounds": 1, "summary": summary}, indent=2))


if __name__ == "__main__":
    main()
