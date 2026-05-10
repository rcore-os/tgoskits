---
sidebar_position: 1
sidebar_label: "Guest 管理入口"
---

# Guest 管理入口

Axvisor 的 Guest 相关操作分布在多个入口点。

## 常用入口

| 入口 | 用途 |
|------|------|
| `os/axvisor/scripts/setup_qemu.sh` | 准备 Guest 镜像、rootfs、VM 配置 |
| `cargo xtask axvisor qemu --arch aarch64` | 启动 Axvisor 并运行 Guest |
| `os/axvisor/configs/vms/*.toml` | Guest VM 配置定义 |
| 运行时 shell 与 VMM 日志 | 运行时状态查看 |

## 推荐工作顺序

1. **准备镜像与 rootfs** — 执行 `setup_qemu.sh`
2. **确认板级和 VM 配置** — 检查 `configs/board/*.toml` 和 `configs/vms/*.toml`
3. **启动 Axvisor** — 执行 `cargo xtask axvisor qemu`
4. **确认 Guest 状态** — 通过 shell 日志或运行时输出

## 详细参考

- [Axvisor 开发指南](/docs/design/systems/axvisor-guide) — 完整开发流程
- [AxVisor 内部机制](/docs/design/architecture/axvisor-internals) — 配置体系与执行路径
- [QEMU 部署](../deploy/qemu) — QEMU 环境配置
