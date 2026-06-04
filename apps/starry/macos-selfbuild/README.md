# StarryOS macOS HVF Self-Build

This case documents a manual Apple Silicon macOS workflow for booting StarryOS
with QEMU/HVF and building StarryOS again inside the StarryOS guest.

It is an operator-facing app scenario, not a CI test. Rootfs images, kernels,
logs, and build outputs are local artifacts and are intentionally not committed.
The repository side is the reproducible runner and checklist; the large rootfs
with guest Rust/Cargo must be supplied as an external artifact.

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

The run script copies the input rootfs before booting. Keep at least one extra
rootfs image worth of free disk space under `target/starry-macos-selfbuild/`.

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

`prepare_rootfs.sh` writes `/opt/tgoskits-src.tar` and
`/opt/tgoskits-src.meta`. The guest script extracts that tarball when
`/opt/tgoskits/Cargo.toml` is not present, prints the source metadata, and checks
it against `TGOSKITS_COMMIT` when that variable is supplied.

## Run

Build or provide the AArch64 StarryOS kernel first, then run:

```bash
KERNEL=target/aarch64-unknown-none-softfloat/release/starryos.bin \
ROOTFS=tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img \
SMP=8 \
JOBS=8 \
SOURCE_TMPFS=1 \
QEMU_TIMEOUT_SEC=7200 \
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
| `QEMU_TIMEOUT_SEC` | `7200` | Host-side timeout for a stuck boot or build. Use `0` to disable it. |
| `BUILD_TARGET` | `aarch64-unknown-none-softfloat` | Guest Cargo target. |
| `BUILD_PACKAGE` | `starryos` | Cargo package to build. |
| `BUILD_BIN` | `starryos` | Cargo binary to build. |
| `FEATURES` | `qemu,gic-v3,cntv-timer,smp` | StarryOS features for the guest build. |
| `EXTRA_RUSTFLAGS` | empty | Extra guest Rust flags for local tuning experiments. |

To reproduce the fastest local profile, keep the same rootfs/kernel setup and
make the tuning knobs explicit:

```bash
KERNEL=target/aarch64-unknown-none-softfloat/release/starryos.bin \
ROOTFS=tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img \
SMP=8 \
JOBS=8 \
SOURCE_TMPFS=1 \
FEATURES='ax-feat/defplat,ax-feat/irq,ax-feat/ipi,ax-feat/rtc,ax-feat/bus-pci,gic-v3,cntv-timer,smp' \
CARGO_PROFILE_RELEASE_LTO=false \
CARGO_PROFILE_RELEASE_OPT_LEVEL=0 \
CARGO_PROFILE_RELEASE_CODEGEN_UNITS=256 \
apps/starry/macos-selfbuild/run_selfbuild.sh
```

## Representative Results

These are reference measurements from the local Apple Silicon HVF self-build
experiment. They document the intended scale of the demo; rerun locally for
authoritative numbers on a given machine. The table separates default
reproduction knobs from later local tuning, so it should not be read as a single
controlled benchmark.

| Case | Guest build knobs | Result |
| --- | --- | --- |
| slow guest baseline | `SMP=8`, `JOBS=1`, ext4 source/target | `951s` |
| first working SMP build | `SMP=8`, `JOBS=8`, ext4 source/target | `917s` |
| tmp target only | `SMP=8`, `JOBS=8`, target dir in `/tmp` | `660s` |
| tmp source and target | `SMP=8`, `JOBS=8`, source copy plus target dir in `/tmp` | `642s` |
| no LTO | `SMP=8`, `JOBS=8`, tmp source/target, `CARGO_PROFILE_RELEASE_LTO=false` | `515s` |
| opt0 plus high CGU | `SMP=8`, `JOBS=8`, no LTO, `OPT_LEVEL=0`, `CODEGEN_UNITS=256` | `427s` |
| tuned local best | `SMP=8`, `JOBS=8`, tmpfs source/target, tuned feature set, no LTO, `OPT_LEVEL=0`, `CODEGEN_UNITS=256` | `331s` |
| host reference | separate macOS host build used as a lower-bound reference, outside the guest | `134s` |

Speedup checkpoints from these local runs:

```text
slow guest baseline -> tuned local best: 951s / 331s = 2.87x
tmp source/target -> tuned local best: 642s / 331s = 1.94x
tuned JOBS=1 -> tuned JOBS=8: 422s / 331s = 1.28x
```

The `134s` host reference is not a guest self-build result and is not used in
the speedup ratios above. Treat it as a host-side lower bound, not as an
apples-to-apples AArch64 guest comparison.

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

## App Flow And QEMU Template

`prebuild.sh` makes `cargo xtask starry app qemu --app macos-selfbuild` usable
by generating an overlay with `/opt/starry-macos-run.sh`,
`/opt/starry-macos-selfbuild.sh`, `/opt/tgoskits-src.tar`, and
`/opt/tgoskits-src.meta`.

`qemu-aarch64-hvf.toml` mirrors the direct QEMU setup used by the host runner.
The direct runner remains preferred for long operator runs because it works on a
temporary rootfs copy and stops QEMU as soon as the PASS/FAIL marker appears.
