# TGOSKits 快速上手指南

本文档旨在帮助开发者快速上手 TGOSKits 工作区，在第一次进入仓库时能够顺利跑通 ArceOS、StarryOS 和 Axvisor 的基本运行路径，并了解后续应该阅读哪些文档以深入理解项目。文档不会涵盖所有细节，而是聚焦于最常见的成功路径和关键命令。

## 1. 命令入口概览

TGOSKits 工作区提供了统一的命令入口来管理 ArceOS、StarryOS 和 Axvisor 三个系统。理解这些命令入口的位置和用途是快速上手的关键。ArceOS 和 StarryOS 主要从仓库根目录通过 `cargo xtask` 启动，而 Axvisor 既可以在根目录通过 `cargo axvisor` 别名操作，也可以进入 `os/axvisor/` 目录使用其独立的 `cargo xtask` 命令。

### 1.1 命令一览表

| 位置 | 命令 | 用途 |
| --- | --- | --- |
| 仓库根目录 | `cargo xtask ...` | 统一入口，负责 ArceOS、StarryOS 和测试 |
| 仓库根目录 | `cargo arceos ...` | `cargo xtask arceos ...` 的别名 |
| 仓库根目录 | `cargo starry ...` | `cargo xtask starry ...` 的别名 |
| 仓库根目录 | `cargo axvisor ...` | 调用 `os/axvisor` 本地 xtask 的别名 |
| `os/axvisor/` | `cargo xtask ...` | Axvisor 自己的构建与运行入口 |

### 1.2 记忆要点

如果你只想记住一条规则：ArceOS 和 StarryOS 主要从仓库根目录启动；Axvisor 的构建和运行既可以在根目录执行 `cargo axvisor ...`，也可以进入 `os/axvisor/` 执行 `cargo xtask ...`。

## 2. 环境准备

在开始构建和运行 TGOSKits 中的系统之前，需要准备基础的编译工具、Rust 工具链以及 QEMU 仿真环境。建议预留至少 10GB 磁盘空间，因为首次下载 rootfs 或 Guest 镜像时会额外占用一些空间。

### 2.1 基础工具

以下是在 Ubuntu/Debian 系统上的最小安装示例，包含了编译、构建和运行所需的基本工具：

```bash
sudo apt update
sudo apt install -y \
    build-essential cmake clang curl file git libssl-dev libudev-dev \
    pkg-config python3 qemu-system-arm qemu-system-riscv64 qemu-system-x86 \
    xz-utils
```

### 2.2 Rust 工具链

TGOSKits 需要使用 Rust 的 nightly 工具链，并支持多个目标平台的交叉编译。首先安装 Rust 工具链，然后添加所需的编译目标，最后安装一些常用的辅助工具。

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

如果你需要为 StarryOS rootfs 或某些用户态程序编译静态二进制文件，需要额外准备 Musl 交叉工具链。但如果只是首次跑通 ArceOS 示例，则不需要安装此工具链。

### 2.4 WSL2 开发提示

如果你在 WSL2 环境下开发，需要注意以下几点：可以正常使用 QEMU 进行纯软件仿真；通常不要指望 KVM 或宿主机硬件虚拟化加速可用；遇到性能问题时，优先减少并行任务和避免依赖硬件加速选项。

## 3. 克隆仓库

使用 Git 克隆 TGOSKits 仓库到本地工作目录：

```bash
git clone https://github.com/rcore-os/tgoskits.git
cd tgoskits
```

## 4. ArceOS 快速上手

ArceOS 是一个模块化的 Unikernel 操作系统，适合作为第一个跑通的示例。通过运行最小的 helloworld 示例，可以验证工具链和 QEMU 环境是否正确配置。

### 4.1 最小示例

首先运行最小的 helloworld 示例，确认工具链和 QEMU 是通的：

```bash
cargo xtask arceos run --package arceos-helloworld --arch riscv64
```

### 4.2 功能示例

在基本示例成功后，可以尝试两个更能体现功能差异的例子。网络示例展示了 ArceOS 的网络功能，文件系统示例展示了块设备支持：

```bash
# 网络示例
cargo xtask arceos run --package arceos-httpserver --arch riscv64 --net

# 文件系统示例
cargo xtask arceos run --package arceos-shell --arch riscv64 --blk
```

### 4.3 架构选择建议

首次上手建议固定使用 `riscv64` 架构，因为它的支持最为完善。等你熟悉基本流程后，再切换到 `x86_64`、`aarch64` 或 `loongarch64` 架构。

## 5. StarryOS 快速上手

StarryOS 是一个兼容 Linux 的操作系统内核，基于 ArceOS 构建。与 ArceOS 不同，StarryOS 在运行前需要先准备 rootfs 镜像。

### 5.1 准备 rootfs

StarryOS 第一次运行前必须先准备 rootfs 镜像。这一步会把 rootfs 镜像下载并准备到 StarryOS 的目标产物目录中，通常会生成对应目标下的 `disk.img`：

```bash
cargo xtask starry rootfs --arch riscv64
```

### 5.2 运行 StarryOS

准备完 rootfs 后，即可运行 StarryOS：

```bash
cargo xtask starry run --arch riscv64 --package starryos
```

如果你改走 `os/StarryOS/Makefile` 路径，才会使用 `os/StarryOS/make/disk.img`。

### 5.3 其他架构

如果你已经熟悉了基本流程，也可以尝试其他架构：

```bash
cargo xtask starry run --arch loongarch64 --package starryos
```

## 6. Axvisor 快速上手

Axvisor 是一个 Type-1 虚拟机监控器，与前两个系统最大的区别是：它不是单独跑一个内核，而是要先准备 Guest 镜像，并让板级配置引用对应的 VM 配置。推荐先使用 QEMU AArch64 路径，因为当前仓库里现成的板级配置和 CI 入口都围绕它。

### 6.1 环境准备

最稳妥的方式不是手工拼 `defconfig/build/qemu`，而是直接使用 Axvisor 仓库自带的 `setup_qemu.sh` 脚本。该脚本会自动完成三件事：下载并解压 Guest 镜像到 `/tmp/.axvisor-images/`、生成 VM 配置文件 `tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml`、复制 `rootfs.img` 到 `os/axvisor/tmp/rootfs.img`。

```bash
cd os/axvisor
./scripts/setup_qemu.sh arceos
```

### 6.2 运行 QEMU

成功运行 `setup_qemu.sh` 后，可以使用以下命令启动 Axvisor 并运行 ArceOS Guest。注意：`tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml` 文件必须先通过 `setup_qemu.sh` 脚本生成。

```bash
cd os/axvisor
cargo xtask qemu \
  --build-config configs/board/qemu-aarch64.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml
```

如果启动成功，ArceOS Guest 会输出 `Hello, world!`。

### 6.3 常见问题：defconfig/build/qemu 失败

如果你尝试使用 `cargo axvisor defconfig`、`cargo axvisor build` 或 `cargo axvisor qemu` 命令时遇到失败，通常是因为 Axvisor 默认使用的 QEMU 配置模板里会引用 `os/axvisor/tmp/rootfs.img` 文件。这个文件不会通过 `cargo axvisor defconfig` 或 `cargo axvisor build` 自动生成，只有你手工准备，或者运行 `./scripts/setup_qemu.sh arceos` 之后，它才会存在。

### 6.4 统一测试命令

除了手工运行 QEMU 外，根工作区还提供了统一的测试入口。这条命令会走自己的测试逻辑，并自动确保测试所需镜像被下载，不要求你手工准备 `os/axvisor/tmp/rootfs.img`：

```bash
cargo xtask test axvisor --target aarch64-unknown-none-softfloat
```

## 7. 开发闭环建议

第一次修改代码时，不要一上来跑全量测试。应该先选离你改动最近的消费者进行验证，确保基本功能正常后，再考虑运行统一测试。这样既能快速发现问题，又能节省大量编译和测试时间。

### 7.1 按改动位置选择验证路径

| 改动位置 | 先做什么 | 再做什么 |
| --- | --- | --- |
| `components/axerrno`、`components/kspin`、`components/percpu` 这类基础 crate | `cargo test -p <crate>` | 再跑一个最小 ArceOS 或 StarryOS 路径 |
| `os/arceos/modules/*` 或 `os/arceos/api/*` | `cargo xtask arceos run --package arceos-helloworld --arch riscv64` | 再补 `cargo xtask test arceos --target riscv64gc-unknown-none-elf` |
| `components/starry-*` 或 `os/StarryOS/kernel/*` | `cargo xtask starry rootfs --arch riscv64` | 再跑 `cargo xtask starry run --arch riscv64 --package starryos` |
| `components/axvm`、`components/axvcpu`、`components/axdevice`、`os/axvisor/src/*` | `cd os/axvisor && cargo xtask build` | 需要 Guest 时先运行 `./scripts/setup_qemu.sh arceos`，再执行 `cargo xtask qemu --build-config ... --qemu-config ... --vmconfigs ...` |

### 7.2 提交前的统一测试

当你准备提交代码前，应该运行统一测试以确保改动没有破坏其他部分：

```bash
cargo xtask test std
cargo xtask test arceos --target riscv64gc-unknown-none-elf
cargo xtask test starry --target riscv64gc-unknown-none-elf
cargo xtask test axvisor --target aarch64-unknown-none-softfloat
```

## 8. 后续学习路径

完成快速上手后，你应该根据接下来的工作重点选择相应的深入文档。以下是针对不同学习目标的文档推荐：

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

本节收集了新手最常遇到的问题及其解决方案，帮助你快速排查和解决常见错误。

### 9.1 `rust-lld` 或目标工具链缺失

如果遇到链接器错误或目标工具链缺失的问题，首先确认 Rust 目标已经安装：

```bash
rustup target list --installed
```

如果缺少对应目标，重新执行以下命令安装：

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

然后确认对应目标产物目录下的 `disk.img` 已生成。只有在本地 Makefile 路径下，才检查 `os/StarryOS/make/disk.img`。

### 9.3 Axvisor 启动不了 Guest

如果 Axvisor 无法启动 Guest，优先检查两件事：

1. `os/axvisor/tmp/rootfs.img` 是否已经由 `./scripts/setup_qemu.sh arceos` 准备好
2. `tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml` 是否已经生成，且其中 `kernel_path` 指向真实存在的镜像文件

### 9.4 在 WSL2 下速度很慢

在 WSL2 环境下运行缓慢通常不是仓库配置问题，而是纯软件仿真导致的。先确保你没有依赖硬件加速，再尽量从最小示例开始，不要第一次就跑最重的系统路径。
