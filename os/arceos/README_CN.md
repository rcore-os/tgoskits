# ArceOS

[English](README.md) | 中文

ArceOS 是集成在 TGOSKits 根工作区 `os/arceos` 下的组件化 Unikernel。Cargo 工作区、依赖锁定和 `cargo xtask` 入口都由仓库根目录维护，因此构建、运行与验证命令应在 TGOSKits 检出目录的根执行。

## 1. 工作区定位

ArceOS 通过 Cargo feature 在编译期装配运行时、硬件抽象、内存、任务、文件系统、网络和图形能力。它不仅提供独立的 Unikernel，也向 StarryOS 和 Axvisor 提供可复用的底层 crate。

### 1.1 源码目录

`os/arceos` 保存 ArceOS 专属源码，通用领域 crate 仍分布在根工作区的其他目录。

| 目录 | 职责 |
| --- | --- |
| `api/` | `arceos_api` 与 POSIX API 聚合层 |
| `configs/` | 板级构建配置 |
| `doc/` | ArceOS 子系统说明 |
| `modules/` | ArceOS 内建运行时模块 |
| `scripts/` | ArceOS 专用辅助脚本 |
| `tools/` | 板卡与调试工具 |
| `ulib/` | `axstd`、`axlibc` 等用户库 |

ArceOS 示例应用位于仓库根目录的 `apps/arceos/`。内建运行时模块位于 `os/arceos/modules/`；通用任务、分配器、驱动、文件系统和网络 crate 分别位于 `components/`、`memory/`、`drivers/`、`fs/` 和 `net/`，不能把 `os/arceos/modules/` 当作全部能力的清单。

### 1.2 模块与 feature

`ax-runtime` 的 `Cargo.toml` 是运行时能力装配的直接依据。`paging` 引入 `ax-mm`，`fs` 引入 `ax-fs-ng` 与块设备驱动，`net` 引入 `ax-net`，`display` 引入 `ax-gpu`，`input` 引入 `ax-input`。`ax-hal`、`ax-log`、`ax-task`、`ax-sync`、`ax-alloc` 和带串口能力的 `ax-driver` 构成运行时基线；具体 feature 组合仍以应用和构建配置为准。

## 2. 快速开始

最短路径是在仓库根目录运行 ArceOS Hello World。目标架构可以和默认值不同，以下示例使用 AArch64。

### 2.1 构建与运行

以下命令都从 TGOSKits 根目录执行。`--package` 选择根工作区中的 ArceOS 应用包，`--arch` 选择目标架构；`build` 生成内核 ELF，`qemu` 在构建后按对应板级配置启动 QEMU。

```bash
git clone https://github.com/rcore-os/tgoskits.git
cd tgoskits

cargo xtask arceos build --package arceos-helloworld --arch aarch64
cargo xtask arceos qemu --package arceos-helloworld --arch aarch64
```

需要交互式 Shell 时，把包名改为 `arceos-shell`。QEMU、板卡和构建配置的完整选择方式见 [ArceOS 构建概览](../../docs/docs/build/arceos/overview.md) 与 [命令索引](../../docs/docs/build/commands.md)。

### 2.2 验证入口

修改 Rust 包后使用根目录任务工具选择受影响范围：

```bash
cargo xtask clippy --package <PACKAGE>
cargo xtask test
cargo xtask arceos test qemu --arch aarch64
```

`cargo xtask test` 运行标准库测试白名单中当前登记的包；改动提交后可用 `cargo xtask test --since <REF>` 选择受影响包。涉及真实启动、设备或系统调用路径时，再按变更选择对应 QEMU 或板卡用例。

## 3. 文档与上游

ArceOS 架构说明由 TGOSKits 文档站点维护，英文模块说明位于 `doc/README.md`。

- [ArceOS 架构](../../docs/docs/architecture/arceos/overview.md)
- [ArceOS 构建概览](../../docs/docs/build/arceos/overview.md)
- [ArceOS 模块说明](./doc/README.md)
- [上游项目](https://github.com/arceos-org/arceos)

## 4. 许可证

ArceOS 采用 Apache License 2.0。详情见 [LICENSE](./LICENSE)。
