---
sidebar_position: 3
sidebar_label: "Axvisor"
title: "Axvisor 快速上手"
---

# Axvisor 快速上手

Axvisor 的最短验证路径建议直接使用测试入口。当前主流快速路径是 AArch64 和 x86_64 的 QEMU 测试；板测则依赖 self-hosted 环境。

## 1. QEMU

Axvisor 的快速验证建议优先从 `test qemu` 开始，而不是直接进入更复杂的板级或 U-Boot 路径。这样可以先确认 hypervisor、Guest 资产和基础运行链路是否已经正常。

### 1.1 AArch64

`aarch64` 是当前 Axvisor 最主流的快速验证路径。无论是本地理解整体链路，还是和 CI 中的自动测试对应，这一条都最值得先跑通。

```bash
cargo xtask axvisor test qemu --target aarch64-unknown-none-softfloat
```

### 1.2 x86_64

`x86_64` 适合作为第二条验证路径，用于确认不同平台上的 hypervisor 启动和 Guest 运行行为。它也是当前 `test qemu` 明确支持的目标之一。

```bash
cargo xtask axvisor test qemu --target x86_64-unknown-none
```

> `axvisor test qemu` 当前只支持 `aarch64` 和 `x86_64`。  
> `--guest` 不是 `test qemu` 的参数；如果需要板级 U-Boot 测试中的 guest 选择，应使用 `cargo xtask axvisor test uboot ...`。

## 2. U-Boot 测试

当需要贴近板级启动链路时，可以进入 `test uboot`。这一入口不是通用目录扫描，而是围绕仓库中已经维护好的板型与 Guest 组合展开。

当前 `test uboot` 使用硬编码白名单中的 `(board, guest)` 组合。主流示例：

```bash
cargo xtask axvisor test uboot --board orangepi-5-plus --guest linux
cargo xtask axvisor test uboot --board phytiumpi --guest linux
cargo xtask axvisor test uboot --board roc-rk3568-pc --guest linux
```

## 3. Board 测试

`test board` 适合在已有板级环境或 self-hosted runner 条件下使用。这里的命令按测试组名组织，而不是按裸板名组织，因此更接近当前仓库中的自动化入口。

当前 `test board` 不是直接传板名，而是使用测试组名：

```bash
cargo xtask axvisor test board -t orangepi-5-plus-linux
cargo xtask axvisor test board -t phytiumpi-linux
cargo xtask axvisor test board -t roc-rk3568-pc-linux
```

如需显式指定板测配置，也可以使用：

```bash
cargo xtask axvisor test board -t orangepi-5-plus-linux --board-test-config <path>
```

> Board 测试通常需要 self-hosted runner、串口服务器或物理板环境，本地普通开发机通常无法直接复现。

## 4. 选择建议

如果你不确定应该从哪条命令开始，可以先按这里的建议选一条最短路径。这样能把“命令是否正确”和“环境是否齐全”两个问题分开验证。

| 目标 | 建议 |
|------|------|
| 第一次跑通 Axvisor | 先用 `test qemu --target aarch64` |
| 验证 x86 路径 | 使用 `test qemu --target x86_64` |
| 验证板级路径 | 使用 `test uboot` 或 `test board` 的硬编码测试组 |

快速上手只覆盖最常见的入口。若需要继续理解测试分组、QEMU/U-Boot/board 三条链路的实现细节，可以继续阅读：

- [Axvisor 开发指南](../design/systems/axvisor-guide)
- [Axvisor 测试套件设计](../design/test/axvisor)
- [CI 自动测试实现](../design/test/ci)
