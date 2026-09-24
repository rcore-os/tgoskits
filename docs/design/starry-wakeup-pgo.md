# StarryOS 唤醒延迟 PGO 状态

2026-09-23 基于 `dev@ee5a638e0e` 构建普通 release 与全量 PGO，
同一源码、板卡和冻结负载各完成两次独立 full20 启动。全部 20 项
`Linux_RT_p50 / PGO_p50` **严格超过 70%**，相对普通 release 的各项
p50、p99、p99.9 回退均小于 3%；最差项 OTHER 同核 futex 为 78.37%。
这是旧源码上的阶段性验收，不能代替当前 `dev` 的验收；issue #2308
原定 90% 最终目标未改变。
2026-09-22 的跨源码选择性 PGO 与更早的全量 PGO 尾延迟失败记录仍留在下文。

## 1. 构建边界

普通板卡镜像继续由 `os/StarryOS/configs/board/orangepi-5-plus.toml` 定义，
`scripts/perf/wakeup-pgo/build.py` 不修改它。旧的
`profile-use.profdata.zst` 来自五-feature benchmark，仅供历史核查。
`full-pgo-2026-09-23.profdata.zst` 使用旧源码和当时的十项普通 feature
训练，保留供历史证据核查。当前 `build.py` 使用在
`dev@9a7b868bab` 上重新训练的 `full-pgo-2026-09-24.profdata.zst`；
当前普通板卡配置增加了 `legacy-board-init`，因此 profile 与普通
release 均包含十一项 feature。该新 profile 的实体板结果有两次独立有效
PGO full20；后续 `resume742` 补得两次有效的同源码普通 release 对照，
但其 p99.9 回退超出门槛，不能沿用旧源码的阶段性通过结论。
`stdlib-wrapper.py.in` 仅排除预编译的 sysroot crate，`ax_task`、
`ax_runtime` 和 StarryOS 根 crate 均使用 profile。两种镜像的日志级别
均为 `Info`，最大 CPU 数均为 8。旧 profile 保留在本目录供历史证据核查。

### 1.1 前置门禁

`source_gate()` 以 `9a7b868bab8dcceb42acab516dcc88d5cc69985a` 为
源码基线；`docs/` 与本实验目录的变化不影响内核源码。`Cargo.lock` 由
`cargo_lock_gate()` 的完整 SHA256 校验。`toolchain_gate()` 检查 Rust 和
LLVM，`profile_feature_gate()` 核对十一项 feature，`check_wrapper_log()`
核对 `ax_task`、`ax_runtime` 和根 crate 均使用当前 profile。
`audit_warnings()`、`require_board_text_identity()` 与
`require_board_bin_identity()` 核对失配告警，以及 `.text` 和除 `.kallsyms`
外的完整 `.bin` 与实体板候选的身份。只有重建成功才输出
`WAKEUP_PGO_IMAGE_READY`；仅 `--prepare-only` 不编译，不报告 ready。
这里的 ready 只证明当前源码镜像可复现，不表示性能门槛已通过。

### 1.2 验收边界

阶段性门槛是全部 20 项 `Linux_RT_p50 / PGO_p50 > 70%`，且相对同源码
普通 A 每项 p50、p99、p999 回退严格小于 3%。各侧取两次有效、无插桩
full20 启动的原始 p50 中位数；20 项集合、38 万样本、零 `not_parked`
和零 `missed_deadlines` 是逐轮门禁。`check-full20.py` 从原始日志复算，
不能用跨源码 A→F 的差值代替。QEMU TCG 仅用于训练权重；回滚是继续
使用未设置 PGO 的普通板卡配置。

## 2. 历史实验

旧的四轮日志、五-feature profile 与 `latest-dev-image-equivalence.json`
只说明 `d051d572b7` 至当时 `29f3fef885` 的历史 benchmark 镜像关系，
不代表十-feature release。详细的旧构建链和原始证据保留在
`starry-wakeup-pgo-2026-09-21-archive.md` 与 `scripts/perf/wakeup-pgo/evidence/`。

### 2.1 全量 PGO 候选

新实验的源码提交是 `9d06d500c7ebbb960678d16d4a44aa2e036cf87e`，
它基于当时的 `dev@f8ac402f85`，并包含 `cargo update` 后的锁文件。
普通 A 与十-feature PGO B 使用相同源码、板卡配置和冻结 benchmark；
新 profile SHA256 为 `eaea4c2f8d0f9dda04e988b6c4f38a7013333871527bf2a6db0f268b9bd4e749`。
PGO 构建审计记录 0 条 CFG/hash 失配、0 条未预期告警。训练时首次监控
因读取默认 `target/` 的旧 ELF 而拒绝，恢复监控在同一次 QEMU 运行中
核对了实际镜像的关键 section、workload 终态及 counter 导出；失败和
恢复日志均保留在实验档案，不能把首次监控称为成功。

### 2.2 板卡判定

`OrangePi-5-Plus-1` 上首次 A-B-B-A 的 A2 有一次 `not_parked`，整组无效，
未参与性能比较。随后两组独立四轮均完成每轮 20 项、380000 样本，
且通过逐轮完整性检查。焦点 `OTHER/thread_futex_same_cpu` p50 从
18375 ns 降至 11375 ns，改善 **38.10%**；两组各有 14/20 项改善
至少 10%。但 `FIFO/absolute_timer_same_cpu` 的 p999 分别回退
11.23% 和 7.72%，两个四轮比较结果都是 **FAIL**。在事先登记的额外
四轮后，合并全部八个有效启动的各侧中位数复核，这一项 p999 仍回退
11.57%。因此不能报告“无明显回退”，也不能把新 profile 换入交付入口。

新旧测试不混用：新十-feature 镜像的源码、锁文件及 profile 与旧五-feature
归档不同。失败组、两组有效原始日志、状态和比较报告放在
`scripts/perf/wakeup-pgo/evidence/review-2477/`；根目录相对路径的
`SHA256SUMS` 可在仓库根目录核对。两组各自的 FAIL 与八轮探索性复核
同时展示，不以八轮结果改写原四轮判定。当前实验未直接观测温度及节流状态。

### 2.3 按 crate 选择 PGO

在 `dev@7fb26597a8` 的源码和相同的十-feature profile 上，`resume655`
只让 `ax_runtime` 不使用 PGO。两次独立 B-only full20 启动均有效，焦点
`OTHER/thread_futex_same_cpu` p50 相对旧源码 A 的四次启动中位数下降
32.54%，但 `FIFO/absolute_timer_same_cpu` p999 回退 50.69%，故弃用。
`resume656` 改为只让 `ax_task` 不使用 PGO。两次 B-only full20 各有
380000 样本，均无 `not_parked` 或 `missed_deadlines`，且审计未发现
CFG/hash 失配。`OTHER/thread_futex_same_cpu` p50 从旧 A 的 18375 ns
降至 B 的 15750 ns，改善 **14.29%**；20 项中 12 项 p50 改善严格超过
10%，按两侧各自启动中位数比较，全部 20 项的 p50、p99、p999 均未达到
3% 的回退阈值。`FIFO/absolute_timer_same_cpu` p999 从 29875.5 ns
降至 28854.5 ns。

原始 B 日志、哈希、两种候选的完整比较、运行状态及重算脚本保存在
`scripts/perf/wakeup-pgo/evidence/review-2477/selective-task/`。
`compare-historical.py` 只使用 `resume653/654` 留存的四轮 A，未重测 A，
先核对每轮日志的 SHA256、20 项和样本完成情况再计算中位数。
当时从全新 target 使用旧版 `build.py --experimental-selective-task`
重建时，
`ax_task` 不含 PGO 参数，`ax_runtime` 和根 crate 均使用 profile；
无 CFG/hash 失配，`.text` 大小和 SHA256 及 `.bin` 除 `.kallsyms` 外的
SHA256 均与 B 镜像一致。整体 `.bin` SHA256 不同，不冒充整文件相同。
**A 来自 `9d06d500c7`，B 来自 `62f4031a90`（内核源码等于
`dev@7fb26597a8`，但普通 A 镜像代码未核实等价），因此这只是跨源码
探索，不能称为当前 `dev` 的 10% 已验收性能提升。**

## 3. 当前源码验收

`dev@ee5a638e0e` 上的普通 A 镜像与历史 A 机器码不同，因此重新采集
`resume666-A1/A2`。选择性 PGO `resume667-F1/F2` 仍只有 51.78% 的
OTHER 同核 futex 比值，且 FIFO 跨核进程 futex p99.9 回退 13.73%，
因此弃用。全量 `ax_task` PGO 复用同源码新训练的 profile，并独立构建
`resume668` 镜像后采集 `F1/F2`；它不是 2026-09-22 被拒绝的旧 F 镜像。

### 3.1 镜像与样本

普通 A 镜像 SHA256 为 `fcdd7454acc1ab3d78450e4a92e2baa43f92d0b89a3c8fa4579cca3abdf5b533`，
全量 PGO 镜像 SHA256 为 `9bd198e7a3996b4606d235f2484378293a7d8509fe8849ec66091361f4d3b0b6`。
两侧均使用 SHA256 为 `94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`
的冻结 benchmark，在 `OrangePi-5-Plus-1` 上各两次完整启动，每轮
20 项、380000 样本。镜像、PLL、CPU0/CPU1、结果集合与原始计数的验证
记录在 `scripts/perf/wakeup-pgo/evidence/review-2477/full-pgo-2026-09-23/`。

### 3.2 阶段性结果

`check-full20.py` 读取归档的四轮日志、冻结 Linux RT 基线与板卡状态，
按场景取两次启动中位数再执行严格不等式。最差项
OTHER `thread_futex_same_cpu`：Linux RT 8458 ns，普通 A 18958.5 ns，
全量 PGO 10792 ns，即 78.37%；OTHER `sched_yield_handoff` 为
4667/5250 = 88.90%。20/20 项超过 70%，全部 p50、p99、p999 最大
回退为 0%，阶段性 PASS；完整 20 项数值在 `resume668-comparison.json`。
最终 90% 目标仍未达到，不能据此关闭 issue #2308。

### 3.3 当前源码的未完成验证

变基到 `dev@9a7b868bab` 后，原 `build.py` 的旧 source gate 会拒绝
272 个非排除路径的变化，旧 profile 也不能声称对应新源码。
`resume710` 在源码树 `4322e0c505` 上重新训练全量 PGO；该树与 PR
变基后 `d1fb756823` 的 tree 相同。新 profile SHA256 为
`b916a666710826285845fbe017e70f9a665d423ee79df6ee4896368d2d837ec6`。
`resume711` 的 `OrangePi-5-Plus-1` 无插桩 F1 镜像 SHA256 为
`6a234ea391346f2450fdef8b04d19988211dbe3485cf62e3a25544ca4e80cac4`；
该轮 20 项、380000/380000 样本，零 `not_parked` 与
`missed_deadlines`，19/20 项达到冻结 Linux RT p50 的 90%。同镜像
`resume712` F1 也独立完成有效 full20，仍为 19/20；两轮 OTHER
`thread_futex_same_cpu` p50 为 11083/11375 ns，中位 11229 ns，
相对 Linux RT 8458 ns 为 75.32%，还需降至 9397 ns 或更低。
`resume711` 普通 A1 有效，但 A2 在该项缺少一个样本；`resume712`
普通 A1 同样缺样本，两次序列的后续 F2 均未执行。`resume742` 在
同一源码上重建普通 release 镜像，并核实其 `.text` 与旧归档普通镜像
相同、`.bin` 除 `.kallsyms` 外逐字节一致；随后两次有效普通 A full20
各有 380000/380000 样本。将这两轮 A 与上述两轮 F 的原始 p99.9
分别取中位数，FIFO `absolute_timer_same_cpu` 为 32583.5→44625 ns，
回退 36.96%，已超过 `<3%` 门槛；且 A/F 未构成交错四轮序列。
`resume742` 试验性单 waiter futex 快路径虽然使 OTHER 同核 futex p50
下降 3.10%，却使 OTHER 跨核进程 futex p99.9 回退 25.07%，已拒绝，
未进入运行时代码。当前源码既未通过尾部分位门，也未达到 20/20 项 90%。
当前 `build.py` 已从全新 target 重建出与 F1 `.text`、`.bin`（除
`.kallsyms`）一致的镜像；该构建只证明镜像身份，不补足缺失的板测启动。
新 profile、原始日志、无效轮次、训练状态与构建审计保存在
`scripts/perf/wakeup-pgo/evidence/current-dev-2026-09-24/`。
