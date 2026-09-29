#!/usr/bin/env python3
"""Recheck resume852 raw logs and receiver-policy contrasts."""

import hashlib
import json
import re
from pathlib import Path
from statistics import median


ROOT = Path(__file__).resolve().parent
MODES = ("fifo", "other", "other", "fifo", "fifo", "other")
CASES = ("empty", "bitset_miss", "bitset_hit")
ROW = re.compile(r"RESUME852_RESULT (\{[^\r\n]*\})")


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(message)


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def rows(text: str, mode: str) -> list[dict]:
    require("RESUME852_INVALID" not in text, f"invalid {mode} round")
    data = [json.loads(value) for value in ROW.findall(text)]
    require([row["case"] for row in data] == list(CASES), f"{mode} case order")
    require(all(row["mode"] == mode and row["samples"] == 20000 for row in data),
            f"{mode} samples or policy")
    return data


starry = json.loads((ROOT / "starry-run1/results.json").read_text())
linux = json.loads((ROOT / "linux-run1/results.json").read_text())
require(starry["state"] == linux["state"] == "collected", "incomplete run")
require(starry["board_released"] and linux["board_released"], "board not released")
require(starry["pll_verified"] and linux["pll_verified"], "PLL not verified")
require(starry["board_id"] == linux["board_id"] == "OrangePi-5-Plus-2", "board mismatch")
require(starry["benchmark_sha256"] == linux["benchmark_sha256"], "binary mismatch")
require(digest(ROOT / "wake_cost.aarch64") == starry["benchmark_sha256"], "binary SHA")
require(digest(ROOT / "initramfs.cpio") == linux["initramfs_sha256"], "initramfs SHA")

starry_data = []
require(len(starry["rounds"]) == 6 and starry["guest_exit"] == 0,
        "Starry group incomplete")
for number, mode in enumerate(MODES, 1):
    result = starry["rounds"][number - 1]
    path = ROOT / f"starry-run1/starry-{number}.log"
    text = path.read_text()
    policy = 1 if mode == "fifo" else 0
    require(result["round"] == number and result["mode"] == mode, "Starry order")
    require(digest(path) == result["log_sha256"], "Starry log SHA")
    require(f"RESUME852_POLICY mode={mode} sender=1 receiver={policy} cpu=0" in text,
            "Starry policy")
    require("RESUME852_DONE 0" in text and "DIAGNOSTIC_EXIT 0" in text,
            "Starry process exit")
    data = rows(text, mode)
    require(data == result["rows"], "Starry raw/JSON mismatch")
    starry_data.append(data)

boot = ROOT / "linux-run1/boot.log"
text = boot.read_text(errors="replace")
require(digest(boot) == linux["boot_sha256"], "Linux boot SHA")
require(linux["failures"] == 0 and text.count("RESUME852_DONE 0") == 6,
        "Linux process exit")
require("RESUME852_LINUX_INIT_DONE failures=0" in text, "Linux init exit")
raw = [json.loads(value) for value in ROW.findall(text)]
require(raw == linux["rows"] and len(raw) == 18, "Linux raw/JSON mismatch")
linux_data = []
for number, mode in enumerate(MODES, 1):
    policy = 1 if mode == "fifo" else 0
    require(text.count(f"RESUME852_POLICY mode={mode} sender=1 receiver={policy} cpu=0") == 3,
            "Linux policy")
    rows_from_json = raw[(number - 1) * 3:number * 3]
    require([row["case"] for row in rows_from_json] == list(CASES), "Linux case order")
    require(all(row["mode"] == mode and row["samples"] == 20000 for row in rows_from_json),
            "Linux samples")
    linux_data.append(rows_from_json)


def contrasts(groups: list[list[dict]], system: str) -> dict[str, int]:
    by_mode = {mode: [] for mode in ("fifo", "other")}
    for mode, data in zip(MODES, groups):
        empty, miss, hit = (row["p50_ns"] for row in data)
        by_mode[mode].append(hit - miss)
        print(f"{system} {mode}: E={empty} M={miss} H={hit} H-M={hit - miss} ns")
    return {mode: int(median(values)) for mode, values in by_mode.items()}


s = contrasts(starry_data, "Starry")
l = contrasts(linux_data, "Linux RT")
starry_class_increment = s["other"] - s["fifo"]
linux_class_increment = l["other"] - l["fifo"]
print(f"OTHER-FIFO H-M: Starry={starry_class_increment} ns, "
      f"Linux RT={linux_class_increment} ns, excess={starry_class_increment - linux_class_increment} ns")
require(starry_class_increment == 875 and linux_class_increment == 1167,
        "class contrast differs from archived decision")
print("resume852 raw logs, sample counts, identities, and contrasts: OK")
