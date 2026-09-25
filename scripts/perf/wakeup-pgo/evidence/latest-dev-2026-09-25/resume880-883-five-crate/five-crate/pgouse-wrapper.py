#!/usr/bin/env python3
"""Apply the exact-source native profile only to five target crates."""

import hashlib
import json
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
PROFILE = ROOT / "profile.profdata"
PROFILE_SHA256 = "92fb238c9f0decd0ee1dcf16c4ef2dc64b846924c4f298d884cf1dad9b3b5fd2"
TRAINED = {"ax_task", "ax_sched", "ax_runtime", "starry_kernel", "ax_sync"}
PGO_FLAGS = {"-Cllvm-args=-disable-vp", "-Cllvm-args=-pgo-warn-missing-function"}


def main():
    compiler, *args = sys.argv[1:]
    crate = args[args.index("--crate-name") + 1] if "--crate-name" in args else None
    target = args[args.index("--target") + 1] if "--target" in args else None
    expected = f"-Cprofile-use={PROFILE}"
    if any(item.startswith("-Cprofile-use=") and item != expected for item in args):
        raise SystemExit("unexpected profile path")
    profiled = crate in TRAINED and target is not None
    if profiled:
        if hashlib.sha256(PROFILE.read_bytes()).hexdigest() != PROFILE_SHA256:
            raise SystemExit("native profile hash mismatch")
    else:
        args = [item for item in args if item != expected and item not in PGO_FLAGS]
    if crate in TRAINED or crate == "starryos":
        with (ROOT / "pgouse-wrapper.jsonl").open("a") as handle:
            handle.write(json.dumps({"crate": crate, "target": target, "profiled": profiled}) + "\n")
    os.execvp(compiler, [compiler, *args])


if __name__ == "__main__":
    main()
