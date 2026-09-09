#!/usr/bin/env python3
"""Build the POSIX binding generator using only files in its Cargo package.

This isolates build.rs from the kernel dependency graph so unpublished workspace
versions cannot mask missing generator inputs. Each feature profile compiles the
generated bindings and checks the C pthread mutex ABI used by that profile.
"""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--target", help="Rust target for generated binding ABI checks")
    args = parser.parse_args()
    root = args.workspace.resolve()
    source = root / "os/arceos/api/arceos_posix_api"
    files = subprocess.check_output(
        ["cargo", "package", "--list", "--allow-dirty", "-p", "ax-posix-api"],
        cwd=root, text=True,
    ).splitlines()
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=root, text=True,
    ))
    package = next(package for package in metadata["packages"] if package["name"] == "ax-posix-api")
    dependency = next(dependency for dependency in package["dependencies"] if dependency["name"] == "bindgen")
    with tempfile.TemporaryDirectory(prefix="posix-bindings-") as directory:
        fixture = Path(directory)
        for name in files:
            path = source / name
            if path.is_file():
                destination = fixture / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(path, destination)
        # Compile the real build script in isolation, with its actual dependency.
        (fixture / "Cargo.toml").write_text(
            '[package]\nname="posix-bindings-probe"\nversion="0.0.0"\nedition="2024"\n'
            '[features]\nsmp=[]\nlockdep=[]\n'
            '[build-dependencies.bindgen]\n'
            f'version={json.dumps(dependency["req"])}\n'
            f'default-features={str(dependency["uses_default_features"]).lower()}\n'
            f'features={json.dumps(dependency.get("features", []))}\n'
        )
        env = os.environ.copy()
        # Use the workspace toolchain even though the fixture is outside it.
        env["RUSTUP_TOOLCHAIN"] = subprocess.check_output(
            ["rustup", "show", "active-toolchain"], cwd=root, text=True,
        ).split()[0]
        for features, words in [("", 5), ("smp", 6), ("lockdep", 9), ("smp,lockdep", 10)]:
            (fixture / "src/lib.rs").write_text(
                '#![no_std]\n'
                'include!(concat!(env!("OUT_DIR"), "/ctypes_gen.rs"));\n'
                'const _: () = assert!(core::mem::size_of::<tm>() == 56);\n'
                'const _: () = assert!(core::mem::offset_of!(tm, __tm_gmtoff) == 40);\n'
                'const _: () = assert!(core::mem::offset_of!(__jmp_buf_tag, __fl) == core::mem::size_of::<__jmp_buf>());\n'
                '#[cfg(target_arch="x86_64")] const _: () = assert!(core::mem::size_of::<__jmp_buf>() == 8 * 8);\n'
                '#[cfg(target_arch="aarch64")] const _: () = assert!(core::mem::size_of::<__jmp_buf>() == 22 * 8);\n'
                '#[cfg(target_arch="riscv64")] const _: () = assert!(core::mem::size_of::<__jmp_buf>() == 26 * 8);\n'
                '#[cfg(target_arch="loongarch64")] const _: () = assert!(core::mem::size_of::<__jmp_buf>() == 21 * 8);\n'
                f'const _: () = assert!(core::mem::size_of::<pthread_mutex_t>() == {words} * core::mem::size_of::<core::ffi::c_long>());\n'
            )
            command = ["cargo", "check", "--manifest-path", str(fixture / "Cargo.toml")]
            if args.target:
                command += ["--target", args.target]
            if features:
                command += ["--features", features]
            subprocess.run(command, env=env, check=True)
            assert not (fixture / "src/ctypes_gen.rs").exists(), "bindings must stay in OUT_DIR"
            print(f"POSIX_BINDINGS_PASSED features={features or 'default'}", flush=True)


if __name__ == "__main__":
    main()
