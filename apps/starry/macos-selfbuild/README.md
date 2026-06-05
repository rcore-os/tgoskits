# StarryOS macOS HVF Self-Build

This case is an Apple Silicon macOS reproduction workflow. The host runs QEMU
with HVF, boots an AArch64 StarryOS SMP guest, and runs guest `cargo build` to
build StarryOS again inside StarryOS.

The normal reproduction path does not build the rootfs locally and does not use
Docker. Use the prebuilt self-build rootfs artifact supplied with the PR or
release notes, then run the macOS/HVF runner below.

## What This Demonstrates

- host: Apple Silicon macOS;
- accelerator: QEMU HVF;
- guest kernel: StarryOS AArch64 SMP kernel;
- guest workload: StarryOS guest runs Cargo and builds the `starryos` binary;
- pass marker: `===STARRY-MACOS-SELFBUILD-PASS jobs=<N> elapsed=<seconds>===`.

Using AArch64/HVF keeps the guest ISA aligned with the Mac host CPU. This avoids
the cross-ISA TCG cost of RISC-V-on-macOS experiments and makes the self-build
result practical to reproduce on a laptop.

## Prerequisites

```bash
brew install qemu e2fsprogs zig llvm
```

The scripts use:

- `qemu-system-aarch64`;
- `debugfs` from Homebrew `e2fsprogs`;
- `zig` or an `aarch64-linux-musl-gcc` cross compiler for the seed kernel;
- `llvm-nm` and `llvm-objdump`, with Homebrew `llvm` used as fallback.

The self-build rootfs artifact must contain the guest Rust/Cargo toolchain,
offline Cargo dependencies, and either a TGOSKits source tree or source tarball.
`check_rootfs.sh` verifies the minimum paths.

## Reproduce From a Fresh Clone

Clone and enter the branch under test:

```bash
git clone https://github.com/yks23/tgoskits.git
cd tgoskits
git checkout app/starry-macos-selfbuild
```

Place the prebuilt self-build rootfs at the standard path. Use either a local
downloaded file:

```bash
apps/starry/macos-selfbuild/fetch_rootfs.sh \
  --input /path/to/rootfs-aarch64-hvf-selfbuild.img
```

or a direct artifact URL:

```bash
ROOTFS_URL='https://.../rootfs-aarch64-hvf-selfbuild.img' \
apps/starry/macos-selfbuild/fetch_rootfs.sh
```

The expected output ends with:

```text
STARRY_MACOS_SELFBUILD_ROOTFS_OK
```

Build the seed StarryOS kernel on macOS:

```bash
apps/starry/macos-selfbuild/build_kernel.sh
```

Run the complete 8-vCPU self-build:

```bash
KERNEL=target/aarch64-unknown-none-softfloat/release/starryos.bin \
ROOTFS=tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img \
SMP=8 \
JOBS=8 \
RAYON_NUM_THREADS=1 \
RUSTC_THREADS=1 \
SOURCE_TMPFS=1 \
QEMU_TIMEOUT_SEC=10800 \
apps/starry/macos-selfbuild/run_selfbuild.sh
```

A successful run prints:

```text
===STARRY-MACOS-SELFBUILD-PASS jobs=8 elapsed=<seconds>===
===STARRY-MACOS-SELFBUILD-RUN-END rc=0===
```

Logs are written under:

```text
target/starry-macos-selfbuild/logs/
```

The runner copies the input rootfs into
`target/starry-macos-selfbuild/rootfs/`, injects the guest runner scripts, and
uses QEMU `-snapshot`, so the input artifact is not modified.

## Quick Boot Check

To verify the rootfs and kernel reach the StarryOS guest shell without starting
the Cargo build:

```bash
KERNEL=target/aarch64-unknown-none-softfloat/release/starryos.bin \
ROOTFS=tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img \
SMP=8 \
BOOT_ONLY=1 \
QEMU_TIMEOUT_SEC=180 \
apps/starry/macos-selfbuild/run_selfbuild.sh
```

`BOOT_ONLY=1` is only a smoke test. Leave it unset for the full self-build.

## Rootfs Contract

The prebuilt rootfs is intentionally kept outside git because it is large. It
must contain:

```text
/usr/bin/cargo
/opt/rust-nightly/bin/cargo
/opt/rust-nightly/bin/rustc
/opt/rust-nightly/lib/rustlib/src/rust/library/Cargo.lock
/usr/bin/aarch64-linux-musl-gcc
/opt/rustc-nightly-sysroot
/opt/rustdoc-nightly-sysroot
/opt/tgoskits/Cargo.toml or /opt/tgoskits-src.tar
/root/.cargo/registry/index
/root/.cargo/registry/cache
```

Check an already placed rootfs with:

```bash
apps/starry/macos-selfbuild/check_rootfs.sh \
  tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img
```

If a prebuilt toolchain rootfs is available and only the source tree needs to be
refreshed, inject the current checkout without rebuilding the toolchain:

```bash
apps/starry/macos-selfbuild/prepare_rootfs.sh \
  --base-rootfs tmp/axbuild/rootfs/rootfs-aarch64-hvf-toolchain.img \
  --output-rootfs tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img
```

`prepare_rootfs.sh` writes `/opt/tgoskits-src.tar` and
`/opt/tgoskits-src.meta`. The guest checks the embedded commit when
`TGOSKITS_COMMIT` is supplied to the host runner.

## Important Knobs

| Variable | Default | Meaning |
| --- | --- | --- |
| `SMP` | `8` | QEMU vCPU count, passed to `-smp`. |
| `JOBS` | `SMP` | Guest Cargo job count. |
| `RAYON_NUM_THREADS` | `1` | Rayon worker limit for guest build scripts. |
| `RUSTC_THREADS` | `1` | Passed as guest `-Zthreads=<N>`. |
| `SOURCE_TMPFS` | `1` | Copy source into `/tmp` before building. |
| `QEMU_TIMEOUT_SEC` | `7200` | Host timeout; use `0` to disable. |
| `QEMU_ACCEL` | `hvf` | QEMU accelerator string. |
| `QEMU_MACHINE` | `virt,gic-version=3` | QEMU machine string. |
| `QEMU_CPU` | `host` | QEMU CPU model for HVF. |
| `BOOT_ONLY` | `0` | Stop after the shell prompt instead of starting Cargo. |
| `BUILD_TARGET` | `aarch64-unknown-none-softfloat` | Guest Cargo target. |
| `BUILD_PACKAGE` | `starryos` | Cargo package to build. |
| `BUILD_BIN` | `starryos` | Cargo binary to build. |
| `FEATURES` | `plat-dyn,cntv-timer,smp,ax-feat/display,ax-feat/rtc,ax-driver/virtio-blk,ax-driver/virtio-net,ax-driver/virtio-gpu,ax-driver/virtio-input,ax-driver/virtio-socket,starry-kernel/input,starry-kernel/vsock` | StarryOS features for the guest build. |
| `EXTRA_RUSTFLAGS` | empty | Extra guest Rust flags for local experiments. |

## Maintainer Rootfs Rebuild

This section is for maintainers who need to recreate the large rootfs artifact.
It is not part of the normal reproduction checklist.

```bash
apps/starry/macos-selfbuild/build_rootfs.sh
```

That helper creates an AArch64 Alpine payload, installs the pinned guest Rust
toolchain and offline Cargo cache, and injects it with `debugfs`. After the
image passes `check_rootfs.sh`, publish the resulting
`tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img` as an external artifact
for reviewers to use with `fetch_rootfs.sh`.
