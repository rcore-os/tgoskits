# StarryOS

English | [中文](README_CN.md)

StarryOS is the Linux-compatible kernel integrated under `os/StarryOS` in the TGOSKits Cargo workspace. The repository root owns the workspace and the `cargo xtask` entry point, so build, run, and test commands in this guide are executed from the root of a TGOSKits checkout.

## 1. Workspace Role

StarryOS combines shared ArceOS runtime capabilities with Linux-oriented process, memory, signal, filesystem, and syscall layers. Its kernel and domain packages participate in the root workspace rather than forming a standalone workspace inside `os/StarryOS`.

### 1.1 Packages

The subsystem contains the workspace packages listed below. The kernel package provides the main Linux-compatible kernel implementation, the `starryos` package supplies the bootable target, and the signal and virtual-memory packages expose reusable domain abstractions to the kernel.

| Package | Directory | Role |
| --- | --- | --- |
| `starry-kernel` | `kernel/` | Linux-compatible kernel implementation |
| `starryos` | `starryos/` | Bootable StarryOS target |
| `starry-signal` | `signal/` | Signal-management abstraction |
| `starry-vm` | `vm/` | Virtual-memory abstraction |

### 1.2 Source Layout

Target definitions live in `configs/board/`. The checked-in QEMU configurations cover `qemu-aarch64`, `qemu-loongarch64`, `qemu-riscv64`, and `qemu-x86_64`; physical-board configurations are maintained alongside them. The `lkm/` directory contains loadable kernel modules, `docs/` stores subsystem notes, and `scripts/` contains StarryOS-specific helper scripts.

The repository-level `apps/starry/` tree contains applications and scenarios that run on top of the kernel. ArceOS-derived runtime, driver, filesystem, and network crates remain in their workspace directories and are selected through Cargo features.

## 2. Quick Start

The integrated workflow starts from a TGOSKits checkout. The root container image already provides the Rust toolchain, QEMU, and common cross-compilation dependencies used by CI.

### 2.1 Environment

Clone the integrated repository and enter its root:

```bash
git clone https://github.com/rcore-os/tgoskits.git
cd tgoskits
```

To use the project container, mount the checkout at `/workspace` and keep the repository root as the working directory:

```bash
docker pull ghcr.io/rcore-os/tgoskits-container:latest
docker run -it --rm \
  -v "$(pwd)":/workspace \
  -w /workspace \
  ghcr.io/rcore-os/tgoskits-container:latest
```

For a manual environment, follow the repository-level prerequisites in the root `README.md`. A QEMU run also requires the `qemu-system-<arch>` binary for the selected target.

### 2.2 Prepare and Run

Select a board configuration when the default target is not the one you need, prepare the managed rootfs, and start QEMU. The following example uses x86_64:

```bash
cargo xtask starry defconfig qemu-x86_64
cargo xtask starry rootfs --arch x86_64
cargo xtask starry qemu --arch x86_64
```

The same workflow applies to the other QEMU configurations:

```bash
cargo xtask starry defconfig qemu-aarch64
cargo xtask starry rootfs --arch aarch64
cargo xtask starry qemu --arch aarch64
```

`cargo xtask starry qemu` rebuilds when required. Use `--rootfs <IMAGE>` to select an explicit managed-image alias or path, and `--rootfs-write-policy persist` when changes to the guest disk need to survive a run.

### 2.3 Validation

Use the root task tool for validation rather than a native workspace-wide `cargo test` or `cargo clippy` invocation:

```bash
cargo xtask clippy --package starry-kernel
cargo xtask test
cargo xtask starry test qemu --arch x86_64
```

`cargo xtask test` runs the packages currently registered in the standard-library test whitelist. For committed changes, `cargo xtask test --since <REF>` narrows that selection to affected packages. QEMU tests exercise the selected runtime path, while package tests and static checks cover their own established boundaries.

## 3. Documentation

The integrated quick start and build references are maintained in the documentation site. Subsystem-specific notes, including OpenRC startup behavior, live under `docs/`.

- [StarryOS quick start](../../docs/docs/quickstart/starryos.md)
- [StarryOS build overview](../../docs/docs/build/starry/overview.md)
- [OpenRC startup notes](./docs/openrc.md)
- [Upstream project](https://github.com/Starry-OS/StarryOS)

## 4. License

StarryOS is released under the Apache License 2.0. See [LICENSE](./LICENSE) and [NOTICE](./NOTICE).
