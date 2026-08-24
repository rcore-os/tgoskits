#!/usr/bin/env python3
"""Summarize baseline/CNN/YOLO closed-loop runs without mixing model costs.

The input is the raw ``run.log`` from ``run-task3-switch.sh``.  The script
matches CONTROL and STATUS by request id, computes tracking metrics against the
target actually sent by each model, and reports model-marker statistics.  For
the fixture-replay YOLO adapter, ``infer_us`` is explicitly labelled as replay
overhead rather than ONNX inference time.
"""

from __future__ import annotations

import argparse
import csv
import math
import re
import statistics
from collections import Counter
from pathlib import Path


CONTROL_RE = re.compile(
    r"TASK3_CONTROL_SENT elapsed_ms=(\d+) request=(\d+) value=(\d+) "
    r"target=(\d+) state=(\d+) seq=(\d+) model=([^\s]+)"
)
STATUS_RE = re.compile(
    r"TASK3_STATUS_RECEIVED elapsed_ms=(\d+) request=(\d+) value=(\d+) "
    r"state=(\d+) sample=(\d+) rtt_ms=(\d+)"
)
INFER_RE = re.compile(
    r"TASK3_INFER elapsed_ms=(\d+) sample=(\d+) output=([\d.+-]+) infer_us=(\d+)"
)
DETECTION_RE = re.compile(r"TASK3_DETECTION .*confidence_milli=(\d+)")


def percentile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = max(0, min(len(ordered) - 1, math.ceil(fraction * len(ordered)) - 1))
    return ordered[index]


def parse_log(path: Path, declared_mode: str) -> tuple[list[dict[str, float | int | str]], dict[str, object]]:
    controls: dict[int, tuple[int, int, int, int, str]] = {}
    infers: dict[int, tuple[int, float, int]] = {}
    rows: list[dict[str, float | int | str]] = []
    detections = 0
    rejections = 0
    confidences: list[float] = []
    model_ready = "unknown"
    text = path.read_text(encoding="utf-8", errors="replace")
    for line in text.splitlines():
        if "TASK3_MODEL_READY" in line:
            match = re.search(r"TASK3_MODEL_READY model=([^\s]+)", line)
            if match:
                model_ready = match.group(1)
        if "TASK3_MODEL_REJECTED" in line:
            rejections += 1
        detection = DETECTION_RE.search(line)
        if detection:
            detections += 1
            confidences.append(int(detection.group(1)) / 1000.0)
        infer = INFER_RE.search(line)
        if infer:
            elapsed, sample, output, infer_us = infer.groups()
            infers[int(sample)] = (int(elapsed), float(output), int(infer_us))
        control = CONTROL_RE.search(line)
        if control:
            elapsed, request, value, target, state, _seq, model = control.groups()
            controls[int(request)] = (
                int(elapsed),
                int(value),
                int(target),
                int(state),
                model,
            )
        status = STATUS_RE.search(line)
        if status:
            elapsed, request, value, state, sample, rtt = status.groups()
            control_row = controls.get(int(request))
            if control_row is None:
                continue
            c_elapsed, control_value, target, state_before, model = control_row
            infer_row = infers.get(int(sample))
            rows.append(
                {
                    "sample": int(sample),
                    "elapsed_ms": int(elapsed),
                    "control_elapsed_ms": c_elapsed,
                    "target": target,
                    "state_before": state_before,
                    "control_value": control_value,
                    "state_after": int(state),
                    "rtt_ms": int(rtt),
                    "infer_us": infer_row[2] if infer_row else None,
                    "model": model,
                }
            )
    rows.sort(key=lambda row: int(row["sample"]))
    mode = declared_mode
    if mode == "ai":
        mode = "cnn"
    return rows, {
        "mode": mode,
        "model_ready": model_ready,
        "detections": detections,
        "rejections": rejections,
        "confidence_mean": statistics.fmean(confidences) if confidences else None,
        "confidence_p95": percentile(confidences, 0.95),
    }


def settle_ms(rows: list[dict[str, float | int | str]]) -> float | None:
    """Return median settling time for stable target steps, if measurable.

    YOLO intentionally changes the target per fixture frame; its target runs
    are too short for a step-response settling claim and therefore return N/A.
    """
    if len(rows) < 4:
        return None
    runs: list[list[dict[str, float | int | str]]] = []
    current: list[dict[str, float | int | str]] = [rows[0]]
    for row in rows[1:]:
        if row["target"] == current[-1]["target"]:
            current.append(row)
        else:
            runs.append(current)
            current = [row]
    runs.append(current)
    settling: list[float] = []
    for run in runs:
        if len(run) < 3:
            continue
        target = float(run[0]["target"])
        band = max(20.0, target * 0.05)
        for index, candidate in enumerate(run):
            if all(abs(float(rest["state_after"]) - target) <= band for rest in run[index:]):
                settling.append(float(candidate["elapsed_ms"]) - float(run[0]["elapsed_ms"]))
                break
    return statistics.median(settling) if settling else None


def scenario_target(elapsed_ms: int) -> int:
    if elapsed_ms < 5_000:
        return 300
    if elapsed_ms < 15_000:
        return 800
    return 500


def scenario_metrics(rows: list[dict[str, float | int | str]]) -> dict[str, float | None]:
    """Metrics against the frozen plant scenario, independent of model target.

    This is the cross-model comparison metric.  The model-target metric above
    remains useful for measuring how well a model's requested setpoint was
    applied, but YOLO fixture replay deliberately emits varying bounded targets
    and therefore cannot be compared to a fixed-step model using that metric.
    """
    errors = [scenario_target(int(row["elapsed_ms"])) - int(row["state_after"]) for row in rows]
    out: dict[str, float | None] = {
        "scenario_tracking_rmse": math.sqrt(statistics.fmean(x * x for x in errors)) if errors else None,
    }
    for name, start, end, target in (("t300", 0, 5_000, 300), ("t800", 5_000, 15_000, 800), ("t500", 15_000, 25_000, 500)):
        segment = [row for row in rows if start <= int(row["elapsed_ms"]) < end]
        seg_errors = [target - int(row["state_after"]) for row in segment]
        out[f"scenario_{name}_rmse"] = math.sqrt(statistics.fmean(x * x for x in seg_errors)) if seg_errors else None
        out[f"scenario_{name}_overshoot"] = max((max(0, int(row["state_after"]) - target) for row in segment), default=None)
        settling: float | None = None
        band = max(20.0, target * 0.05)
        for index, row in enumerate(segment):
            if all(abs(int(rest["state_after"]) - target) <= band for rest in segment[index:]):
                settling = float(int(row["elapsed_ms"]) - start)
                break
        out[f"scenario_{name}_settle_ms"] = settling
    return out


def summarize(path: Path, mode: str) -> dict[str, object]:
    rows, markers = parse_log(path, mode)
    errors = [float(row["target"]) - float(row["state_after"]) for row in rows]
    rtts = [float(row["rtt_ms"]) for row in rows]
    infer = [float(row["infer_us"]) for row in rows if row["infer_us"] is not None]
    positive_overshoot = [
        max(0.0, float(row["state_after"]) - float(row["target"])) for row in rows
    ]
    result = {
        "run": path.parent.name,
        "mode": markers["mode"],
        "model_ready": markers["model_ready"],
        "samples": len(rows),
        "first_ms": rows[0]["elapsed_ms"] if rows else None,
        "last_ms": rows[-1]["elapsed_ms"] if rows else None,
        "tracking_rmse": math.sqrt(statistics.fmean(x * x for x in errors)) if errors else None,
        "mean_rtt_ms": statistics.fmean(rtts) if rtts else None,
        "p95_rtt_ms": percentile(rtts, 0.95),
        "settling_ms": settle_ms(rows),
        "max_positive_overshoot": max(positive_overshoot) if positive_overshoot else None,
        "detection_count": markers["detections"],
        "rejection_count": markers["rejections"],
        "confidence_mean": markers["confidence_mean"],
        "confidence_p95": markers["confidence_p95"],
        "replay_infer_samples": len(infer),
        "replay_infer_mean_us": statistics.fmean(infer) if infer else None,
        "replay_infer_p95_us": percentile(infer, 0.95),
    }
    result.update(scenario_metrics(rows))
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("logs", nargs="+", type=Path)
    parser.add_argument("--modes", required=True, help="comma-separated baseline,cnn,yolo labels")
    parser.add_argument("--out-dir", type=Path, required=True)
    args = parser.parse_args()
    modes = args.modes.split(",")
    if len(modes) != len(args.logs):
        parser.error("--modes must contain one mode per log")
    if any(mode not in {"baseline", "cnn", "yolo", "ai"} for mode in modes):
        parser.error("unsupported mode")
    args.out_dir.mkdir(parents=True, exist_ok=True)
    rows = [summarize(path, mode) for path, mode in zip(args.logs, modes)]
    fields = list(rows[0])
    with (args.out_dir / "model-quant-summary.csv").open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=fields)
        writer.writeheader()
        writer.writerows(rows)
    groups: dict[str, list[dict[str, object]]] = {}
    for row in rows:
        groups.setdefault(str(row["mode"]), []).append(row)
    aggregate_fields = ["mode", "runs", "samples_median", "tracking_rmse_median", "scenario_tracking_rmse_median", "mean_rtt_median_ms", "p95_rtt_median_ms", "settling_median_ms", "max_overshoot_median", "scenario_t300_rmse_median", "scenario_t800_rmse_median", "scenario_t500_rmse_median", "scenario_t300_settle_median_ms", "scenario_t800_settle_median_ms", "scenario_t500_settle_median_ms", "scenario_t300_overshoot_median", "scenario_t800_overshoot_median", "scenario_t500_overshoot_median", "detections_median", "rejections_median", "confidence_mean_median", "replay_infer_mean_median_us", "replay_infer_p95_median_us"]
    aggregates: list[dict[str, object]] = []
    for mode, group in groups.items():
        def med(key: str):
            values = [float(item[key]) for item in group if item[key] is not None]
            return statistics.median(values) if values else None
        aggregates.append({
            "mode": mode,
            "runs": len(group),
            "samples_median": med("samples"),
            "tracking_rmse_median": med("tracking_rmse"),
            "scenario_tracking_rmse_median": med("scenario_tracking_rmse"),
            "mean_rtt_median_ms": med("mean_rtt_ms"),
            "p95_rtt_median_ms": med("p95_rtt_ms"),
            "settling_median_ms": med("settling_ms"),
            "max_overshoot_median": med("max_positive_overshoot"),
            "scenario_t300_rmse_median": med("scenario_t300_rmse"),
            "scenario_t800_rmse_median": med("scenario_t800_rmse"),
            "scenario_t500_rmse_median": med("scenario_t500_rmse"),
            "scenario_t300_settle_median_ms": med("scenario_t300_settle_ms"),
            "scenario_t800_settle_median_ms": med("scenario_t800_settle_ms"),
            "scenario_t500_settle_median_ms": med("scenario_t500_settle_ms"),
            "scenario_t300_overshoot_median": med("scenario_t300_overshoot"),
            "scenario_t800_overshoot_median": med("scenario_t800_overshoot"),
            "scenario_t500_overshoot_median": med("scenario_t500_overshoot"),
            "settling_median_ms": med("settling_ms"),
            "max_overshoot_median": med("max_positive_overshoot"),
            "detections_median": med("detection_count"),
            "rejections_median": med("rejection_count"),
            "confidence_mean_median": med("confidence_mean"),
            "replay_infer_mean_median_us": med("replay_infer_mean_us"),
            "replay_infer_p95_median_us": med("replay_infer_p95_us"),
        })
    with (args.out_dir / "model-quant-aggregate.csv").open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=aggregate_fields)
        writer.writeheader()
        writer.writerows(aggregates)
    print(f"wrote {args.out_dir / 'model-quant-summary.csv'}")
    print(f"wrote {args.out_dir / 'model-quant-aggregate.csv'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
