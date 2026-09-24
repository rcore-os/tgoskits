# 当前源码 PGO 检查点

## 1. 构建身份

`build.py` 的 `source_gate()` 现在以 `dev@9a7b868bab8dcceb42acab516dcc88d5cc69985a` 为基线，`profile_feature_gate()` 要求普通 OrangePi release 的十一项 feature。训练源码 `4322e0c505bad73f10e7ab9e79f520bae0fcab0e` 与 PR #2477 变基 head `d1fb7568235c7b6c1ecea0a36b6db566da52ad59` 的 Git tree 均为 `897fb16b3903908476af83ecb16b122f2a16ed63`；后者没有运行时源码改动。

### 1.1 Profile

`resume710-status.json` 记录 QEMU 训练和构建身份，`resume710-warning-audit.json` 记录 CFG/hash 失配为零、未预期告警为空。仓库内 `full-pgo-2026-09-24.profdata.zst` 解压后 SHA256 为 `b916a666710826285845fbe017e70f9a665d423ee79df6ee4896368d2d837ec6`。`build.py` 的 `unpack_profile()` 对压缩包和解压内容分别校验哈希，`check_wrapper_log()` 确认 `ax_task`、`ax_runtime` 和 StarryOS 根 crate 均使用该 profile。

### 1.2 镜像

从全新 target 执行 `python3 scripts/perf/wakeup-pgo/build.py --output-dir <out>` 输出 `WAKEUP_PGO_IMAGE_READY`。原始 `resume734-build-result-before-status-rename.json` 显示 `.text` 为 7014528 字节、SHA256 `74084b8bdc93384f5dc91f3084eb77e2fdb70bd6fee0f3261a7ef73b77ea3e77`，与上板镜像相同；`.bin` 为 16932864 字节，除重新生成的 `.kallsyms` 外 SHA256 为 `0dd45557e31af1cc27b76133cad76e46ad84414f6526dfe1be4f057c071302d9`，也与上板镜像相同。原始 JSON 的 `status` 沿用旧的 70% 名称，构建后已将 `build.py` 的新报告状态更名为 `current_source_image_matched_unaccepted`；原始 JSON 未改写。`ready` 仅表示镜像身份匹配，**不表示性能验收**。

## 2. 板测边界

`resume711-abba-status.json` 与 `resume712-abba-status.json` 记录 OrangePi-5-Plus-1 的两次 F1/A1/A2/F2 计划顺序、镜像和冻结 benchmark 身份；两次序列都在普通 release 无效轮次后停止，均未执行 F2。原始日志均未裁剪；`SHA256SUMS` 可从仓库根目录校验本目录材料及新 profile。

### 2.1 有效观测

`resume711` F1 与 `resume712` F1 是两次独立有效的当前源码无插桩 PGO full20 启动：每轮 20 个唯一项、380000/380000 样本、零 `not_parked` 与 `missed_deadlines`。镜像 SHA256 `6a234ea391346f2450fdef8b04d19988211dbe3485cf62e3a25544ca4e80cac4`，冻结 benchmark SHA256 `94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`。两轮均有 19/20 项达到冻结 Linux RT 原始 p50 的 90%；OTHER `thread_futex_same_cpu` 分别为 11083/11375 ns，两轮中位 11229 ns，Linux RT 为 8458 ns，比值 75.32%。只有 `resume711` A1 是有效普通 release 启动，不能单独形成同源码回退验收。

### 2.2 未验收项

`resume711` A2 与 `resume712` A1 的 OTHER 同核 futex 均为 19999/20000 样本且 `not_parked=1`，两轮整体无效；两次序列的 F2 均未执行。故当前没有两次有效同源码 A/F full20，也没有三次有效候选 full20；不能判定所有 p50/p99/p99.9 回退 `<3%`，不能把两轮 19/20 视为最终 90% 结论。唯一可成对的 `resume711` A1→F1 中，FIFO `absolute_timer_same_cpu` p99.9 为 32417→42000 ns，候选高 29.56%，属于严重风险信号，但单对不能充当正式两轮回退判定。旧源码 2026-09-23 的 70% 两轮结果保存在相邻 `review-2477/full-pgo-2026-09-23/`，不与本目录拼接。
