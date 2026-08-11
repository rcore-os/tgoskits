# NEXT：下一步更复杂的 vIRQ A/B 分析

## 1. 目标

证明两件事：

1. B 的 targeted notify（定向唤醒）相对 A 的 `notify_all` 有可测量的端到端收益。
2. B 的 per-vCPU 有界队列在更长、更高压的并发场景下，比 A 的无界 VM 队列更稳。

## 2. 为什么当前结论还不够

- 当前两个实验都是单 vCPU，A 的 `notify_all` 没有其他 vCPU 需要唤醒，
  测不到“定向唤醒”这个核心差异。
- 双流 2ms 场景中 B 的 mean/median 更好（mean 约 12%），但 p99/max 波动大，
  且平均略差，3 轮样本不足以支撑强结论。
- emulated GIC 与 passthrough GIC 行为不同：passthrough 下 vCPU 连续运行不退出，
  队列只进不出，无法作为当前实验的默认链路。
- 已发现多个测量伪影（串口 trace 洪峰、ANSI 污染、guest 超时窗），说明当前数字
  仍然受环境噪声影响。

## 3. 下一步分析（按优先级）

### 3.1 双 vCPU guest 启动链路（当前最大阻塞项）

现状：

- 原生 QEMU 中 Zephyr SMP guest 能正常启动。
- AxVisor 中 A 可启动两个 vCPU，但 guest 卡在 SMP 初始化。
- B 的 vCPU1 卡在 startup wait。

需要分析：

- PSCI `CPU_ON` 的 vCPU 状态机与 startup ack 时序。
- GICR/SGI 在双 vCPU 下的初始化与转发路径。
- vCPU task 创建、affinity 注册、per-vCPU wait queue 发布的先后顺序。
- A 的 VM 级 wait queue 与 B 的 per-vCPU wait queue 在启动阶段的行为差异。

验收标准：

- 双 vCPU guest 在 A 和 B 都能完整启动。
- guest 两个线程各自跑通中断接收。
- 每一步卡住的位置有最小复现日志。

风险：

- 会触碰 AxVM 核心和 arm_vgic 路径，改动和回归风险明显大于当前实验。
- 需要独立分支/独立提交，不能混进现有实验改动。

### 3.2 跨 vCPU 定向唤醒实验

设计：

- vCPU0 跑持续负载（例如周期任务），vCPU1 上挂 IRQ consumer。
- host injector 只向 vCPU1 注入，测量 A/B 两条路径的端到端延迟。
- 同时观测 vCPU0 是否被无关唤醒打断。

指标：

- `enqueue → notify → ipi → guest_exit → drain → ISR` 全程延迟。
- A 的 `notify_all` 是否让 vCPU0 产生额外 guest exit。
- B 的定向 notify 是否只唤醒目标 vCPU。

验收标准：

- 同输入、同轮次下，B 的端到端延迟不差于 A。
- B 不唤醒无关 vCPU 这一点有 trace 证据。

### 3.3 长稳 + 高并发压力

设计：

- 至少 2 条注入流，周期降到 1ms，持续 10~30 分钟。
- 统计 `lost`、`overflow`、`inject_errors`、`p99.9`、`max` 和队列深度。

验收标准：

- B 全程无溢出、无丢失，尾延迟有界。
- A 如果出现长尾或队列增长，必须量化而不是只描述“理论上无界”。

### 3.4 统计可信度

- 每个分支至少 5~10 轮，合并样本后做 bootstrap CI 或非参数检验。
- 按 vector 分别统计，避免两条流互相污染。
- 记录 host 负载、QEMU 参数、日志路径、guest 镜像哈希。

### 3.5 passthrough 与 emulated 差异

需要回答：

- 为什么 passthrough 下 vCPU 在 guest 内连续运行、物理 IPI 不引起 guest exit？
- 这是 AxVisor 的 IPI 语义问题，还是当前 GIC 直通配置的固有限制？
- 如果要做 passthrough 下的定向唤醒，需要什么新的验证方法？

## 4. 已确认的实验坑（不要再踩）

- 新 Zephyr SDK 构建的 guest ELF 入口是 `0x400010b4`，不是旧的 `0x40001044`。
- passthrough 配置下 vCPU 不退出，软件 vIRQ 队列只进不出。
- injector 完成时 `dump_realtime_trace()` 会造成串口洪峰，使 QEMU 停顿约 1 秒，
  并导致同 vector 中断背靠背注入后被 GIC 合并。
- guest 等待窗需要放宽到 30s，否则平台停顿会让测量提前结束。
- 统计脚本必须先剥离 ANSI 转义，否则 CSV 首行会被串口交错污染。

## 5. 产出

- 每轮完整日志 + 统计 JSON，不只在文档里放汇总数字。
- 双 vCPU 启动失败的最小复现与根因说明。
- 最终结论只覆盖已验证范围：单 vCPU 下是“并发队列稳定性”，
  跨 vCPU 的“定向唤醒收益”必须等双 vCPU 链路打通后再下结论。
