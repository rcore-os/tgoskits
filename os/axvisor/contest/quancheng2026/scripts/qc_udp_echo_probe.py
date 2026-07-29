#!/usr/bin/env python3

import argparse
import socket
import statistics
import sys
import time


def main() -> int:
    parser = argparse.ArgumentParser(description="Probe a UDP echo endpoint.")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--count", type=int, default=20)
    parser.add_argument("--warmup", type=int, default=2)
    parser.add_argument("--timeout", type=float, default=1.0)
    parser.add_argument("--interval", type=float, default=0.05)
    parser.add_argument("--prefix", default="qc-zephyr")
    args = parser.parse_args()

    successes = 0
    failures = 0
    latencies_ms = []

    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.settimeout(args.timeout)

        for sequence in range(1, args.warmup + 1):
            payload = f"{args.prefix} warmup={sequence:04d}".encode("ascii")
            try:
                sock.sendto(payload, (args.host, args.port))
                echoed, peer = sock.recvfrom(65535)
                result = "PASS" if echoed == payload else "PAYLOAD_MISMATCH"
                print(
                    f"warmup={sequence} result={result} "
                    f"peer={peer[0]}:{peer[1]}"
                )
            except TimeoutError:
                print(f"warmup={sequence} result=TIMEOUT")
            time.sleep(args.interval)

        for sequence in range(1, args.count + 1):
            payload = (
                f"{args.prefix} seq={sequence:04d} sent_ns={time.monotonic_ns()}"
            ).encode("ascii")
            started_ns = time.monotonic_ns()

            try:
                sock.sendto(payload, (args.host, args.port))
                echoed, peer = sock.recvfrom(65535)
                latency_ms = (time.monotonic_ns() - started_ns) / 1_000_000

                if echoed != payload:
                    failures += 1
                    print(
                        f"seq={sequence} result=PAYLOAD_MISMATCH "
                        f"received={len(echoed)} expected={len(payload)}"
                    )
                else:
                    successes += 1
                    latencies_ms.append(latency_ms)
                    print(
                        f"seq={sequence} result=PASS peer={peer[0]}:{peer[1]} "
                        f"bytes={len(echoed)} latency_ms={latency_ms:.3f}"
                    )
            except TimeoutError:
                failures += 1
                print(f"seq={sequence} result=TIMEOUT")

            time.sleep(args.interval)

    print(
        f"summary requested={args.count} successes={successes} failures={failures}"
    )
    if latencies_ms:
        ordered = sorted(latencies_ms)
        p95_index = max(0, int(len(ordered) * 0.95 + 0.999999) - 1)
        print(
            f"latency_ms min={min(ordered):.3f} "
            f"mean={statistics.fmean(ordered):.3f} "
            f"p95={ordered[p95_index]:.3f} max={max(ordered):.3f}"
        )

    return 0 if successes == args.count and failures == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
