# Task 1 交卷状态摘要

> 最后更新：2026-08-02  
> 矩阵报告：[`matrix-20260710T102947Z.md`](./matrix-20260710T102947Z.md)  
> stress baseline vs opt（长稳 180k，**最佳证据**）：[`stress-baseline-vs-opt-long-20260801T104415Z.md`](./stress-baseline-vs-opt-long-20260801T104415Z.md)

## 已完成（可复现）

| 项 | 证据 |
|---|---|
| 混合分区拓扑 Linux+RT | `linux-smp2.toml` + `arceos-rt-smp1.toml`，`mixed-rt-stress-round1` PASS |
| 调度/抢占改造 | `sched-cfs`、`vcpu_priorities`、中断 wake、`task1-rt-opt` |
| 定时器/GIC | `passthrough_timer` 独立配置项；baseline 用 emulated timer |
| 裸机基线 | `run-rt-baseline.sh` → `mode=bare` |
| Guest idle 短测 | `arceos-rt-latency-guest` → `RT_LATENCY_PASS` |
| 30min stress 长稳（baseline vs opt） | 180k 样本，均 `RT_LATENCY_PASS`，**1ms P99 改善 22.1%** |
| 多档强基线短测 | strong / contended / host-share 均已跑通并出报告 |

一键复现：

```bash
# 最佳现有证据（~70min）
CARGO_TARGET_DIR=$PWD/target ./scripts/task1/run-stress-baseline-vs-opt-long.sh

# 强基线变体短测（~7min each）
CARGO_TARGET_DIR=$PWD/target ./scripts/task1/run-stress-strong-baseline-vs-opt-short.sh
CARGO_TARGET_DIR=$PWD/target ./scripts/task1/run-stress-contended-baseline-vs-opt-short.sh
CARGO_TARGET_DIR=$PWD/target ./scripts/task1/run-stress-host-share-baseline-vs-opt-short.sh
```

## 关键数据摘录

### Guest + stress 长稳（180k，emulated-timer baseline vs full opt）— **最佳**

| period_ms | baseline P99 | optimized P99 | 改善 |
|---:|---:|---:|---:|
| 1 | 288544 | 224704 | **22.1%** |
| 10 | 305648 | 294208 | 3.7% |

| period_ms | baseline P999 | optimized P999 | 改善 |
|---:|---:|---:|---:|
| 1 | 428288 | 361488 | 15.6% |
| 10 | 437376 | 419280 | 4.1% |

### 强基线变体短测（18000 样本，均未达 ≥50%）

| 基线 profile | 1ms P99 改善 | 报告 |
|---|---:|---|
| emulated timer 独占 pCPU3（短测） | 7.4% | `stress-baseline-vs-opt-20260731T150033Z.md` |
| pCPU2 共核 + 8×stress + nice=19 | 8.7% | `stress-strong-baseline-vs-opt-20260802T021415Z.md` |
| pCPU3 共核 + 8×stress + slow-vtimer | **-4.5%**（噪声） | `stress-mixed-rt-stress-baseline-contended-short-vs-...-20260802T023342Z.md` |
| pCPU0 宿主共核 + 8×stress | 1.2% | `stress-mixed-rt-stress-baseline-host-share-short-vs-...-20260802T024249Z.md` |

### pre-opt 长稳（仅去 vcpu_priorities，timer 仍直访）

| period_ms | pre-opt P99 | post-opt P99 | 改善 |
|---:|---:|---:|---:|
| 1 | 263312 | 258320 | 1.9% |

## 结论（当前轮次）

1. **QEMU 上 honest 上限约 22.1%**（长稳 emulated timer + 无 priorities vs 全量优化）；短测与各类「更强基线」均未突破 50%。
2. **主要收益来源**：`passthrough_timer` 直访 + `task1-rt-opt` fast-wake；单独去掉 priorities 仅 ~1.9%。
3. **`task1-baseline-slow-vtimer` 对现有 benchmark 无效**：guest 使用 `thread::sleep`（ArceOS 调度器），不走 CNTP_TVAL 模拟路径；日志中无 `Write to emulator register`。
4. **共核/宿主共核在 QEMU 下扰动有限**：stress-ng 式 busy-loop 对独占 pCPU3 的 RT 客户机 P99 抬升不明显。
5. **实板复测基础设施已就绪**：RT guest 构建/部署脚本 + 3 个 board test case（smoke + stress baseline/opt）；待物理板跑数。

## 仍缺项（交卷前建议补）

| 优先级 | 项 | 说明 |
|---|---|---|
| 高 | 赛题 ≥50% 改善证据 | QEMU 长稳 **22.1%** 为当前最佳；实板 RK3588 上 IRQ/定时器/共核扰动可能更大 |
| 中 | 实板 mixed stress | 镜像 + test-suit 已就绪；`run-board-stress-baseline-vs-opt.sh` |
| 中 | IRQ 响应延迟 | `irq.rs` 路径未落地 |
| 低 | RT-Thread 混合长稳 | smoke 已有，非 ArceOS rt-latency 可比 |

## 赛题评分对照（自评）

| 评分项 | 自评 | 说明 |
|---|---|---|
| 多核 Linux 客户机 | ✅ | linux-smp2 smoke |
| 实质改造 | ✅ | 调度/定时器/GIC/中断 |
| 改造前后数据 | 🚧 | 长稳 22.1%，未达 50% |
| 空载+stress 对比 | ✅ | idle + 多档 stress 对比报告 |
| 裸机基线可复现 | ✅ | bare + Zephyr + RT-Thread |
