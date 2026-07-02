---
sidebar_position: 2
sidebar_label: "命令索引"
---

# 命令索引

所有命令由 `scripts/axbuild` 实现，通过 `cargo xtask` 统一入口调用。本文是命令导航页：每个 `cargo xtask <cmd>` 都对应一篇专门的详细文档，按命令本身组织（不再按构建/运行/测试能力分章）。

## 调用方式与别名

默认调用方式为 `cargo xtask <cmd>`，经 `tg-xtask` 包转发到 `axbuild::run()`：

```text
cargo xtask <cmd>  →  cargo run -p tg-xtask -- <cmd>  →  axbuild::run()
```

`.cargo/config.toml` 中预配置了以下别名，使命令更简洁：

| 完整命令 | 别名 |
|----------|------|
| `cargo xtask arceos ...` | `cargo arceos ...` |
| `cargo xtask starry ...` | `cargo starry ...` |
| `cargo xtask axvisor ...` | `cargo axvisor ...` |
| `cargo xtask board ...` | `cargo board ...` |

两种写法等价：

```bash
cargo xtask arceos qemu --package arceos-httpserver
cargo arceos qemu --package arceos-httpserver   # 同上
```

## 命令总览

`cargo xtask` 的顶层命令（与 `tg-xtask --help` 输出一致）：

| 命令 | 类别 | 说明 | 详细文档 |
|------|------|------|----------|
| `cargo xtask test` | 横切·测试 | workspace std 白名单测试 | [Std 白名单测试](./test) |
| `cargo xtask clippy` | 横切·检查 | workspace clippy（feature × target 矩阵） | [Clippy 检查](./clippy) |
| `cargo xtask sync-lint` | 横切·检查 | 可疑 `Relaxed` 原子序检查 | [Sync Lint](./sync_lint) |
| `cargo xtask spin-lint` | 横切·检查 | vendored `spin` 迁移守护 | [Spin Lint](./spin_lint) |
| `cargo xtask board` | 横切·板卡 | 远程板卡管理（ls/connect/config） | [板卡管理](./board) |
| `cargo xtask config` | 横切·辅助 | axconfig 平台配置工具 | [Config 辅助命令](./config_cmd) |
| `cargo xtask backtrace` | 横切·辅助 | host 端 backtrace 符号化 | [Backtrace 符号化](./backtrace) |
| `cargo xtask image` | 横切·镜像 | TGOS rootfs/guest 镜像管理 | [镜像管理](./image) |
| `cargo xtask axloader` | 子系统 | UEFI bootloader 构建与 HTTP smoke 测试 | [Axloader](./axloader) |
| `cargo xtask arceos` | 子系统 | ArceOS 构建/运行/测试 | [ArceOS 概述](./arceos/overview) |
| `cargo xtask starry` | 子系统 | StarryOS 构建/运行/测试/app/perf/kmod | [StarryOS 概述](./starry/overview) |
| `cargo xtask axvisor` | 子系统 | Axvisor 构建/运行/测试（含 `test uboot`） | [Axvisor 概述](./axvisor/overview) |

## 三套 OS 子系统的共享文档

[ArceOS](./arceos/overview)、[StarryOS](./starry/overview)、[Axvisor](./axvisor/overview) 三套子系统共享底层的构建/运行/测试框架，差异主要体现在 CLI 命令面、参数默认值和少量特有行为。共享部分抽成独立文档，各 OS 目录（`arceos/`、`starry/`、`axvisor/`）只描述各自的特有命令和行为并引用这些共享文档：

| 共享能力 | 文档 |
|----------|------|
| 构建过程（八阶段流水线、Feature 解析、axconfig 生成） | [构建过程](./build_process) |
| 运行时环境（QEMU / U-Boot / 板卡） | [运行时环境](./runtime) |
| 测试框架（用例发现、分组构建、资产准备、结果判定） | [测试框架](./test_framework) |

各 OS 目录内含 `overview`（命令与特有行为）和 `test`（该 OS 的测试目录结构与用例类型）。

## 配置与 CI

| 主题 | 文档 |
|------|------|
| 参数与配置（Snapshot / Build Info / axconfig / arch↔target 映射） | [参数与配置](./configuration) |
| 自动 CI 测试（matrix、缓存、self-hosted runner） | [自动 CI 测试](./ci) |

## 快速决策

- **想跑某个 ArceOS app**：`cargo arceos qemu --package <name>` → [ArceOS](./arceos/overview)
- **想跑 StarryOS 内核**：`cargo starry qemu` → [StarryOS](./starry/overview)
- **想跑 Axvisor + Guest**：`cargo axvisor qemu --vmconfigs <cfg>` → [Axvisor](./axvisor/overview)
- **想做静态检查**：`cargo xtask clippy` / `sync-lint` / `spin-lint`
- **想连接物理板卡调试**：`cargo xtask board connect -b <type>` → [板卡管理](./board)
- **想符号化 panic 栈**：`cargo xtask backtrace symbolize --elf <path>` → [Backtrace](./backtrace)
- **想理解构建/运行/测试原理**：[构建过程](./build_process) / [运行时环境](./runtime) / [测试框架](./test_framework)
