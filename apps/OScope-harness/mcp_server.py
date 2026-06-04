#!/usr/bin/env python3
import os
import subprocess
import sys
from pathlib import Path


def harness_kit_checkout() -> Path:
    prebuild = Path(__file__).resolve().with_name("prebuild.sh")
    result = subprocess.run(
        [str(prebuild)],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    )
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    if not lines:
        raise SystemExit(f"{prebuild} did not print a harness kit checkout path")
    return Path(lines[-1])


def main() -> None:
    target = harness_kit_checkout() / "tools/starry-syscall-harness/mcp_server.py"
    os.execv(sys.executable, [sys.executable, str(target), *sys.argv[1:]])


if __name__ == "__main__":
    main()
