# qperf 深调用栈火焰图优化验收报告

## 1. 验收目标

本轮验证要回答的问题：

* qperf 火焰图浅是否确认为“只有 PC/TB leaf 采样”。
* `--full-stack` / `--perf-callchain fp` 是否能生成真实纵向调用链。
* folded stack 是否保留完整 Rust demangled 路径。
* blk/net workload 是否能看到 syscall、fs/net、virtio、allocator、BTreeMap、memcpy/memmove 等路径。
* 默认 leaf profile 是否保持兼容。

## 2. 验收环境

| 项目 | 值 |
| --- | --- |
| 仓库 commit | `34d0e92d5` |
| host | WSL2 Linux `5.15.167.4-microsoft-standard-WSL2` |
| CPU | Intel Core i7-14650HX，24 vCPU |
| QEMU | `qemu-system-riscv64` 10.2.1 |
| guest arch | `riscv64` |
| 运行方式 | 直接在宿主 WSL 环境运行，不在 Docker 中运行。本轮按用户已安装宿主 QEMU 的环境继续验证。 |
| host perf | 未启用 |
| qperf metrics | net case 启用，blk full-stack case 未启用 |

当前工作区还包含本轮代码改动与此前未跟踪的文档/图片文件，验收报告只引用确认存在的 `target/qperf-callchain-validation/` 产物。

full-stack 构建后的 ELF 已做反汇编抽查，`VirtQueue::add_notify_wait_pop` prologue 中存在：

```text
sd      ra, 0x88(sp)
sd      s0, 0x80(sp)
addi    s0, sp, 0x90
```

证据文件：`target/qperf-callchain-validation/full-stack-frame-pointer-objdump.txt`。这说明 `-Cforce-frame-pointers=yes` 已经进入实际 kernel 构建，而不是只停留在 CLI 参数层面。

## 3. 诊断结论

旧 blk 产物：

```bash
awk -F';' '{print NF}' target/qperf-host-rerun/blk-harness/perf/riscv64/latest/qperf/stack.folded | sort -n | uniq -c
```

结果为：

```text
623 1
```

`resolve.stats.json` 显示旧 raw 格式为 v2，`raw_records=1292`、`total_frames=1292`，说明旧样本基本只有 leaf PC。根因是采样侧没有真实 callchain，而不是 flamegraph SVG 样式、采样频率或 analyzer 把多帧压扁。

## 4. 实现与验证矩阵

| 验收项 | 结果 | 证据路径 | 备注 |
| --- | --- | --- | --- |
| `cargo starry perf --help` 暴露 callchain 参数 | PASS | `target/qperf-callchain-validation/cargo-starry-perf-help.txt` | 包含 `--full-stack`、`--perf-callchain`、`--perf-debuginfo`、`--perf-force-frame-pointers`。 |
| harness `perf-profile --help` 暴露 callchain 参数 | PASS | `target/qperf-callchain-validation/harness-perf-profile-help.txt` | 包含 `--callchain`、`--full-stack`。 |
| leaf baseline 兼容 | PASS | `target/qperf-callchain-validation/blk-leaf/perf/riscv64/latest/report.json` | 默认 leaf 仍为一层栈。 |
| blk full-stack 深栈 | PASS | `target/qperf-callchain-validation/blk-full-stack/perf/riscv64/latest/qperf/stack.folded` | 579 条 workload 样本中 578 条为多帧。 |
| net full-stack 深栈 | PASS | `target/qperf-callchain-validation/net-full-stack/perf/riscv64/latest/qperf/stack.folded` | 661 条 workload 样本中 652 条为多帧。 |
| `stack-depth-summary.csv` | PASS | `target/qperf-callchain-validation/*/perf/riscv64/latest/qperf/stack-depth-summary.csv` | 输出 raw FP trace depth 分布。 |
| `hotspots.csv` 深栈百分比 | PASS | `target/qperf-callchain-validation/blk-full-stack/perf/riscv64/latest/hotspots.csv` | 函数热点百分比已按帧总量计算。 |
| net qperf metrics 合入 report | PASS | `target/qperf-callchain-validation/net-full-stack/perf/riscv64/latest/report.json` | `workload_metrics.values` 包含 virtio/net counters。 |
| logical stack fallback | N/A | 无 | 因真实 FP unwind 已可用，本轮未实现 logical stack；CLI 会明确拒绝该模式。 |

## 5. leaf baseline

命令：

```bash
cargo starry perf \
  --case blk-leaf \
  --output-dir target/qperf-callchain-validation/blk-leaf \
  --host-time \
  --timeout 120 \
  --workload-timeout 75 \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --shell-init-cmd 'echo QPERF_BEGIN:blk; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; echo QPERF_END:blk' \
  --no-truncate
```

结果：

| 指标 | 值 |
| --- | ---: |
| result | `ok` |
| dd bytes | 53,601,104 |
| dd elapsed | 5.764698 s |
| dd throughput | 9,298,163 B/s |
| marker window | 5.832138317 s |
| boot samples excluded | 154 |
| selected records | 577 |
| selected multi-frame records | 0 |
| raw max depth | 1 |

depth 分布：

```text
depth,samples
1,577
```

结论：默认 leaf 模式保持旧行为和低开销，但不会生成纵向火焰图。

## 6. blk full-stack

命令：

```bash
cargo starry perf \
  --case blk-full-stack \
  --output-dir target/qperf-callchain-validation/blk-full-stack \
  --full-stack \
  --host-time \
  --timeout 180 \
  --workload-timeout 120 \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --shell-init-cmd 'echo QPERF_BEGIN:blk; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; echo QPERF_END:blk' \
  --no-truncate
```

结果：

| 指标 | 值 |
| --- | ---: |
| result | `incomplete` |
| dd bytes | 53,601,104 |
| dd elapsed | 5.779240 s |
| dd throughput | 9,274,766 B/s |
| marker window | 5.845178559 s |
| boot samples excluded | 160 |
| post-window samples excluded | 1222 |
| selected records | 579 |
| selected multi-frame records | 578 |
| samples with FP | 578 |
| unwind success | 578 |
| report avg symbol depth | 43.887737478 |
| raw max depth | 18 |

`stack-depth-summary.csv`：

```text
depth,samples
1,1
4,2
5,2
6,39
7,2
8,6
9,42
10,52
11,44
12,77
13,41
14,72
15,183
16,8
17,7
18,1
```

`awk -F';' '{print NF}' .../stack.folded` 能看到 folded 符号层数最高到 72。层数大于 raw depth 的原因是 analyzer 通过 debug info 展开了 inline frame。

blk 关键路径已经能在 `stack.folded` 中看到：

```text
starry_kernel::syscall::fs::io::sys_read
  -> ax_fs_ng / rsext4
  -> ax_driver::block::binding::Block::read_blocks_wait
  -> rd_block::CmdQueue::read_blocks_blocking
  -> ax_driver::virtio::block::BlockQueue::submit_request
  -> virtio_drivers::device::blk::VirtIOBlk::read_blocks
  -> virtio_drivers::queue::VirtQueue::add_notify_wait_pop
```

top categories：

| category | samples | percent |
| --- | ---: | ---: |
| block_io_path | 434 | 74.9568% |
| memcpy | 179 | 30.9154% |
| virtio_notify_kick | 176 | 30.3972% |
| virtqueue_add_notify_wait_pop | 173 | 29.8791% |
| lock_mutex_wait | 82 | 14.1623% |

`result=incomplete` 的原因是 QMP stop 后 QEMU 在等待退出阶段被超时清理，导致 plugin shutdown summary 不完整；marker window、raw sample、folded stack 与 report 已生成。本项是 stop/shutdown 可靠性问题，不影响“是否能生成深调用栈”的结论。

## 7. net full-stack

命令：

```bash
cargo starry perf \
  --case net-full-stack \
  --output-dir target/qperf-callchain-validation/net-full-stack \
  --full-stack \
  --qperf-metrics \
  --host-time \
  --timeout 300 \
  --workload-timeout 240 \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --shell-init-cmd 'echo reset > /proc/qperf_metrics; echo QPERF_BEGIN:net; wget -O /dev/null http://10.0.2.2:8000/tmp/axbuild/rootfs/rootfs-riscv64-alpine.img.tar.xz; cat /proc/qperf_metrics; echo QPERF_END:net' \
  --no-truncate
```

结果：

| 指标 | 值 |
| --- | ---: |
| result | `ok` |
| wget bytes | 63,543,705 |
| wget elapsed | 6.688766343 s |
| wget throughput | 9,500,063 B/s |
| marker window | 6.688766343 s |
| boot samples excluded | 159 |
| selected records | 661 |
| selected multi-frame records | 652 |
| samples with FP | 658 |
| unwind success | 652 |
| report avg symbol depth | 50.69591528 |
| raw max depth | 17 |

`stack-depth-summary.csv`：

```text
depth,samples
1,9
3,3
4,2
5,1
6,34
7,7
8,12
9,8
10,58
11,136
12,62
13,93
14,183
15,45
16,5
17,3
```

net 关键路径已经能在 `stack.folded` 中看到：

```text
starry_kernel::syscall::fs::io::sys_readv
  -> starry_kernel::file::net::Socket::read
  -> ax_net_ng::SocketOps::recv
  -> ax_net_ng::poll_interfaces
  -> ax_net_ng::device::driver::RdNetDriver::prefetch_rx_packets
  -> rd_net::RxQueue::receive / reclaim_packet
  -> dma_api::array::ContiguousArray::read_with
  -> compiler_builtins::mem::memcpy
```

net counters 已进入 `report.json.workload_metrics.values`：

| counter | 值 |
| --- | ---: |
| virtqueue_add_count | 47,855 |
| virtio_notify_kick_count | 47,855 |
| virtqueue_pop_complete_count | 47,791 |
| virtqueue_add_notify_wait_pop_count | 1,177 |
| virtqueue_depth_max | 63 |
| virtio_net_rx_packets | 44,139 |
| virtio_net_rx_bytes | 65,935,922 |
| virtio_net_rx_copy_within_count | 44,139 |
| virtio_net_rx_copy_within_bytes | 65,935,922 |
| virtio_net_tx_packets | 2,476 |
| virtio_net_tx_staging_copy_bytes | 148,700 |
| virtio_net_inflight_insert_count | 46,678 |
| virtio_net_inflight_remove_count | 46,614 |
| virtio_net_inflight_get_count | 46,614 |

top categories：

| category | samples | percent |
| --- | ---: | ---: |
| net_rx_tx_path | 441 | 66.7171% |
| memcpy | 233 | 35.2496% |
| lock_mutex_wait | 100 | 15.1286% |
| memmove | 65 | 9.8336% |
| net_inflight_btree | 63 | 9.5310% |

## 8. 证据文件

| 用途 | 路径 |
| --- | --- |
| blk leaf report | `target/qperf-callchain-validation/blk-leaf/perf/riscv64/latest/report.json` |
| blk leaf depth | `target/qperf-callchain-validation/blk-leaf/perf/riscv64/latest/qperf/stack-depth-summary.csv` |
| blk full-stack report | `target/qperf-callchain-validation/blk-full-stack/perf/riscv64/latest/report.json` |
| blk full-stack flamegraph | `target/qperf-callchain-validation/blk-full-stack/perf/riscv64/latest/qperf/flamegraph.svg` |
| blk full-stack workload flamegraph | `target/qperf-callchain-validation/blk-full-stack/perf/riscv64/latest/qperf/flamegraph.workload.svg` |
| blk full-stack folded | `target/qperf-callchain-validation/blk-full-stack/perf/riscv64/latest/qperf/stack.folded` |
| blk full-stack depth | `target/qperf-callchain-validation/blk-full-stack/perf/riscv64/latest/qperf/stack-depth-summary.csv` |
| net full-stack report | `target/qperf-callchain-validation/net-full-stack/perf/riscv64/latest/report.json` |
| net full-stack flamegraph | `target/qperf-callchain-validation/net-full-stack/perf/riscv64/latest/qperf/flamegraph.svg` |
| net full-stack workload flamegraph | `target/qperf-callchain-validation/net-full-stack/perf/riscv64/latest/qperf/flamegraph.workload.svg` |
| net full-stack folded | `target/qperf-callchain-validation/net-full-stack/perf/riscv64/latest/qperf/stack.folded` |
| net full-stack depth | `target/qperf-callchain-validation/net-full-stack/perf/riscv64/latest/qperf/stack-depth-summary.csv` |
| full-stack FP 反汇编抽查 | `target/qperf-callchain-validation/full-stack-frame-pointer-objdump.txt` |

## 9. 验证命令

已执行：

```bash
cargo fmt
cargo clippy --manifest-path tools/qperf/Cargo.toml -- -D warnings
cargo clippy --manifest-path tools/qperf/analyzer/Cargo.toml -- -D warnings
cargo clippy -p axbuild -- -D warnings
python3 -m py_compile tools/starry-syscall-harness/harness.py
```

其中 qperf clippy 初次发现两个手写 `% 8 == 0` 对齐判断，已改为 `is_multiple_of(8)` 后通过。

## 10. 局限性

* 默认模式仍是 leaf；只有 `--full-stack` 或 `--perf-callchain fp --perf-force-frame-pointers --perf-debuginfo` 才能期待深栈。
* 当前 unwind 只覆盖 kernel symbol；用户态栈与用户 ELF symbol 尚未纳入。
* trap、异常入口、任务切换、汇编 trampoline 仍可能截断调用链。
* `report.json.callchain.avg_depth` 是 symbolized/inlined frame 平均层数；raw FP 地址深度以 `stack-depth-summary.csv` 为准。
* `hotspot_categories.csv` 是 inclusive stack 归类，深栈下 allocator/scheduler 可能因为共同上层路径被频繁命中，不能按互斥 CPU 时间解读。
* blk full-stack 本次 QEMU stop 结果为 `incomplete`，需要后续继续改善 QMP stop 与 plugin shutdown flush。
* host perf/PMU 未启用，本报告不包含 host PMU 结论。
* vsock 未在本轮补测；没有 `/dev/vhost-vsock` 的 host 不能做定量结论。

## 11. 结论

结论：PASS，针对“火焰图没有纵向延展”的 MVP 目标已经达成。

证据是：

* leaf baseline 仍全部为一帧，复现了原问题。
* `--full-stack` blk case 中 578/579 条 workload 样本为多帧，raw max depth 为 18，folded symbol depth 最高到 72。
* `--full-stack` net case 中 652/661 条 workload 样本为多帧，raw max depth 为 17，folded symbol depth 最高到 78。
* folded stack 中已经出现 syscall -> fs/net -> driver -> virtqueue/memcpy 的完整工程路径。

因此，现在 qperf 不再只能生成 leaf hotspot 火焰图；在 full-stack 模式下可以生成具备明显纵向展开的 RISC-V kernel flamegraph。默认 leaf 模式保留为低开销兼容路径。
