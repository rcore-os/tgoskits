#!/usr/bin/env python3
"""Restrict PGO flags to the five source-matched runtime crates."""

import hashlib
import json
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
OLD = ROOT.parent / "resume914-current-source-training"
PROFILE = OLD / "profile.profdata"
PROFILE_SHA256 = "445fd8229f286934bebd5f9eee53140fbc117738a04ef5d25b7150d4e15e7de8"
COUNTERS = ROOT / "profiles"
TRAINED = {"ax_task", "ax_sched", "ax_runtime", "starry_kernel", "ax_sync"}
GENERATE_FLAGS = {
    "-Zno-profiler-runtime",
    "-Cllvm-args=-disable-vp",
    "-Cllvm-args=-instrprof-atomic-counter-update-all",
}
USE_FLAGS = {"-Cllvm-args=-disable-vp", "-Cllvm-args=-pgo-warn-missing-function"}


def main():
    compiler, *args = sys.argv[1:]
    crate = args[args.index("--crate-name") + 1] if "--crate-name" in args else None
    target = args[args.index("--target") + 1] if "--target" in args else None
    mode = os.environ["ISSUE2308_PROFILE_MODE"]
    if mode == "generate":
        expected = f"-Cprofile-generate={COUNTERS}"
        flags = GENERATE_FLAGS
    elif mode == "use":
        expected = f"-Cprofile-use={PROFILE}"
        flags = USE_FLAGS
        if hashlib.sha256(PROFILE.read_bytes()).hexdigest() != PROFILE_SHA256:
            raise SystemExit("profile hash mismatch")
    else:
        raise SystemExit(f"unknown profile mode: {mode}")

    if any(item.startswith(("-Cprofile-generate=", "-Cprofile-use=")) and item != expected for item in args):
        raise SystemExit("unexpected profile path")
    profiled = crate in TRAINED and target is not None
    if not profiled:
        args = [item for item in args if item != expected and item not in flags]
    if crate in TRAINED or crate == "starryos":
        with (ROOT / f"{mode}-wrapper.jsonl").open("a") as handle:
            handle.write(json.dumps({"crate": crate, "target": target, "profiled": profiled}) + "\n")
    os.execvp(compiler, [compiler, *args])


if __name__ == "__main__":
    main()
