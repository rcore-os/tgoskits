# StarryOS macOS AArch64 Self-Build

This app reproduces the StarryOS self-build flow on Apple Silicon macOS, with final validation performed on Apple M3. The host first builds an AArch64 StarryOS seed kernel, boots it with QEMU HVF, runs Cargo directly inside the StarryOS guest, extracts the guest-built kernel from the work rootfs, and boots that kernel again with QEMU for verification.

## What The Flow Does

`full_self_build.sh` is the default full-flow entrypoint. It executes:

1. builds the StarryOS AArch64 seed kernel with `cargo xtask starry build`;
2. pulls the managed AArch64 Alpine rootfs with `cargo xtask image pull`;
3. resizes the managed rootfs with `cargo xtask image resize`;
4. prepares the app-local guest toolchain overlay;
5. copies the managed rootfs to a per-run work image;
6. injects the app overlay into the work image with `cargo xtask image inject`;
7. boots QEMU/HVF without `-snapshot`;
8. runs Cargo directly inside the StarryOS guest;
9. refreshes kallsyms and writes the guest-built kernel into the work rootfs;
10. extracts the ELF and `.bin` from the work rootfs with `debugfs` on the host.

## Script Roles

| Script | Role | What it does |
| --- | --- | --- |
| `full_self_build.sh` | Full entrypoint | Wires together seed kernel build, rootfs input preparation, QEMU guest self-build, and artifact extraction. |
| `build_kernel.sh` | Stage 1 | Calls `cargo xtask starry build` on the host to build the AArch64 StarryOS seed kernel used for the first guest boot. Does not prepare the rootfs or launch QEMU. |
| `build_rootfs.sh` | Stage 2 | Prepares the managed AArch64 Alpine rootfs with `cargo xtask image pull/resize` and refreshes the guest toolchain overlay cache. Does not modify the managed image or launch QEMU. |
| `run_selfbuild.sh` | Stage 3 | Copies the managed rootfs to a per-run work image, calls `prebuild.sh` to assemble the overlay, injects it with `cargo xtask image inject`, launches QEMU/HVF, starts the guest Cargo build, and extracts artifacts from the work image. |
| `prebuild.sh` | Internal script | Assembles the per-run overlay: copies the toolchain overlay, archives the current checkout, copies offline Cargo registry cache, and writes the guest runner plus source metadata. |
| `prepare_toolchain_overlay.sh` | Internal/debug script | Downloads and prepares guest Rust/Cargo, Rust source, LLVM/libclang, musl C tools, and Cargo cache. Its output is a filesystem tree, not a rootfs image. |
| `prepare_host_tools.sh` | Internal/debug script | Prepares AArch64 musl compiler wrappers plus tools such as `rust-nm` and `rust-objdump` for the macOS host seed-kernel build. |
| `guest-selfbuild.sh` | Guest-side script | Runs inside the StarryOS guest to unpack source, write Cargo config, run Cargo, refresh kallsyms, and copy guest-built kernel artifacts. |

The managed rootfs default path:

```text
target/starry-macos-selfbuild/tgos-images/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img
```

It is not under `tmp/axbuild/rootfs`. The managed image stays clean; only the per-run work copy under `target/starry-macos-selfbuild/rootfs/` is modified.

## Prerequisites

Install host tools on Apple Silicon macOS:

```bash
brew install qemu e2fsprogs zig llvm
```

## Full Reproduction

### 1. Start self-build

Run this from the repository root:

```bash
apps/starry/macos-selfbuild/full_self_build.sh
```

A successful self-build prints:

```text
===STARRY-MACOS-SELFBUILD-PASS jobs=4 elapsed=<seconds>===
```

### 2. Boot the self-built kernel with QEMU

```bash
qemu-system-aarch64 \
  -snapshot \
  -machine virt,gic-version=3 \
  -nographic \
  -cpu cortex-a53 \
  -m 512M \
  -smp 1 \
  -device virtio-blk-pci,drive=disk0 \
  -drive id=disk0,if=none,format=raw,file=target/starry-macos-selfbuild/tgos-images/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img,file.locking=off \
  -kernel target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat.bin \
  -netdev user,id=net0
```

## Validation And Timing

This validation started from a clean state with `target` and `tmp` directories deleted.

The final validation run used this host:

```text
CPU: Apple M3
Memory: 16 GiB
System: macOS 15.6, Darwin 24.6.0
QEMU: qemu-system-aarch64 with HVF
```

Step 1: Start self-build

Command:

```bash
apps/starry/macos-selfbuild/full_self_build.sh
```

Observed stage timing:

| Stage | Time |
| --- | --- |
| StarryOS AArch64 seed-kernel Cargo build | incremental cache hit: Cargo printed `0.62s`, axbuild stage took `1.09s` |
| Guest Cargo build timing | Cargo printed `21m 47s`; the PASS marker printed `elapsed=1308`, or `21m 48s` |
| Direct QEMU boot verification of the guest-built kernel | about `1s` |

The PASS marker `elapsed` starts immediately before the guest runs `cargo build` and ends after that Cargo command returns. It includes guest Cargo build time, build scripts, build-std, and linking. It does not include host rootfs preparation, QEMU boot time, post-Cargo kallsyms/artifact copying, or host-side extraction.

Step 2: Boot the self-built kernel with QEMU

```bash
qemu-system-aarch64 \
  -snapshot \
  -machine virt,gic-version=3 \
  -nographic \
  -cpu cortex-a53 \
  -m 512M \
  -smp 1 \
  -device virtio-blk-pci,drive=disk0 \
  -drive id=disk0,if=none,format=raw,file=target/starry-macos-selfbuild/tgos-images/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img,file.locking=off \
  -kernel target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat.bin \
  -netdev user,id=net0
```

Boot verification of the self-built kernel reached:

```text
root@starry:/root #
```
