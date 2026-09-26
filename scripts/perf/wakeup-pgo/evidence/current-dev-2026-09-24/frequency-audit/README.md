# CPU 频率可比性审计

## 1. 对照边界

本目录记录 PR #2477 在 `e0cb95b459cedf349f9922c81776246281ee656e` 上的独立频率诊断。默认 OrangePi feature 包含 `ax-driver/rk3588-cpufreq`；`build.toml` 只删除这一项，未改默认配置或运行时源码。`A` 是同源码普通 release、默认 feature；`B` 是同源码普通 release、feature-off；`F` 是当前 PR 的全量 PGO、默认 feature。Linux RT 数据仍使用 `../../review-2477/full-pgo-2026-09-23/linux-rt-baseline.json` 对应的冻结基线，**不是**重新取得的 full20 基准。

### 1.1 完整负载

`A1/A2/A3` 和 `B2/B4/B5` 各为三次有效、无插桩 full20 启动。`A1-full.log` 等八份日志和 `results.json`、`results2.json`、`results3.json` 保存原始 p50、p99、p99.9、样本数、镜像哈希及会话关联；`B1/B3` 在 OTHER 同核 futex 各有一个 `not_parked`，整轮无效且原文保留。`check.py` 独立读取所有行并重算三次启动的 p50 中位数。下表是关键项，单位 ns。

| 场景 | Linux RT | 默认 A | feature-off B | RT/B |
| --- | ---: | ---: | ---: | ---: |
| OTHER thread_futex_same_cpu | 8458 | 18667 | 28000 | 30.21% |
| OTHER sched_yield_handoff | 4667 | 7875 | 10791 | 43.25% |
| FIFO absolute_timer_same_cpu | 13211 | 19500 | 27625 | 47.82% |

完整 B 仅 10/20 项达到 Linux RT p50 的 90%；B 是普通 release，**不是** feature-off PGO。A/B 同时改变 CPU 调频行为和编译布局，差值不能全部归因于主频。

### 1.2 实际频率

`probe.c` 及修正版 `probe2.c` 以 `PERF_COUNT_HW_CPU_CYCLES` 对 CPU0/CPU1 各做三段 500 ms 固定核忙循环，记录 cycles、`time_enabled`、`time_running` 和墙上时间。首版在复位 cycles 后误用累计运行时间显示第二、三段 MHz；`check.py` 始终用相邻运行时间差复算，且修正版在 A/B/F 再独立启动一轮确认。归档 Linux RT 原 `Image` 通过 `linux-init.c` 和一次性 initramfs 装载相同修正版探针，不使用网络或根文件系统。`linux-initramfs.list` 将原构建输入的本机绝对路径规范化为仓库相对路径，归档 `initramfs.cpio` 字节保持原状。CPU 频率取每次启动六段原始读数的中位数。

| 系统与镜像 | 有效独立启动 | 送达频率 |
| --- | --- | ---: |
| Starry A，普通默认 feature | A3、A4、A5 | 约 1150.3 MHz |
| Starry B，普通 feature-off | B1、B2、B3 | 约 816.0 MHz |
| Starry F，PGO 默认 feature | F1、F2 | 约 1150.3 MHz |
| 冻结 Linux RT 原内核 | L3、L4 | 815.989、815.988 MHz |

原 Linux RT `Image` SHA256 为 `aac6d3c5fa0c4fdf65f987af635f4cd55a06852b23046a4242a184acc2fd563b`，原 DTB 为 `316dd15b329756be3887dea22f89fc8d1f5b055f8769761f4144b6b1caaea994`。Linux 频率诊断使用原内核、DTB、`cpuidle.off=1`，但用户态载体与冻结 full20 不同，且**没有在 full20 窗口内逐项同步采频**。原基准另记录 `cpufreq=false` 和 `armclk_l=816 MHz`。这足以发现当前 PGO F 的频率不匹配，不能把 19/20 数值计为同频率 90% 验收；不等于重写冻结 Linux RT 基准。

## 2. 复核链

本目录只归档小型诊断载体、原始日志、会话输出和哈希；大型 Linux/Starry 镜像不进入 Git。`check.py` 以仓库中原有冻结 Linux RT JSON 为基准，核对 full20 的 20 个唯一项目与 380000 样本、无效轮次排除、PMU 原始 cycles/运行时间、固定核、串口固件预检以及各文件哈希。它不把诊断读数充当新的 full20 样本。

### 2.1 离线执行

仓库根目录运行 `python3 scripts/perf/wakeup-pgo/evidence/current-dev-2026-09-24/frequency-audit/check.py`。脚本只用 Python 标准库，不连接板卡或改动源码；`SHA256SUMS` 对归档文件提供第二层固定内容校验。`build.toml` 的唯一 feature 差异可与正常 OrangePi 配置核对。

### 2.2 验收影响

当前 PR 的 F 为约 1150 MHz，冻结 Linux RT 为约 816 MHz，实测频率比约 1.4097；旧 19/20 是配置不匹配的历史数值，不是 90% 门槛进度。后续必须在匹配频率下重训候选、运行同配置普通 release/PGO 的有效 full20，分别检查 p50、p99、p99.9 `<3%` 回退、完整样本与唤醒正确性；该目录不声称任何运行时逻辑优化。
