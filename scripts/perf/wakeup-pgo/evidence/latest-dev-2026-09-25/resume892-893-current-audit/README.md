# 当前源码交接与定时器审计

## 1. 审计边界

本记录把 `resume892`、`resume893` 的只读结论同步到 PR。审计源码为
`b292a098bb60ef604e7677c37cd95d926ff08200`（`dev@714accd8f636c540b2c3554b0b1e5cb885be42a4`
加 AArch64 PGO `memset` 修复）；Linux RT 对照源码为
`8cd9520d35a6c38db6567e97dd93b1f11f185dc6`。两项审计都没有改动生产源码、
构建镜像或新增板卡测量，不能用于宣称性能收益。

### 1.1 当前验收状态

最近有效的同频、无插桩五 crate PGO `resume883` 有两次独立 full20，均为
20/20 项、380000/380000 样本、零 `not_parked` 和 `missed_deadlines`。
冻结 Linux RT p50 的 90% 门槛只过 **11/20** 项。最差 OTHER
`thread_futex_same_cpu` 候选 p50 中位数为 14583 ns，Linux RT 为 8458 ns，
比例 **57.999%**；严格整数门槛要求候选不超过 9397 ns，至少还需降低
5186 ns。三次有效候选启动、同源码普通 release 的全项 p50/p99/p99.9
回退 `<3%` 和生产构建复现仍未完成。
候选镜像 SHA256 为
`e2b96fde90132a7c678a511dbd4d675b40c3743902b2e6ecfd6b1744d4178969`；
冻结 benchmark SHA256 为
`94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`。

### 1.2 Fair 抢占交接

`components/ax-task/src/sched/system/task_system/switch/schedule_out.rs` 的
`OwnerRqScheduleOut::Unlinked` 分支，在 owner-rq 事务内调用
`put_prev_unlinked_current()`；普通 Fair 抢占后与选取后继共享这次事务。
迁移和 Deadline 等特殊情况另有路径，不能将其额外锁成本分摊到普通 Fair
同核交接。`resume891` 对 12 个带探针子轮交叉核对，11 轮 rq-only
`park_block_count` 与 `context_switches_blocked` 相等，另一轮差一次；
“约一半阻塞切换走未计时完整 park 路径”没有证据支撑。计数包含后台事件，
两组六轮组因 `not_parked=1` 均无效，不能归因到目标 handoff。

既有 `exp120` 虚拟时间删减、`resume870` Fair lag 算术快路径和 `resume884`
Fair `Lazy` 改 `Immediate` 均未留下通过 full20 与尾延迟门的改动。
本次没有发现可证明等价、足以填补 5186 ns 缺口的单个 Fair 冗余操作；
后续需按目标线程逐事件关联接收端切换、park、futex wait 与 syscall 返回。

## 2. 定时器与训练覆盖

`components/ax-task/src/runtime/service/ktimer.rs` 为每核 `ktimers/%u`
创建绑定本核的 FIFO 优先级 1 工作线程。冻结 Linux 7.1 PREEMPT_RT
的 `kernel/softirq.c::ktimerd_setup()` 调用
`kernel/sched/syscalls.c::sched_set_fifo_low()`，同为 FIFO 优先级 1；
“Starry worker 是 Fair 而 Linux worker 是 FIFO”并不是可用的优化前提。
相同策略不代表执行时延相同。

### 2.1 实际训练输入

归档的 `workload.sh` 与 `resume883` 实际训练输入逐字节相同：先运行一次
`--policy all --case all`，再重复七次
`--policy other --case thread_futex_same_cpu`。最差项并非完全缺席训练；
此事实不证明训练权重或代码布局已最优，也没有形成新的 profile。
脚本 SHA256 为
`4ed6f7269f0ac6bde5754819e22780bef3b814b808693f722f050c5818f47db2`。

### 2.2 后续约束

若继续优化定时器，需要先测量 worker 的实际执行成本；若继续优化 futex，
需要保持 owner-rq 与 park 交接的所有权和唤醒正确性，并以完整、有效、
无插桩 full20 复核。只读审计不改变 `resume883` 的 11/20 和 57.999%
结论，PR 保持 Draft，不请求合入。
