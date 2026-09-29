# resume864: OTHER gate 唤醒后的切换位置

诊断源码 `b292a098bb60ef604e7677c37cd95d926ff08200` 基于
`dev@714accd8f636c540b2c3554b0b1e5cb885be42a4`。仅在
`qperf-metrics` 下给 `ResolvedFutex::wake` 的 selected-one/enqueued-one
`wake_batch` 前后读取全局 context-switch 计数，并以私有 futex 地址的
页内偏移区分 `gate` 与 `done`。冻结 benchmark 的 AArch64 反汇编
`run_sender.part.0` 在 `0x1bd0/0x1c60` 使用基址 `+0x10` 的 gate，
`0x1c94` 使用 `+0x14` 的 done，与探针分类相符。无 PGO、关闭 cpufreq
feature 的镜像 SHA256 为
`52d38b69d9e694323beddfbd1009dd684a605697fa91b8b703287188f75d2ee7`；
benchmark SHA256 为
`94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`。

OrangePi-5-Plus-1 会话 `d34feb26-2831-4d1b-b042-390cd471c2c9`
通过 U-Boot PLL 检查。同一次启动按 OTHER/FIFO/OTHER/FIFO/OTHER
运行五个独立进程，每轮 20000/20000 样本、零 `not_parked` 和
`missed_deadlines`、退出零。完整串口、进程日志、计数器前后快照和
会话状态在 `run1/`；`python3 check.py` 已从原件复算 5/5 有效。

| 轮次 | gate selected-one | gate 窗口内任意切换 | gate 窗口内抢占切换 | done selected-one | 全轮抢占切换 |
| --- | ---: | ---: | ---: | ---: | ---: |
| OTHER-1 | 21000 | 77 | 58 | 214 | 21546 |
| FIFO-1 | 21000 | 35 | 23 | 21000 | 302 |
| OTHER-2 | 20999 | 76 | 58 | 210 | 21539 |
| FIFO-2 | 21000 | 0 | 0 | 21000 | 85 |
| OTHER-3 | 21000 | 71 | 50 | 224 | 21548 |

OTHER 每轮约 21000 次 gate 命中唤醒中，窗口内任意切换只占
0.338%–0.367%。计数器是**全局**的，其他 CPU 切换只能抬高该数，
故它是目标 CPU 窗口内切换的保守上界，不是精确目标事件数。
即便把 77 次全部归于 20000 次计时样本，比例仍小于 0.4%。
与全轮约 21540 次抢占切换相比，主交接并不发生在 `wake_batch`
内部。OTHER 的 done selected-one 仅 210–224 次，而 FIFO 每轮
21000 次，也符合 OTHER 接收者通常在发送者 park 之前运行的路径；
探针没有逐次关联 gate、done 与目标 tid，不能把该解释写成逐事件证明。

源码的 Fair `WakeeSelected -> Lazy` 和 `prepare_user_return()` 消费
Lazy 与此结果一致；窗口计数只能定位为 `wake_batch` **之后**，
还不能在用户态返回、发送者后续 park、接收者恢复三者间分摊成本。
这次是带插桩的聚焦诊断，p50 不是原生 full20 或 Linux RT 对照，
也不能据此宣称优化收益。下一步只沿 Lazy 用户态返回到接收者恢复
的真实路径寻找语义等价候选；旧源码有效 G1/G2 仍是 11/20 达到
90%，最差 OTHER 同核 futex 58.00%。

首次构建因 Starry kernel 无直接 `ax-task` 依赖而失败，日志保留；
通过现有 `ax-runtime::diagnostics` 边界暴露只读计数后构建成功。
`cargo fmt`、`git diff --check`、`ax-task` 6/6、`ax-runtime` 26/26、
`starry-kernel` 72/72 定向 clippy 与 `cargo xtask test` 68/68
白名单均通过。六个探针源码文件和 tracked diff 已封存；临时探针
已由 `apply_patch` 撤回，`git status --short --branch` 确认工作树
恢复干净。板卡会话删除请求已成功返回，之后 HTTP GET 因网络超时
尚未取得 404 二次证明。

PR 归档不包含 17 MB 的诊断镜像；本地原件在归档前按上列 SHA256
核验。仓库内的 `check.py` 在镜像缺席时核对结果记录中的固定镜像
哈希，并复算串口、五轮原始日志与计数器；若将镜像放回此目录，
脚本也会核验镜像内容。`SHA256SUMS` 只覆盖实际入库文件。

后续 `resume865` 只读审计检查了 Fair 激活、Lazy 请求、
`prepare_user_return()`、发送者 park 和接收者恢复，没有找到可安全
移除的多微秒操作。它曾提出省略当前任务的 runtime deadline 发布，
但 Lazy 未实际切走时会破坏语义，故未实施；旧 `resume862` 已给出
gate 唤醒至少 97.645% 走 off-rq 激活的保守下界。审计没有产生
源码改动、构建或新板测，也不能充当原生性能收益。
