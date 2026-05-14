---
sidebar_position: 1
sidebar_label: "概述"
---

# 调试概述

本文档概述 TGOSKits 当前的 VS Code 本地调试方案，重点说明它的设计目标、组件边界和平台分流思路。

## 设计目标

当前调试方案主要围绕四个目标展开：

- 以 VS Code 作为统一入口，不要求开发者手工先起 QEMU 再附加调试器
- 把构建产物准备和调试会话管理拆开，避免首次冷编译时附加时序不稳定
- 保持 `cargo xtask` 作为运行入口，与仓库现有命令体系对齐
- 在 Linux 与 Windows 上都提供可用的集成终端调试体验

## 组件划分

调试链路由三部分组成：

- `.vscode/launch.json`
- `.vscode/tasks.json`
- `.vscode/session.py`

三者的职责边界是刻意分开的：

- `launch.json` 只描述“调试器如何附加、在哪些位置下断点”
- `tasks.json` 只描述“进入调试前需要先完成哪些前置动作”
- `session.py` 只描述“一个 QEMU debug 会话如何启动、等待、结束和清理”

这样设计的原因是：VS Code 本身不擅长处理“长时间后台任务 + 端口就绪检测 + 失败清理”的组合逻辑，把这些行为收口到脚本层更稳定，也更便于跨平台统一。

## 调试配置组织

当前预置的 AArch64 调试配置包括：

- `ArceOS Main`
- `ArceOS Boot`
- `StarryOS Main`
- `StarryOS Boot`
- `Axvisor Main`
- `Axvisor Boot`

这些配置都会自动完成：

- debug 构建
- QEMU 启动
- GDB stub 打开
- LLDB 附加
- 调试结束后的 QEMU 清理

设计上每个系统都提供 `Main` 和 `Boot` 两类入口：

- `Main` 聚焦主执行路径，适合验证功能逻辑
- `Boot` 聚焦更早的初始化阶段，适合观察平台入口、runtime 初始化和系统装配顺序

这种划分的目的不是单纯增加入口数量，而是让断点位置可以系统性前移或后移，降低不同问题类型之间切换调试上下文的成本。

## 会话状态模型

`session.py` 当前把调试会话抽象成几个稳定状态：

- `starting`
- `running`
- `ready`
- `failed-before-ready`
- `stopping`
- `exited`

这些状态会写入 `target/qemu-debug/*.log`。它们的作用不是替代完整运行日志，而是让“构建失败 / QEMU 没起来 / stub 没打开 / 调试结束清理”这类会话级问题可以单独定位。

## 平台分流

调试方案在平台层面有意做了两条实现路径：

- Linux：优先追求“实时终端 + 实时日志镜像”
- Windows：优先追求“VS Code 集成终端实时输出 + 调试稳定”

这不是功能不一致，而是由平台终端能力和宿主机进程模型差异决定的实现取舍。

## 回归位置

调试方案本身不负责完整验证，它只负责把开发者尽快带到“可断点、可观察”的状态。因此设计上把“调试确认”和“回归验证”明确分成两步：

1. 先通过 VS Code 调试确认行为
2. 再执行最小相关回归

这样可以避免把完整测试矩阵误用成本地调试入口。
## 当前限制与已知约束

### 架构覆盖范围

当前所有 6 个预置调试配置（ArceOS/Axvisor/StarryOS × Main/Boot）**仅支持 AArch64**。调试目标路径、QEMU 命令参数和 GDB stub 端口均围绕 `aarch64-unknown-none-softfloat` target 硬编码。

工作区虽然包含 RISC-V（`riscv_vcpu`、`riscv_plic`、`riscv_vplic`）和 x86（`x86_vcpu`、`x86_vlapic`、`x86-qemu-q35`）相关组件，但当前没有对应的调试配置。添加新架构支持需要：

1. 在 `launch.json` 中新增配置组（调整 target triple 路径、二进制名、断点位置）
2. 在 `tasks.json` 中新增对应的 Build / Start / Prepare / Stop 任务链
3. 确认对应架构的 QEMU system 模拟器可用
4. 确认 `rustup target install` 已安装对应 target

### 端口与并发约束

当前所有会话共享同一个 GDB stub 端口（默认 `1234`，通过 `TGOS_DEBUG_PORT` 配置）。这意味着：

- **同一时刻只能运行一个调试会话**：启动 ArceOS 调试后，未停止就切换到 Axvisor 调试会导致端口冲突
- `session.py` 的 `_port_owned_by_group()` 能检测到"端口被非本会话进程持有"的情况，但不会自动解决冲突
- 如果需要同时调试两个系统，需要为其中一个配置不同的 `TGOS_DEBUG_PORT`，并确保对应 QEMU 启动命令使用 `-gdb tcp::<port>` 参数

### 日志文件管理

每次调试运行会在 `target/qemu-debug/` 下生成或追加以下文件：

- `<session>.log`：完整输出日志（持续累积，不自动清理）
- `<session>.pid`：主进程 PID（会话结束时删除）
- `<session>.pgid`：进程组 ID（会话结束时删除，仅 Linux）

长时间开发后日志文件可能较大。如需清理，可直接删除 `target/qemu-debug/` 目录——不影响调试功能，下次启动会重新创建。