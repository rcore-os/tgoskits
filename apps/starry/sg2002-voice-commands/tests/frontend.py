#!/usr/bin/env python3
"""Check real ORT/pipe streaming equivalence, failure propagation and child reaping."""

import argparse
import os
from pathlib import Path
import shlex
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("ort_include", type=Path)
    parser.add_argument("host_ort_library", type=Path)
    parser.add_argument("reference_python", type=Path)
    parser.add_argument("model_work_directory", type=Path)
    args = parser.parse_args()
    app = Path(__file__).resolve().parents[1]
    library = args.host_ort_library.resolve()
    models = args.model_work_directory.resolve()
    compiler = os.environ.get("CXX", "c++")
    with tempfile.TemporaryDirectory(prefix="voice-frontend-test-") as temporary:
        work = Path(temporary)
        flags = ["-O2", "-std=c++17", "-Wall", "-Wextra", "-Werror",
                 "-I" + str(args.ort_include.resolve())]
        subprocess.run([compiler, *flags, "-shared", "-fPIC", "-Wl,--no-undefined",
                        str(app / "frontend-op.cc"), "-o", str(work / "frontend.so")], check=True)
        subprocess.run([compiler, *flags, str(app / "tests/frontend.cc"), str(library),
                        "-Wl,-rpath," + str(library.parent), "-o", str(work / "check")], check=True)
        worker = work / "reference-worker"
        worker.write_text("#!/bin/sh\nexec " + shlex.join([
            str(args.reference_python.absolute()), str(app / "tests/frontend-reference.py")
        ]) + ' "$@"\n')
        worker.chmod(0o755)
        subprocess.run([str(work / "check"), str(models / "frontend.onnx"),
                        str(models / "frontend-bridge.onnx"), str(work / "frontend.so"),
                        str(worker)], check=True, timeout=60)


if __name__ == "__main__":
    main()
