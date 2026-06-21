# StarryOS macOS AArch64 Self-Build

This app reproduces the StarryOS self-build flow on Apple Silicon macOS. The
final validation environment is Apple M3. The host first builds an AArch64
StarryOS seed kernel, boots it with QEMU HVF, runs Cargo directly inside the
StarryOS guest, extracts the guest-built kernel from the work rootfs, and boots
that kernel again with QEMU.

The macOS/HVF-specific flow stays under `apps/starry/macos-selfbuild`. Outside
this app, the only platform-facing interface is the generic AArch64 boot
argument pair:

```text
someboot.aarch64_timer=virtual someboot.aarch64_gicd_spi=off
```

Without these arguments, AArch64 keeps the default EL1 CNTP/physical timer path
and normal GICv3 distributor initialization.

## What The Flow Does

`full_self_build.sh` is the default full-flow entrypoint. It:

1. builds the StarryOS AArch64 seed kernel with `cargo xtask starry build`;
2. pulls the managed AArch64 Alpine rootfs with `cargo xtask image pull`;
3. resizes that managed rootfs with `cargo xtask image resize`;
4. prepares the app-local guest toolchain overlay;
5. copies the managed rootfs to a per-run work image;
6. injects the app overlay into that work image;
7. boots QEMU/HVF without `-snapshot`;
8. runs Cargo directly inside the StarryOS guest;
9. refreshes kallsyms and writes the guest-built kernel into the work rootfs;
10. extracts the ELF and `.bin` from the work rootfs with `debugfs`.

## Script Roles

| Script | Role | What it does |
| --- | --- | --- |
| `full_self_build.sh` | Full entrypoint | Wires together seed kernel build, rootfs input preparation, QEMU guest self-build, and artifact extraction. |
| `build_kernel.sh` | Stage 1 | Calls `cargo xtask starry build` on the host to build the AArch64 StarryOS seed kernel used for the first guest boot. It does not prepare the rootfs or launch QEMU. |
| `build_rootfs.sh` | Stage 2 | Prepares the managed AArch64 Alpine rootfs with `cargo xtask image pull/resize` and refreshes the guest toolchain overlay cache. It does not patch the managed image or launch QEMU. |
| `run_selfbuild.sh` | Stage 3 | Copies the managed rootfs to a per-run work image, calls `prebuild.sh` to assemble and inject the overlay, launches QEMU/HVF, starts the guest Cargo build, and extracts artifacts from the work image. Boot-only verification also reuses this script. |
| `prebuild.sh` | Internal script | Assembles the per-run overlay: copies the toolchain overlay, archives the current checkout, copies offline Cargo registry cache, and writes the guest runner plus source metadata. |
| `prepare_toolchain_overlay.sh` | Internal/debug script | Downloads and prepares guest Rust/Cargo, Rust source, LLVM/libclang, musl C tools, and Cargo cache. Its output is a filesystem tree, not a rootfs image. |
| `prepare_host_tools.sh` | Internal/debug script | Prepares AArch64 musl compiler wrappers plus tools such as `rust-nm` and `rust-objdump` for the macOS host seed-kernel build. |
| `guest-selfbuild.sh` | Guest-side script | Runs inside the StarryOS guest to unpack source, write Cargo config, run Cargo, refresh kallsyms, and copy guest-built kernel artifacts. |

The managed rootfs default path is:

```text
target/starry-macos-selfbuild/tgos-images/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img
```

It is not under `tmp/axbuild/rootfs`. The managed image stays clean; only the
per-run work copy under `target/starry-macos-selfbuild/rootfs/` is patched.

## Prerequisites

Install host tools on Apple Silicon macOS:

```bash
brew install qemu e2fsprogs zig llvm
```

The first run also needs network access for the managed rootfs, Alpine APKs,
Rust dist components, and Cargo registry archives required by `Cargo.lock`.
After the toolchain overlay is ready, guest Cargo runs offline.

## Full Reproduction

Run this from the repository root:

```bash
apps/starry/macos-selfbuild/full_self_build.sh
```

A successful self-build prints:

```text
===STARRY-MACOS-SELFBUILD-PASS jobs=4 elapsed=<seconds>===
```

The extracted artifacts are:

```text
target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat
target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat.bin
```

## M3 Validation Environment And Timing

The final validation run used this host:

```text
CPU: Apple M3
Memory: 16 GiB
System: macOS 15.6 (24G84), Darwin 24.6.0
QEMU: qemu-system-aarch64 with HVF
```

Command:

```bash
CASE_NAME=refactor-clean-full HOST_HEARTBEAT_SEC=60 \
  apps/starry/macos-selfbuild/full_self_build.sh
```

Observed stage timing:

| Stage | Time |
| --- | --- |
| StarryOS AArch64 seed-kernel Cargo build | `23.42s` |
| Guest Cargo build timing | Cargo printed `28m 29s`; the PASS marker printed `elapsed=1711`, or `28m 31s` |
| Boot-only verification of the guest-built kernel | about `3s` |

The PASS marker `elapsed` starts immediately before the guest runs
`cargo build` and ends after that Cargo command returns. It includes guest Cargo
build time, build scripts, build-std, and linking. It does not include host
rootfs preparation, QEMU boot time, post-Cargo kallsyms/artifact copying, or
host-side extraction.

The guest build used the full StarryOS Cargo graph for this configuration:

```text
Building ... 420/420
Finished `release` profile [optimized] target(s) in 28m 29s
===STARRY-MACOS-SELFBUILD-PASS jobs=4 elapsed=1711===
```

Boot-only verification of the self-built kernel reached:

```text
root@starry:/root #
===HOST-QEMU-STOP reason=boot-only-shell pid=11391 rc=0===
```

## Boot The Guest-Built Kernel

After the full self-build succeeds:

```bash
BOOT_ONLY=1 \
PREPARE_OVERLAY=0 REQUIRE_FRESH_ROOTFS=0 \
KERNEL=target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat.bin \
ROOTFS=target/starry-macos-selfbuild/tgos-images/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img \
SMP=4 JOBS=4 MEM=8192M QEMU_NET=0 QEMU_TIMEOUT_SEC=300 \
CASE_NAME=selfbuilt-boot-verify \
apps/starry/macos-selfbuild/run_selfbuild.sh
```

Successful boot-only verification reaches the StarryOS shell and writes:

```text
root@starry:
===HOST-QEMU-STOP reason=boot-only-shell ... rc=0===
```

## Reusing Prepared Inputs

Reuse the current rootfs and toolchain overlay, and only rerun QEMU:

```bash
ROOTFS_MODE=skip apps/starry/macos-selfbuild/full_self_build.sh
```

Prepare or refresh only the rootfs inputs:

```bash
apps/starry/macos-selfbuild/build_rootfs.sh
```

## Toolchain Overlay

The overlay is a filesystem tree, not a rootfs image:

```text
target/starry-macos-selfbuild/rootfs-build/toolchain-overlay
```

It is prepared from Alpine AArch64 APKs and official Rust dist components. It
contains the guest Rust/Cargo tools, Rust source, LLVM/libclang, musl C tools,
and offline Cargo registry cache. The app injects this tree into the copied work
rootfs before QEMU starts.

## Guest Cargo Build

The guest runs a direct Cargo build of StarryOS:

```text
cargo build -p starryos \
  --target apps/starry/macos-selfbuild/target-aarch64-unknown-none-softfloat-pie.json \
  -Z json-target-spec -Z host-config -Z target-applies-to-host \
  --bin starryos \
  -Z build-std=core,alloc \
  -Z build-std-features=compiler-builtins-mem \
  --features plat-dyn,ax-driver/virtio-blk,ax-driver/virtio-net,smp \
  --release
```

The validated self-build built `420/420` Cargo units.

## Important Knobs

| Variable | Default | Meaning |
| --- | --- | --- |
| `ROOTFS_MODE` | `build-rootfs` | Use `skip` to reuse prepared rootfs inputs. |
| `ROOTFS_SIZE_MIB` | `16384` | Managed rootfs size after `cargo xtask image resize`. |
| `TGOS_IMAGE_LOCAL_STORAGE` | `target/starry-macos-selfbuild/tgos-images` | xtask image storage root. |
| `SMP` | `4` | QEMU vCPU count. |
| `JOBS` | `$SMP` | Guest Cargo jobs. |
| `MEM` | `8192M` | QEMU memory size. |
| `QEMU_APPEND` | `someboot.aarch64_timer=virtual someboot.aarch64_gicd_spi=off` | Generic AArch64 platform boot arguments for macOS/HVF. |
| `QEMU_SNAPSHOT` | `0` | Self-build artifact extraction requires this to stay `0`. |
| `PREPARE_OVERLAY` | `1` | Build and inject the app overlay into the copied work rootfs. |
| `ARTIFACT_EXTRACT` | `1` | Extract the guest-built kernel from the work rootfs after QEMU exits. |
| `ARTIFACT_OUT_DIR` | `target/starry-macos-selfbuild/uploaded` | Host-side kernel artifact output directory. |
| `STARRY_KALLSYMS_RESERVED` | `16M` | Temporary linker reserve used before the guest kallsyms refresh. |

## Logs And Reports

Each run writes logs under:

```text
target/starry-macos-selfbuild/logs/
target/starry-macos-selfbuild/work/
```
