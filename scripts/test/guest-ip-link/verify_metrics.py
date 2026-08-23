#!/usr/bin/env python3
"""Validate the observable success markers from a guest IP-link run."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("log", type=Path)
    args = parser.parse_args()
    text = args.log.read_text(encoding="utf-8")
    if "GIPC_STARRY_STATUS" not in text or "GIPC_STARRY_METRIC" not in text:
        print("missing GIPC success markers", file=sys.stderr)
        return 1
    if "GIPC_STARRY_TIMEOUT" in text or "GIPC_RTOS_ERROR" in text:
        print("guest IP-link reported a failure", file=sys.stderr)
        return 1
    match = re.search(r"GIPC_STARRY_METRIC .*?rtt_ns=(\d+) throughput_bps=(\d+)", text)
    if match is None or int(match.group(1)) <= 0 or int(match.group(2)) <= 0:
        print("missing positive latency/throughput metrics", file=sys.stderr)
        return 1
    print("GIPC_METRICS_OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
