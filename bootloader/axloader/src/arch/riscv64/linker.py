#!/usr/bin/env python3
"""Adapt Rust's mandatory UEFI linker flag to the embedded-PE ELF link."""

import os
from pathlib import Path
import shlex
import subprocess
import sys


def expand_response_files(arguments):
    for argument in arguments:
        if argument.startswith("@"):
            yield from expand_response_files(
                shlex.split(Path(argument[1:]).read_text(encoding="utf-8"))
            )
        else:
            yield argument


def main():
    query_environment = os.environ.copy()
    for variable in ("CARGO_MAKEFLAGS", "MAKEFLAGS", "MFLAGS"):
        query_environment.pop(variable, None)
    library = Path(
        subprocess.check_output(
            ["rustc", "--print", "target-libdir"], text=True, env=query_environment
        ).strip()
    )
    linker = library.parent / "bin" / "rust-lld"
    # os=uefi requires is-like-windows in the pinned compiler. Its GNU linker
    # driver unconditionally adds this COFF-only flag, although this target
    # links ELF and supplies PE section permissions in its embedded header.
    arguments = [
        argument
        for argument in expand_response_files(sys.argv[1:])
        if argument != "--nxcompat"
    ]
    os.execv(linker, [str(linker), *arguments])


if __name__ == "__main__":
    main()
