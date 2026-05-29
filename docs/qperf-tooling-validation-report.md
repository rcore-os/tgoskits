# qperf 工具改造验收报告

## 1. 验收目标

本轮验收验证 qperf 工具改造是否已经从“启动即采样的火焰图工具”升级为可支撑 virtio-blk、virtio-net、virtio-vsock 性能归因和优化 A/B 验证的实验工具。重点回答：

* 是否解决 boot 阶段样本污染 workload 数据面的问题。
* 是否具备 marker 驱动的 workload window。
* 是否具备工程分类热点，而不只是 symbol hotspot。
* 是否具备 virtio-aware counters，并能进入最终 report。
* 是否具备 A/B compare 输出。
* 是否保持旧 perf-profile 命令兼容。
* 是否足以支撑下一轮 virtio 优化验证。

## 2. 验收环境

| 项目 | 值 |
| --- | --- |
| 仓库 commit | `6e748d6b7a5b3a8e90e2cba3b030ea0ca9c3e617` |
| 分支 | `fix/starry-syscall-harness` |
| git status | 工作区非干净，包含本轮 qperf 改造文件和若干既有未跟踪文档；本报告未覆盖已有实验结果 |
| Host kernel | `Linux LAPTOP-SAOPKIGH 5.15.167.4-microsoft-standard-WSL2 x86_64` |
| CPU | Intel Core i7-14650HX, 24 vCPU, WSL2 |
| Docker image | `ghcr.io/rcore-os/tgoskits-container:latest`, image id `sha256:b7c4600e825dcb474d1f6a6bc51b8e6616ada23a24d048fc45522a58f76eb162` |
| Guest arch | `riscv64` |
| QEMU | `qemu-system-riscv64`, `virt` machine, 512 MiB, virtio-blk-pci, virtio-net-pci user net, qperf plugin, QMP unix socket |
| qperf 参数 | marker runs 使用 `--host-time --qperf-metrics --start-marker QPERF_BEGIN --stop-marker QPERF_END --workload-timeout 45` |
| qperf-metrics | blk/net marker 用例启用；兼容性旧命令未启用 |
| host perf | 未启用；报告中显示 `未启用 host perf` |

构建检查：

* `python3 -m py_compile tools/starry-syscall-harness/harness.py`：PASS。
* `cargo clippy -p ax-driver --no-default-features --features 'plat-dyn,virtio-blk,virtio-net,virtio-socket,qperf-metrics' -- -D warnings`：PASS。
* `cargo clippy -p ax-driver --no-default-features --features 'plat-dyn,virtio-blk,virtio-net,virtio-socket' -- -D warnings`：PASS。

## 3. 验收矩阵

| 模块 | 验收项 | 结果 | 证据文件 | 备注 |
| --- | --- | --- | --- | --- |
| harness/window | start/stop marker | PASS | `target/qperf-validation/blk/perf/riscv64/latest/report.json` | window start/stop/duration 已记录 |
| harness/window | boot/post-window 样本排除 | PASS | `target/qperf-validation/blk/perf/riscv64/latest/report.json` | blk boot 排除 164，post-window 排除 492 |
| harness/window | marker missing warning | PASS | `target/qperf-validation/missing-stop/perf/riscv64/latest/report.json` | 超时截断并记录 warning |
| qperf/report | required report artifacts | PASS | `target/qperf-validation/blk/perf/riscv64/latest/` | report/json/md、csv、folded、flamegraph、stdout/stderr 存在 |
| qperf/report | plugin summary / guest instr | PARTIAL | `target/qperf-validation/blk/perf/riscv64/latest/report.json` | blk 缺少 `qperf/qperf.summary.txt`，guest instr/blocks per MB 为 N/A |
| qperf/report | hotspot_categories.csv | PASS | `target/qperf-validation/blk/perf/riscv64/latest/hotspot_categories.csv` | 分类非空，包含 memcpy、virtqueue、block path 等 |
| qperf/report | dd parser | PASS | `target/qperf-validation/blk/perf/riscv64/latest/report.json` | 解析 53,601,104 bytes、5.794463s |
| qperf/report | wget parser | PASS | `target/qperf-validation/net-container/perf/riscv64/latest/report.json` | container 内 HTTP server 用例解析成功 |
| qperf/report | 用户给定 net 命令 | FAIL | `target/qperf-validation/net/perf/riscv64/latest/profile.stdout` | Docker/WSL 拓扑下 `10.0.2.2:8000` connection refused |
| virtio counters | feature 默认关闭 | PASS | `drivers/ax-driver/Cargo.toml`，clippy 输出 | 默认 feature 未强制开启 instrumentation |
| virtio counters | blk counters | PASS | `target/qperf-validation/blk/perf/riscv64/latest/report.json` | blk read bytes/request、virtqueue、notify/kick 计数进入 report |
| virtio counters | net counters | PASS | `target/qperf-validation/net-container/perf/riscv64/latest/report.json` | RX/TX bytes、copy bytes、inflight 操作进入 report |
| virtio counters | reset counters | PARTIAL | marker workload shell 命令与 `/proc/qperf_metrics` 代码 | 已执行 `echo reset`，但未做单独 before/after 定量隔离 |
| compare | self compare | PASS | `target/qperf-validation/blk/compare-self/perf-compare/blk-self-smoke/compare.md` | 主要指标 delta 为 0，结论“基本无变化” |
| compare | cross compare | PASS | `target/qperf-validation/blk/compare-cross/perf-compare/blk-vs-net-cross/compare.md` | 不 crash，缺失字段显示 N/A；跨 workload 结论不应用作优化判断 |
| compatibility | old command | PASS | `target/qperf-validation/compat-old/perf/riscv64/latest/report.json` | 未传 marker/qperf-metrics 时仍可运行 |
| vsock | vhost-vsock 环境 | BLOCKED | `/dev/vhost-vsock` 检查 | 当前 host 无该设备，未做定量结论 |

本轮发现并做了一个最小修复：`report.json` 的 artifacts 列表原先缺少 `profile.stdout`、`profile.stderr`、raw sample、summary、QEMU config 等已生成文件。已在 `tools/starry-syscall-harness/harness.py` 中补充，并通过 py_compile 与旧命令兼容性 smoke test。

## 4. blk 验证结果

命令：

```bash
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --arch riscv64 \
  --host-time \
  --qperf-metrics \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --workload-timeout 45 \
  --shell-init-cmd 'echo reset > /proc/qperf_metrics; echo QPERF_BEGIN:blk-read; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; cat /proc/qperf_metrics; echo QPERF_END:blk-read' \
  --output-dir target/qperf-validation/blk
```

证据：`target/qperf-validation/blk/perf/riscv64/latest/report.json`

| 指标 | 值 |
| --- | --- |
| dd bytes | `53601104` |
| dd elapsed | `5.794463` s |
| dd throughput | `9250400.597950146` B/s，约 `8.8 MB/s` |
| marker window duration | `5.951667225` s |
| boot samples excluded | `164` |
| post-window samples excluded | `492` |
| total workload samples | `590` |
| samples per MB | `11.007236` |
| instructions per MB | N/A |
| blocks per MB | N/A |
| host elapsed per MB | `0.235666` s/MB |
| host elapsed/user/sys | `12.631965` / `14.222615` / `0.750295` s |

Top hotspot categories：

| category | samples | percent |
| --- | ---: | ---: |
| `memcpy` | 173 | 29.3220% |
| `virtio_notify_kick` | 154 | 26.1017% |
| `virtqueue_add_notify_wait_pop` | 151 | 25.5932% |
| `block_io_path` | 108 | 18.3051% |
| `allocator` | 79 | 13.3898% |
| `scheduler_wait_preempt` | 10 | 1.6949% |

Virtio/block counters：

| counter | value |
| --- | ---: |
| `virtqueue_add_notify_wait_pop_count` | 13,780 |
| `virtqueue_add_count` | 13,847 |
| `virtio_notify_kick_count` | 13,847 |
| `virtqueue_pop_complete_count` | 13,783 |
| `virtqueue_depth_max` | 63 |
| `virtio_blk_read_requests` | 13,478 |
| `virtio_blk_read_bytes` | 55,195,136 |
| `virtio_blk_write_requests` | 302 |
| `virtio_blk_write_bytes` | 1,236,992 |

判断：blk 已能支撑后续“同步 `add_notify_wait_pop` 是否下降、queue depth 是否被利用、blk read bytes/request 是否匹配 workload”的 A/B 验证。但当前 blk 报告缺少 `qperf/qperf.summary.txt`，导致 guest instructions/blocks per MB 为 N/A；这会削弱严肃的指令级归一化判断，需要修复 plugin summary 落盘或 QEMU 退出流程。

## 5. net 验证结果

用户给定命令在当前 Docker/WSL 环境下失败：host HTTP server 启动在 WSL host，guest 访问 Docker 内 QEMU slirp 的 `10.0.2.2:8000`，结果为 connection refused。失败证据：`target/qperf-validation/net/perf/riscv64/latest/profile.stdout`。

为验证工具本身，补跑了 container 内 HTTP server 版本，使 guest 的 `10.0.2.2:8000` 指向同一个 Docker 网络命名空间内的服务。证据：`target/qperf-validation/net-container/perf/riscv64/latest/report.json`。

| 指标 | 值 |
| --- | --- |
| wget bytes | `63543705` |
| wget elapsed | `7.299213151` s，来源为 marker window |
| wget throughput | `8705555.473646423` B/s |
| marker window duration | `7.299213151` s |
| boot samples excluded | `155` |
| post-window samples excluded | `0` |
| total workload samples | `722` |
| samples per MB | `11.362258` |
| instructions per MB | `18821395.746439` |
| blocks per MB | `2875126.497581` |
| host elapsed per MB | `0.141124` s/MB |
| host elapsed/user/sys | `8.967565` / `9.559295` / `0.818735` s |

Top hotspot categories：

| category | samples | percent |
| --- | ---: | ---: |
| `memcpy` | 220 | 30.4709% |
| `net_rx_tx_path` | 158 | 21.8837% |
| `allocator` | 114 | 15.7895% |
| `memmove` | 81 | 11.2188% |
| `scheduler_wait_preempt` | 21 | 2.9086% |
| `block_io_path` | 2 | 0.2770% |

Virtio/net counters：

| counter | value |
| --- | ---: |
| `virtio_net_rx_packets` | 44,141 |
| `virtio_net_rx_bytes` | 65,937,102 |
| `virtio_net_rx_copy_within_count` | 44,141 |
| `virtio_net_rx_copy_within_bytes` | 65,937,102 |
| `virtio_net_tx_packets` | 2,478 |
| `virtio_net_tx_bytes` | 149,322 |
| `virtio_net_tx_staging_copy_count` | 2,478 |
| `virtio_net_tx_staging_copy_bytes` | 149,322 |
| `virtio_net_inflight_insert_count` | 46,682 |
| `virtio_net_inflight_remove_count` | 46,618 |
| `virtio_net_inflight_get_count` | 46,618 |
| `virtqueue_depth_max` | 63 |

判断：net 工具链已经能支撑 RX 去 `copy_within()`、TX staging copy 优化、inflight map 替换的 A/B 验证。需要注意，当前用户文档中的 host server 启动方式在 Docker/WSL 下不可复现，应改为 container 内 server、host 网络模式，或显式说明网络拓扑。

## 6. compare 验证结果

Self compare 命令：

```bash
python3 tools/starry-syscall-harness/harness.py perf-compare \
  --baseline target/qperf-validation/blk/perf/riscv64/latest/report.json \
  --candidate target/qperf-validation/blk/perf/riscv64/latest/report.json \
  --name blk-self-smoke \
  --output-dir target/qperf-validation/blk/compare-self
```

输出：

* `target/qperf-validation/blk/compare-self/perf-compare/blk-self-smoke/compare.json`
* `target/qperf-validation/blk/compare-self/perf-compare/blk-self-smoke/compare.md`
* `target/qperf-validation/blk/compare-self/perf-compare/blk-self-smoke/compare.csv`

Self compare 结果为“基本无变化”，可比字段 delta 为 0。由于 blk 报告本身缺 guest instructions/blocks，相关字段显示 N/A。

Cross compare 命令：

```bash
python3 tools/starry-syscall-harness/harness.py perf-compare \
  --baseline target/qperf-validation/blk/perf/riscv64/latest/report.json \
  --candidate target/qperf-validation/net-container/perf/riscv64/latest/report.json \
  --name blk-vs-net-cross \
  --output-dir target/qperf-validation/blk/compare-cross
```

输出：

* `target/qperf-validation/blk/compare-cross/perf-compare/blk-vs-net-cross/compare.json`
* `target/qperf-validation/blk/compare-cross/perf-compare/blk-vs-net-cross/compare.md`
* `target/qperf-validation/blk/compare-cross/perf-compare/blk-vs-net-cross/compare.csv`

Cross compare 不 crash，Markdown 中缺失字段显示 N/A，说明 compare 对 schema 缺口有容错。但跨 workload 的自动结论显示“退化”，这不应解释为真实优化回归；compare 目前不校验 baseline/candidate 是否同一 workload、同一输入大小。

初次将 compare 输出写到 `target/qperf-validation/compare-self` 时失败，原因是该目录由 Docker/root 创建，host 用户无写权限。改写到 `target/qperf-validation/blk/compare-self` 后通过。后续应避免 Docker 创建 root-owned 顶层验证目录，或在 harness 中修正 uid/gid。

## 7. vsock 状态

当前 host 缺少 `/dev/vhost-vsock`：

```text
ls: cannot access '/dev/vhost-vsock': No such file or directory
```

因此本轮未做 virtio-vsock 定量结论，也未伪造 vsock 指标。具备 vhost-vsock 的 Linux host 上应补测：

```bash
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --arch riscv64 \
  --host-time \
  --qperf-metrics \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --workload-timeout 45 \
  --shell-init-cmd 'echo reset > /proc/qperf_metrics; echo QPERF_BEGIN:vsock; <vsock workload>; cat /proc/qperf_metrics; echo QPERF_END:vsock' \
  --output-dir target/qperf-validation/vsock
```

补测时应在报告中记录 `/dev/vhost-vsock` 权限、QEMU vsock 参数、CID/port、workload bytes 与 elapsed。

## 8. 未达标项与风险

* runtime pause/resume 仍未验证为真实启停采样；当前能力主要依赖 timestamp 后处理过滤和 marker window 标注。结论应表述为“boot samples excluded by postprocess”，不能声称采样器运行时暂停。
* blk 用例缺失 `qperf/qperf.summary.txt`，导致 guest instructions per MB 与 guest blocks per MB 为 N/A。这是归一化指标的关键缺口。
* queue depth、notify/kick、pop/complete 计数是 driver-visible 近似统计，不是 virtqueue ring-level 精确硬件事件计数；当前使用 relaxed atomics，snapshot 不是强一致事务。
* host perf 未启用，PMU 级 cycles/cache-miss 等 host 指标不存在。报告有明确 `未启用 host perf` 说明。
* 用户给定 net 命令在当前 Docker/WSL 网络拓扑下失败。工具可用，但示例命令需要改成 container 内 HTTP server 或明确网络前提。
* compare 对跨 workload 输入没有 guard，可能给出形式上的“退化/改善”结论；实际 A/B 应只比较同 workload、同输入、同 qperf 参数。
* compare CSV 中缺失字段为空值，Markdown 显示 N/A；若后续自动消费 CSV，建议也输出显式 N/A。
* `/proc/qperf_metrics reset` 已在 workload 中执行，但本轮未做独立 before/after 断言；建议补一个微型读写 reset 单测或 harness smoke。
* 顶层 `target/qperf-validation` 可能被 Docker 创建为 root-owned，导致 host-side compare 输出 PermissionError。

## 9. 总体结论

结论：**PARTIAL**。

本轮改造达到 qperf 工具 MVP 的主要方向：marker window 可用，boot/post-window 样本能从 workload 报告中排除；工程分类热点、dd/wget/QPERF_METRIC parser、virtio-blk/net counters、A/B compare 都有可复现实验文件支撑。blk 和 net 的关键候选瓶颈已经能在 report 中直接看到。

但仍存在关键缺口：blk 运行缺 guest instruction/block summary，用户给定 net 命令在当前 Docker/WSL 拓扑下不可复现，采样窗口仍是后处理过滤而不是运行时 pause/resume，virtio counters 是 driver-visible 近似值。它可以支撑下一轮小规模 virtio 优化 A/B 验证，但报告必须保留这些限制，不能把当前数据解释为完整 PMU/virtqueue ring 级精确观测。

## 10. 下一步建议

1. net RX 去 `copy_within()` 的 A/B 优化验证：使用 `target/qperf-validation/net-container` 同样的 container 内 HTTP server 拓扑，比较 `virtio_net_rx_copy_within_bytes`、`memmove` category、throughput、samples per MB。
2. net inflight `BTreeMap` 替换固定数组/slab 的 A/B 优化验证：比较 `virtio_net_inflight_insert/remove/get_count`、`net_inflight_btree` category、allocator category、host elapsed per MB。
3. blk `read_blocks_nb()` / `complete_read_blocks()` 最小 pending-read 原型验证：比较 `virtqueue_add_notify_wait_pop_count`、`virtio_notify_kick_count`、`virtqueue_depth_max`、blk throughput、samples per MB。
4. 修复 blk `qperf.summary.txt` 缺失问题后重跑 blk；要求 `guest_instructions_per_MB` 和 `guest_blocks_per_MB` 不再是 N/A。
5. 在具备 `/dev/vhost-vsock` 的 Linux host 上补测 vsock，并把环境阻塞、CID/port、bytes、elapsed、vsock counters 明确写入 report。
