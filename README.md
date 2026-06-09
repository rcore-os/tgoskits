<h1 align="center">TGOSKits</h1>

<p align="center">An integrated Rust workspace for operating system and virtualization development</p>

<div align="center">

[![Build & Test](https://github.com/rcore-os/tgoskits/actions/workflows/ci.yml/badge.svg)](https://github.com/rcore-os/tgoskits/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

## 1. Introduction

TGOSKits is an integrated repository for operating system and virtualization development. It brings together ArceOS, StarryOS, Axvisor, shared components, platform crates, and driver infrastructure in one workspace. A unified `cargo xtask` entry point is used for build, run, debug, and test workflows, making the repository suitable for component development, cross-system integration, and system-level validation.

Project site: [https://rcore-os.cn/tgoskits/](https://rcore-os.cn/tgoskits/). To understand the project scope and system relationships, start from the [TGOSKits documentation](https://rcore-os.cn/tgoskits/docs/introduction).

## 2. Repository

TGOSKits brings multiple standalone subprojects into the root repository through Git Subtree and provides unified entry points for building, running, testing, and documentation. The main directories are:

```text
tgoskits/
├── components/                # reusable component crates
├── os/
│   ├── arceos/                # ArceOS modular kernel
│   ├── StarryOS/              # StarryOS Linux-compatible OS
│   └── axvisor/               # Axvisor Type-I Hypervisor
├── platform/                  # platform and board support crates
├── drivers/                   # reusable drivers and driver subsystems
├── test-suit/                 # system-level test cases
├── xtask/                     # unified root command entry
├── scripts/                   # repository maintenance, test, and sync scripts
└── docs/                      # Docusaurus documentation site
```

For subtree synchronization, component layering, and development conventions, see [repository structure and collaboration](https://rcore-os.cn/tgoskits/docs/contributing/repo) and the [component development guide](https://rcore-os.cn/tgoskits/docs/development/components).

## 3. Quick Experience

### 3.1 Environment Setup

For a first run, the recommended path is to use the project container image. It already includes the Rust toolchain, QEMU, and common cross-compilation dependencies, matching the CI environment:

```bash
git clone https://github.com/rcore-os/tgoskits.git
cd tgoskits

docker pull ghcr.io/rcore-os/tgoskits-container:latest
docker run -it --rm \
  -v "$(pwd)":/workspace \
  -w /workspace \
  ghcr.io/rcore-os/tgoskits-container:latest
```

If you do not use the container, prepare at least Rust, basic build tools, and common QEMU packages. The recommended QEMU version is 10.2.1, matching the container and CI environment; distribution packages are usually enough for quick trials, but switch to the container if a target is missing or behavior differs:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
sudo apt update
sudo apt install -y cmake make ninja-build pkg-config
sudo apt install -y qemu-system-arm qemu-system-riscv64 qemu-system-x86
cargo install cargo-binutils
```

See [quick start overview](https://rcore-os.cn/tgoskits/docs/quickstart/overview) and [CI and container images](https://rcore-os.cn/tgoskits/docs/build/ci) for the full environment guide.

### 3.2 QEMU Verification

First confirm that common QEMU commands are available, preferably matching QEMU 10.2.1 from the container and CI environment:

```bash
qemu-system-riscv64 --version
qemu-system-aarch64 --version
qemu-system-x86_64 --version
qemu-system-loongarch64 --version
```

Then use the unified `cargo xtask` entry point to run the three system paths:

```bash
# ArceOS: run Hello World
cargo xtask arceos qemu --package arceos-helloworld --arch aarch64

# StarryOS: prepare rootfs before the first run
cargo xtask starry rootfs --arch aarch64
cargo xtask starry qemu --arch aarch64

# Axvisor: run a Hypervisor QEMU scenario
cargo xtask axvisor qemu --arch aarch64
```

If you only want the shortest path to a successful run, start with ArceOS Hello World. For more systems, architecture combinations, and QEMU options, see the [quick start overview](https://rcore-os.cn/tgoskits/docs/quickstart/overview) and [run and QEMU](https://rcore-os.cn/tgoskits/docs/build/run).

## 4. Contributing

Issues and pull requests are welcome. A typical workflow is:

1. Read [repository structure and collaboration](https://rcore-os.cn/tgoskits/docs/contributing/repo).
2. Create a feature branch from `dev`.
3. Run the relevant `cargo xtask` build, test, or clippy checks after making changes.
4. Open a PR and describe the change scope, validation, and impact.

For a full development example, documentation contribution, and rootfs maintenance notes, see the [contribution docs](https://rcore-os.cn/tgoskits/docs/contributing/demo). Use [GitHub Issues](https://github.com/rcore-os/tgoskits/issues) for feedback and [GitHub Pull Requests](https://github.com/rcore-os/tgoskits/pulls) for patches.

## 5. License

TGOSKits as a whole is licensed under [Apache-2.0](./LICENSE). Some subtree components may include their own license files; if there is any difference, use the license file in the component directory as the source of truth.
