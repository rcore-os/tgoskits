#!/usr/bin/env python3
"""Check the real plugin ABI and orderly shutdown without booting a guest."""

import argparse
import json
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("plugin", type=Path)
    parser.add_argument("--qemu", default="qemu-system-x86_64")
    args = parser.parse_args()
    plugin = args.plugin.resolve(strict=True)
    version = subprocess.check_output([args.qemu, "--version"], text=True)
    if version.splitlines()[0] != "QEMU emulator version 11.1.1":
        raise RuntimeError(f"expected QEMU 11.1.1, got {version.splitlines()[0]}")

    with tempfile.TemporaryDirectory(prefix="qperf-load-") as directory:
        for mode, callchain in [("tb", "leaf"), ("insn", "fp")]:
            raw = Path(directory) / f"{mode}-{callchain}.bin"
            command = [
                args.qemu, "-machine", "q35", "-accel", "tcg", "-S",
                "-nodefaults", "-display", "none", "-monitor", "none",
                "-qmp", "stdio", "-plugin",
                f"{plugin},out={raw},mode={mode},callchain={callchain}",
            ]
            requests = "".join(json.dumps(request) + "\n" for request in [
                {"execute": "qmp_capabilities", "id": "capabilities"},
                {"execute": "quit", "id": "quit"},
            ])
            result = subprocess.run(
                command, input=requests, capture_output=True, text=True, timeout=30
            )
            if result.returncode != 0:
                raise RuntimeError(f"QEMU exited {result.returncode}:\n{result.stderr}")
            replies = [json.loads(line) for line in result.stdout.splitlines()]
            if not any(reply.get("id") == "quit" and "return" in reply for reply in replies):
                raise RuntimeError(f"QMP quit was not acknowledged: {result.stdout}")
            summary = dict(
                line.split(" = ", 1)
                for line in raw.with_suffix(".summary.txt").read_text().splitlines()
            )
            if not raw.is_file() or summary["sample_failures"] != "0":
                raise RuntimeError(f"plugin output/shutdown failed: {summary}")
            if summary["callchain_method"] != callchain:
                raise RuntimeError(f"plugin arguments were not applied: {summary}")
            print(f"PASS: QEMU 11.1.1 loads and closes qperf ({mode}/{callchain})")

        for argument in ["freq=0", "mode=invalid", "callchain=invalid", "max_depth=0"]:
            raw = Path(directory) / "rejected.bin"
            result = subprocess.run([
                args.qemu, "-machine", "q35", "-accel", "tcg", "-S",
                "-nodefaults", "-display", "none", "-monitor", "none",
                "-plugin", f"{plugin},out={raw},{argument}",
            ], capture_output=True, text=True, timeout=30)
            if result.returncode <= 0 or "qperf install failed:" not in result.stderr:
                raise RuntimeError(f"invalid argument was not cleanly rejected: {result.stderr}")
            if raw.exists():
                raise RuntimeError("invalid arguments created a sample file")
            print(f"PASS: rejects {argument} before publishing callbacks")


if __name__ == "__main__":
    main()
