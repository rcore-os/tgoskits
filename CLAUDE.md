# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

TGOSKits is a monorepo for OS and virtualization development. It uses git-subtree to consolidate 60+ standalone component repositories into a single Cargo workspace, containing three systems — ArceOS (unikernel), StarryOS (teaching OS), and Axvisor (hypervisor) — plus shared platform crates, drivers, and test suites.

## Build System

Everything goes through `cargo xtask`. The root xtask (`tg-xtask`) delegates to `axbuild`. Never use raw `cargo build`/`cargo run`/`cargo test`; prefer the xtask wrappers.

```bash
# Run an app in QEMU
cargo xtask arceos qemu --package ax-helloworld --arch aarch64
cargo xtask starry qemu --arch aarch64
cargo xtask axvisor qemu --arch aarch64

# Prepare StarryOS rootfs (needed once before first run)
cargo xtask starry rootfs --arch aarch64

# Run regression tests
cargo xtask arceos test qemu --target aarch64
cargo xtask starry test qemu --target aarch64
cargo xtask test                      # full regression

# Run clippy on a specific crate
cargo xtask clippy --package <crate-name>
```

## Toolchain

Rust nightly-2026-04-27, edition 2024. Required targets:
- `x86_64-unknown-none`
- `riscv64gc-unknown-none-elf`
- `aarch64-unknown-none-softfloat`
- `loongarch64-unknown-none-softfloat`

`rustfmt.toml` uses `style_edition = "2024"` with `group_imports = "StdExternalCrate"` and `imports_granularity = "Crate"`.

## Repository Layout

```
tgoskits/
├── components/          # 60+ standalone crates (git subtree from individual repos)
├── os/
│   ├── arceos/          # ArceOS unikernel: modules/ api/ ulib/ examples/
│   ├── StarryOS/        # StarryOS: kernel/ starryos/ configs/ make/
│   └── axvisor/         # Axvisor hypervisor: src/ configs/
├── platform/            # Platform crates (axplat-dyn, riscv64-qemu-virt, x86-qemu-q35)
├── drivers/             # Cross-kernel drivers (blk/, net/, pci/, npu/, soc/)
├── test-suit/           # System tests: arceos/, starryos/
├── xtask/               # Root build orchestrator (tg-xtask → axbuild)
├── scripts/
│   ├── repo/            # Git subtree management (repo.py, repos.csv)
│   └── test/            # clippy_crates.csv, std_crates.csv
└── docs/                # Docusaurus developer documentation
```

## Key Architecture Patterns

**Platform abstraction**: `components/axplat_crates/` defines per-architecture platform crates (`axplat-aarch64-qemu-virt`, `axplat-x86-pc`, etc.). Each implements traits and HAL primitives for its target. `platform/axplat-dyn` provides dynamic platform dispatch for Axvisor.

**Driver model**: Drivers under `drivers/` are organized by device type (blk, net, pci, npu, soc). They separate Driver Core from OS Glue and Runtime layers. Use `mmio-api` for MMIO/iomap, `dma-api` for DMA — never raw pointer casts.

**crate_interface pattern**: `components/crate_interface` provides a `#[crate_interface]` macro for loosely-coupled cross-crate trait dispatch without linking dependencies. Modules define traits, impls register at compile time, and callers dispatch through the interface — this is the standard way to break circular dependencies in this codebase.

**ArceOS module graph**: `os/arceos/modules/` contains ~15 crates (axhal, axmm, axtask, axruntime, axdriver, axfs, axnet, etc.). They form a layered unikernel where each module is conditionally compiled via feature flags from the top-level app crate.

**Git subtree workflow**: Components live in `components/` but are managed as subtrees from standalone repos tracked in `scripts/repo/repos.csv`. Use `python3 scripts/repo/repo.py pull/push` to sync with upstreams. Do not modify subtree history manually.

## Branch Strategy

```
feature/* ──PR──► dev ──regular merge──► main
```

- `main`: stable, no direct push
- `dev`: integration branch, CI validation
- Feature branches off `dev`, PR back to `dev`

## Code Quality Requirements

- Run `cargo fmt` after every change
- Run `cargo xtask clippy --package <crate>` for the affected crate after logic changes
- If a crate passes clippy but is missing from `scripts/test/clippy_crates.csv`, add it
- Do not silence clippy with `#[allow]` unless the user explicitly asks; fix the root cause
- PR titles: `type(scope): content` (Conventional Commits), e.g. `feat(axbuild): add Starry remote board test flow`

## Project-Local Skills

Available in `.claude/skills/`:
- `update-std-tests` — audit/update `scripts/test/std_crates.csv`
- `starry-test-suit` — manage StarryOS test cases and configs
- `cross-kernel-driver` — portable driver development patterns
- `review-open-prs` — PR audit and review workflow
- `board-uboot-fsck-repair` — physical board recovery workflow
- `arceos-test-adapter` — adapt ArceOS tests for xtask

## Debugging

VS Code launch configs in `.vscode/launch.json`. Requires CodeLLDB extension and `qemu-system-aarch64` on PATH. Each system provides **Main** (stops at app entry) and **Boot** (stops at platform boot) debug targets.
