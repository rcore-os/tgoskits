# 快速上手

本文档帮助开发者快速上手 TGOSKits 工作区，介绍 ArceOS、StarryOS 和 Axvisor 三个系统的基本运行路径及关键命令，并提供后续深入阅读的文档指引。本文档聚焦最常见的成功路径，不涵盖所有细节。

## 1. 命令入口

TGOSKits 工作区提供统一的命令入口管理 ArceOS、StarryOS 和 Axvisor 三个系统。ArceOS 和 StarryOS 主要从仓库根目录通过 `cargo xtask` 启动，而 Axvisor 既可在根目录通过 `cargo axvisor` 别名操作，也可进入 `os/axvisor/` 目录使用其独立的 `cargo xtask` 命令。

### 1.1 命令一览表

| 位置 | 命令 | 用途 |
| --- | --- | --- |
| 仓库根目录 | `cargo xtask ...` | 统一入口，负责 ArceOS、StarryOS 和测试 |
| 仓库根目录 | `cargo arceos ...` | `cargo xtask arceos ...` 的别名 |
| 仓库根目录 | `cargo starry ...` | `cargo xtask starry ...` 的别名 |
| 仓库根目录 | `cargo axvisor ...` | 调用 `os/axvisor` 本地 xtask 的别名 |
| `os/axvisor/` | `cargo xtask ...` | Axvisor 自己的构建与运行入口 |

若仅需记住一条规则：ArceOS 和 StarryOS 从仓库根目录启动；Axvisor 的构建和运行既可使用根目录 `cargo axvisor ...`，也可进入 `os/axvisor/` 执行 `cargo xtask ...`。

## 2. 环境配置

在构建和运行 TGOSKits 中的系统之前，需准备编译工具、Rust 工具链及 QEMU 仿真环境。建议预留至少 10GB 磁盘空间，用于首次下载 rootfs 或 Guest 镜像。

### 2.1 基础工具

以下为 Ubuntu/Debian 系统上的最小安装示例：

```bash
sudo apt update
sudo apt install -y \
    build-essential cmake clang curl file git libssl-dev libudev-dev \
    pkg-config python3 qemu-system-arm qemu-system-riscv64 qemu-system-x86 \
    xz-utils
```

### 2.2 Rust 工具链

TGOSKits 需要 Rust nightly 工具链，并支持多个目标平台的交叉编译。

安装 Rust 工具链并配置编译目标：

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

rustup target add riscv64gc-unknown-none-elf
rustup target add aarch64-unknown-none-softfloat
rustup target add x86_64-unknown-none
rustup target add loongarch64-unknown-none-softfloat
```

安装辅助工具：

```bash
cargo install cargo-binutils
cargo install ostool
```

### 2.3 可选：Musl 交叉工具链

若需为 StarryOS rootfs 或某些用户态程序编译静态二进制文件，需额外准备 Musl 交叉工具链。仅首次运行 ArceOS 示例时无需安装此工具链。

### 2.4 WSL2 环境说明

在 WSL2 环境下可正常使用 QEMU 进行纯软件仿真；通常不支持 KVM 或宿主机硬件虚拟化加速；遇到性能问题时，建议减少并行任务并避免依赖硬件加速选项。

## 3. 获取源码

使用 Git 克隆 TGOSKits 仓库到本地工作目录：

```bash
git clone https://github.com/rcore-os/tgoskits.git
cd tgoskits
```

## 4. 运行 ArceOS

ArceOS 是一个模块化的 Unikernel 操作系统，适合作为首个运行的示例。通过运行 helloworld 示例可验证工具链和 QEMU 环境是否正确配置。

### 4.1 最小示例

首先运行 helloworld 示例，确认工具链和 QEMU 可用：

```bash
cargo xtask arceos run --package arceos-helloworld --arch riscv64
```

### 4.2 功能示例

基本示例成功后，可尝试以下更具代表性的示例：

```bash
# 网络示例
cargo xtask arceos run --package arceos-httpserver --arch riscv64 --net

# 文件系统示例
cargo xtask arceos run --package arceos-shell --arch riscv64 --blk
```

### 4.3 架构选择

首次运行建议使用 `riscv64` 架构，其支持最为完善。熟悉基本流程后，可切换至 `x86_64`、`aarch64` 或 `loongarch64`。

## 5. 运行 StarryOS

StarryOS 是一个兼容 Linux 的操作系统内核，基于 ArceOS 构建。与 ArceOS 不同，StarryOS 在运行前需先准备 rootfs 镜像。

### 5.1 准备 rootfs

首次运行 StarryOS 前必须准备 rootfs 镜像。此步骤会将镜像下载并准备到目标产物目录中：

```bash
cargo xtask starry rootfs --arch riscv64
```

### 5.2 运行 StarryOS

准备完 rootfs 后即可运行 StarryOS：

```bash
cargo xtask starry run --arch riscv64 --package starryos
```

若使用 `os/StarryOS/Makefile` 路径，镜像位于 `os/StarryOS/make/disk.img`。

### 5.3 其他架构

熟悉基本流程后，也可尝试其他架构：

```bash
cargo xtask starry run --arch loongarch64 --package starryos
```

## 6. 运行 Axvisor

Axvisor 是一个 Type-1 Hypervisor，与前两个系统的区别在于：它并非单独运行一个内核，而是需要先准备 Guest 镜像，并通过板级配置引用对应的 VM 配置。推荐使用 QEMU AArch64 路径，当前仓库的预置配置和 CI 入口均围绕此路径。

### 6.1 环境准备

推荐使用 Axvisor 自带的 `setup_qemu.sh` 脚本，而非手动组合 `defconfig/build/qemu` 命令。该脚本会自动完成以下操作：

1. 下载并解压 Guest 镜像到 `/tmp/.axvisor-images/`
2. 生成 VM 配置文件 `tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml`
3. 复制 `rootfs.img` 到 `os/axvisor/tmp/rootfs.img`

```bash
cd os/axvisor
./scripts/setup_qemu.sh arceos
```

### 6.2 运行 QEMU

成功执行 `setup_qemu.sh` 后，使用以下命令启动 Axvisor 并运行 ArceOS Guest。注意：`tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml` 必须先通过 `setup_qemu.sh` 生成。

```bash
cd os/axvisor
cargo xtask qemu \
  --build-config configs/board/qemu-aarch64.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml
```

如果启动成功，ArceOS Guest 会输出 `Hello, world!`。

### 6.3 常见问题：defconfig/build/qemu 失败

若使用 `cargo axvisor defconfig`、`cargo axvisor build` 或 `cargo axvisor qemu` 时遇到失败，通常是因为默认 QEMU 配置模板会引用 `os/axvisor/tmp/rootfs.img` 文件。该文件不会通过 `cargo axvisor defconfig` 或 `cargo axvisor build` 自动生成，需手动准备或通过 `./scripts/setup_qemu.sh arceos` 创建。

### 6.4 自动化测试

除手动运行 QEMU 外，根工作区还提供统一的测试入口。该命令使用独立的测试逻辑，会自动下载所需镜像，无需手动准备 `os/axvisor/tmp/rootfs.img`：

```bash
cargo xtask test axvisor --target aarch64-unknown-none-softfloat
```

## 7. 开发验证

首次修改代码时，不建议直接运行全量测试。应优先选择距改动最近的消费者进行验证，确认基本功能正常后再考虑运行统一测试。

### 7.1 按改动位置选择验证路径

| 改动位置 | 先做什么 | 再做什么 |
| --- | --- | --- |
| `components/axerrno`、`components/kspin`、`components/percpu` 这类基础 crate | `cargo test -p <crate>` | 再跑一个最小 ArceOS 或 StarryOS 路径 |
| `os/arceos/modules/*` 或 `os/arceos/api/*` | `cargo xtask arceos run --package arceos-helloworld --arch riscv64` | 再补 `cargo xtask test arceos --target riscv64gc-unknown-none-elf` |
| `components/starry-*` 或 `os/StarryOS/kernel/*` | `cargo xtask starry rootfs --arch riscv64` | 再跑 `cargo xtask starry run --arch riscv64 --package starryos` |
| `components/axvm`、`components/axvcpu`、`components/axdevice`、`os/axvisor/src/*` | `cd os/axvisor && cargo xtask build` | 需要 Guest 时先运行 `./scripts/setup_qemu.sh arceos`，再执行 `cargo xtask qemu --build-config ... --qemu-config ... --vmconfigs ...` |

### 7.2 提交前验证

提交代码前，建议运行统一测试以确保改动未影响其他部分：

```bash
cargo xtask test std
cargo xtask test arceos --target riscv64gc-unknown-none-elf
cargo xtask test starry --target riscv64gc-unknown-none-elf
cargo xtask test axvisor --target aarch64-unknown-none-softfloat
```

## 8. 进阶学习

完成快速上手后，应根据接下来的工作重点选择相应的深入文档。以下针对不同学习目标推荐阅读文档：

| 你已经跑通了什么 | 下一篇建议文档 |
| --- | --- |
| 只想继续做 ArceOS 示例、模块或平台 | [arceos-guide.md](arceos-guide.md) |
| 想系统理解 ArceOS 的分层、feature 装配和启动路径 | [arceos-internals.md](arceos-internals.md) |
| 想改 StarryOS 内核、rootfs 或 syscall | [starryos-guide.md](starryos-guide.md) |
| 想系统理解 StarryOS 的 syscall、进程和 rootfs 装载链路 | [starryos-internals.md](starryos-internals.md) |
| 想搞清楚 Axvisor 的板级配置、VM 配置和虚拟化组件 | [axvisor-guide.md](axvisor-guide.md) |
| 想系统理解 Axvisor 的 VMM、vCPU 与配置生效路径 | [axvisor-internals.md](axvisor-internals.md) |
| 想从“组件”视角理解三个系统的关系 | [components.md](components.md) |
| 想理解工作区、xtask、Makefile 和测试矩阵 | [build-system.md](build-system.md) |

## 9. 常见问题

本节收集新手最常遇到的问题及其解决方案。

### 9.1 `rust-lld` 或目标工具链缺失

若遇到链接器错误或目标工具链缺失的问题，首先确认 Rust 目标已安装：

```bash
rustup target list --installed
```

若缺少对应目标，重新执行以下命令安装：

```bash
rustup target add riscv64gc-unknown-none-elf
rustup target add aarch64-unknown-none-softfloat
rustup target add x86_64-unknown-none
rustup target add loongarch64-unknown-none-softfloat
```

### 9.2 StarryOS 提示找不到 rootfs

这是 StarryOS 最常见的问题。先执行 rootfs 准备命令：

```bash
cargo xtask starry rootfs --arch riscv64
```

然后确认对应目标产物目录下的 `disk.img` 已生成。仅在本地 Makefile 路径下，才需检查 `os/StarryOS/make/disk.img`。

### 9.3 Axvisor 无法启动 Guest

优先检查以下两项：

1. `os/axvisor/tmp/rootfs.img` 是否已由 `./scripts/setup_qemu.sh arceos` 创建
2. `tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml` 是否已生成，且其中 `kernel_path` 指向真实存在的镜像文件

### 9.4 WSL2 下运行缓慢

WSL2 下运行缓慢通常由纯软件仿真导致，并非仓库配置问题。建议确保不依赖硬件加速，并从最小示例开始逐步验证。
