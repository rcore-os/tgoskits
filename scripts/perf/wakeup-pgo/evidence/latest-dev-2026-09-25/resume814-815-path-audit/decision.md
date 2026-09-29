# 最新源码的唤醒与交接路径审计

基于 `dev@05175ca38823b631a73777b0130226ddfa558439` 和 AArch64
`memset` 训练前置修复 `69a33650763538692fafea27c869870ed0313642`。
`resume814`、`resume815` 只读核对当前源码及已有诊断；没有修改内核、
构建镜像或重新上板。本文件只保留经主线复核的结论。

## 1. 同核唤醒路径

普通 futex 的 `ResolvedFutex::wake()` 通过 `ThreadWakeBatch::wake_all()`
进入 `TaskSystem::wake_thread_source()`，常见 off-rq 唤醒由
`activate_waking_thread_locked()` 激活。`wake_wait_claim_from_current_cpu()`
属于其他 wait-queue 路径，不能当作此场景的主要优化入口。同源码四 crate
训练 profile 的相应 common block 约为 607065 次，claim 路径最高约
201 次；这是 16 个训练场景的汇总，不是 full20 逐样本归因。

OTHER 的同核激活可发布 Lazy 抢占请求，发送者第一次 syscall 的
`prepare_user_return()` 会处理该请求。FIFO 通常保留当前任务，待随后
阻塞时交接。旧 `resume801` 的全局 context-switch 计数约为每样本
2.1--2.2 次，其中包含计时窗口外的准备和反向交接；`resume805`
核对的同核 futex 正向窗口内只有一次切换，不能推广到全部 20 项。

## 2. 否定的成本假设

`resume741` 的三个有效 OTHER 同核聚焦轮次中，每轮约 63700 次本地
runtime deadline 发布，仅 478--502 次实际 `Program` 硬件 timer，
其余约 63200 次为 `None`。该证据来自较旧但相关的源码，不能当作
当前镜像的逐样本计数；它足以否定“每次唤醒重复写硬件 timer”这一
立项依据。跳过首次发布也尚未证明长运行任务和 hrtick 语义等价。

AArch64 `prepare_switch_to()` 无条件保存和恢复 FP/SIMD 状态。
旧 qperf 的整个 `prepare_arch` 分段约为 0.3 us/切换，且包含探针；
这既不是 FP 独占成本，也不能填平九项各自约 2.27--14.51 us 的
90% 门槛缺口。改成 lazy FP 需要额外保证 CPU owner、首次使用陷阱、
fork、exec 与信号帧状态正确，当前没有可提交的等价实现。

`resume792` 的 timer 诊断中，IRQ-return 调度继续运行约为
0.001 次/样本；不能把 timer 和跨核 futex 的正向路径一概解释为
IRQ-return 调度。`resume810/813` 只有 PMU 窗口聚合计数，没有逐 PC
或调用栈样本；后者还证明 PMU 读取改变唤醒先后比例。现有数据不能
直接归因到某个函数，也不能当成原 benchmark 的可删耗时。

## 3. 当前判定

两轮审计没有找到兼顾正确性和数量级的共用运行时候选。
最近有效、冻结 benchmark、无插桩 full20 仍为 `resume788` F2：
11/20 项达到 Linux RT p50 的 90%，最差 OTHER 同核 futex 为
16625 ns 对 Linux RT 8458 ns，即 50.88%。这不是新增性能收益；
三次有效候选启动、同源码 `<3%` 回退和生产构建复现仍未完成。
