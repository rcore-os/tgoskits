#!/usr/bin/env python3
"""Fetch the pinned Chinese KWS model; keep weights outside the source tree."""

import argparse
import hashlib
import pathlib
import shutil
import subprocess

REPOSITORY = "https://modelscope.cn/models/pkufool/icefall-kws-zipformer-zh-en-3M-2025-12-20/resolve"
REVISION = "541d04e28be57efc6fdf46a341da09e043a37b52"
SUFFIX = "-epoch-13-avg-2-chunk-16-left-64"
FILES = [
    ("onnx/encoder" + SUFFIX + ".onnx", "encoder.onnx",
     "540ff509ed89bd22afe04bf7049a54bb1c95c6d8a18742ea9691910cdb5f859e"),
    ("onnx/decoder" + SUFFIX + ".onnx", "decoder.onnx",
     "63a22dd60f40fff082ac3e09afa507f6787da36df76ded2fbe145fa233e22c21"),
    ("onnx/joiner" + SUFFIX + ".onnx", "joiner.onnx",
     "76f7a24ed0c08633af14b2ee377f747af880d3b65eeba2cd3f31f3380fb73e8d"),
    ("data/lang_phone/tokens.txt", "tokens.txt",
     "2d3f32311f9b692b964da3c90e830258d3e78e013cb0c992dbfb15cd5a1a71b0"),
    ("README.md", "MODEL_CARD.md",
     "34d92bb4dc9fb259efb67f329d2cd68f6e0a6226121a694a3b6b4c748378559c"),
]


def matches(path, digest):
    return path.is_file() and hashlib.sha256(path.read_bytes()).hexdigest() == digest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=pathlib.Path)
    args = parser.parse_args()
    args.directory.mkdir(parents=True, exist_ok=True)
    for source, name, digest in FILES:
        path = args.directory / name
        if matches(path, digest):
            continue
        temporary = path.with_suffix(path.suffix + ".part")
        try:
            subprocess.run([
                "curl", "--fail", "--location", "--silent", "--show-error",
                "--connect-timeout", "15", "--max-time", "180",
                "--output", str(temporary), f"{REPOSITORY}/{REVISION}/{source}",
            ], check=True)
            if not matches(temporary, digest):
                raise ValueError(f"SHA256 mismatch: {source}")
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)
    shutil.copyfile(pathlib.Path(__file__).with_name("keywords.txt"),
                    args.directory / "keywords.txt")
    print(f"Model ready: {args.directory}")


if __name__ == "__main__":
    main()
