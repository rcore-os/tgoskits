---
doc_type: CONVENTIONS
scope: Code conventions and best practices
---
# Conventions

**Analysis Date:** 2026-05-13

## Code Style

### Rust Edition and Toolchain

The workspace uses **Rust edition 2024** declared in `Cargo.toml`:

```toml
[workspace.package]
edition = "2024"
```

The toolchain is pinned to **nightly-2026-04-27** via `rust-toolchain.toml`:

```toml
[toolchain]
profile = "minimal"
channel = "nightly-2026-04-27"
components = ["rust-src", "llvm-tools", "rustfmt", "clippy"]
targets = [
    "x86_64-unknown-none",
    "riscv64gc-unknown-none-elf",
    "aarch64-unknown-none-softfloat",
    "loongarch64-unknown-none-softfloat",
]
```

All target platforms are `no_std` bare-metal targets. The workspace resolver is `"3"` (edition 2024 resolver).

### Formatting

`rustfmt.toml` defines these settings:

| Setting | Value |
|---------|-------|
| `style_edition` | `"2024"` |
| `group_imports` | `"StdExternalCrate"` |
| `imports_granularity` | `"Crate"` |
| `normalize_comments` | `true` |
| `condense_wildcard_suffixes` | `true` |
| `enum_discrim_align_threshold` | `20` |
| `use_field_init_shorthand` | `true` |
| `format_strings` | `true` |
| `format_code_in_doc_comments` | `true` |
| `format_macro_matchers` | `true` |
| `blank_lines_upper_bound` | `1` |
| `unstable_features` | `true` |

Import ordering: `std` and external crates are grouped together, and all imports from the same crate are merged into a single `use` statement. Run `cargo fmt` after every change.

### Linting

Clippy is enforced in CI via `cargo xtask clippy`. All crates listed in `scripts/test/clippy_crates.csv` must pass clippy clean. If a crate passes clippy but is not listed in `clippy_crates.csv`, add it — do not remove crates that fail. Do not suppress clippy with `#[allow]` unless explicitly directed; fix the root cause instead.

After any logic change, run:
```bash
cargo xtask clippy --package <crate-name>
```

### Naming Conventions

**Crates:** Lowercase kebab-case, prefixed by ecosystem:
- `ax-*` — ArceOS ecosystem crates (`ax-hal`, `ax-mm`, `ax-task`)
- `axplat-*` — Platform crates (`axplat-dyn`, `axplat-x86-qemu-q35`, `axplat-riscv64-qemu-virt-hv`)
- `starry-*` — StarryOS crates (`starry-kernel`, `starryos`, `starry-process`, `starry-vm`)
- Cross-cutting: `axdriver-*` (driver subsystems), `axfs-*` (filesystem crates)

**Modules:** snake_case, matching directory structure. Modules in `components/` are independent crates; modules within each OS are internal.

**Features:** snake_case, descriptive (`bus-mmio`, `bus-pci`, `driver-sdmmc`, `pci-list-devices`).

**Files:** Cargo convention — `lib.rs` for library roots, `mod.rs` or `module_name.rs` for submodules.

## Build System

Everything goes through `cargo xtask`. The root xtask at `xtask/` delegates to `scripts/axbuild/` (the `axbuild` crate). Never use raw `cargo build`/`cargo run`/`cargo test`.

Common commands:
```bash
cargo xtask arceos qemu --package ax-helloworld --arch aarch64
cargo xtask starry qemu --arch aarch64
cargo xtask arceos test qemu --target aarch64
cargo xtask starry test qemu --arch riscv64
cargo xtask test                      # full regression
cargo xtask clippy --package <crate>
cargo xtask sync-lint                 # verify clippy/std tracking files
cargo xtask starry rootfs --arch aarch64  # prepare rootfs once before first run
```

## Git Conventions

### Branch Strategy

```
feature/* --PR--> dev --regular merge--> main
```

- `main`: stable, no direct push
- `dev`: integration branch, runs full CI validation
- Feature branches: branch off `dev`, PR back to `dev`

### Commit Messages

Conventional Commits format: `type(scope): subject`

| Type | Use case |
|------|----------|
| `feat` | New feature |
| `fix` | Bug fix |
| `chore` | Maintenance, tooling |
| `refactor` | Code change without behavior change |
| `test` | Adding or updating tests |
| `docs` | Documentation only |

Scopes reflect the affected component: `axbuild`, `axhal`, `axmm`, `starry-kernel`, `axvisor`, `tg-xtask`, `drivers`, `platform`, `ci`, etc.

### PR Title Format

PR titles follow the same pattern: `type(scope): content`. Examples from commit history:
- `feat(axbuild): add Starry remote board test flow`
- `fix(agent): add PR #498 lessons to pr-review`
- `chore(plugin): apply self-evolve fixes`

## Module Organization

### Workspace Layout

All crates live under the root `Cargo.toml` workspace with `resolver = "3"` and `members` listing every crate in `components/`, `drivers/`, `platform/`, `os/`, `xtask/`, and `scripts/axbuild/`.

### crate_interface Pattern

The `components/crate_interface/` (`ax-crate-interface`) crate provides a `#[def_interface]` / `#[impl_interface]` / `#[call_interface]` macro system for loosely-coupled cross-crate trait dispatch without link-time dependencies. This is the standard way to break circular dependencies in the codebase.

Key rules:
- Define a trait annotated with `#[def_interface]` in any crate.
- Register an implementation with `#[impl_interface]` in any other crate.
- Dispatch with `#[call_interface]` from callers.
- Methods with receivers (`self`, `&self`, `&mut self`) are not allowed — only associated functions.
- Generic parameters are not supported on interface methods.
- Use `namespace = "..."` option to disambiguate traits with the same name.

### Driver Layering

Drivers under `drivers/` are organized by device type:
```
drivers/
  blk/simple-sdmmc/
  cpu-infer/
  net/
  npu/
  pci/
  soc/rockchip/
```

Drivers separate into layers: **Core** (device logic), **Capability** (feature traits), **OS Glue** (kernel integration), and **Runtime** (execution context).

Safety rule for MMIO/DMA access: use `mmio-api` for MMIO/iomap operations and `dma-api` for DMA. Never use raw pointer casts for hardware access.

### Platform Abstraction

`platform/` contains per-target platform crates:
```
platform/
  axplat-dyn/              # Dynamic platform dispatch (used by Axvisor/StarryOS)
  riscv64-qemu-virt/       # RISC-V virt machine
  x86-qemu-q35/            # x86 Q35 machine
```

`components/axplat_crates/` defines the platform abstraction layer with per-architecture HAL implementations. Each implements traits and HAL primitives for its target. Platform crates use feature flags to compose capabilities.

### ArceOS Module Graph

`os/arceos/modules/` contains ~15 layered crates (`axhal`, `axmm`, `axtask`, `axruntime`, `axdriver`, `axfs`, `axnet`, etc.). They form a unikernel where each module is conditionally compiled via feature flags from the top-level app crate.

### StarryOS Structure

StarryOS is split into:
- `os/StarryOS/kernel/` — core kernel logic
- `os/StarryOS/starryos/` — userspace compatibility layer
- `os/StarryOS/configs/` — per-board/per-target configuration TOML files
- `os/StarryOS/make/` — build infrastructure

### Axvisor Structure

- `os/axvisor/src/` — hypervisor source
- `os/axvisor/configs/` — board/target configurations

## Safety Rules

### No Raw Pointer Casts for Hardware Access

Use the provided abstractions:
- `mmio-api` — for MMIO and iomap operations
- `dma-api` — for DMA operations

Never use raw pointer casts directly on hardware addresses.

### no_std vs std Separation

All kernel code is `no_std` (bare-metal targets). Only `scripts/axbuild/` (build tooling) and `tg-xtask` use `std`. The `scripts/test/std_crates.csv` file tracks crates eligible for std-mode testing — these are infrastructure crates that can be tested with `cargo test` on the host.

If a crate can be tested in std mode, add it to `scripts/test/std_crates.csv`. The `cargo xtask sync-lint` command verifies that `std_crates.csv` and `clippy_crates.csv` are consistent with the actual crate graph.

### Lock Ordering

The `ax-lockdep` (`components/ax-lockdep`) crate provides lock dependency validation. Follow its conventions for lock acquisition ordering.

### Unsafe Code

Unsafe code is permitted but must be encapsulated behind safe abstractions. Platform HAL crates and MMIO drivers are the primary users of unsafe. Document the safety invariant with `// SAFETY:` comments.

## Documentation

### Project Instructions

`CLAUDE.md` at the repository root provides AI coding guidance. It covers:
- Build system (xtask-based)
- Toolchain and required targets
- Repository layout
- Key architecture patterns (platform abstraction, driver model, crate_interface)
- Branch strategy
- Code quality requirements
- Debugging setup (VS Code launch configs)

### Inline Documentation

Follow Rust documentation conventions:
- `//!` module-level documentation for all public modules
- `///` doc comments for all public items (types, functions, traits)
- `// SAFETY:` comments for unsafe block justifications
- Use code examples in doc comments where helpful

### Developer Documentation Site

Docusaurus-based documentation site at `docs/`:
- `docs/docs/` — MDX/JSX documentation pages
- `docs/blog/` — blog posts
- `docs/community/` — community page
- Build with `yarn` (package manager is Yarn per `docs/yarn.lock`)

### Config Files

`.vscode/launch.json` provides VS Code debug launch configurations for CodeLLDB. Each system provides **Main** (stops at app entry) and **Boot** (stops at platform boot) debug targets.

---

*Convention analysis: 2026-05-13*
