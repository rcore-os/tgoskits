#!/usr/bin/env python3
"""Recompute resume862 validity and qperf stage summaries from raw files."""

import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RUN = ROOT / "run1"
ROUNDS = ("other-1", "fifo-1", "other-2", "fifo-2", "other-3")
STAGES = ("key_and_hint", "bucket_lock", "collect", "unlock", "wake_batch")
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"
IMAGE_SHA = "938919005840f014a48aa830c072ac2f1174f476ca56651d1847faca6c772df2"
SOURCE = "b292a098bb60ef604e7677c37cd95d926ff08200"


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def metrics(path):
    values = {}
    for line in path.read_text().splitlines():
        key, value = line.split()
        assert key not in values and value.isdigit(), line
        values[key] = int(value)
    return values


def delta(prefix, kind):
    before = metrics(RUN / f"{prefix}-{kind}-before")
    after = metrics(RUN / f"{prefix}-{kind}-after")
    assert before.keys() == after.keys()
    differences = {key: after[key] - before[key] for key in before}
    assert all(value >= 0 for value in differences.values())
    return differences


def main():
    source_hashes = dict(line.split(maxsplit=1) for line in
                         (ROOT / "source.sha256").read_text().splitlines())
    assert source_hashes == {
        sha(ROOT / "source-futex.rs"): "os/StarryOS/kernel/src/task/futex.rs",
        sha(ROOT / "source-futex-probe.rs"): "os/StarryOS/kernel/src/task/futex/probe.rs",
        sha(ROOT / "source-debug.rs"): "os/StarryOS/kernel/src/pseudofs/debug.rs",
    }
    archived = json.loads((RUN / "results.json").read_text())
    session = json.loads((RUN / "session.json").read_text())
    assert archived["diagnostic_only"] is True
    assert archived["source_head"] == SOURCE
    assert archived["bench_sha256"] == BENCH_SHA
    assert archived["image_sha256"] == IMAGE_SHA
    if (ROOT / "image.bin").exists():
        assert sha(ROOT / "image.bin") == IMAGE_SHA
    else:
        print("image.bin not archived; image bytes cannot be rehashed")
    assert archived["board_id"] == session["board_id"] == "OrangePi-5-Plus-1"
    assert archived["session_id"] == session["session_id"]
    assert (RUN / "bench.sha256").read_text().split()[0] == BENCH_SHA
    serial = (RUN / "serial.log").read_bytes()
    assert b"fd818040: 00000110 00000082 00000000" in serial
    assert b"fd818280: 00000001" in serial
    assert b"RESUME862_DONE 0" in serial
    assert [row["sequence"] for row in archived["rounds"]] == list(ROUNDS)

    summary = []
    for sequence, archived_row in zip(ROUNDS, archived["rounds"]):
        policy = sequence.split("-", 1)[0]
        prefix = f"{sequence}-thread_futex_same_cpu"
        log_path = RUN / f"{prefix}.log"
        log = log_path.read_text()
        rows = [json.loads(line.split(" ", 1)[1]) for line in log.splitlines()
                if line.startswith("WAKEUP_LATENCY_RESULT ")]
        assert len(rows) == 1
        row = rows[0]
        assert (row["policy"], row["case"]) == (policy, "thread_futex_same_cpu")
        assert row["samples"] == row["attempted"] == 20000
        assert row["not_parked"] == row["missed_deadlines"] == 0
        assert sum(row["histogram_counts"]) == 20000
        assert "WAKEUP_LATENCY_PASSED" in log and "DIAGNOSTIC_EXIT 0" in log
        assert archived_row["valid"] is True
        assert archived_row["benchmark"] == row
        assert archived_row["raw_log_sha256"] == sha(log_path)
        futex, sched = delta(prefix, "futex"), delta(prefix, "sched")
        assert archived_row["delta"] == {"futex": futex, "sched": sched}
        expected_keys = {
            "futex_wake_skipped", "futex_wake_selected_zero",
            "futex_wake_selected_many", "futex_wake_selected_one_coalesced",
            "futex_wake_selected_one_enqueued",
            *(f"futex_wake_{stage}_total_ns" for stage in STAGES),
        }
        assert set(futex) == expected_keys
        assert futex["futex_wake_selected_one_coalesced"] == 0
        count = futex["futex_wake_selected_one_enqueued"]
        attempts = sched["direct_wake_attempts"]
        activations = sched["direct_wake_activations"]
        assert 20000 <= count <= attempts and 20000 <= activations <= attempts
        nonactivations = attempts - activations
        stage_mean_ns = {
            stage: round(futex[f"futex_wake_{stage}_total_ns"] / count, 2)
            for stage in STAGES
        }
        summary.append({
            "sequence": sequence, "valid": True, "p50_ns_diagnostic_only": row["p50_ns"],
            "selected_one_enqueued": count, "direct_wake_attempts": attempts,
            "direct_wake_activations": activations,
            "minimum_target_off_rq_fraction": round((20000 - nonactivations) / 20000, 6),
            "pre_batch_mean_ns_probe_inclusive": round(sum(
                stage_mean_ns[stage] for stage in STAGES[:-1]), 2),
            "stage_mean_ns_probe_inclusive": stage_mean_ns,
            "context_switches_preempted": sched["context_switches_preempted"],
            "context_switches_blocked": sched["context_switches_blocked"],
        })
    assert sum(row["sequence"].startswith("other") for row in summary) == 3
    assert sum(row["sequence"].startswith("fifo") for row in summary) == 2
    print(json.dumps({"diagnostic_only": True, "valid_rounds": 5,
                      "summary": summary}, indent=2))


if __name__ == "__main__":
    main()
