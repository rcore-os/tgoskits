# resume889 qperf 诊断协议

源码 `b292a098bb60ef604e7677c37cd95d926ff08200` 加本目录
`source.patch`；qperf-only、无 PGO、关闭 cpufreq feature，冻结 benchmark
SHA256 `94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`。
同一板 OrangePi-5-Plus-1，一次启动依次运行
FIFO/OTHER/OTHER/FIFO/FIFO/OTHER 六个独立进程，每个只测
`thread_futex_same_cpu`。每轮必须 20000/20000 样本，`not_parked=0`、
`missed_deadlines=0`、程序退出零，`park_block` 与 `park_pick` 计数均不低于
20000 且二者相差不超过 3%。这是分段诊断，不参与 full20/90% 验收。

`run1` 的第 3 轮 OTHER 为 19999/20000、`not_parked=1`，整组无效。
预先限定只做一次独立 `run2` 重试，保留 `run1` 全部原始记录，不跨启动
拼接有效行。若 `run2` 仍无效，则只报告有效轮次的探索性分段均值，并
明确没有完整六轮对照。分段时间含探针开销，不可从无插桩 p50 直接减去。
