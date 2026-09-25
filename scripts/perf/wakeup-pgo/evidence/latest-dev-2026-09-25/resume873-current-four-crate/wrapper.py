#!/usr/bin/env python3
"""Instrument four current-source crates for native PGO training."""
import json
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
PROFILE_DIR = ROOT / "profiles"
INSTRUMENTED = {"ax_task", "ax_sched", "ax_runtime", "starry_kernel"}
PGO_FLAGS = {
    "-Zno-profiler-runtime",
    "-Cllvm-args=-disable-vp",
    "-Cllvm-args=-instrprof-atomic-counter-update-all",
}


def main():
    compiler, *args = sys.argv[1:]
    crate = args[args.index("--crate-name") + 1] if "--crate-name" in args else None
    target = args[args.index("--target") + 1] if "--target" in args else None
    for item in args:
        if item.startswith("-Cprofile-generate=") and Path(item.split("=", 1)[1]) != PROFILE_DIR:
            raise SystemExit(f"resume873: unexpected profile directory: {item}")
    instrumented = crate in INSTRUMENTED and target is not None
    if not instrumented:
        args = [item for item in args if not item.startswith("-Cprofile-generate=") and item not in PGO_FLAGS]
    if crate in INSTRUMENTED or crate == "starryos":
        with (ROOT / "wrapper.jsonl").open("a") as handle:
            handle.write(json.dumps({"crate": crate, "target": target, "instrumented": instrumented}) + "\n")
    os.execvp(compiler, [compiler, *args])


if __name__ == "__main__":
    main()
