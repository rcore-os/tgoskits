# StarryOS 唤醒延迟 PGO 状态

OrangePi-5-Plus 的 profile-use 构建仍是 issue #2308 的实验路径，**目前不可交付**。
2026-09-22 在十项普通板卡 feature、更新后的 `Cargo.lock` 和当时的
`dev@f8ac402f85` 上重新训练后，同板测试确认唤醒 p50 明显下降，但 FIFO
绝对定时器 p999 在两组独立 A-B-B-A 中均超过预定的 3% 回退上限。随后
`dev` 前进到 `7fb26597a8`；新结果没有对这个提交重新完成镜像等价性审计。
`build.py` 的默认入口仍不输出可交付镜像。2026-09-22 对最新已审查的
`dev@7fb26597a8` 还完成了两种按 crate 选择 PGO 的 B-only 探索；
`ax_task` 不使用 PGO 的候选相对留存的旧源码 A 获得超过 10% 的焦点 p50 改善，
但这不是同源码对照，不能打印 `WAKEUP_PGO_IMAGE_READY`。

## 1. 构建边界

普通板卡镜像继续由 `os/StarryOS/configs/board/orangepi-5-plus.toml` 定义，
`scripts/perf/wakeup-pgo/build.py` 不修改它。旧的
`profile-use.profdata.zst` 来自五-feature benchmark，仅供历史核查。
新 `selective-task.profdata.zst` 用十项普通 feature 训练；实验模式将
`ax_task` 留在未使用 PGO 的路径，同时对 `ax_runtime` 和 StarryOS 根 crate
使用 profile。两种镜像的日志级别均为 `Info`，最大 CPU 数均为 8。

### 1.1 前置门禁

`source_gate()` 以 `7fb26597a8ff77e6bff315705520695d8b57a3bc` 为已审查的
源码基线；`docs/` 与本实验目录的变化不影响内核源码。PR 的 `Cargo.lock` 与
基线不同，因此只由 `cargo_lock_gate()` 的完整 SHA256 校验，不能任意漂移。
`toolchain_gate()` 仍校验 Rust 和 LLVM 版本。默认运行在解压和编译前返回
`WAKEUP_PGO_BUILD_NOT_READY`；显式传入 `--experimental-selective-task` 时，
`profile_feature_gate()` 核对十项 feature，`check_wrapper_log()` 核对
`ax_task` 未使用 profile、`ax_runtime` 和根 crate 使用 profile。
`audit_warnings()`、`require_board_text_identity()` 与
`require_board_bin_identity()` 还必须核对失配告警及本次实体板 B 镜像身份。
通过后只输出 `WAKEUP_PGO_EXPERIMENT_IMAGE_MATCHED`，结果文件保持
`ready: false`；匹配代码和镜像不等于性能验收。

### 1.2 恢复条件

要重新开放交付入口，普通 A 与候选 B 必须使用同一精确源码、锁文件、
十项 feature 和冻结负载，并在同一块实体板上采集独立启动轮次。
只有焦点 p50 改善至少 10%，且所有场景的 p50、p99、p999 均无超过 3%
的回退，才可依据该镜像评估开放 READY 入口；不能把当前跨源码
`resume656` 的探索判定升级为正式结论。不得只改 `BASELINE_COMMIT`
或哈希常量来跳过这一步。QEMU TCG 仅用于
训练权重，不提供实体板性能证据；回滚就是继续使用未设置 PGO 的普通板卡配置。

## 2. 实测范围

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
从全新 target 运行 `python3 scripts/perf/wakeup-pgo/build.py
--experimental-selective-task --output-dir <out>` 重建时，
`ax_task` 不含 PGO 参数，`ax_runtime` 和根 crate 均使用 profile；
无 CFG/hash 失配，`.text` 大小和 SHA256 及 `.bin` 除 `.kallsyms` 外的
SHA256 均与 B 镜像一致。整体 `.bin` SHA256 不同，不冒充整文件相同。
**A 来自 `9d06d500c7`，B 来自 `62f4031a90`（内核源码等于
`dev@7fb26597a8`，但普通 A 镜像代码未核实等价），因此这只是跨源码
探索，不能称为当前 `dev` 的 10% 已验收性能提升。**
