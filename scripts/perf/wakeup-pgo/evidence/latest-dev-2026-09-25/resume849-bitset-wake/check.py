#!/usr/bin/env python3
"""Validate three-arm raw logs and report marginal-p50 contrasts."""

import hashlib
import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parent
ROW = re.compile(r"RESUME849_RESULT (\{[^\r\n]*\})")
CASES = ["empty", "bitset_miss", "bitset_hit"]


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require(condition: bool, detail: str) -> None:
    if not condition:
        raise SystemExit(detail)


def extract(text: str) -> list[dict]:
    require("RESUME849_INVALID" not in text, "invalid benchmark marker")
    return [json.loads(match) for match in ROW.findall(text)]


def validate_round(rows: list[dict], label: str) -> tuple[int, int, int]:
    require([row["case"] for row in rows] == CASES, f"{label}: case order")
    require(all(row["samples"] == 20000 for row in rows), f"{label}: samples")
    return tuple(row["p50_ns"] for row in rows)


starry = json.loads((ROOT / "starry-run1/results.json").read_text())
linux = json.loads((ROOT / "linux-run1/results.json").read_text())
require(starry["state"] == linux["state"] == "collected", "incomplete run")
require(starry["board_released"] and linux["board_released"], "session not released")
require(starry["pll_verified"] and linux["pll_verified"], "PLL not checked")
require(starry["board_id"] == linux["board_id"], "different boards")
require(starry["benchmark_sha256"] == linux["benchmark_sha256"], "different binaries")
require(digest(ROOT / "wake_cost.aarch64") == starry["benchmark_sha256"], "binary SHA")
require(digest(ROOT / "initramfs.cpio") == linux["initramfs_sha256"], "initramfs SHA")

starry_p50 = []
require(len(starry["rounds"]) == 3, "Starry round count")
for number, result in enumerate(starry["rounds"], 1):
    path = ROOT / f"starry-run1/starry-{number}.log"
    text = path.read_text()
    require(result["round"] == number and digest(path) == result["log_sha256"], "Starry log SHA")
    require("RESUME849_DONE 0" in text and "DIAGNOSTIC_EXIT 0" in text, "Starry exit")
    raw = extract(text)
    require(raw == result["rows"], "Starry raw/JSON mismatch")
    starry_p50.append(validate_round(raw, f"Starry {number}"))

boot = ROOT / "linux-run1/boot.log"
text = boot.read_text(errors="replace")
require(digest(boot) == linux["boot_sha256"], "Linux boot SHA")
require(linux["failures"] == 0, "Linux failures")
require(text.count("RESUME849_DONE 0") == 3, "Linux exit count")
require("RESUME849_LINUX_INIT_DONE failures=0" in text, "Linux init exit")
raw = extract(text)
require(raw == linux["rows"] and len(raw) == 9, "Linux raw/JSON mismatch")
linux_p50 = [validate_round(raw[index:index + 3], f"Linux {index // 3 + 1}")
             for index in range(0, 9, 3)]

for system, rounds in (("Starry", starry_p50), ("Linux RT", linux_p50)):
    for index, (empty, miss, hit) in enumerate(rounds, 1):
        print(f"{system} {index}: E={empty} M={miss} H={hit} "
              f"M-E={miss - empty} H-M={hit - miss} ns")
require(all(miss == empty for empty, miss, _ in starry_p50 + linux_p50),
        "nonempty mismatch p50 differs from empty")
print("resume849 raw logs, sample counts, identities, and contrasts: OK")
