#!/usr/bin/env python3
"""Recheck raw full20 boots and compare source-matched medians."""

import hashlib
import gzip
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent
IMAGES = ROOT.parent / "resume870-fair-lag-fastpath"
BASELINE = ROOT.parent.parent / "review-2477/full-pgo-2026-09-23/linux-rt-baseline.json"
EXPECTED_BENCH = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"
EXPECTED_IMAGES = {
    "A": "4903ed1d8b2c73dcf7dfedcf50d0762838fc8ce6c06059754f0ce839bb74d2c9",
    "B": "0b76686e32eafd90f81095ba5aca7e4ff5562870fb57748362dc03f62d4062f4",
}
ORDER = ("A1", "B1", "B2", "A2")
METRICS = ("p50_ns", "p99_ns", "p999_ns")


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def row_key(row):
    return row["policy"], row["case"]


def twice_median(first, second):
    return first + second


def verify_log(path, checksum, baseline, tag):
    lines = path.read_text().splitlines()
    assert checksum.split()[0] == EXPECTED_BENCH
    assert lines.count("WAKEUP_LATENCY_PROFILE_START") == 1
    assert lines.count("WAKEUP_LATENCY_PROFILE_DONE") == 1
    assert lines.count("WAKEUP_LATENCY_PASSED") == 1
    assert "WAKEUP_LATENCY_FAILED" not in lines
    metadata = [json.loads(line.split(" ", 1)[1]) for line in lines
                if line.startswith("WAKEUP_LATENCY_METADATA ")]
    assert len(metadata) == 1
    reference = baseline["metadata"][0]
    assert {key: value for key, value in metadata[0].items()
            if key != "clock_pair_min_ns"} == {
                key: value for key, value in reference.items()
                if key != "clock_pair_min_ns"
            }
    starts = [tuple(re.fullmatch(r"WAKEUP_LATENCY_CASE_START case=(\S+) policy=(\S+)", line).groups())
              for line in lines if line.startswith("WAKEUP_LATENCY_CASE_START ")]
    dones = [tuple(re.fullmatch(r"WAKEUP_LATENCY_CASE_DONE case=(\S+) policy=(\S+)", line).groups())
             for line in lines if line.startswith("WAKEUP_LATENCY_CASE_DONE ")]
    rows = [json.loads(line.split(" ", 1)[1]) for line in lines
            if line.startswith("WAKEUP_LATENCY_RESULT ")]
    expected_keys = {row_key(row) for row in baseline["results"]}
    assert len(rows) == len(starts) == len(dones) == 20, tag
    assert starts == dones == [(row["case"], row["policy"]) for row in rows], tag
    assert {row_key(row) for row in rows} == expected_keys, tag
    assert len({row_key(row) for row in rows}) == 20, tag
    for row in rows:
        attempted = 10000 if row["case"] == "absolute_timer_same_cpu" else 20000
        assert row["samples"] == row["attempted"] == attempted, (tag, row_key(row))
        assert row["not_parked"] == row["missed_deadlines"] == 0, (tag, row_key(row))
        assert sum(row["histogram_counts"]) == attempted, (tag, row_key(row))
    assert sum(row["samples"] for row in rows) == 380000, tag
    return rows


def main():
    archived = json.loads((ROOT / "results.json").read_text())
    baseline = json.loads(BASELINE.read_text())
    assert archived["bench_sha256"] == EXPECTED_BENCH
    assert archived["image_sha256"] == EXPECTED_IMAGES
    assert [item["tag"] for item in archived["rounds"]] == list(ORDER)
    assert all(item["valid"] for item in archived["rounds"])
    assert archived["board_id"] == "OrangePi-5-Plus-1"
    for kind in EXPECTED_IMAGES:
        image = IMAGES / f"{kind}.bin"
        if image.is_file():
            assert sha(image) == EXPECTED_IMAGES[kind]
    with gzip.open(ROOT / "serial.log.gz", "rb") as serial_file:
        serial = serial_file.read()
    assert serial.count(b"fd818040: 00000110 00000082 00000000") >= 4
    assert serial.count(b"fd818280: 00000001") >= 4

    rounds = {}
    for item in archived["rounds"]:
        tag = item["tag"]
        log = ROOT / f"{tag}-full.log"
        checksum = (ROOT / f"{tag}.sha256").read_text()
        assert sha(log) == item["raw_log_sha256"]
        rows = verify_log(log, checksum, baseline, tag)
        assert len(rows) == 20 and rows == item["rows"]
        rounds[tag] = {row_key(row): row for row in rows}

    linux = {row_key(row): row for row in baseline["results"]}
    assert all(set(rounds[tag]) == set(linux) for tag in ORDER)
    comparisons = []
    for key in sorted(linux):
        row = {"policy": key[0], "case": key[1]}
        for metric in METRICS:
            a2 = twice_median(rounds["A1"][key][metric], rounds["A2"][key][metric])
            b2 = twice_median(rounds["B1"][key][metric], rounds["B2"][key][metric])
            row[f"A_{metric}"] = a2 / 2
            row[f"B_{metric}"] = b2 / 2
            row[f"B_over_A_{metric}_pct"] = round(100 * b2 / a2, 3)
            row[f"regression_ge_3pct_{metric}"] = b2 * 100 >= a2 * 103
        row["B_over_Linux_p50_pct"] = round(
            100 * linux[key]["p50_ns"] * 2 / (2 * row["B_p50_ns"]), 3
        )
        row["passes_90pct"] = linux[key]["p50_ns"] * 20 >= row["B_p50_ns"] * 18
        comparisons.append(row)
    regressions = [
        {"policy": row["policy"], "case": row["case"], "metric": metric,
         "B_over_A_pct": row[f"B_over_A_{metric}_pct"]}
        for row in comparisons for metric in METRICS
        if row[f"regression_ge_3pct_{metric}"]
    ]
    summary = {
        "source_head": archived["source_head"],
        "board_id": archived["board_id"],
        "session_id": archived["session_id"],
        "sequence": list(ORDER),
        "valid_full20_boots": 4,
        "image_sha256": EXPECTED_IMAGES,
        "bench_sha256": EXPECTED_BENCH,
        "rows_at_90pct": sum(row["passes_90pct"] for row in comparisons),
        "worst_p50": min(comparisons, key=lambda row: row["B_over_Linux_p50_pct"]),
        "regressions_ge_3pct": regressions,
        "comparisons": comparisons,
    }
    (ROOT / "analysis.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps({key: value for key, value in summary.items()
                      if key not in ("comparisons", "worst_p50")}, indent=2))
    print("worst_p50", summary["worst_p50"]["policy"],
          summary["worst_p50"]["case"],
          summary["worst_p50"]["B_over_Linux_p50_pct"])


if __name__ == "__main__":
    main()
