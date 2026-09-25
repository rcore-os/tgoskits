# Fair 唤醒抢占时机筛查

## 1. 试验边界

本次只在隔离工作区的 `WakePreemptionDecision::reschedule_kind()` 中将 Fair
唤醒的 `RescheduleKind::Lazy` 临时改为 `Immediate`，检查更早请求调度能否缩短
OTHER 同核 futex 的任务交接。普通 A 和候选 B 均从 `b292a098bb` 加同一临时
exporter 补丁的工作区构建；`candidate.patch` 保存了包含这处单行试验的完整差异。
这些补丁没有进入 PR 的生产源码。

### 1.1 构建身份

两侧采用同一份 `ordinary.toml`，关闭 cpufreq 和性能插桩。旧 A 镜像 SHA256
为 `ad77bbb56abeeafedb90b767ff7f226cf99df757b838e97526b031c34757e747`，
B 为 `dc09012183d9b44bb7eb3fa1949d695e16f22e4a04153080a068792301d2d6cb`。
旧 A 的构建日志在相邻的 `resume873-876-current-pgo/build-ordinary.log.gz`；
与本目录的 `build.log` 一样，均将同一隔离工作区列为 `Workspace root`。
恢复源码后在另一 target 缓存目录重建的 A 镜像哈希不同，原因未查明，
不能把该重建镜像误认为板测过的 A，也不能把 target 目录误认为源码目录。

### 1.2 板测顺序

OrangePi-5-Plus-1 使用冻结 benchmark SHA256
`94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`，
按 A1、B1、B2、A2 各自独立启动运行无插桩 full20。四次原始场景日志、
串口记录和租约保存在 `full20/`，`status.json` 标注有效性及镜像哈希。

## 2. 性能判定

有效的 A1、B1、B2 均有 20 个唯一项、380000/380000 样本、零
`not_parked`。A2 的 OTHER `thread_futex_same_cpu` 只有 19999/20000
样本且 `not_parked=1`，因此整轮作废，不拼接其余 19 项参与验收。

### 2.1 p50 门槛

B1/B2 原始 p50 中位数只有 **10/20** 项达到冻结 Linux RT 的 90%；
最差 OTHER 同核 futex 为 26396 ns，对 RT 8458 ns 仅 **32.043%**。
有效 A1 的该项为 27125 ns，单轮对两轮候选的探索性差值为 -2.688%，
不足以解决缺口。一次有效 A 不能证明同源码普通 release 的正式回退门。

### 2.2 尾延迟风险

以唯一有效 A1 对 B1/B2 中位数筛查，有六个 p50、p99 或 p99.9
比较至少回退 3%，其中 OTHER 跨核线程 futex 的 p99.9 为 +20.344%。
这只是淘汰依据，不能冒充两次有效普通 A 启动的正式门禁结论。
候选已拒绝；单行调度改动已撤销，工作区差异重新与试验前 exporter
补丁逐字节一致。PR 继续保持 Draft，90% 最终目标仍未完成。

## 3. 离线核验

在本目录运行 `sha256sum -c SHA256SUMS` 与 `python3 check.py`，可以核对
归档字节、每轮样本和 20 项门槛。镜像本体未纳入 Git，仅记录 SHA256；
这些离线检查不等于板卡复测或生产构建复现。
