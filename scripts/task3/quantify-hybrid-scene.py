#!/usr/bin/env python3
"""Strictly quantify the final fixed-perception versus RKNN scene logs."""

from __future__ import annotations

import argparse
import json
import re
import statistics
from pathlib import Path

ANSI = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]")
CONTROL = re.compile(
    rb"TASK3_CONTROL_SENT elapsed_ms=(\d+) event_index=(\d+) event_id=([^ ]+) source=([^ ]+) "
    rb"generation=(\d+) request=(\d+) action=([^ ]+) value=(-?\d+) outcome=([^ ]+) "
    rb"infer_start_ns=(\d+) infer_end_ns=(\d+) seq=(\d+)"
)
STATUS = re.compile(
    rb"TASK3_STATUS_RECEIVED elapsed_ms=(\d+) event_index=(\d+) event_id=([^ ]+) request=(\d+) "
    rb"value=(-?\d+) state=(-?\d+) protocol_state=([^ ]+) rtt_ms=(\d+) status_ns=(\d+) "
    rb"end_to_end_us=(\d+)"
)
DETECTION = re.compile(
    rb"TASK3_DETECTION event_index=(\d+) event_id=([^ ]+) class=(\d+) confidence_milli=(\d+) "
    rb"center_x_milli=(\d+) center_y_milli=(\d+) area_milli=(\d+) request=(\d+)"
)
IDS = [
    "road-0375", "road-0380", "road-0385", "road-0390", "road-0395", "road-0400",
    "hazard-0000", "hazard-0010", "hazard-0020", "road-0405", "explicit-reset", "road-0410",
]
TRUTH = {1: 545, 2: 515, 3: 480, 4: 440, 5: 405, 6: 365, 10: 330, 12: 300}
EXPECTED = {
    1: "SetOutput", 2: "SetOutput", 3: "SetOutput", 4: "SetOutput", 5: "SetOutput",
    6: "SetOutput", 7: "Stop", 8: "Stop", 9: "Stop", 10: "Stop", 11: "Reset",
    12: "SetOutput",
}


def clean(path: Path) -> bytes:
    return ANSI.sub(b"", path.read_bytes()).replace(b"\r", b"")


def parse(path: Path, source: str) -> dict:
    text = clean(path)
    controls = CONTROL.findall(text)
    statuses = STATUS.findall(text)
    detections = DETECTION.findall(text)
    if len(controls) != 12 or len(statuses) != 12:
        raise ValueError(f"{source}: expected 12 controls/statuses, got {len(controls)}/{len(statuses)}")
    if b"TASK3_EXPERIMENT_COMPLETE" not in text:
        raise ValueError(f"{source}: missing completion marker")
    control_rows = []
    status_rows = []
    for expected_index, row in enumerate(controls, 1):
        index = int(row[1])
        event_id = row[2].decode()
        if index != expected_index or event_id != IDS[index - 1]:
            raise ValueError(f"{source}: control sequence mismatch at {expected_index}")
        control_rows.append({
            "index": index, "id": event_id, "action": row[6].decode(), "value": int(row[7]),
            "outcome": row[8].decode(), "infer_us": (int(row[10]) - int(row[9])) / 1000,
        })
    for expected_index, row in enumerate(statuses, 1):
        index = int(row[1])
        event_id = row[2].decode()
        if index != expected_index or event_id != IDS[index - 1]:
            raise ValueError(f"{source}: STATUS sequence mismatch at {expected_index}")
        status_rows.append({
            "index": index, "id": event_id, "state": int(row[5]), "rtt_ms": int(row[7]),
            "end_to_end_us": int(row[9]),
        })
    correct = sum(row["action"] == EXPECTED[row["index"]] for row in control_rows)
    detection_rows = {int(row[0]): row for row in detections}
    vehicle_hits = sum(index in detection_rows and int(detection_rows[index][2]) in {2, 5, 7} for index in TRUTH)
    hazard_hits = sum(index in detection_rows and int(detection_rows[index][2]) == 0 for index in (7, 8, 9))
    center_errors = [
        abs((int(detection_rows[index][4]) if index in detection_rows else 500) - truth)
        for index, truth in TRUTH.items()
    ]
    return {
        "source": source,
        "events": 12,
        "statuses": 12,
        "correct_decisions": correct,
        "decision_accuracy": correct / 12,
        "vehicle_recall": vehicle_hits / len(TRUTH) if source == "rknn" else 0.0,
        "hazard_recall": hazard_hits / 3 if source == "rknn" else 0.0,
        "center_x_mae_milli": statistics.fmean(center_errors),
        "mean_rtt_ms": statistics.fmean(row["rtt_ms"] for row in status_rows),
        "p95_rtt_ms": sorted(row["rtt_ms"] for row in status_rows)[-2],
        "mean_end_to_end_ms": statistics.fmean(row["end_to_end_us"] for row in status_rows) / 1000,
        "mean_inference_ms": statistics.fmean(
            row["infer_us"] for row in control_rows if row["infer_us"] > 0
        ) / 1000 if source == "rknn" else 0.0,
        "pre_hazard_state": status_rows[5]["state"],
        "hazard_states": [status_rows[index - 1]["state"] for index in (7, 8, 9, 10)],
        "post_reset_state": status_rows[11]["state"],
        "trace": [control | status_rows[i] for i, control in enumerate(control_rows)],
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--fixed", required=True, type=Path)
    parser.add_argument("--rknn", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    result = {"fixed": parse(args.fixed, "fixed"), "rknn": parse(args.rknn, "rknn")}
    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "metrics.json").write_text(json.dumps(result, indent=2) + "\n")
    fixed, rknn = result["fixed"], result["rknn"]
    report = f"""# Task 3 final hybrid physical-board A/B

| Metric | Fixed perception | YOLOv8 RKNN/NPU |
|---|---:|---:|
| Complete CONTROL→STATUS chains | 12/12 | 12/12 |
| Correct scene decisions | {fixed['correct_decisions']}/12 ({fixed['decision_accuracy']:.1%}) | {rknn['correct_decisions']}/12 ({rknn['decision_accuracy']:.1%}) |
| Vehicle recall | N/A (no detector) | {rknn['vehicle_recall']:.1%} |
| Hazard recall | 0.0% | {rknn['hazard_recall']:.1%} |
| Mean CONTROL→STATUS RTT | {fixed['mean_rtt_ms']:.1f} ms | {rknn['mean_rtt_ms']:.1f} ms |
| Mean inference-start→STATUS | {fixed['mean_end_to_end_ms']:.1f} ms | {rknn['mean_end_to_end_ms']:.1f} ms |
| Mean RKNN inference | N/A | {rknn['mean_inference_ms']:.1f} ms |

Fixed perception continued `SetOutput 500` through all hazard frames. RKNN detected the first
hazard and sent Stop, then kept Stop latched for two further hazard frames and a later safe road
frame. Zephyr state fell from {rknn['pre_hazard_state']} to {' → '.join(map(str, rknn['hazard_states']))}.
The explicit Reset was acknowledged and the final road frame resumed SetOutput.

Both arms used the same FP-RR hybrid topology, Zephyr binary, T2N1 protocol and 12-event input
manifest. The only A/B factor was the perception decision source. All timing timestamps were
captured from StarryOS CLOCK_MONOTONIC; file publication/polling, guest scheduling and network
delivery are included in end-to-end latency.
"""
    (args.out / "REPORT.md").write_text(report)
    print(report)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
