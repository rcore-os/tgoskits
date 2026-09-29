#!/usr/bin/env python3
"""Recheck the unchanged-image forced-RT handoff diagnostic pair."""

import hashlib
import json
import re
from pathlib import Path
from statistics import median


ROOT = Path(__file__).resolve().parent
STARRY = ROOT.parent / "resume840-forced-rt-preempt" / "run3"
EXPECTED = [
    ("control", "fifo", 1), ("forced", "fifo", 1),
    ("frozen", "fifo", 1), ("control", "other", 1),
    ("control", "other", 2), ("frozen", "fifo", 2),
    ("forced", "fifo", 2), ("control", "fifo", 2),
]


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def parse_log(text):
    marker = "WAKEUP_LATENCY_RESULT "
    rows = [json.loads(line.split(marker, 1)[1]) for line in text.splitlines()
            if marker in line]
    assert len(rows) == 1
    row = rows[0]
    assert row["case"] == "thread_futex_same_cpu"
    assert row["attempted"] == 20000
    assert sum(row["histogram_counts"]) == row["samples"]
    assert "WAKEUP_LATENCY_PASSED" in text
    return row


def main():
    starry_results = json.loads((STARRY / "results.json").read_text())
    linux_status = json.loads((ROOT / "status.json").read_text())
    assert linux_status["state"] == "collected" and linux_status["board_released"]
    assert linux_status["failures"] == 0
    assert linux_status["board_id"] == starry_results["board_id"] == "OrangePi-5-Plus-2"
    assert linux_status["linux_image_sha256"] == "aac6d3c5fa0c4fdf65f987af635f4cd55a06852b23046a4242a184acc2fd563b"
    assert linux_status["dtb_sha256"] == "316dd15b329756be3887dea22f89fc8d1f5b055f8769761f4144b6b1caaea994"
    assert sha(ROOT / "initramfs.cpio") == linux_status["initramfs_sha256"]
    assert sha(ROOT / "boot.log") == linux_status["boot_sha256"]
    assert starry_results["diagnostic_only"] is True
    assert starry_results["image_sha256"] == "9e9847a433cd99808d7f511372d454eb2cf5412a94e42187bcab64780ebfb8a3"
    assert [starry_results["bench_sha256"][name] for name in
            ("frozen", "control", "forced")] == [
                "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773",
                "6f582832f3e10d3d447d8f06afc706c0684c6f80a0b9346369b4dc59d922e713",
                "fabee7b6b3e312b07248d9d797cd3e3b40db406bac9f218d3516c66a1c3abe8c",
            ]
    assert [(row["binary"], row["policy"], row["round"])
            for row in starry_results["rounds"]] == EXPECTED

    starry = {}
    for entry in starry_results["rounds"]:
        key = entry["binary"], entry["policy"], entry["round"]
        path = STARRY / f"{key[0]}-{key[1]}-{key[2]}.log"
        assert sha(path) == entry["raw_log_sha256"]
        row = parse_log(path.read_text())
        assert row == entry["benchmark"] and entry["valid"]
        assert (row["samples"], row["not_parked"], row["missed_deadlines"]) == (20000, 0, 0)
        starry[key] = row

    boot = (ROOT / "boot.log").read_text(errors="replace")
    assert "PREEMPT_RT" in boot
    starts = list(re.finditer(
        r"RESUME841_CASE_START binary=/([a-z]+) policy=(fifo|other) round=([12])", boot))
    assert len(starts) == len(EXPECTED)
    linux = {}
    for index, start in enumerate(starts):
        key = start.group(1), start.group(2), int(start.group(3))
        assert key == EXPECTED[index]
        chunk = boot[start.end():starts[index + 1].start() if index + 1 < len(starts)
                     else boot.index("RESUME841_INIT_DONE")]
        assert f"RESUME841_CASE_DONE binary=/{key[0]} policy={key[1]} " \
               f"round={key[2]} exit=0" in chunk
        assert ("DIAGNOSTIC_FORCED_RT_PREEMPT sender_priority=80 receiver_priority=81" in chunk) \
               == (key[0] == "forced")
        row = parse_log(chunk)
        assert row["policy"] == key[1] and row["missed_deadlines"] == 0
        expected_invalid = key == ("control", "other", 2)
        assert (row["samples"], row["not_parked"]) == ((19999, 1) if expected_invalid
                                                       else (20000, 0))
        linux[key] = row

    for group in ("control", "forced", "frozen"):
        for system, rows in (("Starry", starry), ("Linux RT", linux)):
            values = [row["p50_ns"] for (binary, policy, _), row in rows.items()
                      if binary == group and policy == "fifo"]
            assert len(values) == 2
            print(system, group, "FIFO p50", values, "median", median(values))
    for system, rows in (("Starry", starry), ("Linux RT", linux)):
        valid_other = [row["p50_ns"] for (binary, policy, _), row in rows.items()
                       if binary == "control" and policy == "other"
                       and row["samples"] == row["attempted"]]
        print(system, "control OTHER valid p50", valid_other)
    starry_forced = median(starry[("forced", "fifo", i)]["p50_ns"] for i in (1, 2))
    linux_forced = median(linux[("forced", "fifo", i)]["p50_ns"] for i in (1, 2))
    starry_control = median(starry[("control", "fifo", i)]["p50_ns"] for i in (1, 2))
    linux_control = median(linux[("control", "fifo", i)]["p50_ns"] for i in (1, 2))
    print("forced Starry/Linux RT ratio", round(linux_forced / starry_forced * 100, 2), "%")
    print("control Starry/Linux RT ratio", round(linux_control / starry_control * 100, 2), "%")


if __name__ == "__main__":
    main()
