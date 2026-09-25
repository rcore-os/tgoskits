# resume903–904：同核 futex 调度遍数诊断

## 1. 诊断边界

源码基点为 `dev@714accd8f6` 加 AArch64 PGO 训练前置 `memset` 修复的
`b292a098bb60ef604e7677c37cd95d926ff08200`。`resume903-decision.md`
复算此前归档的 `resume854`、`resume889` 计数：OTHER yield 的完整 rq
事务没有额外的逐样本一遍；OTHER 同核 futex 则有约 2.1 万笔尚未归因的
`owner_rq_scheduler_transactions - context_switches`。旧原始日志及校验脚本
已在同一证据目录的 `resume853-855-fair-yield`、`resume888-889-park-phase` 中。

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
耗时。OTHER 三轮的数据取自两次 `/proc` 快照差值，包含后台活动。

### 2.1 计数核对

OTHER 三轮均满足 `preempt_schedule_passes` 等于切换、进入 rq 后未切换
和提前返回之和；下表列出与 rq 事务差额相关的类别：

| 轮次 | rq 事务减切换 | 抢占帧内 rq 未切换 | 抢占帧前提前返回 | 抢占帧重复遍数 |
| --- | ---: | ---: | ---: | ---: |
| 2 | 21180 | 397 | 577 | 8 |
| 3 | 21154 | 386 | 558 | 8 |
| 6 | 21159 | 372 | 540 | 7 |

### 2.2 判定范围

因此约 2.1 万笔剩余 rq 事务**不是**额外的 2.1 万次无切换抢占帧，
但其来源尚未由这组全局计数定位。不能据此删去 park、尾部或唤醒中的
任何事务。`python3 check.py` 从原始子轮日志、快照和哈希重算门禁及计数。
`cargo xtask clippy --package ax-task`（6/6）、
`cargo xtask clippy --package starry-kernel`（72/72）、
`cargo xtask test --since origin/dev` 与 `cargo fmt` 在诊断源码上通过。

结论：仅归档诊断；临时计数器不进入 PR 的生产源码。qperf 插桩 p50
不能与无插桩候选或 Linux RT 比较。本次没有新的无插桩 full20，也没有
已证实的运行时优化。最近有效五 crate PGO 筛查仍为 11/20 项达 90%，
最差 OTHER 同核 futex 为 57.999%；第三次有效候选启动、同源码全部
p50/p99/p99.9 `<3%` 回退、生产复现及精确 head CI 门禁仍未完成。
