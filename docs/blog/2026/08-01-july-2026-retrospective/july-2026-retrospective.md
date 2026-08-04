---
slug: july-2026-retrospective
title: 2026 年 7 月开发月报
date: 2026-08-01T23:00:00+08:00
authors: [tgoskits-team]
tags: [monthly-report, arceos, starryos, axvisor, axbuild, testing]
---

2026 年 7 月，TGOSKits 的工作重点从“继续增加动态平台和真实应用支持”转向“统一实现路径、理清模块分工，并用完整应用场景检验系统”。全月 `dev` 分支共产生 **217 次非合并提交**，GitHub 仓库合并 **221 个 PR**，其中 **217 个直接进入 `dev`**，涉及 **24 个唯一提交作者邮箱**。本月的主要工作包括：彻底移除静态平台与 `axconfig` 生成路径，重新划分 AxVM/Axvisor 启动、架构、vCPU 和设备代码的职责，补齐 StarryOS 运行 Nix、OpenWrt 和浏览器所需的 Linux 接口，把块设备迁移到 IRQ 驱动的多队列运行方式，以及扩展 RK3588、RK3576、SG2002 和 LS2K1000 的板级与硬件加速支持。

<!-- truncate -->

## 总览

| 指标 | 数据 |
|------|------|
| `dev` 非合并提交数 | 217 |
| 全仓库合并 PR 数 | 221 |
| 直接合入 `dev` 的 PR 数 | 217 |
| 唯一提交作者邮箱数 | 24 |
| 涉及 PR 编号范围 | #1076 ~ #1801 |

GitHub 的 221 个合并 PR 中，除 217 个 `dev` PR 外，还有 [PR #1541](https://github.com/rcore-os/tgoskits/pull/1541)、[PR #1557](https://github.com/rcore-os/tgoskits/pull/1557)、[PR #1702](https://github.com/rcore-os/tgoskits/pull/1702) 和 [PR #1766](https://github.com/rcore-os/tgoskits/pull/1766) 分别合入 `main`、`ltp`、`rock-4d-support` 与 `ivc`。其中 LTP 基础测试和 ROCK 4D 工作随后又通过 [PR #1561](https://github.com/rcore-os/tgoskits/pull/1561) 与 [PR #1704](https://github.com/rcore-os/tgoskits/pull/1704) 进入 `dev`；正文以下以 7 月实际进入 `dev` 的内容为主。

与 6 月相比，非合并提交从 349 降至 217，合并 PR 从 299 降至 221，唯一作者邮箱从 45 降至 24。但本月仍包含 34 项 `refactor`，并完成了 AxVM 连续分层、`cpu-local` 重构、统一模拟设备框架和纯 IRQ 块设备多队列等大规模改动。数量下降主要是因为开发重心从补充大量零散兼容功能，转向整理底层架构和统一已有实现，并不意味着开发停滞。

按“每个提交是否触及某一级目录”统计，同一提交可能同时计入多个目录，7 月最活跃的区域如下：

| 一级目录 | 涉及提交数 | 主要内容 |
|----------|------------|----------|
| `os/` | 110 | StarryOS、ArceOS、Axvisor 内核与运行时 |
| `test-suit/` | 66 | syscall、批量应用兼容性测试、QEMU 与板级回归 |
| `drivers/` | 47 | 块设备、Rockchip 加速器、USB、SD/MMC、AHCI |
| `virtualization/` | 43 | AxVM、各架构 vCPU、地址空间与模拟设备 |
| `components/` | 42 | CPU-local、IRQ、运行时、文件系统与通用能力 |
| `docs/` | 38 | 架构文档、quickstart、review 与开发规范 |
| `apps/` | 38 | Nix、StarryWRT、语言/服务兼容性测试与板级应用 |
| `platforms/` | 31 | someboot、somehal、动态平台与新板卡 |
| `net/` | 22 | ax-net、socket 行为与设备统计 |
| `.github/` | 20 | CI、运行环境、覆盖率与发布流程 |

### 贡献者排行

| 贡献者 | 提交数 | 主要方向 |
|--------|--------|----------|
| 周睿 (ZR233) | 95 | AxVM/Axvisor 分层、动态平台、IRQ/块设备、CI 与开发规范 |
| 禾可 (Lfan-ke) | 32 | StarryOS 应用兼容性测试、Nix/OpenWrt、procfs、网络与 Linux 兼容 |
| ZCShou | 17 | 动态平台唯一化、axbuild/QEMU 配置、文档与 CI |
| Mr. why (silicalet) | 10 | Nix/Nixpkgs、mount tree、cgroup namespace、进程与网络语义 |
| Shi Lei | 9 | might-sleep 检查、syscall conformance、atomic-context 与 scope 修复 |
| github-actions[bot] | 7 | release-plz 自动发布与版本维护 |
| Josen-B | 7 | Axvisor 模拟设备框架、AxLoader、诊断、测试与 CI |
| YanLien | 6 | LTP、LS2K1000、someboot CPU topology 与 RK3576 |
| szy | 5 | TTY/USB、板级测试与 Axvisor Orange Pi guest |
| Joseph Joshua Anggita、Antareske | 4/人 | RK3588 加速器/DVFS、网络统计与应用配置 |
| 其他贡献者 | 若干 | vCPU、StarryOS、驱动、板卡、测试与文档修复 |

---

## 一、仓库设施

### 动态平台成为统一的平台实现

7 月初完成了动态平台迁移的最后一步。[PR #1478](https://github.com/rcore-os/tgoskits/pull/1478) 一次性移除静态平台与 `axconfig` 代码生成，之后所有平台都使用 dynamic platform；[PR #1463](https://github.com/rcore-os/tgoskits/pull/1463) 随后删除 `ax-driver` 中为静态平台保留的旧代码，[PR #1466](https://github.com/rcore-os/tgoskits/pull/1466) 为 ArceOS 补上动态板级流程。平台模块和 feature 也重新整理：someboot/somehal macros 被放回更合适的位置，`ax-feat` 被移除，原先集中在其中的选项改由运行时、API 与用户库分别管理。

- [PR #1478](https://github.com/rcore-os/tgoskits/pull/1478) — 移除静态平台和 `axconfig` generation，动态平台成为唯一路径
- [PR #1463](https://github.com/rcore-os/tgoskits/pull/1463)、[PR #1474](https://github.com/rcore-os/tgoskits/pull/1474) — 清理 ax-driver 与 Axvisor std 的静态兼容面
- [PR #1466](https://github.com/rcore-os/tgoskits/pull/1466) — ArceOS dynamic board flow
- [PR #1485](https://github.com/rcore-os/tgoskits/pull/1485) — 调整 someboot / somehal-macros 位置并补充文档
- [PR #1513](https://github.com/rcore-os/tgoskits/pull/1513) — 移除 `ax-feat`，将选项重新分配给运行时、API 与用户库
- [PR #1613](https://github.com/rcore-os/tgoskits/pull/1613) — ArceOS QEMU 配置布局与 StarryOS/Axvisor 统一
- [PR #1620](https://github.com/rcore-os/tgoskits/pull/1620) — axbuild 改为用明确配置控制构建与启动
- [PR #1503](https://github.com/rcore-os/tgoskits/pull/1503)、[PR #1617](https://github.com/rcore-os/tgoskits/pull/1617) — qemu-user staging symlink 与 RISC-V global-pointer relaxation 修复
- [PR #1421](https://github.com/rcore-os/tgoskits/pull/1421)、[PR #1619](https://github.com/rcore-os/tgoskits/pull/1619) — 移除 vendored spin 并更新到 workspace dependency

这组改动的意义不只是删除旧文件。到月底，平台信息、构建配置、运行时选项和各操作系统的接入代码都有了更清楚的归属。今后新增板卡或架构时，不再需要同时维护 static/dynamic 两套配置，也更不容易因 feature 别名或命令自动推导而出现不同组合行为不一致的问题。

### axbuild、测试编排与 CI

测试基础设施不再只是增加更多 CI 任务，而是开始重点检查“测试是否真正运行并成功”。ArceOS 新增系统级 QEMU 启动测试，axtest 简化内核测试目标；Starry 的 SMP1/SMP4 QEMU 配置合并为统一入口，LTP 系统调用基础测试进入 `dev`，并新增大规模内核行为回归测试。月底，QEMU 必须输出预期的成功标记才算通过，Starry ktest 构建选项、aarch64 配置检查和 test-suit 日志也得到修复，避免出现“QEMU 已退出，但目标程序其实没有成功”或“测试配置没有真正参与构建，却被判定通过”的情况。

- [PR #1365](https://github.com/rcore-os/tgoskits/pull/1365)、[PR #1470](https://github.com/rcore-os/tgoskits/pull/1470) — ArceOS QEMU 基础启动测试与 axtest 目标简化
- [PR #1544](https://github.com/rcore-os/tgoskits/pull/1544) — Starry SMP1/SMP4 QEMU 测试入口统一
- [PR #1561](https://github.com/rcore-os/tgoskits/pull/1561) — LTP 系统调用基础测试应用
- [PR #1674](https://github.com/rcore-os/tgoskits/pull/1674) — 扩展 Starry 内核行为的 axtest 回归覆盖
- [PR #1459](https://github.com/rcore-os/tgoskits/pull/1459)、[PR #1521](https://github.com/rcore-os/tgoskits/pull/1521) — 关闭 self-hosted Rust cache，并恢复 Starry ptrace / Axvisor RISC-V 测试
- [PR #1593](https://github.com/rcore-os/tgoskits/pull/1593)、[PR #1672](https://github.com/rcore-os/tgoskits/pull/1672) — 补全 workspace 源码路径过滤并回收覆盖率产物
- [PR #1626](https://github.com/rcore-os/tgoskits/pull/1626) — Rust nightly 更新至 2026-07-15
- [PR #1628](https://github.com/rcore-os/tgoskits/pull/1628) — Axvisor SVM 迁移到 self-hosted AMD runner
- [PR #1719](https://github.com/rcore-os/tgoskits/pull/1719)、[PR #1777](https://github.com/rcore-os/tgoskits/pull/1777)、[PR #1778](https://github.com/rcore-os/tgoskits/pull/1778)、[PR #1779](https://github.com/rcore-os/tgoskits/pull/1779) — QEMU 成功标记、Starry ktest feature、aarch64 配置 lint 与日志修复
- [PR #1457](https://github.com/rcore-os/tgoskits/pull/1457)、[PR #1636](https://github.com/rcore-os/tgoskits/pull/1636) — 移除 SG2002 board job，并暂时关闭 Axvisor x86_64 UEFI self-hosted job
- [PR #1475](https://github.com/rcore-os/tgoskits/pull/1475) — 修复 publish dry-run 的 locked `ax-errno`
- [PR #1627](https://github.com/rcore-os/tgoskits/pull/1627) — 生成 HTML 测试报告并补充 CI 覆盖率
- [PR #1677](https://github.com/rcore-os/tgoskits/pull/1677)、[PR #1698](https://github.com/rcore-os/tgoskits/pull/1698) — 延后 OVMF 安装并增强 AxLoader HTTP 基础测试的稳定性

release-plz 在 7 月完成 7 轮自动 release 维护；同时，axbuild 增加 session 共享板级文件能力，使 Linux stage 与 Starry/board 测试可以使用同一份输入文件，减少板测前置部署路径的差异。

- [PR #1461](https://github.com/rcore-os/tgoskits/pull/1461)、[PR #1479](https://github.com/rcore-os/tgoskits/pull/1479)、[PR #1537](https://github.com/rcore-os/tgoskits/pull/1537)、[PR #1539](https://github.com/rcore-os/tgoskits/pull/1539)、[PR #1548](https://github.com/rcore-os/tgoskits/pull/1548)、[PR #1549](https://github.com/rcore-os/tgoskits/pull/1549)、[PR #1665](https://github.com/rcore-os/tgoskits/pull/1665) — 自动发布维护
- [PR #1701](https://github.com/rcore-os/tgoskits/pull/1701) — session-shared board files

### 文档、开发规范与审查要求

7 月新增完整的代码质量手册，并把 Rust 编码规范、新功能风险评估、syscall 标准来源、应用实际运行验证和优先复用已有实现等要求写入仓库规则与 PR 审查流程。文档侧还重构了 axbuild、驱动框架和快速开始内容，补上 6 月月报与站内搜索配置。

- [PR #1460](https://github.com/rcore-os/tgoskits/pull/1460)、[PR #1551](https://github.com/rcore-os/tgoskits/pull/1551)、[PR #1673](https://github.com/rcore-os/tgoskits/pull/1673) — Rust coding standards、coding guideline handbook 与 feature review standards
- [PR #1511](https://github.com/rcore-os/tgoskits/pull/1511)、[PR #1633](https://github.com/rcore-os/tgoskits/pull/1633) — 补充审查角度与系统调用基准测试说明
- [PR #1615](https://github.com/rcore-os/tgoskits/pull/1615)、[PR #1625](https://github.com/rcore-os/tgoskits/pull/1625) — 离线 PR 审查基准与同模型评分流程
- [PR #1667](https://github.com/rcore-os/tgoskits/pull/1667)、[PR #1669](https://github.com/rcore-os/tgoskits/pull/1669) — syscall 标准来源要求及规则调整
- [PR #1723](https://github.com/rcore-os/tgoskits/pull/1723)、[PR #1727](https://github.com/rcore-os/tgoskits/pull/1727)、[PR #1765](https://github.com/rcore-os/tgoskits/pull/1765) — 要求应用实际运行、处理遗留 TODO，并优先复用已有实现
- [PR #1469](https://github.com/rcore-os/tgoskits/pull/1469)、[PR #1472](https://github.com/rcore-os/tgoskits/pull/1472)、[PR #1487](https://github.com/rcore-os/tgoskits/pull/1487)、[PR #1634](https://github.com/rcore-os/tgoskits/pull/1634) — 网络、axbuild、驱动架构与 quickstart 文档重构
- [PR #1543](https://github.com/rcore-os/tgoskits/pull/1543)、[PR #1554](https://github.com/rcore-os/tgoskits/pull/1554)、[PR #1560](https://github.com/rcore-os/tgoskits/pull/1560) — review 索引、module layout 与 MAINTAINERS reviewer flow
- [PR #1581](https://github.com/rcore-os/tgoskits/pull/1581)、[PR #1582](https://github.com/rcore-os/tgoskits/pull/1582)、[PR #1605](https://github.com/rcore-os/tgoskits/pull/1605) — LS2K1000、SG2002、VisionFive 2 与 Axvisor quickstart 修复
- [PR #1614](https://github.com/rcore-os/tgoskits/pull/1614)、[PR #1664](https://github.com/rcore-os/tgoskits/pull/1664) — 站点搜索配置与 MAINTAINERS 目录归属调整
- [PR #1552](https://github.com/rcore-os/tgoskits/pull/1552) — 2026 年 6 月开发月报

---

## 二、Axvisor

### AxVM 启动、地址空间与架构职责调整

Axvisor 是 7 月调整最密集的子系统。月初多项相互衔接的 PR 重新划分了职责：AxVM 负责虚拟机地址规划、内存和启动准备、vCPU 创建、FDT 处理与二级页表；Axvisor 上层只保留配置读取、虚拟机管理和宿主机生命周期。不同架构各自特有的处理也回到对应的架构模块，不再散落在 Axvisor 主流程中。

- [PR #1454](https://github.com/rcore-os/tgoskits/pull/1454) — 统一虚拟机地址布局和直通映射预留区
- [PR #1462](https://github.com/rcore-os/tgoskits/pull/1462) — 将虚拟机启动与内存准备移入 AxVM
- [PR #1467](https://github.com/rcore-os/tgoskits/pull/1467) — 拆分各架构 vCPU 实现，移除统一的 `axvcpu` crate
- [PR #1471](https://github.com/rcore-os/tgoskits/pull/1471) — 将 Axvisor 中的架构相关逻辑移入 AxVM `ArchOps`
- [PR #1476](https://github.com/rcore-os/tgoskits/pull/1476) — FDT 处理改用 `fdt-edit`
- [PR #1477](https://github.com/rcore-os/tgoskits/pull/1477) — 统一二级页表，并补齐 RISC-V `Sv39x4` / `Sv48x4`
- [PR #1528](https://github.com/rcore-os/tgoskits/pull/1528) — 由各架构适配层处理 vCPU 退出
- [PR #1562](https://github.com/rcore-os/tgoskits/pull/1562) — 合并并清理分散的架构相关代码

四种架构的后端随后进一步减少了对具体操作系统的依赖，并整理了寄存器和退出原因类型。x86_64 又把 VMX/SVM 从构建时选择改为启动时通过 CPUID 探测，使同一个 Axvisor 构建可以在 Intel 与 AMD 主机上自动选择正确的虚拟化后端。

- [PR #1523](https://github.com/rcore-os/tgoskits/pull/1523) — AArch64 vCPU 不再依赖具体宿主机接口
- [PR #1550](https://github.com/rcore-os/tgoskits/pull/1550)、[PR #1629](https://github.com/rcore-os/tgoskits/pull/1629) — x86 vCPU 减少 OS 依赖，并在运行时选择 VMX/SVM
- [PR #1553](https://github.com/rcore-os/tgoskits/pull/1553) — 整理 LoongArch vCPU 寄存器类型并拆分 AxVM 接入代码
- [PR #1556](https://github.com/rcore-os/tgoskits/pull/1556) — 整理 RISC-V vCPU 接入和退出处理

### 更明确的错误、虚拟中断与模拟设备

在各模块职责稳定后，AxVM、虚拟化后端、地址空间、设备、配置和 hypercall 不再只返回通用 errno 或字符串，而是改用各模块自己定义的错误类型。这样上层可以分清配置错误、硬件不支持、设备访问失败和运行状态异常，并采取不同的处理方式。

- [PR #1590](https://github.com/rcore-os/tgoskits/pull/1590)、[PR #1591](https://github.com/rcore-os/tgoskits/pull/1591) — 为 AxVM 和各架构后端定义各自的错误类型
- [PR #1592](https://github.com/rcore-os/tgoskits/pull/1592)、[PR #1595](https://github.com/rcore-os/tgoskits/pull/1595) — axaddrspace / axdevice 使用明确的错误类型
- [PR #1597](https://github.com/rcore-os/tgoskits/pull/1597)、[PR #1599](https://github.com/rcore-os/tgoskits/pull/1599) — axvmconfig / axhvc 整理错误返回规则

虚拟中断路径增加了每个 vCPU 独立的分发队列和 `VmInterruptSender`，发送中断的一方不再需要了解具体 vCPU 或架构后端。月底的统一模拟设备框架又集中管理了设备配置、创建、注册和运行流程；DMA、定时器、唤醒 vCPU、停止 VM 等敏感操作只授权给确实需要它们的设备。架构层因此不再直接创建和持有普通模拟设备。

- [PR #1661](https://github.com/rcore-os/tgoskits/pull/1661) — 虚拟中断模型与每个 vCPU 独立的分发队列
- [PR #1679](https://github.com/rcore-os/tgoskits/pull/1679) — `VmInterruptSender` 与 `VmRuntimeHandle` 集成
- [PR #1722](https://github.com/rcore-os/tgoskits/pull/1722) — 统一模拟设备框架
- [PR #1770](https://github.com/rcore-os/tgoskits/pull/1770) — AArch64 物理定时器状态虚拟化
- [PR #1776](https://github.com/rcore-os/tgoskits/pull/1776)、[PR #1791](https://github.com/rcore-os/tgoskits/pull/1791) — 宿主机测试避免执行特权 IRQ 操作，并按需启用测试功能

### 板级虚拟机、加载器与诊断

AxLoader 和 Asus NUC15CRH 支持继续修复，Orange Pi 5 Plus 新增运行 StarryOS 虚拟机的场景；宿主机侧增加可选的 panic 调用栈，独立使用的 xtask 命令和 SVM 启动测试超时也得到修正。这些工作让调整后的 AxVM 不只通过库内测试，还实际覆盖 x86 主机、AArch64 开发板和虚拟机启动流程。

- [PR #1555](https://github.com/rcore-os/tgoskits/pull/1555) — AxLoader 与 Asus NUC15CRH 支持增强
- [PR #1611](https://github.com/rcore-os/tgoskits/pull/1611) — 为 SVM 启动测试设置明确的超时时间
- [PR #1653](https://github.com/rcore-os/tgoskits/pull/1653) — Axvisor 宿主机可选输出 panic 调用栈
- [PR #1684](https://github.com/rcore-os/tgoskits/pull/1684) — Orange Pi 5 Plus 上运行 StarryOS 虚拟机
- [PR #1651](https://github.com/rcore-os/tgoskits/pull/1651) — 修复独立使用 xtask 命令时的兼容问题

---

## 三、ArceOS

### 调度、CPU-local 与运行时上下文

ArceOS 在 7 月集中处理“当前代码是在普通任务、中断还是调度过程中运行，以及此时允许做哪些操作”。`might_sleep` 检查覆盖得到增强并进入 std CI；跨核唤醒不再原地等待另一个 CPU 退出 `on_cpu` 状态，而是记录请求后延迟处理。RR 调度器不再把刚唤醒的任务插到队首，新任务也会在进入调度器前完成初始化。`cpu-local` 则明确由每个 CPU 管理自己的寄存器状态，中断代码查询当前 CPU 时也不会在执行期间切换到其他 CPU。

- [PR #1480](https://github.com/rcore-os/tgoskits/pull/1480)、[PR #1689](https://github.com/rcore-os/tgoskits/pull/1689) — might-sleep 检查增强并进入 std CI
- [PR #1495](https://github.com/rcore-os/tgoskits/pull/1495) — 跨核唤醒改为记录请求后延迟处理
- [PR #1532](https://github.com/rcore-os/tgoskits/pull/1532) — 避免 RR 调度器把刚唤醒的任务插到队首
- [PR #1662](https://github.com/rcore-os/tgoskits/pull/1662) — 明确每个 CPU 的寄存器状态归属
- [PR #1675](https://github.com/rcore-os/tgoskits/pull/1675) — ax-runtime 统一安排 UART 输出任务
- [PR #1695](https://github.com/rcore-os/tgoskits/pull/1695) — 控制台排队日志恢复 CRLF 换行
- [PR #1682](https://github.com/rcore-os/tgoskits/pull/1682) — 提前初始化 scope-local 数据
- [PR #1721](https://github.com/rcore-os/tgoskits/pull/1721) — 中断执行期间固定当前 CPU
- [PR #1783](https://github.com/rcore-os/tgoskits/pull/1783) — task 在调度前完成初始化
- [PR #1798](https://github.com/rcore-os/tgoskits/pull/1798) — 防止访问尚未安装的宿主机 CPU-local 区域

### someboot、SMP 与多架构启动

所有平台统一使用动态配置后，someboot 开始在启动时建立 CPU 编号映射，修复固件列出 CPU 的顺序与实际启动 CPU 不一致的问题。AArch64 修复了 64 位定时器截止时间和 EFI 交接，x86 修复了 LAPIC 定时器间隔上限；AArch64 也补上层级 MSI-X 中断域注册，为 PCIe 设备和后续多队列中断提供基础。

- [PR #1522](https://github.com/rcore-os/tgoskits/pull/1522)、[PR #1526](https://github.com/rcore-os/tgoskits/pull/1526) — AArch64 MSI-X 注册与层级中断域
- [PR #1710](https://github.com/rcore-os/tgoskits/pull/1710) — someboot 在启动时建立 CPU 编号映射
- [PR #1720](https://github.com/rcore-os/tgoskits/pull/1720) — AArch64 定时器截止时间使用 64 位宽度
- [PR #1782](https://github.com/rcore-os/tgoskits/pull/1782) — 修复 AArch64 EFI handoff
- [PR #1794](https://github.com/rcore-os/tgoskits/pull/1794) — 限制 x86 LAPIC 定时器间隔

---

## 四、StarryOS

7 月 StarryOS 的重点从单个应用测试扩展到发行版、包管理器和浏览器运行环境。Nix/Nixpkgs、StarryWRT/OpenWrt、语言工具链、监控与网关、科学计算和图形应用同时检验了 syscall、mount namespace、procfs、网络、TTY、文件系统和调度器，推动这些子系统在完整应用场景中配合工作。

### 进程、信号与 Linux syscall 兼容性

月初根据 LTP 测试连续修正了系统调用参数、错误码和各种边界情况。月底又集中补齐浏览器需要的线程 TLS 与退出清理、JIT 内存权限切换、`madvise` 内存回收、memfd 封印、SEQPACKET 消息边界、SCM_RIGHTS 文件描述符传递、批量收发消息、epoll 边缘触发、非阻塞 splice 和并发连接。POSIX 消息队列也从空实现发展为完整的 `mq_*` 系统调用。

- [PR #1464](https://github.com/rcore-os/tgoskits/pull/1464)、[PR #1488](https://github.com/rcore-os/tgoskits/pull/1488) — 修复 LTP 发现的系统调用差异和参数检查
- [PR #1514](https://github.com/rcore-os/tgoskits/pull/1514)、[PR #1517](https://github.com/rcore-os/tgoskits/pull/1517) — nanosleep、shm attach、path/random/ICMP 行为
- [PR #1505](https://github.com/rcore-os/tgoskits/pull/1505) — read-only mmap `fdatasync` 与 packet-info socket options
- [PR #1564](https://github.com/rcore-os/tgoskits/pull/1564) — POSIX message queues (`mq_*`)
- [PR #1631](https://github.com/rcore-os/tgoskits/pull/1631)、[PR #1678](https://github.com/rcore-os/tgoskits/pull/1678) — Linux syscall 行为、socket 与 seccomp flag validation
- [PR #1569](https://github.com/rcore-os/tgoskits/pull/1569) — 浏览器前置 syscall/内存/IPC/event-loop 能力与四架构测试
- [PR #1531](https://github.com/rcore-os/tgoskits/pull/1531)、[PR #1558](https://github.com/rcore-os/tgoskits/pull/1558) — pipe endpoint state、`/proc/pid/comm` 与 partial TCP send

进程生命周期和同步异常递送也持续加固：group exit 会唤醒被阻塞的 sibling，orphan reparent 期间保持可见；`CLONE_PARENT` exit signal 得到支持，同步 fault signal 优先于普通 pending signal；zombie 被 reap 前保留 PID identity，`SIGKILL` 在 ptrace release 前发布，减少 wait/ptrace/进程表之间的竞态。

- [PR #1500](https://github.com/rcore-os/tgoskits/pull/1500)、[PR #1535](https://github.com/rcore-os/tgoskits/pull/1535) — group-exit sibling wake 与 orphan reparent visibility
- [PR #1641](https://github.com/rcore-os/tgoskits/pull/1641) — `CLONE_PARENT` exit signals
- [PR #1700](https://github.com/rcore-os/tgoskits/pull/1700) — synchronous fault signal priority
- [PR #1706](https://github.com/rcore-os/tgoskits/pull/1706) — zombie PID identity 生命周期
- [PR #1801](https://github.com/rcore-os/tgoskits/pull/1801) — ptrace release 前发布 `SIGKILL`

### namespace、mount、procfs 与内存/文件系统

Nix sandbox 推动了一组重要的 namespace 和 mount tree 改进。VFS 开始记录挂载点 ID、父子关系、标志和传播关系，`mountinfo` 改为根据目标进程所在的 namespace 动态生成；`pivot_root`、bind mount、shared/slave propagation 和能够整体回滚的 unmount 流程也得到完整测试。cgroup namespace 则接入 clone、unshare、setns、namespace fd 与 procfs。

- [PR #1644](https://github.com/rcore-os/tgoskits/pull/1644) — 完善挂载树、mountinfo、bind/pivot_root 与挂载传播行为
- [PR #1642](https://github.com/rcore-os/tgoskits/pull/1642) — cgroup namespace 与 `/proc/<pid>/ns/cgroup`
- [PR #1538](https://github.com/rcore-os/tgoskits/pull/1538) — 避免 unshare 失败后破坏 scope 锁状态
- [PR #1637](https://github.com/rcore-os/tgoskits/pull/1637) — 支持 Nix 所需的 `openat2` 路径解析标志

procfs 中原先写死的占位内容继续改为读取内核实际状态，新增或修复 diskstats、net/dev、mounts、mountinfo、vmstat，以及进程的 environ、root、cwd 和 exe 等接口。内存和文件系统方面限制了 page cache 的预分配量以避免内存耗尽，拒绝访问文件末尾之后的私有 mmap 页面，并修复关机卸载文件系统、挂载回调执行时机、overlay 根目录锁和缓存文件扩容后补零等问题。

- [PR #1504](https://github.com/rcore-os/tgoskits/pull/1504)、[PR #1508](https://github.com/rcore-os/tgoskits/pull/1508)、[PR #1525](https://github.com/rcore-os/tgoskits/pull/1525) — 让磁盘、网络、挂载和内存统计反映实际状态
- [PR #1643](https://github.com/rcore-os/tgoskits/pull/1643)、[PR #1645](https://github.com/rcore-os/tgoskits/pull/1645) — 进程环境、路径链接与 `/proc/net/dev` 统计
- [PR #1499](https://github.com/rcore-os/tgoskits/pull/1499)、[PR #1534](https://github.com/rcore-os/tgoskits/pull/1534) — 避免 page cache 耗尽内存，并拒绝访问 mmap 文件末尾之后的页面
- [PR #1683](https://github.com/rcore-os/tgoskits/pull/1683)、[PR #1685](https://github.com/rcore-os/tgoskits/pull/1685) — 调整挂载回调执行时机并修复 overlay 根目录锁
- [PR #1711](https://github.com/rcore-os/tgoskits/pull/1711)、[PR #1790](https://github.com/rcore-os/tgoskits/pull/1790) — 修复关机卸载文件系统与缓存文件扩容后的补零

### Nix、StarryWRT 与应用兼容性测试

Nix 支持从无需沙箱的基础测试，扩展到由 Alpine `apk add nix` 提供的 `nix 2.31.5-r0`、固定版本的 nixpkgs 源码和二进制缓存，最终可以分别验证无沙箱、沙箱和 nixpkgs `hello`。这一过程同时发现并修复了文件描述符表死锁，以及 PTY、pidfd、管道轮询、挂载 namespace、文件打开后删除、文件锁和组合系统测试隔离等问题。StarryWRT 则把 OpenWrt 用户空间、UCI 和 opkg 带到 StarryOS，形成另一条发行版兼容路径。

- [PR #1125](https://github.com/rcore-os/tgoskits/pull/1125) — Nix 基础测试与配套内核回归测试
- [PR #1520](https://github.com/rcore-os/tgoskits/pull/1520) — 在 StarryOS 上启用 Nix 沙箱和 nixpkgs
- [PR #1580](https://github.com/rcore-os/tgoskits/pull/1580)、[PR #1579](https://github.com/rcore-os/tgoskits/pull/1579) — StarryWRT、OpenWrt UCI 与 opkg 兼容性测试
- [PR #1076](https://github.com/rcore-os/tgoskits/pull/1076) — x86_64 self-build 通过 Starry app 执行

批量应用兼容性测试继续覆盖 Java、Node、Python、LLVM、监控、网关、SSH、科学计算与并发模型。与 6 月主要验证“单个应用能否运行”相比，7 月开始同时测试同一语言的多个库、框架和编译后端，并根据程序的实际输出判断是否成功；其中不少测试可以在四种架构或多种架构配置中复用。

- [PR #1437](https://github.com/rcore-os/tgoskits/pull/1437)、[PR #1438](https://github.com/rcore-os/tgoskits/pull/1438) — Java J2SE/JSE 标准库与 JEE 框架测试
- [PR #1439](https://github.com/rcore-os/tgoskits/pull/1439)、[PR #1440](https://github.com/rcore-os/tgoskits/pull/1440) — Node Web 框架与常用库测试
- [PR #1498](https://github.com/rcore-os/tgoskits/pull/1498) — Python TUI 框架四架构测试
- [PR #1516](https://github.com/rcore-os/tgoskits/pull/1516)、[PR #1519](https://github.com/rcore-os/tgoskits/pull/1519) — LLVM 22 工具链与前后端测试矩阵
- [PR #1501](https://github.com/rcore-os/tgoskits/pull/1501)、[PR #1546](https://github.com/rcore-os/tgoskits/pull/1546)、[PR #1502](https://github.com/rcore-os/tgoskits/pull/1502) — 监控组件、网关与 Higress 反向代理
- [PR #1529](https://github.com/rcore-os/tgoskits/pull/1529) — Dropbear SSH 测试套件
- [PR #1570](https://github.com/rcore-os/tgoskits/pull/1570) — SciPy/SymPy、Numba、scikit-learn、pandas 科学计算测试
- [PR #1600](https://github.com/rcore-os/tgoskits/pull/1600) — 单核协作式并发正确性测试
- [PR #1785](https://github.com/rcore-os/tgoskits/pull/1785) — qperf 支持分析 Starry x86_64 性能
- [PR #1547](https://github.com/rcore-os/tgoskits/pull/1547) — LLVM 22 构建选项同步到 ax-runtime 命名空间
- [PR #1649](https://github.com/rcore-os/tgoskits/pull/1649)、[PR #1650](https://github.com/rcore-os/tgoskits/pull/1650) — Nginx/Apache 的 x86_64 与 LoongArch64 配置适配 axbuild 重构

### TTY、USB、perf 与运行时诊断

真实应用继续暴露终端和设备接口中的问题：termios 串口格式和 tty drain ioctl 得到实现，PTY 会在返回 EOF 前先交付已缓冲的数据，usbfs 补齐清除 halt 状态和关闭文件时的清理流程。perf/eBPF 代码也用有名称的常量替代了难以理解的数字，新增 BPF helper、非阻塞操作和回归记录；月底又将 perf 控制操作与 IRQ 输出分别加锁，减少两类操作相互等待。

- [PR #1484](https://github.com/rcore-os/tgoskits/pull/1484)、[PR #1638](https://github.com/rcore-os/tgoskits/pull/1638) — termios/tty drain 与 PTY 缓冲数据交付
- [PR #1655](https://github.com/rcore-os/tgoskits/pull/1655) — usbfs 清除 halt 状态与关闭清理
- [PR #1412](https://github.com/rcore-os/tgoskits/pull/1412)、[PR #1465](https://github.com/rcore-os/tgoskits/pull/1465) — perf/eBPF helper、非阻塞操作、回归记录与忙循环警告修复
- [PR #1793](https://github.com/rcore-os/tgoskits/pull/1793) — 分开保护 perf 控制操作与 IRQ 输出

### 图形、板级与硬件加速

RK3588 本月新增两条可直接供现有用户程序使用的驱动路径：硬件 JPEG 解码器通过 `/dev/mpp_service` 支持未经修改的 Rockchip MPP 程序，RGA2 通过 `/dev/rga` 和 dma-heap 完成二维复制、缩放与色彩转换；CPU 动态调频增加按负载调节频率和电压校准。Wayland GL 侧加入 Doom 测试，并修复 DRM 显示时没有正确处理每行实际跨度的问题。

- [PR #1456](https://github.com/rcore-os/tgoskits/pull/1456) — RK3588 VDPU720 JPEG 解码、MPP ABI 与 JPU-NPU dma-buf 路径
- [PR #1388](https://github.com/rcore-os/tgoskits/pull/1388) — RK3588 RGA2、`/dev/rga` 与 dma-heap
- [PR #1468](https://github.com/rcore-os/tgoskits/pull/1468) — RK3588 PWM sysfs
- [PR #1657](https://github.com/rcore-os/tgoskits/pull/1657) — RK3588 按负载动态调频与电压校准
- [PR #1415](https://github.com/rcore-os/tgoskits/pull/1415) — Doom Wayland GL 测试与 DRM 行跨度修复

SG2002 方向打通了 JPEG 解码、回放和推理性能测试，并新增 AKA-00 网球 YOLO 板级测试。LoongArch64 增加 LS2K1000 物理开发板支持；RK3576/Radxa ROCK 4D 也完成 SoC、时钟、电源域、U-Boot 和 StarryOS 板级配置的初始接入。不过，其自托管板级测试任务在月底暂时关闭，说明代码能够合入主线后，仍需继续解决硬件测试长期稳定运行的问题。

- [PR #1530](https://github.com/rcore-os/tgoskits/pull/1530)、[PR #1572](https://github.com/rcore-os/tgoskits/pull/1572) — AKA-00 YOLO 板测与 SG2002 推理性能测试
- [PR #1540](https://github.com/rcore-os/tgoskits/pull/1540)、[PR #1589](https://github.com/rcore-os/tgoskits/pull/1589)、[PR #1594](https://github.com/rcore-os/tgoskits/pull/1594) — SG2002 摄像头/JPU、缩放 JPEG 解码与回放流程
- [PR #1368](https://github.com/rcore-os/tgoskits/pull/1368)、[PR #1635](https://github.com/rcore-os/tgoskits/pull/1635) — LS2K1000 平台与 StarryOS board support
- [PR #1704](https://github.com/rcore-os/tgoskits/pull/1704)、[PR #1781](https://github.com/rcore-os/tgoskits/pull/1781) — RK3576 ROCK 4D 支持与暂时关闭 board test

---

## 五、组件、驱动与网络栈

### IRQ、平台资源与设备探测

通用驱动代码继续与各操作系统的接入代码分离。IRQ 回调改为由框架持有，somehal 会缓存每个 CPU 的 IRQ 路由；rdrive 在探测设备前应用默认引脚配置、电源域和指定时钟，并修复设备注册表锁在抢占场景下的安全问题。Rockchip reset 被拆成独立接口，DMA cache 同步则统一交给平台提供的 cache 操作完成。

- [PR #1452](https://github.com/rcore-os/tgoskits/pull/1452) — IRQ 框架持有回调函数
- [PR #1458](https://github.com/rcore-os/tgoskits/pull/1458)、[PR #1515](https://github.com/rcore-os/tgoskits/pull/1515)、[PR #1527](https://github.com/rcore-os/tgoskits/pull/1527) — 探测设备前应用 FDT 引脚、电源域与指定时钟配置
- [PR #1494](https://github.com/rcore-os/tgoskits/pull/1494) — somehal 缓存 CPU IRQ routes
- [PR #1630](https://github.com/rcore-os/tgoskits/pull/1630) — axdevice 注册独占 IRQ 线资源
- [PR #1509](https://github.com/rcore-os/tgoskits/pull/1509) — 拆分 Rockchip reset 接口
- [PR #1510](https://github.com/rcore-os/tgoskits/pull/1510) — 修复 rdrive 设备注册表锁在抢占时的安全问题
- [PR #1542](https://github.com/rcore-os/tgoskits/pull/1542) — DMA cache 同步统一使用平台接口

### SD/MMC、USB 与块设备多队列

月初，SD/MMC protocol 被拆成 SDIO 与 RDIF 两组接口模块，SG2002 SD、VisionFive2/JH7110 的 IRQ 驱动 DWMMC、RK3588 EHCI USB2 与 SG2002 DWC2 host 相继进入主线。virtio-blk 也从轮询设备完成状态改为等待 IRQ，为月底统一块设备运行方式做准备。

- [PR #1486](https://github.com/rcore-os/tgoskits/pull/1486) — 拆分 SDIO/RDIF 接口与 DMA 队列基础类型
- [PR #1482](https://github.com/rcore-os/tgoskits/pull/1482) — SG2002 SD 驱动
- [PR #1524](https://github.com/rcore-os/tgoskits/pull/1524) — JH7110 DWMMC 改用 IRQ 驱动
- [PR #1647](https://github.com/rcore-os/tgoskits/pull/1647) — DWMMC 响应寄存器使用 32 位 MMIO 读取
- [PR #1481](https://github.com/rcore-os/tgoskits/pull/1481)、[PR #1496](https://github.com/rcore-os/tgoskits/pull/1496) — RK3588 EHCI USB2 与 SG2002 DWC2 host/axtest
- [PR #1512](https://github.com/rcore-os/tgoskits/pull/1512) — virtio-blk 完成通知改为 IRQ 驱动

月底，[PR #1768](https://github.com/rcore-os/tgoskits/pull/1768) 完成了 7 月规模最大的块设备底层调整。`rdif-block` 让每个请求持有自己的 DMA 资源，并明确硬件队列如何接收、提交和归还请求；`ax-fs-ng` 为每个 CPU 设置有界软件队列，为每个硬件队列设置维护线程，同时补全成组完成、写回顺序、超时恢复和安全关闭流程。NVMe 可以一次提交一批请求，而队列深度只有 1 的 SD/eMMC 控制器仍然一次处理一个请求，不会为了看起来更快而重新加入轮询或虚构批量能力。QEMU 的宿主机和根文件系统磁盘随后统一迁移到 NVMe，使四种架构的 CI 与实际系统使用同一条纯 IRQ 块设备路径。

- [PR #1768](https://github.com/rcore-os/tgoskits/pull/1768) — IRQ 驱动的多队列块设备运行时与驱动迁移
- [PR #1784](https://github.com/rcore-os/tgoskits/pull/1784) — QEMU 块设备迁移到 NVMe
- [PR #1789](https://github.com/rcore-os/tgoskits/pull/1789) — cv181x-sdhci 使用 `tock-registers`
- [PR #1795](https://github.com/rcore-os/tgoskits/pull/1795) — 可复用的多盘 AHCI、NCQ 与共享 IRQ 运行方式
- [PR #1796](https://github.com/rcore-os/tgoskits/pull/1796) — 淘汰旧 axdma 释放流程

AHCI 进一步按控制器、端口和共享 IRQ 划分职责：可复用的驱动核心管理 HBA 和各端口的 DMA 资源，各操作系统只负责准备 MMIO、DMA 和 IRQ，并安排线程与通知。同一 HBA 上的多块磁盘可以分别注册为独立块设备，同时保留 `/dev/sdX`、PARTUUID 和 PARTLABEL 选择根盘的方式。

### ax-net 与 Linux 网络行为

网络栈同时改进设备配置、流量统计和 socket 行为。StarryOS 新增 rtnetlink IPv4 配置、`SO_REUSEPORT`、`IP_MTU_DISCOVER`、接口名查询，以及不同 socket 类型共用的设备 ioctl；ax-net 修复阻塞式 TCP 发送、Unix stream 写端关闭通知和 UDP 关闭前发送剩余数据。设备收发接口开始返回二层帧长度后，`/proc/net/dev` 也能显示真实的字节统计。

- [PR #1497](https://github.com/rcore-os/tgoskits/pull/1497) — rtnetlink IPv4 配置
- [PR #1518](https://github.com/rcore-os/tgoskits/pull/1518) — `SO_REUSEPORT`
- [PR #1533](https://github.com/rcore-os/tgoskits/pull/1533) — 阻塞式 TCP 发送持续到数据全部写入
- [PR #1571](https://github.com/rcore-os/tgoskits/pull/1571) — 设备收发接口返回二层帧长度
- [PR #1568](https://github.com/rcore-os/tgoskits/pull/1568) — `IP_MTU_DISCOVER` 与 UDP 关闭前发送剩余数据
- [PR #1639](https://github.com/rcore-os/tgoskits/pull/1639) — Unix stream 写端关闭状态通知另一端
- [PR #1640](https://github.com/rcore-os/tgoskits/pull/1640) — 接受没有变化的网络接口标志
- [PR #1707](https://github.com/rcore-os/tgoskits/pull/1707) — `SIOCGIFNAME` 与共用设备 ioctl
- [PR #1583](https://github.com/rcore-os/tgoskits/pull/1583) — RTL8125 正确声明千兆自动协商能力

---

## 总结

7 月的工作主要围绕以下几个方向展开：

1. **构建和平台路径统一**：静态平台、`axconfig` 生成代码和 `ax-feat` 退出主线，所有系统改用 dynamic platform、明确的 axbuild 配置和统一的 QEMU 布局。
2. **虚拟化模块分工更清楚**：AxVM 负责虚拟机地址布局、内存与启动准备、各架构 vCPU、NPT 和设备运行；错误类型、虚拟中断和设备授权方式也随之统一。
3. **SMP 和中断运行更稳定**：CPU-local、`might_sleep`、跨核唤醒、scope/IRQ 执行状态、someboot CPU 拓扑以及定时器/EFI 交接修复，减少了多核启动、调度和中断处理中的竞态与错误调用。
4. **StarryOS 开始支持更完整的发行版和浏览器运行环境**：Nix/Nixpkgs、StarryWRT、LTP、mount/cgroup namespace、POSIX 消息队列、procfs 和浏览器所需系统调用，推动 Linux 兼容从零散接口修复走向完整应用验证。
5. **开发板不再只验证启动**：RK3588 JPEG/RGA/动态调频、SG2002 JPU/推理、LS2K1000 与 RK3576 ROCK 4D 把板级工作扩展到媒体、AI、图形和性能管理。
6. **块设备改用纯 IRQ 多队列路径**：NVMe、SD/eMMC、AHCI 与文件系统运行时统一了 DMA 资源管理、硬件队列、IRQ 完成通知和关闭流程，QEMU 与开发板也开始测试同一套可复用实现。
7. **测试结果更能反映真实运行情况**：内核行为回归测试、批量应用兼容性测试、QEMU 成功标记、源码路径检查、开发规范和新功能审查要求，共同减少“测试显示通过但功能实际没有运行”以及后续修改破坏已有行为的情况。

感谢所有贡献者在 7 月的持续投入。
