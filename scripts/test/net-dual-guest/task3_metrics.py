#!/usr/bin/env python3
"""Parse Task-3 control-loop logs into CSV and compute closed-loop metrics.

The script is intentionally stateless: every metric is recomputed from the
raw log markers (TASK3_CONTROL_SENT / TASK3_STATUS_RECEIVED / TASK3_INFER),
so the recorded CSVs can always be regenerated.

Usage:
  python3 scripts/test/net-dual-guest/task3_metrics.py <log> [<log> ...] \
      --out-dir <dir> --label <name> [--plot <png>]

Each input log contributes one run; --label names the run for the summary
table.  The per-run CSV row format is:

  sample,elapsed_ms,target,state_before,control_value,state_after,rtt_ms,model_output,infer_us,mode
"""

import argparse
import csv
import re
import statistics
from pathlib import Path

CONTROL_RE = re.compile(
    r"TASK3_CONTROL_SENT elapsed_ms=(\d+) request=(\d+) value=(\d+) target=(\d+) state=(\d+) seq=(\d+)"
)
INFER_RE = re.compile(
    r"TASK3_INFER elapsed_ms=(\d+) sample=(\d+) output=([\d.+-]+) infer_us=(\d+)"
)
STATUS_RE = re.compile(
    r"TASK3_STATUS_RECEIVED elapsed_ms=(\d+) request=(\d+) value=(\d+) state=(\d+) sample=(\d+) rtt_ms=(\d+)"
)


def parse_run(log_path: Path, mode: str):
    rows = []
    control_by_req = {}
    infer_by_sample = {}
    with open(log_path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            infer = INFER_RE.search(line)
            if infer:
                elapsed, sample, output, infer_us = infer.groups()
                infer_by_sample[int(sample)] = (float(output), int(infer_us), int(elapsed))
                continue
            control = CONTROL_RE.search(line)
            if control:
                elapsed, request, value, target, state, _seq = control.groups()
                control_by_req[int(request)] = (int(elapsed), int(value), int(target), int(state))
                continue
            status = STATUS_RE.search(line)
            if status:
                elapsed, request, value, state, sample, rtt = status.groups()
                req = control_by_req.get(int(request))
                if req is None:
                    continue
                c_elapsed, control_value, target, state_before = req
                infer = infer_by_sample.get(int(sample))
                model_output = infer[0] if infer else None
                infer_us = infer[1] if infer else None
                rows.append(
                    {
                        "sample": int(sample),
                        "elapsed_ms": int(elapsed),
                        "target": target,
                        "state_before": state_before,
                        "control_value": control_value,
                        "state_after": int(state),
                        "rtt_ms": int(rtt),
                        "model_output": model_output,
                        "infer_us": infer_us,
                        "mode": mode,
                    }
                )
    rows.sort(key=lambda row: row["sample"])
    return rows


def rmse(values):
    if not values:
        return None
    return (sum(v * v for v in values) / len(values)) ** 0.5


def segment_metrics(rows):
    """Per-target-segment metrics.  Targets: 0-5s=300, 5-15s=800, 15-25s=500."""
    segments = [
        ("t300", 0, 5_000, 300),
        ("t800", 5_000, 15_000, 800),
        ("t500", 15_000, 25_000, 500),
    ]
    out = []
    for name, start_ms, end_ms, target in segments:
        seg = [
            row
            for row in rows
            if start_ms <= row["elapsed_ms"] < end_ms and row["target"] == target
        ]
        errors = [target - row["state_after"] for row in seg]
        out.append(
            {
                "segment": name,
                "samples": len(seg),
                "rmse": rmse(errors),
                "settle_ms": settling_time(seg, target, start_ms),
                "max_overshoot": max_overshoot(seg, target),
                "mean_rtt_ms": (
                    statistics.fmean(row["rtt_ms"] for row in seg) if seg else None
                ),
            }
        )
    return out


def settling_time(seg, target, start_ms):
    """First elapsed_ms (relative to segment start) after which |error| stays
    within 5% of the segment target for the rest of the segment."""
    if not seg:
        return None
    band = max(20, 0.05 * target)
    for index, row in enumerate(seg):
        if all(abs(target - s["state_after"]) <= band for s in seg[index:]):
            return max(0, row["elapsed_ms"] - start_ms)
    return None


def max_overshoot(seg, target):
    """Largest deviation above the target after the step (undershoot reported
    as negative deviation below target is ignored; overshoot is positive)."""
    if not seg:
        return None
    return max(0, max(row["state_after"] - target for row in seg))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("logs", nargs="+", type=Path)
    parser.add_argument("--out-dir", type=Path, default=Path("results/task3"))
    parser.add_argument("--label", type=str, default="run")
    parser.add_argument("--modes", type=str, default=None,
                        help="comma-separated mode per log: ai|baseline")
    parser.add_argument("--plot", type=Path)
    args = parser.parse_args()

    if args.modes:
        modes = args.modes.split(",")
        if len(modes) != len(args.logs):
            raise SystemExit("--modes must have one entry per log")
    else:
        modes = ["ai" if "ai" in label else "baseline" for label in [args.label] * len(args.logs)]

    args.out_dir.mkdir(parents=True, exist_ok=True)
    summary_rows = []
    for index, (log_path, mode) in enumerate(zip(args.logs, modes), start=1):
        label = f"{args.label}-{index}" if len(args.logs) > 1 else args.label
        rows = parse_run(log_path, mode)
        csv_path = args.out_dir / f"{label}.csv"
        with open(csv_path, "w", newline="", encoding="utf-8") as handle:
            writer = csv.DictWriter(
                handle,
                fieldnames=[
                    "sample",
                    "elapsed_ms",
                    "target",
                    "state_before",
                    "control_value",
                    "state_after",
                    "rtt_ms",
                    "model_output",
                    "infer_us",
                    "mode",
                ],
            )
            writer.writeheader()
            writer.writerows(rows)
        overall = rmse([row["target"] - row["state_after"] for row in rows])
        segs = segment_metrics(rows)
        summary_rows.append(
            {
                "run": label,
                "mode": mode,
                "samples": len(rows),
                "first_ms": rows[0]["elapsed_ms"] if rows else None,
                "last_ms": rows[-1]["elapsed_ms"] if rows else None,
                "overall_rmse": overall,
                "mean_rtt_ms": (
                    statistics.fmean(row["rtt_ms"] for row in rows) if rows else None
                ),
                **{f"{s['segment']}_rmse": s["rmse"] for s in segs},
                **{f"{s['segment']}_settle_ms": s["settle_ms"] for s in segs},
                **{f"{s['segment']}_overshoot": s["max_overshoot"] for s in segs},
            }
        )
        print(f"wrote {csv_path} ({len(rows)} cycles)")

    summary_path = args.out_dir / "summary.csv"
    with open(summary_path, "w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(summary_rows[0]))
        writer.writeheader()
        writer.writerows(summary_rows)
    print(f"wrote {summary_path}")

    if args.plot and args.logs:
        try:
            import matplotlib

            matplotlib.use("Agg")
            import matplotlib.pyplot as plt
        except ImportError:
            print("matplotlib not available; skipping plot")
            return
        fig, axes = plt.subplots(len(args.logs), 1, figsize=(8, 4 * len(args.logs)), sharex=True)
        if len(args.logs) == 1:
            axes = [axes]
        for axis, log_path in zip(axes, args.logs):
            rows = parse_run(log_path, mode)
            times = [row["elapsed_ms"] / 1000 for row in rows]
            axis.step(times, [row["target"] for row in rows], where="post", label="target")
            axis.step(times, [row["state_after"] for row in rows], where="post", label="state")
            axis.step(times, [row["control_value"] for row in rows], where="post", label="control")
            axis.legend()
            axis.grid(True)
        fig.tight_layout()
        fig.savefig(args.plot, dpi=120)
        print(f"wrote {args.plot}")


if __name__ == "__main__":
    main()
