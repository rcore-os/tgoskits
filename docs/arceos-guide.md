# ArceOS 开发指南

ArceOS 是 TGOSKits 工作区中的模块化操作系统内核，同时作为 StarryOS 和 Axvisor 的底层基础能力提供者。本文档介绍 ArceOS 的源码组织结构、构建与运行入口、架构概览、开发工作流、测试验证方法及调试手段，帮助开发者高效开展 ArceOS 相关的开发与集成工作。

## 1. 源码组织

ArceOS 的源码分布在多个目录中，各目录承担不同职责。`os/arceos/` 包含内核模块、API 层及示例应用，`components/` 包含被 ArceOS 及其他系统共同复用的基础 crate，`test-suit/arceos/` 负责系统级自动化测试。理解这些目录的职责边界，是评估改动影响范围的前提。

| 路径 | 角色 | 什么时候会改到 |
| --- | --- | --- |
| `os/arceos/modules/` | 内核模块层 | 改 HAL、调度、驱动、网络、文件系统、运行时 |
| `os/arceos/api/` | feature 与对外 API 聚合 | 要新增 feature、能力开关或统一入口 |
| `os/arceos/ulib/` | 用户侧库 | 要把能力暴露给应用时 |
| `os/arceos/examples/` | 示例应用 | 做最小验证、写新 demo |
| `test-suit/arceos/` | 系统级测试 | 做自动化回归 |
| `components/axplat_crates/platforms/*` 与 `platform/*` | 平台实现 | 新平台或板级适配 |

最重要的认知是：

- `components/` 里的基础 crate 和 `os/arceos/modules/*` 经常一起构成 ArceOS 能力
- 你改了 ArceOS 的基础层，StarryOS 和 Axvisor 也可能被连带影响

## 2. 构建与运行

ArceOS 提供两种构建与运行方式：仓库根目录的 `cargo xtask arceos` 统一入口和 `os/arceos/` 下的本地 Makefile 入口。前者适用于日常开发与 CI 环境，后者适用于需要精细控制 Makefile 变量的场景。

### 仓库根目录的推荐入口

```bash
cargo xtask arceos build --package arceos-helloworld --arch riscv64
cargo xtask arceos run --package arceos-helloworld --arch riscv64
```

常用参数：

- `--package`: 选择应用包，例如 `arceos-helloworld`
- `--arch`: `riscv64`、`x86_64`、`aarch64`、`loongarch64`
- `--platform`: 覆盖默认平台
- `--features`: 传额外 feature
- `--smp`: 指定 CPU 数量
- `--plat-dyn`: 控制是否启用动态平台

### `os/arceos/` 里的本地入口

```bash
cd os/arceos
make A=examples/helloworld ARCH=riscv64 run
make A=examples/httpserver ARCH=riscv64 NET=y run
make A=examples/shell ARCH=riscv64 BLK=y run
```

什么时候更适合用 `make`：

- 你在调试 ArceOS 自己的 Makefile 变量
- 你需要显式操控 `NET=y`、`BLK=y`、`LOG=debug` 这类本地入口参数

## 3. 架构概览

ArceOS 的能力从可复用 crate 出发，经内核模块层聚合，再通过 feature 和用户库暴露给应用。以下流程图展示了各层的职责与依赖关系，有助于判断改动应落在哪一层。

```mermaid
flowchart TD
    ReusableCrate["components/* reusable crates"]
    Modules["os/arceos/modules/*"]
    Axfeat["os/arceos/api/axfeat"]
    Ulib["os/arceos/ulib/axstd or axlibc"]
    Example["os/arceos/examples/*"]
    Tests["test-suit/arceos/*"]
    Platform["components/axplat_crates/platforms/* + platform/*"]

    ReusableCrate --> Modules
    Platform --> Modules
    Modules --> Axfeat
    Axfeat --> Ulib
    Ulib --> Example
    Modules --> Tests
    Ulib --> Tests
```

该链路对应以下几类开发场景：

- 改内部实现：动 `components/` 或 `modules/`
- 新增 feature 开关：动 `axfeat`
- 新增应用侧 API：动 `axstd` / `axlibc`
- 新增验证样例：动 `examples/` 或 `test-suit/arceos/`

## 4. 开发工作流

本节介绍 ArceOS 开发中常见的几类改动及其推荐验证路径。无论修改基础组件、暴露新能力还是添加示例应用，均应先从最小消费者开始验证，确保改动正确。

### 4.1 修改基础组件或模块

若修改内容涉及以下模块：

- `components/axerrno`、`components/kspin`、`components/page_table_multiarch`
- 或 `os/arceos/modules/axhal`、`axtask`、`axdriver`、`axnet`、`axfs`

建议先以最小消费者进行验证：

```bash
cargo xtask arceos run --package arceos-helloworld --arch riscv64
```

如果改的是特定能力，再换对应的示例：

```bash
# 网络相关
cargo xtask arceos run --package arceos-httpserver --arch riscv64 --net

# 文件系统相关
cargo xtask arceos run --package arceos-shell --arch riscv64 --blk
```

### 4.2 新增 Feature 或暴露应用接口

当某个能力已在模块层实现，但需要作为可选能力暴露给应用时，推荐按以下顺序接入：

1. 在 `os/arceos/modules/*` 完成或接入实现
2. 在 `os/arceos/api/axfeat` 暴露 feature
3. 若需直接供应用使用，则接入 `os/arceos/ulib/axstd` 或 `axlibc`

若仅完成第 1 步而未接入 `axfeat` 或 `axstd`，应用层将无法访问该能力。

### 4.3 添加新示例应用

新增示例应用通常放置于 `os/arceos/examples/<name>/`。最小 `Cargo.toml` 可参考：

```toml
[package]
name = "myapp"
version = "0.1.0"
edition.workspace = true

[dependencies]
axstd.workspace = true
```

最小 `src/main.rs` 可以参考现有 `helloworld` 的写法：

```rust
#![cfg_attr(feature = "axstd", no_std)]
#![cfg_attr(feature = "axstd", no_main)]

#[cfg(feature = "axstd")]
use axstd::println;

#[cfg_attr(feature = "axstd", unsafe(no_mangle))]
fn main() {
    println!("Hello from myapp!");
}
```

然后直接运行：

```bash
cargo xtask arceos run --package myapp --arch riscv64
```

### 4.4 添加或修改平台支持

若修改内容涉及平台逻辑，需同时关注以下目录：

- `components/axplat_crates/platforms/*`
- `platform/axplat-dyn`
- `platform/x86-qemu-q35`

验证时通常要显式指定平台：

```bash
cargo xtask arceos run --package arceos-helloworld --arch aarch64 \
    --platform axplat-aarch64-qemu-virt
```

## 5. 测试与验证

ArceOS 提供从示例应用到系统测试的多层验证入口。日常开发使用示例应用进行快速验证，改动稳定后通过系统测试进行回归。适合在宿主机运行的基础 crate 可直接使用 `cargo test`。

### 示例应用

```bash
cargo xtask arceos run --package arceos-helloworld --arch riscv64
cargo xtask arceos run --package arceos-httpclient --arch riscv64
cargo xtask arceos run --package arceos-httpserver --arch riscv64 --net
cargo xtask arceos run --package arceos-shell --arch riscv64 --blk
```

### 系统测试

```bash
cargo xtask test arceos --target riscv64gc-unknown-none-elf
```

这条命令会自动发现 `test-suit/arceos/` 下的测试包，例如任务调度相关测试。

### host / unit 测试

对于适合在 host 上跑的基础 crate，优先先做：

```bash
cargo test -p axerrno
```

## 6. 调试指南

ArceOS 提供日志级别控制和 GDB 调试两种主要调试手段。日志级别可通过本地 Makefile 传入 `LOG` 变量进行控制；断点调试可通过本地 Makefile 的 `debug` 目标启动 GDB。

### 查看详细运行日志

通过本地 Makefile 传入 `LOG` 变量：

```bash
cd os/arceos
make A=examples/helloworld ARCH=riscv64 LOG=debug run
```

### 启动 GDB 调试

本地 Makefile 已内置 `debug` 目标：

```bash
cd os/arceos
make A=examples/helloworld ARCH=riscv64 debug
```

这比在根目录命令里硬塞额外 QEMU 参数更可靠，因为根 `cargo xtask arceos run` 当前并不直接暴露原始 QEMU 参数透传接口。

### 何时优先使用根目录入口

以下场景推荐使用根目录 `cargo xtask arceos` 入口：

- 验证集成行为
- 与 `test-suit`、StarryOS 或 Axvisor 的共享依赖进行对齐
- 保持命令风格与 CI 一致

## 7. 延伸阅读

以下是理解 ArceOS 及其上下文的推荐阅读顺序，涵盖从外部入口到内部机制、从组件视角到系统集成视角的完整知识链。

- [arceos-internals.md](arceos-internals.md): 系统理解 ArceOS 的分层、feature 装配、启动路径和内部机制
- [components.md](components.md): 从组件视角继续看共享依赖怎么接到三个系统
- [build-system.md](build-system.md): 理解根 xtask、Makefile 和 workspace 的边界
- [starryos-guide.md](starryos-guide.md): 如果你的 ArceOS 改动会波及 StarryOS
- [axvisor-guide.md](axvisor-guide.md): 如果你的 ArceOS 改动会波及 Axvisor
