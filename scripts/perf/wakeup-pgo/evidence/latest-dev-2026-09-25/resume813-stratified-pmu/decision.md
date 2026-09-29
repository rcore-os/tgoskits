# resume813：同二进制标记分层的正向 PMU 诊断

同一份静态 AArch64 诊断 benchmark（SHA256 `75939fe5a8ccd15476e1f15f967b11c19e5bb680df3da39be141ab391cb73b76`）在 OrangePi-5-Plus-1 上比较冻结 Linux RT 与当前源码 Starry 普通 A/PGO F。它结合 `resume810` 的 CPU0 正向窗口 PMU 读数和 `resume811` 的发送者用户态标记。Linux 镜像 SHA256 为 `aac6d3c5fa0c4fdf65f987af635f4cd55a06852b23046a4242a184acc2fd563b`；Starry A/F 镜像分别为 `750589429f5afe81b16775dde50050fedb936614b78d7bbde999a51856618473` / `e48394c7ef55008d0badf2cd6dc841fa8fa6593e069720304e2400634614fd6d`。Starry 基于 `69a33650763538692fafea27c869870ed0313642`，两侧均带相同的临时 exporter。Linux 在一次启动中每个策略/事件运行两轮；Starry 按 A1→F1→F2→A2 每个策略/事件运行两轮。共 60 次聚焦调用，全部 20000/20000 样本、零 `not_parked`、零错过截止时间；两个板卡会话均已释放。

逐轮分层中位数见 `analysis.json`。下表只列接收者先于发送者标记的 OTHER 分层；计数为每轮 20000 样本中该分层的中位数，PMU 数值近似减去相邻读数校准中位数，仍只供诊断。

| CPU0 内核态事件 | Linux RT OTHER | Starry A OTHER | Starry F OTHER |
| --- | ---: | ---: | ---: |
| 指令事件组的先于标记样本数 | 18896 (94.48%) | 16561.5 (82.81%) | 19873.5 (99.37%) |
| retired instructions，调整后 p50 | 4992 | 11104 | 8889 |
| CPU cycles，调整后 p50 | 8506.5 | 24753 | 15610 |
| L1I refills，调整后 p50 | 224.5 | 810 | 454 |

所有 FIFO 轮次的接收者先于标记样本数都是零。在**当前同一诊断二进制**下，Linux RT 和 Starry F 的 OTHER 都主要处于先于标记的分层，但 F 正向窗口的指令、周期和 L1I refill 仍明显更多。因此，在该插桩条件下，不能把整个 PMU 差距解释为先后标记样本比例不同；这仍不能隔离 Fair、futex、返回用户态或切换路径中哪些指令可安全删除。接收者观察到的用户态标记也不能区分 syscall 内切换与发送者返回用户态边界的切换。

Linux RT OTHER 的先于标记比例在 `resume811` 的纯标记二进制中仅为 63.40%，加入 PMU 读数后升至约 94–95%；Starry A 也由约 99% 降至约 81–83%。**插桩明显改变了路径比例**，不能把这些比例或调整后的 PMU 数值外推到冻结 full20。逐样本原始 PMU 差值没有导出；首次 PMU 读数与相邻校准读数可能处于不同缓存状态。Starry F 后于标记的分层每轮只有约 126–128 个样本，其 p50 不宜跨系统比较。

未修改生产源码或原始 benchmark，也没有保留优化。最近有效的原 benchmark、无插桩 full20 仍为 `resume788` F2：Linux RT 90% p50 门槛通过 11/20，最差 OTHER 同核 futex 为 16625 ns / 8458 ns，即 50.88%。三次有效候选启动、同源码 `<3%` 尾延迟回退门及生产构建复现仍未完成。后续源码候选须有具体且保持语义的成本假设，并用原始 benchmark 的同源码 A/B 数据验证；本次诊断不构成验收结果。
