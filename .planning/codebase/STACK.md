---
doc_type: STACK
scope: Technology stack and dependencies for TGOSKits
analysis_date: 2026-05-13
---
# Technology Stack

## Languages & Runtimes

**Primary:**
- Rust `nightly-2026-04-27`, edition `2024` - All kernel, hypervisor, driver, platform, and build system code
  - Toolchain declared in `rust-toolchain.toml`
  - Profile: `minimal`
  - Components: `rust-src`, `llvm-tools`, `rustfmt`, `clippy`
  - Resolver: `3` (workspace `Cargo.toml`)
  - Lockfile: `Cargo.lock` present (10,819 lines)

**Secondary:**
- Python 3 - VS Code debug session management (`.vscode/session.py`) and git-subtree management (`scripts/repo/repo.py`)
- JavaScript / TypeScript - Docusaurus documentation site (`docs/package.json`, `docs/docusaurus.config.js`)
  - Node.js `>=18.0`, Yarn package manager
  - Docusaurus `^3.10.1` with `@docusaurus/theme-mermaid`

**Required Compiler Targets (from `rust-toolchain.toml`):**
- `x86_64-unknown-none`
- `riscv64gc-unknown-none-elf`
- `aarch64-unknown-none-softfloat`
- `loongarch64-unknown-none-softfloat`

## Build System

**Primary entry point:** `cargo xtask` (aliased in `.cargo/config.toml`)

```bash
cargo xtask arceos qemu --package ax-helloworld --arch aarch64
cargo xtask starry qemu --arch aarch64
cargo xtask axvisor qemu --arch aarch64
```

**Architecture:**
- `tg-xtask` (`xtask/Cargo.toml`, `xtask/src/main.rs`) -- thin tokio async main that delegates to `axbuild::run()`
- `axbuild` (`scripts/axbuild/`) -- the actual build orchestrator library with CLI subcommands via `clap`
  - Subcommands: `arceos`, `starry`, `axvisor`, `test`, `clippy`, `sync-lint`, `board`
  - Per-system modules: `scripts/axbuild/src/arceos/`, `starry/`, `axvisor/`
  - RootFS management: `scripts/axbuild/src/rootfs/`
  - Test runner: `scripts/axbuild/src/test/`
  - Board management via `ostool`: `scripts/axbuild/src/board.rs`

**Cargo aliases (from `.cargo/config.toml`):**
```toml
arceos  = "run -p tg-xtask -- arceos"
axvisor = "run -p tg-xtask -- axvisor"
starry  = "run -p tg-xtask -- starry"
xtask   = "run -p tg-xtask --"
board   = "run -p tg-xtask -- board"
```

**Build profiles:**
- Release: LTO enabled (`lto = true`)
- Debug: standard (used for QEMU debug sessions)

## Core Dependencies

**Build Orchestrator (`tg-xtask` / `axbuild`):**
- `tokio` 1.0 (async runtime, features: `full`) -- powers the xtask async main
- `clap` 4.6 (CLI framework, `derive` feature) -- all subcommand parsing
- `anyhow` 1.0 -- error handling
- `cargo_metadata` 0.23 -- workspace introspection
- `ostool` 0.15 -- remote board orchestration
- `reqwest` 0.13 -- HTTP client for rootfs downloads
- `flate2` 1.0, `tar` 0.4, `xz2` 0.1 -- archive handling for rootfs
- `serde` 1.0 / `serde_json` 1 / `toml` 1 -- configuration parsing
- `chrono` 0.4 -- timestamping
- `colored` 3, `indicatif` 0.18, `env_logger` 0.11 -- terminal output and progress
- `object` 0.38, `sha2` 0.10 -- binary inspection and integrity
- `proc-macro2` / `quote` / `syn` 2 -- code generation (clippy/sync-lint)
- `walkdir` 2 -- filesystem traversal
- `tracing` 0.1 / `tracing-subscriber` 0.3 / `tracing-log` 0.2 -- structured logging

**Kernel / Driver Key Crates:**
- `spin` 0.10 -- no_std synchronization primitives (Mutex, RwLock, Once)
- `lock_api` 0.4 -- trait definitions for lock implementations
- `buddy-slab-allocator` 0.4 -- physical memory allocation
- `lazy_static` 1.5 (`spin_no_std` feature) -- lazy initialization in no_std
- `cfg-if` 1.0 -- conditional compilation
- `log` 0.4 -- logging facade (used across all crates)
- `smoltcp` 0.13.0 (default-features false) -- TCP/IP networking stack
- `mmio-api` 0.2.1 -- MMIO/iomap abstractions
- `dma-api` 0.7.2 -- DMA abstractions
- `rdif-pcie` 0.2 -- PCIe root complex interface
- `heapless` 0.9 -- fixed-capacity data structures for no_std
- `fdt-edit` 0.2 -- Flattened Device Tree manipulation
- `rdrive` 0.20, `rdif-clk` 0.5 -- Rockchip driver interfaces

**ArceOS Module Configuration:**
- `axconfig-gen` + `axconfig-macros` -- compile-time configuration code generation from TOML
- `cargo-axplat` -- platform crate code generation scaffolding
- `axbuild` -- build orchestrator also used as a library dependency by crates that need build-time support

**Virtualization Crates:**
- `somehal` 0.6 -- hardware abstraction layer traits for hypervisor
- Architecture-specific VMM: `arm_vcpu`, `riscv_vcpu`, `x86_vcpu`, `loongarch_vcpu`
- Interrupt controllers: `arm_vgic`, `riscv_vplic`, `x86_vlapic`

## Platform Targets

**Supported Architectures (4 total):**

| Architecture | Compiler Target | QEMU Machine | Platform Crates |
|-------------|----------------|--------------|-----------------|
| x86_64 | `x86_64-unknown-none` | q35 / pc | `axplat-x86-pc`, `x86-qemu-q35` (HV) |
| AArch64 | `aarch64-unknown-none-softfloat` | virt | `axplat-aarch64-qemu-virt`, `axplat-aarch64-raspi`, `axplat-aarch64-bsta1000b`, `axplat-aarch64-phytium-pi` |
| RISC-V 64 | `riscv64gc-unknown-none-elf` | virt | `axplat-riscv64-qemu-virt`, `riscv64-qemu-virt` (HV) |
| LoongArch 64 | `loongarch64-unknown-none-softfloat` | virt | `axplat-loongarch64-qemu-virt` |

**Platform Crates Location:** `components/axplat_crates/platforms/`, `platform/`

**Hypervisor-specific Platforms:**
- `platform/axplat-dyn` (`axplat-dyn`) -- dynamic platform detection and dispatch for Axvisor, includes runtime board inference via `src/cpu_infer.rs`
- `platform/x86-qemu-q35` (`axplat-x86-qemu-q35`) -- x86_64 QEMU Q35 chipset for Axvisor
- `platform/riscv64-qemu-virt` (`axplat-riscv64-qemu-virt-hv`) -- RISC-V virt for Axvisor with hypervisor extensions

**Physical Board Support:**
- Orange Pi 5 Plus (Rockchip RK3588, AArch64) -- self-hosted CI runner
- Raspberry Pi 4 (AArch64) -- chainloader via `os/arceos/tools/raspi4/chainloader`
- Phytium Pi (AArch64)
- BSTA1000B (AArch64)

**QEMU Versions:**
- CI/Docker: QEMU 10.2.1 built from source (`container/Dockerfile` line 4)
- LoongArch hypervisor: custom QEMU-LVZ fork (`container/Dockerfile.axvisor-lvz`, from `numpy1314/QEMU-LVZ`)

## Development Tools

**Formatting (`rustfmt.toml`):**
```toml
style_edition = "2024"
group_imports = "StdExternalCrate"
imports_granularity = "Crate"
normalize_comments = true
format_strings = true
format_code_in_doc_comments = true
```

**Linting:**
- `cargo xtask clippy --package <crate>` for individual crates
- `cargo xtask clippy` for the maintained whitelist (103 crates in `scripts/test/clippy_crates.csv`)
- `cargo xtask sync-lint` for atomic ordering correctness checks (detects suspicious `Relaxed` synchronization)

**Code Generation:**
- `cargo xtask arceos build` auto-generates `axconfig` via `axconfig-gen` before compilation
- `axconfig-gen` processes per-platform `.axconfig.toml` files

**Debugging:**
- VS Code with CodeLLDB extension (`lldb` type)
- Launch configs in `.vscode/launch.json` -- 6 configurations:
  - ArceOS Main / Boot (AArch64)
  - Axvisor Main / Boot (AArch64)
  - StarryOS Main / Boot (AArch64)
- QEMU session management via `.vscode/session.py` (Python script managing QEMU lifecycle, GDB stub on port 1234)
- Build tasks in `.vscode/tasks.json` -- per-system build + QEMU start/stop

**Version Control:**
- Git subtree workflow for 60+ component repositories (`scripts/repo/repo.py`, `scripts/repo/repos.csv`)
- Conventional Commits: `type(scope): content`

## Key Constraints

- **Rust nightly:** Pinned to `nightly-2026-04-27` -- cannot use stable Rust features
- **Edition 2024:** All crates must use Rust Edition 2024
- **no_std:** The vast majority of crates are `#![no_std]`. Only `tg-xtask`, `axbuild`, some test crates, and `axvmconfig` (with `std` feature) use the standard library
- **Workspace resolver 3:** Edition 2024 default resolver, requiring explicit feature unification
- **Minimal toolchain profile:** Only `rust-src`, `llvm-tools`, `rustfmt`, `clippy` installed
- **LTO for release:** Full link-time optimization enabled on release builds
- **Cross-compilation:** All builds are cross-compiled to bare-metal targets; no native host builds for kernel code
- **Hypervisor split:** Axvisor uses separate platform crates (`platform/`) from ArceOS/Starry (`components/axplat_crates/`) with `hv` features
- **crate_interface pattern:** Components communicate through `#[crate_interface]` macro traits rather than direct dependency linkage, avoiding circular dependencies
- **Docker required for CI:** All CI test jobs run inside Docker containers (ghcr.io hosted), with musl cross-compilers and QEMU user/system emulators pre-installed

---

*Stack analysis: 2026-05-13*
