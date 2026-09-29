#!/usr/bin/env python3
"""Recompute the resume858 timer notification diagnostic from raw logs."""

import hashlib
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parent
RUN = ROOT / "run1"
SOURCE = "69a33650763538692fafea27c869870ed0313642"
IMAGE_SHA = "ec288c217d771e384601d2ea5b4e3a617e81eeeb199730448d34085a13e1390f"
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"


def require(condition, message):
    if not condition:
        raise SystemExit(message)


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def metrics(path):
    result = {}
    for line in path.read_text().splitlines():
        fields = line.split()
        if len(fields) != 2 or not fields[1].isdigit():
            continue
        require(fields[0] not in result, f"duplicate metric {fields[0]} in {path}")
        result[fields[0]] = int(fields[1])
    return result


def p50_bucket(delta, prefix, count):
    halfway = (count + 1) // 2
    cumulative = 0
    for bucket in range(32):
        cumulative += delta[f"{prefix}_bucket_{bucket}"]
        if cumulative >= halfway:
            return bucket
    raise SystemExit(f"no median bucket for {prefix}")


status = json.loads((RUN / "results.json").read_text())
session = json.loads((RUN / "session.json").read_text())
require(status["diagnostic_only"] and status["source_head"] == SOURCE,
        "diagnostic or source identity")
require(status["image_sha256"] == IMAGE_SHA, "recorded image identity")
image = ROOT / "image.bin"
if image.exists():
    require(sha256(image) == IMAGE_SHA, "image bytes")
else:
    print("image.bin not archived; image bytes cannot be rehashed")
require(status["bench_sha256"] == BENCH_SHA and
        (RUN / "sha256").read_text().split()[0] == BENCH_SHA,
        "benchmark identity")
require(status["board_id"] == session["board_id"] == "OrangePi-5-Plus-2"
        and status["session_id"] == session["session_id"],
        "board session identity")
require(len(status["rounds"]) == 4, "four process rounds")

for item, (policy, round_no) in zip(
    status["rounds"], (("fifo", 1), ("fifo", 2), ("other", 1), ("other", 2))
):
    require((item["policy"], item["round"]) == (policy, round_no),
            "round order")
    prefix = f"{policy}-absolute_timer_same_cpu-{round_no}"
    log_path = RUN / f"{prefix}.log"
    log = log_path.read_text()
    rows = [json.loads(line.split(" ", 1)[1]) for line in log.splitlines()
            if line.startswith("WAKEUP_LATENCY_RESULT ")]
    require(len(rows) == 1 and rows[0] == item["benchmark"] and
            item["raw_log_sha256"] == sha256(log_path),
            f"raw benchmark {prefix}")
    row = rows[0]
    require(item["valid"] and row["policy"] == policy and
            row["case"] == "absolute_timer_same_cpu" and
            row["samples"] == row["attempted"] == 10000 and
            row["not_parked"] == row["missed_deadlines"] == 0 and
            "WAKEUP_LATENCY_PASSED" in log and "DIAGNOSTIC_EXIT 0" in log,
            f"sample validity {prefix}")
    before = metrics(RUN / f"{prefix}-before")
    after = metrics(RUN / f"{prefix}-after")
    require(before.keys() == after.keys(), f"metric keys {prefix}")
    delta = {key: after[key] - before[key] for key in before}
    require(all(value >= 0 for value in delta.values()) and delta == item["delta"],
            f"counter deltas {prefix}")
    fresh = delta["ktimer_notify_fresh"]
    already = delta["ktimer_notify_already_pending"]
    notified = delta["ktimer_notify_notified"]
    pending = delta["ktimer_notify_pending"]
    matched = delta["ktimer_stage_matched"]
    require(fresh + already == notified and already == 0 and notified > 0,
            f"fresh/retained classification {prefix}")
    require(delta["ktimer_claim_unmatched"] == 0 and
            delta["ktimer_claim_matched"] == notified + pending and
            matched == notified and delta["ktimer_stage_unmatched"] == pending,
            f"generation pairing {prefix}")
    histogram = [delta[f"ktimer_notify_claim_gap_bucket_{bucket}"]
                 for bucket in range(32)]
    require(sum(histogram) == matched, f"histogram coverage {prefix}")
    bucket = p50_bucket(delta, "ktimer_notify_claim_gap", matched)
    require(bucket == 8, f"unexpected median bucket {prefix}")
    print(f"{prefix}: fresh={fresh}, already_pending={already}, "
          f"pending={pending}, matched={matched}, "
          f"notify-to-claim p50 bucket={bucket * 2}-{(bucket + 1) * 2} us")

print("resume858 fresh-wake diagnostic: OK")
