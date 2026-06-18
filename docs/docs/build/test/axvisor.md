---
sidebar_position: 7
sidebar_label: "Axvisor 测试"
---

# Axvisor 测试

Axvisor 作为 Hypervisor，其测试与 ArceOS 和 StarryOS 的关键区别在于：**构建产物是 hypervisor 而非 kernel**，运行时需要同时准备 hypervisor 和 Guest OS 镜像，且独有 U-Boot 测试模式。QEMU loader 测试现在由顶层 `axloader` 命令负责；Axvisor 命令继续负责 U-Boot 和 Board 测试，覆盖真实硬件部署链路。

## 命令

Axloader 提供 QEMU loader 测试；Axvisor 提供 U-Boot 和 Board 测试，其中 U-Boot 测试为 Axvisor 独有的板上引导验证方式：

```text
cargo xtask axloader test qemu --arch <arch> [--test-case <case>]
cargo xtask axvisor test uboot --board <board> --guest <guest> [--uboot-config <path>]
cargo xtask axvisor test board [--test-case <case>] [--board <name>]
```

旧的 `cargo xtask axvisor test qemu ...` 入口只会返回迁移提示，不再运行 QEMU loader 用例。U-Boot 测试是 Axvisor 独有的，需要指定 board 和 guest 参数来确定测试组合。

## 用例类型

Axloader/Axvisor 用例结构与 StarryOS 类似，支持 Plain、C、Shell、Python、Grouped 全部 pipeline。

Axloader/Axvisor 复用了与 StarryOS 相同的测试基础设施（用例发现、资产准备、结果判定），因为两者都是完整 OS/Hypervisor 级别的测试，需要在 rootfs 用户空间中执行测试命令。五种 pipeline 类型的处理逻辑完全相同，详见 [资产准备](./assets)。

## Axloader QEMU 执行流程

与 StarryOS 高度相似，区别在于：
1. 构建的是 hypervisor 而非 kernel
2. 需要 rootfs 和 guest 镜像准备
3. VM config 注入（`AXVISOR_VM_CONFIGS`）

Axloader 的 QEMU 测试在构建阶段仍编译 Axvisor hypervisor（而非 kernel），hypervisor 负责管理多个 Guest VM 的创建和调度。运行前需要准备 rootfs（供 Guest OS 使用）和 guest 镜像（被 hypervisor 加载的 VM 镜像），这些通过 `--vmconfigs` 参数指定的 VM 配置文件来描述。当前 `dev` 分支尚无独立 axloader package，因此构建仍复用 `AXVISOR_VM_CONFIGS` 环境变量，hypervisor 源码通过 `env!()` 宏读取该路径；命令快照则写入独立的 `.axloader.toml`。

## U-Boot 测试

Axvisor 的 U-Boot 测试按 `board-guest` 组合定位测试场景，合并 U-Boot 与板级配置后执行：

1. `discover_uboot_test_group()` 按 `board-guest` 组合定位测试组
2. 合并显式 U-Boot 配置与 board test 配置
3. `AppContext::uboot()` 执行

U-Boot 测试用于验证 Axvisor 在真实硬件上通过 U-Boot 引导加载的完整流程。`board-guest` 组合（如 `orangepi-5-plus-linux`）唯一确定了一个测试场景：特定的板卡上运行特定的 Guest OS。U-Boot 配置定义了 TFTP 加载地址、内核参数等引导参数。
