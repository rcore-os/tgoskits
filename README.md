# Starry OS

## Quick Start

### 1. Install System Dependencies

This step may vary depending on your operating system. Here is an example based on Debian:

```bash
$ sudo apt update
$ sudo apt install -y build-essential cmake clang qemu-system
```

**Note:** Running on LoongArch64 requires QEMU 10. If the QEMU version in your Linux distribution is too old (e.g. Ubuntu), consider installing QEMU from [source](https://www.qemu.org/download/).

### 2. Install Musl Toolchain

1. Download files from https://github.com/arceos-org/setup-musl/releases/tag/prebuilt
2. Extract to some path, for example `/opt/riscv64-linux-musl-cross`
3. Add bin folder to `PATH`, for example:
   ```bash
   $ export PATH=/opt/riscv64-linux-musl-cross/bin:$PATH
   ```

### 3. Clone repo

```bash
$ git clone --recursive https://github.com/Starry-OS/StarryOS.git
$ cd StarryOS
```

Or if you have already cloned it with out `--recursive` option:

```bash
$ cd StarryOS
$ git submodule update --init --recursive
```

### 4. Setup Rust toolchain

```bash
# Install rustup from https://rustup.rs or using your system package manager

# Make sure that you don't have `RUSTUP_DIST_SERVER` set
$ export RUSTUP_DIST_SERVER=

# Automatically download components via rustup
$ cd StarryOS
$ rustup target list --installed
```

### 5. Build

```bash
# Default target: riscv64
$ make build
# Explicit target
$ make ARCH=riscv64 build
$ make ARCH=loongarch64 build
```

This should also download required binary dependencies like [cargo-binutils](https://github.com/rust-embedded/cargo-binutils).

### 6. Prepare rootfs

```bash
$ make img
$ make img ARCH=riscv64
$ make img ARCH=loongarch64
```

This will download rootfs image from [GitHub Releases](https://github.com/Starry-OS/StarryOS/releases) and setup the disk file for running on QEMU.

### 7. Run on QEMU

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

## What next?

You can check out the [GUI guide](./docs/gui.md) to set up a graphical environment, or explore other documentation in this folder.

## Other Options

TODO

See [Makefile](./Makefile)


## License

This project is now released under the Apache License 2.0. All modifications and new contributions in our project are distributed under the same license. See the [LICENSE](./LICENSE) and [NOTICE](./NOTICE) files for details.