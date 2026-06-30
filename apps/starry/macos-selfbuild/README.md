# StarryOS macOS AArch64 Self-Build

This app reproduces the StarryOS self-build flow on Apple Silicon macOS, with
validation performed on Apple M3. The flow reuses the existing Starry app
runner: the host builds an AArch64 StarryOS seed kernel, host-side
`prebuild.sh` prepares the app overlay, the app runner injects that overlay and
boots the StarryOS guest with QEMU HVF, and Cargo then runs directly inside the
guest. After the build finishes, `full_self_build.sh` extracts the guest-built
kernel from the rootfs used by the app runner and boots that kernel again with a
normal QEMU command for verification.

## What The Flow Does

`full_self_build.sh` is the default full-flow entrypoint. The flow is organized
as these stages:

| Stage | Command / entrypoint | Purpose |
| --- | --- | --- |
| Stage 1 | `prepare_host_tools.sh` | Prepares macOS host wrappers needed by the AArch64 seed-kernel build. |
| Stage 2 | `cargo xtask starry app qemu -t macos-selfbuild --arch aarch64` | Uses the existing Starry app runner to build the seed kernel, ensure the rootfs, run `prebuild.sh`, inject the overlay through the internal `rootfs::inject::inject_overlay()` path, and launch QEMU/HVF. |
| Stage 2 / prebuild | `cargo xtask image resize <ROOTFS> --size-mib 16384` | Host-side `prebuild.sh` grows the rootfs selected by the app runner before overlay injection. |
| Stage 3 | QEMU/HVF guest Cargo build | After StarryOS boots, `shell_init_cmd` starts the guest runner, which runs `cargo build` directly inside the guest. |
| Stage 4 | `debugfs` artifact extraction | Extracts the guest-built kernel ELF and `.bin` from the app runner rootfs. |

## Script Roles

| Script | Role | What it does |
| --- | --- | --- |
| `full_self_build.sh` | Full entrypoint | Prepares host tools, runs the existing Starry app QEMU runner, and extracts guest-built artifacts from the rootfs after the runner succeeds. This is normally the only script to run directly. |
| `prebuild.sh` | Host-side app-runner prebuild | Runs on the host OS from the app runner. It receives `STARRY_ROOTFS` and `STARRY_OVERLAY_DIR`, resizes the selected rootfs, assembles the overlay, copies the toolchain overlay, archives the current checkout, copies offline Cargo registry cache, and writes the guest runner plus source metadata. It does not inject the overlay or launch QEMU. |
| `prepare_toolchain_overlay.sh` | Internal/debug script | Downloads and prepares guest Rust/Cargo, Rust source, LLVM/libclang, musl C tools, and Cargo cache. Its output is a filesystem tree, not a rootfs image, and it is invoked by `prebuild.sh` by default. |
| `prepare_host_tools.sh` | Internal/debug script | Prepares AArch64 musl compiler wrappers plus tools such as `rust-nm` and `rust-objdump` for the macOS host seed-kernel build. |
| `guest-selfbuild.sh` | Guest-side script | Runs inside the StarryOS guest to unpack source, write Cargo config, run Cargo, refresh kallsyms, and copy guest-built kernel artifacts. |

The rootfs is selected by axbuild image storage; this app does not maintain a
separate rootfs copy. In a clean default run, the path is:

```text
tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img
```

If `TGOS_IMAGE_LOCAL_STORAGE` is set, axbuild uses that storage instead. `prebuild.sh` records the exact rootfs used by the app runner in:

```text
target/starry-macos-selfbuild/rootfs.path
```

The app runner injects the self-build overlay into this selected rootfs through
the existing internal `rootfs::inject::inject_overlay()` path. This app does
not expose or depend on a new public injection command.

Because guest-built artifacts must be written back to the rootfs,
`qemu-aarch64.toml` sets `snapshot = false`, which prevents the Starry app
runner from appending the global `-snapshot` option. The standalone verification
command below still uses `-snapshot`; it only checks that the extracted `.bin`
boots and does not need to persist shell writes back to the rootfs.

## Prerequisites

Install host tools on Apple Silicon macOS:

```bash
brew install qemu e2fsprogs zig llvm
```

Here `qemu` provides the HVF VM, `e2fsprogs` provides `e2fsck`, `debugfs`, and
`resize2fs`, `zig` is used to generate AArch64 musl compiler wrappers, and
`llvm` is used as a fallback provider for tools such as `rust-nm` and
`rust-objdump`.

## Full Reproduction

### 1. Start self-build

Run this from the repository root:

```bash
apps/starry/macos-selfbuild/full_self_build.sh
```

This entrypoint calls `cargo xtask starry app qemu -t macos-selfbuild --arch
aarch64` itself, so the lower-level scripts normally do not need to be invoked
manually.

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
`full_self_build.sh` flow after deleting the `target` directory and the default
rootfs image, without using cached build outputs.

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
