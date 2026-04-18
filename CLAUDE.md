# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository Overview

TGOSKits is an integrated OS/virtualization development repository managing 60+ independent component repos via Git Subtree. It unifies ArceOS, StarryOS, Axvisor, and related platform crates in a single workspace with a unified `cargo xtask` build system.

**Branch strategy**: `main` (stable, no direct push) ← `dev` (integration, PR-only) ← `feature/*`. Always target `dev` for PRs.

## Build System

All commands run from the repository root. Aliases exist in `.cargo/config.toml`.

### Unified Commands

```bash
# All go through: cargo run -p tg-xtask -- <args>
cargo xtask <os|test|clippy|board> [subcommand] [options]

# Aliases (equivalent to cargo xtask <name> ...)
cargo arceos ...
cargo starry ...
cargo axvisor ...
cargo board ...
```

### Quick Run

```bash
# ArceOS — fastest path
cargo arceos qemu --package ax-helloworld --arch riscv64

# StarryOS — prepare rootfs first
cargo starry rootfs --arch riscv64
cargo starry qemu --arch riscv64

# Axvisor — needs setup first
cd os/axvisor && ./scripts/setup_qemu.sh arceos
cargo axvisor qemu --arch aarch64
```

### Testing

```bash
# Host std tests (white-listed packages in scripts/test/std_crates.csv)
cargo xtask test

# ArceOS QEMU test suite
cargo arceos test qemu --target riscv64          # all
cargo arceos test qemu --target riscv64 --only-rust
cargo arceos test qemu --target riscv64 --only-c

# StarryOS QEMU tests
cargo starry test qemu --target riscv64
cargo starry test qemu --stress -t riscv64       # stress tests

# Clippy (workspace-wide, uses scripts/test/clippy_crates.csv)
cargo xtask clippy

# Formatting check
cargo fmt --all -- --check
```

### Common Parameters

| Parameter | Short | Description |
|-----------|-------|-------------|
| `--package <pkg>` | `-p` | Application package name |
| `--arch <arch>` | | Architecture alias (aarch64, riscv64, x86_64, loongarch64) |
| `--target <tgt>` | `-t` | Full target triple |
| `--config <path>` | `-c` | Build info TOML path |
| `--plat-dyn` | | Enable dynamic platform linking |

## Architecture

### Directory Layout

- `components/` — Subtree-managed reusable crates (algorithms, sync, containers, address space, device abstractions)
- `os/arceos/` — ArceOS: `modules/` (kernel modules), `api/` (feature selection), `ulib/` (user libraries), `examples/`
- `os/StarryOS/` — StarryOS: `kernel/` (Linux-compatible layer), `starryos/` (user apps), `make/` (build helpers)
- `os/axvisor/` — Axvisor: `src/` (hypervisor), `configs/` (VM configs), local xtask
- `platform/` — Platform-specific crates (arch, HAL, peripherals)
- `test-suit/` — System-level test suites (ArceOS Rust/C tests, StarryOS tests)
- `xtask/` — `tg-xtask` entry point (thin wrapper → `axbuild`)
- `scripts/axbuild/` — Build system core: CLI parsing, command dispatch, test/clippy frameworks
- `scripts/repo/` — Subtree management (`repo.py`)
- `docs/` — Developer documentation (quick-start, build-system, arceos/starryos/axvisor guides, internals)

### Build System Call Chain

```
cargo xtask → tg-xtask (main.rs, tokio async)
  → axbuild::run() (scripts/axbuild/src/lib.rs)
    → Cli::parse → Commands enum
      → arceos::execute() | starry::execute() | axvisor::execute()
        → command_flow::resolve_request() (arch/target/snapshot resolution)
        → command_flow::run_build/qemu/uboot() (ostool Tool API)
```

### Key Source Modules (scripts/axbuild/)

| Module | Responsibility |
|--------|---------------|
| `lib.rs` | CLI definition (`Cli`/`Commands`), dispatch |
| `arceos/mod.rs` | ArceOS build/qemu/test/uboot |
| `starry/mod.rs` | StarryOS build/qemu/test/rootfs/uboot |
| `axvisor/mod.rs` | Axvisor build/qemu/board/test/image/config |
| `command_flow.rs` | Unified build/qemu/uboot execution, snapshot persistence |
| `context/` | Arch/target mapping, snapshot types |
| `test_std.rs` | CSV whitelist → `cargo test -p <pkg>` |
| `clippy.rs` | Package × feature × target clippy matrix |
| `test_qemu.rs` | QEMU test package lists, target parsing |

### Layered Design

```
Applications (examples/, test-suit/)
  → User Libraries (ulib/: ax-std, ax-libc)
    → API Layer (api/: feature selection, stable API, POSIX compat)
      → Kernel Modules (modules/: ax-hal, ax-runtime, ax-task, ax-mm, ax-driver, ...)
        → Components (components/: reusable crates)
          → Platform (platform/: arch-specific HAL, peripherals)
```

## Rust Toolchain

- **Channel**: nightly-2026-04-01 (profile: minimal)
- **Components**: rust-src, llvm-tools, rustfmt, clippy
- **Bare-metal targets**: x86_64-unknown-none, riscv64gc-unknown-none-elf, aarch64-unknown-none-softfloat, loongarch64-unknown-none-softfloat
- **Recommended tools**: `cargo install cargo-binutils ostool`

## Testing Details

- **Std tests**: Whitelist in `scripts/test/std_crates.csv` (one package per line, CSV with `package` header). Run via `cargo xtask test`.
- **Clippy**: Whitelist in `scripts/test/clippy_crates.csv`. Expands to (package × feature × target) matrix.
- **ArceOS QEMU tests**: 15 Rust packages in `ARCEOS_TEST_PACKAGES` + C tests in `test-suit/arceos/c/`. Output matched by regex.
- **Snapshot mechanism**: Build parameters persisted to `.arceos.toml` / `.starry.toml` / `.axvisor.toml` for repeat invocations. Test commands use `Discard` persistence.

## Language

Documentation and README are in Chinese (中文). Code comments and identifiers are in English. Prefer Chinese for user-facing docs when contributing.
