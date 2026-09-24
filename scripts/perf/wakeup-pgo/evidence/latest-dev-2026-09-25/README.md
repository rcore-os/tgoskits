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

### 1.4 匹配 crate 身份的四 crate PGO 筛查

`resume785` 和 `resume786` 基于 `dev@05175ca388` 加相同的 AArch64
`memset` 修复（实验提交 `69a3365076`；对应 PR 提交 `34cb1a14a3` 的
`kprint.rs` 内容相同），并在**两侧**应用相同的临时 debugfs exporter
补丁（SHA256 `d7c1388dec5a849b1bec41e49b51d1f4cea6e2e853f1b4b2ce58d195a71d2b78`）。
两侧均使用去掉 cpufreq 的十项 OrangePi feature 加
`starry-kernel/board-profile-export`，保持 `starry_kernel` crate 身份一致。
训练 profile SHA256 为
`7d71a724fd5ce66e4faf15280995602392fc6ff8e6d41fc197cdbc0eb8e67156`；
候选仅对 `ax_task`、`ax_sched`、`ax_runtime`、`starry_kernel` 使用它。
普通与候选的 `.bin` SHA256 分别为
`750589429f5afe81b16775dde50050fedb936614b78d7bbde999a51856618473`、
`e48394c7ef55008d0badf2cd6dc841fa8fa6593e069720304e2400634614fd6d`。
两侧 `AXTEST_COVERAGE=1` 只使链接器提供空的 profile 起止符号；
镜像没有非空的 `__llvm_prf_*` 计数段，full20 无采样插桩。
候选不再出现 `starry_kernel` 热路径缺失 profile 的警告，但
`ax_runtime::run_idle` 仍有一项部分忽略警告。构建状态见 `resume785/`、
`resume786/`；临时 exporter **没有进入 PR**，因此这些实验镜像不能
直接由当前 PR 的生产构建入口复现或作为最终候选。

`resume787` 在 OrangePi-5-Plus-1 按 A1→F1 运行冻结 full20。A1 有效：
20 项、380000/380000 样本、零 `not_parked`。F1 的 OTHER
`thread_futex_same_cpu` 只有 19999/20000 样本，`not_parked=1`，
**整轮无效**；日志末尾的 `WAKEUP_LATENCY_PASSED` 不改变此判定。
`resume788` 独立重启同一候选 F2，20 项、380000/380000 样本、零
`not_parked` 和 `missed_deadlines`，是一次有效的候选 full20。

只把有效的 A1/F2 作为**单次探索性对照**：F2 有 11/20 项达到冻结
Linux RT p50 的 90%；最差 OTHER `thread_futex_same_cpu` 为 16625 ns，
Linux RT 为 8458 ns，比例 **50.88%**（A1 为 27417 ns）。该单次对照的
20 项 p50、p99、p99.9 均未见达到 3% 的回退，但缺少足够的有效启动，
不能宣称回退门通过。包括 FIFO timer、四项跨核 futex、OTHER timer、
OTHER yield handoff、OTHER 同核 futex 在内的九项仍低于 90%。F1 的
部分行数据只供诊断，不拼接进 F2 验收。完整逐项数值在 `analysis.json`
及原始日志中，`check.py` 逐项复算。

上述 PGO 阶段仅归档了构建与板测筛查，**没有可保留的新运行时逻辑优化**。
需先移除临时 exporter 对生产构建的依赖，重新建立可复现的同源码
普通/PGO 对照，处理无效轮次和九项缺口，并完成既定三次有效候选
full20 与 `<3%` 回退门；PR 保持草稿。

### 1.5 IRQ 描述符索引试验

`resume795` 在相同 `dev@05175ca388` 和早期 `memset` 修复上，只试验
`irq-framework` 的分发收尾：`Registry::begin_dispatch()` 返回描述符索引，
`DispatchGuard` 将它交给 `Registry::end_dispatch()`，省去一次线性查找。
源代码和并发注册测试保存在 `resume795/` 的补丁中，**未应用到 PR 的
运行时源码**。补丁采用零上下文格式，在其父提交 `69a3365076` 上需用
`git apply --unidiff-zero --check` 复核。普通 A1 与试验 B1 均关闭
cpufreq，使用相同的临时 exporter feature；均为无 PGO、无
`qperf-metrics` 的 release 镜像。两次独立启动
各完成 20 项、380000/380000 样本、零 `not_parked`，原始日志与镜像
SHA256 见 `resume795/results.json`。

同源码 A1/B1 中，OTHER 同核 futex p50 为 27417→26834 ns，改善 2.13%，
但 B1 仅 **10/20** 项达到冻结 Linux RT p50 的 90%。相对 A1，FIFO
`sched_yield_handoff` p99.9 回退 5.25%，OTHER `absolute_timer_same_cpu`
p99.9 回退 54.53%，OTHER `sched_yield_no_peer` p50 回退 11.09%。
试验已否决，不把单项 p50 改善计入可保留收益；一次配对足以否决该候选，
不能用于接受候选或证明最终回退门。`check.py` 从原始 full20 日志复算
样本完整性、20 项性能和上述回退。

## 2. 证据核验

从本目录执行 `sha256sum -c SHA256SUMS` 和 `python3 check.py`，可核对归档的
原始日志、状态、样本完整性及逐项性能和回退。未纳入仓库的
完整镜像哈希保存在状态文件，
不能仅凭日志重建镜像身份。冻结 benchmark SHA256 为
`94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`。
