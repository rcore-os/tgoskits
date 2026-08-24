#!/usr/bin/env python3
import re
import sys
from pathlib import Path


def fields(path: Path, marker: str) -> dict[str, int | str]:
    text = path.read_text(errors="replace")
    match = re.search(rf"{marker} (.+)", text)
    if not match:
        raise SystemExit(f"missing {marker} in {path}")
    parsed: dict[str, int | str] = {}
    for key, value in re.findall(r"([a-z0-9_]+)=([^ ]+)", match.group(1)):
        parsed[key] = int(value, 0) if re.fullmatch(r"(?:0x[0-9a-f]+|[0-9]+)", value) else value
    return parsed


def reduction(before: int, after: int) -> float:
    return 0.0 if before == 0 else (before - after) * 100.0 / before


root = Path(sys.argv[1])


def storm_path(case: str) -> Path:
    direct = root / case / "timer-storm.txt"
    if direct.is_file():
        return direct
    return root / case / "stress-noiso/timer-storm.txt"


global_path = storm_path("global-lock")
percpu_path = storm_path("per-cpu-lock")
global_result = fields(global_path, "RT_TIMER_STORM_RESULT")
percpu_result = fields(percpu_path, "RT_TIMER_STORM_RESULT")
global_lock = fields(global_path, "RT_TIMER_STORM_LOCK")
percpu_lock = fields(percpu_path, "RT_TIMER_STORM_LOCK")
global_expiry = fields(global_path, "RT_TIMER_STORM_EXPIRY")
percpu_expiry = fields(percpu_path, "RT_TIMER_STORM_EXPIRY")

throughput_speedup = percpu_result["pairs_per_second"] / global_result["pairs_per_second"]
summary = f"""timer-wheel single-variable A/B
global_pairs_per_second={global_result['pairs_per_second']}
percpu_pairs_per_second={percpu_result['pairs_per_second']}
throughput_speedup={throughput_speedup:.3f}x
global_lock_wait_total_ns={global_lock['wait_total_ns']}
percpu_lock_wait_total_ns={percpu_lock['wait_total_ns']}
lock_wait_max_reduction_pct={reduction(global_lock['wait_max_ns'], percpu_lock['wait_max_ns']):.3f}
lock_wait_total_reduction_pct={reduction(global_lock['wait_total_ns'], percpu_lock['wait_total_ns']):.3f}
global_expiry_p99_late_ns={global_expiry['p99_late_ns']}
percpu_expiry_p99_late_ns={percpu_expiry['p99_late_ns']}
expiry_p99_reduction_pct={reduction(global_expiry['p99_late_ns'], percpu_expiry['p99_late_ns']):.3f}
expiry_max_reduction_pct={reduction(global_expiry['max_late_ns'], percpu_expiry['max_late_ns']):.3f}
"""
(root / "summary.txt").write_text(summary)
print(summary, end="")
