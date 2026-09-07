#!/usr/bin/env python3
"""Exercise sampling, register reads and unwinding in real QEMU Linux user mode."""

import argparse
from pathlib import Path
import subprocess
import tempfile


def run(command):
    result = subprocess.run(command, capture_output=True, text=True, timeout=60)
    if result.returncode != 0:
        raise RuntimeError(f"{command} exited {result.returncode}:\n{result.stderr}")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("plugin", type=Path)
    parser.add_argument("analyzer", type=Path)
    args = parser.parse_args()
    plugin = args.plugin.resolve(strict=True)
    analyzer = args.analyzer.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="qperf-sampling-") as directory:
        root = Path(directory)
        workload = root / "workload"
        run([
            "cc", "-static", "-no-pie", "-O1", "-g", "-fno-omit-frame-pointer",
            "-fno-optimize-sibling-calls", "-pthread",
            str(Path(__file__).with_name("workload.c")), "-o", str(workload),
        ])
        for mode in ["tb", "insn"]:
            for callchain in ["leaf", "fp"]:
                raw = root / f"{mode}-{callchain}.bin"
                result = run([
                    "qemu-x86_64", "-tb-size", "1", "-plugin",
                    f"{plugin},out={raw},freq=1000,mode={mode},callchain={callchain}",
                    str(workload),
                ])
                if "QPERF_WORKLOAD_DONE" not in result.stdout:
                    raise RuntimeError("guest workload did not complete")
                summary = dict(
                    line.split(" = ", 1)
                    for line in raw.with_suffix(".summary.txt").read_text().splitlines()
                )
                if int(summary["samples"]) == 0 or int(summary["sample_failures"]) != 0:
                    raise RuntimeError(f"sampling failed: {summary}")
                if int(summary["dropped_samples"]) != 0:
                    raise RuntimeError(f"sampling lost records: {summary}")
                folded = raw.with_suffix(".folded")
                run([str(analyzer), "--elf", str(workload), str(raw), str(folded)])
                stacks = [line for line in folded.read_text().splitlines() if "workload_leaf" in line]
                if not stacks:
                    raise RuntimeError("no samples resolve to the actual guest workload")
                if callchain == "fp" and not any("workload_thread" in stack for stack in stacks):
                    raise RuntimeError("FP unwinding did not recover the workload caller")
                print(f"PASS: {mode}/{callchain}: {summary['samples']} samples, guest symbols resolved")


if __name__ == "__main__":
    main()
