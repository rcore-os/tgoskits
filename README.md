# Starry OS

## Quick Start for Beginners

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

### 2. Setup Rust toolchain

```bash
# Install rustup from https://rustup.rs or using your system package manager

# Make sure that you don't have `RUSTUP_DIST_SERVER` set
$ export RUSTUP_DIST_SERVER=

# Automatically download components via rustup
$ cd StarryOS
$ rustup target list --installed

```

### 3. Build

```bash
# Default target: riscv64
$ make build
# Explicit target
$ make ARCH=riscv64 build
$ make ARCH=loongarch64 build
```

This should also download required binary dependencies like [cargo-binutils](https://github.com/rust-embedded/cargo-binutils).

### 4. Prepare rootfs

```bash
$ make img
$ make img ARCH=riscv64
$ make img ARCH=loongarch64
```

This will download rootfs image from [GitHub Releases](https://github.com/Starry-OS/StarryOS/releases) and setup the disk file for running on QEMU.

### 5. Run on QEMU

```bash
$ make run ARCH=riscv64
$ make run ARCH=loongarch64

# Shortcut:
$ make rv
$ make la
```

Note:
1. You don't have to rerun the build step before running. `run` will automatically rebuild it.
2. The disk file will **not** be reset between each run. As a result, if you want to switch to another architecture, you must run `make img` with the new architecture before running `make run`.

## Options

TODO

See [Makefile](./Makefile)
