# QEMU Quickstart Guide

English | [中文](qemu-quickstart_cn.md)

This guide covers how to set up the AxVisor development environment locally and run different guest operating systems on QEMU.

## Prerequisites

- **OS**: Linux (native or WSL2)
- **Architecture**: x86_64 host

## 1. Install System Dependencies

```bash
sudo apt update && sudo apt install -y \
  build-essential gcc libssl-dev libudev-dev pkg-config \
  qemu-system-x86 qemu-system-arm qemu-system-misc \
  git curl wget
```

## 2. Install Rust Toolchain

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

Once you enter the project directory, Rust will automatically install the required nightly toolchain, components, and cross-compilation targets based on `rust-toolchain.toml` — no manual configuration needed.

Install additional Cargo tools:

```bash
cargo install cargo-binutils
cargo +stable install ostool --version '^0.8'
```

- `cargo-binutils`: provides `rust-objcopy`, `rust-objdump`, etc.
- `ostool`: custom build runner for AxVisor

## 3. KVM Setup (NimbOS x86_64 Only)

NimbOS runs on x86_64 QEMU and requires KVM hardware acceleration. ArceOS and Linux use AArch64 QEMU (TCG mode) and do not need KVM — you can skip this section.

Verify the KVM device exists:

```bash
ls -la /dev/kvm
```

Add your user to the `kvm` group:

```bash
sudo usermod -aG kvm $USER
```

Apply the group change in the current terminal without re-logging:

```bash
newgrp kvm
```

Verify:

```bash
id  # output should include "kvm"
```

## 4. Running Guest OSes

This branch provides a one-click setup script `scripts/setup_qemu.sh` that automatically downloads guest images, patches configuration paths, and prepares the rootfs.

### ArceOS (AArch64)

```bash
./scripts/setup_qemu.sh arceos

cargo xtask qemu \
  --config configs/board/qemu-aarch64.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml
```

Success indicator: `Hello, world!` appears in the output.

### Linux (AArch64)

```bash
./scripts/setup_qemu.sh linux

cargo xtask qemu \
  --config configs/board/qemu-aarch64.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs tmp/vmconfigs/linux-aarch64-qemu-smp1.generated.toml
```

Success indicator: `test pass!` appears in the output.

### NimbOS (x86_64, requires KVM)

```bash
./scripts/setup_qemu.sh nimbos

cargo xtask qemu \
  --config configs/board/qemu-x86_64.toml \
  --qemu-config .github/workflows/qemu-x86_64-kvm.toml \
  --vmconfigs tmp/vmconfigs/nimbos-x86_64-qemu-smp1.generated.toml
```

After booting, you will enter the Rust user shell (`>>` prompt). Type `usertests` to run the test suite. All tests passing will print `usertests passed!`

> **Note**: NimbOS requires VT-x/KVM. If `/dev/kvm` does not exist or has insufficient permissions, you will get a `Permission denied` error. WSL2 requires nested virtualization support in the kernel to use KVM.

### ArceOS (RISC-V64)

```bash
./scripts/setup_qemu.sh arceos-riscv64

cargo xtask qemu \
  --build-config configs/board/qemu-riscv64.toml \
  --qemu-config .github/workflows/qemu-riscv64.toml \
  --vmconfigs tmp/vmconfigs/arceos-riscv64-qemu-smp1.generated.toml
```

Success indicator: `Hello, world!` appears in the output.

`qemu-riscv64` currently supports the RISC-V ArceOS guest path. Cross-ISA boot such as `riscv64 AxVisor -> aarch64 ArceOS` is not wired up in the current hypervisor stack.

## 5. What Does setup_qemu.sh Do?

The script automates three steps, eliminating manual work:

1. **Download images**: calls `cargo axvisor image pull` to fetch and extract guest images to `/tmp/.axvisor-images/`
2. **Generate temp configs**: copies VM config templates to `tmp/vmconfigs/*.generated.toml`, then uses `sed` to update `kernel_path` (and `bios_path` for NimbOS) to actual image paths without modifying tracked files in `configs/vms/*.toml`
3. **Prepare rootfs**: copies `rootfs.img` to the project's `tmp/` directory for QEMU to use

You can also perform these steps manually if you prefer not to use the script.

## Troubleshooting

### `Path tmp/Image not found`

The `kernel_path` in the VM config points to a non-existent file. Run `./scripts/setup_qemu.sh <guest>` to automatically fix the paths.

### `Could not access KVM kernel module: Permission denied`

Your user is not in the `kvm` group. See the "KVM Setup" section above.

### `qemu-system-aarch64: command not found`

QEMU is not installed. Run the `apt install` command from Step 1.

### `Auto syncing from registry ... timed out`

This usually indicates unstable access to GitHub Raw endpoints. `cargo axvisor image pull` now handles registry bootstrap internally: it prefers the default registry, follows the included registry when present, and falls back to the built-in fallback registry (`v0.0.22.toml`) when the default endpoint is unavailable.

If your network is unstable for specific registry URLs, you can override the fallback registry:

```bash
export AXVISOR_REGISTRY_FALLBACK_URL="https://raw.githubusercontent.com/arceos-hypervisor/axvisor-guest/refs/heads/main/registry/v0.0.22.toml"
./scripts/setup_qemu.sh arceos
```

### First build is very slow

This is expected. AxVisor has many dependencies, and the first compilation needs to download and build all crates. Subsequent incremental builds will be much faster.
