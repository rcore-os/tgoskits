# 最新源码四 crate PGO 训练镜像

## 1. 构建身份

训练基于 `dev@714accd8f636c540b2c3554b0b1e5cb885be42a4` 加既有
`memset` 训练修复的 `b292a098bb60ef604e7677c37cd95d926ff08200`，
固定 nightly-2026-09-04、`Cargo.lock` SHA256
`fb0273d9bd1b14d47604b070c419f4c76f32dcf45890d5bd23165b6ec2cf9f01`。
`profgen.toml` 关闭 cpufreq feature，仅额外启用训练专用
`starry-kernel/board-profile-export`。`exporter.patch.gz` 解压后是隔离工作树相对
`b292a098bb` 的完整临时差异，**未进入生产运行时源码**。

以共享 target 运行 `cargo xtask starry build -c profgen.toml` 已成功。
`wrapper.jsonl` 记录 `ax_sched`、`ax_task`、`ax_runtime`、`starry_kernel`
四个目标 crate 为 `instrumented=true`，最终 `starryos` 为 false。
`cargo fmt`、训练专用 exporter 的
`cargo xtask clippy --package starry-kernel` 76/76 以及差异检查通过。
`build.log.gz` 解压后只有上游 `core` 和 `memchr` 的 future-incompat 警告，
没有 profile 布局或编译失败警告。

训练 ELF SHA256 为
`64d70f907ead65419e75bb871046ff4b8a7f62a83cf03b95e8e8782a1da20d66`；
训练 `.bin` SHA256 为
`731bdb8f1bb694c221899654a380509236cc050f873e05238884af8c15aee1fb`。
二进制未入 Git，原件在本地实验归档。镜像含计数插桩和 exporter，
**不是无插桩候选，也没有上板或 full20 性能结果**。

## 2. 计数布局

`resume873-profile-layout.json` 从上述精确 ELF 的 `__llvm_prf_names`、
`__llvm_prf_cnts` 和 `__llvm_prf_data` 三段派生：18127 个函数记录，
计数段地址 `0xffffffff816fa440`，长度 789760 字节，LLVM 23.1.1
每条 data 记录 72 字节。后续板卡导出的计数必须匹配该长度及 ELF
身份，才能生成新的 profdata；不能混入旧源码或旧 feature 的 profile。

在本目录运行 `sha256sum -c SHA256SUMS` 可核对配置、临时补丁、
wrapper 记录、构建日志和布局原件。下阶段仍需板卡训练计数、无
exporter 的 profile-use 候选、三次有效无插桩 full20、同源码尾延迟
回退和 90% 门槛检查；本记录不改变 PR 的 Draft 状态。

两个 `.gz` 使用 `gzip -n` 保存原始日志和补丁。解压后的原件 SHA256
分别为 `da7b78c2bc71e6ae73327e331006473dddee87e426c0b7d545984507f28611a5`
和 `70af3a4680206ec63411cc84a6662d579906636933a710012bfdbef198f1714f`。
