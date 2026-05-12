---
slug: april-2026-retrospective
title: 2026 年 4 月开发月报
date: 2026-05-12T22:00:00+08:00
authors: [tgoskits-team]
tags: [monthly-report, arceos, starryos, axvisor, axbuild, testing]
---

2026 年 4 月是 TGOSKits 工作区快速发展的一月。全月共产生 **457 次非合并提交**、**102 次合并 PR**，涉及 **31 位贡献者**。本月在工作区层面完成了一次大规模的 crate 统一命名重构，在 StarryOS 层面大量修复了 Linux 兼容性系统调用的行为，在 Axvisor 层面新增了 FreeRTOS/Zephyr 客户机支持和龙芯架构 CI，在构建和测试基础设施上也做了大量改进。

<!-- truncate -->

## 总览

| 指标 | 数据 |
|------|------|
| 非合并提交数 | 457 |
| 合并 PR 数 | 102 |
| 贡献者人数 | 31 |
| 涉及 PR 编号范围 | #49 ~ #414 |

### 贡献者排行

| 贡献者 | 提交数 | 主要方向 |
|--------|--------|----------|
| 周睿 (ZR233) | 215 | 构建系统、crate 重命名、CI、工作区治理 |
| ZCShou | 55 | Axvisor/QEMU 集成、调试基础设施、文档 |
| chyyuu | 42 | crate 统一命名重构 |
| Codex Verify | 30 | 自动化版本管理与依赖更新 |
| Joseph Joshua Anggita | 19 | StarryOS 系统调用修复与特性实现 |
| Tempest (seek-hope) | 10 | StarryOS 测试与 bug 修复 |
| CharlieV | 10 | 文件系统修复、测试基础设施 |
| szy | 9 | 板级支持、驱动迁移、快速启动 |
| Shi Lei | 8 | 同步原语、锁依赖检测、调度器 |
| YanLien | 7 | RK3588 驱动、VM 重定位 |
| Josen-B | 7 | FreeRTOS/Zephyr 客户机支持 |
| 朝倉水希 | 5 | 工具链升级、BSS 修复、linkme 替换 |
| Ivans | 5 | Axvisor 测试与 vCPU 修复 |
| Ticonderoga2017 | 3 | findutils/grep 测试用例 |
| leeehh | 3 | stat 系统调用加固、mmap/madvise 修复 |
| 其他 16 位贡献者 | 若干 | 各类修复与改进 |

---

## 一、仓库设施

### 构建系统 (axbuild)

4 月的 axbuild 经历了多轮重构，核心目标是让 QEMU 仿真测试和物理板卡测试都能通过 `cargo xtask` 一键完成。我们实现了完整的 QEMU 测试编排流程，将 Axvisor 和 Starry 的测试路径统一到同一个编排框架下；随后为 Starry QEMU 测试添加了分组执行能力，使得测试可以按 normal/stress 等类别分别运行；远程物理板卡测试方面，为 OrangePi-5-Plus 建立了从构建、部署、串口等待到结果判定的完整流水线。

- [PR #394](https://github.com/rcore-os/tgoskits/pull/394) — QEMU 测试编排与 Axvisor/Starry 测试重构（ZCShou）
- [PR #369](https://github.com/rcore-os/tgoskits/pull/369) — Starry QEMU 测试分组执行（周睿）
- [PR #234](https://github.com/rcore-os/tgoskits/pull/234) — Starry QEMU 测试流程重构（周睿）
- [PR #199](https://github.com/rcore-os/tgoskits/pull/199) — OrangePi-5-Plus 远程板卡测试（周睿）
- [PR #189](https://github.com/rcore-os/tgoskits/pull/189) — 板卡配置助手（周睿）
- [PR #291](https://github.com/rcore-os/tgoskits/pull/291) — 目标构建配置增强（周睿）

在代码质量方面，我们贡献了 sync-lint 工具的两阶段实现，可以自动检查代码中 `atomic::Ordering` 的使用是否正确，帮助防止隐秘的并发 bug。同时还添加了基于 CSV 白名单的选择性 clippy 检查机制，让 CI 能够逐步扩大 crate 覆盖范围。

- [PR #274](https://github.com/rcore-os/tgoskits/pull/274) — sync-lint 原子序检查第一阶段（Shi Lei）
- [PR #322](https://github.com/rcore-os/tgoskits/pull/322) — sync-lint 混合序检查扩展（Shi Lei）
- 选择性 clippy（CSV 白名单 + CLI 选项）（周睿）

Rootfs 处理也经历了模块化重构。我们将共享的 rootfs 辅助函数提取到公共模块，并将根文件系统从原有格式迁移到 Alpine 以减小镜像体积。同时修复了 ld 在准备 staging rootfs 时解析到错误库的问题。

- [PR #340](https://github.com/rcore-os/tgoskits/pull/340) — rootfs 辅助函数提取（ZCShou）
- [PR #399](https://github.com/rcore-os/tgoskits/pull/399) — rootfs 处理模块化（ZCShou）
- [PR #297](https://github.com/rcore-os/tgoskits/pull/297)、[PR #380](https://github.com/rcore-os/tgoskits/pull/380) — Alpine 根文件系统迁移（ZCShou）
- [PR #413](https://github.com/rcore-os/tgoskits/pull/413) — ld 库解析修复（flying-mice987）
- [PR #414](https://github.com/rcore-os/tgoskits/pull/414) — 预构建脚本重构与 QEMU 发现增强（ZCShou）

### CI/CD 与工具链

我们重组了 CI 流水线，清理了阻塞 clippy 的历史问题；随后将可复用工作流统一，并将常用工具预装到 CI 容器中，减少了单次任务的安装开销。4 月 28 日还进行了一次大规模的工作区元数据继承清理（50+ 次提交），让所有子 crate 通过 `workspace.package` 继承统一的版本号、许可证和仓库地址。

- [PR #178](https://github.com/rcore-os/tgoskits/pull/178) — CI 重组与 clippy 阻塞修复（周睿）
- [PR #236](https://github.com/rcore-os/tgoskits/pull/236) — 可复用工作流统一与容器工具预装（周睿）
- [PR #195](https://github.com/rcore-os/tgoskits/pull/195) — dev-dependencies 裸金属目标限制（周睿）
- 工作区元数据继承（50+ 次提交）（周睿）

工具链方面，我们主导了两次 Rust 工具链升级，并将 `linkme` 库替换为 EII，修复了 BSS 段问题，清理了 StarryOS 中过时的代码。

- [PR #148](https://github.com/rcore-os/tgoskits/pull/148) — 工具链升级至 2026-04-01（朝倉水希）
- [PR #352](https://github.com/rcore-os/tgoskits/pull/352) — 工具链升级至 2025-04-27（朝倉水希）
- [PR #151](https://github.com/rcore-os/tgoskits/pull/151) — linkme 替换为 EII（朝倉水希）
- [PR #171](https://github.com/rcore-os/tgoskits/pull/171) — BSS 段修复（朝倉水希）
- [PR #353](https://github.com/rcore-os/tgoskits/pull/353) — StarryOS 过时代码清理（朝倉水希）

### 文档与调试体验

我们添加了全面的 VS Code 调试文档和配置，包括 ArceOS、Axvisor、StarryOS 的 launch 配置和 QEMU 调试脚本，降低了新贡献者的上手门槛。同时贡献了 StarryOS 快速启动指南。

- [PR #272](https://github.com/rcore-os/tgoskits/pull/272) — VS Code 调试文档与配置（ZCShou）
- [PR #406](https://github.com/rcore-os/tgoskits/pull/406) — 文档链接更新（ZCShou）
- [PR #183](https://github.com/rcore-os/tgoskits/pull/183) — StarryOS 快速启动指南（szy）
- [PR #389](https://github.com/rcore-os/tgoskits/pull/389) — 中断驱动控制台文档（周睿）
- [PR #388](https://github.com/rcore-os/tgoskits/pull/388) — 跨内核驱动技能文档（周睿）

### 测试基础设施

测试能力上，我们建立了 C/Rust 用户态测试基础设施，使得可以在 StarryOS 上直接编译和运行测试程序。同时添加了 Python 测试流水线和 busybox 测试用例，支持通过 sh 脚本注入来验证系统行为。

- [PR #235](https://github.com/rcore-os/tgoskits/pull/235) — C/Rust 用户态测试基础设施（Zhihang Shao）
- [PR #355](https://github.com/rcore-os/tgoskits/pull/355) — Python 测试流水线（CharlieV）
- [PR #299](https://github.com/rcore-os/tgoskits/pull/299) — busybox 测试用例（wyatt-dai）
- [PR #76](https://github.com/rcore-os/tgoskits/pull/76) — GitHub Codespace devcontainer（Zhihang Shao）
- [PR #391](https://github.com/rcore-os/tgoskits/pull/391) — review-open-prs 技能（周睿）

---

## 二、ArceOS

### Crate 统一命名重构

4 月初最重大的工程是将整个工作区中所有 crate 的名称统一到 `ax-*` 前缀规范下，在 4 月 7-8 日集中完成。此次重命名覆盖了 ArceOS 所有核心模块，涉及数百个文件中的引用更新：

- 内核基础设施：`axhal` → `ax-hal`、`axtask` → `ax-task`、`axmm` → `ax-mm`、`axsync` → `ax-sync`、`axdma` → `ax-dma`
- 驱动与设备：`axdriver` → `ax-driver`、`axnet` → `ax-net`、`axnet-ng` → `ax-net-ng`、`axinput` → `ax-input`、`axdisplay` → `ax-display`
- 文件系统：`axfs` → `ax-fs`、`axfs-ng` → `ax-fs-ng`
- 运行时与配置：`axruntime` → `ax-runtime`、`axconfig` → `ax-config`、`axplat` → `ax-plat`
- 标准库与 POSIX：`axstd` → `ax-std`、`axlibc` → `ax-libc`、`arceos_api` → `ax-api`、`arceos_posix_api` → `ax-posix-api`
- 示例应用：`arceos-helloworld` → `ax-helloworld`、`arceos-httpserver` → `ax-httpserver` 等

### 调度器与同步

我们在调度器和同步原语方面做出了多项改进：将调度器回退到旧机制并优化了 mutex handoff 逻辑，在 axtask 中强制执行 `might_sleep` 检查以防止在原子上下文中误调用睡眠操作。同时修复了中断 waker 注册时序——必须在标志交换之前完成注册，否则可能丢失唤醒事件，以及 RISC-V 平台上 IPI 唤醒丢失的问题。

- [PR #56](https://github.com/rcore-os/tgoskits/pull/56) — 调度器回退并优化 mutex handoff（Shi Lei）
- [PR #152](https://github.com/rcore-os/tgoskits/pull/152) — might_sleep 执行与回归修复（Shi Lei）
- [PR #316](https://github.com/rcore-os/tgoskits/pull/316) — 中断 waker 注册时序修复（Joseph Joshua Anggita）
- [PR #222](https://github.com/rcore-os/tgoskits/pull/222) — IPI 唤醒丢失修复（Shi Lei）

### 平台与运行时

平台支持方面，我们为 ArceOS 添加了 RISC-V 64 QEMU Virt 平台支持，实现了中断驱动的控制台输入替代原有的轮询方式，降低了空转时的 CPU 开销。VirtIO 设备支持也得到了加强：升级了 virtio-drivers 并添加了 PCI 块设备和 VirtIO-net-pci 支持。同时修复了 aarch64 plat-dyn 的 Huge PTE 检测和早期 FP/SIMD 初始化问题，统一了分配器后端并添加了 per-CPU buddy slab 支持。

- [PR #293](https://github.com/rcore-os/tgoskits/pull/293) — RISC-V 64 QEMU Virt 平台支持（ZCShou）
- [PR #343](https://github.com/rcore-os/tgoskits/pull/343) — 中断驱动控制台输入（周睿）
- [PR #287](https://github.com/rcore-os/tgoskits/pull/287) — IRQ、RTC 和 TTY 事件支持（周睿）
- [PR #169](https://github.com/rcore-os/tgoskits/pull/169) — VirtIO PCI 块设备升级（周睿）
- [PR #176](https://github.com/rcore-os/tgoskits/pull/176) — VirtIO-net-pci 支持（周睿）
- [PR #184](https://github.com/rcore-os/tgoskits/pull/184) — VirtIO 网络队列与缓冲区管理（ZCShou）
- [PR #168](https://github.com/rcore-os/tgoskits/pull/168) — Huge PTE 检测和早期 FP/SIMD 初始化（szy）
- [PR #154](https://github.com/rcore-os/tgoskits/pull/154) — 一次性定时器首触发修复（szy）
- [PR #149](https://github.com/rcore-os/tgoskits/pull/149) — PCI BAR 探测失败传播（Sasuke0723）
- [PR #161](https://github.com/rcore-os/tgoskits/pull/161) — per-CPU buddy slab 统一分配器（周睿）

---

## 三、StarryOS

4 月是 StarryOS 系统调用兼容性突飞猛进的一个月。多位贡献者在信号、进程凭证、内存管理、文件系统、网络 IPC、epoll 等领域贡献了大量修复，并新增了丰富的综合测试用例。

### 信号与进程

我们在信号处理方面做出了系统性改进：实现了 `SA_RESTART` 系统调用重启语义——当慢速系统调用被信号中断时内核会自动重启该调用；实现了 `PR_SET_PDEATHSIG`/`PR_GET_PDEATHSIG`，子进程可以在父进程死亡时收到指定信号。同时修复了 sigaltstack 的 MINSIGSTKSZ 检查和整体信号处理中的一系列问题。

- [PR #247](https://github.com/rcore-os/tgoskits/pull/247) — SA_RESTART 系统调用重启语义（Joseph Joshua Anggita）
- [PR #249](https://github.com/rcore-os/tgoskits/pull/249) — PR_SET_PDEATHSIG / PR_GET_PDEATHSIG（Joseph Joshua Anggita）
- [PR #207](https://github.com/rcore-os/tgoskits/pull/207) — sigaltstack MINSIGSTKSZ 检查（Joseph Joshua Anggita）
- [PR #49](https://github.com/rcore-os/tgoskits/pull/49) — 信号处理整体修复（Shi Lei）

进程管理方面最重要的改动是实现了每进程凭证子系统，引入了完整的 uid/gid/euid/egid/suid/sgid 管理框架和权限检查逻辑，使得文件访问权限判断、信号发送权限验证等有了正确的基础。相关修复还包括 sched affinity、prlimit64、clone3 等。

- [PR #246](https://github.com/rcore-os/tgoskits/pull/246) — 每进程凭证子系统（Joseph Joshua Anggita）
- [PR #276](https://github.com/rcore-os/tgoskits/pull/276) — sched affinity pid 参数修复（Shuo Zhang）
- [PR #267](https://github.com/rcore-os/tgoskits/pull/267) — proc status CPU 亲和性（Shuo Zhang）
- [PR #269](https://github.com/rcore-os/tgoskits/pull/269) — clone3 读取长度限制（Feiran Qin）
- [PR #319](https://github.com/rcore-os/tgoskits/pull/319) — prlimit64 允许提升硬限制（Joseph Joshua Anggita）
- [PR #208](https://github.com/rcore-os/tgoskits/pull/208) — getgroups size=0 查询修复（Joseph Joshua Anggita）

### 内存管理

我们将 mmap/munmap/mprotect 的错误返回值逐一与 Linux 行为对齐，确保返回正确的 errno。为 madvise 添加了 advice 参数验证、对齐检查和映射存在性判断。同时修复了 mremap 未正确保留映射共享类型的问题，以及 pause() 行为和 NULL 指针验证。

- [PR #285](https://github.com/rcore-os/tgoskits/pull/285) — mmap/munmap/mprotect 对齐 Linux（leeehh）
- [PR #278](https://github.com/rcore-os/tgoskits/pull/278) — madvise 参数验证（leeehh）
- [PR #263](https://github.com/rcore-os/tgoskits/pull/263) — mremap 保留映射共享类型（Tempest）
- [PR #296](https://github.com/rcore-os/tgoskits/pull/296) — pause() 和 NULL 指针验证（CharlieV）

### 文件系统

文件系统修复集中在 VFS 行为和 ext4 稳定性两方面。我们修复了 tmpfs 硬链接返回空数据的关键 bug——原因是硬链接创建时未传播页缓存。目录操作方面，修复了 `mkdir("/")` 返回值、目录 fd 的 read/write errno、打开目录时的 O_WRONLY 拒绝等问题。同时修复了 rename 时源 DirEntry 被提前释放的问题。

- [PR #378](https://github.com/rcore-os/tgoskits/pull/378) — tmpfs 硬链接页缓存传播（CharlieV）
- [PR #348](https://github.com/rcore-os/tgoskits/pull/348) — tmpfs 硬链接回归测试（韩佳辛）
- [PR #375](https://github.com/rcore-os/tgoskits/pull/375) — mkdir("/") 返回 EINVAL（CharlieV）
- [PR #264](https://github.com/rcore-os/tgoskits/pull/264) — 目录 fd read/write 返回 EISDIR（Tempest）
- [PR #324](https://github.com/rcore-os/tgoskits/pull/324) — 目录 fd write errno 修正（CharlieV）
- [PR #253](https://github.com/rcore-os/tgoskits/pull/253) — O_WRONLY/O_RDWR 对目录返回 EISDIR（CharlieV）
- [PR #312](https://github.com/rcore-os/tgoskits/pull/312) — rename 保留源 DirEntry（Joseph Joshua Anggita）
- [PR #303](https://github.com/rcore-os/tgoskits/pull/303) — lseek 负偏移返回 EINVAL（CharlieV）
- [PR #251](https://github.com/rcore-os/tgoskits/pull/251) — fsync/fdatasync 目录 fd 处理（Joseph Joshua Anggita）
- [PR #265](https://github.com/rcore-os/tgoskits/pull/265) — unlinkat 无效标志位拒绝（Jiaxin2006）

ext4 存储稳定性也有重要进展。我们修复了 rsext4 的 JBD2 日志回放问题，使得 Linux rootfs 在异常关机后可以被正确恢复；为数据块缓存添加了增长上限。同时从 x-kernel 同步了 ext4/rsext4 的崩溃一致性修复，并为 StarryOS 添加了 GPT 分区表扫描和根分区自动选择能力。

- [PR #398](https://github.com/rcore-os/tgoskits/pull/398) — rsext4 JBD2 日志回放修复（周睿）
- [PR #408](https://github.com/rcore-os/tgoskits/pull/408) — rsext4 数据块缓存增长限制（周睿）
- [PR #284](https://github.com/rcore-os/tgoskits/pull/284) — rsext4/ext4 崩溃一致性同步（Debin）
- [PR #179](https://github.com/rcore-os/tgoskits/pull/179) — GPT 分区扫描与根分区自动选择（szy）

### 网络/IPC 与 epoll

共享内存管理器在 SMP 场景下存在 AB/BA 死锁问题，我们通过调整锁获取顺序彻底修复。unix socket 方面修复了 bind 后 chown 行为和对端关闭后 recv 返回 EOF 的问题。eventfd 的读写语义也得到了修正。

- [PR #226](https://github.com/rcore-os/tgoskits/pull/226) — SHM AB/BA 死锁修复（Joseph Joshua Anggita）
- [PR #261](https://github.com/rcore-os/tgoskits/pull/261) — sys_shmat 错误处理（Tempest）
- [PR #313](https://github.com/rcore-os/tgoskits/pull/313) — unix socket bind 后 chown（Joseph Joshua Anggita）
- [PR #311](https://github.com/rcore-os/tgoskits/pull/311) — unix stream recv EOF（Joseph Joshua Anggita）
- [PR #370](https://github.com/rcore-os/tgoskits/pull/370) — eventfd 读写语义修复（manchangfengxu）

epoll 方面修复了 `EPOLL_CTL_MOD` 后未重新排队就绪事件的问题、`epoll_pwait` 的 sigsetsize 与 musl 的兼容性，以及 `F_GETFL` 返回不正确的访问模式标志。

- [PR #314](https://github.com/rcore-os/tgoskits/pull/314) — EPOLL_CTL_MOD 后重新排队（Joseph Joshua Anggita）
- [PR #250](https://github.com/rcore-os/tgoskits/pull/250) — epoll_pwait sigsetsize 兼容 musl（Joseph Joshua Anggita）
- [PR #260](https://github.com/rcore-os/tgoskits/pull/260) — F_GETFL 返回正确访问模式标志（Tempest）

### 其他系统调用

4 月还修复了大量零散的系统调用行为偏差。我们为 preadv2/pwritev2 添加了 offset=-1 支持（表示使用当前文件偏移）并拒绝了不支持的 flags；修复了 ftruncate、copy_file_range、getrandom 等参数验证；修复了 futex 等待时用户态内存访问的安全问题；对 stat/fstatat/statx 进行了全面加固。

- [PR #326](https://github.com/rcore-os/tgoskits/pull/326) — preadv2/pwritev2 offset=-1 支持（CharlieV）
- [PR #258](https://github.com/rcore-os/tgoskits/pull/258) — pwrite64 负偏移验证（Tempest）
- [PR #280](https://github.com/rcore-os/tgoskits/pull/280) — pwritev2 修复（Zitao Chen）
- [PR #381](https://github.com/rcore-os/tgoskits/pull/381) — pwrite64 fd 验证（manchangfengxu）
- [PR #209](https://github.com/rcore-os/tgoskits/pull/209) — ftruncate 负长度拒绝（Joseph Joshua Anggita）
- [PR #211](https://github.com/rcore-os/tgoskits/pull/211) — copy_file_range 验证（Joseph Joshua Anggita）
- [PR #210](https://github.com/rcore-os/tgoskits/pull/210) — getrandom 标志验证（Joseph Joshua Anggita）
- [PR #256](https://github.com/rcore-os/tgoskits/pull/256) — pipe fd errno 修正（CharlieV）
- [PR #305](https://github.com/rcore-os/tgoskits/pull/305) — close_all_fds 实现（Debin）
- [PR #257](https://github.com/rcore-os/tgoskits/pull/257) — times() 进程 CPU 时间修复（Zitao Chen）
- [PR #302](https://github.com/rcore-os/tgoskits/pull/302) — futex 用户态内存访问修复（周睿）
- [PR #259](https://github.com/rcore-os/tgoskits/pull/259) — exit_robust_list futex 处理（Tempest）
- [PR #255](https://github.com/rcore-os/tgoskits/pull/255) — ioctl FIONBIO 修复（杨凯森）
- [PR #300](https://github.com/rcore-os/tgoskits/pull/300) — stat/fstatat/statx 加固（leeehh）
- [PR #262](https://github.com/rcore-os/tgoskits/pull/262) — TIOCSPGRP pgid 读取修复（Tempest）
- [PR #402](https://github.com/rcore-os/tgoskits/pull/402) — console UART 写入保持原始模式（周睿）

### 测试与板级支持

4 月新增了大量系统调用综合测试，包括 pipe/pipe2、session 管理和时间系统调用的综合测试、vectored I/O 边界测试、findutils 文件系统遍历测试和 grep 套件，以及 random 和 rlimit 边界测试。

- [PR #335](https://github.com/rcore-os/tgoskits/pull/335) — pipe/pipe2 综合测试（Tempest）
- [PR #336](https://github.com/rcore-os/tgoskits/pull/336) — session 管理测试（Tempest）
- [PR #334](https://github.com/rcore-os/tgoskits/pull/334) — 时间系统调用测试（Tempest）
- [PR #327](https://github.com/rcore-os/tgoskits/pull/327) — vectored I/O 边界测试（CharlieV）
- [PR #310](https://github.com/rcore-os/tgoskits/pull/310)、[PR #367](https://github.com/rcore-os/tgoskits/pull/367) — findutils 文件系统遍历测试（Ticonderoga2017）
- [PR #339](https://github.com/rcore-os/tgoskits/pull/339) — grep 套件和 rename 回归测试（Ticonderoga2017）
- [PR #350](https://github.com/rcore-os/tgoskits/pull/350) — random 和 rlimit 边界测试（Jiaxin2006）

板级支持方面，我们为 StarryOS 添加了 OrangePi 5 Plus 的启动路径和 S100 板卡支持，实现了从 DTB 解析物理内存大小，并修正了 RLIMIT_STACK 的默认值。

- [PR #170](https://github.com/rcore-os/tgoskits/pull/170) — OrangePi 5 Plus 启动路径（szy）
- [PR #194](https://github.com/rcore-os/tgoskits/pull/194) — S100 板卡支持（szy）
- [PR #248](https://github.com/rcore-os/tgoskits/pull/248) — DTB 物理内存大小解析（Joseph Joshua Anggita）

---

## 四、Axvisor

### 多客户机操作系统支持

4 月 Axvisor 最引人注目的进展是实现了对 FreeRTOS 和 Zephyr 两个实时操作系统的客户机启动支持。我们首先在 4 月初实现了 Zephyr 在 QEMU 和 PhytiumPi 上的启动支持，随后扩展到 tac-e400 平台，并同步添加了 OrangePi 5 Plus 上运行 FreeRTOS 和 Zephyr 的能力。这意味着 Axvisor 现在可以同时运行 Linux、FreeRTOS 和 Zephyr 三类客户机操作系统。

- [PR #365](https://github.com/rcore-os/tgoskits/pull/365) — tac-e400 平台 FreeRTOS/Zephyr 支持（Josen-B）
- [PR #390](https://github.com/rcore-os/tgoskits/pull/390) — FreeRTOS/Zephyr VM 入口点修复（Josen-B）
- Zephyr 内核 QEMU/PhytiumPi 启动支持（Josen-B）
- FreeRTOS OrangePi 5 Plus 启动支持（Josen-B、YanLien）

### 架构与平台扩展

我们为 Axvisor 添加了龙芯 LoongArch64 架构的 QEMU 支持和 CI 测试，拓展了架构覆盖范围。为 RISC-V 64 添加了虚拟 PMU 和性能计数器支持，为客户机提供了性能分析能力。同时为 riscv64 QEMU 测试添加了 Linux 客户机支持，并添加了 RK3588 PCIe 主机控制器支持，将 RK3588 时钟驱动迁移到独立的 rockchip-soc 仓库。

- [PR #242](https://github.com/rcore-os/tgoskits/pull/242) — LoongArch64 QEMU 支持和 CI（numpy1314）
- [PR #405](https://github.com/rcore-os/tgoskits/pull/405) — RISC-V 64 虚拟 PMU（ZCShou）
- [PR #351](https://github.com/rcore-os/tgoskits/pull/351) — riscv64 QEMU Linux 客户机测试（Ivans）
- [PR #396](https://github.com/rcore-os/tgoskits/pull/396) — RK3588 PCIe 主机控制器（周睿）
- [PR #384](https://github.com/rcore-os/tgoskits/pull/384) — RK3588 时钟迁移（周睿）

### 关键修复

vCPU 管理方面有几个重要修复。我们修复了 Axvisor 关闭时未唤醒休眠 vCPU 导致挂起的问题，以及 guest 运行前后 host IRQ 状态未正确保存恢复的 bug。同时实现了板卡 guest rootfs 的 fsck 自动修复，避免了物理板卡在异常掉电后因文件系统损坏而无法启动，以及 VM 内核镜像在加载地址调整时的重定位问题。

- [PR #206](https://github.com/rcore-os/tgoskits/pull/206) — vCPU 关闭时唤醒休眠 vCPU（Ivans）
- [PR #186](https://github.com/rcore-os/tgoskits/pull/186) — 恢复 host IRQ 状态（Ivans）
- [PR #304](https://github.com/rcore-os/tgoskits/pull/304) — 板卡 rootfs fsck 自动修复（周睿）
- [PR #222](https://github.com/rcore-os/tgoskits/pull/222) — IPI 唤醒丢失修复（Shi Lei）
- VM 内核镜像重定位（YanLien）
- [PR #245](https://github.com/rcore-os/tgoskits/pull/245) — Axvisor LVZ 容器镜像发布（numpy1314）

---

## 五、组件

### Crate 统一命名

与 ArceOS 核心模块同步，共享组件也完成了 `ax-*` 前缀重命名。重命名涵盖了以下组件：

- `memory_addr` → `ax-memory-addr`、`memory_set` → `ax-memory-set`
- `percpu` → `ax-percpu`、`cpumask` → `ax-cpumask`
- `kspin` → `ax-kspin`、`kernel_guard` → `ax-kernel-guard`
- `handler_table` → `ax-handler-table`、`crate_interface` → `ax-crate-interface`
- `linked_list_r4l` → `ax-linked-list-r4l`、`int_ratio` → `ax-int-ratio`
- `timer_list` → `ax-timer-list`、`ctor_bare` → `ax-ctor-bare`
- `riscv_plic` → `ax-riscv-plic`、`cap_access` → `ax-cap-access`
- `arm_pl011` → `ax-arm-pl011`、`arm_pl031` → `ax-arm-pl031`
- `page_table_multiarch` → `ax-page-table-multiarch`、`page_table_entry` → `ax-page-table-entry`

### 同步原语与锁依赖检测

我们为 mutex 添加了 lockdep 支持，能够检测潜在的死锁和非法睡眠（如在中断上下文持有锁时调用 might_sleep）。为 `kspin` 添加了轻量级自旋锁锁依赖追踪。这两个工具配合使用，能够在开发阶段就发现同步相关的 bug。同时统一了跨架构的断点和调试陷阱处理，处理了缺省页表项使用默认页面大小的问题。

- [PR #271](https://github.com/rcore-os/tgoskits/pull/271) — mutex lockdep 支持（Shi Lei）
- [PR #164](https://github.com/rcore-os/tgoskits/pull/164) — 轻量级 spin lockdep（Shi Lei）
- [PR #244](https://github.com/rcore-os/tgoskits/pull/244) — 跨架构调试陷阱处理统一（linfeng）
- [PR #243](https://github.com/rcore-os/tgoskits/pull/243) — 缺省页表项默认页面大小处理（linfeng）

---

## 六、驱动

4 月在硬件驱动方面取得了多维度进展，涵盖 SoC 时钟/电源管理、块设备存储、串口通信、网络传输以及 PCIe 总线等子系统。

### RK3588 SoC 驱动

RK3588 芯片的驱动支持在 4 月取得了显著进展。我们实现了 CRU 时钟驱动，包含 NPU 相关的时钟配置；随后将这些时钟驱动迁移到新建立的 `rockchip-soc` 仓库，形成了独立的 SoC 支持模块，并添加了 PMU 寄存器定义和电源域管理代码。同时重构了 SD/MMC 驱动集成，添加了对多块读写操作的支持。

- [PR #241](https://github.com/rcore-os/tgoskits/pull/241) — RK3588 CRU 时钟驱动含 NPU 支持（YanLien）
- [PR #384](https://github.com/rcore-os/tgoskits/pull/384) — RK3588 时钟迁移到 rockchip-soc（周睿）
- RK3588 PMU 寄存器定义和电源域管理（周睿）
- SD/MMC 驱动集成重构与多块读写支持（YanLien）
- [PR #397](https://github.com/rcore-os/tgoskits/pull/397) — simple-sdmmc 依赖统一与 clippy 修复（YanLien）

### PCIe 与总线

我们为 RK3588 添加了 PCIe 主机控制器支持，使得 Axvisor 可以通过 PCIe 总线挂载各类高速设备。同时升级了 VirtIO 设备支持：升级了 virtio-drivers 并添加了 PCI 块设备和 VirtIO-net-pci 支持，调整了 VirtIO 网络设备的队列大小和缓冲区管理。还修复了 PCI BAR 探测失败时错误未正确传播的问题。

- [PR #396](https://github.com/rcore-os/tgoskits/pull/396) — RK3588 PCIe 主机控制器（周睿）
- [PR #169](https://github.com/rcore-os/tgoskits/pull/169) — VirtIO PCI 块设备升级（周睿）
- [PR #176](https://github.com/rcore-os/tgoskits/pull/176) — VirtIO-net-pci 支持（周睿）
- [PR #184](https://github.com/rcore-os/tgoskits/pull/184) — VirtIO 网络队列与缓冲区管理（ZCShou）
- [PR #149](https://github.com/rcore-os/tgoskits/pull/149) — PCI BAR 探测失败传播（Sasuke0723）

### 串口与网络驱动

我们创建了统一串口驱动集合 `some-serial`，同时支持 ARM PL011 和 NS16550A 两种常见的 UART 控制器，为不同平台提供了统一的串口抽象。添加了网络传输包装层 `rd-net`，封装了 DMA 缓冲区的分配和管理，简化了上层驱动代码。同时实现了 FDT 中重叠保留内存区域的自动合并。

- [PR #75](https://github.com/rcore-os/tgoskits/pull/75) — 统一串口驱动 some-serial（周睿）
- [PR #72](https://github.com/rcore-os/tgoskits/pull/72) — 网络传输包装层 rd-net（周睿）
- [PR #70](https://github.com/rcore-os/tgoskits/pull/70) — FDT 重叠保留内存区域合并（szy）

---

## 总结

4 月的工作主要围绕以下几个方向展开：

1. **命名规范化**：完成了工作区全量 crate 的 `ax-*` 前缀重命名，建立了统一的命名规范。
2. **系统调用兼容性**：StarryOS 在信号、凭证、内存、文件系统、IPC 等方面大幅提升了 Linux 兼容性。
3. **构建与测试基础设施**：axbuild 和 xtask 的 QEMU/板卡测试编排能力显著增强，CI 流水线更加高效。
4. **多 OS 支持**：Axvisor 新增 FreeRTOS/Zephyr 客户机支持和龙芯架构，拓展了应用场景。
5. **硬件驱动**：RK3588 的时钟、PMU、PCIe 等子系统驱动逐步就位，串口和网络驱动框架初步建立。
6. **调试与质量**：lockdep、sync-lint 等工具的加入让内核开发阶段的 bug 发现更加主动。

感谢所有 31 位贡献者在 4 月的辛勤付出！
