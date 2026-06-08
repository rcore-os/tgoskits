# StarryOS macOS HVF Self-Build

This case is an Apple Silicon macOS reproduction workflow. The host runs QEMU
with HVF, boots an AArch64 StarryOS SMP guest, and runs guest `cargo build` to
build StarryOS again inside StarryOS.

The normal reproduction path builds the rootfs locally on macOS and does not use
Docker. `build_rootfs.sh` creates the AArch64 Alpine guest rootfs, installs the
pinned guest Rust/Cargo toolchain, prefetches the offline Cargo cache, and
injects the current TGOSKits source tree.

## What This Demonstrates

- host: Apple Silicon macOS;
- accelerator: QEMU HVF;
- guest kernel: StarryOS AArch64 SMP kernel;
- guest workload: StarryOS guest runs Cargo and builds the `starryos` binary;
- stable build shape: 8-vCPU guest with one Cargo job (`SMP=8 JOBS=1`);
- pass marker: `===STARRY-MACOS-SELFBUILD-PASS jobs=<N> elapsed=<seconds>===`.

The default reproduction proves that the StarryOS SMP guest can complete a
self-build while booted with 8 vCPUs. It does not claim that parallel guest
compilation is currently stable; keep `JOBS=1` for the graded reproduction.

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

The generated self-build rootfs contains the guest Rust/Cargo toolchain,
offline Cargo dependencies, and a TGOSKits source tarball. `check_rootfs.sh`
verifies the minimum paths.

## Reproduce From a Fresh Clone

Clone and enter the branch under test:

```bash
git clone https://github.com/yks23/tgoskits.git
cd tgoskits
git checkout app/starry-macos-selfbuild
```

Run the complete reproduction on macOS:

```bash
RUST_DIST_SERVER=https://rsproxy.cn \
STARRY_CARGO_REGISTRY_INDEX=sparse+https://rsproxy.cn/index/ \
apps/starry/macos-selfbuild/reproduce.sh
```

The mirror variables are optional. Omit them to use the official Rust and
crates.io endpoints, or set them to closer mirrors when `static.rust-lang.org`
or `index.crates.io` is slow.

`reproduce.sh` runs the three required steps: build or refresh the rootfs, build
the seed kernel, and launch the QEMU HVF self-build. On a base M1 with 8 GB
memory, close other heavy applications before running the 8-vCPU case; if it is
memory pressured, first verify the setup with:

```bash
SMP=4 JOBS=1 MEM=3072M \
RUST_DIST_SERVER=https://rsproxy.cn \
STARRY_CARGO_REGISTRY_INDEX=sparse+https://rsproxy.cn/index/ \
apps/starry/macos-selfbuild/reproduce.sh
```

The rootfs build downloads Alpine AArch64 APKs and Rust nightly components, then
runs `cargo fetch` on the host to populate the guest offline cache. The
`cargo fetch` phase may print only `Updating crates.io index` for several
minutes while the sparse registry cache is being populated.

For reproducibility, the default native rootfs payload is pinned to Alpine
`v3.23` and Rust `nightly-2026-05-28`. Do not switch `ALPINE_BRANCH=edge` for
the graded reproduction: Alpine edge currently provides a newer Cargo/Rust
package set and can change the build-std schedule substantially.

The expected output ends with:

```text
STARRY_MACOS_SELFBUILD_ROOTFS_OK
```

After every `git pull`, branch switch, or source edit, refresh the source
payload embedded in the rootfs before running QEMU:

```bash
apps/starry/macos-selfbuild/prepare_rootfs.sh
```

`run_selfbuild.sh` checks `/opt/tgoskits-src.meta` by default and exits early if
the rootfs was built from a different commit. This avoids accidentally running
an old rootfs for an hour. Set `REQUIRE_FRESH_ROOTFS=0` only for deliberate
stale-rootfs experiments.

If a prebuilt rootfs artifact is supplied, it can still be placed at the
standard path instead of rebuilding locally. Use either a local downloaded file:

```bash
apps/starry/macos-selfbuild/fetch_rootfs.sh \
  --input /path/to/rootfs-aarch64-hvf-selfbuild.img
```

or a direct artifact URL:

```bash
ROOTFS_URL='https://.../rootfs-aarch64-hvf-selfbuild.img' \
apps/starry/macos-selfbuild/fetch_rootfs.sh
```

Build the seed StarryOS kernel on macOS:

```bash
apps/starry/macos-selfbuild/build_kernel.sh
```

Run the complete 8-vCPU, single-Cargo-job self-build:

```bash
KERNEL=target/aarch64-unknown-none-softfloat/release/starryos.bin \
ROOTFS=tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img \
SMP=8 \
JOBS=1 \
RAYON_NUM_THREADS=1 \
SOURCE_TMPFS=0 \
QEMU_TIMEOUT_SEC=10800 \
apps/starry/macos-selfbuild/run_selfbuild.sh
```

A successful run prints:

```text
===STARRY-MACOS-SELFBUILD-FAST-PROFILE expected_crates~348 rustc_threads=1===
===STARRY-MACOS-SELFBUILD-PASS jobs=1 elapsed=<seconds>===
===STARRY-MACOS-SELFBUILD-RUN-END rc=0===
```

The fast reproducible profile should show:

```text
rustc_threads=1
features=plat-dyn,ax-feat/defplat,ax-feat/ipi,ax-feat/irq,ax-feat/rtc,cntv-timer,smp
```

If the log shows the old full-device feature set with `ax-feat/display`,
`ax-driver/virtio-*`, `starry-kernel/input`, or `starry-kernel/vsock`, it is the
slow experimental profile. The guest now refuses that profile unless
`ALLOW_SLOW_SELFBUILD=1` is explicitly set.

The host runner also refuses unexpectedly large Cargo totals by default. The
current fast profile is expected to report about `348` Cargo units. A much larger
total usually means a stale rootfs or slow feature set is being used; refresh the
rootfs from the current checkout and rerun the command above.

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

The generated rootfs is intentionally kept outside git because it is large. It
must contain:

```text
/usr/bin/cargo
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
`/opt/tgoskits-src.meta`. The host runner checks this metadata before booting
QEMU, and the guest checks it again when `TGOSKITS_COMMIT` is supplied.

## Important Knobs

| Variable | Default | Meaning |
| --- | --- | --- |
| `SMP` | `8` | QEMU vCPU count, passed to `-smp`. |
| `JOBS` | `SMP` | Guest Cargo job count. |
| `RAYON_NUM_THREADS` | `1` | Rayon worker limit for guest build scripts. |
| `RUSTC_THREADS` | empty | Optional guest `-Zthreads=<N>` override for local experiments. |
| `SOURCE_TMPFS` | `1` | Copy source into `/tmp` before building. |
| `QEMU_TIMEOUT_SEC` | `7200` | Host timeout; use `0` to disable. |
| `QEMU_ACCEL` | `hvf` | QEMU accelerator string. |
| `QEMU_MACHINE` | `virt,gic-version=3` | QEMU machine string. |
| `QEMU_CPU` | `host` | QEMU CPU model for HVF. |
| `BOOT_ONLY` | `0` | Stop after the shell prompt instead of starting Cargo. |
| `BUILD_TARGET` | `aarch64-unknown-none-softfloat` | Guest Cargo target. |
| `BUILD_PACKAGE` | `starryos` | Cargo package to build. |
| `BUILD_BIN` | `starryos` | Cargo binary to build; set `none` for library package diagnostics. |
| `FEATURES` | `plat-dyn,ax-feat/defplat,ax-feat/ipi,ax-feat/irq,ax-feat/rtc,cntv-timer,smp` | Feature-slim StarryOS build used by the fast reproducible self-build; set it to an empty string for single-crate diagnostics. |
| `REQUIRE_FRESH_ROOTFS` | `1` | Refuse a rootfs whose embedded source commit does not match the checkout. |
| `ALLOW_SLOW_SELFBUILD` | `0` | Permit the slow full-device feature profile only for explicit experiments. |
| `GUEST_MONITOR_INTERVAL_SEC` | `60` | Print guest `ps` snapshots while Cargo runs; set `0` to disable. |
| `EXTRA_RUSTFLAGS` | empty | Extra guest Rust flags for local experiments. |

## Rootfs Rebuild Details

```bash
apps/starry/macos-selfbuild/build_rootfs.sh
```

That helper creates an AArch64 Alpine payload natively on macOS, installs the
pinned guest Rust toolchain and offline Cargo cache, and injects it with
`debugfs`. It writes:

```text
tmp/axbuild/rootfs/rootfs-aarch64-hvf-toolchain.img
tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img
```

The script still accepts `--payload` or `ROOTFS_PAYLOAD_URL` for an externally
prepared payload tarball. `--build-payload-with-docker` is kept only as an
explicit maintainer fallback and is not used by the macOS reproduction path.
