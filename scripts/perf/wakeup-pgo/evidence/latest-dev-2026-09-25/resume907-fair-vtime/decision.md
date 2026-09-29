# resume907: Fair 虚拟时间重复计算筛查（拒绝）

本实验在 `b292a098bb60ef604e7677c37cd95d926ff08200` 上，仅改动
`CpuRunQueueState::put_prev_unlinked_current()`：Fair 当前任务在同一 rq 锁内
`settle_current()` 已更新虚拟时间时，跳过 requeue 前再次计算；non-Fair
路径及 requeue 后的计算保留。补丁见 `source.patch`，SHA-256 为
`b824173a37744537d972c168b61178a21031f86ce21a21903e28ea134c729a50`。
这是一次普通 release、无 PGO、无 qperf 插桩的同源码筛查，不是生产配置的收益。

- 板卡：OrangePi-5-Plus-1，8 CPU，约 816 MHz；租约
  `72e8f57a-cd0a-4aed-8a1b-701c0f8a63b9`，运行后释放并确认 404。
- 顺序：A1、B1、B2、A2 四次独立启动；四轮各 20/20 行、
  380000/380000 样本、`not_parked=0`、`missed_deadlines=0`。
- 冻结 benchmark SHA-256：`94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`。
  配置 SHA-256：`97a5d39843b42bca2e965ce83ebfc6458918dd1566eee46db73232f65a3208b5`。
  A 镜像 SHA-256：`31d68c9c739af52722d8596cdafd52796b571ee26d690a8b2a174aeb04bc1ca9`；
  B 镜像 SHA-256：`448a418b0ef54a3d1b88dbf4af5ee434ba1199a7f83a00363e0f7f7ca7ce940a`。
- `cargo fmt`、`cargo xtask clippy --package ax-task`（6/6）、ArceOS QEMU
  `task-fair-wake-idle-sibling` 与 `task-wait-queue`（各 1/1）、Starry AArch64
  release 构建通过。构建警告限于已知上游 future-incompatibility。

冻结 Linux RT p50、同源码 A1/A2 p50 中位数、B1/B2 p50 中位数（单位 ns）：

| 策略 | 场景 | RT | A | B | RT/B |
|---|---|---:|---:|---:|---:|
| FIFO | absolute_timer_same_cpu | 13211 | 28333 | 28666.5 | 46.085% |
| FIFO | clock_pair | 875 | 875 | 875 | 100.000% |
| FIFO | futex_wait_mismatch | 2333 | 1750 | 1750 | 133.314% |
| FIFO | futex_wake_empty | 1750 | 1750 | 1750 | 100.000% |
| FIFO | getpid | 1458 | 1458 | 1458.5 | 99.966% |
| FIFO | process_futex_cross_cpu | 15167 | 31937 | 31937.5 | 47.490% |
| FIFO | sched_yield_handoff | 3791 | 4958 | 4958 | 76.462% |
| FIFO | sched_yield_no_peer | 2333 | 2333 | 2333 | 100.000% |
| FIFO | thread_futex_cross_cpu | 13708 | 29750 | 29458.5 | 46.533% |
| FIFO | thread_futex_same_cpu | 8458 | 20271 | 20562.5 | 41.133% |
| OTHER | absolute_timer_same_cpu | 23920 | 54625 | 54583.5 | 43.823% |
| OTHER | clock_pair | 875 | 875 | 875 | 100.000% |
| OTHER | futex_wait_mismatch | 2333 | 1750 | 1750 | 133.314% |
| OTHER | futex_wake_empty | 1750 | 1750 | 1750 | 100.000% |
| OTHER | getpid | 1458 | 1458 | 1458 | 100.000% |
| OTHER | process_futex_cross_cpu | 17792 | 35583 | 35437.5 | 50.207% |
| OTHER | sched_yield_handoff | 4667 | 11083.5 | 11083 | 42.110% |
| OTHER | sched_yield_no_peer | 2625 | 2625 | 2770.5 | 94.748% |
| OTHER | thread_futex_cross_cpu | 16041 | 32959 | 32958 | 48.671% |
| OTHER | thread_futex_same_cpu | 8458 | 28000 | 27708.5 | 30.525% |

该 B 只有 **10/20** 项达到冻结 RT 的 90%；最差 OTHER 同核 futex
虽比同源码 A 的 28000 ns 低约 1.041%，仍为 RT 的 30.525%。
OTHER `sched_yield_handoff` 为 11083.5→11083 ns，无实质改善。
60 个同源码 p50/p99/p99.9 比较中有 **8 项回退至少 3%**：
OTHER 跨进程 futex p99.9 +42.888%、OTHER handoff p99.9 +15.999%、
FIFO 跨线程 futex p99 +14.222%、FIFO 跨进程 futex p99 +9.057%、
FIFO mismatch p99 +7.729%、OTHER empty wake p99 +7.700%、
FIFO timer p99 +7.295%、OTHER no-peer yield p50 +5.543%。

**决定：拒绝并撤回运行时代码。** 四轮日志、串口、哈希、构建记录和
`check.py` 随本目录归档；运行 `python3 check.py` 可复算完整表和回退项。
既有五 crate PGO 的 11/20、最差 57.999% 是另一套镜像和实验，不能用
本次普通 release 的 A/B 差值宣称其收益；本筛查也不满足三次有效候选
及生产复现的最终验收。PR 维持 Draft。
