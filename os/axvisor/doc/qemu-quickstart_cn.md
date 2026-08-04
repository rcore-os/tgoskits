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

## 3. KVM 和 UEFI 固件配置（仅 NimbOS x86_64 需要）

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

x86_64 UEFI guest 需要 OVMF 固件。Debian/Ubuntu 安装：

```bash
sudo apt install ovmf
```

如果固件不在标准路径，导出：

```bash
export AXVISOR_X86_64_UEFI_FIRMWARE=/path/to/OVMF_CODE.fd
```

## 4. 运行客户机

> **注意**：本节所有命令均在 **axvisor 目录**（`os/axvisor/`）下执行。如果在 tgoskits 仓库根目录，请先运行 `cd os/axvisor`。

本项目提供 `scripts/quick-start.sh` 一键启动脚本，自动完成镜像下载、配置生成、编译和 QEMU 启动。

### ArceOS（AArch64）

```bash
./scripts/quick-start.sh qemu-aarch64 start --arceos
```

ArceOS 是一个轻量微内核，启动后打印 `Hello, world!` 随即退出。Guest 退出后你进入 **AxVisor 管理 Shell**（`axvisor:/$`），可以继续操作 hypervisor。

### Linux（AArch64）

```bash
./scripts/quick-start.sh qemu-aarch64 start --linux
```

启动后进入 **Linux Guest 的 BusyBox 交互 shell**（提示符 `~ #`）。在另一个终端执行 `pkill qemu` 或关闭 QEMU 窗口退出。

### NimbOS（x86_64，需要 KVM）

NimbOS 镜像不在标准 registry 中，须通过分步方式启动：

```bash
# 第 1 步：下载镜像 + 生成配置
./scripts/setup_qemu.sh nimbos

# 第 2 步：复制脚本打印的绝对路径命令，编译 + 启动
```

启动后进入 **Rust user shell**（`>>` 提示符），可输入 `usertests` 等命令运行测试

> **注意**：NimbOS 依赖 VT-x/KVM。如果 `/dev/kvm` 不存在或权限不足，会报 `Permission denied` 错误。WSL2 需要内核支持嵌套虚拟化才能使用 KVM。

### AxVisor Shell（LoongArch64，需要 QEMU-LVZ）

```bash
./scripts/quick-start.sh qemu-loongarch64 start
```

这条命令直接启动 AxVisor，不加载 guest 镜像。启动后进入 **AxVisor 管理 Shell**（`axvisor:/$`），提示符前会先看到 `Welcome to AxVisor Shell!`。

> **注意**：标准版 `qemu-system-loongarch64` 通常不暴露 LoongArch 虚拟化扩展。请使用 `QEMU-LVZ`，或设置 `AXBUILD_QEMU_SYSTEM_LOONGARCH64=/path/to/qemu-system-loongarch64` 指向已验证的二进制。

## 5. 分步执行（开发调试用）

如果需要反复编译调试但不想每次都重新下载镜像，可分两步：

**第 1 步**：下载镜像 + 生成配置（只需执行一次）

```bash
./scripts/setup_qemu.sh <guest>
# 示例: ./scripts/setup_qemu.sh linux
```

**第 2 步**：编译 + 启动（可重复执行）

`setup_qemu.sh` 执行后会打印完整的 `cargo xtask axvisor qemu` 命令，使用绝对路径，直接复制粘贴即可。示例：

```bash
cargo xtask axvisor qemu \
  --config /home/user/tgoskits/os/axvisor/configs/board/qemu-aarch64.toml \
  --qemu-config /home/user/tgoskits/os/axvisor/.github/workflows/qemu-aarch64.toml \
  --vmconfigs /home/user/tgoskits/os/axvisor/tmp/vmconfigs/linux-aarch64-qemu-smp1.generated.toml
```

`setup_qemu.sh` 自动完成以下三步：

1. **下载镜像**：调用 `cargo xtask image pull` 将 Guest 镜像和 rootfs 下载到 axbuild 镜像缓存
2. **生成临时配置**：复制模板 VM 配置到 `tmp/vmconfigs/*.generated.toml`，用 `sed` 更新 `kernel_path` 到实际镜像路径
3. **准备 rootfs**：将 rootfs 镜像复制到 `tmp/` 目录供 QEMU 使用

## 常见问题

### `Path tmp/Image not found`

VM 配置中的 `kernel_path` 指向了不存在的文件。运行 `./scripts/setup_qemu.sh <guest>` 会自动修正路径。

### `Could not access KVM kernel module: Permission denied`

当前用户不在 `kvm` 组中。参见上文「KVM 配置」一节。

### `qemu-system-aarch64: command not found`

未安装 QEMU。执行第 1 步的 `apt install` 命令。

### LoongArch64 下出现 `Hardware support: false`，随后 panic

当前使用的 LoongArch QEMU 二进制没有提供虚拟化扩展。请切换到 `QEMU-LVZ`，或导出 `AXBUILD_QEMU_SYSTEM_LOONGARCH64` 指向已验证的二进制。

### `Auto syncing from registry ... timed out`

访问 GitHub Raw 不稳定。`cargo xtask image pull` 内部处理 registry 引导逻辑，会自动回退到 fallback registry。

### 首次构建非常慢

正常现象。AxVisor 依赖较多，首次编译需要下载并编译所有 crate。后续增量编译会快很多。
