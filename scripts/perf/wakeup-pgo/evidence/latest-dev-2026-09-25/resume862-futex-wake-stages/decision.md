# resume862：同核 futex 命中唤醒分段

在 `b292a098bb60ef604e7677c37cd95d926ff08200`（最新
`dev@714accd8f6` 加既有 `memset` 训练修复）上，仅对 `qperf-metrics`
加入临时探针。无 PGO、无 cpufreq feature 的诊断镜像 SHA256 为
`938919005840f014a48aa830c072ac2f1174f476ca56651d1847faca6c772df2`；
冻结 benchmark SHA256 为
`94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`。
Plus-1 会话 `1fb7872a-a37a-4c7f-ae99-addfa719f4f6` 的五个进程按
OTHER/FIFO/OTHER/FIFO/OTHER 固定顺序执行，各 20000/20000 样本、
零 `not_parked` 与 `missed_deadlines`，均退出零；PLL/CPU 与镜像大小
由板卡脚本核对。会话已删除，GET 返回 HTTP 404。完整串口、逐轮原始
benchmark 日志和两种 debugfs 的前后快照在 `run1/`。

| 轮次 | 命中且入队 | 全局直接唤醒尝试/激活 | 锁前至批量前均值 | `wake_batch` 均值 | 插桩 p50 |
| --- | ---: | ---: | ---: | ---: | ---: |
| OTHER-1 | 21465 | 21822/21351 | 1657 ns | 9891 ns | 32375 ns |
| FIFO-1 | 42001 | 42331/42324 | 1386 ns | 5416 ns | 25083 ns |
| OTHER-2 | 21454 | 21811/21349 | 1676 ns | 9977 ns | 32375 ns |
| FIFO-2 | 42001 | 42331/42323 | 1360 ns | 5407 ns | 25375 ns |
| OTHER-3 | 21440 | 21800/21353 | 1652 ns | 9875 ns | 32375 ns |

OTHER 三轮锁前至批量前的探针均值拆分为：键/提示 205–208 ns、
bucket 锁取得 432–446 ns、锁内收集 814–819 ns、解锁 198–205 ns。
这些是**含读钟和探针开销的调用均值**，不是用户态 p50 的组成项或
可删除的原生成本。单个目标 gate 唤醒需要一次 direct wake，故即使把
全局 `attempts - activations`（OTHER 447–471）全归给 20000 次目标
唤醒，至少 97.645% 仍走 off-rq 激活。这是保守分支下界，不是精确
目标线程分类；它把 `resume861` 中 OTHER 两轮缺样本导致的未定状态
推进到有效证据。

OTHER 每轮约 21570 次 `context_switches_preempted`，FIFO 只有
316–320 次。源码审计确认 Fair `WakeeSelected` 通常只发布 Lazy 请求，
不会仅因 `PreemptScope` 退出就 arm 本地 immediate preempt pending；
其他来源的 pending 仍可能使 `wake_batch` 区间包含切换。本探针没有
逐次配对窗口内切换与目标唤醒，**无法判定 9.9/5.4 us 区间究竟包含
多少抢占、切换或发送者恢复**。两策略区间差不能当成 futex 批量处理
的独占成本，更不能据此删除 fence、锁或状态机。下一步优先判别
唤醒后 Lazy 请求的消费位置及 park/恢复交接；要声称优化收益，仍需
源代码级安全候选、无插桩同源码对照和完整 20 项尾延迟门禁。

构建首次被共享 target 中并存的 Cargo future-incompat 报告与旧
axbuild 历史拒绝；两份报告均为合法 JSON，较旧历史的 SHA256
`f8b2409683b48171911dd0da4d0dda95160e0b2c32668e303191bfb620325ec1`
已原样移入本目录，当前报告保留在 target。随后相同构建成功，
`core`/`memchr` 警告由 axbuild 的已知例外门禁接受。
`python3 check.py` 已从原始日志与快照复算全部五轮并通过；
`cargo xtask clippy --package starry-kernel` 的 72/72 组配置通过，
`cargo xtask test` 的 68/68 项白名单通过。临时探针源码的快照、
补丁和哈希已归档；提交 PR 前从工作树撤回。

最近的有效无插桩、同频率 full20 仍是旧源码 G1/G2 的 11/20，
最差 OTHER 同核 futex 58.00%；本轮没有新的性能收益或 90% 验收。
