# 当前源码五 crate PGO 与启动门禁

## 1. 广覆盖训练的启动失败

### 1.1 启动依赖插桩

`resume880` 在 `b292a098bb` 源码上排除了 `someboot` 等早期 crate，但镜像仍在 U-Boot 的 `Starting kernel ...` 之后 300 秒无输出。`resume881-readonly-audit.txt` 对该 ELF 的只读核对发现，`fdt_raw`、`log` 等被插桩依赖经 fat LTO 进入 `someboot` 的 MMU 启用前函数；`someboot::fdt::earlycon::setup_earlycon` 内存在访问 profile 计数器的 `ldxr/stxr`。`resume880` 无 ESR/FAR，精确停止位置仍是推断，不能把该反汇编结果写成已观测故障 PC。

### 1.2 函数边界遗漏

`resume882` 根据上述记录属主排除 `fdt_raw`、`log`、`ax_alloc` 后，`check-early-counters.py` 对已命名的 `someboot`/`ax_cpu` 函数报告零计数器站点。板卡因此越过早期串口设置，却在 `Memory Map:` 后 300 秒无进展。`someboot::mem::print_memory_map()` 随后调用被插桩的 `byte_unit::Byte::get_appropriate_unit()`；函数内仍有 profile 计数器 `ldxr/stxr`。这说明仅检查调用者自身不构成完整的预 MMU 安全门禁；由于没有新的 ESR，具体停止指令仍未证实。两次训练均未得到可用 profile 或 full20。

## 2. 五 crate 原生 PGO 筛查

### 2.1 构建身份

`resume883` 从已能上板的四 crate 范围出发，只增补 `ax_sync`，其余仍为 `ax_sched`、`ax_task`、`ax_runtime`、`starry_kernel`。`wrapper.py`、`pgouse-wrapper.py` 与两份 TOML 记录训练和使用两侧的 crate 选择；顶层 binary 的临时 exporter 不参加 profile。训练使用关闭 cpufreq 的同一十项 feature 和同一工作负载，ELF 含 18170 条 LLVM IR 记录及 790272 字节计数段；`run1/result.json` 记录 27 个训练行。profile 原件 SHA256 为 `92fb238c9f0decd0ee1dcf16c4ef2dc64b846924c4f298d884cf1dad9b3b5fd2`，本目录保存其 zstd 压缩件。训练 ELF/bin SHA256 分别为 `98b6bbe38f07db02cd014376f880c311f87e36a5129636acb51039a4c00ccae9`、`7450c7ccafe64b6795a0b95f51c168f5a54852b1295c0f1572b554cba744c999`。

无插桩 profile-use ELF/bin SHA256 分别为 `a91e672f828a9eb2d58c7fa2dd3426937beed61578e23b9b9c1eb223b9deaa3f`、`e2b96fde90132a7c678a511dbd4d675b40c3743902b2e6ecfd6b1744d4178969`。ELF 不含 `__llvm_prf_*` 节。`build-pgouse.log` 只有与四 crate 训练相同的 `ax_runtime::run_idle` 部分 profile 忽略警告，不能据此推断热路径全部匹配。镜像原件仅在本地实验目录，未提交到 PR；最终生产构建复现门禁尚未通过。

### 2.2 full20 结果

OrangePi-5-Plus-1 在两次独立启动中运行同一无插桩候选。`F1-full.log`、`F2-full.log` 各有 20 个唯一项、380000/380000 样本、零 `not_parked` 与 `missed_deadlines`；冻结 benchmark SHA256 为 `94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`。同一源码与频率设置下，按两次 p50 中位数计算，**11/20** 项达到冻结 Linux RT 的 90%，相比 `resume876` 四 crate 的 10/20 仅增加一项。最差 OTHER `thread_futex_same_cpu` 仍为 14583 ns，对冻结 RT 8458 ns 为 **57.999%**；另外八项也未达 90%。`analyze_full20.py` 可重算每项和样本有效性。

这是被拒绝的探索候选，不能以跨候选、不同启动的差值声称稳定收益。第三次有效候选启动、同源码普通 release 全部 p50/p99/p99.9 回退小于 3%、生产构建复现和新 head CI 都未证明，PR 继续 Draft。`resume880` 至 `resume883` 的台账记录连同原始串口、配置、日志、profile 和比较结果保留于此；未把临时 exporter 合入生产源码。
