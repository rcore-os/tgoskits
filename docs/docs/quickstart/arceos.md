---
sidebar_position: 1
sidebar_label: "ArceOS"
title: "ArceOS 快速上手"
---

# ArceOS 快速上手

ArceOS 的最短路径通常是选择一个示例包，通过 `cargo xtask arceos qemu` 直接构建并启动。

## 1. 快速启动

本节给出 ArceOS 在不同架构上的最短启动命令。推荐优先选择 `ax-helloworld`，因为它依赖最少、输出最直接，适合确认基础构建链路和 QEMU 路径是否正常。

### 1.1 RISC-V 64

`riscv64` 是当前最适合作为第一条验证路径的架构之一。命令短、反馈明确，也最便于和测试套件中的主流验证路径对应起来。

```bash
cargo xtask arceos qemu --package ax-helloworld --target riscv64gc-unknown-none-elf
```

### 1.2 AArch64

如果后续工作会涉及 StarryOS 或 Axvisor，AArch64 路径会更容易和其他系统对齐。它适合在完成第一条最小运行路径后继续验证。

```bash
cargo xtask arceos qemu --package ax-helloworld --target aarch64-unknown-none-softfloat
```

### 1.3 x86_64

`x86_64` 更适合本地 x86 平台适配或与 PC 类平台环境对照时使用。启动方式与其他架构一致，主要差别在目标 triple 和底层平台配置。

```bash
cargo xtask arceos qemu --package ax-helloworld --target x86_64-unknown-none
```

### 1.4 LoongArch64

LoongArch64 路径适合作为补充验证，而不是第一次上手的默认首选。使用前建议先确认本地环境或容器环境中对应 QEMU 已可用。

```bash
cargo xtask arceos qemu --package ax-helloworld --target loongarch64-unknown-none-softfloat
```

## 2. 常用包

ArceOS 的快速上手不仅是“把系统跑起来”，还常常需要快速切换到不同类型的示例应用。`--package` 是最常见的切换方式，因此这里列出当前仓库中最适合做入门验证的几个包。

`--package` 用于选择要启动的应用。当前仓库中常见快速上手包包括：

| 包名 | 功能 |
|------|------|
| `ax-helloworld` | 最小 Hello World |
| `ax-httpserver` | HTTP 服务器 |
| `ax-httpclient` | HTTP 客户端 |
| `ax-shell` | 交互式 Shell |

示例：

```bash
# HTTP 服务器
cargo xtask arceos qemu --package ax-httpserver --target riscv64gc-unknown-none-elf

# 文件系统 Shell
cargo xtask arceos qemu --package ax-shell --target aarch64-unknown-none-softfloat

# 仅构建
cargo xtask arceos build --package ax-helloworld --target riscv64gc-unknown-none-elf
```

## 3. 测试入口

当单个示例已经可以稳定启动后，下一步通常是切到测试套件入口做批量验证。ArceOS 的测试支持 Rust 与 C 两条路径，并允许按类型或包名做筛选。

ArceOS 的测试入口支持 Rust/C 混合测试、单独 Rust 测试、单独 C 测试和包过滤：

```bash
# 全部测试（Rust + C）
cargo xtask arceos test qemu --target riscv64gc-unknown-none-elf

# 仅 Rust
cargo xtask arceos test qemu --target riscv64gc-unknown-none-elf --only-rust

# 仅 C
cargo xtask arceos test qemu --target riscv64gc-unknown-none-elf --only-c

# 指定单个 Rust 测试包
cargo xtask arceos test qemu --target aarch64-unknown-none-softfloat -p arceos-affinity
```

详细说明见：[ArceOS 测试套件设计](../design/test/arceos)

## 4. 选择建议

如果不确定该从哪个包或哪个命令开始，可以先按下表选择最接近当前目标的路径。这样可以避免一开始就进入依赖较多或交互更复杂的示例。

| 场景 | 建议命令 |
|------|----------|
| 第一次跑通 | `ax-helloworld` + `riscv64` |
| 网络验证 | `ax-httpserver` 或 `ax-httpclient` |
| 文件系统 / 交互验证 | `ax-shell` |
| 回归测试 | `cargo xtask arceos test qemu ...` |

快速上手只覆盖“先跑起来”的路径。若需要继续理解 ArceOS 的模块层、组件关系或测试实现，可以继续阅读：

- [ArceOS 开发指南](../design/systems/arceos-guide)
- [ArceOS 测试套件设计](../design/test/arceos)
- [组件开发指南](../design/reference/components)
