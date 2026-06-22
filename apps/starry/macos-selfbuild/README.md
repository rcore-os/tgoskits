# StarryOS macOS AArch64 Self-Build

This app reproduces the StarryOS self-build flow on Apple Silicon macOS, with final validation performed on Apple M3. The Starry app runner builds an AArch64 StarryOS seed kernel, prepares the app overlay, boots it with QEMU HVF, runs Cargo directly inside the StarryOS guest, extracts the guest-built kernel from the app runner rootfs, and boots that kernel again with QEMU for verification.

## What The Flow Does

`full_self_build.sh` is the default full-flow entrypoint. The flow is organized
as these stages:

| Stage | Command | Purpose |
| --- | --- | --- |
| Stage 1 | `prepare_host_tools.sh` | Prepares macOS host wrappers needed by the AArch64 seed-kernel build. |
| Stage 2 | `cargo xtask starry app qemu -t macos-selfbuild --arch aarch64` | Uses the existing Starry app runner to build the seed kernel, ensure the rootfs, run `prebuild.sh`, inject the overlay with the internal rootfs injector, and launch QEMU/HVF. |
| Stage 2 / prebuild | `cargo xtask image resize <ROOTFS> --size-mib 16384` | Grows the app runner rootfs before overlay injection. |
| Stage 3 | QEMU/HVF guest Cargo build | Boots StarryOS and runs `cargo build` directly inside the guest. |
| Stage 4 | `debugfs` artifact extraction | Extracts the guest-built kernel ELF and `.bin` from the app runner rootfs. |

## Script Roles

| Script | Role | What it does |
| --- | --- | --- |
| `full_self_build.sh` | Full entrypoint | Prepares host tools, runs the existing Starry app QEMU runner, and extracts guest-built artifacts after the runner succeeds. |
| `prebuild.sh` | App-runner prebuild | Resizes the selected app runner rootfs, assembles the overlay, copies the toolchain overlay, archives the current checkout, copies offline Cargo registry cache, and writes the guest runner plus source metadata. |
| `prepare_toolchain_overlay.sh` | Internal/debug script | Downloads and prepares guest Rust/Cargo, Rust source, LLVM/libclang, musl C tools, and Cargo cache. Its output is a filesystem tree, not a rootfs image. |
| `prepare_host_tools.sh` | Internal/debug script | Prepares AArch64 musl compiler wrappers plus tools such as `rust-nm` and `rust-objdump` for the macOS host seed-kernel build. |
| `guest-selfbuild.sh` | Guest-side script | Runs inside the StarryOS guest to unpack source, write Cargo config, run Cargo, refresh kallsyms, and copy guest-built kernel artifacts. |

The rootfs is selected by axbuild image storage. In a clean default run, the path is:

```text
tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img
```

If `TGOS_IMAGE_LOCAL_STORAGE` is set, axbuild uses that storage instead. `prebuild.sh` records the exact rootfs used by the app runner in:

```text
target/starry-macos-selfbuild/rootfs.path
```

The app runner injects the self-build overlay into this selected rootfs through the existing internal `rootfs::inject::inject_overlay()` path.

Because guest-built artifacts must be written back to the rootfs, `qemu-aarch64.toml` sets `snapshot = false`. The Starry app runner appends a global `-snapshot` by default, and this field lets a case explicitly disable it.

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
  -drive id=disk0,if=none,format=raw,file=tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img,file.locking=off \
  -kernel target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat.bin \
  -netdev user,id=net0
```

## Validation And Timing

This timing was measured on Apple M3 by running the default
`full_self_build.sh` flow after deleting the `rootfs` and `target` directory,
without using cached build outputs.

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

Observed self-build output timing:

```text
===STARRY-MACOS-SELFBUILD-PASS jobs=4 elapsed=1460===
```

The PASS marker elapsed time is `1460s` (`24m 20s`). It starts immediately
before the guest runs `cargo build` and ends after that Cargo command returns.
It includes guest Cargo build time, build scripts, build-std, and linking. It
does not include QEMU boot time, post-Cargo kallsyms/artifact copying, or
host-side extraction.

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
  -drive id=disk0,if=none,format=raw,file=tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img,file.locking=off \
  -kernel target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat.bin \
  -netdev user,id=net0
```

Boot verification of the self-built kernel reached:

```text
root@starry:/root #
```

This confirms that the guest-built `.bin` can boot as a normal StarryOS kernel.
