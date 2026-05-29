# qperf 火焰图指南

## 快速生成

默认 profile 使用 leaf 模式，开销低，但调用栈通常只有一层：

```bash
cargo starry perf --case boot --symbol-style full
```

需要纵向展开的深调用栈时，使用 `--full-stack`：

```bash
cargo starry perf \
  --case blk-full-stack \
  --full-stack \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --workload 'echo QPERF_BEGIN:blk; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; echo QPERF_END:blk'
```

`--full-stack` 会启用 frame pointer callchain，并在 kernel 构建中加入：

```text
-Cdebuginfo=2
-Cstrip=none
-Cforce-frame-pointers=yes
```

因此它比默认 profile 慢，也会触发更多构建工作；只在需要深栈火焰图时开启。

## callchain 模式

| 模式 | 用法 | 说明 |
| --- | --- | --- |
| `leaf` | 默认 | 只记录采样 PC，适合粗略热点和旧命令兼容。 |
| `fp` | `--perf-callchain fp` 或 `--full-stack` | 读取 RISC-V `sp/fp`，按 frame pointer 链恢复 kernel 调用栈。 |
| `logical` | 当前不可用 | 预留人工插桩逻辑栈模式。本轮没有实现，不会伪装成真实 callchain。 |

如果只传 `--perf-callchain fp`，还应配合：

```bash
--perf-debuginfo --perf-force-frame-pointers
```

实际使用中推荐直接传 `--full-stack`。

## 读哪些文件

| 文件 | 说明 |
| --- | --- |
| `qperf/flamegraph.svg` | 完整 profile 火焰图。 |
| `qperf/flamegraph.workload.svg` | marker window 内样本火焰图。 |
| `qperf/flamegraph.boot.svg` | start marker 前样本火焰图。 |
| `qperf/flamegraph.post.svg` | stop marker 后样本火焰图，可能为空。 |
| `qperf/flamegraph.focus.svg` | `--focus` regex 命中的 focused 火焰图。 |
| `qperf/stack.folded` | folded stack 原始输入，默认保留完整 demangled Rust symbol。 |
| `qperf/stack-depth-summary.csv` | raw callchain 地址深度分布。 |
| `hotspots.csv` | 函数级热点聚合。 |
| `hotspot_categories.csv` | 工程类别 inclusive stack 聚合。 |
| `report.json` | 机器可读报告，包含 `callchain` 与 `resolve_stats`。 |

检查火焰图是否真的有深栈：

```bash
awk -F';' '{print NF}' target/.../qperf/stack.folded | sort -n | uniq -c
cat target/.../qperf/stack-depth-summary.csv
```

如果大部分 `NF=1`，说明当前 folded stack 仍是 leaf-only；应确认是否开启 `--full-stack`，以及 kernel 是否用 frame pointer 构建。

## symbol style

| style | 用途 |
| --- | --- |
| `full` | 保留完整 Rust demangled path，适合归因和搜索。 |
| `module` | 保留 crate 与尾部模块/函数，适合 SVG 可读性。 |
| `short` | 只看函数尾名，适合粗略热点。 |

SVG 因空间限制可能截断长 frame；需要确认完整 symbol 时看 `stack.folded`。

## marker/window

推荐所有 workload profile 使用 marker，避免 boot 样本污染：

```bash
cargo starry perf \
  --case blk-read \
  --full-stack \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --workload 'echo QPERF_BEGIN:blk-read; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; echo QPERF_END:blk-read'
```

`report.json.window` 会记录：

* `start_time`
* `stop_time`
* `duration_sec`
* `boot_samples_excluded`
* `post_window_samples_excluded`
* `warnings`

当前 window 过滤是 postprocess 级别过滤，不是 QEMU plugin runtime pause/resume。

## 聚焦 virtio/block 路径

```bash
cargo starry perf \
  --case blk-virtio \
  --full-stack \
  --qperf-metrics \
  --focus 'virtio|VirtQueue|block|memcpy|memmove' \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --workload 'echo reset > /proc/qperf_metrics; echo QPERF_BEGIN:blk; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; cat /proc/qperf_metrics; echo QPERF_END:blk'
```

典型深栈应能看到类似路径：

```text
starry_kernel::syscall::fs::io::sys_read
  -> ax_fs_ng / rsext4
  -> ax_driver::block::binding::Block::read_blocks_wait
  -> rd_block::CmdQueue::read_blocks_blocking
  -> ax_driver::virtio::block::BlockQueue::submit_request
  -> virtio_drivers::queue::VirtQueue::add_notify_wait_pop
```

## 聚焦 net 路径

先启动 host HTTP server：

```bash
python3 -m http.server 8000 --bind 0.0.0.0
```

另一个终端运行：

```bash
cargo starry perf \
  --case net-full-stack \
  --full-stack \
  --qperf-metrics \
  --timeout 300 \
  --workload-timeout 240 \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --workload 'echo reset > /proc/qperf_metrics; echo QPERF_BEGIN:net; wget -O /dev/null http://10.0.2.2:8000/tmp/axbuild/rootfs/rootfs-riscv64-alpine.img.tar.xz; cat /proc/qperf_metrics; echo QPERF_END:net'
```

典型深栈应能看到：

```text
starry_kernel::syscall::fs::io::sys_readv
  -> ax_net_ng::SocketOps::recv
  -> ax_net_ng::poll_interfaces
  -> RdNetDriver::prefetch_rx_packets
  -> rd_net::RxQueue
  -> compiler_builtins::mem::memcpy / memmove
```

## 当前验证数据

本轮验证产物位于 `target/qperf-callchain-validation/`：

| case | callchain | 多帧样本 | raw max depth | folded symbol depth |
| --- | --- | ---: | ---: | ---: |
| blk leaf | `leaf` | 0 / 577 | 1 | 1 |
| blk full-stack | `fp` | 578 / 579 | 18 | 最高 72 |
| net full-stack | `fp` | 652 / 661 | 17 | 最高 78 |

详细报告见 `docs/qperf-callchain-validation-report.md`。

## 技术边界

* qperf 仍是 QEMU TCG plugin 采样，不是 guest PMU。
* `fp` 模式依赖 frame pointer；没有 `-Cforce-frame-pointers=yes` 时无法保证深栈。
* 当前只解析 kernel ELF，用户态符号尚未纳入。
* trap、异常入口、任务切换、汇编 trampoline 可能截断调用链。
* inline frame 会让 folded symbol 层数大于 raw FP 地址层数；两者都是真实信息，但含义不同。
* `hotspot_categories.csv` 是 inclusive stack 分类，不是互斥 CPU 时间分解。
