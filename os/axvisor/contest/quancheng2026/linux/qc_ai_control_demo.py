#!/usr/bin/env python3

import argparse
import math
import socket
import statistics
import time
from types import SimpleNamespace

from qc_reliable_udp_client import CONTROL_PAYLOAD, request_status, send_with_retry


HIDDEN_WEIGHTS = (
    (0.90, -0.35, 0.15),
    (-0.20, 0.80, 0.30),
    (0.45, 0.25, -0.55),
    (-0.60, 0.10, 0.75),
)
HIDDEN_BIAS = (0.05, -0.10, 0.00, 0.12)
OUTPUT_WEIGHTS = (0.70, -0.45, 0.55, 0.35)
OUTPUT_BIAS = -0.05


def relu(value: float) -> float:
    return value if value > 0.0 else 0.0


def sigmoid(value: float) -> float:
    return 1.0 / (1.0 + math.exp(-value))


def mlp_infer(error: float, velocity: float, load: float) -> float:
    features = (error, velocity, load)
    hidden = []
    for weights, bias in zip(HIDDEN_WEIGHTS, HIDDEN_BIAS):
        hidden.append(relu(sum(w * x for w, x in zip(weights, features)) + bias))
    return sigmoid(sum(w * x for w, x in zip(OUTPUT_WEIGHTS, hidden)) + OUTPUT_BIAS)


def sensor_sample(index: int) -> tuple[float, float, float]:
    phase = index / 4.0
    error = math.sin(phase) * 0.8
    velocity = math.cos(phase * 0.7) * 0.5
    load = 0.35 + 0.15 * math.sin(phase * 0.5)
    return error, velocity, load


def main() -> int:
    parser = argparse.ArgumentParser(description="AI inference to RTOS reliable UDP control demo.")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=14242)
    parser.add_argument("--count", type=int, default=20)
    parser.add_argument("--timeout", type=float, default=0.5)
    parser.add_argument("--interval", type=float, default=0.05)
    parser.add_argument("--retries", type=int, default=3)
    parser.add_argument("--manual-score", type=int, default=800)
    args = parser.parse_args()

    peer = (args.host, args.port)
    retry_args = SimpleNamespace(retries=args.retries)
    successes = 0
    failures = 0
    inference_ms = []
    e2e_ms = []
    control_error_ai = []
    control_error_manual = []

    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.settimeout(args.timeout)

        for seq in range(1, args.count + 1):
            error, velocity, load = sensor_sample(seq)
            infer_started = time.monotonic_ns()
            confidence = mlp_infer(error, velocity, load)
            infer_ms = (time.monotonic_ns() - infer_started) / 1_000_000

            setpoint_milli = int(1000 + error * 250)
            ai_score_milli = int(700 + confidence * 300)
            payload = CONTROL_PAYLOAD.pack(setpoint_milli, ai_score_milli, seq)

            ok, _attempts, latency_ms, _parsed = send_with_retry(
                sock, peer, seq, payload, retry_args
            )
            target_output = setpoint_milli
            ai_output = setpoint_milli * ai_score_milli / 1000.0
            manual_output = setpoint_milli * args.manual_score / 1000.0

            inference_ms.append(infer_ms)
            e2e_ms.append(latency_ms)
            control_error_ai.append(abs(target_output - ai_output))
            control_error_manual.append(abs(target_output - manual_output))

            print(
                f"ai seq={seq} error={error:.4f} velocity={velocity:.4f} load={load:.4f} "
                f"confidence={confidence:.4f} setpoint_milli={setpoint_milli} "
                f"ai_score_milli={ai_score_milli} infer_ms={infer_ms:.4f} "
                f"e2e_ms={latency_ms:.3f} result={'PASS' if ok else 'FAIL'}"
            )

            if ok:
                successes += 1
            else:
                failures += 1

            time.sleep(args.interval)

        status_ok = request_status(
            sock,
            peer,
            args.count + 2000,
            args.timeout,
            expected_last_seq=args.count,
        )

    print(
        f"ai_summary requested={args.count} successes={successes} failures={failures} "
        f"status_ok={int(status_ok)}"
    )
    if inference_ms and e2e_ms:
        print(
            f"ai_latency_ms infer_mean={statistics.fmean(inference_ms):.4f} "
            f"e2e_mean={statistics.fmean(e2e_ms):.3f} e2e_max={max(e2e_ms):.3f}"
        )
        print(
            f"control_error mean_ai={statistics.fmean(control_error_ai):.3f} "
            f"mean_manual={statistics.fmean(control_error_manual):.3f}"
        )

    return 0 if failures == 0 and successes == args.count and status_ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
