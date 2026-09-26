# resume903–904：同核 futex 调度遍数诊断

## 1. 诊断边界

源码基点为 `dev@714accd8f6` 加 AArch64 PGO 训练前置 `memset` 修复的
`b292a098bb60ef604e7677c37cd95d926ff08200`。`resume903-decision.md`
复算此前归档的 `resume854`、`resume889` 计数：OTHER yield 的完整 rq
事务没有额外的逐样本一遍；OTHER 同核 futex 的
`owner_rq_scheduler_transactions - context_switches` 约为 2.1 万。
当时漏查了旧 `resume793`、`resume820` 已给出的计时前 yield 解释；
本文件第 2.2 节更正其来源。旧原始日志及校验脚本已在同一证据目录的
`resume853-855-fair-yield`、`resume888-889-park-phase` 中。

### 1.1 探针与构建

`resume904` 只在 `qperf-metrics` 下添加抢占调度入口、遍数、切换、两类
未切换及重复遍数计数器，四文件临时差异见 `source.patch.gz`。普通运行时
调度语义未更改。原始差异压缩于 `source.patch.gz`，其解压后 SHA256 为
`844c9af98ca3fab3673138e6af33802741d8576e44d315d8bb10998e20d3d27a`。
`board.py` 是实际运行的采集脚本原件；若重跑，须先将压缩补丁解压为
`source.patch`，并恢复脚本引用的本地镜像和 benchmark 路径。
`build.toml` 关闭 cpufreq feature；诊断镜像 SHA256 为
`f6087021e4cd4a2c49e563d1a988099fc289ab1b7ea248cee6b47baa32b19bca`，
镜像因体积未入 Git。冻结 benchmark SHA256 为
`94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`。

### 1.2 板卡运行

Plus-1 上一次独立启动的六个定向子轮顺序为 FIFO、OTHER、OTHER、FIFO、
FIFO、OTHER，均为 20000/20000 样本，零 `not_parked` 与
`missed_deadlines`；PLL 频率检查、guest 退出及板卡释放见 `run1/results.json`
与原始串口。

## 2. 结果与决定

本次只核对调度事务的发生次数，不能从全局计数推断单个唤醒交易的
耗时。六轮的数据取自两次 `/proc` 快照差值，包含后台活动。

### 2.1 计数核对

六轮均满足 `preempt_schedule_passes` 等于切换、进入 rq 后未切换和
提前返回之和；下表将 rq 事务差额与 yield 入口、yield 切换及抢占帧内
未切换对齐：

| 轮次 | 策略 | rq 事务减切换 | yield 入口 | yield 切换 | 抢占帧内 rq 未切换 | 扣除后残差 |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 1 | FIFO | 21382 | 21025 | 5 | 355 | 7 |
| 2 | OTHER | 21180 | 21023 | 247 | 397 | 7 |
| 3 | OTHER | 21154 | 21023 | 262 | 386 | 7 |
| 4 | FIFO | 21386 | 21024 | 4 | 359 | 7 |
| 5 | FIFO | 21146 | 21023 | 3 | 119 | 7 |
| 6 | OTHER | 21159 | 21027 | 248 | 372 | 8 |

### 2.2 预唤醒来源

冻结基准的 `apps/starry/wakeup-latency-bench/handoff.c` 在同核每次
`run_sender()` 中先由 `wait_for_receiver_to_park()` 调用 `sched_yield()`，
随后才写入 `wake_timestamp_ns` 并执行 `futex_wake_one()`。该 yield 进入
`yield_current_in_scheduler_frame()` 的 `OwnerRqEntry::SchedulerFrame` 事务；
每轮 1000 次预热加 20000 次测量，合计 21000 次计时前 yield。
现有 `switch_scheduler_detail_owner_drain_count` 的索引 10 只在
`components/ax-task/src/sched/system/task_system/scheduling/yield_entry.rs`
的两条普通 yield 分支记录入口，每轮实测 21023–21027 次，包含少量
基准之外的 yield。
旧 `resume793` 已据此解释类似的 rq 事务差额，`resume820` 已确认
普通同核 futex 计时窗口内没有这笔额外事务。本轮数据是独立交叉核对，
不是首次发现该机制。

上表残差按 `rq事务减切换 - (yield入口 - yield切换) - 抢占帧内rq未切换`
复算。`yield切换` 是全局原因计数，残差仍含后台工作，因此这是聚合
一致性检查，不是逐次 yield 的一对一追踪。六轮残差仅 7–8 笔，
与计时前 yield 来源一致，不支持再按这约 2.1 万笔差额优化唤醒链。

### 2.3 判定范围

`python3 check.py` 从原始子轮日志、快照和哈希重算门禁及上述残差。
不能据此删去 park、尾部或唤醒中的任何事务。
`cargo xtask clippy --package ax-task`（6/6）、
`cargo xtask clippy --package starry-kernel`（72/72）、
`cargo xtask test --since origin/dev` 与 `cargo fmt` 在诊断源码上通过。

结论：仅归档诊断；临时计数器不进入 PR 的生产源码。qperf 插桩 p50
不能与无插桩候选或 Linux RT 比较。本次没有新的无插桩 full20，也没有
已证实的运行时优化。最近有效五 crate PGO 筛查仍为 11/20 项达 90%，
最差 OTHER 同核 futex 为 57.999%；第三次有效候选启动、同源码全部
p50/p99/p99.9 `<3%` 回退、生产复现及精确 head CI 门禁仍未完成。
