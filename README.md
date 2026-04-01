# TGOSKits

[![Build & Test](https://github.com/rcore-os/tgoskits/actions/workflows/test.yml/badge.svg)](https://github.com/rcore-os/tgoskits/actions/workflows/test.yml)

TGOSKits 是一个面向操作系统与虚拟化开发的集成仓库。它使用 Git Subtree 管理 60 多个独立组件仓库，将 ArceOS、StarryOS、Axvisor 以及相关平台 crate 整合在同一工作区中，支持组件级开发、跨系统联调和统一测试。

## 1. 快速导航

当前仓库包含多个系统和数几十个组件仓库，不同开发目标对应不同的文档和命令入口。下面的表格帮助你根据当前任务快速定位到最合适的文档和最短可用命令。

| 你的目标 | 建议先看 | 最短命令 |
| --- | --- | --- |
| 第一次跑起来 | [docs/quick-start.md](docs/quick-start.md) | `cargo xtask arceos run --package arceos-helloworld --arch riscv64` |
| 完整开发示例 | [docs/demo.md](docs/demo.md) | 从零新增一个组件或者修改组件的完整开发流程示例 |
| 组件开发说明 | [docs/components.md](docs/components.md) | 从 `components/` 或 `os/arceos/modules/` 开始 |
| 开发 ArceOS | [docs/arceos-guide.md](docs/arceos-guide.md) | `cargo xtask arceos run --package arceos-helloworld --arch riscv64` |
| 开发 StarryOS | [docs/starryos-guide.md](docs/starryos-guide.md) | `cargo xtask starry rootfs --arch riscv64` |
| 运行 Axvisor | [docs/axvisor-guide.md](docs/axvisor-guide.md) | `cd os/axvisor && ./scripts/setup_qemu.sh arceos` |
| 理解命令构建和测试矩阵 | [docs/build-system.md](docs/build-system.md) | `cargo xtask test` |
| 理解仓库如何组织众多独立组件 | [docs/repo.md](docs/repo.md) | `python3 scripts/repo/repo.py list` |

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

仓库采用 `main` / `dev` / 功能分支三层策略。`main` 作为稳定基线定期发布，`dev` 作为集成分支汇聚所有开发和 CI 验证，开发者基于 `dev` 创建功能分支并通过 PR 合入（禁止直发 `main`）。

| 分支 | 职责 | 规则 |
| --- | --- | --- |
| `main` | 稳定发布分支，每周从 `dev` 合并 | 禁止直接 push |
| `dev` | 集成分支，汇聚开发功能，执行 CI | 通过 PR 合并，push 触发自动同步 |
| 功能分支 | 开发者个人开发 | 完成后向 `dev` 提交 PR（禁止直发 `main`） |

```text
feature/* ──PR──► dev ──push.yml──► 独立组件仓库 dev 分支
                   │
                定期合并
                   ▼
                 main ──push.yml──► ArceOS 组件 release / 其他组件 main
                   ▲
                   │ 独立组件仓库 push.yml + PR
                   └────────────── 独立组件仓库 main 更新
```

详见 [docs/repo.md](docs/repo.md#9-分支管理)。

## 3. 快速体验

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

## 4. 快速开发

对于开发来说，首先在 `components/`、`os/arceos/modules/`、`os/StarryOS/kernel/` 或 `os/axvisor/src/` 里找到你要修改的入口。然后，先跑最小消费者，而不是一上来跑全量测试。改动稳定后，再补系统测试和 host 测试。

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

## 5. 许可证

仓库整体采用 `Apache-2.0` 许可证。各组件可能带有自己的 LICENSE 文件，具体以各组件目录下的为准。
