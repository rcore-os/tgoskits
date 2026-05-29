# qperf 当前 virtio-blk 瓶颈分析

## 1. 数据来源

本页基于当前仓库中已经生成的 marker-aware qperf 结果，不包含新编造数据。

主证据：

* `target/qperf-validation/blk/perf/riscv64/latest/report.json`
* `target/qperf-validation/blk/perf/riscv64/latest/report.md`
* `target/qperf-validation/blk/perf/riscv64/latest/hotspot_categories.csv`
* `target/qperf-validation/blk/perf/riscv64/latest/hotspots.csv`

交叉核对：

* `target/qperf-tooling-experiments/blk/perf/riscv64/latest/report.json`
* `target/qperf-tooling-experiments/blk/perf/riscv64/latest/hotspot_categories.csv`

本轮分析没有重新运行 QEMU。当前宿主环境缺少 `qemu-system-riscv64`，`cargo starry perf --case boot` 的构建阶段可以推进，但启动 QEMU profiling 会失败；失败日志见 `target/qperf-integration-smoke/logs/cargo-starry-perf-boot.log`。

## 2. qperf 当前能力

当前 qperf 已经不只是火焰图采样器，可以同时提供：

* marker/window：用 `QPERF_BEGIN` 和 `QPERF_END` 从 boot 样本中切出 workload window。
* symbol hotspot：`hotspots.csv` 列出采样最多的函数和栈。
* 工程归因类别：`hotspot_categories.csv` 聚合 `memcpy`、`virtqueue_add_notify_wait_pop`、`virtio_notify_kick`、`block_io_path`、`allocator` 等类别。
* workload parser：从 `dd` 输出解析 bytes、elapsed 和 B/s。
* virtio driver-visible counters：从 `/proc/qperf_metrics` 解析 virtqueue、blk、net 计数。
* 归一化指标：`normalized_metrics` 给出 `samples_per_MB`、`host_elapsed_sec_per_MB`、`category_samples_per_MB` 等字段。

需要注意的是，当前 marker/window 仍是后处理 timestamp filter，不是 QEMU plugin runtime pause/resume；virtqueue 计数也仍是 driver-visible 近似值，不是 ring-level 精确事件。

## 3. blk workload 摘要

workload：

```bash
echo reset > /proc/qperf_metrics
echo QPERF_BEGIN:blk-read
dd if=/usr/bin/lto-dump of=/dev/null bs=64k
cat /proc/qperf_metrics
echo QPERF_END:blk-read
```

`target/qperf-validation/blk/perf/riscv64/latest/report.json` 中的关键结果：

| 字段 | 数值 |
| --- | ---: |
| dd bytes | 53,601,104 |
| dd elapsed | 5.794463 s |
| dd throughput | 9,250,400.60 B/s |
| qperf workload window | 5.951667225 s |
| boot samples excluded | 164 |
| post-window samples excluded | 492 |
| folded stack lines | 590 |
| samples per MB | 11.007236 |
| host elapsed sec per MB | 0.235666 |

`host_perf_metrics.enabled` 为 `false`，因此本报告没有 host PMU/perf stat 事件。`guest_instructions_per_MB` 和 `guest_blocks_per_MB` 为 `null`，不能用它们做本轮结论。

## 4. 工程归因热点

`hotspot_categories.csv` 中 workload window 内的 top categories：

| category | samples | percent |
| --- | ---: | ---: |
| memcpy | 173 | 29.3220% |
| virtio_notify_kick | 154 | 26.1017% |
| virtqueue_add_notify_wait_pop | 151 | 25.5932% |
| block_io_path | 108 | 18.3051% |
| allocator | 79 | 13.3898% |
| scheduler_wait_preempt | 10 | 1.6949% |
| memmove | 7 | 1.1864% |
| virtqueue_pop_complete | 2 | 0.3390% |

交叉核对 profile `target/qperf-tooling-experiments/blk/perf/riscv64/latest/` 的结果接近：

| category | samples | percent |
| --- | ---: | ---: |
| memcpy | 166 | 28.1834% |
| virtio_notify_kick | 145 | 24.6180% |
| virtqueue_add_notify_wait_pop | 144 | 24.4482% |
| block_io_path | 115 | 19.5246% |
| allocator | 89 | 15.1104% |

两组结果都显示，blk workload 的主要采样集中在 copy、virtqueue 同步提交/等待、notify/kick、block I/O 路径和 allocator/cache 管理。

## 5. virtio-blk counters

`/proc/qperf_metrics` 被 harness 合入 `report.json.workload_metrics.values`。主 profile 的关键计数如下：

| counter | value |
| --- | ---: |
| virtqueue_add_notify_wait_pop_count | 13,780 |
| virtqueue_add_count | 13,847 |
| virtio_notify_kick_count | 13,847 |
| virtqueue_pop_complete_count | 13,783 |
| virtqueue_depth_max | 63 |
| virtqueue_depth_hist_0 | 13,781 |
| virtqueue_depth_hist_1 | 13,783 |
| virtqueue_depth_hist_33_64 | 35 |
| virtio_blk_read_requests | 13,478 |
| virtio_blk_read_bytes | 55,195,136 |
| virtio_blk_write_requests | 302 |
| virtio_blk_write_bytes | 1,236,992 |

派生指标：

| 指标 | 数值 |
| --- | ---: |
| add_notify_wait_pop per MB | 257.08 |
| notify/kick per MB | 258.33 |
| read request per MB | 251.45 |
| blk read bytes per request | 4,095.20 |
| blk read bytes per add_notify_wait_pop | 4,005.45 |

虽然 guest 命令使用 `bs=64k`，driver-visible blk read request 平均仍约 4 KiB。这说明当前路径没有把上层 64 KiB read 有效合并成更大的 virtqueue 批量请求，而是表现为大量同步 4 KiB 级别请求。

## 6. 代码路径核对

当前 `drivers/ax-driver/src/virtio/block.rs` 中：

* `VirtIoBlkDevice::new()` 调用 `raw.disable_interrupts()`。
* `submit_request()` 的 read 分支调用 `self.raw.raw.read_blocks(request.block_id, &mut buffer)`。
* read 完成后才 `record_blk_read(bytes)`。
* `poll_request()` 当前直接返回 `Ok(())`。

因此从 driver 层看，当前 blk request 是同步完成语义：提交 read 时就在同一调用路径中等待底层 virtqueue 完成，而不是把多个 pending request 放进 virtqueue 后再集中完成。

## 7. 瓶颈判断

当前最明确的 virtio-blk 瓶颈是：

**blk read 路径以接近 4 KiB 粒度频繁执行同步 `VirtQueue::add_notify_wait_pop`，并几乎每个 virtqueue add 都触发 notify/kick，导致 queue depth 没有被持续利用。**

证据链：

* `virtqueue_add_notify_wait_pop` 占 25.5932% inclusive samples。
* `virtio_notify_kick` 占 26.1017% inclusive samples。
* `virtqueue_add_notify_wait_pop_count = 13,780`，`virtio_notify_kick_count = 13,847`，两者与 `virtqueue_add_count = 13,847` 接近。
* `virtio_blk_read_requests = 13,478`，`virtio_blk_read_bytes = 55,195,136`，平均每个 read request 约 4,095 bytes。
* `virtqueue_depth_hist_0` 和 `virtqueue_depth_hist_1` 各约 13.8k，说明大部分观测点接近空队列或单请求状态；`virtqueue_depth_max = 63` 只说明曾经出现过较深队列，不代表 workload 稳定利用 queue depth。
* `drivers/ax-driver/src/virtio/block.rs` 的 `submit_request()` 直接调用同步 `read_blocks()`，`poll_request()` 没有真正完成异步轮询。

## 8. 次级瓶颈和风险

`memcpy` 是采样占比最高的单类，主 profile 中为 29.3220%。这说明 blk workload 中 copy 开销非常明显，但当前 qperf 只能证明 copy 是真实热点，不能仅凭这一份报告断言 copy 全部来自 virtio-blk driver。`hotspots.csv` 还出现 ext4/data block cache、BTreeMap、allocator 相关 symbol，copy 可能来自文件系统 cache、用户/内核 buffer 复制或 block buffer 管理。

因此本轮更稳妥的 blk 优化切入点是同步 virtqueue 提交/等待和 request 粒度，而不是直接宣称 driver DMA copy 是唯一主因。

## 9. 建议的下一步 A/B 实验

1. 实现最小 pending-read 原型：把 blk read 分成 submit 和 complete 两阶段，避免每个 4 KiB request 立即 `add_notify_wait_pop`。
2. 增加或复用 `read_blocks_nb()` / `complete_read_blocks()` 形式的接口，在队列中累积多个请求后统一 notify 或批量回收 used ring。
3. 在 block/fs 层尝试合并连续块，让 `bs=64k` 能转换成更少、更大的 virtio request。
4. 对比 baseline 和 candidate 的以下指标：
   * `virtqueue_add_notify_wait_pop_count`
   * `virtio_notify_kick_count`
   * `virtio_blk_read_requests`
   * `virtqueue_depth_hist_*`
   * `virtqueue_add_notify_wait_pop` category percent
   * `virtio_notify_kick` category percent
   * `dd throughput`
5. 如果候选方案有效，预期现象是 `add_notify_wait_pop per MB` 和 `notify/kick per MB` 明显下降，稳定 queue depth 上升，dd throughput 上升或 host elapsed per MB 下降。

## 10. 后续需要补强的 qperf 字段

为了把结论从 driver-visible 近似推进到 ring-level 精确归因，后续应把以下计数下沉到 `virtio-drivers` 或更接近 virtqueue ring 的位置：

* 每个 queue 的 add、notify、used pop 精确次数。
* notify coalescing 或 skipped notify 次数。
* ring avail/used depth 的采样直方图。
* 等待 used ring 的自旋或阻塞时间。
* 每个 blk request 的 sector count、byte count、merge 来源。
* block/fs copy counters，用于区分 FS cache copy、user copy 和 driver buffer copy。
