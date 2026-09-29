# resume812：修正 Fair 唤醒至切换路径审计

此步没有修改源码或运行板测。先前只读探索声称 Starry 通常等到发送者第二次 `FUTEX_WAIT` 才切换，但当前 `UserExecutionContext::enter()` → `prepare_user_return()` → `needs_reschedule()` 路径及 `resume811` 的同二进制板测与该说法矛盾；探索结果已撤回这一判断。Starry 在首次 syscall 返回用户态前消费 Lazy 请求，但用户态标记不能区分 syscall 内切换与返回边界切换。

`resume810` 中 Starry F 与 Linux RT 的 OTHER 减 FIFO 差值之差约为 2158 指令和 129.5 次 L1I refill；该差值混合了 Fair 工作与发送者切换位置，**不是可删除的 Fair 成本**。历史 `exp40`（Lazy→Immediate）、`exp120`（Fair V 刷新）、`resume717/732`（交接融合/显式 All 点）和 `resume806/807`（候选复用及独立 futex/切换微成本）均不足以支持直接修改源码。此阶段没有找到兼顾语义等价且有证据表明能减少至少约 1000 指令的候选。

另一个范围修正：当前相关源码的 `resume801` 有效 OTHER 聚焦轮次在 20000 样本中记录约 21000 次 `direct_wake_enqueues`，但只有约 960 次 `fair_delayed_begin_count`。这提示成功唤醒的常见路径可能是 off-rq 激活，而不是 on-rq 延迟重激活；计数器为全局值，包含启动和反向活动，不能精确归因到每个样本。因此后续不应只针对 `wake_on_rq_locked()` 下探针。

结论：无生产代码改动。后续使用同一诊断二进制结合 `resume810` 的 CPU0 正向窗口 PMU 与 `resume811` 的发送者用户态标记，按先后顺序分层统计 Linux RT 和 Starry A/F，再选择有明确成本假设的源码候选。
