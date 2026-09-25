# 发送侧真实等待者唤醒诊断

本目录记录 `dev@05175ca388` 加 PR 中早期 `memset` 修复后的只读审计，
以及 OrangePi-5-Plus-2 上一次 Starry/Linux RT 同板实验。Starry 源码为
`69a33650763538692fafea27c869870ed0313642`，镜像未因这些审计改变。
实验是聚焦诊断，不是冻结 benchmark 的 full20 验收，也没有新增运行时优化。

## 1. 诊断结果

发送者和接收者都固定于 CPU0，分别使用 FIFO80 和 FIFO1。接收者先完成
调度策略设置，再发布就绪；发送者每轮等其发布 armed，调用外等待
50 us，预热 1000 次并测量 20000 次。发送者优先级更高，因此接收者不会在定时的唤醒系统调用中
凭普通 FIFO 优先级抢占它。同一静态 AArch64 二进制分别在两侧运行，
每次命中真实等待者的 `FUTEX_WAKE_PRIVATE` 返回 1，空唤醒返回 0。

### 1.1 同板结果

以下数值直接来自 `resume847-lower-priority-wake/` 的原始串口和结果文件，
单位为 ns。每行包含空唤醒和真实等待者唤醒各 20000 个样本；两侧均正常
退出，板卡会话已释放。Linux RT 使用冻结内核镜像；完整镜像、DTB、
benchmark 和一次性 initramfs SHA256 见该目录的 `decision.md`。

| 系统 | 轮次 | 空唤醒 p50 | 真实等待者 p50 | 两个 p50 之差 | 空唤醒 p99 | 真实等待者 p99 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Starry G | 1 | 1750 | 7000 | 5250 | 2042 | 7583 |
| Starry G | 2 | 1750 | 7000 | 5250 | 2042 | 7292 |
| Linux RT | 1 | 2916 | 4375 | 1459 | 3500 | 4667 |
| Linux RT | 2 | 2916 | 4084 | 1168 | 3500 | 4667 |

两个边缘 p50 之差不是逐次配对差值的中位数；不能从表中推算某个内核
函数的耗时。尽管 Starry 的空唤醒快 1166 ns，命中等待者时却慢
2625–2916 ns。该判别将同核差距的相当一部分定位到**发送侧真实等待者唤醒**，
其范围仍包含 futex bucket 收集、等待状态更新、`ThreadWakeBatch`、
`wake_thread_source()`、rq 激活与发布，不能单独归因于
`activate_waking_thread_locked()`。频率由独立实验测得两侧约 816 MHz，
并未在这些调用的测量窗口同步采样。

### 1.2 审计边界

`resume843-memset-native-audit/` 核对当前 G ELF 的 `memset` 直接调用点，
未找到同核 futex 快路径上的大块清零；早期启动所需字节循环不是下一项
可证明的多微秒优化。`resume844-pmu-sample-feasibility/` 核对 PMUv3
采样 IP；已有样本集中在 IRQ 解屏蔽后，不能把采样 PC 误当耗时指令。

`audit845-idle-transition/` 核对 OTHER timer 的 idle-to-worker 路径，
未找到重复生效的物理比较器提交；`audit846-common-boundary/` 比较
yield/futex 缺口并审计公共切换段，未找到可删除的事务。各目录包含主代理
复核后的 `decision.md` 和只读子审计的 `subagent/final.txt`；旧探针、
不同路径的 p50 差值均未用作因果证明。

`audit848-sender-wake/` 追踪 futex bucket PI mutex、接收者任务锁和 rq
锁，在不发生立即抢占时仍需执行的激活、入队和 rq 提交工作。只读审计
未找到已证明可删除的多微秒操作。它提出 bitset 未命中/命中的下一步
诊断，但命中臂还含 waiter 移除、状态转换和 wake batch，不能直接称作
调度器耗时；主代理在 `decision.md` 中修正了原报告的判读边界。

## 2. 证据复核

所有原始板测材料位于 `resume847-lower-priority-wake/`：静态测试二进制、
源码、构建和板卡脚本、一次性 Linux initramfs、会话结果及完整串口。
Starry 和 Linux 内核镜像未复制到 PR，但精确 SHA256 已记录，避免将
其他镜像误认为该次实验。`wake_cost.c` 明确检查优先级设置、唤醒返回值、
样本数和正常结束。

### 2.1 离线校验

在本目录运行 `sha256sum -c SHA256SUMS` 核对归档文件，再运行
`python3 check.py` 从原始日志复核结果 JSON、同一 benchmark SHA256、
20000 样本、退出状态和板卡释放状态。脚本不连接板卡，不生成新性能结果。

### 2.2 验收状态

最近有效、无插桩且频率配置可比的 G1/G2 full20 仍只有 **11/20** 项
达到冻结 Linux RT p50 的 90%，最差 OTHER 同核 futex 为 **58.00%**。
该实验没有重新运行 full20、没有新生产构建或三次有效候选启动，
也没有完成同源码普通 release 的所有 p50/p99/p99.9 `<3%` 回退门。
PR 保持 Draft，不请求合入。
