# 最新源码 PGO 构建门禁审计

## 1. 源码身份

在 PR head `ac9f443d5ebc46cd542725787946abe1e7ecd474` 执行
`python3 scripts/perf/wakeup-pgo/build.py --prepare-only`，脚本输出
`build-result.json` 并以失败状态退出。`build.py` 将可复用范围锁定在
`dev@9a7b868bab8dcceb42acab516dcc88d5cc69985a`、训练提交
`4322e0c505bad73f10e7ab9e79f520bae0fcab0e` 和旧 profile；最新
head 有 28 个非排除源码路径差异，故 `ready=false`。旧 profile 的
cpufreq-on 配置也不是 816 MHz 比较所需的十项 feature 配置。

这次失败是预期的拒绝，不是构建器故障。它证明当前 PR 不能把旧
profile 直接用于最新源码；没有生成镜像，没有板卡运行或性能收益。

## 2. 后续训练

同源码训练计划基于 `dev@714accd8f636c540b2c3554b0b1e5cb885be42a4`
和已有 `memset` 修复提交 `b292a098bb60ef604e7677c37cd95d926ff08200`。
临时 `board-profile-export` 从精确插桩 ELF 导出计数器，选择性训练
`ax_task`、`ax_sched`、`ax_runtime`、`starry_kernel`。训练完成后再用
不含 exporter 的生产源码构建 profile-use 镜像，核对新 profile、
构建参数和最终上板机器码。未经三次有效无插桩 full20 及同源码
p50/p99/p99.9 回退检查，不将训练计数或旧 G1/G2 结果作为 90% 验收。

`sha256sum -c SHA256SUMS` 可核验本次构建器原始输出；本审计没有改动
`build.py` 或生产运行时源码。
