# 当前源码 park 阶段诊断

## 1. 实验边界

本目录归档 `b292a098bb60ef604e7677c37cd95d926ff08200` 上的只读语义核对和临时插桩诊断。两项工作都没有形成可保留的运行时优化，不能替代无插桩 full20 或 PR #2477 的验收。

### 1.1 定时器执行者

`resume888-linux-rt-soft-timer-contract.md` 核对 Linux 7.1 PREEMPT_RT 的 `__hrtimer_setup_sleeper()` 与 `ktimers/%u`：普通 sleeper 使用线程化 soft timer；Starry 的 `ParkSoft`/`ktimer` worker 分工与这一执行上下文相符。因而拒绝把任意 soft callback 移入持有 scheduler baton 的 IRQ 后调度帧；这不是已证明等价的局部快路径。本项没有构建或板测。

### 1.2 同核 futex 分段

`resume889/` 保存 `qperf-metrics` 临时 `source.patch`、构建配置、原始板卡运行驱动、两次独立启动的串口和逐轮计数。OrangePi-5-Plus-1 关闭 cpufreq feature；冻结 benchmark SHA256 为 `94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`。两次均按 FIFO/OTHER/OTHER/FIFO/FIFO/OTHER 运行单项 `thread_futex_same_cpu`，不是 full20。

run1 的第三轮 OTHER 及 run2 的第三、六轮 OTHER 均只有 19999/20000 样本、`not_parked=1`，所以两次完整六轮组均无效。`resume889/decision.md` 只对各启动内部的有效轮次报告探索性阶段耗时；探针本身扰动机器码与缓存，不能与无插桩 p50 相减或声称性能收益。临时补丁已撤回。

## 2. 证据核验

归档保留原始计数与无效轮次，而非筛选后伪造完整对照。镜像本体未入 Git；本地原件的 SHA256 为 `89cd85a42930c422c1f5f1206f38d5cc126f9658d4f7c511352e72d3e7c015c0`，归档脚本只能核对两轮状态记录的镜像哈希是否一致，不能重新计算缺席镜像的哈希。

### 2.1 原始记录

`resume889/run1/` 和 `resume889/run2/` 各含六轮日志、前后计数快照、串口、会话与状态 JSON。`resume889/board.py` 和 `guest.sh` 是原始运行驱动，保留本机路径及板卡会话地址作实验记录，不是可直接在其他环境运行的通用工具。`SHA256SUMS` 固定归档文件字节；不包含未提交的镜像。

### 2.2 离线复算

从本目录运行 `sha256sum -c SHA256SUMS` 和 `python3 resume889/analyze_valid.py` 可核对原件及两次无效组判定。`resume889/check.py` 是预设的完整六轮有效门禁，分别设置 `RESUME889_RUN=run1` 或 `run2` 时应因 `group not collected` 失败，不能把有效子轮拼接成完整组。
