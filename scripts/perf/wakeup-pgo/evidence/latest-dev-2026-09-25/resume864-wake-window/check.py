#!/usr/bin/env python3
"""Recompute resume864 validity and wake-window counts from raw files."""

import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RUN = ROOT / "run1"
ROUNDS = ("other-1", "fifo-1", "other-2", "fifo-2", "other-3")
SOURCE = "b292a098bb60ef604e7677c37cd95d926ff08200"
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"
IMAGE_SHA = "52d38b69d9e694323beddfbd1009dd684a605697fa91b8b703287188f75d2ee7"
KEYS = ("gate", "done", "other")
FIELDS = ("selected_one_enqueued", "any_switch", "preempted_switch", "multiple_switches")


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
    result = {key: after[key] - before[key] for key in before}
    assert all(value >= 0 for value in result.values())
    return result


def main():
    archived = json.loads((RUN / "results.json").read_text())
    session = json.loads((RUN / "session.json").read_text())
    assert archived["diagnostic_only"] is True
    assert archived["source_head"] == SOURCE
    assert archived["bench_sha256"] == (RUN / "bench.sha256").read_text().split()[0] == BENCH_SHA
    assert archived["image_sha256"] == IMAGE_SHA
    if (ROOT / "image.bin").is_file():
        assert sha(ROOT / "image.bin") == IMAGE_SHA
    assert archived["board_id"] == session["board_id"] == "OrangePi-5-Plus-1"
    assert archived["session_id"] == session["session_id"]
    serial = (RUN / "serial.log").read_bytes()
    assert b"fd818040: 00000110 00000082 00000000" in serial
    assert b"fd818280: 00000001" in serial
    assert b"RESUME864_DONE 0" in serial
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
        assert set(futex) == {f"futex_window_{key}_{field}"
                              for key in KEYS for field in FIELDS}
        slots = {}
        for key in KEYS:
            counts = {field: futex[f"futex_window_{key}_{field}"] for field in FIELDS}
            selected = counts["selected_one_enqueued"]
            assert all(counts[field] <= selected for field in FIELDS[1:])
            if key == "gate":
                assert selected >= 20000
            counts["any_fraction"] = round(counts["any_switch"] / selected, 6) if selected else None
            counts["preempted_fraction"] = (
                round(counts["preempted_switch"] / selected, 6) if selected else None
            )
            slots[key] = counts
        summary.append({"sequence": sequence, "valid": True,
                        "probe_p50_ns_not_acceptance": row["p50_ns"],
                        "context_switches_preempted_global": sched["context_switches_preempted"],
                        "slots": slots})
    print(json.dumps({"diagnostic_only": True, "valid_rounds": 5,
                      "image_sha256": archived["image_sha256"], "summary": summary}, indent=2))


if __name__ == "__main__":
    main()
