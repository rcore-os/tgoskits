# QEMU 快速上手指南

[English](qemu-quickstart.md) | 中文

本文档介绍如何在本地搭建 AxVisor 的开发运行环境，并通过 QEMU 运行不同的客户机系统。

## 环境要求

- **操作系统**：Linux（原生 / WSL2 均可）
- **架构**：x86_64 宿主机

## 1. 安装系统依赖

```bash
sudo apt update && sudo apt install -y \
  build-essential gcc libssl-dev libudev-dev pkg-config \
  qemu-system-x86 qemu-system-arm qemu-system-misc \
  git curl wget
```

## 2. 安装 Rust 工具链

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

进入项目目录后，Rust 会根据 `rust-toolchain.toml` 自动安装所需的 nightly 工具链、组件和交叉编译目标，无需手动配置。

安装额外的 Cargo 工具：

```bash
cargo install cargo-binutils
cargo +stable install ostool --version '^0.15'
```

- `cargo-binutils`：提供 `rust-objcopy`、`rust-objdump` 等工具
- `ostool`：AxVisor 的自定义构建运行器

## 3. KVM 配置（仅 NimbOS x86_64 需要）

NimbOS 运行在 x86_64 QEMU 上并依赖 KVM 硬件加速。ArceOS 和 Linux 使用 AArch64 QEMU（TCG 模式），不需要 KVM，可跳过本节。

确认 KVM 设备存在：

```bash
ls -la /dev/kvm
```

将当前用户加入 `kvm` 组：

```bash
sudo usermod -aG kvm $USER
```

使组权限在当前终端立即生效（无需重新登录）：

```bash
newgrp kvm
```

验证：

```bash
id  # 输出应包含 "kvm"
```

## 4. 运行客户机

本分支提供了一键部署脚本 `scripts/setup_qemu.sh`，自动完成镜像下载、配置路径修改和 rootfs 准备。

### ArceOS（AArch64）

```bash
./scripts/setup_qemu.sh arceos

cargo xtask qemu \
  --config configs/board/qemu-aarch64.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml
```

启动成功标志：输出中出现 `Hello, world!`

### Linux（AArch64）

```bash
./scripts/setup_qemu.sh linux

cargo xtask qemu \
  --config configs/board/qemu-aarch64.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs tmp/vmconfigs/linux-aarch64-qemu-smp1.generated.toml
```

启动成功标志：输出中出现 `test pass!`

### NimbOS（x86_64，需要 KVM）

```bash
./scripts/setup_qemu.sh nimbos

cargo xtask qemu \
  --config configs/board/qemu-x86_64.toml \
  --qemu-config .github/workflows/qemu-x86_64-kvm.toml \
  --vmconfigs tmp/vmconfigs/nimbos-x86_64-qemu-smp1.generated.toml
```

启动成功后会进入 Rust user shell（`>>` 提示符），输入 `usertests` 运行测试套件，全部通过后输出 `usertests passed!`

> **注意**：NimbOS 依赖 VT-x/KVM。如果 `/dev/kvm` 不存在或权限不足，会报 `Permission denied` 错误。WSL2 需要内核支持嵌套虚拟化才能使用 KVM。

### ArceOS（RISC-V64）

```bash
./scripts/setup_qemu.sh arceos-riscv64

cargo xtask qemu \
  --build-config configs/board/qemu-riscv64.toml \
  --qemu-config .github/workflows/qemu-riscv64.toml \
  --vmconfigs tmp/vmconfigs/arceos-riscv64-qemu-smp1.generated.toml
```

启动成功标志：输出中出现 `Hello, world!`

当前 `qemu-riscv64` 这条快速启动链路支持的是 RISC-V 版 ArceOS Guest。像 `riscv64 AxVisor -> aarch64 ArceOS` 这样的跨 ISA 启动，在现有 hypervisor 栈里还没有接通。

## 5. setup_qemu.sh 做了什么

该脚本自动完成以下三步，省去手动操作：

1. **下载镜像**：调用 `cargo axvisor image pull` 将 Guest 镜像下载并解压到 `/tmp/.axvisor-images/`
2. **生成临时配置**：复制模板 VM 配置到 `tmp/vmconfigs/*.generated.toml`，并用 `sed` 更新 `kernel_path`（以及 NimbOS 的 `bios_path`）到实际镜像路径，不修改仓库内 `configs/vms/*.toml`
3. **准备 rootfs**：将 `rootfs.img` 复制到项目的 `tmp/` 目录下供 QEMU 使用

如果不想使用脚本，也可以手动执行上述步骤。

## 常见问题

### `Path tmp/Image not found`

VM 配置中的 `kernel_path` 指向了不存在的文件。运行 `./scripts/setup_qemu.sh <guest>` 会自动修正路径。

### `Could not access KVM kernel module: Permission denied`

当前用户不在 `kvm` 组中。参见上文「KVM 配置」一节。

### `qemu-system-aarch64: command not found`

未安装 QEMU。执行第 1 步的 `apt install` 命令。

### `Auto syncing from registry ... timed out`

这通常是访问 GitHub Raw 不稳定导致的。`cargo axvisor image pull` 现在会在命令内部处理 registry 引导逻辑：优先使用默认 registry，若其中声明了 include 就继续跟随 include；若默认入口不可用，则自动回退到内建 fallback registry（当前指向 `v0.0.25.toml`）。

如果你所在网络环境对部分 URL 不稳定，可显式覆盖 fallback registry：

```bash
export AXVISOR_REGISTRY_FALLBACK_URL="https://raw.githubusercontent.com/arceos-hypervisor/axvisor-guest/refs/heads/main/registry/v0.0.25.toml"
./scripts/setup_qemu.sh arceos
```

### 首次构建非常慢

正常现象。AxVisor 依赖较多，首次编译需要下载并编译所有 crate。后续增量编译会快很多。
