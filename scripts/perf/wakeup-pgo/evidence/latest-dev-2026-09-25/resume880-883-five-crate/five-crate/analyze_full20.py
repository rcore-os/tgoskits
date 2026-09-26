#!/usr/bin/env python3
"""Validate both five-crate full20 boots and compare frozen Linux RT p50."""

import hashlib
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RUN = ROOT / "full20"
BASELINE = ROOT.parent.parent.parent / "review-2477/full-pgo-2026-09-23/linux-rt-baseline.json"
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"
IMAGE_SHA = "e2b96fde90132a7c678a511dbd4d675b40c3743902b2e6ecfd6b1744d4178969"


def main():
    baseline = json.loads(BASELINE.read_text())
    assert baseline["bench_sha256"] == BENCH_SHA
    archived = json.loads((RUN / "results.json").read_text())
    assert archived["image_sha256"] == {"F": IMAGE_SHA}
    assert archived["bench_sha256"] == BENCH_SHA
    assert archived["board_id"] == "OrangePi-5-Plus-1"
    assert [item["tag"] for item in archived["rounds"]] == ["F1", "F2"]

    linux = {(row["policy"], row["case"]): row for row in baseline["results"]}
    valid = {}
    for item in archived["rounds"]:
        tag = item["tag"]
        raw = (RUN / f"{tag}-full.log").read_bytes()
        assert hashlib.sha256(raw).hexdigest() == item["raw_log_sha256"]
        log = raw.decode()
        checksum = (RUN / f"{tag}.sha256").read_text()
        assert checksum.split()[0] == BENCH_SHA
        rows = [json.loads(line.removeprefix("WAKEUP_LATENCY_RESULT ")) for line in log.splitlines()
                if line.startswith("WAKEUP_LATENCY_RESULT ")]
        metadata = [json.loads(line.removeprefix("WAKEUP_LATENCY_METADATA ")) for line in log.splitlines()
                    if line.startswith("WAKEUP_LATENCY_METADATA ")]
        assert len(metadata) == 1
        assert {key: value for key, value in metadata[0].items() if key != "clock_pair_min_ns"} == {
            key: value for key, value in baseline["metadata"][0].items() if key != "clock_pair_min_ns"
        }
        assert len(rows) == 20 and len({(row["policy"], row["case"]) for row in rows}) == 20
        assert {"WAKEUP_LATENCY_CASE_START", "WAKEUP_LATENCY_CASE_DONE"} <= set(log.split())
        assert len(re.findall(r"^WAKEUP_LATENCY_CASE_START ", log, re.M)) == 20
        assert len(re.findall(r"^WAKEUP_LATENCY_CASE_DONE ", log, re.M)) == 20
        assert "WAKEUP_LATENCY_PASSED" in log and "WAKEUP_LATENCY_FAILED" not in log
        assert all(row["samples"] == row["attempted"] and not row["not_parked"]
                   and not row["missed_deadlines"] and sum(row["histogram_counts"]) == row["samples"]
                   for row in rows)
        assert sum(row["samples"] for row in rows) == 380000
        assert item["valid"] and item["rows"] == rows
        valid[tag] = {(row["policy"], row["case"]): row for row in rows}
    assert len(linux) == 20 and all(set(rows) == set(linux) for rows in valid.values())

    comparison = []
    for key in sorted(linux):
        f1, f2 = (valid[tag][key]["p50_ns"] for tag in ("F1", "F2"))
        total = f1 + f2
        rt = linux[key]["p50_ns"]
        comparison.append({
            "policy": key[0], "case": key[1], "linux_rt_p50_ns": rt,
            "F1_p50_ns": f1, "F2_p50_ns": f2, "candidate_median_p50_ns": total / 2,
            "rt_over_candidate_pct": round(200 * rt / total, 3),
            "passes_90pct": rt * 20 >= total * 9,
        })
    report = {
        "source_head": archived["source_head"],
        "source_patch_sha256": archived["source_patch_sha256"],
        "session_id": archived["session_id"],
        "board_id": archived["board_id"],
        "benchmark_sha256": BENCH_SHA,
        "candidate_image_sha256": IMAGE_SHA,
        "valid_boots": ["F1", "F2"],
        "candidate_90pct_rows_two_boot_median": sum(row["passes_90pct"] for row in comparison),
        "worst": min(comparison, key=lambda row: row["rt_over_candidate_pct"]),
        "same_source_tail_gate_proven": False,
        "acceptance_proven": False,
        "comparison": comparison,
    }
    (ROOT / "full20-analysis.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({key: value for key, value in report.items() if key != "comparison"}, indent=2))


if __name__ == "__main__":
    main()
