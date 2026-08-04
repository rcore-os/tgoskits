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
cargo +stable install ostool --version '^0.15'
```

- `cargo-binutils`: provides `rust-objcopy`, `rust-objdump`, etc.
- `ostool`: custom build runner for AxVisor

## 3. KVM and UEFI Firmware Setup (NimbOS x86_64 Only)

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

The optional x86_64 UEFI guest path uses an external OVMF-compatible firmware image. On Debian/Ubuntu, install it with:

```bash
sudo apt install ovmf
```

If your firmware is not installed in a standard location, export:

```bash
export AXVISOR_X86_64_UEFI_FIRMWARE=/path/to/OVMF_CODE.fd
```

## 4. Running Guest OSes

> **Note**: All commands in this section are run from the **axvisor directory** (`os/axvisor/`). If you are in the tgoskits repository root, run `cd os/axvisor` first.

This branch provides a one-click setup script `scripts/quick-start.sh` that automatically downloads guest images, generates configs, builds, and launches QEMU.

### ArceOS (AArch64)

```bash
./scripts/quick-start.sh qemu-aarch64 start --arceos
```

ArceOS is a lightweight unikernel. It prints `Hello, world!` and exits immediately. After the guest exits, you land in the **AxVisor management shell** (`axvisor:/$`).

### Linux (AArch64)

```bash
./scripts/quick-start.sh qemu-aarch64 start --linux
```

You land in the **Linux guest's BusyBox interactive shell** (prompt `~ #`). Run `pkill qemu` from another terminal or close the QEMU window to exit.

### NimbOS (x86_64, requires KVM)

NimbOS images are not available through the standard registry. Use the two-step approach:

```bash
# Step 1: download images + generate configs
./scripts/setup_qemu.sh nimbos

# Step 2: copy the absolute-path command printed by the script
```

After booting, you enter the **Rust user shell** (`>>` prompt). Try commands like `usertests` to explore.

> **Note**: NimbOS requires VT-x/KVM. If `/dev/kvm` does not exist or has insufficient permissions, you will get a `Permission denied` error. WSL2 requires nested virtualization support in the kernel to use KVM.

### AxVisor Shell (LoongArch64, requires QEMU-LVZ)

```bash
./scripts/quick-start.sh qemu-loongarch64 start
```

This command launches AxVisor directly without booting a guest image. You enter the **AxVisor management shell** (`axvisor:/$`), preceded by `Welcome to AxVisor Shell!` in the output.

> **Note**: Stock `qemu-system-loongarch64` usually does not expose LoongArch virtualization extensions. Use `QEMU-LVZ`, or set `AXBUILD_QEMU_SYSTEM_LOONGARCH64=/path/to/qemu-system-loongarch64` to a validated virtualization-capable binary.

## 5. Step-by-Step Execution (for Development)

If you need to rebuild repeatedly without re-downloading images every time, split into two steps:

**Step 1**: Download images + generate configs (run once)

```bash
./scripts/setup_qemu.sh <guest>
# Example: ./scripts/setup_qemu.sh linux
```

**Step 2**: Build + launch (repeat as needed)

`setup_qemu.sh` prints the full `cargo xtask axvisor qemu` command with absolute paths — copy and paste it directly. Example:

```bash
cargo xtask axvisor qemu \
  --config /home/user/tgoskits/os/axvisor/configs/board/qemu-aarch64.toml \
  --qemu-config /home/user/tgoskits/os/axvisor/.github/workflows/qemu-aarch64.toml \
  --vmconfigs /home/user/tgoskits/os/axvisor/tmp/vmconfigs/linux-aarch64-qemu-smp1.generated.toml
```

`setup_qemu.sh` automates three steps:

1. **Download images**: calls `cargo xtask image pull` to fetch and extract guest images and rootfs to the axbuild image cache
2. **Generate temp configs**: copies VM config templates to `tmp/vmconfigs/*.generated.toml`, then uses `sed` to update `kernel_path` and firmware paths to actual image paths without modifying tracked files in `configs/vms/**/*.toml`
3. **Prepare rootfs**: copies the rootfs image to the project's `tmp/` directory for QEMU to use

## Troubleshooting

### `Path tmp/Image not found`

The `kernel_path` in the VM config points to a non-existent file. Run `./scripts/setup_qemu.sh <guest>` to automatically fix the paths.

### `Could not access KVM kernel module: Permission denied`

Your user is not in the `kvm` group. See the "KVM Setup" section above.

### `UEFI firmware image not found`

Install OVMF or set `AXVISOR_X86_64_UEFI_FIRMWARE` to the firmware image path before running `./scripts/setup_qemu.sh linux-x86_64-uefi`.

### `qemu-system-aarch64: command not found`

QEMU is not installed. Run the `apt install` command from Step 1.

### `Hardware support: false` followed by a panic on LoongArch64

AxVisor was launched with a LoongArch QEMU binary that does not provide virtualization extensions. Switch to `QEMU-LVZ`, or export `AXBUILD_QEMU_SYSTEM_LOONGARCH64` to point at a validated `qemu-system-loongarch64` binary before running `./scripts/quick-start.sh qemu-loongarch64 start`.

### `Auto syncing from registry ... timed out`

This usually indicates unstable access to GitHub Raw endpoints. `cargo xtask image pull` handles registry bootstrap internally and falls back to the built-in fallback registry when the default endpoint is unavailable.

### First build is very slow

This is expected. AxVisor has many dependencies, and the first compilation needs to download and build all crates. Subsequent incremental builds will be much faster.
