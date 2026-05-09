---
sidebar_position: 2
sidebar_label: "命令总览"
---

# 命令总览

## 顶层命令

| 命令 | 作用 |
|------|------|
| `cargo xtask test` | 运行 host/std 白名单测试 |
| `cargo xtask clippy` | 对 workspace 做静态检查 |
| `cargo xtask board ...` | 板卡管理相关命令 |
| `cargo xtask arceos ...` | ArceOS 构建、运行、测试 |
| `cargo xtask starry ...` | StarryOS 构建、运行、rootfs、测试 |
| `cargo xtask axvisor ...` | Axvisor 构建、运行、镜像、配置、测试 |

## 各系统高频子命令

| 系统 | 常用子命令 | 说明 |
|------|-----------|------|
| **ArceOS** | `build`, `qemu`, `uboot`, `test qemu` | 构建 / QEMU 运行 / U-Boot 路径 / 测试 |
| **StarryOS** | `build`, `rootfs`, `qemu`, `uboot`, `test qemu` | 构建 / rootfs 准备 / 运行 / 测试 |
| **Axvisor** | `build`, `qemu`, `test qemu`, `defconfig`, `config`, `image` | 构建 / 运行 / 测试 / 配置 / 镜像 |

## 入口选择建议

| 目标 | 推荐命令 |
|------|---------|
| 验证功能 | 最小 `qemu` 路径 |
| 回归测试 | 对应系统的 `test qemu` |
| 检查 std crate | `cargo xtask test` |
| 静态分析 | `cargo xtask clippy` |

完整参数与行为说明：[构建系统完整说明](/docs/design/reference/build-system)
