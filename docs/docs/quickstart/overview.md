---
sidebar_position: 0
sidebar_label: "环境准备"
---

# 环境准备

本文是 ArceOS、StarryOS、Axvisor 快速上手的公共前置文档，重点说明当前仓库推荐的开发环境、QEMU 支持范围和统一命令入口。

## 1. 环境

本节给出快速上手所需的最小环境集合。目标不是覆盖所有开发场景，而是先确保常用 QEMU 路径和基础构建链路可以稳定运行。

### 1.1 最低要求

如果只是跟随快速上手文档完成首次启动，下面这组要求已经足够。后续若涉及板测、Guest 镜像或更复杂的系统调试，再按具体系统补充依赖。

| 项目 | 要求 |
|------|------|
| 操作系统 | Linux x86_64（推荐 Ubuntu 22.04+ / Debian 12+） |
| Rust 工具链 | 由仓库 `rust-toolchain.toml` 管理 |
| QEMU | 建议使用仓库容器镜像内置版本 |
| 磁盘空间 | 建议至少 20 GB（工具链、QEMU、构建产物、rootfs、Guest 镜像） |

### 1.2 推荐方式

对首次接触 TGOSKits 的开发者，容器方式通常是最省时间的选择。它可以直接复用仓库当前维护的测试环境，减少宿主机和 CI 之间的差异。

推荐直接使用仓库提供的容器环境。该环境与 CI 使用的基础镜像一致，已包含：

- QEMU
- Rust toolchain
- 交叉编译工具链
- `cmake`、`ninja`、`pkg-config` 等构建依赖

```bash
docker build -t tgoskits-env -f container/Dockerfile .
docker run -it --rm -v "$(pwd)":/workspace -w /workspace tgoskits-env
```

容器化测试环境详见：[测试基础设施与环境](../design/test/infrastructure)

### 1.3 手动安装

手动安装适合已经有本地工具链管理习惯，或者不方便使用容器的环境。建议把它视为容器方案的替代路径，而不是默认首选路径。

如果不使用容器，至少需要准备：

```bash
# 1. 安装 Rust（会按仓库 toolchain 自动切换）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 2. 安装基础构建工具（Ubuntu / Debian）
sudo apt update
sudo apt install -y cmake make ninja-build pkg-config

# 3. 安装常用 QEMU
sudo apt install -y qemu-system-arm qemu-system-riscv64 qemu-system-x86

# 4. 安装常用 Rust 辅助工具
cargo install cargo-binutils
```

> 手动安装适合已有本地环境的开发者；首次上手更建议直接使用容器。

## 2. QEMU 支持

三套系统的快速上手都依赖 QEMU，因此先明确当前主流支持的目标架构会更有帮助。这里列出的组合，都是仓库中已有现成命令和测试路径支撑的常用目标。

当前仓库主流快速上手路径覆盖以下架构：

| 架构 | 常见 Target Triple | 常用 QEMU |
|------|--------------------|-----------|
| `riscv64` | `riscv64gc-unknown-none-elf` | `qemu-system-riscv64` |
| `aarch64` | `aarch64-unknown-none-softfloat` | `qemu-system-aarch64` |
| `x86_64` | `x86_64-unknown-none` | `qemu-system-x86_64` |
| `loongarch64` | `loongarch64-unknown-none-softfloat` | `qemu-system-loongarch64` |

### 2.1 验证 QEMU

如果这些命令都能正常输出版本信息，通常说明宿主机上的 QEMU 安装已经满足快速上手的基本要求。若某个架构缺失，优先切换到容器环境会更省事。

```bash
qemu-system-riscv64 --version
qemu-system-aarch64 --version
qemu-system-x86_64 --version
qemu-system-loongarch64 --version
```

> 若某个架构的 QEMU 未安装，优先使用容器环境而不是在宿主机单独补齐。

## 3. 命令入口

TGOSKits 当前通过 `cargo xtask` 统一封装各系统的常用命令。无论是快速启动、测试套件还是镜像准备，优先从这一入口进入，通常最接近仓库当前的维护方式。

当前仓库推荐通过 `cargo xtask` 统一调度：

```bash
cargo xtask --help
```

常见入口如下：

| 目标 | 文档 | 常用命令 |
|------|------|----------|
| ArceOS | [ArceOS 快速上手](./arceos) | `cargo xtask arceos qemu ...` |
| StarryOS | [StarryOS 快速上手](./starryos) | `cargo xtask starry qemu ...` |
| Axvisor | [Axvisor 快速上手](./axvisor) | `cargo xtask axvisor test qemu ...` |

环境确认无误后，可以直接进入具体系统的快速上手页面。每一页都会给出当前项目中可用的最短命令路径，而不是抽象的概念说明：

- [ArceOS 快速上手](./arceos)
- [StarryOS 快速上手](./starryos)
- [Axvisor 快速上手](./axvisor)

如果需要先了解仓库整体结构，也可以继续阅读：[项目概览](../introduction/overview)
