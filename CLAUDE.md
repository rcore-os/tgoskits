# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository Overview

TGOSKits is a monorepo workspace for OS and virtualization development, containing three systems that share a common component layer:

- **ArceOS** (`os/arceos/`) — modular unikernel kernel; provides HAL, scheduling, networking, filesystem, memory management
- **StarryOS** (`os/StarryOS/`) — Linux-compatible OS built on ArceOS modules; adds syscalls, process management, signals
- **Axvisor** (`os/axvisor/`) — type-1 hypervisor built on ArceOS; adds vCPU/VM management, guest image loading

Dependency flow: `components/` + `drivers/` → ArceOS modules → StarryOS / Axvisor

## Build System

All builds, runs, and tests go through `cargo xtask` (implemented in `scripts/axbuild/`).

### Common Commands

```bash
# ArceOS
cargo xtask arceos qemu --package arceos-helloworld --arch aarch64
cargo xtask arceos build --package arceos-helloworld --arch riscv64

# StarryOS (prepare rootfs once first)
cargo xtask starry rootfs --arch aarch64
cargo xtask starry qemu --arch aarch64

# Axvisor
cargo xtask axvisor qemu --arch aarch64

# Linting
cargo xtask clippy                        # all workspace crates, no-deps by default; prints timing
cargo xtask clippy --package <crate>      # specific crate
cargo xtask clippy --since <git-ref>      # only changed crates
cargo xtask clippy --all                  # all workspace crates
cargo xtask sync-lint --since <git-ref>   # check suspicious Relaxed atomic ordering

# Formatting
cargo fmt --all -- --check
cargo fmt --all

# Host std tests
cargo xtask test   # runs against scripts/test/std_crates.csv whitelist

# Board management
cargo xtask board ls
cargo xtask board connect -b <board-type>
```

Supported architectures: `aarch64`, `riscv64`, `x86_64`, `loongarch64`

### Cargo Aliases (`.cargo/config.toml`)

`cargo arceos`, `cargo starry`, `cargo axvisor`, `cargo board` are shortcuts for the xtask subcommands.

## Toolchain

- Rust nightly-2026-04-27, edition 2024, resolver v3 (see `rust-toolchain.toml`)
- Targets: `x86_64-unknown-none`, `riscv64gc-unknown-none-elf`, `aarch64-unknown-none-softfloat`, `loongarch64-unknown-none-softfloat`
- Container image: `ghcr.io/rcore-os/tgoskits-container:latest`

## Code Style

`rustfmt.toml` enforces: `group_imports = "StdExternalCrate"`, `imports_granularity = "Crate"`, `format_strings = true`, `use_field_init_shorthand = true`.

## Architecture

### Components (`components/`)

Reusable `#![no_std]` crates shared across all three OSes: `axplat` (platform trait framework), `axcpu`, `memory_addr`/`memory_set`, `page_table_multiarch`, `percpu`, `kspin`, `kernel_guard`, `axerrno`, `axio`, `crate_interface`, `starry-process`/`starry-signal`/`starry-vm`, virtualization primitives (`axvcpu`, `axvm`, `axvisor_api`, arch-specific vcpu/vgic).

### Drivers (`drivers/`)

Organized by device type: `blk/`, `net/`, `pci/`, `intc/`, `serial/`, `usb/`, `soc/`, `npu/`, `tpu/`.

**Four-layer cross-kernel model:**
1. **Driver Core** (`drivers/<type>/<crate>/src/`) — OS-independent, `#![no_std]`, registers and state machines only
2. **Capability Boundary** — small trait objects: `mmio_api::MmioOp`, `dma_api::DmaOp`
3. **OS Glue** (`platform/axplat-dyn/src/drivers/`) — FDT/PCI probe, MMIO mapping, IRQ registration
4. **Runtime** — blocking/poll/future wrappers

**rdif traits** (`drivers/interface/rdif-*`) define per-device-class interfaces. **rdrive** (`drivers/rdrive/`) is the dynamic driver management framework with priority-sorted two-phase probing and PID-aware device locking.

### Platform (`platform/`)

Board-level platform crates: `axplat-dyn` (dynamic dispatch), `somehal`, `riscv64-visionfive2`, and `ax-plat-x86-pc`; RISC-V QEMU and Axvisor x86_64 use the dynamic platform path.

## CI

- **fmt**: `cargo fmt --all -- --check`
- **clippy**: `cargo xtask clippy --since <base>` (incremental for PRs; prints start, finish, and elapsed time)
- **sync-lint**: `cargo xtask sync-lint --since <base>`
- **std tests**: `cargo xtask test`
- **QEMU tests**: ArceOS/StarryOS/Axvisor across all 4 architectures
- **Board tests**: self-hosted runners for OrangePi-5-Plus, RDK-S100
- Std test crate list: `scripts/test/std_crates.csv`

## Conventions

- PR titles: Conventional Commits `type(scope): content`, e.g. `feat(axbuild): add board test flow`, `fix(starry-process): correct tty cleanup`
- PR titles in English, bodies in Chinese
- For any PR review, fully read (完整阅读) `.claude/skills/review-single-pr/SKILL.md` first; `AGENTS.md` and that skill are the review source of truth.
- Before deciding merge readiness, create a todo/checklist from the full `review-single-pr` requirements and verify each applicable item as satisfied, not applicable with reason, or blocking with evidence.
- Do not silence clippy warnings with `allow`; fix the root cause
- Do not add agent/AI branding or signatures to commits/PRs
- Read and strictly follow all conventions in AGENTS.md
