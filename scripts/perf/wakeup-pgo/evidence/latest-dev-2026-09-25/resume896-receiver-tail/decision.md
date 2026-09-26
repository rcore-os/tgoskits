# resume896：当前源码的接收端尾部探针

## 1. 测量边界

这是 `b292a098bb60ef604e7677c37cd95d926ff08200` 上启用 `qperf` 的无 PGO 诊断，不是生产优化或 full20 验收。冻结 benchmark SHA256 为 `94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`；镜像 SHA256 为 `9d8fb22acb51f1a1e14f41e88dac9f8449672ee5a6b45a65e44b1a291379e965`。OrangePi-5-Plus-1 的 PLL 已核为约 816 MHz，两次租用均已释放。

## 2. 样本有效性

`analyze.py` 将每轮 benchmark 样本完整性与探针的 generation、错误计数和直方图覆盖分别核对，完整六轮组只有六轮都有效才成立。

### 2.1 测量轮次

两次独立启动各按 FIFO、OTHER、OTHER、FIFO、FIFO、OTHER 运行 `thread_futex_same_cpu`。两次启动的 FIFO 第 1/4/5 轮和 OTHER 第 3 轮均有 20000/20000 样本、零 `not_parked`。OTHER 第 2/6 轮无效：run1 分别为 19999/20000、19998/20000 样本，`not_parked=1/2`；run2 两轮都是 19999/20000，`not_parked=1/1`。**两组六轮组均无效**，不能拼接无效轮次进行验收。12 轮探针的重叠、重复切换、代际不符、取消、缺失切换和无效时间戳计数均为零。

### 2.2 阶段分布

两个有效 OTHER 子轮各自从 `Thread::scheduler_switch_in` hook 入口计到 `ThreadWaitState::finish`，均值分别为 1807.527 和 1827.722 ns；250 ns 直方图的 p50 桶起点均为 1750 ns，p99 桶起点分别为 5750 和 5500 ns。六个有效 FIFO 子轮的同一区间均值为 1369.817–1409.892 ns，p50 桶起点为 1250 ns。这些数据包含临时探针开销、未启用 PGO，只覆盖接收端切入及 futex wait 解卷，不含 owner 交接尾部和 syscall 返回。FIFO 与 OTHER 策略路径不同、样本不配对，差值不是可删除开销。不能从 `resume883` 无插桩 14583 ns p50 中相减，更不能称为优化收益。此接收端片段本身没有定位到 OTHER 同核 futex 达到 90% 所需的 5186 ns 以上降幅。

## 3. 复核与决定

`analyze.py` 按原始前后快照核对输入哈希、源码/benchmark/镜像/配置身份、PLL 和板卡释放标记、样本门禁、探针错误及直方图总数；`analysis.json` 是其输出。`source.patch.gz` 和 `build.log.gz` 是无时间戳 gzip 归档，解压后的字节与本地原件相同；前者是已撤回的临时插桩差异，不应应用到生产分支。`image.bin` 仅保存在本地，Git 只收录其 SHA256。下一步需要按代际关联 owner 交接与用户态返回区间，针对明确成本点提出改动，再用同源码普通 release、无插桩 full20 与尾延迟门禁检验。
