# OpenRace 实时性 vIRQ A/B 进度总结

> 日期：2026-08-09
> 工作树：B = `/home/huhu/tgoskits-realtime`（openrace/realtime-virq-ab）；A = `/home/huhu/tgoskits-origin`（contest/openrace-2026）

## 一句话结论

**性能（延迟）优越性没有被验证出来；验证出来的是正确性修复和"定向唤醒不打扰无关 vCPU"的隔离性机制。** 端到端延迟由 A/B 共享路径（IPI、guest 进出、GIC LR）主导，notify 策略不进目标延迟关键路径；在 QEMU/TCG、N=2、空闲非目标 vCPU 的设置下，延迟差异结构上不存在。

## 已完成的提交（B）

| commit | 内容 |
| --- | --- |
| `32cfe9103` | Zephyr 周期延迟基线（实验起点） |
| `f298ee57b` | **B 核心改动**：vIRQ 走 per-vCPU 有界 dispatcher + 定向唤醒 |
| `a8b38e02a` | 双流 vIRQ A/B 压力场景 + 统计脚本 |
| `415cc4109` | 双 vCPU 启动握手改用 VM 级等待队列 |
| `20e83803a` | GICR_TYPER_LAST 只置在最后一个 redistributor |
| `f837d3ad0` | **双核启动根因**：HVC/SMC 返回地址重复 +4（已同步 A） |
| `284a22bec` | 双核启动修复与修复后 A/B 数据记录 |
| `041210b5f` | park 路径 4 个修复 + E1 计数器 |
| `00844b74c` | E1 实验资产（suspend guest、E1_MODE、--exact 统计） |
| `3ae0cd32f` | E1 结果与消融矩阵记录 |

A 侧镜像：`418fa8be3`（HVC 修复）、`18d81849c`（GICR_LAST）、`25a62d65c`（park 修复 + 条件 wait）、`8c853a921`（E1 资产）。

## 实验结果

### 标准双流（单 vCPU，2ms × 300，修复后各 3 轮）

| 指标 | A | B |
| --- | ---: | ---: |
| mean | 311µs | 301µs（低 3.2%） |
| p99 | 748µs | 749µs |
| 丢失/溢出 | 0 | 0 |

差异在噪声内（轮间波动 16-32%，检测 ~10µs 均值差约需 200 轮/臂），不显著。

### 双 vCPU 启动

- 修复前：必现 `FAR=0` data abort，guest 卡死。
- 修复后：A/B 都完整启动（`Secondary CPU core 1 is up` + READY），无 fault。

### E1 跨 vCPU 定向唤醒（suspend-idle guest，单注入器打 vCPU1）

| 变体 | 空闲 vCPU0 重入 guest | vcpu0 被 notify 唤醒(host) | 完成 |
| --- | ---: | ---: | ---: |
| A 旧（无条件 wait + notify_all） | 61 次 | ~124 | 否（中断搁浅） |
| A 条件 wait + notify_all | 1 | ~124 | 300/300 |
| B（条件 wait + 定向 notify） | 1 | **0** | 300/300 |

延迟（park 路径）：A≈545µs / B≈662µs mean，无稳定差异。

**结论**：条件 wait 是功能性关键；定向 notify 的可测收益是 host 侧唤醒事件 124→0（多 vCPU 下随 notify_all 按 O(N) 放大）。

## 已修复的真实 bug（5 个）

1. HVC/SMC ELR 被重复 +4（双核启动崩溃根因）。
2. `if let` 临时生命周期让 per-vCPU wait-queue map 锁被 park 永久持有 → notify 全死锁。
3. machine 锁内 notify 与 park 条件求值构成 ABBA。
4. GIC busy 时 drain 静默丢弃中断（改重入队，保持边沿计数）。
5. 注入器 `ax_std::thread::sleep` 被 AxVM 定时器轮盘"停驻"物理 CNTP 饿死（改轮盘 sleep）。

## 遗留问题

- **偶发 vGIC 竞态**（约 1/4 轮）：guest 在注入收尾时可能停摆（LR 停在 Active、AP1R 不为 0），A/B 共享路径，需 KVM 式 per-vCPU software pending + maintenance interrupt 模型。
- **定时器轮盘停驻 CNTP**是真实架构 bug，`sleep_until` 只是实验级绕过；正确修法是 per-CPU 定时器权威（CNTP = min(host, guest)）。
- **过载/积压实验未做**：这是唯一还有机会拉开数量级的实验（B 有界队列在 64 显式报错 vs A 无界队列静默增长），需周期 <100µs 或人为停顿 vCPU 制造真实积压。
- 环境限制：x86_64 跑 aarch64 QEMU 是纯 TCG，<50µs 的效应不可信；若目标是延迟优越性证据，需真实硬件或 KVM。

## 实验资产与日志

- 实验配置：`scripts/test/zephyr-soft-virq-suspend/`（suspend-idle SMP guest、consumer 先挂起再 pin 到 CPU1、timeslice 关闭）；`E1_MODE` 注入器开关在 `os/axvisor/src/realtime_probe.rs`；`--exact` 统计在 `scripts/test/virq_latency_stats.py`。
- 标准双流日志：`/tmp/ab-{A,B}-standard-{1,2,3}.log`
- E1 日志：`/tmp/e1-B-r{1,3,4}.log`、`/tmp/e1-A-r{1,3,4}.log`（干净轮）；`/tmp/e1-A-base{,2}.log`（旧 A 失败示例）
- 双核启动日志：`/tmp/ab-{A,B}-dual-boot.log`

## 下一步候选（按优先级）

1. 修偶发 vGIC 竞态（KVM 式 pending 模型），稳定实验基线。
2. 过载/积压实验（有界 vs 无界行为分叉）。
3. 真实硬件或 KVM 上复测延迟（如果目标是延迟证据）。
