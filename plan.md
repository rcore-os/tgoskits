# OpenRace 2026 赛题 1–3 核心执行计划

> 更新时间：2026-08-08
> 主线：QEMU AArch64 + AxVisor + Linux 2-vCPU + Zephyr 1-vCPU
> 详细历史 TODO：[docs/my/openrace2026-todo.md](docs/my/openrace2026-todo.md)

## 目标

完成三个可复现、可演示、可提交的闭环：

1. **任务一：** Axvisor 实时关键路径有一项实质改造，并用同口径数据证明尾延迟或抖动改善。
2. **任务二：** Linux/RTOS 之间通过双向 UDP/IP 完成控制、状态、错误消息和故障恢复。
3. **任务三：** Linux Guest 执行小型神经网络，驱动 RTOS 控制对象，并与固定参数控制公平对比。

## 固定决策

- 使用 QEMU AArch64；Linux Guest 使用 2 vCPU，Zephyr Guest 使用 1 vCPU。
- `passthrough` 作为启动、隔离和回归控制组；它不能单独证明 Axvisor 软件 vIRQ/timer 改造收益。
- 任务一必须另有可观测的软件 vIRQ/timer workload；PPI27 workaround 不作为 Gate。
- 任务二使用 virtio-net/桥接等 UDP/IP 数据面，不用 IVC、HyperCall、裸 MMIO 或 vsock 传业务载荷。
- 任务三使用固定权重的小型 MLP，不依赖 NPU；失败时回退固定参数控制。

## 执行顺序

### 0. 冻结实验输入

- [ ] 固定 commit、Rust nightly、QEMU、Guest 镜像/FDT、VM 配置、pCPU/vCPU 绑定、内存、设备/IRQ、workload、随机种子和运行时长。
- [ ] 每次实验保存 manifest、原始日志/CSV/trace、统计结果和命令。
- [ ] 固定指标：实时看 p99/p99.9/max、jitter、调度/中断响应和 overrun；网络看成功率、RTT、吞吐、超时和恢复时间；控制看至少两项误差/调节指标及端到端延迟。

### 1. 任务一：实时改造与验证

1. [ ] 对当前可运行的 A 基线做分层 trace，确认事件链：产生 → 入队 → 锁 → notify/IPI → 唤醒 → 注入 → Guest 中断 → 周期任务。
2. [ ] 若当前 workload 没覆盖目标软件层，只增加一个最小 synthetic vIRQ 或正确语义的 timer workload，并先通过 smoke test。
3. [ ] 用 trace 数据只选择一个主改造点：vIRQ 队列/锁、notify/IPI、唤醒/调度、timer 或 IRQ affinity。
4. [ ] 为该改造添加“旧实现必然失败、修复后通过”的确定性回归测试；运行 `cargo fmt` 和修改 crate 的 `cargo xtask clippy --package <crate>`。
5. [ ] 在相同镜像、配置、绑定、压力和样本数下完成 Native / A（未改造）/ B（改造后）的 idle + stress 对比，补齐 300/300 样本。
6. [ ] 完成 30 分钟初验和 1 小时最终长稳，检查 panic、Guest 卡死、IRQ 重入、队列溢出、DMA quarantine、串口/trace 丢失。

**任务一完成 Gate：** 软件路径有代码和 trace 双重证据；B 的实时尾部指标有可重复改善且无严重退化；Linux 2-vCPU 稳定运行；原始数据和复现命令齐全。

> 当前状态：启动、双 Guest 和短基线证据已有；实质实时改造收益、完整 A/B 和长稳尚未完成。当前先完成上面的 1–4，不把现有 `passthrough` 数据写成实时收益。

### 2. 任务二：双 Guest 网络通信

1. [ ] 为 Linux 和 Zephyr 分配独立网卡、MAC、IP、端口、IRQ 和后端；先验证互 ping 和双向 UDP。
2. [ ] 固定最小协议：`version/type/length/sequence/timestamp/status/error`；消息只保留 `SENSOR`、`CONTROL`、`STATUS`、`ERROR`、`ACK`、`HEARTBEAT`。
3. [ ] 实现 CONTROL/STATUS 的 ACK、超时、有限重传、重复包去重和乱序拒绝；heartbeat 超时进入安全模式，恢复后重建会话。
4. [ ] RTOS 返回实际应用值或拒绝原因；非法参数、版本错误和超时必须产生错误或安全降级。
5. [ ] 自动测试正常、丢包、短暂断网、重复/乱序；保存 pcap、两侧日志和机器可读摘要。

**任务二完成 Gate：** 抓包可证明业务走 UDP/IP；控制、状态、错误、重试、超时和恢复都有可复算证据。

### 3. 任务三：AI 控制闭环

1. [ ] 在 Zephyr 中建立固定参数控制基线、确定性对象、限幅和安全状态。
2. [ ] 在 Linux Guest 中运行固定权重的小型 MLP；记录输入/输出、归一化、模型来源和权重哈希。
3. [ ] 通过任务二协议完成 `SENSOR → Linux 推理 → CONTROL → RTOS 应用 → STATUS`；RTOS 校验范围、变化率、序号和新鲜度。
4. [ ] 对断网、模型退出和非法参数执行固定参数回退或安全降级，并记录状态。
5. [ ] 使用相同 seed、目标和扰动，多轮比较固定参数与 AI 控制；至少报告两项控制指标，并报告端到端延迟。
6. [ ] 准备 5 分钟双终端演示：传感器、推理结果、控制参数、实际应用值、状态回传和一次故障恢复。

**任务三完成 Gate：** Linux 确实执行模型；模型输出经任务二网络被 RTOS 实际应用；闭环和故障回退可观察；两项指标可复算。

### 4. 总体验收与提交

- [ ] 汇总任务一 Native/A/B 的 idle、stress、长稳数据。
- [ ] 汇总任务二正常和故障注入数据；所有数字可追溯到原始文件。
- [ ] 汇总任务三固定参数/AI 对比、模型信息和端到端延迟。
- [ ] 编写设计文档、测试文档和复现说明，包含依赖、构建、镜像、QEMU 命令、实验命令和结果生成方法。
- [ ] 在干净环境按文档复现核心演示，整理源码/配置 PR，并录制约 5 分钟视频。

## 最终提交判据

只有以下条件全部满足，才认为赛题 1–3 主线完成：

- 三个任务都有代码、配置、原始数据和文档证据。
- 任务一有改造前后、空载/压力、原生 RTOS 对照和长稳结果。
- 任务二有 UDP/IP 抓包、协议可靠性和故障恢复结果。
- 任务三有真实推理、RTOS 实际控制、故障回退和至少两项指标对比。
- 新环境可以依照复现说明完成核心启动、通信和闭环演示。

加分项（StarryOS、第二种 RTOS/板卡等）不进入当前主线，待上述 Gate 全部通过后再单独安排。
