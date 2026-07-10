---
slug: june-2026-retrospective
title: 2026 年 6 月开发月报
date: 2026-07-01T23:00:00+08:00
authors: [tgoskits-team]
tags: [monthly-report, arceos, starryos, axvisor, axbuild, testing]
---

2026 年 6 月是 TGOSKits 工作区从“功能快速补齐”继续走向“动态平台、真实应用和可复用驱动边界收束”的一月。全月共产生 **349 次非合并提交**、**299 次合并 PR**，涉及 **45 个唯一提交作者邮箱**。本月主线集中在动态平台取代静态平台配置、Axvisor VM/IRQ/guest 启动路径重构、ArceOS 调度与运行时保护、StarryOS Linux 兼容性和应用 carpet 测试、SG2002/RK3588/OrangePi 5 Plus 板级能力、以及 IRQ/DMA/SDMMC/ax-net 等跨组件基础设施。

<!-- truncate -->

## 总览

| 指标 | 数据 |
|------|------|
| 非合并提交数 | 349 |
| 合并 PR 数 | 299 |
| 唯一提交作者邮箱数 | 45 |
| 涉及 PR 编号范围 | #515 ~ #1453 |

### 贡献者排行

| 贡献者 | 提交数 | 主要方向 |
|--------|--------|----------|
| 周睿 (ZR233) | 149 | 动态平台、CI/release、Axvisor、IRQ/driver、板级支持 |
| 禾可 (Lfan-ke) | 32 | StarryOS Linux 兼容性、语言 carpet、LoongArch/aarch64 修复 |
| ZCShou | 15 | axbuild、ax-net、文档、动态平台配置整理 |
| Shi Lei | 12 | lockdep、stack protector、调度和 syscall conformance |
| Wuxun / 1301182193 | 12 | StarryOS fd/wait/timerfd、进程与系统调用修复 |
| cqwhfhh | 9 | StarryOS epoll、xattr、fcntl、网络和 VFS 行为 |
| Josen-B | 8 | Axvisor x86_64、HTTP bootloader、测试与板级启动 |
| github-actions[bot] | 8 | release-plz 自动发布与版本维护 |
| linfeng | 7 | StarryOS 调试和兼容性修复 |
| Joseph Joshua Anggita | 7 | StarryOS 图形、Wayland、PMU、设备与系统调用 |
| Antareske、Utopia-V、取地址符、Joseph Zhao | 6-7/人 | StarryOS 应用测试、动态链接、seccomp/cgroup、文件系统 |
| 其他贡献者 | 若干 | StarryOS、ArceOS、Axvisor、驱动、网络、测试与 CI 修复 |

---

## 一、仓库设施

### CI、测试调度与 release

6 月 CI 工作继续围绕 StarryOS QEMU、ArceOS Rust std、syscall/app 分组测试、container 化和 self-hosted runner 稳定性展开。我们将 Starry 测试切换到 container 环境，修正 workflow concurrency、增量 clippy 选择、container job 之间的 `tg-xtask` 传递，并继续放宽部分 QEMU smoke 超时，减少有效测试被误杀的情况。月底，CI 又进一步减少增量 clippy 冗余检查。

- [PR #1078](https://github.com/rcore-os/tgoskits/pull/1078)、[PR #1084](https://github.com/rcore-os/tgoskits/pull/1084)、[PR #1088](https://github.com/rcore-os/tgoskits/pull/1088) — Starry QEMU apps、增量 clippy 和核心源码 change filter 改进
- [PR #1090](https://github.com/rcore-os/tgoskits/pull/1090)、[PR #1102](https://github.com/rcore-os/tgoskits/pull/1102)、[PR #1105](https://github.com/rcore-os/tgoskits/pull/1105) — Starry 测试切换到 container 环境并调整 self-hosted 配置
- [PR #1174](https://github.com/rcore-os/tgoskits/pull/1174)、[PR #1184](https://github.com/rcore-os/tgoskits/pull/1184)、[PR #1201](https://github.com/rcore-os/tgoskits/pull/1201)、[PR #1209](https://github.com/rcore-os/tgoskits/pull/1209)、[PR #1236](https://github.com/rcore-os/tgoskits/pull/1236) — 重组 ArceOS Rust QEMU、Starry QEMU system 与 syscall CI 入口
- [PR #1181](https://github.com/rcore-os/tgoskits/pull/1181)、[PR #1183](https://github.com/rcore-os/tgoskits/pull/1183)、[PR #1187](https://github.com/rcore-os/tgoskits/pull/1187) — workflow concurrency、增量 clippy 与 container job 修复
- [PR #1339](https://github.com/rcore-os/tgoskits/pull/1339) — self-hosted runner prerequisite preflight
- [PR #1448](https://github.com/rcore-os/tgoskits/pull/1448) — 减少增量 clippy 冗余检查

release-plz 继续承担组件版本发布和元数据同步。本月多轮自动 release PR 合入，配合版本元数据和 CI 状态门禁维护，让 crate publish 顺序和发布前检查之间的衔接更稳定。

- [PR #1134](https://github.com/rcore-os/tgoskits/pull/1134)、[PR #1195](https://github.com/rcore-os/tgoskits/pull/1195)、[PR #1219](https://github.com/rcore-os/tgoskits/pull/1219) — release-plz 自动发布维护
- [PR #1263](https://github.com/rcore-os/tgoskits/pull/1263)、[PR #1342](https://github.com/rcore-os/tgoskits/pull/1342)、[PR #1344](https://github.com/rcore-os/tgoskits/pull/1344)、[PR #1359](https://github.com/rcore-os/tgoskits/pull/1359) — 6 月下旬多轮 release-plz 维护

### axbuild、rootfs 与动态平台配置

axbuild 在 6 月继续从“按系统分散处理”向统一 image/rootfs/test orchestration 演进。image 管理被提升为顶层命令，rootfs 存储路径进一步统一，overlay 注入 symlink、managed rootfs image path、rootfs resize 和 macOS self-build workflow 得到修复。月底，axbuild 命令实现被模块化，并开始使用 typed config serialization 与结构化 `build.rs` source generation，为后续移除静态平台和配置生成路径做准备。

- [PR #1182](https://github.com/rcore-os/tgoskits/pull/1182) — axbuild image 管理提升为顶层命令
- [PR #1191](https://github.com/rcore-os/tgoskits/pull/1191)、[PR #1197](https://github.com/rcore-os/tgoskits/pull/1197) — rootfs 存储统一、overlay 注入与 test-only workspace deps 修复
- [PR #1347](https://github.com/rcore-os/tgoskits/pull/1347) — axbuild command implementation 模块化
- [PR #1349](https://github.com/rcore-os/tgoskits/pull/1349) — Starry uImage 使用 ITS companion files
- [PR #1333](https://github.com/rcore-os/tgoskits/pull/1333) — rootfs resize 与 macOS self-build workflow
- [PR #1420](https://github.com/rcore-os/tgoskits/pull/1420)、[PR #1422](https://github.com/rcore-os/tgoskits/pull/1422) — typed config serialization 与 build.rs Rust source generation

动态平台配置也进入收束阶段。RISC-V、LoongArch 和 VisionFive2 的相关配置不断合并，静态平台残留开始被移除，平台配置、runtime path、IRQ routing 和 board smoke 测试逐步指向统一的动态平台路径。

- [PR #1071](https://github.com/rcore-os/tgoskits/pull/1071)、[PR #1074](https://github.com/rcore-os/tgoskits/pull/1074)、[PR #1075](https://github.com/rcore-os/tgoskits/pull/1075)、[PR #1085](https://github.com/rcore-os/tgoskits/pull/1085) — 动态平台与链接脚本分层，迁移 riscv64 QEMU 配置
- [PR #1387](https://github.com/rcore-os/tgoskits/pull/1387) — dynamic runtime path 移除 `ax-config`
- [PR #1428](https://github.com/rcore-os/tgoskits/pull/1428) — 移除 LoongArch static platform

### 文档与审查流程

文档侧继续围绕开发计划、月报、syscall 文档、review 流程和驱动架构展开。本月补充了 5 月总结，更新 review-single-pr 的 Starry app QEMU 验证、crates.io patch policy、无关 CI 失败记录和 test coverage wiring 检查要求，并新增 resolve-github-issue 技能文档。

- [PR #1064](https://github.com/rcore-os/tgoskits/pull/1064)、[PR #1066](https://github.com/rcore-os/tgoskits/pull/1066)、[PR #1067](https://github.com/rcore-os/tgoskits/pull/1067)、[PR #1068](https://github.com/rcore-os/tgoskits/pull/1068) — 5 月总结、开发计划与 syscall 文档整理
- [PR #1079](https://github.com/rcore-os/tgoskits/pull/1079)、[PR #1082](https://github.com/rcore-os/tgoskits/pull/1082)、[PR #1113](https://github.com/rcore-os/tgoskits/pull/1113)、[PR #1116](https://github.com/rcore-os/tgoskits/pull/1116) — PR review 流程文档增强
- [PR #1281](https://github.com/rcore-os/tgoskits/pull/1281) — single PR review 中 test coverage wiring 检查要求
- [PR #1338](https://github.com/rcore-os/tgoskits/pull/1338)、[PR #1341](https://github.com/rcore-os/tgoskits/pull/1341) — resolve-github-issue 技能文档

---

## 二、Axvisor

### host API、VM 配置与 axvm 分层

Axvisor 本月的核心目标是把 VM 生命周期、host API、device、irqchip 和 architecture setup 的边界拆清楚。月初完成 ArceOS API 边界重构，随后 VM 配置整理为 platform-first 结构。月底，axvm 的 VM lifecycle state machine 开始重构，为动态平台和多架构 guest 支持提供更清晰的内核边界。

- [PR #1019](https://github.com/rcore-os/tgoskits/pull/1019) — Axvisor ArceOS API 边界重构
- [PR #1063](https://github.com/rcore-os/tgoskits/pull/1063) — Axvisor VM 配置整理为 platform-first 结构
- [PR #1447](https://github.com/rcore-os/tgoskits/pull/1447) — axvm VM lifecycle state machine 重构

### x86_64、动态平台与 guest boot

x86_64 方向继续从 5 月的 SVM/VMX bringup 走向动态平台默认路径。6 月合入了 x86_64 dynamic QEMU guest boot，移除废弃 q35 静态平台，修复 SVM guest timer calibration stall，并为 axbuild 增加 x86 KVM acceleration 支持。Asus NUC15CRH board 和 HTTP bootloader inspection/publishing 也继续推进，使本地、QEMU 和板级启动路径更容易复现。

- [PR #1166](https://github.com/rcore-os/tgoskits/pull/1166) — Axvisor x86_64 dynamic QEMU guest boot
- [PR #1186](https://github.com/rcore-os/tgoskits/pull/1186) — 移除废弃 q35 静态平台
- [PR #1205](https://github.com/rcore-os/tgoskits/pull/1205) — SVM guest timer calibration stall 修复
- [PR #1221](https://github.com/rcore-os/tgoskits/pull/1221) — axbuild x86 KVM acceleration 支持
- [PR #1148](https://github.com/rcore-os/tgoskits/pull/1148) — HTTP bootloader inspection/publishing/features 流程

### LoongArch64、VisionFive2 与板级动态平台

LoongArch64 本月完成 dynamic UEFI platform boot 的关键补齐，修复 timer IRQ ack 顺序，并推进 Linux guest on QEMU。VisionFive2 方向新增 board smoke、dynamic RTC/MMC，并移除 static platform。AArch64 HVF timer boot 和 someboot MMU enable / relocation state 也得到修复和拆分。

- [PR #1190](https://github.com/rcore-os/tgoskits/pull/1190)、[PR #1216](https://github.com/rcore-os/tgoskits/pull/1216) — LoongArch64 dynamic UEFI platform boot 支持
- [PR #1202](https://github.com/rcore-os/tgoskits/pull/1202) — 回滚早期不可用的 LoongArch64 UEFI dynamic platform 尝试
- [PR #1222](https://github.com/rcore-os/tgoskits/pull/1222) — timer IRQ ack 顺序修复
- [PR #1207](https://github.com/rcore-os/tgoskits/pull/1207) — Axvisor LoongArch Linux guest on QEMU
- [PR #1214](https://github.com/rcore-os/tgoskits/pull/1214) — LoongArch64 QEMU virt FDT RAM size 检测
- [PR #1348](https://github.com/rcore-os/tgoskits/pull/1348)、[PR #1353](https://github.com/rcore-os/tgoskits/pull/1353)、[PR #1371](https://github.com/rcore-os/tgoskits/pull/1371) — VisionFive2 board smoke、dynamic RTC/MMC 与 static platform 移除
- [PR #1334](https://github.com/rcore-os/tgoskits/pull/1334)、[PR #1362](https://github.com/rcore-os/tgoskits/pull/1362) — AArch64 HVF timer boot 与 someboot MMU/relocation state 拆分

### interrupt fabric 与 IRQ routing

VM interrupt 和平台 IRQ routing 是本月 Axvisor 的另一条主线。per-VM interrupt fabric 合入后，RISC-V IRQ 路由到 vPLIC backend，x86、LoongArch、RISC-V 的 somehal IRQ routing 也开始重组。与此同时，QEMU high MMIO PCI window、LoongArch passthrough IRQ id 和 IOAPIC forwarding lock 等细节修复，让 passthrough 和动态平台路径更稳定。

- [PR #1273](https://github.com/rcore-os/tgoskits/pull/1273) — per-VM interrupt fabric
- [PR #1289](https://github.com/rcore-os/tgoskits/pull/1289) — QEMU high MMIO PCI window 映射修复
- [PR #1317](https://github.com/rcore-os/tgoskits/pull/1317) — RISC-V IRQ 路由到 vPLIC backend
- [PR #1430](https://github.com/rcore-os/tgoskits/pull/1430)、[PR #1442](https://github.com/rcore-os/tgoskits/pull/1442)、[PR #1443](https://github.com/rcore-os/tgoskits/pull/1443) — x86、LoongArch 与 RISC-V somehal IRQ routing 重组
- [PR #1346](https://github.com/rcore-os/tgoskits/pull/1346)、[PR #1424](https://github.com/rcore-os/tgoskits/pull/1424)、[PR #1425](https://github.com/rcore-os/tgoskits/pull/1425) — IRQ domain 与 trap vector 分离及回滚/重合入

---

## 三、ArceOS

### std-aware 构建与动态平台

ArceOS 在 6 月继续收束 Rust std-aware 构建路径。月初延续 5 月的 `arceos-rust` 工作，合入统一 std-aware build 流程；随后重组 ArceOS apps 和 someboot linker script fragments。aarch64 SMP IPI readiness、loongarch64 LASX state 保存等多架构细节也支撑了用户态、Git HTTPS 和更复杂应用场景。

- [PR #1080](https://github.com/rcore-os/tgoskits/pull/1080) — std-aware ArceOS builds 统一流程
- [PR #1180](https://github.com/rcore-os/tgoskits/pull/1180) — ArceOS apps 重组
- [PR #1218](https://github.com/rcore-os/tgoskits/pull/1218) — someboot linker script fragments
- [PR #1196](https://github.com/rcore-os/tgoskits/pull/1196) — aarch64 SMP IPI readiness 修复
- [PR #1178](https://github.com/rcore-os/tgoskits/pull/1178) — loongarch64 LASX state 保存

### 调度、lockdep 与运行时保护

运行时可靠性方面，ArceOS 在 6 月继续加强调度和锁检查能力。axtask 优化了 `select_run_queue` 的当前 CPU affinity 偏好，修复 sleep deadline 使用 wall-clock 的问题，并对 poll_io 优先级、remote IPI kick、delivered remote reschedule request 等 SMP 调度细节做了修复。lockdep 方向新增 lockdep-aware spin rwlock，修复 Starry lock order regressions，并移除 kernel path 中的 spin mutex usage。stack protector 也进入 axruntime，用于更早暴露栈破坏问题。

- [PR #1012](https://github.com/rcore-os/tgoskits/pull/1012) — axtask `select_run_queue` 当前 CPU affinity 偏好
- [PR #1239](https://github.com/rcore-os/tgoskits/pull/1239) — axruntime compiler-backed stack protector
- [PR #1240](https://github.com/rcore-os/tgoskits/pull/1240) — axtask sleep deadline 使用单调时间
- [PR #1278](https://github.com/rcore-os/tgoskits/pull/1278) — irq-safe deferred notifications
- [PR #1286](https://github.com/rcore-os/tgoskits/pull/1286)、[PR #1290](https://github.com/rcore-os/tgoskits/pull/1290) — task sleep timing 与 remote wake progress 稳定性
- [PR #1337](https://github.com/rcore-os/tgoskits/pull/1337)、[PR #1354](https://github.com/rcore-os/tgoskits/pull/1354)、[PR #1381](https://github.com/rcore-os/tgoskits/pull/1381) — ax-task ready poll_io、remote IPI kick 与 reschedule request 修复
- [PR #1397](https://github.com/rcore-os/tgoskits/pull/1397) — lockdep-aware spin rwlock
- [PR #1375](https://github.com/rcore-os/tgoskits/pull/1375)、[PR #1380](https://github.com/rcore-os/tgoskits/pull/1380) — Starry lock order regressions 与 kernel path spin mutex usage 清理

### CPU 异常、backtrace 与测试稳定性

CPU 异常和诊断方面，x86_64 新任务 x87 stack 初始状态、`#DE` 到 `SIGFPE` 的递送语义得到修复；axbacktrace 继续加固正确性、减少额外分配并补充性能回归覆盖。CI 侧的 ArceOS remote wake、host HTTP response 等测试也被逐步稳定下来。

- [PR #1029](https://github.com/rcore-os/tgoskits/pull/1029) — axbacktrace 正确性、分配行为和性能回归覆盖
- [PR #1366](https://github.com/rcore-os/tgoskits/pull/1366)、[PR #1367](https://github.com/rcore-os/tgoskits/pull/1367) — x86_64 x87 初始状态与 `#DE` 到 `SIGFPE`
- [PR #1287](https://github.com/rcore-os/tgoskits/pull/1287) — host HTTP response 测试稳定性

---

## 四、StarryOS

6 月 StarryOS 的关键词是“真实发行版应用”和“Linux 兼容语义”。系统调用、进程、ptrace/GDB、namespace/cgroup、图形、语言运行时、板级设备和诊断工具都继续扩展，测试矩阵从单个应用用例进入 carpet-style 覆盖。

### 进程、系统调用与 Linux 兼容性

进程与系统调用方面，本月补齐了 memfd seal、waitid `P_PGID`/`P_PIDFD`、child subreaper、fork-exec-wait4、namespace/unshare 基础语义，并继续推进 futex `WAKE_OP`、fcntl lock deadlock、legacy getrlimit/setrlimit、membarrier、fd table/wait/timerfd 等行为。`execveat`、seccomp/capabilities、overlayfs、新 mount API ENOSYS、PID 1 和 sysfs cgroup mount point 也进入兼容性主线。

- [PR #515](https://github.com/rcore-os/tgoskits/pull/515) — memfd seal 行为完善
- [PR #1032](https://github.com/rcore-os/tgoskits/pull/1032) — waitid `P_PGID` / `P_PIDFD`
- [PR #1051](https://github.com/rcore-os/tgoskits/pull/1051) — child subreaper
- [PR #1050](https://github.com/rcore-os/tgoskits/pull/1050) — fork-exec-wait4
- [PR #1056](https://github.com/rcore-os/tgoskits/pull/1056)、[PR #981](https://github.com/rcore-os/tgoskits/pull/981)、[PR #1031](https://github.com/rcore-os/tgoskits/pull/1031) — namespace/unshare 基础语义
- [PR #1052](https://github.com/rcore-os/tgoskits/pull/1052) — futex `WAKE_OP`
- [PR #1055](https://github.com/rcore-os/tgoskits/pull/1055) — fcntl lock deadlock
- [PR #1210](https://github.com/rcore-os/tgoskits/pull/1210)、[PR #1225](https://github.com/rcore-os/tgoskits/pull/1225)、[PR #1237](https://github.com/rcore-os/tgoskits/pull/1237) — legacy rlimit、membarrier、fd table/wait/timerfd 修复
- [PR #1144](https://github.com/rcore-os/tgoskits/pull/1144) — `execveat`
- [PR #1275](https://github.com/rcore-os/tgoskits/pull/1275) — seccomp/capabilities
- [PR #1223](https://github.com/rcore-os/tgoskits/pull/1223)、[PR #1241](https://github.com/rcore-os/tgoskits/pull/1241)、[PR #1233](https://github.com/rcore-os/tgoskits/pull/1233)、[PR #1243](https://github.com/rcore-os/tgoskits/pull/1243) — overlayfs、new mount API、PID 1 与 sysfs cgroup mount point

### ptrace/GDB、x86_64 ABI 与异常状态

调试和 ABI 方面，StarryOS 继续补齐 ptrace/GDB 支持。x86_64 ptrace cleanup 合入后，GDB 支持扩展到 aarch64/loongarch64 和 multiarch GDB 场景；x86 ptrace GDB 对齐、x86_64 native GDB smoke、AVX/XCR0 用户态状态和 x86_64 context switch 中 AVX state 保存也完成修复。同步 `SIGSEGV` 的 `siginfo.si_addr` 和 `#DE` 到 `SIGFPE` 的递送语义，使异常路径更接近 Linux 行为。

- [PR #1062](https://github.com/rcore-os/tgoskits/pull/1062) — x86_64 ptrace cleanup
- [PR #1247](https://github.com/rcore-os/tgoskits/pull/1247)、[PR #1292](https://github.com/rcore-os/tgoskits/pull/1292) — aarch64/loongarch64 与 multiarch GDB ptrace 支持
- [PR #1314](https://github.com/rcore-os/tgoskits/pull/1314)、[PR #1330](https://github.com/rcore-os/tgoskits/pull/1330) — x86 ptrace GDB 对齐与 x86_64 native GDB smoke
- [PR #1112](https://github.com/rcore-os/tgoskits/pull/1112)、[PR #1329](https://github.com/rcore-os/tgoskits/pull/1329) — AVX/XCR0 用户态状态与 context switch 中 AVX state 保存
- [PR #1331](https://github.com/rcore-os/tgoskits/pull/1331) — synchronous SIGSEGV `siginfo.si_addr`

### proc/sysfs、内存、VFS 与文件系统

proc/sysfs、内存和文件系统方向继续补齐真实应用所需的可观测性和边界语义。`/proc` 暴露进程内存统计，VmPeak/VmHWM 与 COW RSS per-VA charge tracking 得到增强；file-backed mmap EOF populate、readahead、cold user pages、tmpfs directory cookies、VFS uid/gid 传递和 xattr store 等路径也完成修复。page reclaim 进入 file-backed memory pressure 场景，为自编译和大型应用测试提供更稳定的内存基础。

- [PR #1007](https://github.com/rcore-os/tgoskits/pull/1007) — file-backed memory pressure page reclaim
- [PR #1097](https://github.com/rcore-os/tgoskits/pull/1097)、[PR #1040](https://github.com/rcore-os/tgoskits/pull/1040) — VFS 创建路径 uid/gid 与 xattr store
- [PR #1164](https://github.com/rcore-os/tgoskits/pull/1164)、[PR #1217](https://github.com/rcore-os/tgoskits/pull/1217) — file-backed mmap EOF populate 与 readahead
- [PR #1171](https://github.com/rcore-os/tgoskits/pull/1171)、[PR #1173](https://github.com/rcore-os/tgoskits/pull/1173)、[PR #1316](https://github.com/rcore-os/tgoskits/pull/1316) — `/proc` 内存统计、RSS accounting 与 VmPeak/VmHWM
- [PR #1326](https://github.com/rcore-os/tgoskits/pull/1326)、[PR #1328](https://github.com/rcore-os/tgoskits/pull/1328) — tmpfs directory cookies 与 cold user pages
- [PR #1372](https://github.com/rcore-os/tgoskits/pull/1372) — proc mem monitor 稳定性

### 应用、服务与语言 carpet 测试

应用覆盖是 6 月 StarryOS 最醒目的进展之一。diffutils、pip、nginx、Git stress、Mosquitto、动态 musl/glibc 链接应用测试进入矩阵；随后又补充 CPython 3.14、Go 1.26、OpenJDK multi-version、Node.js v22、HDL toolchain、UV 0.11、Apache normal、Python web/scientific、cargo jobserver wait stress 等 carpet 测例。测试从“单点 app 能跑”扩展到语言运行时、包管理器、Web 服务、构建系统和多进程 daemon 行为的稳定回归。

- [PR #875](https://github.com/rcore-os/tgoskits/pull/875)、[PR #1002](https://github.com/rcore-os/tgoskits/pull/1002)、[PR #1014](https://github.com/rcore-os/tgoskits/pull/1014)、[PR #1026](https://github.com/rcore-os/tgoskits/pull/1026)、[PR #1072](https://github.com/rcore-os/tgoskits/pull/1072)、[PR #1041](https://github.com/rcore-os/tgoskits/pull/1041)、[PR #1048](https://github.com/rcore-os/tgoskits/pull/1048) — diffutils、pip、nginx、Git stress、Mosquitto、动态 musl/glibc 测试覆盖
- [PR #1018](https://github.com/rcore-os/tgoskits/pull/1018) — nginx multi-worker signal interruption 与 `EPOLLEXCLUSIVE`
- [PR #1257](https://github.com/rcore-os/tgoskits/pull/1257)、[PR #1282](https://github.com/rcore-os/tgoskits/pull/1282)、[PR #1261](https://github.com/rcore-os/tgoskits/pull/1261)、[PR #1283](https://github.com/rcore-os/tgoskits/pull/1283)、[PR #1285](https://github.com/rcore-os/tgoskits/pull/1285)、[PR #1211](https://github.com/rcore-os/tgoskits/pull/1211) — CPython、Go、OpenJDK、Node.js、HDL toolchain 与 UV language carpet
- [PR #1038](https://github.com/rcore-os/tgoskits/pull/1038)、[PR #1311](https://github.com/rcore-os/tgoskits/pull/1311) — Alpine nginx normal 与 Apache normal tests
- [PR #1327](https://github.com/rcore-os/tgoskits/pull/1327) — cargo jobserver wait stress
- [PR #1441](https://github.com/rcore-os/tgoskits/pull/1441)、[PR #1449](https://github.com/rcore-os/tgoskits/pull/1449) — Python web framework 与 scientific-computing carpet

### 图形、Wayland、HVF 与可视化测试

图形方向从 5 月的 Weston/DRM/KMS bringup 继续推进到可回归的 Wayland app 和可视化测试。visual-regression test pipeline 与 Xwayland 场景合入后，Wayland app case、PRIME dma-buf/ffplay Wayland integration test、resource monitoring visualization 和 Qt6 calculator test 继续补齐图形应用可见用例。evdev demand-driven polling 和 input event delivery 修复也让输入路径更可靠。

- [PR #516](https://github.com/rcore-os/tgoskits/pull/516) — visual-regression test pipeline 与 Xwayland 场景
- [PR #1160](https://github.com/rcore-os/tgoskits/pull/1160) — Wayland app case
- [PR #1268](https://github.com/rcore-os/tgoskits/pull/1268) — PRIME dma-buf / ffplay Wayland integration test
- [PR #1370](https://github.com/rcore-os/tgoskits/pull/1370) — resource monitoring visualization
- [PR #1396](https://github.com/rcore-os/tgoskits/pull/1396) — Qt6 calculator test 与 input event delivery
- [PR #1450](https://github.com/rcore-os/tgoskits/pull/1450) — evdev demand-driven polling

### 板级、设备与 Starry 自构建

板级和设备支持方面，SG2002、K230、OrangePi 5 Plus 和 RK3588 都有进展。K230 QEMU boot 与 KPU devfs 暴露合入，SG2002 AIC8800 Wi-Fi SoftAP、AP/STA runtime switch、SDIO recovery、AIC8800DC SoftAP 场景不断补齐；OrangePi 5 Plus 增加 RKNN bench validation；USB serial tty 支持进入 Starry，并将 USB serial logic 移入 driver crate。Starry 自编译方面，riscv64/x86_64 self-compilation、loongarch64 `to_bin`、static-pie ELF loader 和 macOS HVF self-build 文档继续推进。

- [PR #1046](https://github.com/rcore-os/tgoskits/pull/1046)、[PR #1054](https://github.com/rcore-os/tgoskits/pull/1054) — QEMU K230 boot 与 K230 KPU 设备暴露
- [PR #1189](https://github.com/rcore-os/tgoskits/pull/1189) — OrangePi 5 Plus UVC/RKNN bench validation
- [PR #1185](https://github.com/rcore-os/tgoskits/pull/1185)、[PR #1266](https://github.com/rcore-os/tgoskits/pull/1266)、[PR #1276](https://github.com/rcore-os/tgoskits/pull/1276)、[PR #1318](https://github.com/rcore-os/tgoskits/pull/1318) — SG2002 AIC8800 Wi-Fi SoftAP、AP/STA switch 与 recovery
- [PR #1270](https://github.com/rcore-os/tgoskits/pull/1270)、[PR #1269](https://github.com/rcore-os/tgoskits/pull/1269) — SG2002 tty serial MMIO iomap 与 TPU TDMA IRQ
- [PR #1378](https://github.com/rcore-os/tgoskits/pull/1378) — Starry USB serial tty 与 driver crate 拆分
- [PR #881](https://github.com/rcore-os/tgoskits/pull/881)、[PR #973](https://github.com/rcore-os/tgoskits/pull/973)、[PR #1025](https://github.com/rcore-os/tgoskits/pull/1025)、[PR #1033](https://github.com/rcore-os/tgoskits/pull/1033) — Starry 自编译、loongarch64 `to_bin` 与 static-pie ELF loader 修复

### eBPF、kmod、perf 与诊断

诊断扩展方面，本月移植 LKM loader 与 kmod build flow，现代化 eBPF apps，新增 rawtp/upb2 demos，并修复 LoongArch DMW 下 eBPF ringbuf mmap。ARM PMUv3 hardware-PMU perf 支持合入后，StarryOS 可以覆盖 `perf stat`、`perf record` 和 `perf report` 一类场景；BPF JIT memory ownership、LoongArch DMW-backed kmods 和 perf-hw busy loop warning 也得到修复。

- [PR #851](https://github.com/rcore-os/tgoskits/pull/851) — LKM loader 与 kmod build flow
- [PR #1192](https://github.com/rcore-os/tgoskits/pull/1192)、[PR #1208](https://github.com/rcore-os/tgoskits/pull/1208) — eBPF apps 现代化、rawtp/upb2 demos 与 ringbuf mmap 修复
- [PR #1256](https://github.com/rcore-os/tgoskits/pull/1256) — BPF JIT memory ownership
- [PR #1279](https://github.com/rcore-os/tgoskits/pull/1279) — LoongArch DMW-backed kmods
- [PR #1395](https://github.com/rcore-os/tgoskits/pull/1395) — ARM PMUv3 hardware-PMU perf 支持

---

## 五、组件、驱动与网络栈

### IRQ framework、DMA/MMIO 与 driver runtime

跨组件驱动边界在 6 月继续收束。shared IRQ framework、高层 DMA sync helpers、ax-driver IRQ binding registration、dynamic platform RTC 和 submit-poll block IRQ registration 为统一中断注册、DMA 同步和平台路由打基础。月底 IRQ domain 与 trap vector 分离进入主线，为后续 typed domain metadata 和 boxed callback 方向打下基础。

- [PR #1065](https://github.com/rcore-os/tgoskits/pull/1065) — shared IRQ framework
- [PR #1028](https://github.com/rcore-os/tgoskits/pull/1028) — high-level DMA sync helpers
- [PR #1100](https://github.com/rcore-os/tgoskits/pull/1100) — 移除 ax-driver 冗余 MMIO cfg gate
- [PR #1150](https://github.com/rcore-os/tgoskits/pull/1150) — ax-driver IRQ binding registration 信息统一
- [PR #1242](https://github.com/rcore-os/tgoskits/pull/1242) — dynamic platform RTC 支持
- [PR #1228](https://github.com/rcore-os/tgoskits/pull/1228) — submit-poll fs block IRQ registration 适配

### SDMMC、RDIF、axdevice 与 USB

设备模型方面，本月新增 SDIO host bus abstraction、physical bus transaction traits、owned DMA queue primitives 和 native SDMMC RDIF path；axdevice 统一 Device model，增加 indexed dispatch 与 conflict detect；serial IRQ model、virtio-net queue access、CrabUSB 和 USB serial driver crate 也完成多项修复。这些工作共同推动驱动从单系统 glue 走向跨平台、可复用的 runtime 边界。

- [PR #1336](https://github.com/rcore-os/tgoskits/pull/1336) — SDIO host bus abstraction、physical bus transaction traits、owned DMA queue primitives 与 native SDMMC RDIF path
- [PR #1258](https://github.com/rcore-os/tgoskits/pull/1258) — device foundation
- [PR #1335](https://github.com/rcore-os/tgoskits/pull/1335) — axdevice Device model、indexed dispatch 与 conflict detect
- [PR #1265](https://github.com/rcore-os/tgoskits/pull/1265) — serial IRQ model 对齐
- [PR #1392](https://github.com/rcore-os/tgoskits/pull/1392) — virtio-net queue access 序列化
- [PR #1376](https://github.com/rcore-os/tgoskits/pull/1376) — xHCI ISO ring xrun events 忽略与 CrabUSB 错误处理
- [PR #1378](https://github.com/rcore-os/tgoskits/pull/1378) — USB serial logic 拆入 driver crate

### ax-net 与网络行为

网络栈在 6 月完成一次重要整理：`ax-net` 统一到 `net/ax-net` crate，Starry 侧引用同步迁移；随后增加多网卡、per-interface routing、DNS 和 `SO_BINDTODEVICE` 支持，并补充 locking/concurrency 文档。月底继续修复 network poll wakeup、拆分 NIC IRQ handler 与 queue，并让 socket QoS options 更接近 Linux 行为。

- [PR #1203](https://github.com/rcore-os/tgoskits/pull/1203)、[PR #1220](https://github.com/rcore-os/tgoskits/pull/1220) — 网络栈统一到 `net/ax-net` crate 并迁移 Starry 引用
- [PR #1244](https://github.com/rcore-os/tgoskits/pull/1244) — 多网卡、per-interface routing、DNS 与 `SO_BINDTODEVICE`
- [PR #1340](https://github.com/rcore-os/tgoskits/pull/1340) — ax-net locking/concurrency 文档与 deprecated interfaces 清理
- [PR #1319](https://github.com/rcore-os/tgoskits/pull/1319) — socket QoS options 对齐 Linux
- [PR #1432](https://github.com/rcore-os/tgoskits/pull/1432)、[PR #1435](https://github.com/rcore-os/tgoskits/pull/1435) — network poll wakeup 与 NIC IRQ handler / queue 拆分

---

## 总结

6 月的工作主要围绕以下几个方向展开：

1. **动态平台收束**：axbuild、somehal、Axvisor、ArceOS 和板级配置继续移除静态平台残留，动态平台成为默认路径。
2. **虚拟化分层**：Axvisor 完成 host API、VM config、interrupt fabric、guest boot 和 axvm lifecycle 的关键重构，x86_64、LoongArch64、RISC-V 与 VisionFive2 路径同步推进。
3. **运行时可靠性**：ArceOS 在 lockdep、stack protector、SMP 调度、remote wake、CPU 异常和 backtrace 上持续加固。
4. **Linux 兼容性与真实应用**：StarryOS 补齐大量 syscall、ptrace/GDB、namespace/cgroup、proc/sysfs、内存和 VFS 语义，并把应用测试扩展到语言运行时、Web 服务、构建工具和图形应用 carpet。
5. **板级与设备能力**：SG2002、K230、OrangePi 5 Plus、RK3588、USB serial、AIC8800、KPU、RKNPU 和 PMU/perf 场景持续进入测试和应用路径。
6. **跨组件驱动基础**：IRQ domain、DMA sync、SDMMC/RDIF、axdevice、CrabUSB、ax-net 和 driver runtime 边界继续清晰化，为后续跨内核复用打基础。

感谢所有贡献者在 6 月的持续投入。
