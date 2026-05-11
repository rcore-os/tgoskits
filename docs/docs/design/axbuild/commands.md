---
sidebar_position: 2
sidebar_label: "命令参考"
---

# 命令参考

所有命令由 `scripts/axbuild` 实现，通过 `cargo xtask` 统一入口调用。axbuild 使用 clap 进行命令行参数解析，顶层命令按 `<os> <action>` 的模式组织，其中 `<os>` 为 `arceos`、`starry`、`axvisor` 之一，`<action>` 为 `build`、`qemu`、`uboot`、`board` 或 `test`。此外还有一些不绑定特定 OS 的横切命令（如 `clippy`、`test`、`board`）。

命令按三大能力划分：**构建**（`build`）负责编译 OS 产物；**运行**（`qemu`、`uboot`、`board`）在构建基础上增加目标环境执行；**测试**（`test qemu`、`test board`）进一步增加用例发现和结果判定。每个子命令的参数包括通用选项（`--arch`、`--target`、`--debug`）和特定选项（如测试的 `--test-case`、`--test-group`）。

## 顶层命令

顶层命令提供了面向操作的全局视图，覆盖构建、运行、测试和开发辅助四个类别。

| 命令 | 能力 | 说明 | 实现位置 |
|------|------|------|---------|
| `cargo xtask <os> build` | 构建 | 编译 OS 产物 | `axbuild::<os>::build` |
| `cargo xtask <os> qemu` | 运行 | 编译并在 QEMU 中运行 | `axbuild::<os>::qemu` |
| `cargo xtask <os> uboot` | 运行 | 编译并通过 U-Boot 运行 | `axbuild::<os>::uboot` |
| `cargo xtask <os> board` | 运行 | 编译并在远程板卡运行 | `axbuild::<os>::board` |
| `cargo xtask <os> test qemu` | 测试 | QEMU 测试套件 | `axbuild::<os>::test` |
| `cargo xtask <os> test board` | 测试 | 板级测试套件 | `axbuild::<os>::test` |
| `cargo xtask test` | 测试 | host/std 白名单测试 | `axbuild::test::std` |
| `cargo xtask clippy` | 测试 | workspace 静态检查 | `axbuild::clippy` |
| `cargo xtask sync-lint` | 测试 | Relaxed 原子序检查 | `axbuild::sync_lint` |
| `cargo xtask board ...` | 运行 | 板卡管理 (ls/connect/config) | `axbuild::board` |

`cargo xtask <os> qemu` 等运行类命令会先触发构建再执行运行，因此用户通常不需要单独先 `build` 再运行。板卡管理命令（`board ls`、`board connect`、`board config`）是独立的工具集，用于查看可用板卡、分配串口连接和配置 ostool-server 地址。

## ArceOS

ArceOS 的命令特点是需要显式指定 `--package`（如 `arceos-httpserver`），因为 ArceOS 以模块化 app 的方式组织——每个包对应一个独立的可运行应用。

```text
cargo xtask arceos <subcommand> [options]
```

| 子命令 | 能力 | 说明 |
|--------|------|------|
| `build` | 构建 | 编译 |
| `qemu` | 运行 | 编译并在 QEMU 中运行 |
| `uboot` | 运行 | 编译并通过 U-Boot 运行 |
| `test qemu` | 测试 | QEMU 测试（Rust + C） |

**通用参数**：`--package`（必需）、`--arch`、`--target`、`--config`、`--plat_dyn`、`--smp`、`--debug`

**QEMU 额外参数**：`--qemu-config`、`--rootfs`

**测试参数**：`--test-group`、`--test-case`、`--package`、`--list`

`--plat_dyn` 控制是否使用动态平台加载（仅 aarch64 支持），`--smp` 设置对称多处理器核数。测试方面，ArceOS 支持 Rust 和 C 两类用例，通过 `--test-group` 选择测试组。

## StarryOS

StarryOS 与 ArceOS 的主要区别在于：不需要 `--package`（编译整个内核），增加了 rootfs 管理命令，以及支持 `--stress` 快捷方式选择压力测试组。

```text
cargo xtask starry <subcommand> [options]
```

| 子命令 | 能力 | 说明 |
|--------|------|------|
| `build` | 构建 | 编译 |
| `qemu` | 运行 | 编译并在 QEMU 中运行（含 rootfs 准备） |
| `uboot` | 运行 | 编译并通过 U-Boot 运行 |
| `board` | 运行 | 编译并在远程板卡运行 |
| `rootfs` | 运行 | 下载 rootfs 到 target 目录 |
| `defconfig` | 构建 | 生成默认板卡配置 |
| `config ls` | — | 列出可用板卡名称 |
| `quick-start` | 运行 | 常见平台便捷入口 |
| `test qemu` | 测试 | QEMU 测试（normal / stress） |
| `test board` | 测试 | 板级测试 |

**通用参数**：`--arch`、`--target`、`--config`、`--smp`、`--debug`

**Board 额外参数**：`--board-config`、`--board-type`、`--server`、`--port`

**测试参数**：`--test-group`、`--test-case`、`--stress`、`--list`

板卡运行通过 `ostool-server` 与远程板卡交互，需要指定 `--server` 和 `--port` 参数或通过 `board config` 预先配置。

## Axvisor

Axvisor 作为 Hypervisor，增加了 `--vmconfigs` 参数用于指定虚拟机配置列表，以及 `image` 子命令管理 Guest 镜像，并独有 `test uboot` 测试模式。

```text
cargo xtask axvisor <subcommand> [options]
```

| 子命令 | 能力 | 说明 |
|--------|------|------|
| `build` | 构建 | 编译 |
| `qemu` | 运行 | 编译并在 QEMU 中运行（含 rootfs 准备） |
| `uboot` | 运行 | 编译并通过 U-Boot 运行 |
| `board` | 运行 | 编译并在远程板卡运行 |
| `defconfig` | 构建 | 生成默认板卡配置 |
| `config ls` | — | 列出可用板卡名称 |
| `image` | 运行 | Guest 镜像管理 |
| `test qemu` | 测试 | QEMU 测试 |
| `test uboot` | 测试 | U-Boot 测试 |
| `test board` | 测试 | 板级测试 |

**通用参数**：`--arch`、`--target`、`--config`、`--plat_dyn`、`--smp`、`--debug`、`--vmconfigs`

**测试参数**：`--test-group`、`--test-case`、`--list`、`--board`（board 测试）

通过 U-Boot 加载并验证 Hypervisor 和 Guest 组合的正确性。在 loongarch64 架构上运行时，axbuild 会自动搜索 LVZ 扩展版 QEMU。

## 其他命令

### `cargo xtask test`

对 `scripts/test/std_crates.csv` 白名单中的每个 crate 执行 `cargo test -p <package>`。白名单机制确保只有已知能在当前环境中通过的 crate 被纳入测试，避免因个别 crate 的平台限制导致整个测试流程失败。

### `cargo xtask clippy`

- 默认使用 `scripts/test/clippy_crates.csv` 白名单
- `--all` 检查全部 workspace 包
- `--package <name>` 检查指定包
- 对每个包还会检查其所有 feature 组合和 `docs.rs` 目标平台

clippy 检查不仅运行默认 lint 规则，还会遍历每个包的所有 feature 组合（确保 feature 门控代码也被检查）以及 `docs.rs` 目标平台（验证文档构建不会因平台特定代码报错）。

### `cargo xtask sync-lint`

扫描 workspace 中所有 Rust 源文件，检测可疑的 `Relaxed` 原子序使用。在内核/裸机环境中，不恰当的 `Relaxed` 排序可能导致难以复现的并发 bug，此命令帮助开发者审查所有 `Ordering::Relaxed` 使用点。

### `cargo xtask board`

| 子命令 | 说明 |
|--------|------|
| `ls` | 列出可用远程板卡类型 |
| `connect -b <type>` | 分配板卡并连接串口 |
| `config` | 编辑板卡服务器配置 |

板卡管理通过 `ostool-server` 进行。`ls` 列出服务器上所有可用的板卡类型（如 OrangePi-5-Plus），`connect` 分配一块空闲板卡并建立串口连接，`config` 编辑板卡服务器的地址和端口配置。
