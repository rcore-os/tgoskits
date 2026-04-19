---
sidebar_position: 2
sidebar_label: "环境与平台"
---

# 环境与平台

TGOSKits 不是单一系统仓库，因此"支持什么硬件"需要从**开发宿主环境**、**目标架构**和**物理板级支持**三个维度分别说明。

## 开发环境

### 宿主要求

当前最稳妥的开发宿主环境是 **Linux x86_64**（推荐 Ubuntu 22.04+ 或 Debian 12+）。仓库同时提供两种环境准备方式：

| 方式 | 适用场景 | 说明 |
|------|---------|------|
| **手动安装** | 日常本地开发 | 通过 `rustup`、系统包管理器逐项安装 |
| **Container 镜像** | CI / 需要精确复现 CI 环境 | 基于 `container/Dockerfile` 构建，含预装 QEMU 和交叉工具链 |

### 工具链

工具链版本由仓库根目录 `rust-toolchain.toml` 锁定，`cargo` 会自动安装：

| 属性 | 值 |
|------|-----|
| 频道 | `nightly-2026-04-01` |
| Profile | `minimal`（仅安装必需组件） |
| 组件 | `rust-src`, `llvm-tools`, `rustfmt`, `clippy` |

**内置的交叉编译目标**（4 个）：

| Target Triple | 架构 | 浮点模式 |
|--------------|------|---------|
| `x86_64-unknown-none` | x86_64 | — |
| `riscv64gc-unknown-none-elf` | RISC-V 64 | 双精度硬浮点 (GC) |
| `aarch64-unknown-none-softfloat` | AArch64 | 软浮点 |
| `loongarch64-unknown-none-softfloat` | LoongArch64 | 软浮点 |

> 使用前请确保已执行 `rustup target list --installed` 确认上述 target 已安装。若使用 Container 镜像开发，这些 target 已预装。

### 外部依赖

| 类别 | 工具 | 用途 | 安装方式 |
|------|------|------|---------|
| 模拟器 | QEMU ≥ 10.2.1 | 系统级验证的主要执行环境 | 源码构建或发行版包（Container 内已预装 v10.2.1） |
| 辅助构建 | `cmake`, `make`, `ninja-build` | C 测试用例的交叉编译构建 | 系统 apt 包 |
| 辅助分析 | `cargo-binutils` | 二进制分析（`cargo size`, `cargo objdump` 等） | `cargo install cargo-binutils` |
| 镜像操作 | `ostool` | ELF/镜像格式转换与操作 | 仓库内建 |

完整手动安装步骤：[快速开始指南](/docs/design/reference/quick-start)

### 容器镜像

仓库在 `container/Dockerfile` 中定义了标准测试镜像，以 Ubuntu 24.04 为基础层：

| 内容 | 版本/说明 |
|------|----------|
| QEMU | 10.2.1 源码构建，覆盖 system + linux-user target |
| 交叉编译器 | aarch64 / riscv64 / x86_64 / loongarch64 的 musl 工具链 |
| Rust toolchain | 与 `rust-toolchain.toml` 一致 |
| 工作目录 | `/workspace`（与 GitHub Actions checkout 配合） |

对于 Axvisor LoongArch LVZ 场景，还有扩展镜像 `container/Dockerfile.axvisor-lvz`，额外包含 QEMU-LVZ 定制构建。

本地复用方式：

```bash
docker build -t tgoskits-test-env -f container/Dockerfile .
docker run -it --rm -v "$(pwd)":/workspace -w /workspace tgoskits-test-env
```

容器化设计详解：[测试基础设施与环境](../design/test/env)

## 目标架构

### 架构支持

| 目标架构 | Target Triple | QEMU 机器类型 | ArceOS | StarryOS | Axvisor | 成熟度 |
|----------|--------------|---------------|--------|----------|---------|--------|
| **RISC-V 64** | `riscv64gc-unknown-none-elf` | `-machine virt -cpu rv64` | ✅ 全量测试 | ✅ 全量测试 | 有配置占位 | **主要验证架构**，日常快速验证首选 |
| **AArch64** | `aarch64-unknown-none-softfloat` | `-cpu cortex-a53` | ✅ 全量测试 | ✅ 全量测试（含板级） | ✅ QEMU + 多板级 | **Axvisor 主要开发架构** |
| **x86_64** | `x86_64-unknown-none` | `-machine q35 -cpu max` | ✅ 全量测试 | ✅ 全量测试 | stub 实现 | 非当前首选体验路径 |
| **LoongArch64** | `loongarch64-unknown-none-softfloat` | `-machine virt -cpu la464` | ✅ 全量测试 | ✅ 全量测试 | LVZ 扩展镜像 | 实验性 |

> 所有架构的 ArceOS 和 StarryOS 均在 CI 的 Container 环境中运行完整 QEMU 测试矩阵。Axvisor 当前仅对 AArch64 提供完整的 QEMU + 板级测试覆盖。

### 验证路径

如果目标是最快确认环境可用，建议按以下顺序执行：

| 优先级 | 系统 | 命令 | 预期结果 | 前置条件 |
|--------|------|------|---------|---------|
| 1️⃣ | ArceOS | `cargo xtask arceos qemu --package ax-helloworld --arch riscv64` | QEMU 输出 "Hello, world!" 后退出 | 无（最短路径） |
| 2️⃣ | StarryOS | `cargo xtask starry qemu --target riscv64` | 启动 Shell 并执行冒烟命令 | 首次需准备 rootfs（可自动补齐） |
| 3️⃣ | Axvisor | `cargo xtask axvisor test qemu --target aarch64` | Guest 输出 "guest test pass!" | 需 Guest 镜像（可自动下载） |

### CI 测试矩阵

CI 中 Container 环境实际执行的测试命令：

```bash
# 主机端验证
cargo xtask test                    # 标准库单元测试（std_crates.csv 白名单，46+ 包）
cargo xtask clippy                  # Clippy 静态检查

# ArceOS QEMU 测试（4 架构）
cargo xtask arceos test qemu --target x86_64
cargo xtask arceos test qemu --target riscv64
cargo xtask arceos test qemu --target aarch64
cargo xtask arceos test qemu --target loongarch64

# StarryOS QEMU 测试（4 架构）
cargo xtask starry test qemu --target riscv64
cargo xtask starry test qemu --target aarch64
cargo xtask starry test qemu --target loongarch64
cargo xtask starry test qemu --target x86_64

# Axvisor QEMU 测试
cargo xtask axvisor test qemu --target aarch64

# 板级测试（self-hosted runner）
cargo xtask axvisor test board -t orangepi-5-plus-linux
cargo xtask axvisor test board -t roc-rk3568-pc-linux
cargo xtask starry test board -t smoke-orangepi-5-plus
```

> Stress 测试（`--stress`）当前为 CI 占位实现（`echo TODO!`），尚未接入正式执行流程。

## 板级支持

### Axvisor 开发板

Axvisor 通过硬编码的测试组管理板级支持，每组对应一块物理开发板：

| 开发板 | SoC | VM 配置 | Guest OS | 测试状态 |
|--------|-----|---------|----------|---------|
| **qemu-aarch64** | — (QEMU 模拟) | `linux-aarch64-qemu-smp1.toml` | Linux | ✅ CI 自动运行 |
| **qemu-riscv64** | — (QEMU 模拟) | `arceos-riscv64-qemu-smp1.toml` | ArceOS | 配置就绪 |
| **OrangePi-5-Plus** | Rockchip RK3588 | `linux-aarch64-orangepi5p-smp1.toml` | Linux | ✅ CI self-hosted |
| **phytiumpi** | 飞腾 E2000 | `linux-aarch64-e2000-smp1.toml` | Linux | ✅ CI self-hosted |
| **ROC-RK3568-PC** | Rockchip RK3568 | `linux-aarch64-rk3568-smp1.toml` | Linux | ✅ CI self-hosted |
| **RDK-S100** | — | `linux-aarch64-s100-smp1.toml` | Linux | ✅ CI self-hosted |

板级配置文件位于 `os/axvisor/configs/` 下：
- `board/<board_name>.toml` — 构建配置（编译选项、内核特性）
- `vms/<vm_config>.toml` — 虚拟机配置（内存、CPU 数量、设备列表）
- `board-test/<group>.toml` — 板级测试配置（串口参数、超时、判定正则）

### StarryOS 开发板

StarryOS 通过测试用例目录中的 `board-{board_name}.toml` 文件声明板级支持：

| 开发板 | 对应用例 | 说明 |
|--------|---------|------|
| **OrangePi-5-Plus** | `normal/smoke/` 下的 `board-orangepi-5-plus.toml` | 冒烟测试，CI self-hosted 运行 |

> StarryOS 的板级支持通过自动扫描 `test-suit/starryos/normal/*/board-*.toml` 发现，新增板级只需在对应用例目录添加配置文件即可。

### SoC 驱动

`drivers/` 目录包含针对特定 SoC 平台的驱动代码：

| 驱动 | 目标平台 | 功能 |
|------|---------|------|
| `rk3588-clk/` | Rockchip RK3588 | 时钟控制器驱动 |
| `rockchip-pm/` | Rockchip 系列 | 电源管理驱动 |
| `rockchip-npu/` | Rockchip 系列 | NPU（神经网络处理器）驱动 |

这些驱动服务于 OrangePi-5-Plus（RK3588）和 ROC-RK3568-PC 等 Rockchip 平台。

## 选型建议

| 目标 | 路径 | 理由 |
|---------|---------|------|
| **第一次跑通** | QEMU + RISC-V 64（ArceOS helloworld） | 无需额外准备，构建最快 |
| **改 ArceOS/StarryOS 内核** | QEMU + RISC-V 64 或 AArch64 | 两套系统的主要验证架构 |
| **改 Axvisor** | QEMU + AArch64 → OrangePi-5-Plus 板级 | AArch64 是 Axvisor 主要开发架构 |
| **做板级适配新硬件** | 参考现有 `configs/board/*.toml` + `drivers/` | 需同时准备构建配置、VM 配置和驱动代码 |
| **本地复现 CI 问题** | Container 镜像 (`container/Dockerfile`) | 与 CI 完全一致的工具链版本 |

## 延伸阅读

- [统一快速上手](/docs/quickstart/arceos-qemu)
- [测试基础设施与环境](../design/test/env) — Container 镜像设计与 CI 集成
- [测试套件体系总览](../design/test/test-suit-design) — 测试用例结构与发现机制
- [Arch / Target 映射](../design/build/arch) — 架构与编译目标的详细映射关系
- [配置体系总览](../design/guest-config/config-overview) — 板级配置与 VM 配置格式说明
