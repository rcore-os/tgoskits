# StarryOS

[![CI](https://github.com/arceos-org/starry-next/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/starry-next/actions/workflows/ci.yml)

A monolithic kernel based on [ArceOS](https://github.com/arceos-org/arceos).

## Quick Start

### 1. Install Build Dependencies

Install [cargo-binutils](https://github.com/rust-embedded/cargo-binutils) to use `rust-objcopy` and `rust-objdump` tools:

```bash
cargo install cargo-binutils
```

#### Dependencies for C apps

Install `libclang-dev`:

```bash
sudo apt install libclang-dev
```

Download & install [musl](https://musl.cc) toolchains:

```bash
# download
wget https://musl.cc/aarch64-linux-musl-cross.tgz
wget https://musl.cc/riscv64-linux-musl-cross.tgz
wget https://musl.cc/x86_64-linux-musl-cross.tgz
wget https://github.com/LoongsonLab/oscomp-toolchains-for-oskernel/releases/download/gcc-13.2.0-loongarch64/gcc-13.2.0-loongarch64-linux-gnu.tgz
wget https://github.com/LoongsonLab/oscomp-toolchains-for-oskernel/raw/refs/heads/main/musl-loongarch64-1.2.2.tgz
# install
tar zxf aarch64-linux-musl-cross.tgz
tar zxf riscv64-linux-musl-cross.tgz
tar zxf x86_64-linux-musl-cross.tgz
tar zxf gcc-13.2.0-loongarch64-linux-gnu.tgz
tar zxf musl-loongarch64-1.2.2.tgz && cd musl-loongarch64-1.2.2 && ./setup && cd ..
# exec below command in bash OR add below info in ~/.bashrc
export PATH=`pwd`/x86_64-linux-musl-cross/bin:`pwd`/aarch64-linux-musl-cross/bin:`pwd`/riscv64-linux-musl-cross/bin:`pwd`/gcc-13.2.0-loongarch64-linux-gnu/bin:`pwd`/musl-loongarch64-1.2.2/bin:$PATH
```

#### Dependencies for running apps

```bash
# for Debian/Ubuntu
sudo apt-get install qemu-system
```

```bash
# for macos
brew install qemu
```

Notice: The version of `qemu` should **be no less than 8.2.0**.

Other systems, arch and version please refer to [Qemu Download](https://www.qemu.org/download/#linux)

### 2. Build & Run

#### Qucik Run

```bash
# Clone the base repository
./scripts/get_deps.sh

# Run riscv64 example
make clean
make ARCH=riscv64 AX_TESTCASE=nimbos user_apps
make ARCH=riscv64 AX_TESTCASE=nimbos BLK=y NET=y ACCEL=n EXTRA_CONFIG=../configs/riscv64.toml FEATURES=fp_simd run
# Run x86_64 example
make clean
make ARCH=x86_64 AX_TESTCASE=nimbos user_apps
make ARCH=x86_64 AX_TESTCASE=nimbos BLK=y NET=y ACCEL=n EXTRA_CONFIG=../configs/x86_64.toml FEATURES=fp_simd run
# Run aarch64 example
make clean
make ARCH=aarch64 AX_TESTCASE=nimbos user_apps
make ARCH=aarch64 AX_TESTCASE=nimbos BLK=y NET=y ACCEL=n EXTRA_CONFIG=../configs/aarch64.toml FEATURES=fp_simd run
# Run Loongarch64 example
make clean
make ARCH=loongarch64 AX_TESTCASE=nimbos user_apps
make ARCH=loongarch64 AX_TESTCASE=nimbos BLK=y NET=y ACCEL=n EXTRA_CONFIG=../configs/loongarch64.toml FEATURES=fp_simd run
```

#### Commands Explanation

```bash
# Clone the base repository
./scripts/get_deps.sh

# Build user applications
make ARCH=<arch> AX_TESTCASE=<testcases> user_apps

# Build kernel
make ARCH=<arch> LOG=<log> AX_TESTCASE=<testcases> build

# Run kernel
make ARCH=<arch> LOG=<log> AX_TESTCASE=<testcases> run
```

Where `testcases` are shown under the `apps/` folder.

`<arch>` should be one of `riscv64`, `aarch64`, `x86_64`, `loongarch64`.

`<log>` should be one of `off`, `error`, `warn`, `info`, `debug`, `trace`.

More arguments and targets can be found in [Makefile](./Makefile).

For example, to run the [nimbos testcases](apps/nimbos/) on `qemu-system-x86_64` with log level `info`:

```bash
make ARCH=x86_64 LOG=info AX_TESTCASE=nimbos run
```

Note: Arguments like `NET`, `BLK`, and `GRAPHIC` enable devices in QEMU, which take effect only at runtime, not at build time.
