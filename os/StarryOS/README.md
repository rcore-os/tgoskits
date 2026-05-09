<h1 align="center">StarryOS</h1>

<p align="center">An experimental monolithic OS based on ArceOS</p>

<div align="center">

[![GitHub Stars](https://img.shields.io/github/stars/Starry-OS/StarryOS?style=for-the-badge)](https://github.com/Starry-OS/StarryOS/stargazers)
[![GitHub Forks](https://img.shields.io/github/forks/Starry-OS/StarryOS?style=for-the-badge)](https://github.com/Starry-OS/StarryOS/network)
[![GitHub License](https://img.shields.io/github/license/Starry-OS/StarryOS?style=for-the-badge)](https://github.com/Starry-OS/StarryOS/blob/main/LICENSE)
[![Build status](https://img.shields.io/github/check-runs/Starry-OS/StarryOS/main?style=for-the-badge)](https://github.com/Starry-OS/StarryOS/actions)

</div>

English | [中文](README_CN.md)

## Supported Architectures

- [x] RISC-V 64
- [x] LoongArch64
- [x] AArch64
- [ ] x86_64 (work in progress)

## Features

TODO

## Quick Start

### 1. Clone repo

```bash
git clone --recursive https://github.com/Starry-OS/StarryOS.git
cd StarryOS
```

Or if you have already cloned it without `--recursive` option:

```bash
cd StarryOS
git submodule update --init --recursive
```

### 2. Install Prerequisites

#### A. Using Docker

We provide a prebuilt Docker image with all dependencies installed.

For users in mainland China, you can use the following image which includes optimizations like Debian packages mirrors and crates.io mirrors:

```bash
docker pull docker.cnb.cool/starry-os/arceos-build
docker run -it --rm -v $(pwd):/workspace -w /workspace docker.cnb.cool/starry-os/arceos-build
```

For other users, you can use the image hosted on GitHub Container Registry:

```bash
docker pull ghcr.io/arceos-org/arceos-build
docker run -it --rm -v $(pwd):/workspace -w /workspace ghcr.io/arceos-org/arceos-build
```

**Note:** The `--rm` flag will destroy the container instance upon exit. Any changes made inside the container (outside of the mounted `/workspace` volume) will be lost. Please refer to the [Docker documentation](https://docs.docker.com/) for more advanced usage.

#### B. Manual Setup

##### i. Install System Dependencies

This step may vary depending on your operating system. Here is an example based on Debian:

```bash
sudo apt update
sudo apt install -y build-essential cmake clang qemu-system
```

**Note:** Running on LoongArch64 requires QEMU 10. If the QEMU version in your Linux distribution is too old (e.g. Ubuntu), consider building QEMU from [source](https://www.qemu.org/download/).

##### ii. Install Musl Toolchain

1. Download files from [setup-musl releases](https://github.com/arceos-org/setup-musl/releases/tag/prebuilt)
2. Extract to some path, for example `/opt/riscv64-linux-musl-cross`
3. Add bin folder to `PATH`, for example:

   ```bash
   export PATH=/opt/riscv64-linux-musl-cross/bin:$PATH
   ```

##### iii. Setup Rust toolchain

```bash
# Install rustup from https://rustup.rs or using your system package manager

# Automatically download components via rustup
cd StarryOS
cargo -V
```

### 3. Prepare rootfs

```bash
cargo xtask starry rootfs --arch riscv64
cargo xtask starry rootfs --arch loongarch64
```

This will download rootfs image from [Starry-OS/rootfs](https://github.com/Starry-OS/rootfs/releases) and set up the disk file for running on QEMU.

### 4. Build and run on QEMU

```bash
cargo xtask starry build --arch riscv64
cargo xtask starry build --arch loongarch64

# Run on QEMU (also rebuilds if necessary)
cargo xtask starry qemu --arch riscv64
cargo xtask starry qemu --arch loongarch64
```

Note:

1. Binary dependencies will be automatically built during `cargo xtask starry build`.
2. You don't have to rerun `build` every time. `qemu` automatically rebuilds if necessary.
3. The disk file will **not** be reset between each run. As a result, if you want to switch to another architecture, you must run `cargo xtask starry rootfs --arch <arch>` with the new architecture before `cargo xtask starry qemu --arch <arch>`.

## What next?

Explore the board configs under [`configs/board`](./configs/board), or run `cargo xtask starry --help` for the full command list.

If you're interested in contributing to the project, please see our [Contributing Guide](./CONTRIBUTING.md).

## License

This project is now released under the Apache License 2.0. All modifications and new contributions in our project are distributed under the same license. See the [LICENSE](./LICENSE) and [NOTICE](./NOTICE) files for details.
