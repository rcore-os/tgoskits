# resume889: 当前源码 futex park 分段诊断

源码为 `b292a098bb60ef604e7677c37cd95d926ff08200` 加仅用于
`qperf-metrics` 的 [临时补丁](source.patch)。配置使用十项 OrangePi
feature、关闭 cpufreq、无 PGO；冻结 benchmark SHA256 为
`94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`。
镜像 SHA256 为
`89cd85a42930c422c1f5f1206f38d5cc126f9658d4f7c511352e72d3e7c015c0`。
OrangePi-5-Plus-1 两次启动按预定 FIFO/OTHER/OTHER/FIFO/FIFO/OTHER 顺序，
每个进程仅运行 `thread_futex_same_cpu`。两次租约均释放，板卡池回到 4/4。

| 启动 | FIFO 有效 | OTHER 有效 | 无效轮次及原因 |
| --- | ---: | ---: | --- |
| run1 | 3/3 | 2/3 | 第 3 轮 OTHER：19999/20000，`not_parked=1` |
| run2 | 3/3 | 1/3 | 第 3、6 轮 OTHER：各 19999/20000，`not_parked=1` |

两组都不满足完整六轮对照，严格 `check.py` 均以 `group not collected`
拒绝。`analyze_valid.py` 校验了每轮原始文件哈希、镜像/配置/benchmark
身份、直方图计数、样本和计数器差异；它只在**各启动内部**报告有效轮次
的探索性中位数，没有跨启动拼接。

| 启动 | 阶段 | FIFO ns/事件 | OTHER ns/事件 | OTHER-FIFO ns/事件 |
| --- | --- | ---: | ---: | ---: |
| run1 | park_block | 1774.87 | 3396.57 | 1621.70 |
| run1 | park_pick | 946.00 | 1637.44 | 691.44 |
| run2 | park_block | 1761.75 | 3410.05 | 1648.30 |
| run2 | park_pick | 941.01 | 1649.72 | 708.71 |

`park_block` 从请求 defer 后经过 current settle、Blocked 发布及 class
dequeue 到 park preemption 完成；`park_pick` 从该点经过请求合并和下一
线程选取。有效 FIFO 每轮约 4.2 万次事件，OTHER 约 2.18 万次，均覆盖
至少每次 benchmark attempt 一次阻塞，但包含后台事件。阶段值含多次
时钟读取与 qperf 记录导致的机器码/缓存扰动；不能把 2.3 us/事件的
OTHER-FIFO 合计差值当作可删除成本，也不能与无插桩 p50 相减。

源代码检查显示 Fair 阻塞需要保存睡眠 lag、维护延迟 dequeue 与
状态/placement 发布；本次没有证明其中存在可删除的重复事务。
因此不保留探针、不提出生产补丁，也不改变最新有效五 crate PGO
11/20 的 90% 结果。

验证：`cargo fmt`；`cargo xtask clippy --package ax-task` 6/6 通过；
`cargo xtask clippy --package starry-kernel` 的 base 与 qperf-metrics
组合通过，但全 72 组合矩阵为节省临时诊断资源主动中止，**不算完整通过**；
`cargo xtask starry build -c` 成功。标准库测试和无插桩 full20 未运行。
临时补丁已按具体改动恢复，主工作树 `git status --short` 为空。

原始证据：`run1/`、`run2/` 保留全部 12 个日志及前后计数快照，
`run1/serial.log` SHA256
`9f47fce27f231bfce23fdb68d3cf7aaa7ea0b0acad68bbdba72c6495c9ee4752`，
`run2/serial.log` SHA256
`9650666c145c10b8bdbf5385563cbe343e6a8a73b3a77b028bec118ea0d81fcc`。
