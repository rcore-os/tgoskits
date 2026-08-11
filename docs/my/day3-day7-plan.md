# 赛题一 Day3–Day7 冲刺计划

> 目标：一周内完成任务一的可提交主线，按 30 分评分项争取达到 24 分以上。
> 主线平台：QEMU AArch64 + AxVisor + Linux 2-vCPU + Zephyr 1-vCPU。
> 计划不包含任务二、任务三，避免并行扩张。

## OS 选择策略

本周任务一主线继续使用 Linux，不在 Day3 一开始切换 StarryOS：

- 赛题一明确要求多核 Linux Guest；Linux 是当前最稳的必选基线；
- StarryOS 的额外分值只有在后续任务二、任务三也用 StarryOS 完成时才成立，单独启动 StarryOS 不会自动加分；
- 当前已有 StarryOS 直接启动证据，但还没有 AxVisor 下的 StarryOS、网络和 AI 闭环证据，切换会阻塞任务一。

采用双轨策略：

1. **必选主线：** AxVisor + Linux 2-vCPU + Zephyr，先拿稳任务一 24/30 左右的基础分；
2. **加分支线：** Day6/Day7 在主线稳定后验证 StarryOS；只有 AxVisor 启动、网络和最小应用都稳定，才把它用于后续任务二/三争取额外分值。

因此，StarryOS 是后续加分项，不是本周任务一的替代入口。

## 当前起点

已完成：

- Linux 2-vCPU 在 AxVisor 下启动；
- Zephyr 在 AxVisor 下启动；
- Linux + Zephyr 双 Guest 同时启动；
- 原生 Zephyr 与 AxVisor Zephyr 的初步 microbenchmark；
- 延迟 CSV 统计脚本；
- Day1/Day2 证据文档。

未完成：

- 双 Guest 设备隔离已通过最小设备直通配置消除 DMA quarantine；
- 没有长时间周期任务 jitter 数据；
- 没有 AxVisor 实时关键路径的实质代码改造；
- 没有改造前后 A/B 数据；
- 没有完整 idle/stress/原生 RTOS 对照报告。

## 每日安排

### Day3：设备隔离 + 改造前基线

**目标：** 让实验环境干净，并冻结改造对象。

1. 复现双 Guest，确认 `excluded_devices` 是否实际加载；
2. 核对 Host FDT 和 Guest FDT 的 UART、GIC、timer、PCIe/NVMe 路径；
3. Day3 干净基线中 Linux 和 Zephyr 都只保留 UART/GIC/timer，不直通 PCIe/NVMe；Linux NVMe 直通作为后续单独对照场景；
4. 消除 DMA quarantine，或记录明确的设备生命周期根因；
5. 运行 Zephyr 10 ms 周期任务，采集三组 30 秒数据（若周期镜像已准备）：
   - Zephyr 单 Guest；
   - Linux + Zephyr，Linux idle；
   - Linux + Zephyr，Linux CPU stress；
6. 依据 p99、p99.9、max 和 deadline miss 冻结一个改造点。

**验收：** 双 Guest 可观察；设备告警为 0 或根因明确。周期镜像尚未准备时，不伪造 CSV，改造点顺延到 Day4。

**提交：** `docs/my/day3-progress.md`、原始日志/CSV、统计结果。

### Day4：实施一项 AxVisor 实时改造

**目标：** 满足“有实质改造”这一核心评分项。

1. 先写一个旧实现必失败的确定性回归测试；
2. 只改一个关键路径：
   - 默认优先选择 Day3 数据支持的 vIRQ pending/锁临界区或 timer rearm；
   - 只有数据明确显示共享调度竞争时，才改调度/抢占；
3. 保持 Linux、Zephyr、CPU 绑定和 QEMU 版本不变；
4. 运行目标 crate 的 fmt、clippy、回归测试；
5. 先做短 A/B（每组 30 秒），确认没有启动回归。

**验收：** 代码 diff 能解释实时性改善机制；回归测试在旧实现失败、新实现通过；双 Guest 仍可启动。

**提交：** 代码、测试、改造说明、Day4 A/B 初步数据。

### Day5：改造前后完整对比

**目标：** 把“改了代码”变成“数据证明有效”。

1. 使用同一镜像、配置、CPU 绑定和 workload；
2. 对改造前/改造后分别运行：
   - Zephyr idle；
   - Linux idle；
   - Linux CPU stress；
   - 若设备隔离稳定，再加 Linux NVMe/I/O stress；
3. 每组先 5 分钟，保留原始 CSV 和启动日志；
4. 统计 mean、p99、p99.9、max、deadline miss；
5. 记录调度延迟、中断响应延迟和周期抖动；
6. 生成一张改造前后对比表，不能只展示平均值。

**验收：** 至少一个压力场景的最坏延迟或超期次数改善，或者能明确说明改造没有收益并回滚到备选点。

**提交：** Day5 性能报告和全部原始数据。

### Day6：稳定性与复现收口

**目标：** 确认结果不是偶然，并让别人能复跑。

1. 选最终改造版本运行 30 分钟长稳测试；
2. 检查 panic、Guest 卡死、DMA quarantine、串口丢失和 vCPU 停顿；
3. 在干净 shell 环境复跑构建和启动命令；
4. 固化 Rust/QEMU/镜像哈希、CPU/内存/设备/中断配置；
5. 整理一键统计命令和结果目录结构；
6. 若 Day4 改造不稳定，优先回滚，不在 Day6 引入第二个大改造。

**验收：** 30 分钟无崩溃；新环境按文档能启动；数据和命令一一对应。

**提交：** 稳定性日志、复现说明、配置归档。

### Day7：任务一交付包

**目标：** 形成可以评审和提交的任务一材料。

1. 写实时化目标、问题定位、改造机制和非目标；
2. 写 Linux 2-vCPU、CPU 绑定、内存、设备、IRQ 和启动参数；
3. 写改造前/后、idle/stress、原生 Zephyr 对比方法；
4. 汇总最坏延迟、p99.9、deadline miss 和长稳结果；
5. 列出已知限制：QEMU 与真实板卡差异、Zephyr 选择原因、未完成项；
6. 运行最终 fmt、目标 crate clippy、回归测试和工作区状态检查；
7. 完成 Day7 文档后提交本周最后一个 commit。

**验收：** 任务一的 6 个评分点均有对应代码、命令、原始数据或解释；文档可复现。

## 评分目标映射

| 评分项 | 分值 | Day3–Day7 交付 |
| --- | ---: | --- |
| 实时目标与关键路径分析 | 4 | Day3 选点、Day7 设计说明 |
| 关键机制实质改造 | 8 | Day4 代码 + 回归测试 |
| Linux 2-vCPU 稳定启动 | 4 | 已完成，Day6 长稳复验 |
| 改造前后最坏延迟对比 | 5 | Day5 A/B 数据 |
| 空载与 stress 对比 | 4 | Day3 基线、Day5 完整矩阵 |
| 原生 Zephyr 基线对比 | 5 | Day2 初测、Day5/Day7 统一口径 |
| **目标合计** | **24/30 以上** | 还需评审数据质量和改造收益 |

## 三条停止规则

1. DMA quarantine 未解释前，不把实时数据写成正式结论；
2. Day4 改造没有确定性回归测试，不进入 Day5 大规模测量；
3. 一周内不切换 RTOS、不同时改多个 AxVisor 子系统、不新增复杂测试框架。
