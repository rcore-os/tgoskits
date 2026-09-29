# resume811：同核 futex 唤醒顺序诊断

同一份静态 AArch64 诊断 benchmark（SHA256 `3c72c5ead9c0aad2e0bd271f244807f16cad206d125141199fa52deab86e6189`）在 OrangePi-5-Plus-1 上运行，PLL 设置核对为约 816 MHz。它复制冻结 full20 的代码，并在发送者 `futex_wake_one()` 返回后的第一处 C 语句写入标记，接收者首次读时钟后观察标记。标记不能区分唤醒 syscall 内的抢占和 syscall 返回后、写入标记前的抢占。诊断版改变了代码布局和交接节奏，其 p50 只供诊断；原始 benchmark 和内核源码均未修改。

Linux 使用冻结 v7.1 PREEMPT_RT 镜像（SHA256 `aac6d3c5fa0c4fdf65f987af635f4cd55a06852b23046a4242a184acc2fd563b`）。Starry 使用当前源码普通 A 镜像（SHA256 `750589429f5afe81b16775dde50050fedb936614b78d7bbde999a51856618473`）和 PGO F 镜像（SHA256 `e48394c7ef55008d0badf2cd6dc841fa8fa6593e069720304e2400634614fd6d`），启动顺序为 A1→F1→F2→A2。两个板卡会话已删除，复查均返回 HTTP 404。

| 系统 | 策略 | 有效聚焦轮次 | 接收者先于标记的比例中位数 | 诊断 p50 ns |
| --- | --- | ---: | ---: | ---: |
| Linux RT | OTHER | 2 | 63.40% | 8458 |
| Linux RT | FIFO | 3 | 0% | 8458 |
| Starry A | OTHER | 5 | 99.19% | 27125 |
| Starry A | FIFO | 6 | 0% | 19541 |
| Starry F | OTHER | 5 | 99.385% | 16334 |
| Starry F | FIFO | 6 | 0% | 11375 |

Linux OTHER 第 1 轮、Starry A1 OTHER 第 2 轮和 F1 OTHER 第 2 轮各只有 19999/20000 样本，`not_parked=1`；原始日志保留，但计算中位数时排除。其余轮次均为 20000/20000、零 `not_parked` 和零错过截止时间。`python3 analyze.py` 复核日志和 benchmark 哈希、样本完整性及标记计数。一次性静态二进制未入库；日志和状态文件保留运行时哈希。

Linux v7.1 在未启用 `CONFIG_PREEMPT_LAZY` 时把 Fair `resched_curr_lazy()` 映射为普通 need-resched；唤醒抢占成立时可在 `try_to_wake_up()` 内切换。Starry 发布 Lazy 请求，但 `prepare_user_return()` 在首次 syscall 返回用户态前检查所有待处理原因。Starry 约 99% 的先于标记结果与此路径一致，否定了“测得的正向窗口通常等到发送者下一次 `FUTEX_WAIT(done)` 才切换”的假设。Linux 约 63% 的结果也否定了“Linux 总能避免唤醒时抢占”的假设。仅凭标记计数无法确定内核栈位置或路径成本。

没有保留优化。旧源码上的 `exp40` Fair Lazy→Immediate 尝试未显示显著收益，不能在缺少新成本假设和当前源码 full20 防线时重复。最近有效的原 benchmark full20 仍为 `resume788` F2：90% 门槛通过 11/20，最差 OTHER 同核 futex 为 16625 ns / Linux RT 8458 ns，即 50.88%。下一步应区分 Fair 唤醒至切换的工作，而不能预设发送者第二次 syscall 位于正向窗口内。
