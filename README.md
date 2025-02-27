# StarryOS

[![CI](https://github.com/arceos-org/starry-next/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/starry-next/actions/workflows/ci.yml)

A monolithic kernel based on [ArceOS](https://github.com/arceos-org/arceos).

## Quick Start

### Build and Run through Docker
Install [Docker](https://www.docker.com/) in your system.

Then build all dependencies through provided dockerfile:

```bash
./scripts/get_deps.sh
cd .arceos
docker build -t starry -f Dockerfile .
```

Create a container and build/run app:
```bash
# back to the root directory of the project
cd ..
docker run --privileged --rm -it -v $(pwd):/starry -w /starry starry bash

# Now build/run app in the container
make user_apps
make defconfig
make run
```

### Manually Build and Run

#### 1. Install Build Dependencies

```bash
cargo install cargo-binutils axconfig-gen

sudo apt install libclang-dev cmake dosfstools build-essential
```

Download & install [musl](https://musl.cc) toolchains:

```bash
# download
wget https://musl.cc/aarch64-linux-musl-cross.tgz
wget https://musl.cc/riscv64-linux-musl-cross.tgz
wget https://musl.cc/x86_64-linux-musl-cross.tgz
wget https://github.com/LoongsonLab/oscomp-toolchains-for-oskernel/releases/download/loongarch64-linux-musl-cross-gcc-13.2.0/loongarch64-linux-musl-cross.tgz
# install
tar zxf aarch64-linux-musl-cross.tgz
tar zxf riscv64-linux-musl-cross.tgz
tar zxf x86_64-linux-musl-cross.tgz
tar zxf loongarch64-linux-musl-cross.tgz

# exec below command in bash OR add below info in ~/.bashrc
export PATH=`pwd`/x86_64-linux-musl-cross/bin:`pwd`/aarch64-linux-musl-cross/bin:`pwd`/riscv64-linux-musl-cross/bin:`pwd`/loongarch64-linux-musl-cross/bin:$PATH
```

#### 2. Dependencies for running apps

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

#### 3. Build & Run

```bash
# Clone the base repository
./scripts/get_deps.sh

# Run riscv64 example
make ARCH=riscv64 AX_TESTCASE=nimbos user_apps
# When running on a new architecture, you need to generate the configuration file again.
make ARCH=riscv64 defconfig
make ARCH=riscv64 AX_TESTCASE=nimbos BLK=y NET=y ACCEL=n run

# Run x86_64 example
make ARCH=x86_64 AX_TESTCASE=nimbos user_apps
# When running on a new architecture, you need to generate the configuration file again.
make ARCH=x86_64 defconfig
make ARCH=x86_64 AX_TESTCASE=nimbos BLK=y NET=y ACCEL=n run

# Run aarch64 example
make ARCH=aarch64 AX_TESTCASE=nimbos user_apps
make ARCH=aarch64 defconfig
make ARCH=aarch64 AX_TESTCASE=nimbos BLK=y NET=y ACCEL=n run

# Run Loongarch64 example
make ARCH=loongarch64 AX_TESTCASE=nimbos user_apps
make ARCH=loongarch64 defconfig
make ARCH=loongarch64 AX_TESTCASE=nimbos BLK=y NET=y ACCEL=n run

# Run another example (libc testcases)
make ARCH=riscv64 AX_TESTCASE=libc user_apps
make ARCH=riscv64 defconfig
# When running libc testcases, you need to enable `fp_simd` feature.
make ARCH=riscv64 AX_TESTCASE=libc BLK=y NET=y FEATURES=fp_simd ACCEL=n run
```

#### 4. Commands Explanation

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

Note: Arguments like `NET`, `BLK`, and `GRAPHIC` enable devices in QEMU, which take effect only at runtime, not at build time. More features can be found in the [Cargo.toml of arceos](https://github.com/oscomp/arceos/blob/main/ulib/axstd/Cargo.toml).

## Test for oscomp testcases

We can run [testcases of the OS competition](https://github.com/oscomp/testsuits-for-oskernel/tree/pre-2025) with StarryOS. Guidence can be found in [Starry-Tutorial](https://azure-stars.github.io/Starry-Tutorial-Book/ch01-04.html#在-os-比赛上测试-starry).