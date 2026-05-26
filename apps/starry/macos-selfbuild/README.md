# StarryOS macOS HVF Self-Build

This case documents a manual Apple Silicon macOS workflow for booting StarryOS
with QEMU/HVF and building StarryOS again inside the StarryOS guest.

It is an operator-facing app scenario, not a CI test. Rootfs images, kernels,
logs, and build outputs are local artifacts and are intentionally not committed.

## What This Demonstrates

- host: Apple Silicon macOS with QEMU HVF;
- guest kernel: StarryOS AArch64 SMP kernel;
- guest workload: `cargo build` inside StarryOS builds the `starryos` binary;
- pass marker: `===STARRY-MACOS-SELFBUILD-PASS jobs=<N> elapsed=<seconds>===`.

Using AArch64/HVF keeps the guest ISA aligned with the Mac host CPU. This avoids
the large cross-ISA TCG cost from RISC-V-on-macOS experiments and makes SMP
performance work observable in minutes instead of hours.

## Host Prerequisites

```bash
brew install qemu e2fsprogs
```

The scripts expect these host tools:

- `qemu-system-aarch64`;
- `debugfs` from Homebrew `e2fsprogs`.

The runner also needs:

- an AArch64 StarryOS kernel binary, normally
  `target/aarch64-unknown-none-softfloat/release/starryos.bin`;
- a prepared ext4 rootfs image that contains guest Cargo/Rust and the TGOSKits
  source tree under `/opt/tgoskits`.

The rootfs should contain at least:

```text
/usr/bin/cargo
/opt/rustc-nightly-sysroot
/opt/rustdoc-nightly-sysroot
/opt/tgoskits/Cargo.toml or /opt/tgoskits-src.tar
```

Check a prepared rootfs before booting:

```bash
apps/starry/macos-selfbuild/check_rootfs.sh \
  tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img
```

If the base rootfs already contains the guest Rust/Cargo toolchain, inject the
current TGOSKits source tree with:

```bash
apps/starry/macos-selfbuild/prepare_rootfs.sh \
  --base-rootfs tmp/axbuild/rootfs/rootfs-aarch64-hvf-toolchain.img \
  --output-rootfs tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img
```

`prepare_rootfs.sh` writes `/opt/tgoskits-src.tar`. The guest script extracts
that tarball when `/opt/tgoskits/Cargo.toml` is not present.

## Run

Build or provide the AArch64 StarryOS kernel first, then run:

```bash
KERNEL=target/aarch64-unknown-none-softfloat/release/starryos.bin \
ROOTFS=tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img \
SMP=8 \
JOBS=8 \
SOURCE_TMPFS=1 \
EXTRA_RUSTFLAGS='-Z threads=2' \
apps/starry/macos-selfbuild/run_selfbuild.sh
```

The host runner copies the input rootfs into
`target/starry-macos-selfbuild/rootfs/`, injects the guest self-build scripts,
and boots QEMU with `-snapshot` so the boot run does not mutate the copied image.
The original input rootfs is not modified.

Logs are written under:

```text
target/starry-macos-selfbuild/logs/
```

The successful run prints:

```text
===STARRY-MACOS-SELFBUILD-PASS jobs=8 elapsed=<seconds>===
```

and the host side stops QEMU after seeing:

```text
===STARRY-MACOS-SELFBUILD-RUN-END rc=0===
```

## Important Knobs

| Variable | Default | Meaning |
| --- | --- | --- |
| `SMP` | `8` | QEMU vCPU count, passed to `-smp`. |
| `JOBS` | `SMP` | Guest `CARGO_BUILD_JOBS` and `RAYON_NUM_THREADS`. |
| `SOURCE_TMPFS` | `1` | Copy `/opt/tgoskits` into `/tmp` before building to reduce ext4 output pressure. |
| `BUILD_TARGET` | `aarch64-unknown-none-softfloat` | Guest Cargo target. |
| `BUILD_PACKAGE` | `starryos` | Cargo package to build. |
| `BUILD_BIN` | `starryos` | Cargo binary to build. |
| `FEATURES` | `qemu,gic-v3,cntv-timer,smp` | StarryOS features for the guest build. |
| `EXTRA_RUSTFLAGS` | empty | Extra guest Rust flags. The fastest local run used `-Z threads=2`. |

## Representative Results

These are reference measurements from the Apple Silicon HVF self-build
experiment. They are included to document the intended scale of the demo; rerun
locally for authoritative numbers on a given machine.

| Case | Guest build knobs | Result |
| --- | --- | --- |
| slow guest baseline | `SMP=8`, `JOBS=1`, ext4 source/target | `951s` |
| first working SMP build | `SMP=8`, `JOBS=8`, ext4 source/target | `917s` |
| tmpfs source/target | `SMP=8`, `JOBS=8`, `SOURCE_TMPFS=1` | `660s` |
| optimized fast build | `SMP=8`, `JOBS=8`, `SOURCE_TMPFS=1`, `EXTRA_RUSTFLAGS='-Z threads=2'` | `331s` |
| host oracle | macOS host build of the same kernel target | `134s` |

The main performance lesson is:

```text
T_build(N) = T_std/cache
           + T_serial(link/build.rs)
           + T_parallel_crates / N
           + T_fs(N)
           + T_smp(N)
           + T_wait(N)
```

The workflow proves that StarryOS can self-build under an SMP guest. It also
separates the remaining performance work into filesystem, scheduler/wakeup,
wait/pipe/process, and Cargo critical-path costs.

## QEMU Template

`qemu-aarch64-hvf.toml` mirrors the direct QEMU setup used by the host runner.
The direct runner is preferred because it can inject scripts into a temporary
rootfs copy and stop QEMU as soon as the PASS/FAIL marker appears.

The template remains useful for manual `cargo xtask starry qemu` experiments
after `/opt/starry-macos-selfbuild.sh` and `/opt/starry-macos-run.sh` have been
installed into the rootfs.
