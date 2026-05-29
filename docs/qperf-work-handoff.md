# qperf / StarryOS 性能工具链交接文档

本文面向后续接手的工程 agent，用于快速理解本轮 qperf 工具链改造、验证实验、virtio-blk 性能瓶颈定位与优化结果。后续可直接基于本文撰写总结报告或制作 PPT。

## 1. 当前状态

| 项目 | 内容 |
| --- | --- |
| 仓库 | `/home/cg24/tgoskits` |
| 当前分支 | `fix/starry-syscall-harness` |
| 已推送远端 | `origin/fix/starry-syscall-harness` |
| 最新提交 | `2cfa4e3f5 perf(virtio-blk): reduce qperf sync read queue churn` |
| 主要目标 | 把 qperf 从“能出火焰图的采样器”改造成能支撑 StarryOS virtio-blk/net/vsock 性能归因、A/B 验证和深栈火焰图分析的工具链 |

本轮工作包含两类成果：

1. qperf 工具链能力建设：marker/window、工程分类、virtio counters、A/B compare、`cargo starry perf` 集成、RISC-V frame-pointer 深调用栈火焰图。
2. 基于新版 qperf 的 virtio-blk 瓶颈分析与一个最小优化：rsext4 data block readahead + virtio-blk direct read fast path。

## 2. 关键提交

| 提交 | 标题 | 主要内容 |
| --- | --- | --- |
| `ea91bd04f` | `feat(qperf): add workload-aware profiling metrics` | workload marker/window、workload stdout parser、工程热点分类、virtio-aware counters、`/proc/qperf_metrics`、`perf-compare`、qperf 验收报告 |
| `34d0e92d5` | `feat(qperf): integrate cargo starry profiling workflow` | `cargo starry perf`、`cargo starry run --perf`、默认 qperf 参数、火焰图输出整理、smoke 脚本、集成文档 |
| `d94a25be7` | `feat(qperf): add RISC-V callchain flamegraphs` | qperf raw sample 扩展 PC/SP/FP、RISC-V frame-pointer unwinder、深栈 folded stack、stack depth summary、callchain 文档与验证 |
| `2cfa4e3f5` | `perf(virtio-blk): reduce qperf sync read queue churn` | 基于 qperf 结果实现 virtio-blk direct read fast path、rsext4 readahead、blk A/B 报告和 PNG 火焰图 |

## 3. 工具链改造成果总览

### 3.1 harness / workload window

主要文件：

* `tools/starry-syscall-harness/harness.py`
* `tools/starry-syscall-harness/README.md`
* `tools/starry-syscall-harness/mcp_server.py`
* `tools/starry-syscall-harness/ui_server.py`
* `tools/starry-syscall-harness/web/*`

新增能力：

* `perf-profile` 支持 `--start-marker`、`--stop-marker`、`--workload-timeout`。
* guest stdout 出现 marker 后，报告中记录 workload window。
* 当前采样启停仍主要是 postprocess 层的 timestamp 过滤，报告中明确标注 `method: qperf_raw_elapsed_timestamp_filter`。
* `report.json` 增加 `window` 字段：`start_marker`、`stop_marker`、`start_time`、`stop_time`、`duration_sec`、`boot_samples_excluded`、`post_window_samples_excluded`、`truncated_by_timeout`、`warnings`。
* marker 缺失时报告 warning，避免把 boot 成本静默混入 workload 数据面结论。
* workload 结束后 harness 侧通过 QMP quit / 进程终止机制主动收尾，减少对 guest `poweroff -f` 的依赖。

### 3.2 qperf postprocess / 工程归因指标

主要文件：

* `tools/qperf/analyzer/src/main.rs`
* `tools/qperf/src/profiler.rs`
* `tools/starry-syscall-harness/harness.py`

保留的基础产物：

* `report.json`
* `report.md`
* `hotspots.csv`
* `hotspot_categories.csv`
* `qperf/stack.folded`
* `qperf/flamegraph.svg`
* `qperf/summary.txt`

新增能力：

* 工程热点分类：`virtqueue_add_notify_wait_pop`、`virtqueue_add`、`virtqueue_pop_complete`、`virtio_notify_kick`、`memcpy`、`memmove`、`allocator`、`scheduler_wait_preempt`、`lock_mutex_wait`、`pci_probe_transport`、`net_inflight_btree`、`block_io_path`、`net_rx_tx_path`、`vsock_tx_rx_path`。
* workload stdout parser：
  * 解析 `dd` 的 bytes、elapsed、throughput。
  * 解析 `wget` 的下载状态和可获得的吞吐信息。
  * 解析 `QPERF_METRIC key=value` 自定义指标。
* 归一化指标：
  * `guest_instructions_per_MB`
  * `guest_blocks_per_MB`
  * `host_elapsed_sec_per_MB`
  * `samples_per_MB`
  * `category_samples_per_MB`
* host perf：
  * 启用 `--host-perf` 时合并 `perf stat`。
  * 未启用时在报告中明确写出“未启用 host perf”。

### 3.3 virtio-aware counters

主要文件：

* `drivers/ax-driver/Cargo.toml`
* `drivers/ax-driver/src/lib.rs`
* `drivers/ax-driver/src/qperf_metrics.rs`
* `drivers/ax-driver/src/virtio/block.rs`
* `drivers/ax-driver/src/virtio/net.rs`
* `os/StarryOS/kernel/src/pseudofs/proc.rs`
* `os/StarryOS/kernel/Cargo.toml`
* `os/StarryOS/starryos/Cargo.toml`

新增能力：

* feature-gated instrumentation：`qperf-metrics` 默认关闭。
* `/proc/qperf_metrics` 支持：
  * `cat /proc/qperf_metrics` 导出 `QPERF_METRIC key=value ...`
  * `echo reset > /proc/qperf_metrics` 重置 counters
* virtio-blk counters：
  * `virtio_blk_read_requests`
  * `virtio_blk_read_bytes`
  * `virtio_blk_write_requests`
  * `virtio_blk_write_bytes`
  * `virtio_blk_direct_read_requests`
  * `virtio_blk_direct_read_bytes`
* virtqueue counters：
  * `virtqueue_add_count`
  * `virtio_notify_kick_count`
  * `virtqueue_pop_complete_count`
  * `virtqueue_add_notify_wait_pop_count`
  * `virtqueue_depth_max`
  * `virtqueue_depth_hist_*`
* virtio-net counters：
  * `virtio_net_rx_packets`
  * `virtio_net_tx_packets`
  * `virtio_net_rx_bytes`
  * `virtio_net_tx_bytes`
  * `virtio_net_rx_copy_within_count`
  * `virtio_net_rx_copy_within_bytes`
  * `virtio_net_tx_staging_copy_count`
  * `virtio_net_tx_staging_copy_bytes`
  * `virtio_net_inflight_insert_count`
  * `virtio_net_inflight_remove_count`
  * `virtio_net_inflight_get_count`

注意：这些 counters 是 driver-visible 近似统计，不是 virtqueue ring-level 精确硬件统计。

### 3.4 A/B compare

主要文件：

* `tools/starry-syscall-harness/harness.py`

命令：

```bash
python3 tools/starry-syscall-harness/harness.py perf-compare \
  --baseline <baseline-report.json> \
  --candidate <candidate-report.json> \
  --name <case-name> \
  --output-dir <output-dir>
```

输出：

* `compare.json`
* `compare.md`
* `compare.csv`

覆盖内容：

* workload throughput / elapsed
* guest executed instructions / blocks
* host elapsed / user / sys
* hotspot categories
* virtio counters
* copy bytes
* notify/kick count
* queue depth 字段

缺失字段显示 `N/A`，不会 crash。

### 3.5 `cargo starry perf` / `cargo starry run --perf`

主要文件：

* `scripts/axbuild/src/starry/mod.rs`
* `scripts/axbuild/src/starry/perf.rs`
* `scripts/axbuild/src/context/workspace.rs`
* `scripts/axbuild/src/lib.rs`
* `tools/starry-syscall-harness/scripts/qperf-smoke.sh`

新增命令：

```bash
cargo starry perf --case boot
```

blk 示例：

```bash
cargo starry perf \
  --case blk-read \
  --qperf-metrics \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --workload 'echo reset > /proc/qperf_metrics; echo QPERF_BEGIN:blk; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; cat /proc/qperf_metrics; echo QPERF_END:blk'
```

`run --perf` 示例：

```bash
cargo starry run \
  --perf \
  --perf-case blk-read \
  --perf-workload 'echo QPERF_BEGIN:blk; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; echo QPERF_END:blk' \
  --perf-start-marker QPERF_BEGIN \
  --perf-stop-marker QPERF_END \
  --perf-qperf-metrics
```

默认输出路径：

```text
target/qperf/<case>/perf/<arch>/latest/
```

典型产物：

```text
report.md
report.json
hotspots.csv
hotspot_categories.csv
profile.stdout
profile.stderr
qperf/flamegraph.svg
qperf/flamegraph.workload.svg
qperf/flamegraph.boot.svg
qperf/flamegraph.post.svg
qperf/flamegraph.focus.svg
qperf/stack.folded
qperf/stack-depth-summary.csv
qperf/summary.txt
qperf/resolve.stats.json
```

### 3.6 深调用栈火焰图

主要文件：

* `tools/qperf/src/profiler.rs`
* `tools/qperf/src/reg.rs`
* `tools/qperf/analyzer/src/main.rs`
* `scripts/axbuild/src/starry/perf.rs`
* `tools/starry-syscall-harness/harness.py`

新增能力：

* qperf raw sample 记录 RISC-V `pc`、`sp`、`fp/s0`。
* analyzer 支持基于 frame pointer 的 RISC-V kernel callchain 恢复。
* `--full-stack` 自动启用 debuginfo、force-frame-pointers 和 `--perf-callchain fp`。
* 支持 `--perf-callchain leaf|fp|logical`。
* 支持 `--symbol-style full|module|short`。
* folded stack 默认保留完整 demangled Rust 路径。
* 新增 `qperf/stack-depth-summary.csv`。
* 报告中新增 `callchain` 字段：`enabled`、`method`、`samples_with_fp`、`unwind_success`、`unwind_failed`、`avg_depth`、`max_depth`。

深栈能力的关键结论：

* 旧火焰图浅的根因是 qperf 主要记录 guest PC/TB leaf hotspot，没有 callchain。
* 新版在 `--full-stack` 下可以生成明显纵向展开的调用栈，例如 syscall -> fs -> rsext4 -> ax-driver -> virtio -> virtqueue。
* 默认模式仍是 `leaf`，避免普通 profile 强制引入 frame pointer / debuginfo 的开销。

## 4. virtio-blk 瓶颈定位与优化

### 4.1 采样命令

baseline：

```bash
cargo starry perf \
  --case blk-baseline \
  --output-dir target/qperf-virtio-blk-opt/baseline \
  --full-stack \
  --qperf-metrics \
  --host-time \
  --timeout 240 \
  --workload-timeout 160 \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --shell-init-cmd 'echo reset > /proc/qperf_metrics; echo QPERF_BEGIN:blk; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; cat /proc/qperf_metrics; echo QPERF_END:blk' \
  --no-truncate
```

candidate：

```bash
cargo starry perf \
  --case blk-direct-readahead \
  --output-dir target/qperf-virtio-blk-opt/candidate-readahead \
  --full-stack \
  --qperf-metrics \
  --host-time \
  --timeout 240 \
  --workload-timeout 160 \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --shell-init-cmd 'echo reset > /proc/qperf_metrics; echo QPERF_BEGIN:blk; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; cat /proc/qperf_metrics; echo QPERF_END:blk' \
  --no-truncate
```

compare：

```bash
python3 tools/starry-syscall-harness/harness.py perf-compare \
  --baseline target/qperf-virtio-blk-opt/baseline/perf/riscv64/latest/report.json \
  --candidate target/qperf-virtio-blk-opt/candidate-readahead/perf/riscv64/latest/report.json \
  --name blk-readahead \
  --output-dir target/qperf-virtio-blk-opt/compare-readahead
```

### 4.2 baseline 结论

baseline 报告：

* `target/qperf-virtio-blk-opt/baseline/perf/riscv64/latest/report.json`
* `target/qperf-virtio-blk-opt/baseline/perf/riscv64/latest/report.md`
* `target/qperf-virtio-blk-opt/baseline/perf/riscv64/latest/qperf/flamegraph.svg`
* `target/qperf-virtio-blk-opt/baseline/perf/riscv64/latest/qperf/stack.folded`
* `target/qperf-virtio-blk-opt/baseline/perf/riscv64/latest/qperf/stack-depth-summary.csv`

关键数据：

| 指标 | baseline |
| --- | ---: |
| dd bytes | 53,601,104 |
| dd elapsed | 6.367021 s |
| throughput | 8,418,553 B/s |
| marker window | 6.529766005 s |
| samples | 642 |
| callchain avg symbol depth | 43.306853583 |
| raw max depth | 16 |
| virtio_blk_read_requests | 13,629 |
| virtio_blk_read_bytes | 55,813,632 |
| virtqueue_add_count | 14,417 |
| virtio_notify_kick_count | 14,417 |
| virtqueue_add_notify_wait_pop_count | 14,354 |
| virtqueue_pop_complete_count | 14,354 |

baseline 工程热点：

| category | percent |
| --- | ---: |
| block_io_path | 75.8567% |
| virtio_notify_kick | 32.8660% |
| virtqueue_add_notify_wait_pop | 31.9315% |
| memcpy | 28.1931% |
| lock_mutex_wait | 10.5919% |

qperf 识别出的核心瓶颈：

* 53.6 MB 顺序读触发 13,629 次 blk read request，平均每次约 4 KiB。
* 每个小 read 基本都走一次同步 `VirtQueue::add_notify_wait_pop`。
* `virtio_notify_kick_count` 和 `virtqueue_add_notify_wait_pop_count` 与 request 数同量级。
* 深栈火焰图确认路径是：sys_read -> axfs/rsext4 -> ax-driver block -> rd-block -> virtio-blk -> virtqueue。

### 4.3 优化实现

最终优化涉及文件：

* `components/rsext4/src/cache/data_block.rs`
* `drivers/interface/rdif-block/src/lib.rs`
* `drivers/blk/rd-block/src/lib.rs`
* `drivers/ax-driver/src/block/binding.rs`
* `drivers/ax-driver/src/virtio/block.rs`
* `drivers/ax-driver/src/qperf_metrics.rs`

实现内容：

1. `rdif_block::IQueue` 增加默认 `read_blocks_direct()`，默认返回 `BlkError::NotSupported`，保证旧 driver 兼容。
2. `rd_block::CmdQueue` 增加 `read_blocks_direct()` 转发。
3. `ax_driver::block::Block::read_block()` 优先尝试 direct read，只有 `NotSupported` 时回退原路径。
4. `ax_driver::virtio::block::BlockQueue` 实现 direct read：
   * 只在目标 buffer 物理连续时启用。
   * 使用 `axklib::mem::virt_to_phys` 检查 4 KiB 页物理连续性。
   * 调用 `VirtIOBlk::read_blocks()` 直接 DMA 到目标 buffer。
5. `drivers/ax-driver/src/qperf_metrics.rs` 增加 direct read counters。
6. `rsext4::cache::data_block::DataBlockCache` 在 miss 时做最多 8 个连续 data block 的 readahead：
   * 使用 `Jbd2Dev::read_blocks()` 批量读取。
   * 拆成 `CachedBlock` 插入 cache。
   * 遇到已缓存 block 提前停止。
   * 遵守 cache 容量与 LRU 驱逐。

direct-only 曾作为尝试，但 A/B 显示退化：

| 指标 | baseline | direct-only |
| --- | ---: | ---: |
| throughput | 8,418,553 B/s | 6,889,660 B/s |
| workload elapsed | 6.367021 s | 7.779933 s |
| `virtqueue_add_notify_wait_pop_count` | 14,354 | 14,354 |

结论：direct-only 没有减少同步 virtqueue 次数，因此不是有效优化；最终有效的是 readahead 降低同步小 I/O 数量。

### 4.4 A/B 结果

最终 compare：

* `target/qperf-virtio-blk-opt/compare-readahead/perf-compare/blk-readahead/compare.md`
* `target/qperf-virtio-blk-opt/compare-readahead/perf-compare/blk-readahead/compare.csv`
* `target/qperf-virtio-blk-opt/compare-readahead/perf-compare/blk-readahead/compare.json`

compare 结论：`明显改善`。

| 指标 | baseline | candidate | 变化 |
| --- | ---: | ---: | ---: |
| throughput_bytes_per_second | 8,418,553 | 10,551,346 | +25.3344% |
| workload elapsed | 6.367021 s | 5.080025 s | -20.2135% |
| samples.total_samples | 642 | 528 | -17.7570% |
| virtio_blk_read_requests | 13,629 | 2,111 | -84.5110% |
| virtio_notify_kick_count | 14,417 | 2,899 | -79.8918% |
| virtqueue_add_count | 14,417 | 2,899 | -79.8918% |
| virtqueue_add_notify_wait_pop_count | 14,354 | 2,836 | -80.2424% |
| virtqueue_pop_complete_count | 14,354 | 2,836 | -80.2424% |

hotspot category 对比：

| category | baseline | candidate | delta |
| --- | ---: | ---: | ---: |
| virtio_notify_kick | 32.8660% | 14.2045% | -18.6615 pp |
| virtqueue_add_notify_wait_pop | 31.9315% | 13.4470% | -18.4845 pp |
| block_io_path | 75.8567% | 66.2879% | -9.5688 pp |
| memcpy | 28.1931% | 31.0606% | +2.8675 pp |
| lock_mutex_wait | 10.5919% | 10.4167% | -0.1752 pp |

candidate 报告：

* `target/qperf-virtio-blk-opt/candidate-readahead/perf/riscv64/latest/report.json`
* `target/qperf-virtio-blk-opt/candidate-readahead/perf/riscv64/latest/report.md`
* `target/qperf-virtio-blk-opt/candidate-readahead/perf/riscv64/latest/qperf/flamegraph.svg`
* `target/qperf-virtio-blk-opt/candidate-readahead/perf/riscv64/latest/qperf/stack.folded`
* `target/qperf-virtio-blk-opt/candidate-readahead/perf/riscv64/latest/qperf/stack-depth-summary.csv`

注意：candidate `report.json` 的 `result` 是 `incomplete`，原因是 stop marker 后 QEMU 收尾阶段被 SIGKILL。marker window、stdout、counters、folded stack、flamegraph、compare 都已生成；因此本结论只使用 workload marker 内的 dd 和 qperf counters/category 数据，不使用该次 host elapsed 作为严肃指标。

## 5. 图片与 PPT 素材

可直接用于报告或 PPT 的图片：

| 图片 | 用途 |
| --- | --- |
| `docs/flamegraphs/qperf-virtio-blk-baseline-fullstack.png` | baseline 深栈火焰图，展示 sys_read -> rsext4 -> ax-driver -> virtio queue |
| `docs/flamegraphs/qperf-virtio-blk-readahead-fullstack.png` | readahead candidate 深栈火焰图，展示 load_readahead/read_blocks_direct 路径 |
| `docs/flamegraphs/qperf-host-rerun-blk-workload.png` | 早期 host rerun blk workload 火焰图 |
| `docs/flamegraphs/qperf-host-rerun-net-workload.png` | 早期 host rerun net workload 火焰图 |
| `docs/flamegraphs/qperf-long-chain-real.virtio-blk-patched.flamegraph.svg` | 深栈火焰图能力展示 |
| `docs/flamegraphs/qperf-long-chain-demo.synthetic.flamegraph.svg` | synthetic long-chain demo 展示 |

最终优化报告已内嵌 PNG：

* `docs/qperf-virtio-blk-deepstack-optimization-report.md`

如需要重新从 SVG 生成 PNG，可使用：

```bash
convert -background white \
  target/qperf-virtio-blk-opt/baseline/perf/riscv64/latest/qperf/flamegraph.svg \
  docs/flamegraphs/qperf-virtio-blk-baseline-fullstack.png

convert -background white \
  target/qperf-virtio-blk-opt/candidate-readahead/perf/riscv64/latest/qperf/flamegraph.svg \
  docs/flamegraphs/qperf-virtio-blk-readahead-fullstack.png
```

## 6. 主要文档索引

建议后续总结报告优先阅读：

| 文档 | 内容 |
| --- | --- |
| `docs/qperf-tooling-redesign.md` | 工具改造设计 |
| `docs/qperf-marker-and-metrics-usage.md` | marker/window 和 metrics 使用 |
| `docs/qperf-tooling-validation-report.md` | qperf 工具改造验收 |
| `docs/qperf-cargo-starry-integration.md` | `cargo starry perf` 使用指南 |
| `docs/qperf-cargo-starry-integration-report.md` | cargo 集成报告 |
| `docs/qperf-flamegraph-guide.md` | 火焰图阅读与参数指南 |
| `docs/qperf-callchain-flamegraph-design.md` | 深调用栈设计 |
| `docs/qperf-callchain-validation-report.md` | 深调用栈验证报告 |
| `docs/qperf-host-rerun-virtio-bottleneck-report.md` | host QEMU rerun 后的 virtio 瓶颈分析 |
| `docs/qperf-current-blk-bottleneck-analysis.md` | blk 当前瓶颈分析 |
| `docs/qperf-virtio-blk-deepstack-optimization-report.md` | 本轮 virtio-blk 深栈采样与优化最终报告 |

## 7. 已运行验证

工具链和优化相关验证包括：

```bash
cargo fmt
cargo clippy -p rdif-block -- -D warnings
cargo clippy -p rd-block -- -D warnings
cargo clippy -p rsext4 -- -D warnings
cargo clippy -p ax-driver --no-default-features --features 'plat-dyn,virtio-blk,virtio-net,virtio-socket,qperf-metrics' -- -D warnings
git diff --check
```

qperf 实验验证包括：

```bash
cargo starry perf --case blk-baseline ...
cargo starry perf --case blk-direct-readahead ...
python3 tools/starry-syscall-harness/harness.py perf-compare ...
```

详细命令见：

* `docs/qperf-virtio-blk-deepstack-optimization-report.md`
* `docs/qperf-callchain-validation-report.md`
* `docs/qperf-cargo-starry-integration-report.md`

## 8. PPT 叙事建议

建议 PPT 按 8 页左右展开：

| 页码 | 标题 | 关键内容 | 推荐素材 |
| --- | --- | --- | --- |
| 1 | 背景：为什么 qperf 要改造 | 原 qperf 从 boot 开始采样、只有 leaf hotspot、无法回答 virtqueue/copy/counter 问题 | `docs/qperf-virtio-drivers-performance-report.md` |
| 2 | 工具链改造总览 | marker/window、工程分类、virtio counters、A/B compare、cargo starry 集成、深栈火焰图 | 本文第 3 节 |
| 3 | 一条命令生成报告 | `cargo starry perf` 和 `cargo starry run --perf` 使用方式、输出文件 | `docs/qperf-cargo-starry-integration.md` |
| 4 | 深栈火焰图能力 | leaf-only 到 frame-pointer callchain，stack depth summary | `docs/flamegraphs/qperf-virtio-blk-baseline-fullstack.png` |
| 5 | virtio-blk baseline 瓶颈 | 13,629 次 4KiB read、14,354 次 `add_notify_wait_pop`、notify/kick 高占比 | baseline 表格和火焰图 |
| 6 | 优化方案 | direct-only 退化，最终采用 rsext4 readahead 减少同步小 I/O | 本文第 4.3 节 |
| 7 | A/B 结果 | throughput +25.33%，read request -84.51%，add_notify_wait_pop -80.24% | compare 表格 |
| 8 | 局限与下一步 | runtime pause/resume、ring-level counters、net/vsock、真正异步 blk queue | 本文第 9/10 节 |

一句话结论可写为：

> 本轮把 qperf 从 leaf hotspot 采样器升级为可由 `cargo starry perf` 一键运行、支持 workload window、virtio counters、工程分类、A/B compare 和 RISC-V 深调用栈火焰图的 StarryOS 性能工具链，并用它定位到 virtio-blk 同步小 I/O 瓶颈，通过 rsext4 readahead 将 blk 顺序读吞吐提升约 25%。

## 9. 局限性

必须在正式报告中如实说明：

* workload window 当前主要是 postprocess timestamp 过滤，不是真正 QEMU plugin runtime pause/resume。
* `qperf-metrics` counters 是 driver-visible 近似统计，不是 virtqueue ring-level 精确统计。
* 深调用栈依赖 `--full-stack`、debuginfo 和 frame pointer；默认 `leaf` 模式仍不会生成深栈。
* frame-pointer unwinder 主要覆盖 kernel 栈；trap/syscall/task switch 边界仍可能导致 partial unwind。
* candidate readahead run 的 `report.json` 标为 `incomplete`，原因是 stop marker 后 QEMU shutdown 被 SIGKILL；该次 host elapsed 不应作为严肃对比指标。
* host perf/PMU 是可选项，未启用时只依赖 qperf guest sampling 和 workload stdout/counters。
* vsock 在当前环境可能受 `/dev/vhost-vsock` 缺失阻塞，不能做定量结论。
* net 路径已有 counters 和报告基础，但还没有在本轮实现 net RX/TX 优化补丁。

## 10. 下一步建议

基于当前工具链，后续建议按以下顺序推进：

1. virtio-net RX 去 `copy_within()` A/B：
   * 使用 `virtio_net_rx_copy_within_count/bytes` 验证 copy 是否下降。
   * 观察 `memcpy/memmove`、`net_rx_tx_path`、吞吐变化。
2. virtio-net inflight `BTreeMap<u16, ...>` 替换：
   * 对比 `virtio_net_inflight_insert/remove/get_count` 与 `net_inflight_btree` category。
   * 尝试固定数组或 slab。
3. virtio-blk pending read / async queue 原型：
   * 当前 readahead 降低了同步小 I/O 次数，但没有真正利用 virtqueue queue depth。
   * 下一步应实现 `read_blocks_nb()` / `complete_read_blocks()` 或 pending-read API，用 qperf 验证 `add_notify_wait_pop` 是否继续下降。
4. ring-level virtqueue counters：
   * 将 counters 从 driver-visible 近似下沉到 `virtio-drivers` ring 操作层。
   * 增加更精确的 queue depth avg/histogram、available/used ring 统计。
5. runtime sampling pause/resume：
   * 当前 window 后处理能避免报告污染，但 raw sample 仍包含 boot/post。
   * 后续可在 QEMU plugin 或 harness/QMP 层实现真正运行时启停采样。
6. vsock 补测：
   * 在具备 `/dev/vhost-vsock` 的 Linux host 上补跑 vsock workload。
   * 不具备环境时报告只能写阻塞原因，不能编造数据。

## 11. 后续 agent 快速入口

查看最终优化报告：

```bash
sed -n '1,260p' docs/qperf-virtio-blk-deepstack-optimization-report.md
```

查看 A/B compare：

```bash
sed -n '1,220p' target/qperf-virtio-blk-opt/compare-readahead/perf-compare/blk-readahead/compare.md
```

查看 stack depth：

```bash
cat target/qperf-virtio-blk-opt/baseline/perf/riscv64/latest/qperf/stack-depth-summary.csv
cat target/qperf-virtio-blk-opt/candidate-readahead/perf/riscv64/latest/qperf/stack-depth-summary.csv
```

检查 folded stack 是否有纵向展开：

```bash
awk -F';' '{print NF}' target/qperf-virtio-blk-opt/baseline/perf/riscv64/latest/qperf/stack.folded | sort -n | uniq -c
```

重跑 blk profile：

```bash
cargo starry perf \
  --case blk-rerun \
  --output-dir target/qperf-virtio-blk-rerun \
  --full-stack \
  --qperf-metrics \
  --host-time \
  --timeout 240 \
  --workload-timeout 160 \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --shell-init-cmd 'echo reset > /proc/qperf_metrics; echo QPERF_BEGIN:blk; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; cat /proc/qperf_metrics; echo QPERF_END:blk' \
  --no-truncate
```

## 12. 最终交付物清单

代码能力：

* qperf marker/window 支持。
* qperf 工程热点分类和 workload metric parser。
* qperf virtio-aware counters 与 `/proc/qperf_metrics`。
* qperf A/B compare。
* `cargo starry perf` 和 `cargo starry run --perf`。
* RISC-V frame-pointer deep callchain flamegraph。
* virtio-blk direct read fast path。
* rsext4 data block readahead。

文档：

* `docs/qperf-tooling-redesign.md`
* `docs/qperf-marker-and-metrics-usage.md`
* `docs/qperf-tooling-improvement-report.md`
* `docs/qperf-tooling-validation-report.md`
* `docs/qperf-cargo-starry-integration.md`
* `docs/qperf-cargo-starry-integration-report.md`
* `docs/qperf-flamegraph-guide.md`
* `docs/qperf-callchain-flamegraph-design.md`
* `docs/qperf-callchain-validation-report.md`
* `docs/qperf-host-rerun-virtio-bottleneck-report.md`
* `docs/qperf-current-blk-bottleneck-analysis.md`
* `docs/qperf-virtio-blk-deepstack-optimization-report.md`
* `docs/qperf-work-handoff.md`

图片：

* `docs/flamegraphs/qperf-virtio-blk-baseline-fullstack.png`
* `docs/flamegraphs/qperf-virtio-blk-readahead-fullstack.png`
* `docs/flamegraphs/qperf-host-rerun-blk-focus.png`
* `docs/flamegraphs/qperf-host-rerun-blk-workload.png`
* `docs/flamegraphs/qperf-host-rerun-net-workload.png`
* `docs/flamegraphs/qperf-long-chain-real.virtio-blk-patched.flamegraph.svg`
* `docs/flamegraphs/qperf-long-chain-demo.synthetic.flamegraph.svg`

实验产物：

* `target/qperf-virtio-blk-opt/baseline/perf/riscv64/latest/`
* `target/qperf-virtio-blk-opt/candidate-readahead/perf/riscv64/latest/`
* `target/qperf-virtio-blk-opt/compare-readahead/perf-compare/blk-readahead/`

