# TGOSKits

[![Build & Test](https://github.com/rcore-os/tgoskits/actions/workflows/test.yml/badge.svg)](https://github.com/rcore-os/tgoskits/actions/workflows/test.yml)

TGOSKits 是一个面向操作系统与虚拟化开发的集成仓库。它使用 Git Subtree 管理 60 多个独立组件仓库，将 ArceOS、StarryOS、Axvisor 以及相关平台 crate 整合在同一工作区中，支持组件级开发、跨系统联调和统一测试。

## 1. 快速导航

仓库包含多个系统和数十个组件，不同开发目标对应不同的文档和命令入口。下面的表格帮助你根据当前任务快速定位到最合适的文档和最短可用命令。

| 你的目标 | 建议先看 | 最短命令 |
| --- | --- | --- |
| 理解仓库组织和 Subtree 管理 | [docs/repo.md](docs/repo.md) | `python3 scripts/repo/repo.py list` |
| 第一次跑起来 | [docs/quick-start.md](docs/quick-start.md) | `cargo xtask arceos run --package arceos-helloworld --arch riscv64` |
| 理解命令入口和测试矩阵 | [docs/build-system.md](docs/build-system.md) | `cargo xtask test` |
| 基于组件开发 | [docs/components.md](docs/components.md) | 从 `components/` 或 `os/arceos/modules/` 开始 |
| 按 crate 维度学习仓库 | [docs/crates/README.md](docs/crates/README.md) | 先看批次总览，再跳到具体 crate 文档 |
| 开发 ArceOS | [docs/arceos-guide.md](docs/arceos-guide.md) | `cargo xtask arceos run --package arceos-helloworld --arch riscv64` |
| 开发 StarryOS | [docs/starryos-guide.md](docs/starryos-guide.md) | `cargo xtask starry rootfs --arch riscv64` |
| 运行 Axvisor | [docs/axvisor-guide.md](docs/axvisor-guide.md) | `cd os/axvisor && ./scripts/setup_qemu.sh arceos` |

## 2. 仓库结构

仓库按职责将代码划分为顶层目录：`components/` 存放独立的可复用组件，`os/` 存放三个目标系统的源码，`platform/` 存放平台相关 crate，`docs/` 集中管理开发者文档。`scripts/repo/` 提供 subtree 管理工具。

```text
tgoskits/
├── components/                # subtree 管理的独立组件 crate
├── os/
│   ├── arceos/                # ArceOS: modules / api / ulib / examples
│   ├── StarryOS/              # StarryOS: kernel / starryos / make
│   └── axvisor/               # Axvisor: src / configs / local xtask
├── platform/                  # 平台相关 crate
├── test-suit/                 # ArceOS / StarryOS 系统测试
├── xtask/                     # 根目录 tg-xtask
├── scripts/
│   └── repo/                  # subtree 管理脚本与 repos.csv
└── docs/                      # 开发者文档
```

`components/` 下大多数组件直接平铺，类别信息来自 `scripts/repo/repos.csv` 和根 `Cargo.toml`。

## 3. 命令入口

仓库提供了统一的 `cargo xtask` 命令来管理 ArceOS、StarryOS 的构建和测试，Axvisor 则使用自带的 xtask 并通过根目录别名调用。各子系统也保留了传统的 `make` 入口。

| 位置 | 命令 | 说明 |
| --- | --- | --- |
| 仓库根目录 | `cargo xtask ...` | 统一入口：ArceOS、StarryOS、统一测试 |
| 仓库根目录 | `cargo arceos ...` / `cargo starry ...` | xtask 别名 |
| 仓库根目录 | `cargo axvisor ...` | 调用 `os/axvisor` 自带 xtask |
| `os/arceos/` / `os/StarryOS/` | `make ...` | 传统构建入口 |

根 `Cargo.toml` 通过 `members`、`exclude` 和 `[patch.crates-io]` 管理统一 workspace；`os/arceos` 和 `os/StarryOS` 保留独立 workspace。在仓库根目录开发适合跨系统联调，在子目录开发适合聚焦单一系统。更多细节见 [docs/build-system.md](docs/build-system.md)。

## 4. 快速体验

以下命令提供了三个系统的最小运行路径，帮助你快速验证环境是否就绪。ArceOS 可直接运行，StarryOS 需要先准备 rootfs，Axvisor 需要先使用 setup 脚本准备 Guest 镜像和配置。

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
cargo axvisor qemu \
  --config os/axvisor/.build-aarch64-unknown-none-softfloat.toml \
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
cargo xtask test
# ArceOS
cargo xtask arceos run --package arceos-helloworld --arch riscv64

# StarryOS（首次运行前先准备 rootfs）
cargo xtask starry rootfs --arch riscv64
cargo xtask starry run --arch riscv64 --package starryos

# Axvisor（需先准备 Guest 和 rootfs）
cd os/axvisor
./scripts/setup_qemu.sh arceos
cargo axvisor qemu \
  --config os/axvisor/.build-aarch64-unknown-none-softfloat.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml

# Axvisor 统一测试
cargo axvisor test qemu --target aarch64
```

修改组件后的验证策略见 [docs/components.md](docs/components.md)。

## 5. 分支管理

仓库采用 `main` / `dev` / 功能分支三层策略。`main` 作为稳定基线定期发布，`dev` 作为集成分支汇聚所有开发和 CI 验证，开发者基于 `dev` 创建功能分支并通过 PR 合入（禁止直发 `main`）。

| 分支 | 职责 | 规则 |
| --- | --- | --- |
| `main` | 稳定发布分支，每周从 `dev` 合并 | 禁止直接 push |
| `dev` | 集成分支，汇聚开发功能，执行 CI | 通过 PR 合并，push 触发自动同步 |
| 功能分支 | 开发者个人开发 | 完成后向 `dev` 提交 PR（禁止直发 `main`） |

```text
feature/* ──PR──► dev ──push.yml──► 组件仓库 dev
                   │
                定期合并
                   │
                   ▼
           main ◄──PR── 组件仓库 main
```

详见 [docs/repo.md](docs/repo.md#9-分支管理)。

## 6. 许可证

仓库整体采用 `Apache-2.0` 许可证。各组件可能带有自己的 LICENSE 文件，具体以各组件目录下的为准。
