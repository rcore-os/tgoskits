# TGOSKits

[![Build & Test](https://github.com/rcore-os/tgoskits/actions/workflows/test.yml/badge.svg)](https://github.com/rcore-os/tgoskits/actions/workflows/test.yml)

TGOSKits 是一个面向操作系统与虚拟化开发的集成仓库。它用 Git Subtree 管理 60+ 独立组件，并把 ArceOS、StarryOS、Axvisor 以及相关平台 crate 放在同一工作区中，方便做组件级开发、系统级联调和统一测试。

## 这个仓库适合谁

- 想先跑通 ArceOS、StarryOS 或 Axvisor 的新开发者
- 想直接修改 `components/` 并验证其在多个系统中的影响的贡献者
- 想在统一工作区里管理 Subtree、测试矩阵和系统集成的人

## 从哪里开始

| 你的目标 | 建议先看 | 最短命令 |
| --- | --- | --- |
| 第一次把仓库跑起来 | [docs/quick-start.md](docs/quick-start.md) | `cargo xtask arceos run --package arceos-helloworld --arch riscv64` |
| 理解命令入口、工作区和测试入口 | [docs/build-system.md](docs/build-system.md) | `cargo xtask test std` |
| 基于组件开发三个系统 | [docs/components.md](docs/components.md) | 从 `components/` 或 `os/arceos/modules/` 开始定位 |
| 开发 ArceOS 应用或模块 | [docs/arceos-guide.md](docs/arceos-guide.md) | `cargo xtask arceos run --package arceos-helloworld --arch riscv64` |
| 深入理解 ArceOS 的分层、feature 装配与启动路径 | [docs/arceos-internals.md](docs/arceos-internals.md) | `cargo xtask arceos run --package arceos-helloworld --arch riscv64` |
| 修改 StarryOS 内核或 rootfs 路径 | [docs/starryos-guide.md](docs/starryos-guide.md) | `cargo xtask starry rootfs --arch riscv64` |
| 深入理解 StarryOS 的 syscall、进程与 rootfs 装载路径 | [docs/starryos-internals.md](docs/starryos-internals.md) | `cargo xtask starry run --arch riscv64 --package starryos` |
| 运行或扩展 Axvisor | [docs/axvisor-guide.md](docs/axvisor-guide.md) | `cargo axvisor defconfig qemu-aarch64` |
| 深入理解 Axvisor 的 VMM、vCPU 与配置体系 | [docs/axvisor-internals.md](docs/axvisor-internals.md) | `cd os/axvisor && ./scripts/setup_qemu.sh arceos` |
| 管理组件来源和同步 | [docs/repo.md](docs/repo.md) | `python3 scripts/repo/repo.py list` |

## 仓库结构

```text
tgoskits/
├── components/                # subtree 管理的独立组件 crate
├── os/
│   ├── arceos/                # ArceOS: modules/api/ulib/examples
│   ├── StarryOS/              # StarryOS: kernel/starryos/make
│   └── axvisor/               # Axvisor: src/configs/local xtask
├── platform/                  # 平台相关 crate
├── test-suit/                 # ArceOS / StarryOS 系统测试
├── xtask/                     # 根目录 tg-xtask
├── scripts/
│   └── repo/                  # subtree 管理脚本与 repos.csv
└── docs/                      # 新开发者文档
```

最容易误解的一点是：`components/` 并不是按 `Hypervisor/ArceOS/Starry` 再分子目录。大多数组件直接平铺在 `components/` 下，类别信息主要来自 `scripts/repo/repos.csv`、根 `Cargo.toml` 和各系统对它们的依赖关系。

## 理解工作区关系

- 根 `Cargo.toml` 把常用组件、ArceOS 模块与示例、StarryOS 包、Axvisor、`platform/` 和 `xtask/` 放进一个统一 workspace。
- `os/arceos` 和 `os/StarryOS` 自己仍保留独立 workspace；根工作区通过 `members`、`exclude` 和 `[patch.crates-io]` 把需要的 crate 接进来。
- 一些目录本身是嵌套 workspace，比如 `components/axplat_crates`、`components/axdriver_crates`。这些目录通常不会直接作为根 workspace 成员加入，而是通过 patch 指向其中的具体 crate。

这意味着：

- 在仓库根目录开发，适合做跨系统联调和统一测试。
- 在 `os/arceos/`、`os/StarryOS/` 或 `os/axvisor/` 子目录开发，适合聚焦某一个系统的本地构建入口。

## 命令入口

| 位置 | 命令 | 说明 |
| --- | --- | --- |
| 仓库根目录 | `cargo xtask ...` | 根 `tg-xtask`，负责 ArceOS、StarryOS 和统一测试 |
| 仓库根目录 | `cargo arceos ...` | `cargo xtask arceos ...` 的别名 |
| 仓库根目录 | `cargo starry ...` | `cargo xtask starry ...` 的别名 |
| 仓库根目录 | `cargo axvisor ...` | 调用 `os/axvisor` 自带 xtask 的别名 |
| `os/arceos/` | `make ...` | ArceOS 的传统构建入口 |
| `os/StarryOS/` | `make ...` | StarryOS 的传统构建入口 |
| `os/axvisor/` | `cargo xtask ...` | Axvisor 本地 xtask，等价于根目录 `cargo axvisor ...` |

需要特别注意：

- 根目录的 `cargo xtask` 目前只有 `test`、`arceos`、`starry` 三类子命令。
- Axvisor 的构建与运行命令由 `os/axvisor` 自己的 xtask 提供，所以要么在根目录执行 `cargo axvisor ...`，要么进入 `os/axvisor/` 执行 `cargo xtask ...`。

## 5 分钟体验

```bash
git clone https://github.com/rcore-os/tgoskits.git
cd tgoskits

# ArceOS: 最快的 Hello World 路径
cargo xtask arceos run --package arceos-helloworld --arch riscv64

# StarryOS: 首次运行前先准备 rootfs
cargo xtask starry rootfs --arch riscv64
cargo xtask starry run --arch riscv64 --package starryos

# Axvisor: 推荐使用官方 setup 脚本准备 Guest 和 rootfs
cd os/axvisor
./scripts/setup_qemu.sh arceos
cargo xtask qemu \
  --build-config configs/board/qemu-aarch64.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml
```

Axvisor 不能只靠 `defconfig/build/qemu` 三条命令直接跑起来，因为默认 QEMU 配置会引用 `tmp/rootfs.img`。推荐先用 `os/axvisor/scripts/setup_qemu.sh` 自动准备 Guest 镜像、VM 配置和 rootfs，再运行 QEMU。完整说明见 [docs/axvisor-guide.md](docs/axvisor-guide.md)。

## 基于组件开发的最短闭环

1. 在 `components/`、`os/arceos/modules/`、`os/StarryOS/kernel/` 或 `os/axvisor/src/` 里找到你要修改的入口。
2. 先跑最小消费者，而不是一上来跑全量测试。
3. 改动稳定后，再补系统测试和 host 测试。

常用验证命令如下：

```bash
# host / std crate
cargo xtask test std

# ArceOS
cargo xtask arceos run --package arceos-helloworld --arch riscv64
cargo xtask test arceos --target riscv64gc-unknown-none-elf

# StarryOS
cargo xtask starry rootfs --arch riscv64
cargo xtask starry run --arch riscv64 --package starryos
cargo xtask test starry --target riscv64gc-unknown-none-elf

# Axvisor
cd os/axvisor
./scripts/setup_qemu.sh arceos
cargo xtask qemu \
  --build-config configs/board/qemu-aarch64.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml

# Axvisor 统一测试
cargo xtask test axvisor --target aarch64-unknown-none-softfloat
```

## Subtree 与组件来源

- 组件来源、类别和落地路径记录在 [scripts/repo/repos.csv](scripts/repo/repos.csv)。
- Subtree 的添加、拉取、推送和 CI 同步策略见 [docs/repo.md](docs/repo.md)。
- 新开发者如果只是“改组件并验证系统”，可以先读 [docs/components.md](docs/components.md)，不必先掌握全部 Subtree 细节。

## 进一步阅读

- [docs/quick-start.md](docs/quick-start.md): 第一天先把三个系统跑起来
- [docs/build-system.md](docs/build-system.md): 理解命令入口、workspace 和测试入口
- [docs/components.md](docs/components.md): 理解组件如何接入 ArceOS、StarryOS、Axvisor
- [docs/arceos-guide.md](docs/arceos-guide.md): ArceOS 的模块、API、平台与示例
- [docs/arceos-internals.md](docs/arceos-internals.md): ArceOS 的分层、feature 装配、启动流程与内部机制
- [docs/starryos-guide.md](docs/starryos-guide.md): StarryOS 的内核、rootfs 与 syscall 开发
- [docs/starryos-internals.md](docs/starryos-internals.md): StarryOS 的叠层架构、syscall 分发、进程与地址空间机制
- [docs/axvisor-guide.md](docs/axvisor-guide.md): Axvisor 的组件、板级配置与 VM 配置
- [docs/axvisor-internals.md](docs/axvisor-internals.md): Axvisor 的五层架构、VMM 启动链与 `axvisor_api` 机制

## 许可证

仓库整体采用 `Apache-2.0`，各组件可能带有自己的许可证文件，具体以各组件目录为准。
