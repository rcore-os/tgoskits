---
slug: august-2026-retrospective
title: 2026 年 8 月开发月报
date: 2026-09-01T23:00:00+08:00
authors: [tgoskits-team]
tags: [monthly-report, arceos, starryos, axvisor, axbuild, testing]
---

2026 年 8 月，TGOSKits 把 7 月划定的模块边界进一步落实到工作区布局、运行时语义和 CI 证据链上。全月 `dev` 分支共产生 **262 次非合并提交**，GitHub 仓库合并 **264 个 PR**，其中 **262 个直接进入 `dev`**，涉及 **27 个唯一提交作者邮箱**。本月的主要工作包括：按分层把文件系统、内存和驱动软件包迁入独立目录并统一依赖与锁原语；删除 `irq`/`multitask` 可选 feature，让中断与多任务成为固定能力；以解析后的设备图重建 Axvisor 虚拟设备路径，并把 AxVM 迁移到真实 Rust `std`；为 StarryOS 引入 NixOS Stage-2 用户态，并集中校准系统调用 ABI 的参数宽度与输入边界；把网络栈切换到队列级 NAPI 运行时；CI 则改为按改动影响选择检查项。

<!-- truncate -->

## 总览

| 指标 | 数据 |
|------|------|
| `dev` 非合并提交数 | 262 |
| 全仓库合并 PR 数 | 264 |
| 直接合入 `dev` 的 PR 数 | 262 |
| 唯一提交作者邮箱数 | 27 |
| 涉及 PR 编号范围 | #1565 ~ #2246 |

GitHub 的 264 个合并 PR 中，除 262 个 `dev` PR 外，还有 [PR #2208](https://github.com/rcore-os/tgoskits/pull/2208) 与 [PR #2212](https://github.com/rcore-os/tgoskits/pull/2212) 进入调度器重构工作分支 `codex/refactor-ax-task-from-1596`。

与 7 月相比，非合并提交从 217 增至 262，合并 PR 从 221 增至 264，唯一作者邮箱从 24 增至 27。本月有 31 项提交以 `refactor` 开头，其中相当一部分是整批删除：`irq`/`multitask` feature、旧分配器、`no_std` 兼容分支和重复的锁实现被移除；长期打开的调度器重构 [PR #1775](https://github.com/rcore-os/tgoskits/pull/1775) 的多个前置改动也在本月落地。

按“每个提交是否触及某一级目录”统计，8 月最活跃的区域如下：

| 一级目录 | 涉及提交数 | 主要内容 |
|----------|------------|----------|
| `os/` | 145 | StarryOS、ArceOS 内核与运行时、调度前置重构 |
| `test-suit/` | 113 | Starry/ArceOS 测试套件整合与新增 carpet |
| `scripts/` | 80 | axbuild/xtask、CI 脚本与镜像存储 |
| `docs/` | 72 | 架构文档重构与站点整理 |
| `virtualization/` | 61 | AxVM、vCPU、vPCI 与虚拟设备 |
| `drivers/` | 48 | 中断控制器、SD/MMC、USB 与串口 |
| `.github/` | 48 | 影响矩阵、清单路由与运行器 |
| `components/` | 37 | cpu-local、ax-sync、运行时与通用组件 |
| `platforms/` | 29 | someboot、串口与平台时钟 |
| `fs/` | 27 | ax-fs-ng、axfs-ng-vfs 与 rsext4 |
| `apps/` | 25 | NixOS、板级应用与基准测试 |
| `net/` | 17 | ax-net 队列级 NAPI 运行时 |
| `memory/` | 13 | 页表泛化、分配器收敛与 DMA |

### 贡献者排行

| 贡献者 | 提交数 | 主要方向 |
|--------|--------|----------|
| 周睿 (ZR233) | 99 | AxVM/Axvisor 设备图与 vCPU、运行时与调度前置重构、CI 影响矩阵与项目技能 |
| ZCShou | 30 | 工作区目录重组、文档重构、页表泛化与镜像存储 |
| Jingming Liang (eprogressing) | 30 | StarryOS 系统调用 ABI 参数宽度与边界校验系列 |
| Mr. why (silicalet) | 18 | NixOS Stage-2 用户态、挂载上下文、signalfd/pidfd 与 Unix socket |
| szy (bullhh) | 11 | Axvisor 浏览器控制台、guest UART 直通、Orange Pi 5 Plus 机器人流测试与 RK3588 guest |
| Joseph Joshua Anggita | 10 | futex 分片与快路径、COW 修复、调度器内省与系统调用性能 |
| Josen-B | 9 | AxLoader KVM/HTTP boot、IVC、rsext4 修复与模拟设备文档 |
| YanLien | 7 | 多 VM 控制台、guest FDT、GICv2 目标掩码与 ROCK 4D guest |
| YoungCloud | 6 | vPCI 基础、OVMF ACPI 验证、virtqueue 加固与 x86 MMIO 解码 |
| github-actions[bot] | 6 | release-plz 自动发布与版本维护 |
| Ivans (Ivans-11)、aptacc2421 | 5/人 | UVC/USB/TTY 修复；axum 管理面与 eventfd/epoll |
| 其他贡献者 | 若干 | SD/MMC、串口、板级测试、ABI 修复与文档 |

---

## 一、仓库设施

### 工作区按分层重组

8 月上半月完成了一轮工作区目录重组，使 crate 位置与 TGOSKits 分层约定一致：可复用的文件系统、内存和驱动软件包分别迁入 `fs/`、`memory/` 与 `drivers/`，StarryOS 专属的 `starry-process`、`starry-signal` 和 `starry-vm` 回到 `os/StarryOS`；两个旧分配器被整体删除，页表实现统一收敛到 `page-table-generic` 并改用通用命名。工作区依赖与锁原语同步统一：所有 crate 共享同一份依赖表，锁实现收敛进 `ax-sync` 并由 `ax-task` 复用，跨模块错误改为各领域自有的类型边界。

- [PR #1867](https://github.com/rcore-os/tgoskits/pull/1867) — 文件系统 crate 迁入 `fs/`
- [PR #1868](https://github.com/rcore-os/tgoskits/pull/1868)、[PR #1878](https://github.com/rcore-os/tgoskits/pull/1878) — `axalloc` 迁入 `memory/ax-alloc`，删除 `ax-allocator` 与 `bitmap-allocator`
- [PR #1911](https://github.com/rcore-os/tgoskits/pull/1911)、[PR #1937](https://github.com/rcore-os/tgoskits/pull/1937) — 页表执行统一到 `page-table-generic` 并改为通用命名
- [PR #1951](https://github.com/rcore-os/tgoskits/pull/1951) — 块设备与网络驱动 crate 迁入 `drivers/`
- [PR #1974](https://github.com/rcore-os/tgoskits/pull/1974) — `starry-process`、`starry-signal`、`starry-vm` 迁入 `os/StarryOS`
- [PR #1860](https://github.com/rcore-os/tgoskits/pull/1860)、[PR #1956](https://github.com/rcore-os/tgoskits/pull/1956)、[PR #1962](https://github.com/rcore-os/tgoskits/pull/1962) — 统一工作区依赖，锁原语收敛到 `ax-sync` 并移入 `ax-task`
- [PR #2024](https://github.com/rcore-os/tgoskits/pull/2024) — 引入领域自有的错误边界

这组迁移之后，crate 的目录位置即说明其复用层级，跨目录的循环引用和重复实现更容易在评审中被发现，新增组件也不再需要在多个位置之间选择落点。

### CI 影响矩阵与清单路由

CI 从“固定任务表”转向“声明清单 + 影响路由”。检查项迁移到 manifest v3 描述并模块化，PR 测试矩阵按改动影响选择要运行的检查；clippy 收窄到受影响目标，重复事件去重，`Cargo.lock` 作为软影响输入，受保护分支使用独立队列。镜像存储、运行器准备与 release 前置检查也得到整理，多起近期测试不稳定问题在同月修复。

- [PR #2097](https://github.com/rcore-os/tgoskits/pull/2097) — 按影响选择 PR 测试矩阵
- [PR #2105](https://github.com/rcore-os/tgoskits/pull/2105)、[PR #2069](https://github.com/rcore-os/tgoskits/pull/2069)、[PR #2114](https://github.com/rcore-os/tgoskits/pull/2114) — manifest v3 路由、检查模块化与 CI 工作流重构
- [PR #2025](https://github.com/rcore-os/tgoskits/pull/2025)、[PR #2013](https://github.com/rcore-os/tgoskits/pull/2013) — 镜像存储重构与 self-hosted 运行器本地存储路径
- [PR #2126](https://github.com/rcore-os/tgoskits/pull/2126)、[PR #2134](https://github.com/rcore-os/tgoskits/pull/2134)、[PR #2022](https://github.com/rcore-os/tgoskits/pull/2022)、[PR #2151](https://github.com/rcore-os/tgoskits/pull/2151) — 收窄 clippy 范围、host-test 目标 lint 与增量检出历史
- [PR #2122](https://github.com/rcore-os/tgoskits/pull/2122)、[PR #2140](https://github.com/rcore-os/tgoskits/pull/2140)、[PR #2136](https://github.com/rcore-os/tgoskits/pull/2136)、[PR #2143](https://github.com/rcore-os/tgoskits/pull/2143)、[PR #2144](https://github.com/rcore-os/tgoskits/pull/2144) — 事件分组去重、软影响输入与受保护分支队列
- [PR #2119](https://github.com/rcore-os/tgoskits/pull/2119)、[PR #2102](https://github.com/rcore-os/tgoskits/pull/2102)、[PR #2246](https://github.com/rcore-os/tgoskits/pull/2246) — 运行器规划、容器镜像与 initramfs 构建依赖
- [PR #1907](https://github.com/rcore-os/tgoskits/pull/1907)、[PR #2112](https://github.com/rcore-os/tgoskits/pull/2112)、[PR #2189](https://github.com/rcore-os/tgoskits/pull/2189)、[PR #2074](https://github.com/rcore-os/tgoskits/pull/2074) — QEMU smoke、aarch64 失败匹配、dev 测试与系统测试清理稳定性
- [PR #1969](https://github.com/rcore-os/tgoskits/pull/1969)、[PR #2018](https://github.com/rcore-os/tgoskits/pull/2018)、[PR #1964](https://github.com/rcore-os/tgoskits/pull/1964) — Axvisor 构建产物保留、rootfs 写入隔离与构建噪声
- [PR #2127](https://github.com/rcore-os/tgoskits/pull/2127)、[PR #1930](https://github.com/rcore-os/tgoskits/pull/1930)、[PR #1837](https://github.com/rcore-os/tgoskits/pull/1837) — 简化内部依赖发布检查，release 前统一 `ax-errno` 并防止过期依赖锁
- [PR #1939](https://github.com/rcore-os/tgoskits/pull/1939)、[PR #1959](https://github.com/rcore-os/tgoskits/pull/1959) — Axvisor 测试任务均衡与 ovmf-acpi-svm 迁至 self-hosted AMD CI

release-plz 在 8 月完成 6 轮自动 release 维护，发布流程与上述门禁衔接保持稳定。

### 测试套件整合与项目技能

StarryOS 与 ArceOS 的测试套件合并为统一布局，axtest 标准化 Cargo 与 QEMU 测试流程，宿主层与 QEMU 层测试分离。仓库指南同步迁移为按任务语义组织的项目技能：开发规范、feature 评审、syscall 兼容与 humanizer 等技能取代原先分散的指南文档，本地验证入口统一为 `cargo xtask` 命令族；PR 审查流程改为以当前提交的 CI 结果为第一证据。

- [PR #2173](https://github.com/rcore-os/tgoskits/pull/2173) — 合并 Starry 与 ArceOS 测试套件
- [PR #2088](https://github.com/rcore-os/tgoskits/pull/2088)、[PR #2240](https://github.com/rcore-os/tgoskits/pull/2240) — 标准化 axtest 的 Cargo/QEMU 流程，分离宿主与 QEMU 测试层
- [PR #2019](https://github.com/rcore-os/tgoskits/pull/2019) — 受限 TCG 下守护任务 IPI 测试进度
- [PR #2095](https://github.com/rcore-os/tgoskits/pull/2095)、[PR #1886](https://github.com/rcore-os/tgoskits/pull/1886) — feature 开发与 syscall 兼容指南、humanizer 技能
- [PR #2236](https://github.com/rcore-os/tgoskits/pull/2236)、[PR #2215](https://github.com/rcore-os/tgoskits/pull/2215)、[PR #2216](https://github.com/rcore-os/tgoskits/pull/2216)、[PR #2218](https://github.com/rcore-os/tgoskits/pull/2218) — 指南迁移为项目技能、布局标准化与 xtask 验证命令
- [PR #2111](https://github.com/rcore-os/tgoskits/pull/2111)、[PR #1960](https://github.com/rcore-os/tgoskits/pull/1960)、[PR #2187](https://github.com/rcore-os/tgoskits/pull/2187)、[PR #2177](https://github.com/rcore-os/tgoskits/pull/2177) — 中文审查指引、单 PR 审查流程与以 CI 为准的验证

### 文档重构

站点文档删并了组件库与开发指南类页面，按文件系统、内存与网络、Axvisor guest 与模拟设备等主题重写为架构参考；7 月月报也在 8 月初发布。

- [PR #2198](https://github.com/rcore-os/tgoskits/pull/2198) — 文件系统架构完整文档
- [PR #2104](https://github.com/rcore-os/tgoskits/pull/2104) — ax-net 网络与内存架构图
- [PR #2103](https://github.com/rcore-os/tgoskits/pull/2103)、[PR #2084](https://github.com/rcore-os/tgoskits/pull/2084) — Axvisor guest 架构拆分与模拟设备框架参考
- [PR #2099](https://github.com/rcore-os/tgoskits/pull/2099)、[PR #2083](https://github.com/rcore-os/tgoskits/pull/2083)、[PR #2087](https://github.com/rcore-os/tgoskits/pull/2087)、[PR #1943](https://github.com/rcore-os/tgoskits/pull/1943) — 文档一致性、内存架构与侧边栏、运行时文档合并与站点结构精简
- [PR #1792](https://github.com/rcore-os/tgoskits/pull/1792)、[PR #1828](https://github.com/rcore-os/tgoskits/pull/1828) — 架构文档目录化与 AxVM 生命周期权威文档
- [PR #1858](https://github.com/rcore-os/tgoskits/pull/1858) — 2026 年 7 月开发月报

---

## 二、Axvisor

### 设备图与虚拟设备注册

8 月 Axvisor 最大的结构变化是虚拟设备路径改为从“解析后的设备图”构建：设备声明、MMIO/PIO/IRQ 资源规划、固件数据边界和各架构接入统一到一处，固定资源冲突有统一的所有者诊断，构建失败可按整 VM 回滚。虚拟设备注册随之统一，guest 设备、AArch64 定时器与宿主机定时器的所有权也被重新划归 AxVM。

- [PR #1718](https://github.com/rcore-os/tgoskits/pull/1718) — 从解析后的设备图构建 VM
- [PR #2138](https://github.com/rcore-os/tgoskits/pull/2138) — 统一虚拟设备注册
- [PR #1717](https://github.com/rcore-os/tgoskits/pull/1717)、[PR #2190](https://github.com/rcore-os/tgoskits/pull/2190) — 统一 guest 设备与 AArch64/宿主机定时器所有权
- [PR #2121](https://github.com/rcore-os/tgoskits/pull/2121) — 架构能力分层
- [PR #2207](https://github.com/rcore-os/tgoskits/pull/2207) — 生成的 guest FDT 排除已禁用设备

### AxVM 迁移到真实 std

AxVM 从混合 `core`/`alloc`/mini-std 的状态调整为仅支持真实 Rust `std` 的 crate：集合、内存对象、格式化、线程、`Mutex` 与 `OnceLock` 统一使用 `std`，并为标准 mutex 增加统一的 poison 恢复扩展。配套地清除了宿主机依赖、废弃配置与遗留 guest 支持。

- [PR #1910](https://github.com/rcore-os/tgoskits/pull/1910) — AxVM 迁移到真实 Rust `std`
- [PR #1861](https://github.com/rcore-os/tgoskits/pull/1861)、[PR #1866](https://github.com/rcore-os/tgoskits/pull/1866)、[PR #1972](https://github.com/rcore-os/tgoskits/pull/1972) — 清理宿主机依赖、移除 NimbOS guest 与遗留 CI、删除过时配置

### 管理面、控制台与 IVC

Axvisor 的外部接口本月明显增强：新增基于 axum 的管理 HTTP 控制面和 VM 暂停/恢复端点，浏览器控制台让 guest 输出可以直接在网页中查看，多 VM 场景下控制台复用得到改进；虚拟机间通信补齐了演示与协议增强。

- [PR #1909](https://github.com/rcore-os/tgoskits/pull/1909) — axum 管理 HTTP 控制面与 VM 生命周期 API
- [PR #2098](https://github.com/rcore-os/tgoskits/pull/2098) — VM 暂停/恢复生命周期端点
- [PR #2211](https://github.com/rcore-os/tgoskits/pull/2211)、[PR #2241](https://github.com/rcore-os/tgoskits/pull/2241)、[PR #2195](https://github.com/rcore-os/tgoskits/pull/2195) — 浏览器控制台、投递稳定性与 CRLF 输出
- [PR #1912](https://github.com/rcore-os/tgoskits/pull/1912) — 多 VM guest 控制台复用
- [PR #1834](https://github.com/rcore-os/tgoskits/pull/1834) — 虚拟机间通信演示与协议增强
- [PR #1862](https://github.com/rcore-os/tgoskits/pull/1862)、[PR #1616](https://github.com/rcore-os/tgoskits/pull/1616)、[PR #1607](https://github.com/rcore-os/tgoskits/pull/1607) — shell 采用 shlex 分词与文件系统命令、镜像布局修正

### vCPU、虚拟中断与多架构修复

设备访问现在绑定到发起访问的 vCPU，RISC-V 的 VSEIP 在未绑定期间保持，LoongArch 级联中断的所有权得到保留。x86 侧能在 NPF/EPT 上解码任意宽度的设备 MMIO 访问，保留 VMX MSR 并取消被丢弃的 APIC 定时器；AArch64 修正 HVC 异常 PC 与 EL2 使能发布时序。并发方面修复了 machine 锁与等待队列的锁序反转、双 guest 启动唤醒竞态等。

- [PR #2092](https://github.com/rcore-os/tgoskits/pull/2092) — 设备访问绑定到发起访问的 vCPU
- [PR #2090](https://github.com/rcore-os/tgoskits/pull/2090)、[PR #2093](https://github.com/rcore-os/tgoskits/pull/2093) — RISC-V VSEIP 未绑定期间保持、LoongArch 级联 IRQ 所有权
- [PR #1681](https://github.com/rcore-os/tgoskits/pull/1681)、[PR #1692](https://github.com/rcore-os/tgoskits/pull/1692) — SMP guest 虚拟中断注入与 PSCI_VERSION hypercall
- [PR #2082](https://github.com/rcore-os/tgoskits/pull/2082)、[PR #2133](https://github.com/rcore-os/tgoskits/pull/2133) — x86 任意宽度 MMIO 解码、VMX MSR 保留与 APIC 定时器取消
- [PR #1953](https://github.com/rcore-os/tgoskits/pull/1953)、[PR #2191](https://github.com/rcore-os/tgoskits/pull/2191)、[PR #2132](https://github.com/rcore-os/tgoskits/pull/2132) — HVC 异常 PC、EL2 使能发布与 RISC-V 宿主机状态保持
- [PR #2145](https://github.com/rcore-os/tgoskits/pull/2145)、[PR #2168](https://github.com/rcore-os/tgoskits/pull/2168)、[PR #2020](https://github.com/rcore-os/tgoskits/pull/2020)、[PR #1966](https://github.com/rcore-os/tgoskits/pull/1966) — 锁序反转、运行时通知锁、vm_show 嵌套锁与双 guest 启动竞态
- [PR #2199](https://github.com/rcore-os/tgoskits/pull/2199)、[PR #1949](https://github.com/rcore-os/tgoskits/pull/1949) — 隔离中断控制器修复移植与 hypervisor IRQ 入口状态规范化

### vPCI、virtio 与设备加固

Axvisor 新增通用 vPCI 基础和 x86 PCI 枚举，guest 可以获得直通的 PCI 设备；virtio 侧补齐了 virtio-mmio 块设备核心和双 guest virtio-net，共享 virtqueue 针对不可信 guest 做了加固，fw_cfg DMA 故障路径得到修正。

- [PR #2197](https://github.com/rcore-os/tgoskits/pull/2197) — 通用 vPCI 基础与 x86 PCI 枚举
- [PR #1935](https://github.com/rcore-os/tgoskits/pull/1935)、[PR #1927](https://github.com/rcore-os/tgoskits/pull/1927)、[PR #1926](https://github.com/rcore-os/tgoskits/pull/1926) — virtio-mmio 块设备核心、双 guest virtio-net 与 guest FDT 保留 PSCI
- [PR #1984](https://github.com/rcore-os/tgoskits/pull/1984) — 共享 virtqueue 针对不可信 guest 加固
- [PR #1918](https://github.com/rcore-os/tgoskits/pull/1918)、[PR #2139](https://github.com/rcore-os/tgoskits/pull/2139) — fw_cfg DMA 故障处理与不可恢复故障传播

### AxLoader、OVMF 与板级 guest

AxLoader 为 x86_64 smoke 测试启用 KVM 加速，收紧 HTTP boot 内核 URL 校验并复用 ostool 的 OVMF 资产；x86 OVMF ACPI 路径在 VMX 与 SVM 上都得到验证。板级方面，Orange Pi 5 Plus 启用 rockchip-dwmmc 并新增机器人流程测试，ROCK 4D 可以作为 guest 启动，RK3588 guest 的 cpufreq/SCMI 与 NPU 时钟得到修复。

- [PR #1897](https://github.com/rcore-os/tgoskits/pull/1897)、[PR #1882](https://github.com/rcore-os/tgoskits/pull/1882)、[PR #1917](https://github.com/rcore-os/tgoskits/pull/1917) — AxLoader KVM 加速、HTTP boot URL 校验与 OVMF 资产复用
- [PR #1931](https://github.com/rcore-os/tgoskits/pull/1931) — x86 OVMF ACPI 在 VMX 与 SVM 上验证
- [PR #2164](https://github.com/rcore-os/tgoskits/pull/2164)、[PR #1952](https://github.com/rcore-os/tgoskits/pull/1952) — Orange Pi 5 Plus rockchip-dwmmc 与机器人流程测试
- [PR #1880](https://github.com/rcore-os/tgoskits/pull/1880) — ROCK 4D guest 启动
- [PR #1919](https://github.com/rcore-os/tgoskits/pull/1919)、[PR #1908](https://github.com/rcore-os/tgoskits/pull/1908) — RK3588 guest cpufreq/SCMI 与 NPU 800 MHz 时钟
- [PR #2120](https://github.com/rcore-os/tgoskits/pull/2120)、[PR #1977](https://github.com/rcore-os/tgoskits/pull/1977) — 非控制台物理 UART 直通与控制台设备轮询唤醒保留

---

## 三、ArceOS

### IRQ 与多任务成为固定能力

8 月运行时最重要的删除是 `irq` 和 `multitask` 两个顶层 Cargo feature。关闭它们会进入 busy-wait、忽略 timeout、静默丢弃设备 IRQ、空 registrar 和固定 PID/pthread/futex stub 等不可用降级路径；[PR #2188](https://github.com/rcore-os/tgoskits/pull/2188) 删除了全仓的 feature 转发链、构建配置和条件编译分支，平台 IRQ framework、timer、dispatcher 与多任务调度成为所有目标的固定能力，仅保留设备无 IRQ、host-test dummy 等合法语义。平台和启动组合因此显著收敛，上层代码不再需要为“可能没有中断/调度”的世界编写两套逻辑。

### 调度器与运行时所有权的前置重构

重建调度器与运行时所有权的 [PR #1775](https://github.com/rcore-os/tgoskits/pull/1775) 本月仍未合并，但它的多个前置改动进入 `dev`：`cpu-local` 定义了调度器中立的执行上下文边界，`percpu` 提供调度器可预固定的当前 CPU 区域访问，AArch64 的 current 不再依赖 TLS，axtask 在上下文切换之间保持调度栈帧所有权。IPI 改为类型化发布传输，控制台仲裁统一到可睡眠运行时，串口建立有界 owner-affine 运行时，PL011 的接收丢弃与早期控制台启动都有界化；QEMU 定时器与 exec 停滞、过期周期定时器截止时间补跑和调度器时钟源得到修复。

- [PR #2080](https://github.com/rcore-os/tgoskits/pull/2080)、[PR #2081](https://github.com/rcore-os/tgoskits/pull/2081)、[PR #1970](https://github.com/rcore-os/tgoskits/pull/1970) — 调度器中立的执行上下文边界、预固定 CPU 区域访问与 TLS 解耦
- [PR #2101](https://github.com/rcore-os/tgoskits/pull/2101)、[PR #1857](https://github.com/rcore-os/tgoskits/pull/1857) — 调度栈帧所有权与并发信号唤醒保留
- [PR #1916](https://github.com/rcore-os/tgoskits/pull/1916) — 类型化 IPI 发布传输
- [PR #2113](https://github.com/rcore-os/tgoskits/pull/2113)、[PR #2076](https://github.com/rcore-os/tgoskits/pull/2076)、[PR #2203](https://github.com/rcore-os/tgoskits/pull/2203)、[PR #1983](https://github.com/rcore-os/tgoskits/pull/1983) — 控制台仲裁、有界 owner-affine UART 运行时与 PL011 边界
- [PR #2130](https://github.com/rcore-os/tgoskits/pull/2130)、[PR #2137](https://github.com/rcore-os/tgoskits/pull/2137)、[PR #1900](https://github.com/rcore-os/tgoskits/pull/1900) — QEMU 定时器/exec 停滞、过期周期定时器截止时间补跑与调度器时钟源
- [PR #1989](https://github.com/rcore-os/tgoskits/pull/1989) — 调度器负载均衡内省接口
- [PR #2208](https://github.com/rcore-os/tgoskits/pull/2208)、[PR #2212](https://github.com/rcore-os/tgoskits/pull/2212) — 工作分支上的事务化地址空间切换与 LTP 覆盖替换

### 用户访问、内存与组件

用户态访问路径继续加固：跨页非对齐 fault 被正确处理，用户访问与架构状态迁移更严格。内存侧修复了 lazy remap 与 populate 失败后的回滚，DMA 增加 uncached 别名重映射的设备一致性支持，tracepoint 被抽取为独立组件；ax-posix-api 实现 eventfd 并为 std async 桥接 epoll，tokio/mio 得以在 ArceOS 上初始化 reactor。

- [PR #1855](https://github.com/rcore-os/tgoskits/pull/1855)、[PR #2075](https://github.com/rcore-os/tgoskits/pull/2075) — 跨页非对齐 fault 与用户访问状态迁移加固
- [PR #2094](https://github.com/rcore-os/tgoskits/pull/2094)、[PR #2079](https://github.com/rcore-os/tgoskits/pull/2079) — lazy remap 与 populate 失败回滚
- [PR #2106](https://github.com/rcore-os/tgoskits/pull/2106) — 设备 DMA 一致性与 uncached 别名重映射
- [PR #2107](https://github.com/rcore-os/tgoskits/pull/2107) — 抽取 tracepoint 组件
- [PR #1887](https://github.com/rcore-os/tgoskits/pull/1887) — eventfd 实现并为 std async 桥接 epoll

### someboot 与 SMP 启动

someboot 把次级 CPU 启动改为可查询的接口，让上层可以知道每个次级 CPU 的启动状态，而不是假设固件列出的顺序即启动顺序；定时器边界算术也得到加固，避免边界条件下的错误时间计算。

- [PR #1981](https://github.com/rcore-os/tgoskits/pull/1981) — 可查询的次级 CPU 启动接口
- [PR #1982](https://github.com/rcore-os/tgoskits/pull/1982) — 定时器边界算术加固

---

## 四、StarryOS

### 系统调用 ABI 参数校准系列

Jingming Liang 在 8 月贡献了一组系统性的 ABI 修复：逐个核对系统调用参数的位宽、符号性和长度边界，使 StarryOS 对这些参数的解释与 Linux 保持一致。这组修复覆盖 `clone3` 参数大小、`getcwd` 无符号长度、`PR_SET_NAME` 用户读取边界、`unshare`/`mremap`/`unlinkat` 标志全宽校验、`tgkill` PID 参数、ptrace 请求宽度、`personality` 无符号整数、`getgroups`/`setgroups` 有符号大小、UTS setter 与 `syslog` 有符号长度、`getdents` 缓冲与计数 ABI、`openat2` how 结构大小以及超大模块镜像处理。

- [PR #2047](https://github.com/rcore-os/tgoskits/pull/2047)、[PR #2048](https://github.com/rcore-os/tgoskits/pull/2048)、[PR #2049](https://github.com/rcore-os/tgoskits/pull/2049) — `clone3` 参数大小、`getcwd` 无符号长度与 `PR_SET_NAME` 读取边界
- [PR #2050](https://github.com/rcore-os/tgoskits/pull/2050)、[PR #2051](https://github.com/rcore-os/tgoskits/pull/2051)、[PR #2053](https://github.com/rcore-os/tgoskits/pull/2053) — `unshare`、`unlinkat`、`mremap` 标志宽度校验
- [PR #2052](https://github.com/rcore-os/tgoskits/pull/2052)、[PR #2054](https://github.com/rcore-os/tgoskits/pull/2054) — `tgkill` PID 参数与 ptrace 请求宽度
- [PR #2055](https://github.com/rcore-os/tgoskits/pull/2055)、[PR #2056](https://github.com/rcore-os/tgoskits/pull/2056)、[PR #2057](https://github.com/rcore-os/tgoskits/pull/2057) — `personality`、`getgroups`/`setgroups` ABI
- [PR #2058](https://github.com/rcore-os/tgoskits/pull/2058)、[PR #2060](https://github.com/rcore-os/tgoskits/pull/2060)、[PR #2059](https://github.com/rcore-os/tgoskits/pull/2059) — UTS setter、`syslog` 与 `getdents` 长度/缓冲 ABI
- [PR #2044](https://github.com/rcore-os/tgoskits/pull/2044)、[PR #2042](https://github.com/rcore-os/tgoskits/pull/2042)、[PR #2035](https://github.com/rcore-os/tgoskits/pull/2035) — `openat2` how 大小、超大模块镜像与 proc status 补充组

### 内核输入边界与进程生命周期

另一组修复收紧了内核路径对用户输入的边界：`getdents64` 临时缓冲、`getrandom` 临时存储、mqueue 发送长度与描述符状态读取、`execve` 参数加载、shebang 解释器递归和 ELF 元数据都有明确上限或校验。进程生命周期方面，PID namespace 身份所有权统一，新 PID namespace 中的线程被拒绝，竞态下的进程组、wait 候选重扫、ptrace stop 的 `SIGKILL` 释放和 zombie 发布前的资源释放得到修正；reboot 系统调用不再拆卸文件系统，chroot 内的父目录遍历保持在根之内。

- [PR #2030](https://github.com/rcore-os/tgoskits/pull/2030)、[PR #2029](https://github.com/rcore-os/tgoskits/pull/2029)、[PR #2028](https://github.com/rcore-os/tgoskits/pull/2028)、[PR #2027](https://github.com/rcore-os/tgoskits/pull/2027) — `getdents64`、`getrandom`、mqueue 发送与状态读取边界
- [PR #2033](https://github.com/rcore-os/tgoskits/pull/2033)、[PR #2032](https://github.com/rcore-os/tgoskits/pull/2032)、[PR #2031](https://github.com/rcore-os/tgoskits/pull/2031) — `execve` 参数加载、shebang 递归与 ELF 元数据校验
- [PR #2023](https://github.com/rcore-os/tgoskits/pull/2023)、[PR #1947](https://github.com/rcore-os/tgoskits/pull/1947) — PID namespace 身份所有权与线程拒绝
- [PR #1942](https://github.com/rcore-os/tgoskits/pull/1942)、[PR #1940](https://github.com/rcore-os/tgoskits/pull/1940)、[PR #1913](https://github.com/rcore-os/tgoskits/pull/1913)、[PR #1800](https://github.com/rcore-os/tgoskits/pull/1800) — 进程组竞态、wait 重扫、ptrace `SIGKILL` 与 zombie 前资源释放
- [PR #2220](https://github.com/rcore-os/tgoskits/pull/2220)、[PR #2037](https://github.com/rcore-os/tgoskits/pull/2037)、[PR #2100](https://github.com/rcore-os/tgoskits/pull/2100) — reboot 卸载、chroot 遍历与 `sync_file_range` fd 校验
- [PR #1925](https://github.com/rcore-os/tgoskits/pull/1925)、[PR #1944](https://github.com/rcore-os/tgoskits/pull/1944) — 事件通知语义与 netlink sockopt 回写顺序

### NixOS Stage-2 用户态

7 月在 Alpine 用户态运行 Nix 的基础上，8 月把目标推进到完整的 NixOS Stage-2：使用锁定的 NixOS flake 在主机构建并校验独立的 ext4 rootfs，StarryOS 增加仅 NixOS 应用选择的 PID-1 启动路径，运行生成的 Stage-2 `/init`，由 systemd 作为 PID 1 管理声明式系统状态；axbuild 相应增加 `AppOwned` rootfs 契约，只接受应用构建器发布且已验证的工件。配套的挂载上下文与通知也同时完善。

- [PR #1923](https://github.com/rcore-os/tgoskits/pull/1923) — NixOS Stage-2 用户态基线与 systemd PID 1
- [PR #1902](https://github.com/rcore-os/tgoskits/pull/1902) — 完整挂载上下文与通知

### 信号、pidfd 与文件系统语义

signalfd 掩码更新与 epoll 唤醒、pidfd fdinfo 元数据、OOM score 调整解析、UTS hostname sysctl、目录 `fstatfs`、Unix socket 内省与凭证等 Linux 语义在本月补齐；ptrace 的 syscall/exec stop 时序得到修正，`/dev/fb0` 读写尊重偏移，IPv4 ping 系统调用路径打通。

- [PR #1850](https://github.com/rcore-os/tgoskits/pull/1850)、[PR #1848](https://github.com/rcore-os/tgoskits/pull/1848)、[PR #1851](https://github.com/rcore-os/tgoskits/pull/1851) — signalfd 掩码更新、epoll 唤醒与 pidfd fdinfo
- [PR #1844](https://github.com/rcore-os/tgoskits/pull/1844)、[PR #1847](https://github.com/rcore-os/tgoskits/pull/1847)、[PR #1849](https://github.com/rcore-os/tgoskits/pull/1849) — OOM score、UTS hostname 与 limit sysctl 写入
- [PR #1905](https://github.com/rcore-os/tgoskits/pull/1905)、[PR #1899](https://github.com/rcore-os/tgoskits/pull/1899)、[PR #1883](https://github.com/rcore-os/tgoskits/pull/1883) — Unix socket 内省/凭证、目录 `fstatfs` 与 ptrace stop 时序
- [PR #1995](https://github.com/rcore-os/tgoskits/pull/1995)、[PR #1896](https://github.com/rcore-os/tgoskits/pull/1896)、[PR #1906](https://github.com/rcore-os/tgoskits/pull/1906)、[PR #1846](https://github.com/rcore-os/tgoskits/pull/1846) — `/dev/fb0` 偏移、IPv4 ping、capbset 参数与 netlink 地址复用

### fork、COW 与热路径优化

Joseph Joshua Anggita 修复了一个影响实际负载的问题：COW 页帧引用计数从 `u8` 扩展到 `u32`，解决约 250 个进程后 `fork()` 返回 `EFAULT` 的故障；匿名 `mprotect(+W)` 正确触发 COW，失败的 COW 克隆会回滚。性能方面，每进程 futex 表分片、`WaitQueue::is_empty` O(1) 化、seccomp 无锁快路径、`sys_write` 去除冗余校验和 `user_access_ok_page` 无锁用户拷贝共同降低了热路径开销。cgroup v2 的 pids 限制得到强制执行并暴露 `pids.peak`。

- [PR #1991](https://github.com/rcore-os/tgoskits/pull/1991)、[PR #1992](https://github.com/rcore-os/tgoskits/pull/1992)、[PR #2096](https://github.com/rcore-os/tgoskits/pull/2096) — COW 引用计数扩展、匿名 mprotect COW 与失败回滚
- [PR #1997](https://github.com/rcore-os/tgoskits/pull/1997)、[PR #1999](https://github.com/rcore-os/tgoskits/pull/1999)、[PR #1998](https://github.com/rcore-os/tgoskits/pull/1998)、[PR #2063](https://github.com/rcore-os/tgoskits/pull/2063) — futex 表分片、seccomp 快路径、`sys_write` 冗余校验与 `user_access_ok_page` 无锁用户拷贝
- [PR #2014](https://github.com/rcore-os/tgoskits/pull/2014)、[PR #2091](https://github.com/rcore-os/tgoskits/pull/2091) — cgroup v2 pids 限制与 `pids.peak`

### 基准与兼容性 carpet

新的基准与 carpet 测试覆盖了调度与定时器、多媒体与消息队列：LTP hackbench 进入 SMP QEMU 基准，单核定时器抢占行为得到持续覆盖，多媒体软件栈 carpet 覆盖音频/视频解码、编码与音视频同步，POSIX 消息队列 carpet 与 Open POSIX 一致性测试对齐；板级新增可复现的 iperf3 基准和预构建 aka-rk3588 应用。

- [PR #2017](https://github.com/rcore-os/tgoskits/pull/2017) — LTP hackbench SMP QEMU 基准
- [PR #1843](https://github.com/rcore-os/tgoskits/pull/1843) — 单核定时器抢占覆盖
- [PR #1712](https://github.com/rcore-os/tgoskits/pull/1712) — 多媒体与流媒体软件栈 carpet
- [PR #1565](https://github.com/rcore-os/tgoskits/pull/1565) — POSIX 消息队列 carpet 与 Open POSIX 一致性
- [PR #1948](https://github.com/rcore-os/tgoskits/pull/1948)、[PR #1976](https://github.com/rcore-os/tgoskits/pull/1976) — 可复现 iperf3 板级基准与预构建 aka-rk3588 应用

---

## 五、组件、驱动与网络栈

### 共享块缓存与 ext4 语义对齐

块设备与文件系统层之间引入共享 block cache：多个文件系统 wrapper 或分区共享同一物理设备视图，JBD2 元数据在 commit record 持久化前不会触达 home block，reclaim/unregister 不在持锁路径阻塞。`rsext4` 完成了一次与 Linux v7.1 ext4/JBD2 可观察语义对齐的重构，未实现的增强保持确定、typed 的拒绝并转入 issue 跟踪；journal I/O 失败、空 extent 节点、块边界符号链接等具体缺陷同月修复。

- [PR #2171](https://github.com/rcore-os/tgoskits/pull/2171) — 块设备与文件系统层共享 block cache
- [PR #2170](https://github.com/rcore-os/tgoskits/pull/2170)、[PR #2135](https://github.com/rcore-os/tgoskits/pull/2135)、[PR #2150](https://github.com/rcore-os/tgoskits/pull/2150)、[PR #1842](https://github.com/rcore-os/tgoskits/pull/1842) — 回收注册锁、块运行时生命周期发布、拓扑变化后重新规划卸载与只读挂载
- [PR #1957](https://github.com/rcore-os/tgoskits/pull/1957) — `rsext4` 与 Linux v7.1 语义对齐
- [PR #1967](https://github.com/rcore-os/tgoskits/pull/1967)、[PR #1968](https://github.com/rcore-os/tgoskits/pull/1968)、[PR #1881](https://github.com/rcore-os/tgoskits/pull/1881)、[PR #1845](https://github.com/rcore-os/tgoskits/pull/1845) — journal 失败传播、空 extent 节点、API 命名与边界符号链接
- [PR #1950](https://github.com/rcore-os/tgoskits/pull/1950) — 普通文件的 `futimens`

### 队列级 NAPI 网络运行时

ax-net 以 Linux v7.1 NAPI 和 IRQ-only 块设备 hctx 运行时为基线，破坏性替换旧网络运行时接口：每 queue 的所有权固定，IRQ callback 与 queue processing 绑定同一 CPU，不再保留无中断、周期 polling 或全局 wake-all 路径。数据报路径修复了自旋锁下睡眠 mutex 的问题，TCP snooping 前先校验报文。

- [PR #2178](https://github.com/rcore-os/tgoskits/pull/2178) — 队列级 NAPI 运行时
- [PR #1963](https://github.com/rcore-os/tgoskits/pull/1963)、[PR #2036](https://github.com/rcore-os/tgoskits/pull/2036) — 数据报自旋锁与 TCP snooping 报文校验

### 中断控制器抽取为可复用驱动

x86 的 APIC 与 LoongArch 的中断控制器分别抽取为 OS 无关的驱动 crate，并接入 `rdif-intc` 接口，使 ArceOS、StarryOS 与 Axvisor 复用同一条中断控制器路径。GIC 侧补上 GICv3 NMI 属性，修正 GICv2 CPU target 掩码并兼容单处理器目标，消除了 ArceOS 启动挂起与 StarryOS 启动 panic；FDT IRQ 绑定也增加校验。

- [PR #2118](https://github.com/rcore-os/tgoskits/pull/2118) — x86 中断控制器抽取并支持 `rdif-intc`
- [PR #2174](https://github.com/rcore-os/tgoskits/pull/2174) — LoongArch OS 无关中断控制器
- [PR #2167](https://github.com/rcore-os/tgoskits/pull/2167)、[PR #1803](https://github.com/rcore-os/tgoskits/pull/1803)、[PR #2007](https://github.com/rcore-os/tgoskits/pull/2007) — GICv3 NMI 属性、GICv2 CPU target 掩码与单处理器目标处理
- [PR #2062](https://github.com/rcore-os/tgoskits/pull/2062) — FDT IRQ 绑定校验

### SD/MMC、USB 与串口

SDIO 协议统一后，AIC8800 无线网卡驱动迁移到统一的 SDIO 路径；RK3588 DWCMSHC 时序、SD 卡启动、动态调频忙等归因和 Rockchip reset 失败生命周期得到修复，VisionFive 2 恢复 `booti` 与 SD rootfs 启动。USB 侧 DWC2 的流式 DDMA 后端迁移到主线 API，HCD 端点生命周期与 Linux 对齐，UVC 的异步传输生命周期、RGB 帧布局和测试迁移完成。串口方面 RK3588 UART 资源初始化、TTY 输入 flush 与唤醒、SG2002 UART1 25.8 MHz 时钟修正相继落地。

- [PR #2201](https://github.com/rcore-os/tgoskits/pull/2201) — 统一 SDIO 协议与 AIC8800 驱动
- [PR #2242](https://github.com/rcore-os/tgoskits/pull/2242)、[PR #1830](https://github.com/rcore-os/tgoskits/pull/1830)、[PR #2165](https://github.com/rcore-os/tgoskits/pull/2165)、[PR #1987](https://github.com/rcore-os/tgoskits/pull/1987)、[PR #1954](https://github.com/rcore-os/tgoskits/pull/1954) — DWCMSHC 时钟、RK3588 SD 启动、调频忙等、reset 生命周期与 VisionFive 2 启动
- [PR #2066](https://github.com/rcore-os/tgoskits/pull/2066)、[PR #1980](https://github.com/rcore-os/tgoskits/pull/1980) — DWC2 DDMA 后端与 HCD 端点生命周期
- [PR #1924](https://github.com/rcore-os/tgoskits/pull/1924)、[PR #2039](https://github.com/rcore-os/tgoskits/pull/2039)、[PR #1961](https://github.com/rcore-os/tgoskits/pull/1961) — UVC 异步传输、帧布局校验与测试迁移
- [PR #1921](https://github.com/rcore-os/tgoskits/pull/1921)、[PR #1922](https://github.com/rcore-os/tgoskits/pull/1922)、[PR #1714](https://github.com/rcore-os/tgoskits/pull/1714) — RK3588 UART 初始化、TTY flush 与 SG2002 UART1 时钟
- [PR #2038](https://github.com/rcore-os/tgoskits/pull/2038) — fxmac 超大接收描述符拒绝

### rdrive 与设备树

rdrive 的探测路径更加宽容和高效：FDT assigned-clocks 改为尽力而为而不再让探测失败，新增 `fdt_ref()` 以借用方式访问设备树避免克隆；rknn 的 JPEG 图像加载器在错误输入下安全失败。

- [PR #2002](https://github.com/rcore-os/tgoskits/pull/2002)、[PR #2004](https://github.com/rcore-os/tgoskits/pull/2004) — assigned-clocks 尽力而为与 `fdt_ref()` 借用
- [PR #2041](https://github.com/rcore-os/tgoskits/pull/2041) — rknn JPEG 加载器安全失败

---

## 总结

8 月的工作主要围绕以下几个方向展开：

1. **工作区布局与分层对齐**：文件系统、内存、驱动和 StarryOS 专属 crate 迁入与复用层级一致的目录，工作区依赖、锁原语与错误边界统一，新增组件的落点和复用边界更加明确。
2. **运行时能力固定化**：`irq` 与 `multitask` feature 退出主线，中断框架、定时器与多任务调度成为所有平台的固定能力，删除了大量不可用的降级路径。
3. **Axvisor 以设备图为中心**：虚拟设备声明、资源规划与注册统一到解析后的设备图，AxVM 迁移到真实 Rust `std`，vPCI、浏览器控制台和 axum 管理面补齐了外部接口。
4. **StarryOS 走向完整发行版**：NixOS Stage-2 用户态让 systemd 作为 PID 1 运行声明的系统状态；系统调用 ABI 校准系列让参数位宽、符号性和长度边界与 Linux 保持一致。
5. **存储与网络按 Linux 语义重建**：块设备与文件系统层共享缓存，`rsext4` 对齐 Linux v7.1，ax-net 切换到队列级 NAPI 运行时并消除全局唤醒路径。
6. **调度器重构前置落地**：`cpu-local`/`percpu`/调度栈帧/IPI/串口等底层边界先行整理，主 PR #1775 与工作分支继续推进事务化地址空间切换。
7. **CI 证据链收紧**：按影响选择测试、清单化路由、事件去重与受保护分支队列，加上指南技能化和以当前提交 CI 为准的审查流程，让合并决策更依赖可复核的运行证据。

感谢所有贡献者在 8 月的持续投入。
