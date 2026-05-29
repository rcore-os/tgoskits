# qperf Tooling Improvement Report

## 当前问题复盘

原 qperf 更接近“从 QEMU 启动到退出的栈采样器”。对 virtio-blk/net/vsock 做性能归因时有三个直接问题：

- 采样窗口默认覆盖 boot，PCI probe、调度、allocator、rootfs 初始化会混入数据面结论。
- 输出主要是 folded stack、火焰图和符号热点，不能直接回答 queue depth、notify/kick、copy bytes、inflight 操作次数等工程问题。
- 优化前后需要人工翻多个报告，缺少字段级 delta、百分比变化和数据不足提示。

## 改造目标

本轮改造的目标是形成一个最小可闭环的实验工具：

- 用 guest stdout marker 定义 workload 窗口，默认避免 boot 污染。
- 保留原有 qperf 输出，同时增加类别归因、workload metrics、归一化指标。
- 用 feature-gated 轻量 counter 记录 virtio-blk/net 的关键路径事件。
- 增加 A/B compare，支撑优化验证。
- 对 vsock 环境阻塞显式记录，不伪造吞吐或 counter。

## 实现方案

### marker/window

`cargo xtask starry perf` 与 `harness.py perf-profile` 新增：

- `--start-marker TEXT`
- `--stop-marker TEXT`
- `--workload-timeout SECONDS`
- `--qperf-metrics`

qperf plugin raw record 升级为带 `elapsed_ns` 的 v2 格式。`qperf-analyzer resolve` 新增 `--start-sec`、`--stop-sec`、`--stats`，按 marker 时间在 postprocess 阶段过滤 folded stack 和 flamegraph。旧 raw 格式仍可解析，但不能做时间窗口过滤。

marker 模式下，harness 会在 shell prompt 出现后注入 workload，并先关闭串口回显，避免 shell 把整行命令回显出来导致 `QPERF_END` 被提前匹配。stop marker 出现后，harness 优先通过 QMP `quit` 停 QEMU，失败再退回 SIGINT。

### 分类聚合

报告新增 `hotspot_categories.csv`，并在 `report.json.hotspots.category_totals` 和 `report.md` 写入 inclusive category 聚合。当前类别包括：

- `virtqueue_add_notify_wait_pop`
- `virtqueue_add`
- `virtqueue_pop_complete`
- `virtio_notify_kick`
- `memcpy`
- `memmove`
- `allocator`
- `scheduler_wait_preempt`
- `lock_mutex_wait`
- `pci_probe_transport`
- `net_inflight_btree`
- `block_io_path`
- `net_rx_tx_path`
- `vsock_tx_rx_path`

类别是工程归因视角，和符号热点并列展示；一个栈可以同时计入 subsystem 和 bottleneck 类别。

### workload metrics

harness 解析 guest stdout：

- `dd`：bytes、seconds、reported MB/s、派生 B/s。
- `wget`：saved 状态、BusyBox 进度条里的大小；若 wget 不输出耗时，则使用 marker window duration 作为 `elapsed_source=marker_window`。
- `QPERF_METRIC key=value`：合入 `report.json.workload_metrics.values`。

新增归一化字段：

- `guest_instructions_per_MB`
- `guest_blocks_per_MB`
- `host_elapsed_sec_per_MB`
- `samples_per_MB`
- `category_samples_per_MB`

本轮示例未启用 `--host-perf`，报告中明确记录 `未启用 host perf`。

### virtio counters

`ax-driver` 新增默认关闭的 `qperf-metrics` feature。启用后通过 `AtomicU64` 记录 driver-visible counter，并由 StarryOS `/proc/qperf_metrics` 导出 `QPERF_METRIC`：

- virtqueue add/notify/pop/add_notify_wait_pop 近似计数。
- driver 可见 inflight depth max 和 histogram。
- blk read/write request count 和 bytes。
- net RX/TX packet count 和 bytes。
- net RX `copy_within` count 和 bytes。
- net TX staging copy count 和 bytes。
- net inflight map insert/remove/get count。

这些 counter 是 driver glue 层视角的近似值。精确 descriptor-ring depth、精确 notify/kick 和 `VirtQueue::add_notify_wait_pop()` 内部事件仍需要在 `virtio-drivers` crate 内增加 instrumentation。

### A/B compare

新增：

```bash
python3 tools/starry-syscall-harness/harness.py perf-compare \
  --baseline <baseline-report-or-dir> \
  --candidate <candidate-report-or-dir> \
  --name <case-name>
```

输出：

- `compare.json`
- `compare.md`
- `compare.csv`

对比字段包括 workload throughput/elapsed、guest executed instructions/blocks、host elapsed/user/sys、hotspot categories、virtio counters、copy bytes、notify/kick count、queue depth max/histogram。缺失字段显示 `N/A`。

已做 smoke test：

```text
target/qperf-tooling-experiments/blk/compare-self/perf-compare/blk-self-smoke/compare.md
```

结论为 `基本无变化`，这是同一份 blk report 自比较的预期结果。

## 新增 JSON 字段说明

`report.json.window`：

- `start_marker` / `stop_marker`：marker 文本。
- `start_time` / `stop_time`：相对 QEMU 启动的秒数。
- `duration_sec`：workload 窗口时长。
- `workload_timeout`：窗口超时配置。
- `truncated_by_timeout`：是否由 workload timeout 截断。
- `boot_samples_excluded`：过滤掉的 start marker 之前样本数。
- `post_window_samples_excluded`：过滤掉的 stop marker 之后样本数。
- `stop_method`：如 `qmp_quit`。
- `warnings`：marker 缺失、旧 raw 格式等风险。

`report.json.workload_metrics`：

- `dd[]`
- `wget[]`
- `custom[]`
- `values`
- `raw_metric_lines`

`report.json.normalized_metrics`：

- `workload_bytes`
- `workload_elapsed_seconds`
- `samples_per_MB`
- `host_elapsed_sec_per_MB`
- `guest_instructions_per_MB`
- `guest_blocks_per_MB`
- `category_samples_per_MB`

## 示例实验结果

实验环境：

- arch: `riscv64`
- qperf freq: `99`
- mode: `tb`
- `--host-time` enabled
- `--host-perf` disabled
- `--qperf-metrics` enabled
- net 用例在同一个 Docker 容器内启动临时 `python3 -m http.server 8000`，确保 guest 的 `10.0.2.2:8000` 可达。

### blk focused workload

命令：

```bash
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --arch riscv64 \
  --timeout 90 \
  --format folded \
  --top 20 \
  --host-time \
  --qperf-metrics \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --workload-timeout 45 \
  --output-dir target/qperf-tooling-experiments/blk \
  --shell-init-cmd 'echo reset > /proc/qperf_metrics; echo QPERF_BEGIN:blk-read; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; cat /proc/qperf_metrics; echo QPERF_END:blk-read'
```

产物：

```text
target/qperf-tooling-experiments/blk/perf/riscv64/latest/report.json
```

实际结果：

- samples: `589`
- window: `1.58549195 -> 7.539382377`, duration `5.953890427s`
- boot samples excluded: `156`
- post-window samples excluded: `496`
- dd: `53601104 bytes`, `5.805581s`, `9232685.58 B/s`
- host elapsed: `12.552265s`
- samples_per_MB: `10.98858`
- host_elapsed_sec_per_MB: `0.234179`

主要类别：

| Category | Samples | Percent |
|---|---:|---:|
| `memcpy` | 166 | 28.18% |
| `virtio_notify_kick` | 145 | 24.62% |
| `virtqueue_add_notify_wait_pop` | 144 | 24.45% |
| `block_io_path` | 115 | 19.52% |
| `allocator` | 89 | 15.11% |

关键 counter：

| Counter | Value |
|---|---:|
| `virtqueue_add_notify_wait_pop_count` | 13780 |
| `virtqueue_add_count` | 13847 |
| `virtio_notify_kick_count` | 13847 |
| `virtqueue_pop_complete_count` | 13783 |
| `virtqueue_depth_max` | 63 |
| `virtio_blk_read_requests` | 13478 |
| `virtio_blk_read_bytes` | 55195136 |
| `virtio_blk_write_requests` | 302 |
| `virtio_blk_write_bytes` | 1236992 |

结论：blk 数据面窗口里 `add_notify_wait_pop` 和 notify/kick 占比明确可见，且 counter 显示 read request 数与同步等待次数基本同量级，支持继续验证异步化、批处理和更高 queue depth 的优化方向。

### net focused workload

命令核心：

```bash
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --arch riscv64 \
  --timeout 120 \
  --format folded \
  --top 20 \
  --host-time \
  --qperf-metrics \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --workload-timeout 60 \
  --output-dir target/qperf-tooling-experiments/net \
  --shell-init-cmd 'echo reset > /proc/qperf_metrics; echo QPERF_BEGIN:net-wget; wget -O /dev/null http://10.0.2.2:8000/tmp/axbuild/rootfs/rootfs-riscv64-alpine.img.tar.xz; cat /proc/qperf_metrics; echo QPERF_END:net-wget'
```

产物：

```text
target/qperf-tooling-experiments/net/perf/riscv64/latest/report.json
```

实际结果：

- samples: `719`
- window: `1.566331117 -> 8.825886896`, duration `7.259555779s`
- boot samples excluded: `154`
- post-window samples excluded: `494`
- wget: BusyBox progress `60.6M`, parsed as `63543705 bytes`
- wget elapsed source: `marker_window`
- wget throughput: `8753112.03 B/s`
- host elapsed: `13.836669s`
- samples_per_MB: `11.315047`
- host_elapsed_sec_per_MB: `0.21775`

主要类别：

| Category | Samples | Percent |
|---|---:|---:|
| `memcpy` | 231 | 32.13% |
| `net_rx_tx_path` | 169 | 23.50% |
| `allocator` | 117 | 16.27% |
| `memmove` | 53 | 7.37% |
| `scheduler_wait_preempt` | 24 | 3.34% |

关键 counter：

| Counter | Value |
|---|---:|
| `virtqueue_add_notify_wait_pop_count` | 605 |
| `virtqueue_add_count` | 47289 |
| `virtio_notify_kick_count` | 47289 |
| `virtqueue_pop_complete_count` | 47225 |
| `virtqueue_depth_max` | 63 |
| `virtio_net_rx_packets` | 44141 |
| `virtio_net_rx_bytes` | 65937102 |
| `virtio_net_rx_copy_within_bytes` | 65937102 |
| `virtio_net_tx_staging_copy_bytes` | 149442 |
| `virtio_net_inflight_insert_count` | 46684 |
| `virtio_net_inflight_remove_count` | 46620 |
| `virtio_net_inflight_get_count` | 46620 |

结论：net 数据面里的 copy 成本已被同时体现在符号热点、类别聚合和 counter 中。RX `copy_within` bytes 与 RX bytes 同量级，说明 RX 路径每包仍有完整搬移；TX staging copy 相对下载流量较小。inflight map 操作次数和包数同量级，后续应针对该结构做替换或减少访问频率的 A/B 验证。

## 新旧报告对比

旧报告只能回答“哪些 guest 函数/栈采样最多”，且默认混入 boot-to-exit 样本。新报告可以直接看到：

- marker window 起止时间、时长、boot/post-window 排除样本数。
- 符号热点与工程类别热点分离展示。
- workload bytes、elapsed、throughput 和 per-MB 归一化指标。
- virtio counter 与 copy bytes。
- host perf 未启用时的显式说明。
- A/B compare 的 delta、百分比变化和 `N/A` 缺失字段。

本轮没有生成“旧工具同 workload”的历史 baseline，因此没有伪造旧版数字。已有 `blk-self-smoke` compare 只用于验证 compare 工具输出路径和缺失字段处理。

## 局限性

- qperf plugin 仍未支持运行时暂停/恢复；当前是 timestamped raw sample + analyzer postprocess 过滤。
- driver-visible queue depth/notify/kick 是近似 counter，不等价于 virtio ring 内部精确事件。
- `guest_instructions_per_MB` 和 `guest_blocks_per_MB` 当前为 `N/A`，需要 qperf summary 稳定导出 executed instruction/block 字段后才能归一化。
- net wget 的 BusyBox 输出没有真实下载耗时，本轮用 marker window duration 派生 elapsed/throughput，并在 JSON 中记录 `elapsed_source=marker_window`。
- host perf 本轮未启用；报告只合入 host wall/user/sys time。
- 当前宿主机没有 `/dev/vhost-vsock`：`ls /dev/vhost-vsock` 返回 `No such file or directory`。因此本轮没有 vsock 吞吐数据，也没有编造 vsock counter。
- `target/qperf-tooling-experiments` 曾由 Docker root 创建，直接写顶层 compare 目录会遇到权限问题；后续可统一在 wrapper 里 chown 输出根目录。

## 后续优化建议

virtio-blk：

- 将同步 `add_notify_wait_pop` 路径改为可批处理或异步 completion，先用 `virtqueue_add_notify_wait_pop_count`、throughput 和 `block_io_path` category 做 A/B。
- 对连续 read 请求做合并或更大块提交，观察 request count、notify/kick count 是否下降。
- 在 `virtio-drivers` 内加入精确 ring depth 和 kick counter，校准 driver-visible 近似值。

virtio-net：

- 优先减少 RX `copy_within`，目标是让 `virtio_net_rx_copy_within_bytes / virtio_net_rx_bytes` 明显下降。
- 减少 TX staging Vec copy，观察 `virtio_net_tx_staging_copy_bytes` 和 `memcpy` category。
- 替换或减少 inflight `BTreeMap` 操作，使用 counter 和 `net_inflight_btree` category 做验证。
- 将 RX/TX queue lock 拆分或缩短锁内 copy，观察 `net_rx_tx_path`、allocator、scheduler 类别变化。

virtio-vsock：

- 先补齐 `/dev/vhost-vsock` 环境和可重复 workload。
- 环境不可用时，report 应保留 blocker 字段或 stdout marker 说明，不输出吞吐。
- 可用后按 net 的方式增加 vsock TX/RX bytes、packet、copy、queue counter，并纳入 compare。
