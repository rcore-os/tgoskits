<h1 align="center">TGOSKits</h1>

<p align="center">An integrated repository for operating system and virtualization development</p>

<div align="center">

[![Build & Test](https://github.com/rcore-os/tgoskits/actions/workflows/test.yml/badge.svg)](https://github.com/rcore-os/tgoskits/actions/workflows/test.yml)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

TGOSKits is an integrated repository for operating system and virtualization development. It uses Git Subtree to manage more than 60 standalone component repositories, bringing ArceOS, StarryOS, Axvisor, and related platform crates into a single workspace for component-level development, cross-system integration, and unified testing.

## 1. Quick Navigation

This repository contains multiple systems and dozens of standalone components. Different development goals map to different documents and command entry points. The table below helps you quickly find the most relevant document and the shortest useful command for your current task.

| Your Goal | Recommended First Reading | Shortest Command |
| --- | --- | --- |
| First successful run | [docs/docs/quickstart/qemu/aarch64.md](docs/docs/quickstart/qemu/aarch64.md) | `cargo xtask arceos qemu --package ax-helloworld --arch riscv64` |
| Full development example | [docs/docs/demo.md](docs/docs/demo.md) | A complete example for creating or modifying a component from scratch |
| Component development guide | [docs/docs/design/architecture/arch.md](docs/docs/design/architecture/arch.md) | Start from `components/` or `os/arceos/modules/` |
| Develop ArceOS | [docs/docs/arceos-guide.md](docs/docs/arceos-guide.md) | `cargo xtask arceos qemu --package ax-helloworld --arch riscv64` |
| Develop StarryOS | [docs/docs/starryos-guide.md](docs/docs/starryos-guide.md) | `cargo xtask starry qemu --arch riscv64` |
| Develop Axvisor | [docs/docs/manual/deploy/qemu.md](docs/docs/manual/deploy/qemu.md) | `cargo xtask axvisor test qemu --target aarch64` |
| Understand the build and test matrix | [docs/docs/design/build/flow.md](docs/docs/design/build/flow.md) | `cargo xtask test` |
| Understand how the repository organizes many standalone components | [docs/docs/repo.md](docs/docs/repo.md) | `python3 scripts/repo/repo.py list` |

## 2. Repository Layout

The repository is organized by responsibility: `components/` stores reusable standalone components, `os/` stores the source code of the three target systems, `platform/` stores platform-related crates, and `docs/` centralizes developer documentation. `scripts/repo/` provides subtree management tools.

```text
tgoskits/
├── components/                # standalone component crates managed by subtree
├── os/
│   ├── arceos/                # ArceOS: modules / api / ulib / examples
│   ├── StarryOS/              # StarryOS: kernel / starryos / make
│   └── axvisor/               # Axvisor: src / configs / local xtask
├── platform/                  # platform-related crates
├── test-suit/                 # ArceOS / StarryOS system tests
├── xtask/                     # root tg-xtask
├── scripts/
│   └── repo/                  # subtree management scripts and repos.csv
└── docs/                      # developer documentation
```

The repository follows a three-layer branch strategy: `main`, `dev`, and feature branches. `main` serves as the stable baseline, `dev` serves as the integration branch for development and CI validation, and developers create feature branches from `dev` and merge back via PRs. Direct pushes to `main` are forbidden.

| Branch | Responsibility | Rule |
| --- | --- | --- |
| `main` | Stable release branch, regularly merged from `dev` | No direct push |
| `dev` | Integration branch for development and CI | Merge through PR |
| Feature branches | Individual development branches | Submit PRs to `dev` when ready |

```text
feature/* ──PR──► dev
                   │
                regular merge
                   ▼
                 main
```

If you need to synchronize with component repositories, maintainers should explicitly run `scripts/repo/repo.py pull/push`. See [docs/docs/repo.md](docs/docs/repo.md) for details.

## 3. Quick Experience

The following commands provide the shortest runnable path for the three systems, helping you verify that your environment is ready. All three systems use the unified `cargo xtask <os> <subcommand>` entry point; `cargo arceos`, `cargo starry`, and `cargo axvisor` are only equivalent aliases. ArceOS can run directly, StarryOS requires a prepared rootfs, and Axvisor requires guest images and configuration prepared beforehand.

```bash
git clone https://github.com/rcore-os/tgoskits.git
cd tgoskits

# ArceOS: fastest Hello World path
cargo xtask arceos qemu --package ax-helloworld --arch riscv64
# Equivalent alias
cargo arceos qemu --package ax-helloworld --arch riscv64

# StarryOS: prepare rootfs before the first run
cargo xtask starry qemu --arch riscv64
# Equivalent alias
cargo starry qemu --arch riscv64

# Axvisor: recommended to use the official setup script for guest and rootfs
cargo xtask axvisor qemu --arch aarch64
# Equivalent alias
cargo axvisor qemu --arch aarch64
```

Axvisor cannot be started only with `build/qemu`, because guest images, VM configuration, and rootfs are still required before runtime. It is recommended to use `os/axvisor/scripts/setup_qemu.sh` to prepare those runtime resources first, then run `cargo xtask axvisor qemu --arch <arch>`. See [docs/docs/manual/deploy/qemu.md](docs/docs/manual/deploy/qemu.md) and [docs/docs/axvisor-guide.md](docs/docs/axvisor-guide.md) for the full workflow.

## 4. Quick Development

For development, first locate the code entry point you want to modify under `components/`, `os/arceos/modules/`, `os/StarryOS/kernel/`, or `os/axvisor/src/`. During debugging, run the smallest `qemu` consumer first to confirm the behavior, then run the corresponding `test qemu` command for regression, and finally add host or std tests when the change is stable.

```bash
# host / std crates
cargo xtask test

# ArceOS: run the smallest app, then the unified QEMU tests
cargo xtask arceos qemu --package ax-helloworld --arch riscv64
cargo xtask arceos test qemu --target riscv64

# StarryOS: prepare rootfs, run the minimal boot, then unified QEMU tests
cargo xtask starry rootfs --arch riscv64
cargo xtask starry qemu --arch riscv64
cargo xtask starry test qemu --target riscv64
cargo xtask starry test qemu --stress -t riscv64
cargo xtask starry test qemu --stress -t riscv64 -c stress-ng-0

# Axvisor: prepare guest and rootfs, run the minimal boot, then unified QEMU tests
(cd os/axvisor && ./scripts/setup_qemu.sh arceos)
cargo xtask axvisor qemu --arch aarch64
cargo xtask axvisor test qemu --target aarch64
```

See [docs/docs/design/test/test.md](docs/docs/design/test/test.md) and [docs/docs/components.md](docs/docs/components.md) for component validation strategies after modifications.

## 5. License

The repository as a whole is licensed under `Apache-2.0`. Individual components may also include their own LICENSE files; when in doubt, use the files in each component directory as the source of truth.
