# TGOSKits

[![Build & Test](https://github.com/rcore-os/tgoskits/actions/workflows/test.yml/badge.svg)](https://github.com/rcore-os/tgoskits/actions/workflows/test.yml)

TGOSKits 是一个面向操作系统与虚拟化开发的集成仓库。它使用 Git Subtree 管理 60 多个独立组件仓库，并将 ArceOS、StarryOS、Axvisor 以及相关平台 crate 整合在同一工作区中，既支持组件级开发，又方便进行系统级联调和统一测试。

## 1. 目标用户

本仓库适合以下开发者使用：

- 想先跑通 ArceOS、StarryOS 或 Axvisor 的新开发者
- 想直接修改 `components/` 并验证其在多个系统中的影响的贡献者
- 想在统一工作区里管理 Subtree、测试矩阵和系统集成的人

## 2. 快速导航

根据你的开发目标，可以选择不同的入门路径。下表列出了常见目标对应的推荐文档和最短命令，帮助你快速定位到感兴趣的内容。

| 你的目标 | 建议先看 | 最短命令 |
| --- | --- | --- |
| 理解仓库组织结构和组件管理 | [docs/repo.md](docs/repo.md) | `python3 scripts/repo/repo.py list` |
| 第一次把仓库跑起来 | [docs/quick-start.md](docs/quick-start.md) | `cargo xtask arceos run --package arceos-helloworld --arch riscv64` |
| 理解命令入口、工作区和测试入口 | [docs/build-system.md](docs/build-system.md) | `cargo xtask test std` |
| 基于组件开发三个系统 | [docs/components.md](docs/components.md) | 从 `components/` 或 `os/arceos/modules/` 开始定位 |
| 按 crate 维度系统学习仓库 | [docs/crates/README.md](docs/crates/README.md) | 先看批次总览，再跳到具体 crate 文档 |
| 开发 ArceOS 应用或模块 | [docs/arceos-guide.md](docs/arceos-guide.md) | `cargo xtask arceos run --package arceos-helloworld --arch riscv64` |
| 深入理解 ArceOS 的分层、feature 装配与启动路径 | [docs/arceos-internals.md](docs/arceos-internals.md) | `cargo xtask arceos run --package arceos-helloworld --arch riscv64` |
| 修改 StarryOS 内核或 rootfs 路径 | [docs/starryos-guide.md](docs/starryos-guide.md) | `cargo xtask starry rootfs --arch riscv64` |
| 深入理解 StarryOS 的 syscall、进程与 rootfs 装载路径 | [docs/starryos-internals.md](docs/starryos-internals.md) | `cargo xtask starry run --arch riscv64 --package starryos` |
| 运行或扩展 Axvisor | [docs/axvisor-guide.md](docs/axvisor-guide.md) | `cargo axvisor defconfig qemu-aarch64` |
| 深入理解 Axvisor 的 VMM、vCPU 与配置体系 | [docs/axvisor-internals.md](docs/axvisor-internals.md) | `cd os/axvisor && ./scripts/setup_qemu.sh arceos` |

## 3. 仓库结构

TGOSKits 采用了清晰的目录组织结构，将组件、操作系统、平台和工具分别放置在不同的目录中。需要注意的是，`components/` 目录并不是按 `Hypervisor/ArceOS/Starry` 再分子目录，大多数组件直接平铺在 `components/` 下，类别信息主要来自 `scripts/repo/repos.csv`、根 `Cargo.toml` 和各系统对它们的依赖关系。

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

## 4. 按 Crate 学习仓库

如果你想系统地理解这个仓库里每个 crate 的定位、边界和它们如何流转到 ArceOS / StarryOS / Axvisor，而不是仅仅停留在"先跑起来"阶段，那么最直接的入口是 [docs/crates/README.md](docs/crates/README.md)。这份文档提供了按 crate 维度的索引、批次分类和推荐阅读顺序。

### 4.1 文档关系

这份总览和本 README、[docs/components.md](docs/components.md) 的关系可以这样理解：根 `README.md` 回答"仓库怎么跑、怎么进入三个系统"；[docs/components.md](docs/components.md) 回答"组件处在哪一层、通常被谁消费"；[docs/crates/README.md](docs/crates/README.md) 回答"具体某个 crate 做什么、依赖谁、该按什么顺序读"。

### 4.2 使用方式

建议按下面三种方式使用 crates 文档：

| 你的目的 | 建议先看 `docs/crates/README.md` 的哪一部分 | 适合接着看什么 |
| --- | --- | --- |
| 已经知道 crate 名字，想直接查技术文档 | `文档索引` | 直接跳到对应 `docs/crates/<name>.md` |
| 想按主线系统学习，而不是按目录乱看 | `手工精修批次` 与 `批次与三大系统子系统对照` | 再沿 `按批次推荐阅读与快速跳转` 顺序读 |
| 想补某条能力链，比如平台、驱动、文件系统、虚拟化 | `按批次推荐阅读与快速跳转` | 再结合 [docs/components.md](docs/components.md) 看它落在哪一层 |

### 4.3 推荐阅读路径

如果你不知道第一篇该看哪一篇，可以直接从下面几条阅读路径起步：想理解 ArceOS 主干可以先看 `axhal`、`axtask`、`axruntime`、`axmm`；想理解平台与板级 bring-up 可以先看 `axplat`、`axplat-macros`、对应 `axplat-*` 平台包；想理解 Axvisor 可以先看 `axvm`、`axvcpu`、`axaddrspace`、`axvisor_api`、`axvisor`；想理解 StarryOS 可以先看 `starry-kernel`、`starry-process`、`starry-signal`、`starry-vm`。这样读的好处是先建立统一术语，再回到具体源码时，不容易把"平台包、模块层、叶子基础件、样例程序、测试桩"混在一起。

## 5. 工作区关系

TGOSKits 采用了复杂的 workspace 结构来管理多个系统和组件。根 `Cargo.toml` 把常用组件、ArceOS 模块与示例、StarryOS 包、Axvisor、`platform/` 和 `xtask/` 放进一个统一 workspace。`os/arceos` 和 `os/StarryOS` 自己仍保留独立 workspace，根工作区通过 `members`、`exclude` 和 `[patch.crates-io]` 把需要的 crate 接进来。一些目录本身是嵌套 workspace，比如 `components/axplat_crates`、`components/axdriver_crates`，这些目录通常不会直接作为根 workspace 成员加入，而是通过 patch 指向其中的具体 crate。

这种结构意味着在仓库根目录开发适合做跨系统联调和统一测试，而在 `os/arceos/`、`os/StarryOS/` 或 `os/axvisor/` 子目录开发则适合聚焦某一个系统的本地构建入口。

## 6. 命令入口

TGOSKits 提供了统一的命令入口来管理三个系统的构建、运行和测试。根目录的 `cargo xtask` 目前只有 `test`、`arceos`、`starry` 三类子命令，而 Axvisor 的构建与运行命令由 `os/axvisor` 自己的 xtask 提供，所以要么在根目录执行 `cargo axvisor ...`，要么进入 `os/axvisor/` 执行 `cargo xtask ...`。

| 位置 | 命令 | 说明 |
| --- | --- | --- |
| 仓库根目录 | `cargo xtask ...` | 根 `tg-xtask`，负责 ArceOS、StarryOS 和统一测试 |
| 仓库根目录 | `cargo arceos ...` | `cargo xtask arceos ...` 的别名 |
| 仓库根目录 | `cargo starry ...` | `cargo xtask starry ...` 的别名 |
| 仓库根目录 | `cargo axvisor ...` | 调用 `os/axvisor` 自带 xtask 的别名 |
| `os/arceos/` | `make ...` | ArceOS 的传统构建入口 |
| `os/StarryOS/` | `make ...` | StarryOS 的传统构建入口 |
| `os/axvisor/` | `cargo xtask ...` | Axvisor 本地 xtask，等价于根目录 `cargo axvisor ...` |

## 7. 快速体验

本节提供了三个系统的最短运行路径，帮助你在 5 分钟内快速体验 TGOSKits 的基本功能。

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

需要注意的是，Axvisor 不能只靠 `defconfig/build/qemu` 三条命令直接跑起来，因为默认 QEMU 配置会引用 `tmp/rootfs.img`。推荐先用 `os/axvisor/scripts/setup_qemu.sh` 自动准备 Guest 镜像、VM 配置和 rootfs，再运行 QEMU。完整说明见 [docs/axvisor-guide.md](docs/axvisor-guide.md)。

## 8. 基于组件开发的最短闭环

第一次修改组件代码时，建议遵循以下流程：首先在 `components/`、`os/arceos/modules/`、`os/StarryOS/kernel/` 或 `os/axvisor/src/` 里找到你要修改的入口；然后先跑最小消费者进行验证，而不是一上来跑全量测试；最后在改动稳定后，再补充系统测试和 host 测试。常用验证命令如下：

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

## 9. 许可证

仓库整体采用 `Apache-2.0` 许可证，各组件可能带有自己的许可证文件，具体以各组件目录下的 LICENSE 文件为准。
