#!/usr/bin/env python3
"""Reconcile resume889 rq-only park probes with switch reasons."""

import hashlib
import json
from pathlib import Path
from statistics import median


ROOT = Path(__file__).resolve().parent.parent / "resume888-889-park-phase/resume889"
HASHES = {
    "run1": "9dba93b430b6b0a72fa9334a93bbbc6af2fcad64875a5eb1ee77826676bcf645",
    "run2": "3644d31fb2b1e69e1cf0e1fa14a17d813c7dc3a754fb674b4872a381f74c7c3a",
}
MODES = ("fifo", "other", "other", "fifo", "fifo", "other")
REASONS = ("preempted", "yield", "blocked", "exited", "migrated")


for boot, expected_hash in HASHES.items():
    path = ROOT / boot / "results.json"
    assert hashlib.sha256(path.read_bytes()).hexdigest() == expected_hash
    data = json.loads(path.read_text())
    assert data["source_head"] == "b292a098bb60ef604e7677c37cd95d926ff08200"
    assert data["board_id"] == "OrangePi-5-Plus-1"
    assert data["state"] == "invalid" and len(data["rounds"]) == 6
    valid = {"fifo": [], "other": []}
    for index, (mode, result) in enumerate(zip(MODES, data["rounds"], strict=True), 1):
        assert result["round"] == index and result["mode"] == mode
        assert len(result["rows"]) == 1
        row = result["rows"][0]
        assert row["policy"] == mode and row["case"] == "thread_futex_same_cpu"
        assert row["attempted"] == 20000 and row["missed_deadlines"] == 0
        assert sum(row["histogram_counts"]) == row["samples"]
        assert result["valid"] == (row["samples"] == 20000 and row["not_parked"] == 0)
        counts = result["delta"]
        fast = counts["switch_scheduler_detail_park_block_count"]
        pick = counts["switch_scheduler_detail_park_pick_count"]
        blocked = counts["context_switches_blocked"]
        preempted = counts["context_switches_preempted"]
        assert fast == pick
        assert abs(fast - blocked) <= 1, (boot, index, fast, blocked)
        assert sum(counts[f"context_switches_{reason}"] for reason in REASONS) == counts[
            "context_switches"
        ]
        if result["valid"]:
            valid[mode].append(result)
        print(boot, index, mode, "valid" if result["valid"] else "invalid",
              "fast", fast, "blocked", blocked, "preempted", preempted)
    assert len(valid["fifo"]) == 3
    assert len(valid["other"]) == (2 if boot == "run1" else 1)
    for mode, results in valid.items():
        blocked_per_attempt = median(
            result["delta"]["context_switches_blocked"] / 20000 for result in results
        )
        preempted_per_attempt = median(
            result["delta"]["context_switches_preempted"] / 20000 for result in results
        )
        print(boot, mode, "valid", len(results), "blocked/attempt",
              f"{blocked_per_attempt:.5f}", "preempted/attempt",
              f"{preempted_per_attempt:.5f}")

print("resume891 route reconciliation: OK; both full groups remain invalid")
