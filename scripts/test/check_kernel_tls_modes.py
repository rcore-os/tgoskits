#!/usr/bin/env python3
"""Verify the compiled TLS mode and feature propagation through ax-cpu.

Cargo's resolved features and build-script output must agree: adding uspace to
an existing TLS build must stop selecting kernel TLS in both ax-cpu and cpu-local.
The same target directory is reused to exercise feature transitions.
"""

from pathlib import Path
import subprocess


def main():
    root = Path(__file__).resolve().parents[2]
    # Start with the formerly rejected combination, then toggle the TLS mode.
    for features, expected in [("tls,uspace", False), ("tls", True),
                               ("uspace", False), ("", False), ("tls", True)]:
        args = ["--locked", "-p", "ax-cpu", "--lib", "--no-default-features"]
        if features:
            args += ["--features", features]
        subprocess.run(["cargo", "check", *args], cwd=root, check=True)
        tree_args = [arg for arg in args if arg != "--lib"]
        tree = subprocess.check_output(
            ["cargo", "tree", *tree_args, "--prefix", "none", "--format", "{p}|{f}"],
            cwd=root, text=True,
        )
        dependency = next(line for line in tree.splitlines() if line.startswith("cpu-local "))
        resolved = set(dependency.split("|", 1)[1].removesuffix(" (*)").split(","))
        assert set(filter(None, features.split(","))) <= resolved, dependency
        for package in ["ax-cpu", "cpu-local"]:
            # Inspect each package's build-script output for the same image mode.
            command = ["cargo", "rustc", "--locked", "-p", package, "--lib",
                       "--no-default-features"]
            if features:
                command += ["--features", features]
            cfg = subprocess.check_output(command + ["--", "--print", "cfg"],
                                          cwd=root, text=True)
            assert ("kernel_tls" in cfg.splitlines()) == expected, (package, features, cfg)
        print(f"KERNEL_TLS_MODE_PASSED features={features or 'none'}", flush=True)


if __name__ == "__main__":
    main()
