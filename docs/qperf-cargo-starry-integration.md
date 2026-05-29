# qperf cargo starry 集成指南

## 快速开始

默认 boot profile：

```bash
cargo starry perf --case boot
```

blk marker profile：

```bash
cargo starry perf \
  --case blk-read \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --workload-timeout 45 \
  --workload 'echo QPERF_BEGIN:blk-read; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; echo QPERF_END:blk-read'
```

`--qperf-metrics` 只负责把 guest stdout 中的 `QPERF_METRIC key=value`
合入报告；内核或 driver 侧的 `/proc/qperf_metrics` 插桩需要由被测分支单独提供。

`cargo starry run --perf` 是 `cargo starry qemu --perf` 的 alias 路径，适合用户按 run 语义启动默认 qperf：

```bash
cargo starry run --perf --perf-case boot
```

如果只是复盘已有结果或在没有 host QEMU 的机器上做报告分析，可以直接阅读已经生成的
`report.json`、`hotspot_categories.csv` 和 `qperf/stack.folded`。当前仓库中的 blk
瓶颈分析见 `docs/qperf-current-blk-bottleneck-analysis.md`。

## 环境依赖

`cargo starry perf` 在 host 上直接运行 QEMU，需要：

* `qemu-system-riscv64` 或对应 arch 的 system QEMU 在 `PATH` 中。
* Rust `llvm-tools`/`cargo-binutils` 提供 `rust-objcopy`、`rust-nm`。
* axbuild 依赖的 host 库，例如 `libudev.pc`；Debian/Ubuntu 通常来自 `libudev-dev`。

本入口会把 StarryOS build config、generated axconfig 和 managed rootfs 隔离到当前 qperf 输出目录下的 `axbuild-tmp/`。高级用户可以显式设置 `AXBUILD_TMP_DIR=<dir>` 复用或重定向这部分临时文件。

## 默认值

`cargo starry perf` 默认使用：

| 参数 | 默认值 |
| --- | --- |
| arch | `riscv64` |
| case | `boot` |
| output | `target/qperf/<case>/perf/<arch>/latest/` |
| qperf freq | `99` |
| max depth | `128` |
| mode | `tb` |
| format | `all` |
| top | `80` |
| min percent | `0.3` |
| host time | enabled |
| qperf metrics | disabled unless guest prints `QPERF_METRIC` lines |

生成完成后会打印：

```text
qperf report generated:
  report: target/qperf/<case>/perf/riscv64/latest/report.md
  flamegraph: target/qperf/<case>/perf/riscv64/latest/qperf/flamegraph.svg
  folded stack: target/qperf/<case>/perf/riscv64/latest/qperf/stack.folded
  json: target/qperf/<case>/perf/riscv64/latest/report.json
```

## 高级参数

常用 qperf 参数：

```bash
cargo starry perf \
  --case net-wget \
  --freq 199 \
  --max-depth 256 \
  --mode insn \
  --symbol-style full \
  --focus 'virtio|net|memcpy|memmove' \
  --no-truncate
```

`cargo starry run --perf` 使用 `--perf-*` 前缀：

```bash
cargo starry run \
  --perf \
  --perf-case blk-read \
  --perf-workload 'echo QPERF_BEGIN:blk; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; echo QPERF_END:blk' \
  --perf-start-marker QPERF_BEGIN \
  --perf-stop-marker QPERF_END \
  --perf-qperf-metrics \
  --perf-symbol-style full \
  --perf-focus 'virtio|block'
```

当前 `run --perf` 只支持默认 qperf QEMU/rootfs flow，以及 `--arch`、`--debug` 这类轻量 build override。带 `--qemu-config`、`--rootfs`、`--config`、`--target`、`--smp` 的 perf run 仍应使用 plain `cargo starry qemu` 或后续扩展。

## 输出文件

| 文件 | 说明 |
| --- | --- |
| `report.json` | 机器可读报告 |
| `report.md` | 可直接阅读/粘贴的 Markdown 报告 |
| `hotspots.csv` | symbol hotspot |
| `hotspot_categories.csv` | 工程归因类别 |
| `qperf/stack.folded` | 完整 folded stack |
| `qperf/flamegraph.svg` | 默认完整火焰图 |
| `qperf/flamegraph.workload.svg` | marker workload window 火焰图 |
| `qperf/flamegraph.boot.svg` | boot 阶段火焰图 |
| `qperf/flamegraph.post.svg` | post-window 火焰图 |
| `qperf/flamegraph.focus.svg` | `--focus` 过滤后的火焰图 |
| `qperf/summary.txt` | qperf run 摘要 |

## 用 qperf 做 blk 瓶颈初筛

推荐先跑 marker + metrics workload：

```bash
cargo starry perf \
  --case blk-read \
  --qperf-metrics \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --workload-timeout 45 \
  --workload 'echo reset > /proc/qperf_metrics; echo QPERF_BEGIN:blk-read; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; cat /proc/qperf_metrics; echo QPERF_END:blk-read'
```

读报告时优先看三组字段：

| 字段 | 用途 |
| --- | --- |
| `window` | 确认 boot/post-window 样本是否被排除 |
| `hotspot_categories.csv` | 看 copy、virtqueue、allocator、scheduler 等工程类别占比 |
| `workload_metrics.values` | 看 `virtqueue_add_notify_wait_pop_count`、notify/kick、blk read bytes/requests 等 driver-visible counters |

当前已有 blk profile 显示，`virtqueue_add_notify_wait_pop` 和
`virtio_notify_kick` 都在 workload window 内占到约 25% 级别，且
`virtio_blk_read_bytes / virtio_blk_read_requests` 约为 4 KiB。这个结果指向
“大量同步 4 KiB 级别 virtqueue 请求，queue depth 没有被持续利用”的 blk
优化方向。完整证据和限制见 `docs/qperf-current-blk-bottleneck-analysis.md`。

## A/B compare

```bash
python3 tools/starry-syscall-harness/harness.py perf-compare \
  --baseline target/qperf/blk-read/perf/riscv64/latest/report.json \
  --candidate target/qperf/blk-read-patched/perf/riscv64/latest/report.json \
  --name blk-read-ab \
  --output-dir target/qperf/compare
```

## 网络与 vsock 注意事项

在 Docker/WSL 下，guest 的 `10.0.2.2` 通常指向 QEMU slirp 所在网络命名空间，不一定能访问 WSL host 上启动的 HTTP server。net profile 建议在同一个 Docker 容器内启动 HTTP server，或明确使用 host 网络。

vsock 需要 host 具备 `/dev/vhost-vsock`。如果不存在，不应输出伪造吞吐或 counter。

## 当前局限

* qperf 仍是 QEMU TCG plugin 采样，不是 guest PMU。
* marker window 仍是 raw timestamp 后处理过滤，不是 runtime pause/resume。
* stack unwind 依赖 frame pointer 和可解析 DWARF；inline、tail call、汇编 trampoline 可能断栈。
* virtio counters 是 driver-visible 近似值，不是 ring-level 精确硬件事件。
* user symbol 解析当前只在符号存在于 kernel ELF 时可见。
