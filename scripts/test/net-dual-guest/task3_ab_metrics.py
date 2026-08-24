#!/usr/bin/env python3
"""Analyze the integrated RK3588 Task-3 manual-versus-YOLO experiment."""

from __future__ import annotations

import argparse
import csv
import json
import re
import statistics
from dataclasses import asdict, dataclass
from pathlib import Path


ANSI_ESCAPE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")


@dataclass
class Sample:
    index: int
    image_id: str
    image_sha256: str
    truth_target: int | None
    expected: str
    source: str
    outcome: str
    request: int
    infer_us: int | None = None
    detected_target: int | None = None
    sent_target: int | None = None
    state_before: int | None = None
    state_after: int | None = None
    rtt_ms: int | None = None


@dataclass
class Run:
    mode: str
    samples: list[Sample]
    declared_samples: int


def fields(line: str) -> dict[str, str]:
    result: dict[str, str] = {}
    for token in line.split()[1:]:
        if "=" in token:
            key, value = token.split("=", 1)
            result[key] = value
    return result


def parse_optional_target(value: str) -> int | None:
    return None if value == "none" else int(value)


def parse_log(log: str, mode: str) -> Run:
    if mode not in {"manual", "yolo"}:
        raise ValueError(f"unsupported Task-3 A/B mode: {mode}")
    samples: dict[int, Sample] = {}
    inference_by_request: dict[int, int] = {}
    detection_by_request: dict[int, int] = {}
    controls_by_sample: dict[int, dict[str, str]] = {}
    statuses_by_sample: dict[int, dict[str, str]] = {}
    declared_samples: int | None = None

    for raw_line in log.splitlines():
        line = ANSI_ESCAPE.sub("", raw_line).strip()
        if line.startswith("TASK3_SAMPLE "):
            item = fields(line)
            index = int(item["sample"])
            if index in samples:
                raise ValueError(f"duplicate Task-3 sample index {index}")
            if item["source"] != mode:
                raise ValueError(
                    f"sample {index} source {item['source']} does not match mode {mode}"
                )
            samples[index] = Sample(
                index=index,
                image_id=item["image_id"],
                image_sha256=item["image_sha256"],
                truth_target=parse_optional_target(item["truth_target"]),
                expected=item["expected"],
                source=item["source"],
                outcome=item["outcome"],
                request=int(item["request"]),
            )
        elif line.startswith("TASK3_INFER "):
            item = fields(line)
            inference_by_request[int(item["request"])] = int(item["infer_us"])
        elif line.startswith("TASK3_DETECTION "):
            item = fields(line)
            detection_by_request[int(item["request"])] = int(item["center_x_milli"])
        elif line.startswith("TASK3_CONTROL_SENT "):
            item = fields(line)
            controls_by_sample[int(item["sample"])] = item
        elif line.startswith("TASK3_STATUS_RECEIVED "):
            item = fields(line)
            statuses_by_sample[int(item["sample"])] = item
        elif line.startswith("TASK3_EXPERIMENT_COMPLETE "):
            item = fields(line)
            if item.get("run_mode") != mode:
                raise ValueError("experiment completion mode does not match requested mode")
            declared_samples = int(item["samples"])

    if declared_samples is None:
        raise ValueError("missing TASK3_EXPERIMENT_COMPLETE marker")
    if len(samples) != declared_samples:
        raise ValueError(
            f"completion declares {declared_samples} samples but log contains {len(samples)}"
        )

    ordered = [samples[index] for index in sorted(samples)]
    for sample in ordered:
        sample.infer_us = inference_by_request.get(sample.request)
        sample.detected_target = detection_by_request.get(sample.request)
        control = controls_by_sample.get(sample.index)
        status = statuses_by_sample.get(sample.index)
        if control is not None:
            if control["image_id"] != sample.image_id or control["source"] != mode:
                raise ValueError(f"sample {sample.index} CONTROL identity mismatch")
            sample.sent_target = int(control["value"])
            sample.state_before = int(control["state"])
        if status is not None:
            if status["image_id"] != sample.image_id:
                raise ValueError(f"sample {sample.index} STATUS identity mismatch")
            if control is None:
                raise ValueError(f"sample {sample.index} has STATUS without CONTROL")
            sample.state_before = int(status["state_before"])
            sample.state_after = int(status["state_after"])
            sample.rtt_ms = int(status["rtt_ms"])
        if control is not None and status is None:
            raise ValueError(f"sample {sample.index} has CONTROL without STATUS")
        if sample.outcome == "accepted" and (control is None or status is None):
            raise ValueError(f"accepted sample {sample.index} has no complete CONTROL/STATUS")
        if sample.outcome == "rejected" and (control is not None or status is not None):
            raise ValueError(f"rejected sample {sample.index} unexpectedly issued CONTROL")
        if sample.outcome not in {"accepted", "rejected"}:
            raise ValueError(f"sample {sample.index} has invalid outcome {sample.outcome}")
        if mode == "yolo" and sample.infer_us is None:
            raise ValueError(f"YOLO sample {sample.index} has no inference timing")
    return Run(mode=mode, samples=ordered, declared_samples=declared_samples)


def parse_path(path: Path, mode: str) -> Run:
    return parse_log(path.read_text(encoding="utf-8", errors="replace"), mode)


def mean_absolute(values: list[int]) -> float | None:
    return statistics.fmean(abs(value) for value in values) if values else None


def summarize(run: Run) -> dict[str, int | float | None | str]:
    target_errors = [
        sample.sent_target - sample.truth_target
        for sample in run.samples
        if sample.expected == "accept"
        and sample.sent_target is not None
        and sample.truth_target is not None
    ]
    state_errors = [
        sample.state_after - sample.truth_target
        for sample in run.samples
        if sample.expected == "accept"
        and sample.state_after is not None
        and sample.truth_target is not None
    ]
    perception_errors = [
        sample.detected_target - sample.truth_target
        for sample in run.samples
        if sample.expected == "accept"
        and sample.detected_target is not None
        and sample.truth_target is not None
    ]
    rtts = [sample.rtt_ms for sample in run.samples if sample.rtt_ms is not None]
    inference = [sample.infer_us for sample in run.samples if sample.infer_us is not None]
    expected_matches = (
        [
            (sample.expected == "accept") == (sample.outcome == "accepted")
            for sample in run.samples
        ]
        if run.mode == "yolo"
        else []
    )
    expected_rejections = (
        [sample for sample in run.samples if sample.expected == "reject"]
        if run.mode == "yolo"
        else []
    )
    safe_rejections = [
        sample for sample in expected_rejections if sample.outcome == "rejected"
    ]
    return {
        "mode": run.mode,
        "samples": len(run.samples),
        "control_status_complete": sum(
            sample.sent_target is not None and sample.state_after is not None
            for sample in run.samples
        ),
        "perception_mae": mean_absolute(perception_errors),
        "target_mae": mean_absolute(target_errors),
        "state_mae": mean_absolute(state_errors),
        "mean_rtt_ms": statistics.fmean(rtts) if rtts else None,
        "max_rtt_ms": max(rtts) if rtts else None,
        "mean_infer_us": statistics.fmean(inference) if inference else None,
        "expected_behavior_accuracy": (
            statistics.fmean(expected_matches) if expected_matches else None
        ),
        "safe_rejection_rate": (
            len(safe_rejections) / len(expected_rejections)
            if expected_rejections
            else None
        ),
    }


def verify_comparable(manual: Run, yolo: Run) -> None:
    manual_identity = [
        (sample.index, sample.image_id, sample.image_sha256, sample.truth_target, sample.expected)
        for sample in manual.samples
    ]
    yolo_identity = [
        (sample.index, sample.image_id, sample.image_sha256, sample.truth_target, sample.expected)
        for sample in yolo.samples
    ]
    if manual_identity != yolo_identity:
        raise ValueError("manual and YOLO runs do not use the same frozen image manifest")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manual", required=True, type=Path)
    parser.add_argument("--yolo", required=True, type=Path)
    parser.add_argument("--out-dir", required=True, type=Path)
    args = parser.parse_args()

    manual = parse_path(args.manual, "manual")
    yolo = parse_path(args.yolo, "yolo")
    verify_comparable(manual, yolo)
    args.out_dir.mkdir(parents=True, exist_ok=True)

    for run in (manual, yolo):
        with (args.out_dir / f"{run.mode}.csv").open(
            "w", newline="", encoding="utf-8"
        ) as stream:
            writer = csv.DictWriter(stream, fieldnames=list(asdict(run.samples[0])))
            writer.writeheader()
            writer.writerows(asdict(sample) for sample in run.samples)
    summary = [summarize(manual), summarize(yolo)]
    (args.out_dir / "summary.json").write_text(
        json.dumps(summary, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )
    with (args.out_dir / "summary.csv").open("w", newline="", encoding="utf-8") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(summary[0]))
        writer.writeheader()
        writer.writerows(summary)
    print(f"PASS: Task-3 integrated A/B metrics written to {args.out_dir}")


if __name__ == "__main__":
    main()
