# 基于组件开发指南

TGOSKits 的核心价值不只是"把仓库放在一起"，而是让你可以从组件出发，一路追到 ArceOS、StarryOS 和 Axvisor 的实际消费者。这篇文档专门回答这个问题。

本文档旨在为开发者提供清晰的组件开发与集成指南，帮助理解 TGOSKits 的多层组件架构、判断改动的影响范围、选择正确的验证路径，以及掌握将新组件接入三套系统的标准流程。无论你是修改已有组件还是新增功能模块，这里都会告诉你应该从哪里开始、如何验证、以及最终如何将组件集成到目标系统中。

如果你已经知道目标 crate 的名字，建议和 [`docs/crates/README.md`](crates/README.md) 配合阅读：这里负责回答"它处在哪一层、通常影响谁"，crate 索引负责回答"它具体依赖谁、文档入口在哪"。

## 1. 组件不只在 `components/`

新开发者最容易误解的一点是：只有 `components/` 才算组件。实际上，TGOSKits 里至少有六类"组件化层次"，每一层都有其特定的角色定位和消费者群体。理解这些层次的划分，对于判断改动影响范围和选择正确的验证路径至关重要。

下面的表格总结了这六类组件层次的路径、角色、典型内容和主要消费者：

| 路径 | 角色 | 典型内容 | 主要消费者 |
| --- | --- | --- | --- |
| `components/` | subtree 管理的独立可复用 crate | `axerrno`、`kspin`、`axvm`、`starry-process` | 三套系统都可能直接或间接使用 |
| `os/arceos/modules/` | ArceOS 内核模块 | `axhal`、`axtask`、`axnet`、`axfs` | ArceOS，且经常被 StarryOS 和 Axvisor 复用 |
| `os/arceos/api/` | feature 与对外 API 聚合 | `axfeat`、`arceos_api` | ArceOS 应用、StarryOS、Axvisor |
| `os/arceos/ulib/` | 用户侧库 | `axstd`、`axlibc` | ArceOS 示例与用户应用 |
| `os/StarryOS/kernel/` | StarryOS 内核逻辑 | syscall、进程、内存、文件系统 | `starryos` 包 |
| `os/axvisor/` | Hypervisor 运行时与配置 | `src/`、`configs/board/`、`configs/vms/` | Axvisor |

除了上述六类核心组件层次，还有两个经常需要一起查看的目录：平台实现目录（`components/axplat_crates/platforms/*` 与 `platform/*`）和系统级测试入口（`test-suit/*`）。这些目录虽然不直接参与组件的分层架构，但在验证组件功能和系统集成时扮演着重要角色。

此外还有两个经常要一起看的目录：

- `components/axplat_crates/platforms/*` 与 `platform/*`：平台实现
- `test-suit/*`：系统级测试入口

## 2. 组件是怎样流到三个系统里的

理解组件的流向对于把握改动的影响范围至关重要。下面的流程图展示了从可复用 crate 到最终系统的典型路径：

```mermaid
flowchart TD
    ReusableCrate["components/* reusable crates"]
    ArceosModules["os/arceos/modules/*"]
    ArceosApi["os/arceos/api/* + os/arceos/ulib/*"]
    ArceosApps["ArceOS examples + test-suit/arceos"]
    StarryKernel["os/StarryOS/kernel + components/starry-*"]
    AxvisorRuntime["os/axvisor + components/axvm/axvcpu/axdevice/*"]
    PlatformCrates["components/axplat_crates/platforms/* + platform/*"]

    ReusableCrate --> ArceosModules
    ArceosModules --> ArceosApi
    ArceosApi --> ArceosApps
    ReusableCrate --> StarryKernel
    ArceosModules --> StarryKernel
    ReusableCrate --> AxvisorRuntime
    ArceosModules --> AxvisorRuntime
    PlatformCrates --> ArceosApps
    PlatformCrates --> StarryKernel
    PlatformCrates --> AxvisorRuntime
```

这张图的意思不是所有改动都要经过所有层，而是告诉你常见路径通常有三种：

1. 纯复用 crate 直接被系统包依赖  
   例如 `components/starry-process`、`components/axvm`

2. 先经过 ArceOS 模块层，再被上层系统消费  
   例如 `axhal`、`axtask`、`axdriver`、`axnet`

3. 通过平台和配置接到最终系统  
   例如 `axplat-*`、`platform/x86-qemu-q35`、Axvisor 的 `configs/board/*.toml`

## 3. 先判断你的改动应该落在哪

在开始修改之前，首先要明确你的改动属于哪个层次、会影响哪些系统，这样才能选择最合适的开发位置和验证策略。下面的表格根据你要改动的功能类型，推荐了优先查看的位置和常见的影响面：

| 你要改什么 | 优先看哪里 | 常见影响面 |
| --- | --- | --- |
| 通用基础能力：错误、锁、页表、Per-CPU、容器 | `components/axerrno`、`components/kspin`、`components/page_table_multiarch`、`components/percpu` | 三套系统都可能受影响 |
| ArceOS 内核服务：调度、HAL、驱动、网络、文件系统 | `os/arceos/modules/*`，以及相关 `axdriver_crates` / `axmm_crates` / `axplat_crates` | ArceOS，且可能波及 StarryOS / Axvisor |
| ArceOS 的 feature 或应用接口 | `os/arceos/api/axfeat`、`os/arceos/ulib/axstd`、`os/arceos/ulib/axlibc` | ArceOS 应用与上层系统 |
| StarryOS 的 Linux 兼容行为 | `components/starry-*`、`os/StarryOS/kernel/*` | StarryOS |
| Hypervisor、vCPU、虚拟设备、VM 管理 | `components/axvm`、`components/axvcpu`、`components/axdevice`、`components/axvisor_api`、`os/axvisor/src/*` | Axvisor |
| 平台、板级适配或 VM 启动配置 | `components/axplat_crates/platforms/*`、`platform/*`、`os/axvisor/configs/*` | 一到多个系统 |

如果你还不知道一个 crate 是谁维护、来自哪个独立仓库，先看 `scripts/repo/repos.csv`。它是所有 subtree 组件的来源总表。

## 4. 修改已有组件时，推荐的验证闭环

修改已有组件时，不应该一上来就运行完整的测试矩阵，而是应该采用渐进式验证策略：从最小的消费者开始，逐步扩大验证范围，最后再补充统一测试。这种方法既能快速发现问题，又能避免浪费时间在无关的测试上。

### 4.1 先找最近的消费者

不要一上来跑完整测试矩阵。先问自己：

- 这个 crate 是被哪个包直接依赖的
- 它是只影响一个系统，还是会同时影响多个系统
- 有没有比"启动整套系统"更小的验证入口

通常可以先看相关 `Cargo.toml`，再选择最小运行路径。

### 4.2 从最小路径开始

根据改动位置的不同，推荐的验证路径也有所差异。下表列出了不同类型改动的第一步和第二步验证方法：

| 改动位置 | 第一步验证 | 第二步验证 |
| --- | --- | --- |
| `components/axerrno`、`components/kspin`、`components/lazyinit` 这类基础 crate | `cargo test -p <crate>` | `cargo xtask arceos run --package arceos-helloworld --arch riscv64` |
| `os/arceos/modules/*` | `cargo xtask arceos run --package arceos-helloworld --arch riscv64` | 需要功能时换成 `arceos-httpserver --net` 或 `arceos-shell --blk` |
| `components/starry-*`、`os/StarryOS/kernel/*` | `cargo xtask starry run --arch riscv64 --package starryos` | `cargo xtask test starry --target riscv64gc-unknown-none-elf` |
| `components/axvm`、`components/axvcpu`、`components/axdevice`、`os/axvisor/src/*` | `cd os/axvisor && cargo xtask build` | 准备好 Guest 后运行 `./scripts/setup_qemu.sh arceos`，再执行 `cargo xtask qemu --build-config ... --qemu-config ... --vmconfigs ...` |

### 4.3 最后再补统一测试

在完成最小路径验证后，如果改动涉及跨系统基础组件，还需要运行统一测试以确保改动不会破坏其他系统。统一测试包括标准库测试、ArceOS 测试、StarryOS 测试和 Axvisor 测试：

```bash
cargo xtask test std
cargo xtask test arceos --target riscv64gc-unknown-none-elf
cargo xtask test starry --target riscv64gc-unknown-none-elf
cargo xtask test axvisor --target aarch64-unknown-none-softfloat
```

如果你改的是跨系统基础组件，至少要跑：

- 一条 host/`std` 路径
- 一条 ArceOS 路径
- 一条它真正影响到的系统路径

## 5. 新增组件

新增组件时，最重要的是先确定它应该属于哪一层，然后按照标准模板创建目录、配置文件和工作区接线。如果一开始就做对了，后续的维护和集成都会顺畅很多。

### 5.1 先选层次，再创建目录

先问自己这个新 crate 应该属于哪一层：

- 真正可复用的独立 crate：放 `components/`
- 仅属于 ArceOS 的 OS 模块：放 `os/arceos/modules/`
- 仅属于 ArceOS 的 API 或用户库：放 `os/arceos/api/` 或 `os/arceos/ulib/`
- 仅属于 StarryOS / Axvisor 的系统内部逻辑：优先放对应系统目录

### 5.2 标准目录结构

确认层次后，按照下面的标准结构创建组件目录。这是 `components/` 下独立 subtree crate 的推荐模板：

```
my_component/
├── Cargo.toml                  # Crate 元数据和依赖配置
├── rust-toolchain.toml         # Rust 工具链配置
├── LICENSE                     # 许可证文件
├── CHANGELOG.md                # 版本变更日志（可选）
├── README.md                   # 项目简介（英文）
├── README_CN.md                # README 中文版（可选）
├── .cargo/
│   └── config.toml             # Cargo 配置（默认 target、编译选项等）
├── .github/
│   ├── workflows/
│   │  ├── check.yml            # 代码检查工作流
│   │  ├── test.yml             # 测试工作流
│   │  ├── deploy.yml           # 文档部署工作流
│   │  ├── push.yml             # 同步到父仓库
│   │  └── release.yml          # 发布工作流
│   └── config.json             # 项目配置文件
├── scripts/                    # 实用脚本
│   ├── check.sh                # 代码检查（调用 axci/check.sh）
│   └── test.sh                 # 测试（调用 axci/tests.sh）
├── tests/                      # 集成测试文件
└── src/                        # 组件源码目录
    └── lib.rs                  # 库入口，导出公共 API
```

> **注意**：如果组件仅作为 TGOSKits 内部原型，不需要立即添加 `.github/`、`scripts/`、`tests/` 等 subtree CI 相关文件。只有在组件准备作为独立 subtree 仓库长期维护时才需要补齐。

### 5.3 配置文件模板

#### Cargo.toml

```toml
[package]
name = "my_component"
version = "0.1.0"
edition = "2024"
authors = ["作者"]
description = "组件描述"
license = "Apache-2.0"
repository = "https://github.com/org/repo"
keywords = ["os", "component"]
categories = ["embedded", "no-std"]

[dependencies]
```

在 TGOSKits 工作区内，更推荐直接复用根工作区的配置：

```toml
[package]
name = "my_component"
version = "0.1.0"
edition.workspace = true

[dependencies]
```

#### .github/config.json

CI/CD 流程读取的组件配置文件：

```json
{
  "targets": [
    "aarch64-unknown-none-softfloat"
  ],
  "rust_components": [
    "rust-src",
    "clippy",
    "rustfmt"
  ]
}
```

| 字段 | 说明 |
|------|------|
| `targets` | 编译目标平台列表 |
| `rust_components` | 需要安装的 Rust 组件 |

#### .cargo/config.toml

```toml
[target.aarch64-unknown-linux-gnu]
linker = "aarch64-linux-gnu-gcc"
runner = ["qemu-aarch64", "-L", "/usr/aarch64-linux-gnu"]
```

#### rust-toolchain.toml

```toml
[toolchain]
profile = "minimal"
channel = "nightly-2025-05-20"
components = ["rust-src", "llvm-tools", "rustfmt", "clippy"]
targets = ["aarch64-unknown-none-softfloat"]
```

### 5.4 把组件接到根 workspace

普通 leaf crate 常见需要两步：在根 `Cargo.toml` 的 `[workspace.members]` 里加入路径，以及在 `[patch.crates-io]` 里加入同名 patch，让其他包解析到本地源码：

```toml
[workspace]
members = [
    "components/my_component",
]

[patch.crates-io]
my_component = { path = "components/my_component" }
```

### 5.5 遇到嵌套 workspace 时不要照抄

`components/axplat_crates`、`components/axdriver_crates`、`components/axmm_crates` 这类目录本身是独立 workspace。给这类目录加新 crate 时，需要特别注意处理方式：

- 先在它自己的 workspace 里接好
- 再在根 `Cargo.toml` 里为具体 leaf crate 增加 patch 或 member
- 不要把整个父目录直接重新塞回根 workspace

### 5.6 什么时候需要改 `repos.csv`

只有当这个新组件本身要作为独立 subtree 仓库管理时，才需要把它加入 `scripts/repo/repos.csv`。如果你只是先在 TGOSKits 内部做原型，不一定要立刻动 subtree 配置。Subtree 的详细操作请参阅 [repo.md](repo.md)。

## 6. 把组件接到 ArceOS

在 ArceOS 里，组件开发常见的落地链路是：先在 `components/` 或 `os/arceos/modules/` 实现复用逻辑，如果要作为可选能力暴露则接到 `os/arceos/api/axfeat`，如果要给应用直接用则再接到 `os/arceos/ulib/axstd` 或 `axlibc`，最后用 `os/arceos/examples/*` 或 `test-suit/arceos/*` 验证。这个流程确保了组件能够以正确的层次接入系统，并提供适当的验证入口。

最常用的三个验证入口：

```bash
cargo xtask arceos run --package arceos-helloworld --arch riscv64
cargo xtask arceos run --package arceos-httpserver --arch riscv64 --net
cargo xtask arceos run --package arceos-shell --arch riscv64 --blk
```

什么时候要动哪层：

- 只改内部实现：通常只动 `components/` 或 `modules/`
- 要新增 feature 开关：动 `os/arceos/api/axfeat`
- 要新增应用侧 API：动 `os/arceos/ulib/axstd` 或 `axlibc`
- 要增加示例：动 `os/arceos/examples/`

## 7. 把组件接到 StarryOS

StarryOS 的一个关键点是：它既复用了大量 ArceOS 模块，也维护了自己的一套 `starry-*` 组件和 `os/StarryOS/kernel/` 内核逻辑。因此，在 StarryOS 中集成组件时，需要明确改动是通用基础能力、Linux 兼容行为，还是启动包和平台入口。

常见路径如下：

- 通用基础能力：先改 `components/*` 或 `os/arceos/modules/*`
- Linux 兼容行为：改 `components/starry-*` 或 `os/StarryOS/kernel/*`
- 启动包、特性组合、平台入口：改 `os/StarryOS/starryos`

验证时建议优先用根目录集成入口：

```bash
cargo xtask starry rootfs --arch riscv64
cargo xtask starry run --arch riscv64 --package starryos
```

如果你的改动会影响用户态行为，例如 syscall、文件系统或 rootfs 内程序，通常还要再做一层验证：

- 把测试程序放进 rootfs
- 或直接扩展 `test-suit/starryos`

## 8. 把组件接到 Axvisor

Axvisor 的组件化通常分成三层：复用 crate（如 `axvm`、`axvcpu`、`axdevice`、`axvisor_api`）、Hypervisor 运行时（`os/axvisor/src/*`）以及板级与 VM 配置（`os/axvisor/configs/board/*` 与 `os/axvisor/configs/vms/*`）。因此，做 Axvisor 相关改动时要特别注意区分这是"代码"改动还是"配置"改动，它影响的是 Hypervisor 本身还是 Guest 启动参数。

最小验证路径通常是：

```bash
cd os/axvisor
cargo xtask build
```

只有当 Guest 镜像、`tmp/rootfs.img` 和 `vmconfigs` 已经准备好时，再继续：

```bash
cd os/axvisor
./scripts/setup_qemu.sh arceos
cargo xtask qemu \
  --build-config configs/board/qemu-aarch64.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml
```

如果你改的是板级能力，还要一起看：

- `components/axplat_crates/platforms/*`
- `platform/x86-qemu-q35`
- `os/axvisor/configs/board/*.toml`

## 9. 测试与代码检查

组件通过 `scripts/` 下的脚本调用 [axci](https://github.com/arceos-hypervisor/axci) 统一测试框架。首次运行时自动下载 axci 到 `scripts/.axci/` 目录。

### 9.1 test.sh — 测试

```bash
# 运行全部测试（单元测试 + 集成测试）
./scripts/test.sh

# 仅运行单元测试
./scripts/test.sh unit

# 仅运行集成测试
./scripts/test.sh integration

# 列出所有可用的测试套件
./scripts/test.sh list

# 指定编译目标
./scripts/test.sh --targets aarch64-unknown-none-softfloat

# 指定测试套件（支持前缀匹配）
./scripts/test.sh integration --suite axvisor-qemu

# 仅显示将要执行的命令
./scripts/test.sh --dry-run -v

# 使用文件系统模式，不修改配置文件
./scripts/test.sh integration --suite axvisor-qemu-aarch64-arceos --fs

# 打印 U-Boot 和串口输出到终端
./scripts/test.sh integration --suite axvisor-qemu-aarch64-arceos --fs --print
```

**支持的测试类型**：

| 类型 | 说明 | 示例 |
|------|------|------|
| 单元测试 | `cargo test` 在宿主机运行 | `./scripts/test.sh unit` |
| QEMU 集成测试 | 在 QEMU 中启动 axvisor 运行客户机镜像 | `--suite axvisor-qemu-aarch64-arceos` |
| 开发板集成测试 | 通过 U-Boot 在物理开发板上运行 | `--suite axvisor-board-phytiumpi-arceos` |
| Starry 测试 | 构建并运行 StarryOS | `--suite starry-aarch64` |

### 9.2 check.sh — 代码检查

```bash
# 运行全部检查（格式 + clippy + 构建 + 文档）
./scripts/check.sh

# 仅运行指定阶段
./scripts/check.sh --only fmt
./scripts/check.sh --only clippy
./scripts/check.sh --only build

# 指定编译目标
./scripts/check.sh --targets aarch64-unknown-none-softfloat
```

## 10. CI/CD 与 GitHub Workflows

所有 CI 工作流通过调用 axci 的共享工作流实现（push.yml 除外）。

### check.yml — 代码检查

触发条件：push 到任意分支（tag 除外）、PR、手动触发

功能：格式检查 (rustfmt)、静态分析 (clippy)、编译检查、文档生成检查

```yaml
jobs:
  check:
    uses: arceos-hypervisor/axci/.github/workflows/check.yml@main
```

### test.yml — 集成测试

触发条件：push 到任意分支（tag 除外）、PR、手动触发

功能：运行单元测试、QEMU / 开发板集成测试

### release.yml — 发布

触发条件：push 版本 tag（`v*.*.*` 或 `v*.*.*-preview.*`）

流程：check + test 通过后执行 verify-tag → GitHub Release → crates.io publish

### deploy.yml — 文档部署

触发条件：push 稳定版 tag（`v*.*.*`，不含 `-preview.*`）

功能：生成 API 文档 (rustdoc) 并部署到 GitHub Pages

### push.yml — 同步到父仓库

触发条件：push 到 main 分支（此工作流为独立实现，直接复制自 axci，不使用 `uses:`）

功能：从父仓库 `scripts/repo/repos.csv` 定位组件 subtree 路径，执行 `git subtree pull`，创建或更新 PR

## 11. 什么时候需要看 `repo.md`

并不是所有的组件开发工作都需要深入了解 subtree 的细节。下面这些场景暂时不需要进入 subtree 细节：

- 修改已有组件源码
- 在 TGOSKits 里先做联调验证
- 只做根 workspace 内的依赖接线

但是，下面这些场景就应该去看 [repo.md](repo.md)：

- 新增一个要长期独立维护的 subtree 组件
- 需要同步组件仓库和主仓库
- 需要改 `scripts/repo/repos.csv`

## 12. 推荐阅读顺序

为了帮助开发者系统地掌握 TGOSKits 的组件开发和集成，建议按照以下顺序阅读相关文档：

- [quick-start.md](quick-start.md): 先把三套系统入口跑通
- [build-system.md](build-system.md): 再理解 workspace、xtask 和测试矩阵
- [arceos-guide.md](arceos-guide.md): 继续看 ArceOS 的模块与 API 关系
- [starryos-guide.md](starryos-guide.md): 继续看 StarryOS 的 rootfs、syscall 和内核入口
- [axvisor-guide.md](axvisor-guide.md): 继续看 Axvisor 的板级配置、VM 配置和 Guest 启动
