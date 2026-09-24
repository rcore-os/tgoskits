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

`resume711` A2 与 `resume712` A1 的 OTHER 同核 futex 均为 19999/20000 样本且 `not_parked=1`，两轮整体无效；两次序列的 F2 均未执行。后续 `resume742` 补得两次有效普通 A（见第 3 节），但未补得第三次有效 PGO 候选启动；两轮 19/20 不能视为最终 90% 结论。旧源码 2026-09-23 的 70% 两轮结果保存在相邻 `review-2477/full-pgo-2026-09-23/`，不与本目录拼接。

## 3. 单 waiter 候选复测

`resume742/` 保存同一源码 `b59894344a23786d7b5ba57b56eabeb716eb1ba6` 的普通 release 对照与试验性 futex 快路径结果。`candidate.patch.gz` 解压后的补丁仅改变 `ResolvedFutex::wake()` 的单 waiter 分支和新增的 `collect_single_futex_wake()`；该补丁已被拒绝，**没有应用到本 PR 的运行时代码**。冻结 benchmark SHA256 为 `94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`。

### 3.1 镜像和有效性

普通 A 与候选 B 都从本 PR 精确源码编译，使用同一十一项 OrangePi 配置、不启用 PGO 或插桩。A 镜像 SHA256 为 `58801abbba44d6dd61a2807efc62b678274586e075e31beca7fce9b913f84eca`，B 为 `c83b426636bc9d2b672087a773ca9e26cc26a7589e12848c388ba033e9ecebf7`。重新编译的 A `.text` SHA256 `553c78302dc1621c008e17a9390e4e65488108d56b407feed1bb9c250fed16af` 与旧归档普通镜像一致；**重新编译的 A 与旧归档普通镜像**的 `.bin` 除 `.kallsyms` 外逐字节相同，因此旧镜像机器码可复用，但本轮仍重新上板采普通 A。

OrangePi-5-Plus-1 两次租约依次运行 A1/B1/B2 与 A2/B3。A1、B1、A2、B3 均为有效无插桩 full20：各 20 项、380000/380000 样本、零 `not_parked` 与 missed deadline。B2 的 OTHER `thread_futex_same_cpu` 为 19999/20000、`not_parked=1`，整轮无效并原样归档，不参与中位数。`session.json`、`session2.json` 保存租约；两次租约均已释放。`status.json` 记录镜像和原始日志哈希；运行 `python3 scripts/perf/wakeup-pgo/evidence/current-dev-2026-09-24/resume742/check.py` 可重验完整性和比较数值。

### 3.2 结果和决定

有效轮次的逐项 p50、p99、p99.9 使用两次原始值的中位数比较。B 的 OTHER 同核 futex p50 从 A 的 18812.5 降至 18229.5 ns（延迟降低 3.10%），但 OTHER `process_futex_cross_cpu` p99.9 从 52937.5 增至 66209 ns（+25.07%）、p99 从 25958.5 增至 27708.5 ns（+6.74%），FIFO `sched_yield_handoff` p99.9 从 3645.5 增至 3792 ns（+4.02%）。三项超过 `<3%` 回退门，故拒绝该运行时补丁；单项 p50 改善不能用于声称整体性能提升。

新增两次有效普通 A 也允许对现有两次同源码 PGO F 重新检查回退：FIFO `absolute_timer_same_cpu` p99.9 的 A 中位数 32583.5 ns、F 中位数 44625 ns，候选高 **36.96%**。A 与 F 不构成交错 A/F/A/F 四轮，但该尾部风险已明确超过门限；现有 PGO 的 19/20 项 90% 结果仍只是阶段观测，不能请求验收或合入。
