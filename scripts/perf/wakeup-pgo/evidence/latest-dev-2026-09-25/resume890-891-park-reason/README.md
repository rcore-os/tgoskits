# park 路径与切换原因复核

本项在当前源码 `b292a098bb60ef604e7677c37cd95d926ff08200` 上只读核对，
复用 `resume888-889-park-phase/resume889/` 的两次 OrangePi-5-Plus-1
原始 `results.json`。没有源码修改、构建、租约或新板测。两次六轮组都因
OTHER 子轮 `not_parked=1` 而无效；这里只核对计数器口径，不参与无插桩
full20 或 PR 验收。

## 计数口径

`resume889` 的 `park_block` 和 `park_pick` 探针只位于
`TaskSystem::try_commit_park_in_rq()`，不能单凭 OTHER 的探针次数少于 FIFO
就推断剩余的阻塞切换进入 `commit_park_owner()` 的完整路径。
`check.py` 固定两份原件的 SHA256、样本与直方图完整性，并独立核对
`context_switches_blocked` 和其他切换原因。12 个子轮中，11 轮的
`park_block_count` 与 `context_switches_blocked` 完全相等；run2 第一轮为
42061/42060，只差一次，可能是非原子快照边界。每轮的 `park_pick_count`
与 `park_block_count` 完全相等。这足以否定“OTHER 约一半阻塞切换走了
未计时的完整 park 路径”的推断，但不是目标线程的逐事件关联证明。

| 启动 | 策略 | 有效子轮 | Blocked/attempt | Preempted/attempt |
| --- | --- | ---: | ---: | ---: |
| run1 | FIFO | 3/3 | 2.11500 | 0.01480 |
| run1 | OTHER | 2/3 | 1.08950 | 1.07765 |
| run2 | FIFO | 3/3 | 2.10320 | 0.00300 |
| run2 | OTHER | 1/3 | 1.08820 | 1.07695 |

表中比值是各有效子轮的全局切换计数除以 20000 次 attempt 后，在单次
启动内部取中位；计数包含后台事件，不能解释为目标线程每次 handoff
的排他切换次数。

OTHER/FIFO 的主要区别是切换原因组合：OTHER 有更多 `Preempted`，FIFO
主要是 `Blocked`。旧 `resume777` 已拆过 Fair 抢占的若干叶阶段；
在没有目标 handoff 窗口与这些切换逐次关联的证据前，不按 park 单事件
均值选择运行时代码改动，也不为“遗漏完整 park 路径”补做板测。

## PMU 边界和结论

直接在 `ax-task` 的 park 路径读取 PMU 会绕过 Starry
`perf::hw_owner::on_pmu()` 对寄存器访问、IRQ 串行及硬件槽位的管理。
现有整 case PMU 计数还包含后台与双向交接，不能定位 park 叶阶段；
本次没有增加内核 PMU 探针。后续若需要阶段 PMU，须先设计显式诊断
槽位及 owner 接口，不能把未经隔离的计数用于优化归因。

从本目录运行 `python3 check.py` 可复算表格。最近有效同频率五 crate
PGO 仍只有 **11/20** 项达到冻结 Linux RT p50 的 90%，最差 OTHER
同核 futex 为 **57.999%**。本项没有新增无插桩 full20 或性能收益，
没有保留生产运行时逻辑改动；PR #2477 继续保持 Draft。
