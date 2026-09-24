# 唤醒性能后续实验记录

本目录保存 PR #2477 之后的原始 full20 日志和实验状态。`resume771` 与
`resume773` 使用不同源码，不能把两者的差值解释为优化收益。两组镜像均
关闭 `ax-driver/rk3588-cpufreq` feature；独立 PLL 检查报告约 816 MHz。
该频率检查不等于在每个 full20 计时窗口同步采频。

## 1. 实验边界

完整日志保留 20 个场景的原始 p50、p99、p99.9 和样本计数；`status.json`
记录源码、镜像、benchmark SHA256、启动和判定。下列实验均未产生可保留的
调度器或 futex 运行时改动。

### 1.1 原生选择性 PGO

`resume771` 基于 `c346962754ff7a96359a928f07ffaedbcfeb9ca5`，仅对
`ax_task`、`ax_sched`、`ax_runtime` 使用原生训练的 profile。普通 release
A1 和无插桩 PGO F1 在 OrangePi-5-Plus-1 上各完成一次有效 full20：
每轮 20 项、380000/380000 样本、`not_parked=0`。F1 的 20 项中没有一项
达到冻结 Linux RT p50 的 90%；最差 OTHER `thread_futex_same_cpu` 为
20708 ns，对应 Linux RT 8458 ns，比例 40.84%。相对同源码 A1，p50、
p99、p99.9 分别有 11、11、10 项回退至少 3%。候选已拒绝，单次配对
只作淘汰筛查，不构成三次启动的验收。

### 1.2 最新源码普通基线

`resume773` 基于 `dev@05175ca38823b631a73777b0130226ddfa558439`，
使用普通 release、无 PGO、无 `qperf-metrics` 插桩。OrangePi-5-Plus-1 的
A1 有效 full20 包含 20 项、380000/380000 样本、`not_parked=0`；OTHER
`thread_futex_same_cpu` p50 为 27125 ns。该轮仅建立新源码对照，
**没有同源码候选，也没有 90% 验收结论**。普通镜像 SHA256 为
`b872b4b59829e0b63bd21a6d79c75295c5a4ee7fd470ef72afc8585a1976b8e5`。

### 1.3 四 crate 原生 PGO 训练前置修复

`resume779` 在相同 `dev@05175ca388` 上将 AArch64 C ABI `memset` 改为
不接受编译器插桩的 naked 汇编；非 AArch64 实现保持不变。原生
`ax_task`、`ax_sched`、`ax_runtime`、`starry_kernel` 四 crate 的
profile-generate 镜像在 OrangePi-5-Plus-1 启动到 Starry shell。
训练镜像 `.bin` SHA256 为
`7165206f4851cf923212ee2de2dea28b86dd0ec42dd9c34e23eb7d600227f653`，
串口原始记录 SHA256 为
`4c05ee9633f0d49106229aca071a825fa7f53392953b251b3f942decc1965b64`。

`resume780` 临时启用 debugfs profile exporter 后取得四 crate 原生计数，
profdata SHA256 为
`7d71a724fd5ce66e4faf15280995602392fc6ff8e6d41fc197cdbc0eb8e67156`。
16 个定向训练场景都运行至完成标记，但四次 OTHER 同核 futex 各有
`19999/20000` 样本和 `not_parked=1`；这份训练记录不满足 full20 验收。
exporter 已撤回，未纳入 PR 源码。`resume781` 的 profile-use 构建因
训练 feature 改变 `starry_kernel` 身份，出现 14792 个缺失 profile 警告，
未作为候选上板；`resume782` 的严格符号/CFG 哈希映射也无法覆盖最热的
`sys_futex` 和 `ResolvedFutex::wait_nofault_until`，因此未把部分映射
当作已解决的训练方案。

`resume783` 在上述 `memset` 源码上完成十项 feature、无 PGO、无插桩
的普通 release 构建；`.bin` SHA256 为
`4d2e6656ff13684de1ae0202e25c150a027bc6ee504facd479cf9eed566c31a4`。
反汇编核对 `memset` 的零长度返回、逐字节写入和原指针返回；ELF 无
`__llvm_prf_*` section。`cargo fmt` 和 `git diff --check` 通过；
`cargo xtask clippy --package starry-kernel` 展开 72 组配置，前 10 组
通过后因磁盘不足主动停止，**不能报告全量 clippy 通过**。普通 release
尚未上板，未取得同源码 A/F full20，不能声称性能改善或 `<3%` 回退门通过。

## 2. 证据核验

从本目录执行 `sha256sum -c SHA256SUMS` 和 `python3 check.py`，可核对归档的
原始日志、状态、样本完整性和 `resume771` 的逐项分析。未纳入仓库的
完整镜像哈希保存在状态文件，
不能仅凭日志重建镜像身份。冻结 benchmark SHA256 为
`94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`。
