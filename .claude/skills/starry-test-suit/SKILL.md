---
name: starry-test-suit
description: Add, adapt, regroup, or validate StarryOS test-suit cases in this repository. Use this skill whenever the user wants to create or modify `test-suit/starryos` cases, update `qemu-*.toml` runtime configs, move cases between `normal` and `stress`, tune `success_regex`/`fail_regex`/`shell_init_cmd`, or wire Starry test-suit changes into `cargo starry test qemu` and CI.
---

# Starry Test Suit

## Overview

This skill captures the repo-specific way to maintain StarryOS system tests. Starry test-suit is data-driven: cases live under `test-suit/starryos`, `cargo starry test qemu` builds `starryos` itself, and xtask discovers per-arch runtime configs from case directories.

## Workflow

1. Read the target case directory under `test-suit/starryos` and the current xtask flow in `scripts/axbuild/src/starry/test_suit.rs`.
2. Decide whether the case belongs to `normal` or `stress`.
3. Add or update only the `qemu-<arch>.toml` files for architectures that actually work.
4. Validate with `cargo xtask starry test qemu ...` on the affected architectures.
5. If the case changes repo expectations, update docs or CI in the same change.

## Layout Rules

- `normal` cases live at `test-suit/starryos/normal/<case>/qemu-<arch>.toml`.
- `stress` cases live at `test-suit/starryos/stress/<case>/qemu-<arch>.toml`.
- `cargo starry test qemu -t <arch>` only runs `normal`.
- `cargo starry test qemu --stress -t <arch>` only runs `stress`.
- `-c/--test-case` only searches inside the current group.
- Keep case directories one level below `normal/` or `stress/`; do not add extra nesting.
- Batch QEMU runs skip case directories that do not contain `qemu-<arch>.toml`; explicit `-c` still requires the directory and matching config to exist.
- Cases may optionally provide `c/CMakeLists.txt` and `c/prebuild.sh`; anything that must land in the guest rootfs should be installed via CMake `install()`, not left as a prebuild side effect.

## Case Content

Each `qemu-<arch>.toml` should define runtime behavior, not build config:

- `args`: arch-specific QEMU args
- `to_bin` / `uefi`
- `shell_prefix`
- `shell_init_cmd`
- `success_regex`
- `fail_regex`
- `timeout`

Prefer multi-line TOML strings for longer shell scripts. Keep `shell_init_cmd` self-contained; do not add extra host-side helper files unless there is a strong reason.

## Editing Rules

- Reuse the closest existing Starry case as a template, but update it to match the real behavior.
- Keep arch-specific QEMU boot args intact; only change the test script and matchers unless the platform really changed.
- Choose `success_regex` from a stable, unique success line.
- Keep `fail_regex` narrow. Avoid patterns that match benign output like `failed: 0`.
- Only include an architecture if it really passes. It is acceptable for one case to support fewer archs than another.
- For slow package-install cases, increase `timeout` only after confirming the command still makes progress.

## Validation

Use xtask commands, not raw cargo runs:

```bash
cargo xtask starry test qemu -t riscv64
cargo xtask starry test qemu -t aarch64 -c smoke
cargo xtask starry test qemu --stress -t riscv64 -c stress-ng-0
```

When changing logic or xtask behavior, also run:

```bash
cargo test -p axbuild
cargo clippy -p axbuild --all-targets --all-features
```

## Common Pitfalls

- Do not run multiple `cargo starry test qemu` commands in parallel in one workspace checkout; Starry build artifacts and generated config files can interfere with each other.
- `test-suit/starryos` is not a Cargo crate. Do not add `Cargo.toml` or `src/` back there.
- `normal/apk/qemu-x86_64.toml` is intentionally absent because the x86_64 apk path was not stable enough; treat that as a deliberate omission unless you have a verified fix.
- `stress` cases are allowed to be slower or flaky during bring-up; `normal` cases should be kept reliable.
- CI for PRs to `main` runs extra Starry stress coverage, so changes under `stress/` should be checked with `--stress` before landing.
