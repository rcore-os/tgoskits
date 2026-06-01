---
slug: may-2026-retrospective
title: 2026 年 5 月开发月报
date: 2026-06-01T23:00:00+08:00
authors: [tgoskits-team]
tags: [monthly-report, arceos, starryos, axvisor, axbuild, testing]
---

2026 年 5 月是 TGOSKits 工作区持续扩展系统能力和工程基础设施的一月。全月共产生 **469 次非合并提交**、**390 次合并 PR**，涉及 **60 位贡献者**。本月的主要工作集中在 StarryOS Linux 兼容性和真实应用支持、Axvisor x86_64/loongarch64 虚拟化能力、RK3588/SG2002/K230 板级支持、图形桌面能力、rsext4 与 SD/MMC 存储路径、以及 axbuild/rootfs/CI 测试基础设施。

<!-- truncate -->

## 总览

| 指标 | 数据 |
|------|------|
| 非合并提交数 | 469 |
| 合并 PR 数 | 390 |
| 贡献者人数 | 60 |
| 涉及 PR 编号范围 | #205 ~ #1027 |

### 贡献者排行

| 贡献者 | 提交数 | 主要方向 |
|--------|--------|----------|
| 周睿 (ZR233) | 117 | StarryOS USB/网络/板级支持、CI、组件发布、驱动迁移 |
| eternalcomet | 46 | ArceOS Rust std 支持、示例应用与接口适配 |
| ZCShou | 40 | axbuild、rootfs、文档、平台配置、CI 与 Axvisor 重构 |
| 禾可 (Lfan-ke) | 22 | StarryOS loongarch64、OpenJDK、信号、内存与 runtime 修复 |
| Hong Deyao | 18 | StarryOS 文件系统、procfs、busybox 兼容性 |
| Joseph Joshua Anggita | 13 | DRM/KMS、memfd、timerfd、AF_NETLINK、HVF 与图形 bringup |
| 杨凯森 (yks23) | 12 | StarryOS syscall 上下文、SMP/HVF、axsync/axtask 修复 |
| CN-TangLin | 10 | eBPF 观测、JIT、用户态测试与 kprobe |
| Zitao Chen、YanLien、Shuo Zhang、Jiaxin2006、Feiran Qin、CharlieV | 9/人 | StarryOS syscall、rsext4、K230、backtrace、procfs、timer/TTY/网络 |
| 其他贡献者 | 若干 | StarryOS、Axvisor、ArceOS、网络、文件系统、测试与 CI 修复 |

---

## 一、仓库设施

### 构建系统与 rootfs 流程

5 月 axbuild 的重点是统一 StarryOS、Axvisor 和板级测试场景中的 rootfs、QEMU 和平台配置流程。我们重构了 Axvisor 与 Starry 的 rootfs 处理和 QEMU 配置，将启动参数、磁盘镜像准备、临时配置生成和测试入口集中到更一致的流程中。随后，QEMU 构建配置和测试执行逻辑也完成整理，使托管的 QEMU drive rootfs 镜像可以在 CI 和本地测试中被稳定准备。

- [PR #433](https://github.com/rcore-os/tgoskits/pull/433) — Axvisor 与 Starry rootfs / QEMU 配置重构（ZCShou）
- [PR #527](https://github.com/rcore-os/tgoskits/pull/527) — QEMU 构建配置与测试执行流程重构（ZCShou）
- Starry QEMU drive rootfs 镜像准备修复（周睿）
- Starry board examples 支持（周睿）

平台配置系统也在 5 月继续演进。我们添加了 RISC-V VisionFive2 平台支持，完善平台包 alias、manifest 目录解析和配置加载报告结构，并移除了 `cargo-axplat` 相关路径。RISC-V QEMU 与 SG2002 动态平台支持随后进入 dev，为后续多平台配置统一打下基础。

- [PR #541](https://github.com/rcore-os/tgoskits/pull/541) — RISC-V VisionFive2 平台支持与构建系统增强（ZCShou）
- [PR #552](https://github.com/rcore-os/tgoskits/pull/552) — 平台配置处理重构与 `cargo-axplat` 移除（ZCShou）
- [PR #961](https://github.com/rcore-os/tgoskits/pull/961) — RISC-V QEMU 与 SG2002 动态平台支持（周睿）

### CI/CD 与发布流程

CI 方面，本月继续围绕 release、fork、hosted Axvisor 测试和 runner 环境做加固。我们规范化了 fork 场景下的 GHCR 镜像名称，修复了 release publish 的触发顺序，并为 Git/CI 命令添加 `safe.directory` 支持，降低容器和 CI 环境中目录所有权不一致导致的失败概率。月底，部分 CI job 也迁移到 self-hosted runner 并启用 container 运行。

- [PR #467](https://github.com/rcore-os/tgoskits/pull/467) — release publish 在 dev checks 后执行（周睿）
- [PR #469](https://github.com/rcore-os/tgoskits/pull/469) — fork 场景下 GHCR 镜像名称规范化（周睿）
- [PR #537](https://github.com/rcore-os/tgoskits/pull/537) — Git 和 CI 命令添加 `safe.directory` 支持（ZCShou）
- [PR #546](https://github.com/rcore-os/tgoskits/pull/546) — 跳过 hosted Axvisor SVM 测试（周睿）
- [PR #928](https://github.com/rcore-os/tgoskits/pull/928) — CI job 迁移到 self-hosted runner 并启用 container（ZCShou）
- [PR #1027](https://github.com/rcore-os/tgoskits/pull/1027) — Rust toolchain 更新至 nightly-2026-05-28 并修复 clippy（周睿）

### 测试基础设施

测试基础设施继续向真实应用和系统调用覆盖扩展。StarryOS QEMU 测试新增 Rust 用户程序交叉编译流水线，并将 PostgreSQL 覆盖保留在 stress 分组中。随后，Redis、SQLite、MariaDB、DeepSeek TUI、GCC、sched-family、futex、socket、qperf 等应用和系统调用覆盖陆续进入测试矩阵，使 normal/stress 分组更接近真实发行版工作负载。

- [PR #471](https://github.com/rcore-os/tgoskits/pull/471) — StarryOS QEMU 测试用例 Rust 交叉编译流水线（CharlieV）
- [PR #436](https://github.com/rcore-os/tgoskits/pull/436) — PostgreSQL 覆盖保留在 stress 分组（周睿）
- [PR #802](https://github.com/rcore-os/tgoskits/pull/802) — Redis app 测试支持（Kevin Choo）
- [PR #895](https://github.com/rcore-os/tgoskits/pull/895) — sqlite3 CLI 多架构压力测试配置（Hong Song）
- [PR #906](https://github.com/rcore-os/tgoskits/pull/906) — MariaDB 支持（Wuxun）
- [PR #940](https://github.com/rcore-os/tgoskits/pull/940) — qperf TCG 热点 profiling 工具（cg24-THU）
- [PR #986](https://github.com/rcore-os/tgoskits/pull/986) — sched-family 测试套件与内核调度 ABI 修复（megumi）

### 文档与开发流程

文档方面，本月继续补齐开发、测试和审查流程。Docusaurus 版本和文档结构得到更新，架构、开发和测试流程说明进一步完善；同时新增 `review-single-pr` 技能，并补充了单个 PR 集中审查和冲突处理流程。

- [PR #435](https://github.com/rcore-os/tgoskits/pull/435) — Docusaurus 升级与文档章节增强（ZCShou）
- [PR #530](https://github.com/rcore-os/tgoskits/pull/530) — 架构、开发和测试流程文档增强（ZCShou）
- [PR #536](https://github.com/rcore-os/tgoskits/pull/536) — 新增 review-single-pr 技能（周睿）
- [PR #424](https://github.com/rcore-os/tgoskits/pull/424)、[PR #497](https://github.com/rcore-os/tgoskits/pull/497) — PR 审查工作流文档更新（周睿）
- [PR #453](https://github.com/rcore-os/tgoskits/pull/453) — 调试检查机制总结（Shi Lei）

---

## 二、ArceOS

### Rust std 支持

本月 ArceOS Rust 标准库支持正式进入工作区。相关工作先通过 PR #374 合入，随后因集成风险通过 PR #548 回滚；在完成接口命名、构建脚本、POSIX/hermit ABI 支撑和示例配置调整后，最终由 PR #561 重新合入。当前源码位于 `os/arceos/ulib/arceos-rust`，并包含 `arceos-rust-interface`、std 示例应用和多架构测试配置。后续 PR 继续补齐 linker script 路径、发布元数据，以及 I/O、线程和 Tokio 等测试覆盖。

- [PR #374](https://github.com/rcore-os/tgoskits/pull/374) — ArceOS Rust standard library 支持首次合入（eternalcomet）
- [PR #548](https://github.com/rcore-os/tgoskits/pull/548) — 回滚 #374 以处理集成风险（周睿）
- [PR #561](https://github.com/rcore-os/tgoskits/pull/561) — 重新合入 Rust std app support for ArceOS（eternalcomet）
- [PR #581](https://github.com/rcore-os/tgoskits/pull/581) — 修复 `arceos-rust` build script 中 linker script 路径不匹配（周睿）
- [PR #615](https://github.com/rcore-os/tgoskits/pull/615) — `arceos-rust-interface` 发布元数据补充（周睿）
- [PR #621](https://github.com/rcore-os/tgoskits/pull/621) — ArceOS I/O、threading 和 Tokio 测试覆盖（ZCShou）

### 运行时可靠性

ArceOS 本月继续加强运行时错误发现能力。lockdep 扩展了 task-held tracking，并加入 QEMU 回归覆盖；多任务栈增加 canary 检查，用于更早发现栈溢出或破坏；panic 路径增加递归防护，避免二次 panic 导致信息丢失或系统陷入不可诊断状态。

- [PR #415](https://github.com/rcore-os/tgoskits/pull/415) — lockdep task-held tracking 与 QEMU 回归覆盖（Shi Lei）
- [PR #416](https://github.com/rcore-os/tgoskits/pull/416) — 多任务栈 canary 检查（Shi Lei）
- [PR #420](https://github.com/rcore-os/tgoskits/pull/420) — ax-runtime panic 递归防护（Shi Lei）
- [PR #861](https://github.com/rcore-os/tgoskits/pull/861) — 工作区 spin 使用迁移到 ax-kspin（Shi Lei）

### 网络栈与 POSIX 行为

网络栈方面，`ax-net` 迁移到 crates.io 版本的 smoltcp，并在后续同步到 smoltcp 0.13.1。`ax-net-ng` 新增 ICMP raw socket 支持，并修复 TCP send 后未轮询接口导致 epoll waiter 无法被及时唤醒的问题。随后，socket option、ARP pending queue、SIOCGIFINDEX/FIONREAD 等行为也继续向 Linux 兼容语义靠拢。

- [PR #410](https://github.com/rcore-os/tgoskits/pull/410) — `ax-net` 迁移到 crates.io smoltcp（周睿）
- [PR #368](https://github.com/rcore-os/tgoskits/pull/368) — `ax-net-ng` ICMP raw socket 支持（sunhaosheng）
- [PR #485](https://github.com/rcore-os/tgoskits/pull/485) — TCP send 后轮询接口以唤醒 epoll waiter（CharlieV）
- [PR #884](https://github.com/rcore-os/tgoskits/pull/884) — `SO_TYPE`、`SO_PROTOCOL`、`SO_DOMAIN` socket option 支持（取地址符）
- [PR #911](https://github.com/rcore-os/tgoskits/pull/911) — ARP pending queue drain 与 cache TTL/capacity 调整（Feiran Qin）

---

## 三、StarryOS

5 月是 StarryOS Linux 兼容性和真实应用 bringup 快速推进的一月。系统调用、procfs、文件系统、网络、图形、调试、板级支持和测试覆盖都取得了持续进展，工作重点从单点 syscall 修复扩展到更完整的发行版应用运行环境。

### 进程、信号与系统调用兼容性

进程和信号方面，本月补齐了 `vfork` 父进程阻塞语义、`CLONE_VM` 下 exec 行为、`getpgrp`、POSIX timer、`mremap`、timerfd、message queue blocking、`sigwaitinfo` 等能力。同时，`brk`、`fallocate`、`mmap`、`fadvise64`、`clock_getres`、`membarrier`、`rseq`、`splice`、file sync、mount/umount2 等行为继续向 Linux errno 和 ABI 语义靠拢。

- [PR #377](https://github.com/rcore-os/tgoskits/pull/377) — `vfork` 父进程阻塞与 `CLONE_VM` 下 exec 修复（CharlieV）
- [PR #409](https://github.com/rcore-os/tgoskits/pull/409) — `vfork`、`getpgrp` 和时间系统调用增强（YanLien）
- [PR #205](https://github.com/rcore-os/tgoskits/pull/205) — `mremap` 实现（Joseph Joshua Anggita）
- [PR #341](https://github.com/rcore-os/tgoskits/pull/341) — POSIX timer 系统调用实现（CharlieV）
- [PR #503](https://github.com/rcore-os/tgoskits/pull/503) — timerfd 支持（Joseph Joshua Anggita）
- [PR #488](https://github.com/rcore-os/tgoskits/pull/488) — message queue 阻塞语义（CHEN XIZHOU）
- [PR #535](https://github.com/rcore-os/tgoskits/pull/535) — `sigwaitinfo` 阻塞信号等待修复（CharlieV）
- [PR #876](https://github.com/rcore-os/tgoskits/pull/876) — mount 和 umount2 语义对齐 Linux（54dK3n）
- [PR #897](https://github.com/rcore-os/tgoskits/pull/897) — `sys_membarrier` 和 `sys_rseq` 补齐（ya2yo）

### procfs、busybox 与网络兼容性

procfs 和 busybox 兼容性是本月最密集的方向之一。我们实现或修复了 `/proc/stat`、`/proc/cpuinfo`、`/proc/uptime`、`/proc/meminfo`、`/proc/[pid]/status`、`/proc/[pid]/statm` 和 `sysinfo()`，并补齐 busybox `pidof`、`arp`、`ip addr`、`ip link`、`hwclock`、`ttysize`、`nice`、`arping`、`run-parts` 等命令依赖的内核接口。

- [PR #452](https://github.com/rcore-os/tgoskits/pull/452) — `/proc/stat`、`/proc/cpuinfo`、`/proc/uptime`，并修复 `/proc/meminfo` / `sysinfo()`（Feiran Qin）
- [PR #482](https://github.com/rcore-os/tgoskits/pull/482) — procfs 暴露 init pid 以支持 busybox `pidof`（Hong Deyao）
- [PR #480](https://github.com/rcore-os/tgoskits/pull/480) — procfs 暴露 ARP 表以支持 busybox `arp`（Hong Deyao）
- [PR #481](https://github.com/rcore-os/tgoskits/pull/481)、[PR #483](https://github.com/rcore-os/tgoskits/pull/483) — busybox `ip addr` / `ip link` 支持（Hong Deyao）
- [PR #477](https://github.com/rcore-os/tgoskits/pull/477) — busybox `nice` 支持（Hong Deyao）
- [PR #484](https://github.com/rcore-os/tgoskits/pull/484) — busybox `arping` 支持（Hong Deyao）
- [PR #517](https://github.com/rcore-os/tgoskits/pull/517) — `run-parts` 非 ELF fallback 到 `/bin/sh`（Zitao Chen）

网络方面，最小 netlink socket、AF_NETLINK rtnetlink/uevent/genl plumbing、UDP loopback dispatch、TCP/UDP bind 检查拆分和部分 socket ioctl/option 行为完成修复，为 busybox、OpenJDK 和发行版网络工具提供了更稳定的基础。

- [PR #512](https://github.com/rcore-os/tgoskits/pull/512) — AF_NETLINK rtnetlink/uevent/genl plumbing（Joseph Joshua Anggita）
- [PR #529](https://github.com/rcore-os/tgoskits/pull/529) — UDP loopback dispatch 与 recv EAGAIN 语义（CharlieV）
- [PR #543](https://github.com/rcore-os/tgoskits/pull/543) — TCP/UDP bind 检查拆分（sunhaosheng）
- [PR #869](https://github.com/rcore-os/tgoskits/pull/869) — TCP socket `FIONREAD` 支持（Long Weili）
- [PR #923](https://github.com/rcore-os/tgoskits/pull/923) — `SIOCGIFINDEX` 与 `SIOCGIFCONF` sizing 修复（禾可）

### 文件系统、块设备与 rsext4

文件系统方面，本月集中修复了目录、链接、rename、truncate、mknod、权限、file sync、mount/umount、xattr、inotify、loop/mount cache 与 busybox 场景的边界语义。块设备方面，loop 设备新增 `BLKSSZGET` / `BLKPBSZGET` ioctl，并修复非块 fd 上的 ioctl warning。

- [PR #449](https://github.com/rcore-os/tgoskits/pull/449) — `linkat` flags 验证与 symlink 语义保留（Hong Deyao）
- [PR #451](https://github.com/rcore-os/tgoskits/pull/451) — `renameat2` 支持 `RENAME_NOREPLACE`（Hong Deyao）
- [PR #460](https://github.com/rcore-os/tgoskits/pull/460)、[PR #462](https://github.com/rcore-os/tgoskits/pull/462) — `faccessat2` / `fchmodat2` 参数验证（Hong Deyao）
- [PR #463](https://github.com/rcore-os/tgoskits/pull/463)、[PR #464](https://github.com/rcore-os/tgoskits/pull/464) — `readlinkat` 零长度 buffer、`mknodat` mode type-zero 语义修复（Hong Deyao）
- [PR #472](https://github.com/rcore-os/tgoskits/pull/472) — advisory file locks（Pengjie Wang）
- [PR #501](https://github.com/rcore-os/tgoskits/pull/501) — loop/mount 缓存语义（Ticonderoga2017）
- [PR #882](https://github.com/rcore-os/tgoskits/pull/882) — rsext4 xattr syscall stubs（取地址符）

rsext4 继续强化异常恢复和高压场景下的稳定性。journal recovery、MBR 分区扫描、clean journal 重复回放、readdir 物理偏移、多 entry clock LRU 等路径完成修复或优化，提升了板级测试、自编译和异常掉电后的恢复能力。

- [PR #531](https://github.com/rcore-os/tgoskits/pull/531) — mount repair 前先回放 journal（周睿）
- [PR #539](https://github.com/rcore-os/tgoskits/pull/539) — 避免 clean journal 重复回放（YanLien）
- [PR #927](https://github.com/rcore-os/tgoskits/pull/927) — rsext4 journal recovery 与 MBR partition scanning 改进（YanLien）
- [PR #971](https://github.com/rcore-os/tgoskits/pull/971) — 多 entry clock LRU block cache（Tempest）
- [PR #1001](https://github.com/rcore-os/tgoskits/pull/1001) — rsext4 readdir 使用物理 byte offset（取地址符）

### 图形、桌面与应用 bringup

图形桌面相关工作在 5 月从预研进入核心路径合入阶段。DRM dumb buffer、KMS、evdev、sysfs/udev、Weston bringup、AF_UNIX cmsg、memfd seal 和 Apple HVF native execution 等支撑能力陆续进入 dev，使 StarryOS 具备更完整的图形栈 bringup 基础。

- [PR #506](https://github.com/rcore-os/tgoskits/pull/506)、[PR #514](https://github.com/rcore-os/tgoskits/pull/514) — DRM dumb buffer 与 KMS 能力（Joseph Joshua Anggita）
- [PR #508](https://github.com/rcore-os/tgoskits/pull/508)、[PR #509](https://github.com/rcore-os/tgoskits/pull/509) — sysfs/udev 与 Weston bringup（Joseph Joshua Anggita）
- [PR #513](https://github.com/rcore-os/tgoskits/pull/513) — evdev ioctl 补齐（Joseph Joshua Anggita）
- [PR #507](https://github.com/rcore-os/tgoskits/pull/507) — memfd seal mask 与 shrink/grow/write enforcement（Joseph Joshua Anggita）
- [PR #511](https://github.com/rcore-os/tgoskits/pull/511) — Apple HVF native execution 的 GICv3 与 CNTV backend（Joseph Joshua Anggita）

真实应用覆盖也继续扩展。Redis、SQLite、MariaDB、curl、DeepSeek TUI、GCC、sched-family、futex、socket、capget/capset、getrusage、shm、inotifywait、eBPF 用户态测试等陆续进入测试或应用覆盖，为后续发行版支持提供了更细的回归信号。

- [PR #865](https://github.com/rcore-os/tgoskits/pull/865) — shm syscall conformance suite（Joseph Zhao）
- [PR #894](https://github.com/rcore-os/tgoskits/pull/894) — inotifywait 支持（Shuo Zhang）
- [PR #898](https://github.com/rcore-os/tgoskits/pull/898) — capset syscall 用户态测试（WellDown64）
- [PR #902](https://github.com/rcore-os/tgoskits/pull/902) — getrusage syscall 用户态测试（WellDown64）
- [PR #907](https://github.com/rcore-os/tgoskits/pull/907) — DeepSeek TUI 示例应用（CharlieV）
- [PR #945](https://github.com/rcore-os/tgoskits/pull/945) — GCC compilation test case（Ticonderoga2017）
- [PR #988](https://github.com/rcore-os/tgoskits/pull/988) — jcode app case for x86_64 QEMU（Feiran Qin）

### 并发调试与运行时观测

并发与可诊断性方面，StarryOS 受益于工作区 lockdep 和运行时检查增强。futex/robust-list 的关键修复、动态调试控制、raw backtrace、memtrack alloc backtrace、qperf profiling、ax-task wake/preempt 修复和若干 SMP 调度改进，为定位长尾卡死问题提供了更细粒度的观测和回归路径。

- [PR #415](https://github.com/rcore-os/tgoskits/pull/415) — lockdep task-held tracking 与 QEMU 回归覆盖（Shi Lei）
- [PR #446](https://github.com/rcore-os/tgoskits/pull/446) — Starry runtime dynamic debug control（linfeng）
- [PR #498](https://github.com/rcore-os/tgoskits/pull/498) — 关闭 `FutexGuard::drop` 和 `close_all_fds` 中的 TOCTOU 窗口（Tempest）
- [PR #545](https://github.com/rcore-os/tgoskits/pull/545) — futex wait 与 robust-list 语义加固（Long Weili）
- [PR #619](https://github.com/rcore-os/tgoskits/pull/619) — axbacktrace raw backtrace report 与 ArceOS backtrace test（Jiaxin2006）
- [PR #793](https://github.com/rcore-os/tgoskits/pull/793) — axbuild host backtrace symbolize streaming（Jiaxin2006）
- [PR #1020](https://github.com/rcore-os/tgoskits/pull/1020) — memtrack alloc backtrace e2e（Jiaxin2006）

### 板级支持

围绕 RK3588 和 OrangePi 5 Plus，本月继续补齐 USB、网络和板级测试路径。StarryOS 可以通过 usbfs / sysfs 暴露 USB 设备，并添加 USB audio、storage、UVC 与 RKNN 示例；Realtek RTL8125 驱动完成 OrangePi board bringup，RK3588 PCIe clock gates 和 USB PHY clocks 也得到修复。SG2002 平台支持和 RISC-V 动态平台适配也进入 dev。

- [PR #404](https://github.com/rcore-os/tgoskits/pull/404) — realtek-rtl8125 OrangePi board bringup（周睿）
- [PR #474](https://github.com/rcore-os/tgoskits/pull/474) — RK3588 PCIe clock gates（周睿）
- [PR #528](https://github.com/rcore-os/tgoskits/pull/528) — RK3588 USB PHY clocks（周睿）
- [PR #554](https://github.com/rcore-os/tgoskits/pull/554) — SG2002 平台支持（周睿）
- [PR #555](https://github.com/rcore-os/tgoskits/pull/555) — 动态平台串口控制台输入修复（周睿）
- [PR #929](https://github.com/rcore-os/tgoskits/pull/929) — SG2002 CI build 修复（周睿）
- [PR #961](https://github.com/rcore-os/tgoskits/pull/961) — RISC-V QEMU 与 SG2002 动态平台支持（周睿）

---

## 四、Axvisor

### x86_64 虚拟化支持

x86_64 是 Axvisor 本月最重要的推进方向之一。AMD SVM 支持进入 dev，Intel VMX QEMU guest 启动支持完成，随后又继续增强 SVM 和 PIT 处理，为 Linux guest boot 和后续配置整理打基础。

- [PR #445](https://github.com/rcore-os/tgoskits/pull/445) — Axvisor x86_64 AMD SVM 支持（Ivans）
- [PR #526](https://github.com/rcore-os/tgoskits/pull/526) — x86_64 VMX QEMU guest boot 支持（Josen-B）
- [PR #930](https://github.com/rcore-os/tgoskits/pull/930) — x86_64 Linux guest boot VMX 支持（Josen-B）
- [PR #1005](https://github.com/rcore-os/tgoskits/pull/1005) — SVM 支持增强与 Linux guest PIT handling 改进（Josen-B）

### loongarch64 与板级测试

loongarch64 方向从任务拆分推进到部分 vCPU 和平台修复落地。guest timer emulation、runtime IPI 修复和用户态 HWCAP/LSX 相关支持进入 dev，为后续 Linux 客户机启动提供基础。与此同时，PhytiumPi 和 ROC-RK3568 board tests 也被纳入 Axvisor 测试体系。

- [PR #899](https://github.com/rcore-os/tgoskits/pull/899) — loongarch_vcpu guest timer emulation（numpy1314）
- [PR #873](https://github.com/rcore-os/tgoskits/pull/873) — loongarch64 QEMU virt runtime IPI deadlock 修复（禾可）
- [PR #917](https://github.com/rcore-os/tgoskits/pull/917) — loongarch64 LSX 状态保存和 HWCAP 暴露（禾可）
- [PR #934](https://github.com/rcore-os/tgoskits/pull/934) — PhytiumPi 与 ROC-RK3568 board tests（YanLien）

### 启动、配置与运行时

启动稳定性方面，本月的重点是 Axvisor 与 Starry 的 rootfs 处理和 QEMU 配置重构。构建流程统一 rootfs 路径解析、临时配置生成和测试执行入口，为后续多客户机启动和测试矩阵扩展打基础。月底，self-hosted runner、allocator 和 platform IRQ/FDT 整理继续落地。

- [PR #433](https://github.com/rcore-os/tgoskits/pull/433) — Axvisor rootfs 与 QEMU 配置重构（ZCShou）
- [PR #928](https://github.com/rcore-os/tgoskits/pull/928) — CI job 迁移到 self-hosted runner 并启用 container（ZCShou）
- [PR #974](https://github.com/rcore-os/tgoskits/pull/974) — Axvisor buddy-slab allocator 启用（周睿）
- [PR #979](https://github.com/rcore-os/tgoskits/pull/979) — platform-specific IRQ handling 与 architecture setup（ZCShou）

---

## 五、组件

### SomeHAL 与 Sparreal 组件迁移

组件层面，本月新增 SomeHAL 初始实现，作为硬件抽象方向的进一步探索。与此同时，我们迁移了 Sparreal driver crates，并添加 sparreal-os 组件仓库链接，为后续跨项目组件复用和发布整理做准备。

- SomeHAL 初始实现（周睿）
- [PR #540](https://github.com/rcore-os/tgoskits/pull/540) — Sparreal driver crates 迁移（周睿）
- Sparreal-os 组件仓库链接补充（周睿）

### 驱动接口与组件治理

驱动接口继续向可复用组件方向演进。DMA API 拆分 coherent 与 streaming 路径，静态 probe 迁移到 platform-owned registration；UART driver consolidation 和 block driver submit poll 也推动驱动层从单项目 glue 向更清晰的组件边界收敛。

- [PR #932](https://github.com/rcore-os/tgoskits/pull/932) — coherent 与 streaming DMA API 拆分（周睿）
- [PR #937](https://github.com/rcore-os/tgoskits/pull/937) — static probes 移交 platform-owned registration（周睿）
- [PR #965](https://github.com/rcore-os/tgoskits/pull/965) — UART driver consolidation（周睿）
- [PR #976](https://github.com/rcore-os/tgoskits/pull/976) — block drivers 切换到 submit poll（周睿）

### 发布元数据与依赖维护

工作区发布元数据和组件版本继续维护。我们更新了 core crate package versions，修复组件 release metadata，并移除了过时示例、刷新文档和依赖。rockchip-soc 升级至 0.1.2 后，又继续补齐 RK3588 PCIe clock gates 和 USB PHY clocks。

- [PR #458](https://github.com/rcore-os/tgoskits/pull/458) — 组件 release metadata 修复（周睿）
- [PR #470](https://github.com/rcore-os/tgoskits/pull/470) — core crate package versions 更新（周睿）
- [PR #456](https://github.com/rcore-os/tgoskits/pull/456) — rockchip-soc 0.1.2 更新与过时配置清理（周睿）
- [PR #474](https://github.com/rcore-os/tgoskits/pull/474) — RK3588 PCIe clock gates（周睿）
- [PR #528](https://github.com/rcore-os/tgoskits/pull/528) — RK3588 USB PHY clocks（周睿）

---

## 六、驱动

5 月硬件驱动工作围绕 RK3588、OrangePi 5 Plus、SG2002、SD/MMC 和可复用驱动接口展开。驱动层面的重点是 RTL8125 网络、RK3588 PCIe/USB clocks、动态平台 USB host 集成、SD/MMC host driver、serial/UART consolidation、block submit poll，以及不可用 SD/MMC 设备容忍处理。

### RK3588、USB 与 OrangePi 5 Plus

RK3588 和 OrangePi 5 Plus 相关驱动继续补齐。RTL8125 网络驱动完成 OrangePi board bringup，RK3588 PCIe clock gates 和 USB PHY clocks 得到修复，USB host 集成、USB board 支持和 usbfs/sysfs 暴露路径也支撑了 StarryOS 侧的 USB audio、storage、UVC 与 RKNN 示例。

- [PR #404](https://github.com/rcore-os/tgoskits/pull/404) — realtek-rtl8125 OrangePi board bringup（周睿）
- [PR #474](https://github.com/rcore-os/tgoskits/pull/474) — RK3588 PCIe clock gates（周睿）
- [PR #528](https://github.com/rcore-os/tgoskits/pull/528) — RK3588 USB PHY clocks（周睿）
- [PR #434](https://github.com/rcore-os/tgoskits/pull/434) — 容忍不可用 Rockchip SD/MMC 设备（周睿）
- [PR #964](https://github.com/rcore-os/tgoskits/pull/964) — cvi_usb_camera 初始化路径允许 sleep（wyatt-dai）
- [PR #980](https://github.com/rcore-os/tgoskits/pull/980) — Rockchip FIQ debugger UART（周睿）

### SD/MMC 与板级存储

SD/MMC 方向开始抽象可复用 host backend，完整的可复用 SD/MMC 协议和 host 驱动进入 dev。后续板级测试围绕 PhytiumPi、ROC-RK3568 和 Axvisor board test 展开，为 StarryOS 与 Axvisor 共用存储路径提供基础。

- reusable SD/MMC host backends 初步实现（YanLien）
- [PR #538](https://github.com/rcore-os/tgoskits/pull/538) — 可复用 SD/MMC 协议与 host 驱动（YanLien）
- [PR #934](https://github.com/rcore-os/tgoskits/pull/934) — PhytiumPi 与 ROC-RK3568 board tests（YanLien）

### SG2002 与 RISC-V 动态平台

SG2002 方向在 5 月进入 dev。平台支持合入后，CI build 修复和 RISC-V 动态平台适配继续补齐，为后续实际板级 smoke、外设覆盖和 StarryOS 测试矩阵联动做准备。

- [PR #554](https://github.com/rcore-os/tgoskits/pull/554) — SG2002 平台支持（周睿）
- [PR #929](https://github.com/rcore-os/tgoskits/pull/929) — SG2002 CI build 修复（周睿）
- [PR #961](https://github.com/rcore-os/tgoskits/pull/961) — RISC-V QEMU 与 SG2002 动态平台支持（周睿）

---

## 总结

5 月的工作主要围绕以下几个方向展开：

1. **Linux 兼容性与真实应用**：StarryOS 在进程、信号、内存、文件系统、procfs、TTY、网络、busybox、Debian/Alpine 应用和多架构测试覆盖上继续补齐。
2. **图形与桌面能力**：DRM/KMS、evdev、sysfs/udev、Weston、memfd seal 和 Apple HVF 相关支撑进入核心路径。
3. **虚拟化能力**：Axvisor x86_64 SVM/VMX guest boot、loongarch64 guest timer/IPI、board tests、allocator、IRQ/FDT 和 rootfs/QEMU 配置持续完善。
4. **构建与测试基础设施**：axbuild/rootfs/QEMU 配置进一步统一，CI 对 release、fork、safe.directory、hosted/self-hosted runner、Starry grouped tests 和真实应用压力测试做了加固。
5. **板级与驱动**：RK3588/OrangePi 5 Plus USB、PCIe、SD/MMC、RTL8125，SG2002 平台支持，以及 DMA/serial/block driver 抽象同步推进。
6. **组件治理**：SomeHAL、Sparreal driver crates、release metadata、rockchip-soc、DMA API 和驱动注册模型为后续复用和发布打基础。

感谢所有 60 位贡献者在 5 月的持续投入。
