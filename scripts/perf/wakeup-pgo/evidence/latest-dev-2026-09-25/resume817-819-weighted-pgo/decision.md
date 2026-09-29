# 重新加权的四 crate PGO 筛查

## 1. 训练与构建

在 `dev@05175ca38823b631a73777b0130226ddfa558439` 加 AArch64 `memset`
前置修复 `69a33650763538692fafea27c869870ed0313642` 的源码上，沿用
`resume780` 原生训练镜像，SHA256 为
`45a77e44eca6fb458ce576735e1bebe316e78e1650894740be18de2469693917`。
冻结 benchmark SHA256 为
`94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`。
在 OrangePi-5-Plus-1 会话 `acd98c0d-dfb8-41a3-a579-f46f3e9e5fa9` 中，
训练工作负载运行完整 20 项一次，再额外运行 OTHER 同核线程 futex 七次。
`training-workload.log` 有 27 行结果，合计 17 个 `not_parked`；训练数据
**不用于性能验收**。计数区为 789760 字节，SHA256
`90687119fbb2ae5218855724980967efda0a5329df7a28f07a459d8cf72357ca`；
合并后的 profdata SHA256 为
`fcb20e25969c8eba19d0abb0ac105870b83257b6a9a3a535cd09b189e42d7b3a`。
本目录保留训练原始日志、工作负载脚本、状态和构建记录；计数二进制、
profdata 及镜像留在实验归档，未加入 PR。

候选与同源码普通 A1、旧 profile F2 使用相同十项 cpufreq-off feature，
两侧额外应用同一个临时 `board-profile-export` 补丁，SHA256
`d7c1388dec5a849b1bec41e49b51d1f4cea6e2e853f1b4b2ce58d195a71d2b78`。
`AXTEST_COVERAGE=1` 只提供链接符号。`wrapper.jsonl` 确认只有 `ax_sched`、
`ax_task`、`ax_runtime`、`starry_kernel` 使用新 profile；`starryos` 未使用。
`cargo xtask starry build -c build.toml` 成功，原始构建日志无损保存为
`build.log.gz`。候选 `.bin` SHA256
`9e9847a433cd99808d7f511372d454eb2cf5412a94e42187bcab64780ebfb8a3`，
ELF SHA256 `31313ba5988b7aa21165d97907435807eb29db7e406a81613ae43185e1599bac`。
构建无热 futex 函数缺失 profile 或 CFG 不匹配，仍有
`ax_runtime::run_idle` 的部分忽略警告。ELF 无非空 `__llvm_prf_*` 计数段，
板测不含 profile-generate 插桩。临时 exporter 已从实验源码撤回，
**本 PR 的 `build.py` 尚不能生产复现此候选**。

## 2. 两次完整板测

同一镜像在 OrangePi-5-Plus-1 独立启动两次，分别是会话
`2127299c-79d1-4e2a-b79f-7d0f86016618`（G1）和
`8424c03a-6e02-40e8-a630-de99671c171b`（G2）。两次均使用上述冻结
benchmark，各有 20/20 项、380000/380000 样本、零 `not_parked` 和
`missed_deadlines`。原始 full20 日志 SHA256 分别是
`34814808115915e9597399405c57b9028d1228006e0681d632bbf2a14f4f701f`
和 `6db2164fbf9e732d2e1c4a9baa2d3745ca8bf990df43c82b994eb51ac701cce4`。
本目录保留两次完整串口、full20 日志、状态及逐项复算 JSON；
`../check.py` 从原始行重算有效性、中位数和回退。

G1/G2 的逐项 p50 中位数有 **11/20** 项达到冻结 Linux RT 的 90%。
最差 OTHER `thread_futex_same_cpu` 为 **14583.5 ns**，对 Linux RT
8458 ns 是 **58.00%**，距 90% 所需的至多 9397 ns 仍差 5186.5 ns。
旧 profile F2 单次有效值为 16625 ns；这说明重训权重对该项有局部改善，
不是 20 项整体过关，也不是与多次旧 profile 配对后的因果估计。
OTHER timer 为 40625.5 ns，对 RT 仅 58.88%；FIFO 同核 futex 为
11958 ns，对 RT 为 70.73%。完整九项未达值见 `analysis.json`。

与**仅一次**同源码普通 A1 对比，20 项 p50、p99、p99.9 没有达到 3%
的回退；这不能代替既定的多次普通启动回退门。与旧 F2 对比，FIFO timer
p99.9 增加 6.32%，OTHER 进程/线程跨核 futex p99.9 分别增加
18.11%/22.36%；不能只报告最差 p50 的下降。频率采用与 A1/F2 相同的
cpufreq-off 配置和约 816 MHz 独立 PLL 检查，未在 full20 窗口逐项同步采频。

## 3. 判定

**拒绝提升为 PR 验收候选。** 九项仍低于 90%，只有两次有效候选启动，
普通 A 只有一次有效启动，临时 exporter 又未进入生产构建。此次 PR
只归档可复算的阶段证据，没有新调度器、futex 或其他运行时逻辑改动，
不宣称已达成 issue #2308 的最终目标，PR 继续保持 Draft。
