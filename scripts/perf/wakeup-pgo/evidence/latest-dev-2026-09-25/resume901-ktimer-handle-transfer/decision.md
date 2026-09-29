# resume901: ktimer 句柄转移筛查

## 1. 改动与身份

本轮只触及软任务计时器的句柄交接，不改变到期队列的存储或取消协议。

### 1.1 句柄转移

`b292a098bb` 上的临时补丁仅改变软任务计时器完成后的句柄交接：
`complete_task_timer_execution()` 保留取消与 park generation 判定，但只
返回 `should_wake`；`service_current_ktimer_pass()` 在退出关中断段后
将已取得的 `ThreadHandle` 消费为 `ThreadWakeHandle`，避免再次克隆
`Arc` 和增加外部租约。补丁 SHA-256 为
`31d2a41caaae729ddd3d2003e970b44386efaffd09c8df3a9befda6a793e66dc`。

### 1.2 控制镜像

同源码普通 release A 镜像 SHA-256 为
`31d68c9c739af52722d8596cdafd52796b571ee26d690a8b2a174aeb04bc1ca9`，
候选 B 为
`7cd0b108614985db8df6d98ecc5bd6da3b2d0f7ad7acefab5dfa547f22101be2`。
两者使用同一份 `ordinary.toml`，其 SHA-256 为
`97a5d39843b42bca2e965ce83ebfc6458918dd1566eee46db73232f65a3208b5`；
无 PGO、qperf 或 cpufreq feature。控制镜像复用 `resume898` 已板测的
未改源码构建；`board.py` 在运行前核对了其源码、配置和镜像哈希。
脚本末尾沿用旧版的 `RESUME898_FULL20_COMPLETE` 输出标签；本轮身份由
`resume901` 镜像、补丁哈希和会话记录确定，不以该标签判定。

## 2. 验证与板测

`cargo fmt`、`cargo xtask clippy --package ax-task` 的 6/6 组合、
AArch64 QEMU `task-sleep` 与 `task-kernel-timer` 各 1/1 通过。

### 2.1 启动有效性

OrangePi-5-Plus-1 使用冻结 benchmark SHA-256
`94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`，
按 A1、B1、B2、A2 四次独立启动运行无插桩 full20。A1、B2、A2
各有 20/20 项、380000/380000 样本、零 `not_parked` 和漏唤醒。
B1 的 OTHER 同核 futex 只有 19999/20000 样本、`not_parked=1`，
整轮作废，不拼接其余场景。

### 2.2 性能门槛

唯一有效候选 B2 的 OTHER `absolute_timer_same_cpu` p50 为 56166 ns，
A1/A2 各为 54500 ns，探索性回退 **3.057%**。B1 的该行虽有完整
10000/10000 样本、p50 为 56250 ns，仍不得从无效整轮抽行构造验收。
B2 全表只有 10/20 项达到冻结 Linux RT p50 的 90%；最差 OTHER
同核 futex 为 28000 ns，对 RT 8458 ns 为 **30.207%**。B2 对
A1/A2 中位数的 60 个 p50/p99/p99.9 筛查比较中，8 项回退至少 3%；
FIFO timer p99.9 为 79958 ns，对 A 中位数 44771 ns 高 **78.593%**。
只有一次有效候选启动，这些比较是淘汰筛查，不是正式两轮回退门禁。

## 3. 决定与复核

候选未提高目标 timer p50，且单轮已触及多项回退；不补采第二次候选，
不进入 PGO 或生产源码。临时补丁已按 SHA 核对并精确撤回，主工作树
恢复干净，板卡池 OrangePi-5-Plus 为 4/4 可用。最新有效五 crate
PGO 的 11/20、最差 57.999% 状态不变，90% 最终目标未完成。

运行 `python3 check.py` 可从四份原始日志复核每轮有效性、20 项 Linux
RT 比率和同源码筛查回退。`full20/` 保存原始结果与串口。镜像本体留在
本地实验目录，不进入 PR；仅有镜像哈希不能证明生产构建复现。
