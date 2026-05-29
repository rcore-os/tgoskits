# qperf 深调用栈火焰图设计说明

## 1. 问题背景

旧版 qperf 生成的 `qperf/stack.folded` 大多只有一层函数名，因此 `flamegraph.svg` 只能横向展示 leaf hotspot，几乎没有纵向调用链。典型检查命令如下：

```bash
awk -F';' '{print NF}' target/qperf-host-rerun/blk-harness/perf/riscv64/latest/qperf/stack.folded | sort -n | uniq -c
```

旧 blk 产物的结果为 `623 1`，说明 623 条 folded stack 全部只有一帧。结合 `resolve.stats.json` 中 `format_version=2`、`raw_records=1292`、`total_frames=1292`，可以判断当时 raw sample 本身只有 PC/TB leaf 地址，不是 analyzer 把多帧调用栈压扁了。

## 2. 根因诊断

本轮诊断结论如下：

| 类别 | 结论 |
| --- | --- |
| raw sample 是否有 callchain | 旧格式没有，只有 elapsed timestamp 与 PC leaf。 |
| analyzer 是否压扁多帧 | 未发现多帧被压扁的问题；问题主要在采样侧没有多帧数据。 |
| QEMU plugin 是否读寄存器 | 旧执行回调使用 `QEMU_PLUGIN_CB_NO_REGS`，无法稳定读取 guest SP/FP。 |
| RISC-V FP 寄存器别名 | QEMU 暴露 `sp`、`fp`，同时也可能有 `x2`、`s0`、`x8` 别名；旧实现没有完整候选别名。 |
| 构建参数 | 旧 `--full-stack` 没有把 `-Cforce-frame-pointers=yes` 稳定传入实际 StarryOS kernel 构建。 |
| debug info | 只保留 leaf symbol 时还能解析函数名，但深栈与 inline 展开需要 `debuginfo=2` 与不剥离符号。 |

因此，火焰图浅的核心原因不是 SVG 展示参数、采样频率或 `--max-depth`，而是 qperf 采样链路没有拿到可 unwind 的 guest callchain。

## 3. 实现方案

本轮采用真实 frame-pointer callchain 方案，默认仍保留 leaf 模式：

| 模式 | 说明 |
| --- | --- |
| `leaf` | 默认模式，只记录 PC leaf，开销低、兼容旧命令。 |
| `fp` | 通过 QEMU TCG plugin 读取 guest `pc/sp/fp`，按 RISC-V frame pointer 链恢复调用栈。 |
| `logical` | 预留 CLI 值，但当前不实现；如果后续真实 unwind 不够稳定，再做人工插桩逻辑栈。 |

`cargo starry perf --full-stack` 等价于：

* 启用 qperf `callchain=fp`。
* 为 StarryOS kernel 构建加入 `-Cdebuginfo=2`、`-Cstrip=none`、`-Cforce-frame-pointers=yes`。
* 启用 `DWARF=y`、`BACKTRACE=y`，方便符号和 debug 信息保留。

## 4. raw sample v3

qperf plugin 新增 v3 sample 格式：

```text
elapsed_ns
pc
sp
fp
cpu
callchain
trace[]
```

其中：

* `pc` 是采样点 leaf PC。
* `sp` 是 guest stack pointer。
* `fp` 是 RISC-V `s0/fp`。
* `trace[]` 是 plugin 通过 frame pointer unwind 得到的地址链。
* `callchain=leaf|fp` 标记该样本来源。

analyzer 继续兼容旧 v1/v2 raw 格式；旧数据会自动退化为 leaf-only 栈。

## 5. RISC-V frame pointer unwind

RISC-V ABI 下，开启 frame pointer 后通常可从当前 `fp` 附近读取上一个 frame pointer 与 return address。本轮实现按如下策略恢复：

1. QEMU execute callback 使用 `QEMU_PLUGIN_CB_R_REGS`。
2. 采样时读取 guest `sp` 与 `fp`，寄存器候选包括 `sp/x2` 与 `fp/s0/x8`。
3. 只对 kernel text 或其物理映射别名范围内的 PC 做 unwind，避免在 OpenSBI 或用户态地址上误读。
4. 按 `fp - 16` 读取 `{prev_fp, ra}`。
5. 遇到非法地址、非单调 frame pointer、距离过大、循环 frame、不可读内存或非 kernel text return address 时停止。
6. 单个样本 unwind 失败时回退到 leaf，不中断整个 profile。

输出新增：

* `qperf/stack-depth-summary.csv`
* `report.json.callchain`
* `report.json.resolve_stats.depth_histogram`
* `qperf/summary.txt` 中的 sample format 与 callchain 字段

## 6. analyzer 与 folded stack

analyzer 的变化：

* 保留 raw sample 中的多帧地址，不再只按 leaf 聚合。
* folded stack 按 caller-to-leaf 顺序输出。
* `stack.folded` 默认保留完整 demangled Rust 路径。
* `hotspots.csv` 继续提供函数热点聚合，但函数百分比按函数帧总量计算，避免深栈下出现超过 100% 的函数占比。
* `hotspot_categories.csv` 是 inclusive stack 分类，表示某类别在多少条样本调用栈中出现，不是互斥 CPU 时间分摊。

## 7. cargo starry 集成

新增或完善的参数：

```text
cargo starry perf --full-stack
cargo starry perf --perf-callchain leaf|fp|logical
cargo starry perf --perf-debuginfo
cargo starry perf --perf-force-frame-pointers
cargo starry perf --symbol-style full|module|short
cargo starry perf --max-depth 128
```

推荐深栈用法：

```bash
cargo starry perf \
  --case blk-full-stack \
  --full-stack \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --workload 'echo QPERF_BEGIN:blk; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; echo QPERF_END:blk'
```

默认 `cargo starry perf` 仍使用 `leaf`，避免对普通 profile 强制引入 frame pointer 与 debug info 的构建开销。

## 8. 局限性

* `fp` 模式依赖 kernel 编译时保留 frame pointer；没有 `--full-stack` 时不能期待深栈。
* 目前只解析 kernel ELF，用户态符号和用户栈不是本轮目标。
* trap/syscall/task 切换附近的栈可能截断，不能保证穿透所有上下文切换。
* inline 展开会让 `stack.folded` 的符号层数大于 raw FP frame 数；原始地址深度以 `stack-depth-summary.csv` 为准。
* QEMU 退出如果被 SIGKILL 截断，plugin shutdown summary 可能缺失；analyzer 仍可处理已落盘 raw sample。
* `logical` 模式尚未实现，当前不会把人工 instrumentation 路径伪装成真实 CPU callchain。
