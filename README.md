# Starry OS

*An experimental monolithic OS based on ArceOS*

[![GitHub Stars](https://img.shields.io/github/stars/Starry-OS/StarryOS?style=for-the-badge)](https://github.com/Starry-OS/StarryOS/stargazers)
[![GitHub Forks](https://img.shields.io/github/forks/Starry-OS/StarryOS?style=for-the-badge)](https://github.com/Starry-OS/StarryOS/network)
[![GitHub License](https://img.shields.io/github/license/Starry-OS/StarryOS?style=for-the-badge)](https://github.com/Starry-OS/StarryOS/blob/main/LICENSE)
[![Build status](https://img.shields.io/github/check-runs/Starry-OS/StarryOS/main?style=for-the-badge)](https://github.com/Starry-OS/StarryOS/actions)

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
$ git clone --recursive https://github.com/Starry-OS/StarryOS.git
$ cd StarryOS
```

Or if you have already cloned it with out `--recursive` option:

```bash
$ cd StarryOS
$ git submodule update --init --recursive
```

### 2. Install Prerequisites

#### A. Using Docker

We provide a prebuilt Docker image with all dependencies installed.

For users in mainland China, you can use the following image which includes optimizations like Debian packages mirrors and crates.io mirrors:

```bash
$ docker pull docker.cnb.cool/starry-os/arceos-build
$ docker run -it --rm -v $(pwd):/workspace -w /workspace docker.cnb.cool/starry-os/arceos-build
```

For other users, you can use the image hosted on GitHub Container Registry:

```bash
$ docker pull ghcr.io/arceos-org/arceos-build
$ docker run -it --rm -v $(pwd):/workspace -w /workspace ghcr.io/arceos-org/arceos-build
```

**Note:** The `--rm` flag will destroy the container instance upon exit. Any changes made inside the container (outside of the mounted `/workspace` volume) will be lost. Please refer to the [Docker documentation](https://docs.docker.com/) for more advanced usage.

#### B. Manual Setup

##### i. Install System Dependencies

This step may vary depending on your operating system. Here is an example based on Debian:

```bash
$ sudo apt update
$ sudo apt install -y build-essential cmake clang qemu-system
```

**Note:** Running on LoongArch64 requires QEMU 10. If the QEMU version in your Linux distribution is too old (e.g. Ubuntu), consider building QEMU from [source](https://www.qemu.org/download/).

##### ii. Install Musl Toolchain

1. Download files from https://github.com/arceos-org/setup-musl/releases/tag/prebuilt
2. Extract to some path, for example `/opt/riscv64-linux-musl-cross`
3. Add bin folder to `PATH`, for example:
   ```bash
   $ export PATH=/opt/riscv64-linux-musl-cross/bin:$PATH
   ```

##### iii. Setup Rust toolchain

```bash
# Install rustup from https://rustup.rs or using your system package manager

# Automatically download components via rustup
$ cd StarryOS
$ cargo -V
```

### 3. Prepare rootfs

```bash
# Default target: riscv64
$ make rootfs
# Explicit target
$ make ARCH=riscv64 rootfs
$ make ARCH=loongarch64 rootfs
```

This will download rootfs image from [Starry-OS/rootfs](https://github.com/Starry-OS/rootfs/releases) and set up the disk file for running on QEMU.

### 4. Build and run on QEMU

```bash
# Default target: riscv64
$ make build
# Explicit target
$ make ARCH=riscv64 build
$ make ARCH=loongarch64 build

# Run on QEMU (also rebuilds if necessary)
$ make ARCH=riscv64 run
$ make ARCH=loongarch64 run
```

Note:
1. Binary dependencies will be automatically built during `make build`.
2. You don't have to rerun `build` every time. `run` automatically rebuilds if necessary.
3. The disk file will **not** be reset between each run. As a result, if you want to switch to another architecture, you must run `make rootfs` with the new architecture before `make run`.

## What next?

You can check out the [GUI guide](./docs/x11.md) to set up a graphical environment, or explore other documentation in this folder.

If you're interested in contributing to the project, please see our [Contributing Guide](./CONTRIBUTING.md).

See more build options in the [Makefile](./Makefile).

## License

This project is now released under the Apache License 2.0. All modifications and new contributions in our project are distributed under the same license. See the [LICENSE](./LICENSE) and [NOTICE](./NOTICE) files for details.
