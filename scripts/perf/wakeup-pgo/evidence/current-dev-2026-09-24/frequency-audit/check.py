#!/usr/bin/env python3
"""Recompute the frequency audit from archived full20 and PMU raw outputs."""

import hashlib
import json
import re
import statistics
from pathlib import Path

ROOT = Path(__file__).resolve().parent
BASELINE = ROOT.parents[1] / "review-2477/full-pgo-2026-09-23/linux-rt-baseline.json"
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"
IMAGES = {
    "A": "58801abbba44d6dd61a2807efc62b678274586e075e31beca7fce9b913f84eca",
    "B": "c2592ff126a0e5a60b2a33ec9883ebe2f200ff0f748e14ea132a8437683db1d5",
    "F": "6a234ea391346f2450fdef8b04d19988211dbe3485cf62e3a25544ca4e80cac4",
}
LINUX_IMAGE = "aac6d3c5fa0c4fdf65f987af635f4cd55a06852b23046a4242a184acc2fd563b"
LINUX_DTB = "316dd15b329756be3887dea22f89fc8d1f5b055f8769761f4144b6b1caaea994"
PROBE = "2600e1e631d499a7cde6db90b3f067e6a63656a73c997ba418f7bcfa963f2239"
PROBE2 = "ef41ecec0b569d46240941bb81004482db6c1cbe7684f2dbc19c3d59021370c0"
INITRAMFS = "3f53e9ba6b39955e849b9ae64d04d5ae0fa4fb26d93298564db9deeb6c71d114"
READING = re.compile(
    rb"FREQ_PROBE cpu=(\d+) rep=(\d+) cycles=(\d+) "
    rb"enabled_ns=(\d+) running_ns=(\d+) wall_ns=(\d+) mhz=([\d.]+)"
)


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify_manifest():
    for line in (ROOT / "SHA256SUMS").read_text().splitlines():
        expected, filename = line.split("  ", 1)
        assert sha(ROOT / filename) == expected, filename


def verify_full20():
    baseline = json.loads(BASELINE.read_text())
    assert baseline["image_sha256"] == LINUX_IMAGE
    assert baseline["bench_sha256"] == BENCH_SHA
    expected = {(row["policy"], row["case"]) for row in baseline["results"]}
    assert len(expected) == 20
    assert sha(ROOT / "build.toml") == "d9be1a872ccb140fbd2d688e770bf6c988bc7394fbcc91e40600528c38bfa9a5"
    recorded = {}
    for filename in ("results.json", "results2.json", "results3.json"):
        data = json.loads((ROOT / filename).read_text())
        assert data["source_head"] == "e0cb95b459cedf349f9922c81776246281ee656e"
        assert data["feature_off_build_sha256"] == sha(ROOT / "build.toml")
        assert data["bench_sha256"] == BENCH_SHA
        assert data["board_id"] == "OrangePi-5-Plus-1"
        assert data["image_sha256"] == {key: IMAGES[key] for key in ("A", "B")}
        for entry in data["rounds"]:
            assert entry["tag"] not in recorded
            recorded[entry["tag"]] = entry
    valid = ("A1", "A2", "A3", "B2", "B4", "B5")
    invalid = ("B1", "B3")
    assert set(recorded) == set(valid)
    rows = {}
    for tag in valid + invalid:
        log_path = ROOT / f"{tag}-full.log"
        log = log_path.read_text()
        assert "WAKEUP_LATENCY_PROFILE_DONE" in log
        assert "WAKEUP_LATENCY_PASSED" in log
        assert (ROOT / f"{tag}.sha256").read_text().split()[0] == BENCH_SHA
        raw = [json.loads(line.split(" ", 1)[1]) for line in log.splitlines()
               if line.startswith("WAKEUP_LATENCY_RESULT ")]
        assert len(raw) == 20
        assert {(row["policy"], row["case"]) for row in raw} == expected
        assert sum(row["attempted"] for row in raw) == 380000
        assert all(row["missed_deadlines"] == 0 for row in raw)
        assert all(sum(row["histogram_counts"]) == row["samples"] for row in raw)
        if tag in invalid:
            assert sum(row["samples"] for row in raw) == 379999
            assert [(row["policy"], row["case"]) for row in raw if row["not_parked"]] == [
                ("other", "thread_futex_same_cpu")
            ]
            continue
        assert sum(row["samples"] for row in raw) == 380000
        assert all(row["not_parked"] == 0 for row in raw)
        assert raw == recorded[tag]["rows"]
        assert sha(log_path) == recorded[tag]["raw_log_sha256"]
        assert recorded[tag]["image_sha256"] == IMAGES[tag[0]]
        rows[tag] = {(row["policy"], row["case"]): row for row in raw}
    count = 0
    key_values = {}
    for reference in baseline["results"]:
        key = reference["policy"], reference["case"]
        on = statistics.median(rows[tag][key]["p50_ns"] for tag in ("A1", "A2", "A3"))
        off = statistics.median(rows[tag][key]["p50_ns"] for tag in ("B2", "B4", "B5"))
        count += reference["p50_ns"] * 10 >= off * 9
        key_values[key] = (reference["p50_ns"], on, off)
    assert key_values[("other", "thread_futex_same_cpu")] == (8458, 18667, 28000)
    assert key_values[("other", "sched_yield_handoff")] == (4667, 7875, 10791)
    assert key_values[("fifo", "absolute_timer_same_cpu")] == (13211, 19500, 27625)
    assert count == 10
    print(f"FEATURE_OFF_ORDINARY_NINETY {count}/20")


def verify_readings(raw_log, saved_rows, corrected):
    matches = READING.findall(raw_log)
    assert len(matches) == len(saved_rows) == 6
    assert [(int(row[0]), int(row[1])) for row in matches] == [
        (cpu, rep) for cpu in (0, 1) for rep in range(3)
    ]
    previous_running = {0: 0, 1: 0}
    previous_enabled = {0: 0, 1: 0}
    frequencies = []
    for raw, saved in zip(matches, saved_rows):
        cpu, rep, cycles, enabled, running, wall = map(int, raw[:6])
        displayed = float(raw[6])
        assert (cpu, rep, cycles, enabled, running, wall, displayed) == (
            saved["cpu"], saved["rep"], saved["cycles"], saved["enabled_ns"],
            saved["running_ns"], saved["wall_ns"], saved["mhz"]
        )
        delta_running = running - previous_running[cpu]
        delta_enabled = enabled - previous_enabled[cpu]
        previous_running[cpu] = running
        previous_enabled[cpu] = enabled
        assert 490_000_000 < delta_running < 510_000_000
        assert 490_000_000 < delta_enabled < 510_000_000
        assert 490_000_000 < wall < 510_000_000
        assert abs(delta_enabled - delta_running) < 1_000_000
        mhz = cycles * 1000 / delta_running
        if corrected or rep == 0:
            assert abs(mhz - displayed) < 0.002
        else:
            assert displayed < mhz * 0.6
        frequencies.append(mhz)
    return statistics.median(frequencies)


def verify_starry_pmu():
    assert sha(ROOT / "probe") == PROBE
    assert sha(ROOT / "probe2") == PROBE2
    medians = {}
    for filename, serial_name, probe_sha, corrected, tags in (
        ("starry-results3.json", "starry-serial3.log", PROBE, False,
         ("A3", "B1", "F1", "A4", "B2")),
        ("starry-results4.json", "starry-serial4.log", PROBE2, True,
         ("A5", "B3", "F2")),
    ):
        data = json.loads((ROOT / filename).read_text())
        serial = (ROOT / serial_name).read_bytes()
        assert data["board_id"] == "OrangePi-5-Plus-1"
        assert data["probe_sha256"] == probe_sha
        assert tuple(row["tag"] for row in data["rounds"]) == tags
        for round_ in data["rounds"]:
            tag = round_["tag"]
            output = (ROOT / f"{tag}.console.log").read_bytes()
            assert output in serial
            assert sha(ROOT / f"{tag}.console.log") == round_["console_sha256"]
            assert f"RESUME748_DONE {tag} 0".encode() in output
            assert round_["image_sha256"] == IMAGES[tag[0]]
            median = verify_readings(output, round_["readings"], corrected)
            assert 805 < median < 827 if tag[0] == "B" else 1140 < median < 1160
            medians[tag] = median
    for tag in ("A1", "A2"):
        output = (ROOT / f"{tag}.console.log").read_bytes()
        assert b"FREQ_PROBE cpu=" not in output
        assert f"RESUME748_DONE {tag} 7".encode() in output
    on = statistics.median(medians[tag] for tag in ("A3", "A4", "A5"))
    off = statistics.median(medians[tag] for tag in ("B1", "B2", "B3"))
    pgo = statistics.median(medians[tag] for tag in ("F1", "F2"))
    print(f"STARRY_ORDINARY_ON {on:.3f} MHz")
    print(f"STARRY_ORDINARY_OFF {off:.3f} MHz")
    print(f"STARRY_PGO_ON {pgo:.3f} MHz")
    return pgo


def verify_linux_pmu(pgo):
    assert sha(ROOT / "initramfs.cpio") == INITRAMFS
    assert sha(ROOT / "probe2") == PROBE2
    medians = []
    for tag in (3, 4):
        data = json.loads((ROOT / f"linux-results{tag}.json").read_text())
        boot = (ROOT / f"linux-boot{tag}.log").read_bytes()
        serial = (ROOT / f"linux-serial{tag}.log").read_bytes()
        assert data["board_id"] == "OrangePi-5-Plus-1"
        assert data["linux_sha256"] == LINUX_IMAGE
        assert data["dtb_sha256"] == LINUX_DTB
        assert data["probe_sha256"] == PROBE2
        assert data["initramfs_sha256"] == INITRAMFS
        assert data["boot_sha256"] == sha(ROOT / f"linux-boot{tag}.log")
        assert boot in serial
        assert b"Linux version" in boot and b"PREEMPT_RT" in boot
        assert b"RESUME749_INIT_DONE cpu0=0 cpu1=0" in boot
        assert b"fd818040: 00000110 00000082 00000000" in serial
        assert b"fd818280: 00000001" in serial
        assert b"fd818314: 0000803f 00000000 00000000" in serial
        median = verify_readings(boot, data["readings"], True)
        assert 815.9 < median < 816.1
        medians.append(median)
        print(f"LINUX_RT_BOOT_{tag} {median:.3f} MHz")
    linux = statistics.median(medians)
    print(f"PGO_OVER_LINUX_FREQUENCY {pgo / linux:.6f}")


def main():
    verify_manifest()
    verify_full20()
    pgo = verify_starry_pmu()
    verify_linux_pmu(pgo)


if __name__ == "__main__":
    main()
