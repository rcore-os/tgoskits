---
name: starry-test-suit
description: Add, regroup, adapt, or validate StarryOS test-suit cases in this repository. Use this skill when working under `test-suit/starryos`, updating Starry `qemu-*.toml` or `board-*.toml`, changing normal/stress grouping, tuning success/fail regexes, adding C/shell/Python/grouped case assets, or touching Starry test-suit CI/xtask behavior.
---

# Starry Test Suit

## Overview

StarryOS tests are data-driven. Cases live under `test-suit/starryos`, while discovery and execution are implemented mainly in:

- `scripts/axbuild/src/starry/test.rs`
- `scripts/axbuild/src/test/qemu.rs`
- `scripts/axbuild/src/test/case.rs`
- `scripts/axbuild/src/test/build.rs`

QEMU cases build the `starryos` package and run a per-arch `qemu-<arch>.toml`. Board cases build StarryOS for a board target and run a `board-<board>.toml` through the board runner.

## Workflow

1. Inspect the target directory under `test-suit/starryos` and the current Starry test flow in `scripts/axbuild/src/starry/test.rs`.
2. Decide whether the case is QEMU or board, and whether it belongs in `normal` or `stress`.
3. For QEMU, choose the build group (`qemu-smp1`, `qemu-smp4`, or another existing group) and add only the `qemu-<arch>.toml` files for architectures that actually pass.
4. If the case needs guest assets, use exactly one pipeline: `c/`, `sh/`, `python/`, or grouped `test_commands` with subcase directories.
5. For board tests, add `board-<board>.toml` under `<build_group>/<case>/` and ensure the mapped board config exists under `os/StarryOS/configs/board/`.
6. Validate with the matching `cargo xtask starry test ...` command.
7. If discovery rules, CI expectations, or directory conventions change, update `test-suit/starryos/GUIDE.md` and relevant docs in the same change.

## Layout Rules

- QEMU cases live at `test-suit/starryos/{normal,stress}/<build_group>/<case>/qemu-<arch>.toml`.
- Board cases live at `test-suit/starryos/<group>/<build_group>/<case>/board-<board>.toml`.
- Build configs live at `test-suit/starryos/<group>/<build_group>/build-<target>.toml`; `build-<arch>.toml` is also recognized when present.
- QEMU discovery first selects build groups with a build config matching the requested arch/target, then discovers cases with the matching `qemu-<arch>.toml`.
- Board discovery scans for `board-*.toml`; the build config defaults to `os/StarryOS/configs/board/<board>.toml` and may be overridden by a matching test-suit build config in the build group.
- Batch QEMU runs skip case directories without the requested `qemu-<arch>.toml`; explicit `-c/--test-case` requires the case and config to exist in a matching build group.
- `--stress` is equivalent to `--test-group stress`; do not combine it with `--test-group normal`.

## QEMU Asset Pipelines

Each QEMU case may use at most one asset pipeline:

- `plain`: no extra asset directory and no `test_commands`; boots the shared rootfs with QEMU `-snapshot`.
- `c`: case directory has `c/CMakeLists.txt`; CMake builds and installs artifacts into a rootfs overlay.
- `sh`: case directory has `sh/`; scripts are copied into the guest overlay.
- `python`: case directory has `python/`; the runner installs `python3` in staging and copies `.py` files into `/usr/bin/`.
- `grouped`: `qemu-<arch>.toml` defines `test_commands`; subdirectories such as `<subcase>/c/` are built and a `/usr/bin/starry-run-case-tests` runner is injected.

Pipeline cases use per-case rootfs copies and cache injected images under `target/<target>/qemu-cases/.../cache/rootfs/`. Plain cases do not copy the rootfs.

## Case Content

Each `qemu-<arch>.toml` should define runtime behavior, not build config:

- `args`: arch-specific QEMU args
- `to_bin` / `uefi`
- `shell_prefix`
- `shell_init_cmd` for plain, C, shell, or Python cases
- `test_commands` for grouped cases; do not combine with `shell_init_cmd`
- `success_regex`
- `fail_regex`
- `timeout`

Prefer multi-line TOML strings for longer shell commands. Keep `fail_regex` narrow and choose stable, unique success markers.

## Editing Rules

- Reuse the closest existing case as a template.
- Keep arch-specific boot args intact unless the platform requirement really changes.
- Add architecture configs only after validating that architecture.
- Do not define more than one pipeline in the same case directory.
- For C cases, install outputs through CMake `install()` so they land in the guest overlay.
- Use `prebuild.sh` only for packages or setup that must happen inside the staging rootfs.
- For grouped cases, keep `test_commands` aligned with installed guest paths and include the grouped success/fail regexes.
- Board case names and board config names should match the actual board target, such as `board-orangepi-5-plus.toml`.

## Validation

Use xtask commands:

```bash
cargo xtask starry test qemu --arch riscv64
cargo xtask starry test qemu --arch aarch64 -c smoke
cargo xtask starry test qemu --stress --arch riscv64
cargo xtask starry test board --board orangepi-5-plus
```

When changing Rust logic under `scripts/axbuild`, also run targeted formatting and clippy according to the repository rules:

```bash
cargo fmt
cargo xtask clippy --package axbuild
```

## Common Pitfalls

- Do not run multiple `cargo xtask starry test qemu` commands in parallel in one workspace checkout.
- `test-suit/starryos` is not a Cargo crate. Do not add `Cargo.toml` or `src/` there.
- Do not rely on build group names to distinguish QEMU from board; QEMU is discovered by `qemu-<arch>.toml`, board by `board-<board>.toml`.
- `shell_init_cmd` and `test_commands` are mutually exclusive.
- `stress` cases may be slow or heavy; `normal` cases should stay reliable for regular CI.
- If a case needs SMP, use an appropriate build group/config such as `qemu-smp4` instead of only adding QEMU `-smp`.
