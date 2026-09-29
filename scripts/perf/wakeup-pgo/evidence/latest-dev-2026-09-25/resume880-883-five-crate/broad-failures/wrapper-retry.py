#!/usr/bin/env python3
"""Keep PGO scope identical across training and use, excluding root exporter."""

import hashlib
import json
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
PROFILE_DIR = ROOT / "profiles-retry"
PROFILE = ROOT / "broad-retry.profdata"
UNPROFILED = {
    "starryos", "core", "alloc", "compiler_builtins", "profiler_builtins",
    "someboot", "somehal", "some_serial", "ax_hal", "ax_cpu", "ax_percpu",
    "ax_plat", "axplat_dyn", "ax_ctor_bare", "aarch64_cpu", "aarch64_cpu_ext",
    "ax_memory_addr",
}
PGO_FLAGS = {
    "-Zno-profiler-runtime",
    "-Cllvm-args=-disable-vp",
    "-Cllvm-args=-instrprof-atomic-counter-update-all",
    "-Cllvm-args=-pgo-warn-missing-function",
}


def main():
    compiler, *args = sys.argv[1:]
    crate = args[args.index("--crate-name") + 1] if "--crate-name" in args else None
    target = args[args.index("--target") + 1] if "--target" in args else None
    generate = f"-Cprofile-generate={PROFILE_DIR}"
    use = f"-Cprofile-use={PROFILE}"
    profile_args = [item for item in args if item.startswith(("-Cprofile-generate=", "-Cprofile-use="))]
    if target is None and not profile_args:
        os.execvp(compiler, [compiler, *args])
    if len(profile_args) != 1 or profile_args[0] not in (generate, use):
        raise SystemExit(f"resume879: unexpected profile arguments: {profile_args}")
    mode = "generate" if profile_args[0] == generate else "use"
    profiled = target is not None and crate not in UNPROFILED
    if profiled and mode == "use":
        expected = (ROOT / "profile-retry.sha256").read_text().split()[0]
        actual = hashlib.sha256(PROFILE.read_bytes()).hexdigest()
        if actual != expected:
            raise SystemExit("resume879: native profile hash mismatch")
    if not profiled:
        args = [item for item in args if item not in PGO_FLAGS and item not in (generate, use)]
    if target is not None:
        with (ROOT / "wrapper-retry.jsonl").open("a") as output:
            output.write(json.dumps({"mode": mode, "crate": crate, "target": target, "profiled": profiled}) + "\n")
    os.execvp(compiler, [compiler, *args])


if __name__ == "__main__":
    main()
