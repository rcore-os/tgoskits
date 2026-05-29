# qperf 火焰图指南

## 生成

```bash
cargo starry perf --case boot --symbol-style full --max-depth 128
```

聚焦 virtio/block 路径：

```bash
cargo starry perf \
  --case blk-read \
  --qperf-metrics \
  --focus 'virtio|VirtQueue|block|memcpy|memmove' \
  --workload 'echo QPERF_BEGIN:blk; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; echo QPERF_END:blk' \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END
```

## 读哪些文件

* `qperf/flamegraph.svg`：完整 profile。
* `qperf/flamegraph.workload.svg`：marker window 内样本。
* `qperf/flamegraph.boot.svg`：start marker 前样本。
* `qperf/flamegraph.post.svg`：stop marker 后样本。
* `qperf/flamegraph.focus.svg`：`--focus` regex 命中的样本。
* `qperf/stack.folded`：原始 folded stack，保留完整 demangled Rust symbol。

## symbol style

| style | 用途 |
| --- | --- |
| `full` | 保留完整 Rust demangled path，适合归因和搜索 |
| `module` | 保留 crate 与尾部模块/函数，适合 SVG 可读性 |
| `short` | 只看函数尾名，适合粗略热点 |

SVG 内部会因为空间限制截断长 frame；需要确认完整 symbol 时看 `stack.folded`。

## 当前能看到什么

在已有 blk raw sample 上，新 analyzer 已能把 fallback symtab 名 demangle 成类似：

```text
<virtio_drivers::queue::VirtQueue<ax_driver::virtio::VirtIoHalImpl, 16usize>>::add_notify_wait_pop
compiler_builtins::int::specialized_div_rem::u128_div_rem
<ax_alloc::buddy_slab::GlobalAllocator>::alloc
```

这比旧报告中的 `_R...` mangled symbol 可读，但不等于完整调用链恢复。现有 qperf stack 由 frame pointer 链恢复；如果编译器省略 frame pointer、函数被 inline、发生 tail call 或进入汇编路径，栈会只剩一两个 frame。

## 长调用链演示

当前仓库中保留了两类火焰图副本：

* `docs/flamegraphs/qperf-long-chain-demo.synthetic.flamegraph.svg`
* `docs/flamegraphs/qperf-long-chain-real.virtio-blk-patched.flamegraph.svg`

第一份由 `tools/qperf/examples/long_chain_flamegraph_demo.py` 生成，用于演示
flamegraph 前端在 folded stack 已经包含长调用链时可以纵向展开到较深层级。它不是
真实 StarryOS profile 数据，不能用于性能结论。

第二份来自已有 virtio-blk folded stack，能反映当前真实 qperf 的调用栈恢复能力。它比
synthetic demo 更短，原因是当前 qperf 依赖 QEMU TCG 采样点和 guest frame pointer
链恢复调用栈；如果采样点落在缺少 frame pointer、inline、tail call 或汇编 trampoline
附近，analyzer 无法凭空补出父调用链。

## 调参建议

* 想提高时间粒度：提高 `--freq`，例如 `--freq 199`。
* 想提高栈深：提高 `--max-depth` 或使用 `--full-stack`。
* 想看更细 frame：使用 `--no-truncate`。
* 想看指令级采样点：使用 `--mode insn`，但开销更高。
* 想排除 boot 污染：加 `--start-marker`、`--stop-marker` 和 workload marker。

## 技术边界

当前 qperf 不能保证恢复截图级“完整 Rust 调用栈”。达到那一级别通常还需要：

* StarryOS kernel 编译时稳定保留 frame pointer。
* DWARF unwind 信息完整并可由 analyzer 使用。
* qperf raw sample 带 vCPU/thread/阶段元数据。
* QEMU plugin 或 guest-side unwinder 支持更可靠的 backtrace。
