# resume895: 当前源码同核 futex 事务级候选审计

## 1. 范围

在 `b292a098bb60ef604e7677c37cd95d926ff08200` 上只读复核同核 futex
的唤醒、rq 入队、抢占/阻塞切换和接收端返回。独立探索原件为
`resume895-scout/final.txt`；首选服务在读工具前限流，启动器按技能规则
切到备用服务。主代理核对了 `resume883` 原始 full20 行、futex wake batch、
`WakeIntent::Sync`、`schedule_out_owner_rq_owned`、`ThreadWaitState`、
`SwitchInCompletion` 和 benchmark 的同核等待顺序。本轮没有源码修改、
镜像构建、板卡测量或原生性能收益。

## 2. 当前证据

`resume883` 两次有效无插桩 full20 中，OTHER/FIFO 同核 futex p50
分别为 14583/11667 ns，差 2916 ns；OTHER/FIFO `sched_yield_handoff`
p50 分别为 7292/4083 ns，差 3209 ns。这说明策略相关路径有稳定的
数微秒差额，但**不能**把跨策略 p50 差当成 Fair 可删除成本的严格上界：
FIFO 与 Fair 的 class、唤醒和切换实现均不同，逐项 p50 也不是同一
样本的配对差值。OTHER 同核 futex 仍需至少降低 5186 ns 才达严格
90% 门槛，且 FIFO 同核、定时器、跨核 futex、OTHER yield 共九项未达。

当前 futex `wake_batch` 在 bucket 锁外唤醒；off-rq 激活使用 task lock
内一次 owner-rq 事务，普通 Fair 抢占使用 rq-only 事务。尚无证据证明
这些所有权不同的交易可以安全合并。`ThreadWakeHandle::wake_sync()`
并非现成的同核快路径：`default_sync_wakeup_preempts()` 与普通 Fair
抢占判定相同，`WakeIntent::Sync` 只影响目的 CPU 选择；冻结同核双方
已绑 CPU0，跨核接收端也已绑 CPU1。因此不把整个 futex wake batch
改用 Sync hint。

## 3. 决定

不为未量化的事务合并、Fair AVL 或 Sync hint 修改生产代码。下一次
诊断只跟踪目标接收线程的 `switch-in hook → futex wait finish → 下一次
CLOCK_MONOTONIC`，按等待 generation 配对，记录覆盖、重复切换、
未匹配、溢出及 OTHER/FIFO 分布。它只用于判定接收端是否存在值得
优化的公共多微秒成本，不替代无插桩 full20。设计须避免 `resume868`
被拒绝的无保护跨核覆盖槽位；若增加日志，使用对象自有 generation
状态或非覆盖式有限缓冲并显式统计失败。现有 `resume889` 的 park
分段计数包含背景事件且两组六轮组均无效，不能用其均值作收益预测。
