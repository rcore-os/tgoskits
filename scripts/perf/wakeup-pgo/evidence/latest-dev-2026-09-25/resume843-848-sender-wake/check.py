#!/usr/bin/env python3
"""Check the archived resume847 wake-cost results against raw logs."""

import hashlib
import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parent / "resume847-lower-priority-wake"
ROW = re.compile(r"RESUME847_RESULT (\{[^\r\n]*\})")


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(message)


def rows(path: Path) -> list[dict]:
    log = path.read_text(errors="replace")
    require("RESUME847_INVALID" not in log, f"invalid run: {path}")
    return [json.loads(match) for match in ROW.findall(log)]


starry = json.loads((ROOT / "starry-run1/results.json").read_text())
linux = json.loads((ROOT / "linux-run1/results.json").read_text())
require(starry["state"] == linux["state"] == "collected", "incomplete run")
require(starry["board_released"] and linux["board_released"], "board not released")
require(starry["pll_verified"] and linux["pll_verified"], "PLL not verified")
require(starry["benchmark_sha256"] == linux["benchmark_sha256"], "different benchmarks")
require(digest(ROOT / "wake_cost.aarch64") == starry["benchmark_sha256"], "benchmark hash")
require(digest(ROOT / "initramfs.cpio") == linux["initramfs_sha256"], "initramfs hash")

for number, round_result in enumerate(starry["rounds"], 1):
    require(round_result["round"] == number, "Starry round order")
    path = ROOT / f"starry-run1/starry-{number}.log"
    log = path.read_text(errors="replace")
    require("RESUME847_DONE 0" in log and "DIAGNOSTIC_EXIT 0" in log, "Starry exit")
    require(digest(path) == round_result["log_sha256"], "Starry log hash")
    require(rows(path) == round_result["rows"], "Starry rows")

boot = ROOT / "linux-run1/boot.log"
boot_text = boot.read_text(errors="replace")
require(digest(boot) == linux["boot_sha256"], "Linux boot hash")
require(boot_text.count("RESUME847_DONE 0") == 2, "Linux exit count")
require("RESUME847_LINUX_INIT_DONE failures=0" in boot_text, "Linux init status")
require(linux["failures"] == 0 and rows(boot) == linux["rows"], "Linux rows")

for result in [*starry["rounds"][0]["rows"], *starry["rounds"][1]["rows"], *linux["rows"]]:
    require(result["samples"] == 20000, "incomplete samples")
require(starry["rounds"][0]["rows"][1]["p50_ns"] == 7000, "Starry parked p50")
require(starry["rounds"][1]["rows"][1]["p50_ns"] == 7000, "Starry parked p50")
require([linux["rows"][1]["p50_ns"], linux["rows"][3]["p50_ns"]] == [4375, 4084], "Linux parked p50")
print("resume847 raw logs, samples, and hashes: OK")
