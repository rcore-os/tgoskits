#!/usr/bin/env python3
"""Run TPU-MLIR 1.30.2 with Ubuntu 24.04's coherent host glibc."""

import importlib.metadata
import os
from pathlib import Path
import runpy
import sys

import tpu_mlir

if importlib.metadata.version("tpu-mlir") != "1.30.2":
    raise SystemExit("This conversion is verified with tpu-mlir==1.30.2")
if len(sys.argv) < 2 or sys.argv[1] not in ("model_transform.py", "model_deploy.py"):
    raise SystemExit("Usage: tpu-tool.py {model_transform.py|model_deploy.py} [arguments]")

# Import initializes the wheel's environment. Prefer host glibc to the bundled
# older copy, and the real ELF tools to pip's wrappers that reset this environment.
root = Path(tpu_mlir.__file__).parent
os.environ["LD_LIBRARY_PATH"] = "/usr/lib/x86_64-linux-gnu:" + os.environ["LD_LIBRARY_PATH"]
os.environ["PATH"] = ":".join((str(root / "bin"), str(root / "python/tools"),
                               str(Path(sys.executable).parent), os.environ["PATH"]))
tool = root / "python/tools" / sys.argv[1]
sys.argv = [str(tool), *sys.argv[2:]]
runpy.run_path(str(tool), run_name="__main__")
