---
sidebar_position: 1
sidebar_label: "配置体系概览"
---

# 配置体系概览

Axvisor 最容易被低估的部分不是代码，而是**配置**。很多运行问题本质上来自板级配置、VM 配置、镜像路径或 rootfs 没有对齐。

## 两条配置主线

| 类型 | 作用 | 入口文件 |
|------|------|---------|
| **板级配置** | 构建目标、feature、日志级别、默认 VM 组合 | `configs/board/*.toml` |
| **VM 配置** | Guest CPU、内存、镜像、设备与中断模式 | `configs/vms/*.toml` |

## 常见配置入口

- `os/axvisor/configs/board/*.toml`
- `os/axvisor/configs/vms/*.toml`
- `.axvisor.toml` 与构建配置文件
- `os/axvisor/tmp/*` 下的临时镜像与运行时文件

## 排查经验

| 问题类型 | 优先检查 |
|----------|---------|
| 编译问题 | 板级配置和 feature |
| 启动问题 | VM 配置、`kernel_path`、`image_location` |
| "能 build 但不能 boot" | 通常不在 Rust 代码主路径 |

详细说明：[AxVisor 内部机制](/docs/design/architecture/axvisor-internals)
