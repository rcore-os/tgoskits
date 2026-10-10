---
slug: september-2026-retrospective
title: 2026 年 9 月开发月报
date: 2026-10-01T23:00:00+08:00
authors: [tgoskits-team]
tags: [monthly-report, arceos, starryos, axvisor, axbuild, testing]
---
2026 年 9 月，TGOSKits 围绕 [9 月开发计划 #2373](https://github.com/rcore-os/tgoskits/discussions/2373)，把 Axvisor 的客户机、IVC、调度和管理能力接入 RK3588 机器人场景。Linux/StarryOS 感知域与 Zephyr 控制域完成了实板基础验证，IVC 留下了超过 1 Gbps 的四档带宽记录，vCPU 超分实验推进到 8 个物理核运行 24 个 VM；调度器重构、任务切换测量和夜间性能历史也进入主线。月度总结会议判断机器人平台已基本进入收尾阶段，主要重构已经完成，后续开发重点转向性能优化、可靠性提升和安全性补充。

计划 #2373 的 128 实例、任务切换最大值小于 800 cycles 和无人干预捡球闭环尚未完成；双客户机机器人自动 CI 在月内因回归暂停。栈溢出、CPU 调度异常和系统崩溃问题仍需通过长稳测试、统一镜像构建和稳定版发布周期处理。

截至 9 月 30 日，`dev` 分支有 **239 次非合并提交**，全仓库合并 **231 个 PR**，其中 **229 个直接进入 `dev`**，涉及 **24 个唯一提交作者邮箱**。

<!-- truncate -->

## 1. 月度概览

统计截至 `dev` 提交 `d9878e4816e04f2fb88069a83d7bec65e73b92a1`，即 2026 年 9 月 30 日 13:05（北京时间）。

### 1.1 开发规模

`dev` 分支产生 239 次非合并提交，全仓库合并 231 个 PR。

| 指标                     | 数据                 |
| ------------------------ | -------------------- |
| `dev` 非合并提交数     | 239                  |
| 全仓库合并 PR 数         | 231                  |
| 直接合入`dev` 的 PR 数 | 229                  |
| 唯一提交作者邮箱数       | 24，含自动发布机器人 |
| 合并 PR 编号范围         | #1321～#2548         |

另外两个 PR 是进入 `main` 的 [PR #2402](https://github.com/rcore-os/tgoskits/pull/2402) 和进入调度器工作分支的 [PR #2247](https://github.com/rcore-os/tgoskits/pull/2247)。本月 `os/`、`test-suit/` 和 `scripts/` 分别出现在 131、111 和 106 次非合并提交中。

### 1.2 主要贡献者

贡献者提交数按作者邮箱统计，均为非合并提交；周睿的两个邮箱合计 98 次。

| 贡献者              | 提交数 |
| ------------------- | ------ |
| 周睿（ZR233）       | 98     |
| ZCShou              | 29     |
| 禾可（Lfan-ke）     | 16     |
| Josen-B             | 14     |
| YanLien             | 13     |
| Debin               | 11     |
| github-actions[bot] | 10     |
| szy（bullhh）       | 8      |
| Mchnan              | 7      |
| Alayfolk            | 6      |

## 2. 九月计划完成情况

讨论 #2373 的目标是以由固件直接引导的 Type 1 Axvisor 为基础，在 RK3588 上形成富功能域与实时、轻量域协同运行的机器人环境。

### 2.1 六项任务总览

AX-1～AX-6 的完成情况如下。

| 任务                    | 本月完成情况                                                                                                                        | 尚未满足或尚未确认的验收项                                                                                                     |
| ----------------------- | ----------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| AX-1 多客户机与资源分区 | Linux + Zephyr、StarryOS + Zephyr 已完成基础手动验证；PR #2314 接入两种双客户机实板测试；摄像头、NPU、UART6 和宿主管理设备的归属明确 | Linux、Zephyr、ArceOS 三客户机并行、创建前资源冲突拒绝和完整隔离结果仍待对应测试合入后的回归                                   |
| AX-2 IVC 通信           | PR #2214、PR #2268 已合入；四档各 100 次的板卡 benchmark 已取得超过 1 Gbps 的历史结果                                                | 当前版本夜间 benchmark 失败，需修复并重新回归；历史带宽结果已完成                                                              |
| AX-3 128 实例超分       | QEMU 双物理核实验已完成；PR #2498 提供 Orange Pi 8 核运行 24 个单 vCPU VM 的实板测试及心跳结果                                       | PR #2498 仍在 review；128 实例的全量 ping 与 `clean` 后不可达结果尚未完成                                                     |
| AX-4 调度与中断时延     | 调度器重构、唤醒路径优化和任务切换测例已合入；任务切换已完成板卡手动运行并生成 10 组报告                                            | 当前 10 组平均值为 1703～1707 cycles，计划要求的每组最大值小于 800 cycles 尚未达到；三客户机并行的 Zephyr 中断时延结果仍待补齐 |
| AX-5 管理与测试控制面   | 控制台、测试步骤、客户机构建和性能历史已通过 PR 接入统一入口，相关测例已支持手动运行                                                | 128 实例与三客户机的完整 `run/test/clean` 尚未完成；双客户机机器人自动 CI 因 PR #2494 暂停                                    |
| AX-6 机器人闭环         | PR #2314 已合入；Linux + Zephyr、StarryOS + Zephyr 均完成实板手动验证，包含感知、IVC、执行器反馈和清理                               | PR #2494 因 Zephyr 虚拟 MMIO 缺少设备移除双客户机自动检查；无人干预完整捡球和异常安全验收仍待恢复回归                           |

截至 9 月 24 日，[进度记录](https://github.com/rcore-os/tgoskits/discussions/2373#discussioncomment-18576437)显示，#2373 相关测试用例大部分已完成编写并通过手动验证，机器人平台进入收尾阶段。

### 2.2 多客户机与资源分区

AX-1 的主要成果是两种双客户机机器人配置进入可运行的实板测试：Linux 或 StarryOS 使用摄像头、NPU 和根存储，Zephyr 独占 UART6 来访问底盘与机械臂，Axvisor 保留管理串口、RTL8125 及相关 PCIe、USB0 和 PHY 资源。[PR #2314](https://github.com/rcore-os/tgoskits/pull/2314) 将两种场景接入同一块实体板的顺序测试，StarryOS 客户机从当前 checkout 构建，避免使用预部署内核代替本次源码。

对应配置保存在 `test-suit/axvisor/normal/board-orangepi-5-plus/dual-linux-zephyr/` 和 `dual-starry-zephyr/`。其中 `devices.disabled` 同时排除宿主管理设备及其依赖，避免客户机重置 PCIe 或 USB PHY 后使网页管理面失联。

本月还通过 [PR #2400](https://github.com/rcore-os/tgoskits/pull/2400) 修正 VM 中断解析顺序，[PR #2438](https://github.com/rcore-os/tgoskits/pull/2438) 保留客户机执行期间的宿主中断所有权。Linux、Zephyr、ArceOS 三客户机同时运行，以及创建前拒绝跨 VM 资源冲突的验收结果尚未完成。

### 2.3 跨客户机通信

AX-2 在 9 月补齐了 StarryOS 应用访问 IVC 的接口。[PR #2214](https://github.com/rcore-os/tgoskits/pull/2214) 提供 `/dev/axivc` 管理设备和 publisher/subscriber 通道设备，支持通道生命周期、等待对端、共享内存 `mmap` 与缓存维护；同时修正小缓冲区不消费消息、读取返回实际 payload 长度，以及超调用失败时恢复本地通道状态等语义。

[PR #2268](https://github.com/rcore-os/tgoskits/pull/2268) 加入 Orange Pi 5 Plus 的 Zephyr-Starry IVC benchmark。当前板卡配置要求最终标志 `AXVISOR_IVC_BENCH_RESULT=PASS cases=4 testTime=100 bytes=1232076800 chunks=400`，对应 256 KiB、512 KiB、1 MiB、10 MiB 四档、每档 100 次。9 月 20 日、主线版本 `e55adc1bbaca88f99ad64e0e757d3b4273d0eeba` 的[性能历史](https://github.com/rcore-os/tgoskits/blob/7e480c161c4cf82bc83cc64aa213f6b06b1c78f0/history.json)记录的平均带宽如下。

| 数据量  | 发送带宽     | 接收带宽     |
| ------- | ------------ | ------------ |
| 256 KiB | 2265.35 MB/s | 1704.28 MB/s |
| 512 KiB | 2282.96 MB/s | 1950.19 MB/s |
| 1 MiB   | 2286.86 MB/s | 2044.50 MB/s |
| 10 MiB  | 2239.21 MB/s | 2076.89 MB/s |

四档两个方向的记录均超过 1 Gbps。按测例的 KiB/MiB 数据长度计算，[9 月 30 日夜间运行](https://github.com/rcore-os/tgoskits/actions/runs/36642969439)中的 AXIVC benchmark 失败，性能历史没有该日的新 IVC 数据点。

### 2.4 轻量实例与超分

9 月上半月的[进度记录](https://github.com/rcore-os/tgoskits/discussions/2373#discussioncomment-18396211)引用 [PR #2131](https://github.com/rcore-os/tgoskits/pull/2131)，多个单 vCPU 客户机已在 QEMU AArch64 的两个物理 CPU 上分时运行。该实验 PR 后来关闭，未直接合并。

9 月下旬的[实板进度](https://github.com/rcore-os/tgoskits/discussions/2373#discussioncomment-18571149)报告了 Orange Pi 8 核运行 24 个 VM。[PR #2498](https://github.com/rcore-os/tgoskits/pull/2498) 生成 24 份 VM 配置，每个物理核分配 3 个单 vCPU VM，每个 VM 使用 128 MiB 内存，并通过 virtio-net 心跳检查客户机之间的通信。

PR #2498 仍未合并。计划要求的 `10.0.0.1`～`10.0.0.128` 全量 ping，以及 `clean` 后全部地址不可达，尚无完整结果。

### 2.5 调度切换与中断时延

AX-4 的实现基础在 9 月明显推进。[PR #1775](https://github.com/rcore-os/tgoskits/pull/1775) 于 9 月 8 日合入，重建调度器和运行时所有权；[PR #2313](https://github.com/rcore-os/tgoskits/pull/2313)、[PR #2357](https://github.com/rcore-os/tgoskits/pull/2357) 整理调度命名空间、线程生命周期和 PREEMPT_RT 锁语义。随后 [PR #2399](https://github.com/rcore-os/tgoskits/pull/2399)、[PR #2435](https://github.com/rcore-os/tgoskits/pull/2435) 优化调度、futex、唤醒及实时任务记账热路径；[PR #2386](https://github.com/rcore-os/tgoskits/pull/2386)、[PR #2389](https://github.com/rcore-os/tgoskits/pull/2389) 则把 CPU 算力信息接入 Fair 任务放置。

月度总结会议评价多核调度重构的性能已接近 Linux RT 水平，剩余差距主要来自锁机制、死循环等上层使用方式。

[PR #2428](https://github.com/rcore-os/tgoskits/pull/2428) 将任务切换测量放入 `test-suit/axvisor/normal/board-orangepi-5-plus/task-switch-overhead/`。客户机通过 AArch64 PMU cycle counter 测量同核两个 FIFO 线程的 ping-pong，默认执行 10 组，每个方向每组 100 万次，并输出 `AXVISOR_TASK_SWITCH_GROUP_SUMMARY` 的最小、平均和最大周期数。板卡入口已跑通，夜间历史也开始保存分组数据。

9 月 30 日、版本 `5fd1c6c84eb6e36b84a8de6f1424a2ade191f198` 的[历史数据](https://github.com/rcore-os/tgoskits/blob/7e480c161c4cf82bc83cc64aa213f6b06b1c78f0/history.json)中，10 组平均值为 1703～1707 cycles，高于计划的 800 cycles 门槛。当前 board 只匹配最后一组输出，夜间图表只保存平均值，尚无每组最大值小于 800 cycles 的结果。

[PR #2362](https://github.com/rcore-os/tgoskits/pull/2362) 落地带宿主负载的单 vCPU 吞吐回归，客户机自行检查工作量、定时唤醒和吞吐门槛。`blocks/s` 为吞吐指标，三客户机并行负载和 Zephyr 中断时延结果尚未补齐。

### 2.6 管理与测试控制面

AX-5 本月主要完成了已有管理能力与真实测试的衔接。`GuestConsoleMux` 相关修复覆盖物理与浏览器输出、COM1 恢复、输出顺序、背压和退出前排空，见 [PR #2248](https://github.com/rcore-os/tgoskits/pull/2248)、[PR #2204](https://github.com/rcore-os/tgoskits/pull/2204)、[PR #2285](https://github.com/rcore-os/tgoskits/pull/2285)、[PR #2311](https://github.com/rcore-os/tgoskits/pull/2311)、[PR #2473](https://github.com/rcore-os/tgoskits/pull/2473) 和 [PR #2488](https://github.com/rcore-os/tgoskits/pull/2488)。[PR #2462](https://github.com/rcore-os/tgoskits/pull/2462) 补充客户机镜像校验和 shell 编辑，减少管理操作中的无效输入。

测试侧，[PR #2361](https://github.com/rcore-os/tgoskits/pull/2361) 将执行流程迁到 `shell_check_steps`；`guest-builds.toml` 与 `guest_build::prepare()` 让性能测例在运行前自动构建所需客户机。[PR #2431](https://github.com/rcore-os/tgoskits/pull/2431) 把 Axvisor 压力和性能检查分到 nightly，[PR #2453](https://github.com/rcore-os/tgoskits/pull/2453) 把结果接入[性能历史页面](https://rcore-os.github.io/tgoskits/axvisor-perf/)。`ci_perf_report.py` 从客户机输出提取 IVC 带宽、vCPU 吞吐和任务切换指标，缺少成功输出的检查不会产生新数据点。

板卡测例、串口结果和历史指标已接入统一保存路径。128 实例和三客户机的 `run/test/clean` 尚未验收，双客户机机器人自动检查已暂停。

测试排队已拖慢开发，后续计划扩充 CI 服务器容量，并在局域网内部署 Docker 和 APK 镜像中转服务。

### 2.7 机器人分域闭环

AX-6 是本月集成工作的重点。单系统机器人程序中的感知与控制被拆到两个客户机，应用使用 `PerceptionResultV2` 和 IVC 传输感知结果，Zephyr 驱动底盘与机械臂并返回控制反馈。[PR #2314](https://github.com/rcore-os/tgoskits/pull/2314) 的实板验证覆盖配置握手、真实摄像头与 NPU 工作负载、两个 FPS 窗口、受控执行器反馈，以及完成后的资源清理。

两种场景使用相同 IVC key `0x49564301`，板卡入口以 `DUAL_PICK_CI_PASS` 作为最终结果。9 月 21 日的实测结果如下。

| 场景              | 两个性能窗口      | 控制与清理结果                          |
| ----------------- | ----------------- | --------------------------------------- |
| Linux + Zephyr    | 29.94 / 29.99 FPS | `checks=31`，IVC 丢帧 0，最终清理完成 |
| StarryOS + Zephyr | 29.47 / 30.02 FPS | `checks=31`，IVC 丢帧 0，最终清理完成 |

测试后半段使用预设球/桶结果驱动控制流程，允许模拟夹持成功，未覆盖真实识别准确率、地面行驶、实际抓球入桶、长期稳定性和硬件急停。AX-6 的无人干预寻球、追球、抓取、找桶、放球闭环尚未完成。

9 月 23 日的 [PR #2494](https://github.com/rcore-os/tgoskits/pull/2494) 记录了 Zephyr 访问 `0x09000030` 时虚拟 MMIO 总线缺少设备，感知端出现 IVC 通知失败和等待超时。双客户机机器人检查已从 CI catalog 移除，PR 和 nightly 均不再调度该条目。

机器人相关的单客户机路径也继续完善。[PR #2441](https://github.com/rcore-os/tgoskits/pull/2441) 恢复受权限约束的 NPU Direct DMA 提交，要求推理、性能、机械臂、运动反馈、停车和模型释放全部完成后才报告成功；原生 Linux、原生 StarryOS、Axvisor + Linux 和 Axvisor + StarryOS 分别取得 29.99、30.00、29.80 和 29.61 FPS 的历史结果。

后续利用连续推理和串口数据收发形成持续高负载，构造单客户机崩溃并观察 Hypervisor 和其他客户机；大厅展示机器人增加持续供电和运行计时，停机后自动清零。

### 2.8 SG2002 独立场景

M3 包括 SG2002 自主找球、夹球和连续运行。本月 [PR #2414](https://github.com/rcore-os/tgoskits/pull/2414) 修正 CVI JPEG 解码的色度格式，[PR #2548](https://github.com/rcore-os/tgoskits/pull/2548) 删除 SG2002 设备树中过期的 host initramfs 声明。

讨论 #2373 的 9 月进度更新未提供 SG2002 自主找球、夹球和连续运行结果，M3 尚未完成。

## 3. 其他主线进展

讨论 #2373 同时要求清理冗余代码、优化性能和处理遗留问题。本月还在 StarryOS 应用、存储、设备驱动和验证设施上合入了一批改进。

### 3.1 StarryOS 应用与 Linux 兼容

StarryOS 的用户态启动和图形应用继续扩展。[PR #2446](https://github.com/rcore-os/tgoskits/pull/2446) 将 OpenRC 设为 Alpine 默认 init；[PR #2226](https://github.com/rcore-os/tgoskits/pull/2226) 引入本地 StarryOS-backed `nixosTest` 框架。[PR #2483](https://github.com/rcore-os/tgoskits/pull/2483) 接入 virgl DRM，[PR #2284](https://github.com/rcore-os/tgoskits/pull/2284)、[PR #2365](https://github.com/rcore-os/tgoskits/pull/2365) 补齐 KMS 查询、dma-buf seek 和 vblank clock；[PR #2323](https://github.com/rcore-os/tgoskits/pull/2323) 加入 Weston 下 Firefox 的 HTTPS 渲染及远程桌面应用测试。

Linux 行为兼容继续按接口收敛，包括调度优先级范围、CPU affinity 参数长度、seccomp 校验、TTY session 与 PTY 生命周期，以及 Unix socket 和 epoll 唤醒。[PR #2274](https://github.com/rcore-os/tgoskits/pull/2274) 为 AArch64 Linux `perf` 提供内核支持，[PR #2433](https://github.com/rcore-os/tgoskits/pull/2433) 保持已激活 lease 对应的 epoll interest 注册，减少重复操作。

应用和接口仍有未收敛项：[PR #2466](https://github.com/rcore-os/tgoskits/pull/2466) 的容器用户态修补随后由 [PR #2480](https://github.com/rcore-os/tgoskits/pull/2480) 回退；`fcntl14` 在 [PR #2472](https://github.com/rcore-os/tgoskits/pull/2472) 修复后，又通过 [PR #2513](https://github.com/rcore-os/tgoskits/pull/2513) 暂时排除不稳定测例。

### 3.2 内存与存储性能

内存侧，[PR #2261](https://github.com/rcore-os/tgoskits/pull/2261) 重建 VMA 生命周期与事务更新，[PR #2520](https://github.com/rcore-os/tgoskits/pull/2520) 为 `memory-set` 的空闲区间建立索引，减少 VMA 放置扫描；[PR #2455](https://github.com/rcore-os/tgoskits/pull/2455) 将页表回收推迟到 TLB 确认之后，[PR #2464](https://github.com/rcore-os/tgoskits/pull/2464) 在惰性切换中保留带标签的 TLB 项。

存储侧，[PR #2349](https://github.com/rcore-os/tgoskits/pull/2349) 引入异步块请求运行时；[PR #2427](https://github.com/rcore-os/tgoskits/pull/2427)、[PR #2449](https://github.com/rcore-os/tgoskits/pull/2449)、[PR #2463](https://github.com/rcore-os/tgoskits/pull/2463) 分别处理回写边界、缓存淘汰与映射发布、块 IRQ 事件消费。[PR #2492](https://github.com/rcore-os/tgoskits/pull/2492) 批量回写连续脏 folio，[PR #2452](https://github.com/rcore-os/tgoskits/pull/2452)、[PR #2525](https://github.com/rcore-os/tgoskits/pull/2525) 优化 rsext4 格式化与 journal 提交阶段。AxVM 的 [PR #2310](https://github.com/rcore-os/tgoskits/pull/2310)、[PR #2390](https://github.com/rcore-os/tgoskits/pull/2390) 则补齐按需文件 I/O 和 x86 virtio-blk 文件后端。

### 3.3 驱动与设备能力

网络侧，[PR #2222](https://github.com/rcore-os/tgoskits/pull/2222) 接入 AKA WPA2 Wi-Fi 和 iperf 验证；AIC8800 随后修正固件 credit、队列预算和 D80 传输语义，并在 [PR #2547](https://github.com/rcore-os/tgoskits/pull/2547) 聚合发送写入、缓存固件 credit。RTL8125 数据路径也在 [PR #2227](https://github.com/rcore-os/tgoskits/pull/2227) 优化。StarryOS 通过 [PR #1566](https://github.com/rcore-os/tgoskits/pull/1566)、[PR #1567](https://github.com/rcore-os/tgoskits/pull/1567) 增加 TUN/TAP 虚拟网络设备与数据通路测试。

媒体与加速设备方面，[PR #2219](https://github.com/rcore-os/tgoskits/pull/2219) 支持通过 V4L2 和 usbfs 使用 UVC，[PR #2437](https://github.com/rcore-os/tgoskits/pull/2437) 统一 BSP-backed PWM 与 StarryOS 接入，[PR #2506](https://github.com/rcore-os/tgoskits/pull/2506) 整理可移植 GPU 和 display 驱动栈，[PR #2536](https://github.com/rcore-os/tgoskits/pull/2536) 让 virtio-gpu 异步提交使用真实 fence。设备 DMA 侧，[PR #2505](https://github.com/rcore-os/tgoskits/pull/2505)、[PR #2537](https://github.com/rcore-os/tgoskits/pull/2537) 补齐 ArceOS PCI DMA translation 和 SMMU 描述符发布顺序。

### 3.4 验证设施与代码清理

本月删除配置快照、不可执行脚手架和重复断言，相关改动有 [PR #2303](https://github.com/rcore-os/tgoskits/pull/2303)、[PR #2307](https://github.com/rcore-os/tgoskits/pull/2307)、[PR #2326](https://github.com/rcore-os/tgoskits/pull/2326)、[PR #2440](https://github.com/rcore-os/tgoskits/pull/2440)、[PR #2500](https://github.com/rcore-os/tgoskits/pull/2500)。StarryOS 的独立 syscall probes 分批迁入 LTP，[PR #2495](https://github.com/rcore-os/tgoskits/pull/2495) 提供单个 LTP 用例的 QEMU 运行入口。

CI 在支持 fork PR 的同时，通过 [PR #2474](https://github.com/rcore-os/tgoskits/pull/2474) 隔离 fork 与 self-hosted 运行器；[PR #2543](https://github.com/rcore-os/tgoskits/pull/2543) 避免已有 push 运行时重复展开 PR 矩阵，[PR #2545](https://github.com/rcore-os/tgoskits/pull/2545) 允许 PR 板卡任务取得空闲运行器。构建工具增加有界并行 Clippy、外部 Starry QEMU 运行与统一 host initramfs 流程，分别见 [PR #2482](https://github.com/rcore-os/tgoskits/pull/2482)、[PR #2509](https://github.com/rcore-os/tgoskits/pull/2509)、[PR #2528](https://github.com/rcore-os/tgoskits/pull/2528)。

## 4. 月度会议结论

机器人平台已基本收尾。后续开发聚焦性能优化、可靠性提升和安全性补充，采用统一镜像和稳定版流程组织开发与验证。

### 4.1 后续开发重点

性能优化覆盖内核和驱动热路径、锁机制、轮询和任务使用方式，并在真实机器人负载下验证调度器表现。

可靠性工作包括解决系统崩溃、栈溢出和 CPU 调度异常，增加 Hypervisor 与客户机专项测试。安全性工作从内存安全扩展到信息安全和功能安全，覆盖机器人执行器与客户机故障隔离。

### 4.2 镜像与仓库职责

所有测试用例、客户机操作系统及特殊场景的镜像构建与打包统一收归 TGOSImages，统一工具链、客户机和运行资源版本，支持特定硬件发行版构建。

| 仓库层次   | 会议明确的职责                                                    |
| ---------- | ----------------------------------------------------------------- |
| TGOSKits   | 内核、虚拟化及其他基础组件代码                                    |
| TGOSImages | 完整镜像构建系统、客户机与测试场景的构建及打包逻辑                |
| 发行版仓库 | 基于 TGOSImages 复用 TGOSKits，为 RK3588 等特定硬件生成完整发行版 |

当前 `test-suit/` 和客户机构建入口仍在 TGOSKits 中，Zephyr 镜像、模型及用户态资源依赖关联仓库和预部署内容，迁移边界尚未落地。

### 4.3 长稳与故障隔离

会议提出以机器人真实工况设计专项长稳测试，持续执行推理和串口数据收发，在高负载下观察系统崩溃、栈溢出及调度异常。故障隔离测试则主动构造单客户机崩溃，检查 Hypervisor 与其他客户机是否继续正常运行，补齐短时启动和功能测试无法证明的可靠性行为。

计划每周周末执行长稳测试，对发现的问题告警并提交 PR 修复。

大厅展示机器人增加电源连接与运行状态定时器，停机后自动清零。

### 4.4 稳定版发布

会议指出，组件频繁独立推送导致仓库没有明确的整体稳定版，难以对齐二期课题 1.0 版本的验收要求。拟采用工作区版本号代表整体稳定版，独立组件继续遵循语义化版本控制，使整体交付身份与各软件包版本能够同时追溯。组件版本不必强行一致，但需要明确某个工作区稳定版对应哪些组件与镜像。

| 阶段       | 工作安排                                       |
| ---------- | ---------------------------------------------- |
| 集中开发期 | 实现功能与新特性                               |
| 稳定测试期 | 暂停新功能合并，集中进行回归测试与 Bug 修复    |
| 统一发布期 | 测试通过后，将对应组件版本统一发布到 crates.io |

组件自动发布不等同于整体稳定版交付。稳定测试期暂停新功能合并，继续修复和回归。

### 4.5 命名与文档规范

部分组件命名缺少统一风格，计划编写统一的命名规范与编码风格指南。

工作区稳定版为文档提供版本依据，避免开发状态与已发布版本混写。

### 4.6 计划剩余验收

剩余验收项：

- 恢复 Zephyr 虚拟串口和双客户机机器人自动回归，重新验证当前主线镜像；同时处理最新 IVC benchmark 失败。
- 完成并合入 24 VM 超分改动，再推进到 128 实例，保存全量 ping 与 clean 后不可达的结果。
- 按当前任务切换测量口径定位开销，补齐 10 组最大值小于 800 cycles 的证据，以及三客户机并行时的 Zephyr 中断时延输出。
- 完成 Linux、Zephyr、ArceOS 三客户机并行和资源冲突、访问隔离验收，并接入管理面的完整测试与清理流程。
- 对 AX-6 单独验证真实无人干预抓球入桶、动作反馈关联，以及过期或损坏消息、失联和客户机异常时的执行器安全行为。
- 补齐 SG2002 自主找球、夹球与连续运行的独立板卡记录，不以 RK3588 场景结果替代。
