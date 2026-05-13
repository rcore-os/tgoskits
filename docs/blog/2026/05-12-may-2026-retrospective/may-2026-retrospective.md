---
slug: may-2026-retrospective
title: 2026 年 5 月开发月报
date: 2026-05-12T23:00:00+08:00
authors: [tgoskits-team]
tags: [monthly-report, arceos, starryos, axvisor, axbuild, testing]
---

2026 年 5 月的开发工作仍在进行中。本文统计范围为 **2026 年 5 月 1 日至 2026 年 5 月 12 日**，截至目前 TGOSKits 工作区共产生 **96 次非合并提交**、**80 个已进入 dev 的 PR**，涉及 **20 位贡献者**。本月上旬的重点集中在 StarryOS Linux 兼容性继续补齐、Axvisor x86_64 虚拟化能力推进、RK3588/OrangePi 5 Plus 板级和 USB 支持增强、axbuild/rootfs/CI 流程重构，以及部分仍在评审中的图形、memfd、SG2002 等方向。

<!-- truncate -->

## 总览

| 指标 | 数据 |
|------|------|
| 非合并提交数 | 96 |
| 已进入 dev 的 PR 数 | 80 |
| 贡献者人数 | 20 |
| 涉及 PR 编号范围 | #205 ~ #556 |
| 统计截止日期 | 2026-05-12 |

### 贡献者排行

| 贡献者 | 提交数 | 主要方向 |
|--------|--------|----------|
| 周睿 (ZR233) | 39 | StarryOS USB/网络/板级支持、CI、组件发布、驱动迁移 |
| Hong Deyao | 13 | StarryOS 文件/进程/procfs/busybox 兼容性 |
| ZCShou | 9 | axbuild、rootfs、文档、VisionFive2、配置系统 |
| Zitao Chen | 8 | StarryOS syscall 参数验证、loop/ioctl、busybox 测试 |
| CharlieV | 6 | POSIX timer、vfork/execve、TTY、网络 epoll 唤醒 |
| Shi Lei | 4 | lockdep、栈保护、panic 防护、调试文档 |
| Joseph Joshua Anggita | 2 | mremap、AArch64 EL0 cache 指令支持 |
| linfeng | 2 | 内存映射元数据、动态调试控制 |
| YanLien | 2 | vfork/time syscall、rsext4、SD/MMC 驱动 |
| 其他 11 位贡献者 | 若干 | StarryOS、Axvisor、ArceOS、网络与测试修复 |

---

## 一、仓库设施

### 自动测试与 QEMU/rootfs 流程

5 月上旬 axbuild 的工作重点是让 StarryOS、Axvisor 和板级测试场景共享更一致的 rootfs、QEMU 和平台配置流程。我们重构了 Axvisor 与 Starry 的 rootfs 处理和 QEMU 配置，将原先散落在不同路径中的启动参数、磁盘镜像准备和测试配置进一步集中；随后又整理了 QEMU 构建配置和测试执行流程，确保托管的 QEMU drive rootfs 镜像都能被正确准备。

- [PR #433](https://github.com/rcore-os/tgoskits/pull/433) — Axvisor 与 Starry rootfs / QEMU 配置重构（ZCShou）
- [PR #527](https://github.com/rcore-os/tgoskits/pull/527) — QEMU 构建配置与测试执行流程重构（ZCShou）
- Starry QEMU drive rootfs 镜像准备修复（周睿）
- Starry board examples 支持（周睿）

平台配置系统也继续演进。我们添加了 RISC-V VisionFive2 平台支持，平台包 alias 解析、manifest 目录解析、配置加载报告结构和 `cargo-axplat` 移除则在后续 PR 中继续推进。由于这部分仍有后续 PR 在推进，本文将其作为阶段性进展记录。

- [PR #541](https://github.com/rcore-os/tgoskits/pull/541) — RISC-V VisionFive2 平台支持与构建系统增强（ZCShou）
- [PR #552](https://github.com/rcore-os/tgoskits/pull/552) — 平台配置处理重构与 `cargo-axplat` 移除（ZCShou，后续合入）
- ax-config-gen 默认 feature 修复与 manifest 目录解析增强（ZCShou）

### 配置系统、CI 与发布流程

CI 方面，本月继续围绕 release、fork、hosted Axvisor 测试等路径做加固。我们规范化了 fork 场景下的 GHCR 镜像名称，修复了 release publish 的触发顺序，并为 Git/CI 命令添加了 `safe.directory` 支持，降低容器和 CI 环境中目录所有权不一致导致的失败概率。

- [PR #467](https://github.com/rcore-os/tgoskits/pull/467) — release publish 在 dev checks 后执行（周睿）
- [PR #469](https://github.com/rcore-os/tgoskits/pull/469) — fork 场景下 GHCR 镜像名称规范化（周睿）
- [PR #537](https://github.com/rcore-os/tgoskits/pull/537) — Git 和 CI 命令添加 `safe.directory` 支持（ZCShou）
- [PR #546](https://github.com/rcore-os/tgoskits/pull/546) — 跳过 hosted Axvisor SVM 测试（周睿）
- [PR #473](https://github.com/rcore-os/tgoskits/pull/473) — release-plz 失败处理与版本发布维护（周睿）

测试基础设施方面，StarryOS QEMU 测试新增了 Rust 用户程序交叉编译流水线，并将 PostgreSQL 覆盖保留在 stress 分组中；同时新增 USB board 与 QEMU smoke 测试，覆盖 OrangePi 5 Plus 相关设备路径。

- [PR #471](https://github.com/rcore-os/tgoskits/pull/471) — StarryOS QEMU 测试用例 Rust 交叉编译流水线（CharlieV）
- [PR #436](https://github.com/rcore-os/tgoskits/pull/436) — PostgreSQL 覆盖保留在 stress 分组（周睿）
- USB board 和 QEMU 测试用例补充（周睿）

### 文档与审查工作流

文档继续补齐开发、测试和审查流程。我们升级了 Docusaurus，并扩展了文档结构；同时新增 `review-single-pr` 技能，补充了单个 PR 集中审查流程，并对已有的 open PR 审查流程增加了冲突处理说明。

- [PR #435](https://github.com/rcore-os/tgoskits/pull/435) — Docusaurus 升级与文档章节增强（ZCShou）
- [PR #530](https://github.com/rcore-os/tgoskits/pull/530) — 架构、开发和测试流程文档增强（ZCShou）
- [PR #536](https://github.com/rcore-os/tgoskits/pull/536) — 新增 review-single-pr 技能（周睿）
- [PR #424](https://github.com/rcore-os/tgoskits/pull/424)、[PR #497](https://github.com/rcore-os/tgoskits/pull/497) — PR 审查工作流文档更新（周睿）
- [PR #453](https://github.com/rcore-os/tgoskits/pull/453) — 调试检查机制总结（Shi Lei）

### Issue 与后续 PR

截至 2026 年 5 月 12 日，本月新建的独立 issue 较少，项目讨论主要集中在 PR review 中推进。由于当前还未到月底，以下 5 月创建的 PR 在文章初稿中曾作为后续事项记录，其中一部分已经在当前仓库合入：

- [PR #555](https://github.com/rcore-os/tgoskits/pull/555) — 修复动态平台串口控制台输入（周睿，已合入）
- [PR #554](https://github.com/rcore-os/tgoskits/pull/554) — SG2002 平台支持（周睿，未完成）
- [PR #552](https://github.com/rcore-os/tgoskits/pull/552) — 平台配置处理重构（ZCShou，后续合入）
- [PR #545](https://github.com/rcore-os/tgoskits/pull/545) — futex wait 与 robust-list 语义加固（LetsWalkInLine，未完成）
- [PR #543](https://github.com/rcore-os/tgoskits/pull/543) — TCP/UDP bind 检查拆分（sunhaosheng，已合入）
- [PR #538](https://github.com/rcore-os/tgoskits/pull/538) — 可复用 SD/MMC 协议和 host 驱动（YanLien，未完成）
- [PR #535](https://github.com/rcore-os/tgoskits/pull/535) — sigwaitinfo 阻塞信号等待修复（CharlieV，未完成）
- [PR #526](https://github.com/rcore-os/tgoskits/pull/526) — Axvisor x86_64 VMX QEMU guest 启动（Josen-B，未完成）
- [PR #520](https://github.com/rcore-os/tgoskits/pull/520) — DHCP 租约生命周期与 `/proc/net/dhcp`（yydawx，未完成）
- [PR #503](https://github.com/rcore-os/tgoskits/pull/503) ~ [PR #518](https://github.com/rcore-os/tgoskits/pull/518) — timerfd、DRM、memfd、netlink、evdev、Weston、视觉回归等 StarryOS 桌面/图形相关能力（Joseph Joshua Anggita 等，部分后续合入）
- [PR #425](https://github.com/rcore-os/tgoskits/pull/425) — StarryOS Linux 兼容 KCOV 尝试（flying-mice987，未完成）

---

## 二、ArceOS

### Rust std 支持

本月有一次 ArceOS Rust 标准库支持的尝试进入 dev，但随后因集成风险被回滚。该过程为后续标准库支持留下了可复用的经验：这类跨工作区变更需要同时覆盖构建系统、crate feature、裸机目标限制和测试矩阵。

- [PR #374](https://github.com/rcore-os/tgoskits/pull/374) — ArceOS Rust standard library 支持尝试（eternalcomet）
- [PR #548](https://github.com/rcore-os/tgoskits/pull/548) — 回滚 #374（周睿）

### 运行时可靠性与网络栈

ArceOS 本月上旬继续加强运行时错误发现能力。lockdep 扩展了 task-held tracking，并加入 QEMU 回归覆盖；多任务栈增加 canary 检查，用于更早发现栈溢出或破坏；panic 路径增加递归防护，避免二次 panic 导致信息丢失或系统陷入不可诊断状态。

- [PR #415](https://github.com/rcore-os/tgoskits/pull/415) — lockdep task-held tracking 与 QEMU 回归覆盖（Shi Lei）
- [PR #416](https://github.com/rcore-os/tgoskits/pull/416) — 多任务栈 canary 检查（Shi Lei）
- [PR #420](https://github.com/rcore-os/tgoskits/pull/420) — ax-runtime panic 递归防护（Shi Lei）

网络栈方面，我们将 `ax-net` 迁移到 crates.io 版本的 smoltcp，并在后续同步到 smoltcp 0.13.1；`ax-net-ng` 新增 ICMP raw socket 支持，并修复 TCP send 后未轮询接口导致 epoll waiter 无法被及时唤醒的问题。动态平台和网络集成也随 USB/板级支持做了适配。

- [PR #410](https://github.com/rcore-os/tgoskits/pull/410) — `ax-net` 迁移到 crates.io smoltcp（周睿）
- smoltcp 更新至 0.13.1（周睿）
- [PR #368](https://github.com/rcore-os/tgoskits/pull/368) — `ax-net-ng` ICMP raw socket 支持（sunhaosheng）
- [PR #485](https://github.com/rcore-os/tgoskits/pull/485) — TCP send 后轮询接口以唤醒 epoll waiter（CharlieV）

---

## 三、StarryOS

5 月上旬 StarryOS 继续以 Linux 兼容性和真实应用 bringup 为主线。相对于按内核子系统拆分，本节更贴近当前开发计划的任务线：RK3588 机器人、并发卡死、busybox/procfs、Debian/syscall、文件系统、图形桌面、调试覆盖率和 SG2002。

### RK3588 机器人与板级支持

围绕 RK3588 和 OrangePi 5 Plus，本月继续补齐 USB、网络和板级测试路径。StarryOS 已能通过 usbfs / sysfs 暴露 USB 设备，并添加 USB audio、storage、UVC 与 RKNN 示例；Realtek RTL8125 驱动完成 OrangePi board bringup，RK3588 PCIe clock gates 和 USB PHY clocks 也得到修复。

- [PR #404](https://github.com/rcore-os/tgoskits/pull/404) — realtek-rtl8125 OrangePi board bringup（周睿）
- [PR #474](https://github.com/rcore-os/tgoskits/pull/474) — RK3588 PCIe clock gates（周睿）
- [PR #528](https://github.com/rcore-os/tgoskits/pull/528) — RK3588 USB PHY clocks（周睿）
- USB host 集成、RK3588 USB board 支持、usbfs / sysfs 暴露 USB 设备（周睿）
- USB audio、storage、OrangePi 5 Plus UVC / RKNN 示例（周睿、szy）
- [PR #555](https://github.com/rcore-os/tgoskits/pull/555) — 动态平台串口控制台输入修复（周睿，已合入）

### 并发卡死与运行时调试

并发与可诊断性方面，StarryOS 受益于工作区 lockdep 和运行时检查增强，同时有多项 futex / robust-list 修复正在推进。动态调试控制也进入 dev，为后续定位长尾卡死问题提供更细粒度的开关。

- [PR #415](https://github.com/rcore-os/tgoskits/pull/415) — lockdep task-held tracking 与 QEMU 回归覆盖（Shi Lei）
- [PR #446](https://github.com/rcore-os/tgoskits/pull/446) — Starry runtime dynamic debug control（linfeng）
- [PR #498](https://github.com/rcore-os/tgoskits/pull/498) — 关闭 `FutexGuard::drop` 和 `close_all_fds` 中的 TOCTOU 窗口（未完成）
- [PR #545](https://github.com/rcore-os/tgoskits/pull/545) — futex wait 与 robust-list 语义加固（LetsWalkInLine，未完成）

### busybox、procfs 与网络兼容性

procfs 和 busybox 兼容性是 5 月上旬最密集的方向之一。我们实现或修复了 `/proc/stat`、`/proc/cpuinfo`、`/proc/uptime`、`/proc/meminfo` 和 `sysinfo()`，并补齐 busybox `pidof`、`arp`、`ip addr`、`ip link`、`hwclock`、`ttysize` 等命令依赖的内核接口。网络方面，最小 netlink socket 和 TCP/UDP bind 检查拆分已进入 dev，DHCP、AF_NETLINK 和 UDP loopback 仍在评审中。

- [PR #452](https://github.com/rcore-os/tgoskits/pull/452) — `/proc/stat`、`/proc/cpuinfo`、`/proc/uptime`，并修复 `/proc/meminfo` / `sysinfo()`（Feiran Qin）
- [PR #482](https://github.com/rcore-os/tgoskits/pull/482) — procfs 暴露 init pid 以支持 busybox `pidof`（Hong Deyao）
- [PR #480](https://github.com/rcore-os/tgoskits/pull/480) — procfs 暴露 ARP 表以支持 busybox `arp`（Hong Deyao）
- [PR #481](https://github.com/rcore-os/tgoskits/pull/481)、[PR #483](https://github.com/rcore-os/tgoskits/pull/483) — busybox `ip addr` / `ip link` 支持（Hong Deyao）
- [PR #521](https://github.com/rcore-os/tgoskits/pull/521) — 移除 fake RTC 以修复 busybox `hwclock` 行为（Zitao Chen）
- [PR #490](https://github.com/rcore-os/tgoskits/pull/490) — busybox `ttysize` 测试用例（Zitao Chen）
- 最小 netlink socket 支持（周睿）
- [PR #477](https://github.com/rcore-os/tgoskits/pull/477) — busybox `nice` 支持（Hong Deyao，后续合入）
- [PR #478](https://github.com/rcore-os/tgoskits/pull/478)、[PR #484](https://github.com/rcore-os/tgoskits/pull/484) — busybox `iostat`、`arping` 支持（未完成）
- [PR #543](https://github.com/rcore-os/tgoskits/pull/543) — TCP/UDP bind 检查拆分（sunhaosheng，已合入）
- [PR #512](https://github.com/rcore-os/tgoskits/pull/512)、[PR #520](https://github.com/rcore-os/tgoskits/pull/520)、[PR #529](https://github.com/rcore-os/tgoskits/pull/529) — AF_NETLINK、DHCP 和 UDP loopback 语义（未完成）

### Debian 文件系统与系统调用兼容性

Debian 和通用 Linux 兼容性方面，我们实现了 `vfork` 父进程阻塞语义并修复 `CLONE_VM` 下 exec 的问题，补齐 `getpgrp`、部分时间系统调用、POSIX timer 和 `mremap`。同时，`brk`、`fallocate`、`mmap` fd 处理、`fadvise64`、`clock_getres` 等系统调用继续向 Linux errno 行为靠拢，timerfd 已进入 dev，`sigwaitinfo` 和部分 message queue 阻塞语义仍在 PR 中推进。

- [PR #377](https://github.com/rcore-os/tgoskits/pull/377) — `vfork` 父进程阻塞与 `CLONE_VM` 下 exec 修复（CharlieV）
- [PR #409](https://github.com/rcore-os/tgoskits/pull/409) — `vfork`、`getpgrp` 和时间系统调用增强（YanLien）
- [PR #205](https://github.com/rcore-os/tgoskits/pull/205) — `mremap` 实现（Joseph Joshua Anggita）
- [PR #341](https://github.com/rcore-os/tgoskits/pull/341) — POSIX timer 系统调用实现（CharlieV）
- [PR #500](https://github.com/rcore-os/tgoskits/pull/500) — 僵尸进程 `getsid` / `getpgid` / `getpriority` 返回值修复（CharlieV）
- [PR #224](https://github.com/rcore-os/tgoskits/pull/224)、[PR #468](https://github.com/rcore-os/tgoskits/pull/468) — 信号阻塞检查隔离与跨架构 signal restore 测试修复（Shuo Zhang、Long Weili）
- [PR #486](https://github.com/rcore-os/tgoskits/pull/486)、[PR #441](https://github.com/rcore-os/tgoskits/pull/441)、[PR #450](https://github.com/rcore-os/tgoskits/pull/450)、[PR #444](https://github.com/rcore-os/tgoskits/pull/444)、[PR #430](https://github.com/rcore-os/tgoskits/pull/430) — `brk`、`fallocate`、`mmap`、`fadvise64`、`clock_getres` 语义修复
- timerfd 与 file handle 支持（周睿）
- [PR #488](https://github.com/rcore-os/tgoskits/pull/488) — message queue 阻塞语义（CHEN XIZHOU，后续合入）
- [PR #535](https://github.com/rcore-os/tgoskits/pull/535) — `sigwaitinfo` 阻塞信号等待修复（CharlieV，未完成）

### 文件系统、块设备与 ext4/rsext4

文件系统方面，本月集中修复了大量目录、链接、rename、truncate、mknod、权限与 busybox 场景的边界语义。块设备方面，loop 设备新增 `BLKSSZGET` / `BLKPBSZGET` ioctl，并修复非块 fd 上的 ioctl warning。rsext4 则继续强化日志恢复路径，提升异常断电或板级测试后的恢复稳定性。

- [PR #449](https://github.com/rcore-os/tgoskits/pull/449) — `linkat` flags 验证与 symlink 语义保留（Hong Deyao）
- [PR #451](https://github.com/rcore-os/tgoskits/pull/451) — `renameat2` 支持 `RENAME_NOREPLACE`（Hong Deyao）
- [PR #460](https://github.com/rcore-os/tgoskits/pull/460)、[PR #462](https://github.com/rcore-os/tgoskits/pull/462) — `faccessat2` / `fchmodat2` 参数验证（Hong Deyao）
- [PR #463](https://github.com/rcore-os/tgoskits/pull/463)、[PR #464](https://github.com/rcore-os/tgoskits/pull/464) — `readlinkat` 零长度 buffer、`mknodat` mode type-zero 语义修复（Hong Deyao）
- [PR #466](https://github.com/rcore-os/tgoskits/pull/466) — `truncate` / `ftruncate` 参数验证（Zitao Chen）
- [PR #465](https://github.com/rcore-os/tgoskits/pull/465)、[PR #489](https://github.com/rcore-os/tgoskits/pull/489)、[PR #491](https://github.com/rcore-os/tgoskits/pull/491) — busybox blockdev / loop 设备和 ioctl 语义修复
- [PR #531](https://github.com/rcore-os/tgoskits/pull/531) — mount repair 前先回放 journal（周睿）
- [PR #539](https://github.com/rcore-os/tgoskits/pull/539) — 避免 clean journal 重复回放（YanLien）
- [PR #472](https://github.com/rcore-os/tgoskits/pull/472) — advisory file locks（Pengjie Wang，后续合入）
- [PR #501](https://github.com/rcore-os/tgoskits/pull/501) — loop/mount 缓存语义（未完成）

### SD/MMC 驱动

SD/MMC 方向开始抽象可复用 host backend，相关工作已经有部分提交进入 dev；完整的可复用 SD/MMC 协议和 host 驱动仍在 PR #538 中继续评审。

- reusable SD/MMC host backends 初步实现（YanLien）
- [PR #538](https://github.com/rcore-os/tgoskits/pull/538) — 可复用 SD/MMC 协议与 host 驱动（YanLien，未完成）

### X11 / Wayland 图形支持

图形桌面相关工作开始浮现，包括 DRM dumb buffer、KMS、evdev、sysfs/udev、Weston bringup 和视觉回归测试管线等。这些目前大多还未合入，但已经构成 5 月后半段的重要候选方向。

- [PR #506](https://github.com/rcore-os/tgoskits/pull/506)、[PR #514](https://github.com/rcore-os/tgoskits/pull/514) — DRM dumb buffer 与 KMS 能力（Joseph Joshua Anggita，未完成）
- [PR #508](https://github.com/rcore-os/tgoskits/pull/508)、[PR #509](https://github.com/rcore-os/tgoskits/pull/509) — sysfs/udev 与 Weston bringup（Joseph Joshua Anggita，未完成）
- [PR #513](https://github.com/rcore-os/tgoskits/pull/513) — aarch64 plat-dyn 默认关闭与 evdev ioctl 补齐（Joseph Joshua Anggita，未完成）
- [PR #516](https://github.com/rcore-os/tgoskits/pull/516) — visual-regression test pipeline 与 Xwayland 场景（Joseph Joshua Anggita，未完成）

### 运行时覆盖率与 SG2002 适配

运行时覆盖率方面，Linux 兼容 KCOV 接口仍在 PR 中尝试。SG2002 方向目前主要体现为 PR #554 中的开发板支持草案，尚未进入当前 dev/doc 分支。

- [PR #425](https://github.com/rcore-os/tgoskits/pull/425) — StarryOS Linux 兼容 KCOV 尝试（flying-mice987，未完成）
- [PR #554](https://github.com/rcore-os/tgoskits/pull/554) — SG2002 平台支持（周睿，未完成）

---

## 四、Axvisor

### loongarch64 架构支持

loongarch64 方向本月主要处于任务拆分和后续计划确认阶段。相关 issue 已经打开，后续目标是继续扩展 Axvisor 的 loongarch64 架构支持，并推进 Linux 客户机启动。

- [Issue #550](https://github.com/rcore-os/tgoskits/issues/550) — 扩展完善 Axvisor 的 loongarch64 架构支持并启动 Linux 客户机（未完成）
- [Issue #549](https://github.com/rcore-os/tgoskits/issues/549)、[Issue #553](https://github.com/rcore-os/tgoskits/issues/553) — 后续方向任务边界梳理（未完成）

### x86_64 架构支持

x86_64 是 Axvisor 本月上旬最重要的推进方向。AMD SVM 支持已经合入；Intel VMX QEMU guest 启动支持仍在评审中，并继续围绕 x86 虚拟化后端、BIOS 镜像加载和 QEMU 启动路径做配套整理。

- [PR #445](https://github.com/rcore-os/tgoskits/pull/445) — Axvisor x86_64 AMD SVM 支持（Ivans）
- [PR #526](https://github.com/rcore-os/tgoskits/pull/526) — x86_64 VMX QEMU guest boot 支持（Josen-B，未完成）
- [Issue #455](https://github.com/rcore-os/tgoskits/issues/455) — x86_64 VMX 处理器启动 Linux（未完成）

### 板级和 rootfs 启动稳定性

启动稳定性方面，本月实际合入的重点是 Axvisor 与 Starry 的 rootfs 处理和 QEMU 配置重构：构建流程统一 rootfs 路径解析、临时配置生成和测试执行入口，为后续多客户机启动和测试矩阵扩展打基础。OrangePi-5-Plus Linux smoke 中的 eMMC busy timeout 仍需继续跟踪。

- [PR #433](https://github.com/rcore-os/tgoskits/pull/433) — Axvisor rootfs 与 QEMU 配置重构（ZCShou）
- [Issue #442](https://github.com/rcore-os/tgoskits/issues/442) — Axvisor OrangePi-5-Plus Linux smoke eMMC busy timeout（未完成）

---

## 五、组件

### SomeHAL 与 Sparreal 组件迁移

组件层面，本月新增 SomeHAL 初始实现，作为硬件抽象方向的进一步探索。与此同时，我们迁移了 Sparreal driver crates，并添加 sparreal-os 组件仓库链接，为后续跨项目组件复用和发布整理做准备。

- SomeHAL 初始实现（周睿）
- [PR #540](https://github.com/rcore-os/tgoskits/pull/540) — Sparreal driver crates 迁移（周睿）
- Sparreal-os 组件仓库链接补充（周睿）

### 发布元数据、依赖与 rockchip-soc 维护

工作区发布元数据和组件版本继续维护。我们更新了 core crate package versions，修复组件 release metadata，并移除了过时示例、刷新文档和依赖。rockchip-soc 升级至 0.1.2 后，又继续补齐 RK3588 PCIe clock gates 和 USB PHY clocks。

- [PR #458](https://github.com/rcore-os/tgoskits/pull/458) — 组件 release metadata 修复（周睿）
- [PR #470](https://github.com/rcore-os/tgoskits/pull/470) — core crate package versions 更新（周睿）
- [PR #456](https://github.com/rcore-os/tgoskits/pull/456) — rockchip-soc 0.1.2 更新与过时配置清理（周睿）
- [PR #474](https://github.com/rcore-os/tgoskits/pull/474) — RK3588 PCIe clock gates（周睿）
- [PR #528](https://github.com/rcore-os/tgoskits/pull/528) — RK3588 USB PHY clocks（周睿）

---

## 六、驱动

### RK3588、USB 与 OrangePi 5 Plus 驱动

5 月上旬硬件驱动工作继续围绕 RK3588 和 OrangePi 5 Plus 展开。驱动层面的重点是 RTL8125 网络、RK3588 PCIe/USB clocks、动态平台 USB host 集成，以及不可用 SD/MMC 设备容忍处理；这些能力也支撑了 StarryOS 章节中提到的机器人和 USB 用户态场景。

- [PR #404](https://github.com/rcore-os/tgoskits/pull/404) — realtek-rtl8125 OrangePi board bringup（周睿）
- USB host 集成与 RK3588 USB board 支持（周睿）
- [PR #434](https://github.com/rcore-os/tgoskits/pull/434) — 容忍不可用 Rockchip SD/MMC 设备（周睿）
- RK3588 PCIe 资源 bringup（YanLien）

### SD/MMC 驱动

SD/MMC 方向开始抽象可复用 host backend，相关工作已经有部分提交进入 dev，完整的可复用 SD/MMC 协议和 host 驱动仍在 PR #538 中继续评审。

- reusable SD/MMC host backends 初步实现（YanLien）
- [PR #538](https://github.com/rcore-os/tgoskits/pull/538) — 可复用 SD/MMC 协议与 host 驱动（YanLien，未完成）

### SG2002 开发板支持

SG2002 方向目前仍处于 PR #554 的平台支持草案阶段，尚未进入当前 dev/doc 分支。本文只将其作为后续候选方向记录。

- [PR #554](https://github.com/rcore-os/tgoskits/pull/554) — SG2002 平台支持（周睿，未完成）

---

## 总结

5 月上旬的工作主要围绕以下几个方向展开：

1. **StarryOS 长尾兼容性**：继续补齐进程、信号、内存、文件系统、procfs、TTY、网络和 busybox 真实应用场景中的 Linux 语义。
2. **图形与桌面能力预研**：DRM、evdev、sysfs/udev、Weston、视觉回归等 PR 已经打开，虽未全部合入，但方向非常明确。
3. **Axvisor x86_64 支持**：AMD SVM 已进入 dev，VMX guest boot 和相关 QEMU/配置路径继续完善。
4. **构建与测试基础设施**：axbuild/rootfs/QEMU 配置进一步统一，CI 对 release、fork、safe.directory 和 hosted 测试路径做了加固。
5. **板级与驱动**：RK3588/OrangePi 5 Plus USB、PCIe、SD/MMC、RTL8125，以及 SG2002 平台支持同步推进。
6. **组件治理**：SomeHAL、Sparreal driver crates、release metadata 和 rockchip-soc 版本维护为后续复用和发布打基础。

由于本文统计截止于 **2026 年 5 月 12 日**，本月仍有多项 PR 会在后续继续变化。月底正式回顾时，可基于这些 PR 的最终状态继续补全统计数据和章节内容。
