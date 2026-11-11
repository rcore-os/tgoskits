#!/usr/bin/env python3
"""Report build progress without leaving a separate sleep process behind."""

from pathlib import Path
import sys
import time


def emit(log, marker, phase, started, base):
    crates = []
    for line in log.read_text(errors="replace").splitlines():
        fields = line.split()
        if len(fields) >= 2 and fields[0] == "Compiling":
            crates.append(fields[1])
    total = len(crates)
    print(
        f"==={marker}-PROGRESS phase={phase} elapsed={int(time.time()) - started} "
        f"phase_compile_units={total - base} total_compile_units={total} "
        f"distinct_crates={len(set(crates))}===",
        flush=True,
    )


if __name__ == "__main__":
    mode, log, marker, phase, started, base = sys.argv[1:]
    arguments = Path(log), marker, phase, int(started), int(base)
    if mode == "once":
        emit(*arguments)
    elif mode == "watch":
        while True:
            time.sleep(60)
            emit(*arguments)
    else:
        raise SystemExit("expected once or watch")
