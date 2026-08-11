# Task-3 AI 控制闭环设计文档

> 分支：`openrace/task3-ai-control`（基于 Task-2 基线 `01f77307e`）
> 状态：M0–M5 完成，M6 收口中（2026-08-12）
> 关联：`task3-ai-control-todo.md`（TODO）、`task3-ai-session-notes.md`（工作记录）

## 1. 目标与边界

Task-3 的目标是在既有 Task-2 双 Guest UDP/IP 链路上，实现一个**可复现、可量化、
可录制演示**的 AI 控制闭环：

```text
Zephyr 虚拟对象/传感器
        │ T2N1 STATUS
        ▼
Linux Guest 时序模型推理
        │ T2N1 CONTROL
        ▼
Zephyr 应用控制并更新虚拟对象
        │ T2N1 STATUS
        └────────── 回到 Linux
```

本方案是 QEMU 上的**软件在环（SIL）验证**。明确不声称：

- 已在真实板卡上运行；
- QEMU 测得的延迟是硬实时保证；
- 冻结场景之外的泛化能力。

## 2. 系统架构

### 2.1 平台

| 项 | 值 |
|---|---|
| QEMU | 10.2.1，AArch64，`virt,virtualization=on,gic-version=3` |
| 虚拟机监控 | AxVisor，VirtIO-MMIO 直通 |
| Linux Guest | 控制器 + 模型推理（initramfs 静态加载） |
| Zephyr Guest | 虚拟对象 + 执行器（board `qemu_cortex_a53`，Zephyr 4.4.99） |
| 数据链路 | 两个独立 VirtIO-MMIO 端点，QEMU socket 点对点 |
| 端点 | Linux `10.0.42.15:4242` ↔ Zephyr `10.0.42.2:4242` |

### 2.2 协议与消息

沿用 Task-2 的 T2N1 可靠消息（CONTROL / STATUS / ACK / HEARTBEAT / ERROR）。
Task-3 采用请求-响应模式：一条 CONTROL 对应一条 STATUS，控制周期 100 ms
（5–10 Hz），不引入高频遥测流，因此不修改协议状态机。

## 3. 冻结场景与参数

正式对比前冻结以下参数（固定 seed，看完结果不修改场景）：

```text
state 范围     0..1000
output 范围    0..1000
base_loss      15
nonlinear_loss 120
response       0.35
目标轨迹       0-5s: 300, 5-15s: 800, 15-25s: 500
扰动轨迹       8s: +150 负载, 17s: -150 负载
控制周期       5-10 Hz（请求-响应）
baseline       Kp=2 纯 P 控制器（M0 冻结，非故意调差）
```

对象更新（Zephyr 整数语义，C 除法截断向零，Python 训练端逐点复刻）：

```text
loss(state) = base_loss + trunc(nonlinear_loss * state² / 1_000_000)
state_next = clamp(state + trunc(response * (output - state)) - loss + disturbance, 0, 1000)
```

## 4. 模型设计与训练

### 4.1 模型结构

1D 时序 CNN，纯 Rust `no_std` 前向推理，权重 `include_bytes!` 嵌入：

```text
输入：64 个历史采样点 × 4 特征 [state, target, error, prev_output]
Conv1D 4→32 k=5 ReLU
Conv1D 32→64 k=5 ReLU
Global Average Pooling
Dense 64→32 ReLU
Dense 32→1
```

- 参数量：13,089；MACs 估算：~0.7M/推理；
- 权重：`components/task3-model/model/weights.bin`，SHA-256 记录于
  `model/model.json`（含 `trainer_sha256`、归一化参数）；
- Guest 内推理：均值 11.3 ms、p95 14.6 ms（QEMU TCG 环境）。

### 4.2 训练方法：残差 + DAgger

- **残差学习**：模型只学习冻结 P 控制器缺失的损耗/扰动补偿项，P 项保证闭环
  稳定；纯模仿在非线性 plant 上会放大误差（固定场景 RMSE 189 → 33）。
- **teacher**：gain=0.5 逆控制跟踪策略（gain=1.0 一步到位是 bang-bang，闭环振荡发散）。
- **DAgger 真闭环**：逐 100 ms `plant.step` rollout，模型输出真实反馈进状态；
  6 轮、每轮 120 个随机 episode、teacher 打标、3 倍重采样。
- **数据**：400 个随机 episode（随机目标阶梯/扰动/初值），94,800 样本；
  冻结的固定测试场景不进入训练集。
- **特征契约**：`scripts/task3/features.py::build_window` 为唯一实现，
  数据集/DAgger/评估/Rust guest 全部复用；Rust `build_features` 镜像并由
  golden-window 测试跨语言锁定（误差 < 1e-12）。
- **golden 测试**：卷积对照 torch f64 参考（1e-9），输入含非对称模式（斜坡）
  以捕获镜像/翻转类 bug。

## 5. 控制器与 baseline

- baseline：`output = clamp(Kp * error + bias, 0, 1000)`，Kp=2、bias=0；
- AI 模式：`output = clamp(P 输出 + model(features) * 1000, 0, 1000)`，Zephyr
  侧仍做最终限幅与 Safe 兜底；
- 请求-响应循环：收到 STATUS 后才发下一条 CONTROL（`request_in_flight` 跟踪），
  模型输出进入 CONTROL.value，通过日志与 request ID 关联。

## 6. 故障注入与恢复

### 6.1 为什么不用 `set_link`

实测 QEMU `set_link` 在本组合（virtio-mmio 直通 AxVisor + 此 Linux guest）中
**不切断数据路径**——连开机前置 down 也无效（双方仍互收心跳并恢复），
`netdev_del` 能断但不能恢复（listen 端口不释放、NIC 仍挂旧 peer）。

### 6.2 P3 代理黑障（M5 断链方案）

`ack_drop_proxy.py` 新增 `--blackout-start-ms/--blackout-duration-ms`：在真实
guest 链路中间按时间窗口丢弃**全部帧**（双向），窗口结束自动恢复转发。

```text
黑障 25s→35s（丢 102 帧）→ 双方 RetryExhausted/HeartbeatTimeout 进 Safe
→ 黑障结束 → heartbeat 恢复 → 可靠流重同步 → 控制环续跑
```

### 6.3 恢复路径的根因修复

1. **in-flight 标记**：进 Safe 时 `request_in_flight` 未清，`TASK2_RECOVERED`
   补发被早退吞掉；现于 RetryExhausted/HeartbeatTimeout 时复位。
2. **可靠流重同步**：Safe→Active 时把 `next_tx/next_rx/pending` 重置为
   FIRST，双方从序号 1 重新同步；否则丢失的 STATUS 会让序号永久分歧并
   `out_of_order` 死循环（Rust 协议 + Zephyr C 双端修复，含回归测试）。
3. **ACK 竞争**：STATUS 可能先于其 CONTROL 的 ACK 被处理，`queue_reliable`
   报 `ReliableFramePending`；改为延迟到 Acknowledged 事件续发
   （`TASK2_CONTROL_DEFERRED`），非致命。
4. **发送稳健性**：非阻塞 UDP 发送遇 WouldBlock 短暂重试（`send_datagram`）。

## 7. 实验证据

### 7.1 M4 指标（QEMU 双 Guest，3 AI + 3 baseline，各 ~39 s）

| 指标 | AI (n=3) | baseline (n=3) |
|---|---|---|
| 整体 RMSE | 29.2 / 29.3 / 29.3 | 190.6 / 191.3 / 190.7 |
| t300 段 RMSE（0-5s） | 49.1 | 102.9 |
| t800 段 RMSE（5-15s） | 40.8 | 217.0-219.3 |
| t500 段 RMSE（15-25s） | 21.6 | 192.2 |
| t500 稳态误差 | ~2（498 vs 500） | ~192（308 vs 500） |
| Guest 推理耗时 | 均值 11.3 ms，p95 14.6 ms | - |
| CONTROL→STATUS RTT | ~104 ms | ~91 ms |

原始数据：`results/task3/run-{1..6}.csv`、`summary.csv`、`comparison.png`。

### 7.2 M5 故障闭环（fault-final3 / fault-final4，2 次复现）

| 事件 | 观测 |
|---|---|
| 黑障窗口 | 25s→35s，代理丢弃 102 帧（双向） |
| Safe 进入 | VM1 RetryExhausted / HeartbeatTimeout，VM2 对应进入 Safe |
| 恢复 | `TASK2_RECOVERED` 后可靠流从序号 1 重同步 |
| 续跑 | final4 恢复后 82 个 STATUS 周期，rtt ~74–109 ms，0 协议错误 |

证据：`results/task3/fault/`（guest/proxy 日志 + 双端 pcap + SHA-256）。

## 8. 复现命令

```bash
# 训练（固定 seed）
python3 scripts/task3/generate_dataset.py --train-episodes 400 --val-episodes 40
python3 scripts/task3/train_model.py --epochs 60
python3 scripts/task3/dagger_train.py --iterations 6 --epochs 25 --closed-loop-weight 3
python3 scripts/task3/export_golden.py && cargo test -p task3-model

# 构建 guest（baseline / AI）
TASK3_CONTROL_LOOP=1 bash scripts/test/net-dual-guest/build-linux-task2.sh
TASK3_CONTROL_LOOP=1 TASK3_AI=1 bash scripts/test/net-dual-guest/build-linux-task2.sh
bash scripts/test/net-dual-guest/build-linux-initramfs.sh

# Zephyr（含恢复重同步修复；需 Zephyr SDK 与源码）
bash scripts/test/net-dual-guest/build-zephyr-task2.sh

# 实验（AI/baseline 对比 + M5 故障闭环）
bash scripts/task3/run-task3-experiment.sh ai-runX ai
bash scripts/task3/run-task3-experiment.sh baseline-runX baseline
bash scripts/task3/run-task3-fault.sh fault-runX

# 指标
python3 scripts/test/net-dual-guest/task3_metrics.py <logs...> \
  --out-dir results/task3 --label run --modes ai,ai,ai,baseline,baseline,baseline \
  --plot results/task3/comparison.png
```

## 9. 已知边界与诚实声明

- 所有结论基于 QEMU SIL 验证，不声称真实板卡或硬实时保证；
- 冻结场景外推有限：模型只在随机化训练分布上验证；
- t800 目标（800）超过 plant 可持续上限（~760，含扰动 ~880），AI 以
  ~790-810 逼近而非达到；
- baseline 为 M0 冻结的 Kp=2 纯 P 控制器，非故意调差；
- "3+3 组"是同冻结场景的重放（可复现性强，非统计显著性样本）；
- 断链通过 P3 代理黑障实现，非 QEMU `set_link`；`set_link` 无效已作为
  环境边界记录。

## 10. 完成定义对照

| 完成定义 | 状态 |
|---|---|
| QEMU 双 Guest 可复现启动 | ✅ |
| Zephyr 非恒定虚拟对象 | ✅ |
| Linux Guest 真实 1D CNN 推理 | ✅ |
| 推理输出进入 T2N1 CONTROL | ✅ |
| Zephyr 应用控制并回传 STATUS | ✅ |
| baseline/AI 各 ≥100 控制周期 | ✅ |
| RMSE、调节时间、延迟数据 | ✅ |
| 一次 link down/up 安全行为证据 | ✅（M5，2 次复现） |
| 原始日志、CSV、模型哈希、复现命令 | ✅（`results/task3/` + 本文档） |
| 可展示完整闭环的演示视频 | 暂缓（用户明确不做） |
| Task-2 原有测试/证据不回归 | ✅（协议 20 测试 + Python 20 测试通过） |
