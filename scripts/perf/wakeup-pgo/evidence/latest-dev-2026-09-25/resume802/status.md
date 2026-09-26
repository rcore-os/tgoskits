# resume802：Fair 唤醒抢占路径复核

## 1. 当前证据

`resume801` 的同镜像同核 futex 诊断里，有效 OTHER 的
`direct_wake_preemptions` 为 1.0502/样本，FIFO 两轮为
0.0026、0.0144/样本；两策略的完整切换和 scheduler-rq 事务次数
只相差约 0.05–0.09/样本，未出现额外一次过渡。

当前 `CpuRunQueueState::wakeup_preempt_with_intent()` 先处理 pending
resched、idle 和等优先级 RT，再读取 current entity，执行 EEVDF
判据、Fair 队列最早可运行实体选择与 slice protection 取消。
`git diff --stat 5c06b7fbafae385abb770fee2785eff03dd11c2b..HEAD`
对该文件、`wake/activation.rs`、`algorithm/fair.rs` 和
`algorithm/fair_queue.rs` 均无输出，说明这些源码文件未变；
不保证生成机器码或周边调用成本相同。

旧 `resume589` 在 `5c06b7fbafae385abb770fee2785eff03dd11c2b`
的同型板、相同 benchmark 上用 qperf 拆分：OTHER 泛化判据调用
约 1.07175/样本，current entity clone 227.57 ns、EEVDF 判据
275.92 ns、最早实体选择 300.78 ns、slice 处理 169.32 ns，四段
合计 973.59 ns；外层 aggregate 1862.14 ns，含其他决策和计数
开销。它是旧源码插桩均值，不是当前原生可得收益上限。

## 2. 决定

当前有效 PGO F2 的 OTHER 同核 futex 16625 ns，90% 上限 9397 ns，
差 7228 ns；FIFO 同项 11667 ns，也比上限慢 2270 ns。
只优化 Fair 抢占判据不能完成两策略目标。旧分段和当前计数均未
发现可以保持抢占选择、slice protection 与发布顺序而删去的
多微秒事务，因此不做跳过/推迟判据或无依据的 entity clone 微优化。
本次没有源码、镜像或板测改动；下一候选须针对策略无关交接骨架
或有独立证据的多阶段机制，并经原生 full20 验收。
