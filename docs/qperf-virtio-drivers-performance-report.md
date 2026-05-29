# qperf virtio-drivers 性能测试与瓶颈定位报告

## 背景与目标

本次使用本仓库新的 `tools/starry-syscall-harness` 和 `tools/qperf`，对 StarryOS 在 QEMU riscv64 环境中集成的 `virtio-drivers` 路径做性能采样和瓶颈定位。目标不是做理论方案，而是保留可复现实验命令、原始输出、qperf 火焰图和由数据支持的优化方向。

需要特别说明测试对象：

- 本仓库当前锁定依赖为 crates.io `virtio-drivers 0.13.0`，见根 `Cargo.toml` 的 `virtio-drivers = { version = "0.13.0", default-features = false }` 和 `Cargo.lock` 的 registry source。
- GitHub `rcore-os/virtio-drivers` 上游 `master` 在测试时为 `d6818a8731b9422dbd06032f2fb232b6ea477814`。本次没有把依赖临时切换到该 HEAD，因为上游 master 与 crates.io 0.13.0 已存在源码差异，即使版本号相同，直接 patch 依赖会改变本仓库可复现基线。
- 因此本报告的性能数据代表“本仓库当前集成的 `virtio-drivers 0.13.0`”。上游 HEAD 作为源码对照和后续优化方向参考。

## 实验环境

| 项目 | 值 |
| --- | --- |
| 日期 | 2026-05-29 Asia/Shanghai |
| 本仓库 commit | `6e748d6b7` |
| 上游 virtio-drivers | `rcore-os/virtio-drivers` `master` `d6818a8731b9422dbd06032f2fb232b6ea477814` |
| Host kernel | `Linux LAPTOP-SAOPKIGH 5.15.167.4-microsoft-standard-WSL2 #1 SMP Tue Nov 5 00:21:55 UTC 2024 x86_64` |
| CPU | Intel Core i7-14650HX, 24 logical CPUs |
| Hypervisor | Microsoft WSL2, VT-x exposed |
| Docker image | `ghcr.io/rcore-os/tgoskits-container:latest` `sha256:b7c4600e825dcb474d1f6a6bc51b8e6616ada23a24d048fc45522a58f76eb162`, created `2026-05-08T07:18:59.511877974Z` |
| Guest arch | `riscv64` |
| QEMU devices | `virtio-blk-pci` rootfs, `virtio-net-pci` user net |
| qperf mode | QEMU TCG plugin, `mode=tb`, `freq=99`, `max_depth=64`, `queue_size=4096`, `format=all` |

qperf 是 QEMU TCG plugin。这里的 `executed_instructions`、`executed_blocks` 是 QEMU guest 执行回调统计，不是 guest PMU 的硬件 cycles/cache-miss。`--host-time` 记录 host 侧 QEMU wrapper 的 wall/user/sys CPU 时间。本轮未启用 `--host-perf`，因此没有 host `perf stat` 计数。

## qperf/harness 使用方法

基础命令模式如下：

```bash
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --repo-root /work \
  --arch riscv64 \
  --timeout 45 \
  --format all \
  --freq 99 \
  --max-depth 64 \
  --mode tb \
  --top 60 \
  --min-percent 0.5 \
  --host-time \
  --shell-init-cmd '<guest workload>' \
  --output-dir target/qperf-virtio-drivers/<case> \
  --no-docker
```

外层用 Docker 固定工具链：

```bash
docker run --rm -v "$PWD":/work -w /work \
  -e STARRY_SYSCALL_HARNESS_IN_DOCKER=1 \
  ghcr.io/rcore-os/tgoskits-container:latest bash -lc '<perf-profile commands>'
```

每个成功 profile 生成：

- `report.json`: harness 汇总结果。
- `report.md`: harness 自动报告。
- `hotspots.csv`: top symbol 表。
- `qperf/stack.folded`: folded stack 原始输入。
- `qperf/flamegraph.svg`: 火焰图。
- `qperf/summary.txt`: qperf 参数和采样统计。
- `profile.stdout` / `profile.stderr`: guest 输出和 QEMU/qperf 命令输出。

## 实验设计

| 用例 | 目的 | guest workload |
| --- | --- | --- |
| `boot` | 设备枚举、PCI transport、virtio-blk/net init 基线 | 无额外 workload，profile 到 timeout |
| `blk-read` | 文件读触发 virtio-blk read path | `echo QPERF_BLK_READ; time dd if=/usr/bin/lto-dump of=/dev/null bs=64k; sleep 1` |
| `net-wget` | HTTP 下载触发 virtio-net RX path | `echo QPERF_NET_WGET; time wget -O /dev/null http://10.0.2.2:8000/tmp/axbuild/rootfs/rootfs-riscv64-alpine.img.tar.xz; sleep 1` |
| `blk-read-focused` | 缩短采样窗口，减少 workload 结束后的 idle 稀释 | `echo QPERF_BLK_READ_FOCUSED; time dd ...; sync; poweroff -f` |
| `net-wget-focused` | 缩短采样窗口，观察 net 数据面 copy/memmove | `echo QPERF_NET_WGET_FOCUSED; time wget ...; sync; poweroff -f` |
| `vsock-probe` | 尝试启用 `vhost-vsock-pci` | `--qemu-arg=-device --qemu-arg=vhost-vsock-pci,guest-cid=3` |

`poweroff -f` 在当前 StarryOS guest 中最终触发 `Unimplemented syscall: reboot`，因此 QEMU 仍由 timeout 停止。focused 用例仍有价值，因为 timeout 从 45s 缩短为 25s，数据面热点被稀释得更少。

## 结果文件

| 用例 | report | folded stack | flamegraph |
| --- | --- | --- | --- |
| boot | `target/qperf-virtio-drivers/boot/perf/riscv64/latest/report.json` | `target/qperf-virtio-drivers/boot/perf/riscv64/latest/qperf/stack.folded` | `target/qperf-virtio-drivers/boot/perf/riscv64/latest/qperf/flamegraph.svg` |
| blk-read | `target/qperf-virtio-drivers/blk-read/perf/riscv64/latest/report.json` | `target/qperf-virtio-drivers/blk-read/perf/riscv64/latest/qperf/stack.folded` | `target/qperf-virtio-drivers/blk-read/perf/riscv64/latest/qperf/flamegraph.svg` |
| net-wget | `target/qperf-virtio-drivers/net-wget/perf/riscv64/latest/report.json` | `target/qperf-virtio-drivers/net-wget/perf/riscv64/latest/qperf/stack.folded` | `target/qperf-virtio-drivers/net-wget/perf/riscv64/latest/qperf/flamegraph.svg` |
| blk-read-focused | `target/qperf-virtio-drivers/blk-read-focused/perf/riscv64/latest/report.json` | `target/qperf-virtio-drivers/blk-read-focused/perf/riscv64/latest/qperf/stack.folded` | `target/qperf-virtio-drivers/blk-read-focused/perf/riscv64/latest/qperf/flamegraph.svg` |
| net-wget-focused | `target/qperf-virtio-drivers/net-wget-focused/perf/riscv64/latest/report.json` | `target/qperf-virtio-drivers/net-wget-focused/perf/riscv64/latest/qperf/stack.folded` | `target/qperf-virtio-drivers/net-wget-focused/perf/riscv64/latest/qperf/flamegraph.svg` |
| vsock-probe | `target/qperf-virtio-drivers/vsock-probe/perf/riscv64/latest/report.json` | 未生成 | 未生成 |

火焰图文件大小：boot 56 KiB，blk-read 50 KiB，net-wget 49 KiB，blk-read-focused 47 KiB，net-wget-focused 44 KiB。

## Baseline 数据

### qperf 运行统计

| 用例 | result | samples | dropped | failures | guest executed instructions | guest executed blocks | host elapsed | host user | host sys |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| boot | ok | 1976 | 0 | 0 | 1,825,991,247 | 392,723,472 | 20.054893s | 22.224841s | 0.156351s |
| blk-read | ok | 4454 | 0 | 0 | 4,185,872,192 | 905,971,235 | 45.054700s | 50.170774s | 1.088724s |
| net-wget | ok | 4454 | 0 | 0 | 4,408,078,862 | 871,425,485 | 45.054578s | 49.912610s | 1.003815s |
| blk-read-focused | ok | 2474 | 0 | 0 | 2,327,694,436 | 508,696,929 | 25.055819s | 27.984471s | 0.858122s |
| net-wget-focused | ok | 2474 | 0 | 0 | 2,657,789,635 | 496,566,438 | 25.054676s | 27.591356s | 0.812188s |
| vsock-probe | incomplete | 0 | 0 | 0 | 0 | 0 | 0.060224s | 0.000608s | 0.009832s |

### workload 输出

| 用例 | guest 原始输出摘要 | 说明 |
| --- | --- | --- |
| blk-read | `53601104 bytes (51.1MB) copied, 5.897731 seconds, 8.7MB/s`; `real 0m 5.95s`; `sys 0m 0.1717986s` | 来自 `profile.stdout` |
| blk-read-focused | `53601104 bytes (51.1MB) copied, 6.018606 seconds, 8.5MB/s`; `real 0m 6.08s`; `sys 0m 0.1717986s` | `poweroff -f` 后出现 `Function not implemented` |
| net-wget | `'/dev/null' saved`; `real 0m 7.66s`; `sys 0m 0.1717986s` | 下载对象为 `rootfs-riscv64-alpine.img.tar.xz`，host 文件大小 63,552,204 bytes，折算约 8.30 MB/s |
| net-wget-focused | `'/dev/null' saved`; `real 0m 7.51s`; `sys 0m 0.1717986s` | 同一文件，折算约 8.46 MB/s |
| vsock-probe | `Could not open '/dev/vhost-vsock': No such file or directory` | host/WSL2 环境没有 vhost-vsock 设备 |

## 火焰图与热点

### boot

Top symbols:

| percent | samples | symbol |
| ---: | ---: | --- |
| 4.200% | 83 | `ax_driver::pci::fdt::probe_generic_ecam+0x710` |
| 3.492% | 69 | `ax_task::wait_queue::WaitQueue::wait_until...RawMutex...+0x466` |
| 2.227% | 44 | `ax_task::wait_queue::WaitQueue::wait_until...RawMutex...+0x45a` |
| 1.771% | 35 | `_head+0x4f8` |
| 1.569% | 31 | `TaskInner::current_check_preempt_pending+0x38` |

boot 火焰图显示 profile 从内核启动开始采样，PCI ECAM probe、任务调度/等待和 allocator 是主要背景成本。后续 blk/net 用例中这些启动成本仍然存在，因此报告分析以 focused 用例的相对变化为主。

### virtio-blk

`blk-read-focused` top symbols:

| percent | samples | symbol |
| ---: | ---: | --- |
| 6.669% | 165 | `compiler_builtins::mem::memcpy+0x4a` |
| 3.072% | 76 | `virtio_drivers::queue::VirtQueue::add_notify_wait_pop+0xcc` |
| 2.910% | 72 | `ax_driver::pci::fdt::probe_generic_ecam+0x710` |
| 2.789% | 69 | `ax_task::wait_queue::WaitQueue::wait_until...RawMutex...+0x45a` |
| 2.749% | 68 | `ax_task::wait_queue::WaitQueue::wait_until...RawMutex...+0x466` |
| 2.627% | 65 | `virtio_drivers::queue::VirtQueue::add_notify_wait_pop+0xc8` |

按 folded stack 子串聚合：

| category | blk-read | blk-read-focused |
| --- | ---: | ---: |
| scheduler yield/preempt | 16.861% | 12.490% |
| PCI probe/transport | 9.026% | 11.318% |
| allocator | 7.544% | 8.407% |
| task wait/mutex | 8.577% | 7.922% |
| memcpy | 4.064% | 7.559% |
| virtqueue `add_notify_wait_pop` | 3.323% | 6.427% |
| net inflight BTree | 3.817% | 3.355% |

结论：

- `VirtQueue::add_notify_wait_pop` 在 blk 读路径中是明确热点。focused 窗口中两个偏移合计已经接近 5.7%，按子串聚合为 6.427%。
- 当前 `virtio-drivers` blk API 的同步路径为 `VirtIOBlk::read_blocks()` -> `request_read()` -> `VirtQueue::add_notify_wait_pop()`。该函数每个请求执行 add、notify、busy-spin 等待、pop，天然限制队列深度。
- 本仓库 glue 层 `drivers/ax-driver/src/virtio/block.rs` 的 `BlockQueue::submit_request()` 直接调用同步 `read_blocks()` / `write_blocks()`，`poll_request()` 直接返回 Ok，没有使用 `virtio-drivers` 已提供的 `read_blocks_nb()` / `complete_read_blocks()`。
- qperf 中的 `memcpy` 占比更高，但它包含文件系统、页缓存、用户缓冲和块层复制，不应全部归因于 `virtio-drivers`。它仍然说明 blk workload 的端到端开销被 copy 明显影响。

相关源码：

- `virtio-drivers` `queue.rs`: `VirtQueue::add()` 构造 descriptor 并写 avail ring，`fence(Ordering::SeqCst)` 后更新 idx；`add_notify_wait_pop()` 负责同步 notify 和 busy-spin 等待。
- `virtio-drivers` `device/blk.rs`: `QUEUE_SIZE = 16`，同步 `read_blocks()` 进入 `request_read()`。
- `drivers/ax-driver/src/virtio/block.rs`: glue 层同步提交，未暴露异步队列深度。

### virtio-net

`net-wget-focused` top symbols:

| percent | samples | symbol |
| ---: | ---: | --- |
| 5.416% | 134 | `compiler_builtins::mem::memcpy+0x4a` |
| 3.436% | 85 | `ax_task::wait_queue::WaitQueue::wait_until...RawMutex...+0x45a` |
| 3.355% | 83 | `ax_driver::pci::fdt::probe_generic_ecam+0x710` |
| 2.870% | 71 | `compiler_builtins::mem::memcpy+0x10c` |
| 2.749% | 68 | `ax_task::wait_queue::WaitQueue::wait_until...RawMutex...+0x466` |
| 1.738% | 43 | `compiler_builtins::mem::memmove+0x2a4` |

按 folded stack 子串聚合：

| category | net-wget | net-wget-focused |
| --- | ---: | ---: |
| scheduler yield/preempt | 15.402% | 12.975% |
| memcpy | 5.770% | 9.984% |
| allocator | 7.432% | 8.892% |
| task wait/mutex | 8.442% | 8.488% |
| PCI probe/transport | 7.454% | 7.639% |
| net inflight BTree | 4.176% | 3.395% |
| memmove | 1.796% | 3.072% |
| net RX submit/complete | 1.325% | 2.264% |
| virtqueue add/notify/pop | 0.403% | 0.526% |

结论：

- net 下载路径的主要数据面热点是 copy/move，而不是 `VirtQueue::add_notify_wait_pop`。focused 窗口中 `memcpy` 聚合 9.984%，`memmove` 聚合 3.072%。
- 本仓库 glue 层 RX 回收在 `NetInner::reclaim_rx()` 中调用 `VirtIONetRaw::receive_complete()` 后执行 `buffer.copy_within(header_len..header_len + packet_len, 0)`，这会把 virtio-net header 后面的包体整体前移。
- TX 路径 `submit_tx()` 为每包分配 staging `Vec`，填 virtio header 后 `extend_from_slice(packet)`，这也是一条固定 copy 路径。
- token/inflight 管理使用 `BTreeMap<u16, ...>`。qperf 聚合中 `RxInflight`/`TxInflight` BTree 相关符号在 net focused 中为 3.395%，说明在 QUEUE_SIZE 仅 64 的场景下，通用 BTree 结构可能不是最合适的数据结构。
- `VirtIONetRaw::receive_begin()` / `transmit_begin()` 每次 packet 提交都会 `add` 并判断 `should_notify()`。本次 profile 中 virtqueue add/notify/pop 占比低于 copy 和 BTree，但仍可作为后续高 PPS workload 的观察点。

相关源码：

- `virtio-drivers` `device/net/dev_raw.rs`: `transmit_begin()`、`receive_begin()` 调用 virtqueue add 并按 `should_notify()` notify。
- `drivers/ax-driver/src/virtio/net.rs`: `submit_tx()` staging Vec copy，`reclaim_rx()` `copy_within()`，`BTreeMap<u16, TxInflight/RxInflight>`。

### virtio-vsock

本轮未获得 vsock 数据面 profile。原因是 QEMU 启动 `vhost-vsock-pci` 时失败：

```text
qemu-system-riscv64: -device vhost-vsock-pci,guest-cid=3: Could not open '/dev/vhost-vsock': No such file or directory
Error: qperf QEMU run failed before producing samples: exit status: 1
```

因此本报告不对 vsock 性能下定量结论。只能基于源码记录后续需要验证的风险点：

- `virtio-drivers` vsock `QUEUE_SIZE = 8`。
- `send_packet_to_tx_queue()` 对每个包调用 `tx.add_notify_wait_pop()`，数据面可能和 blk 同样受同步 notify/wait 限制。
- 需要在具备 `/dev/vhost-vsock` 的 host 上重新运行 vsock connect/send/recv workload，才能确认。

## 公共瓶颈分析

### 1. 采样窗口从 boot 开始，启动成本会污染数据面结论

所有 profile 都从 QEMU 启动开始采样。`probe_generic_ecam` 在 boot、blk、net 中都稳定出现，focused 用例仍有 7% 到 11% 的 PCI probe/transport 聚合占比。这是设备枚举和初始化成本，不等价于运行期吞吐瓶颈。

下一步最好扩展 harness/qperf 支持 guest shell prompt 后再开始采样，或者支持 workload 结束后由 harness 主动停止 QEMU。当前 guest `poweroff -f` 失败，不能作为结束信号。

### 2. blk 的同步队列模型值得优先优化

证据：

- `blk-read-focused` 中 `VirtQueue::add_notify_wait_pop` 聚合 6.427%。
- 同步 blk glue 层没有利用 non-blocking API，导致请求不能自然形成队列深度。
- `VirtIOBlk` 内部队列大小为 16，但同步路径每次只提交一个请求，队列大小没有被利用。

低风险验证方向：

1. 在 `ax-driver` blk glue 层建立最小 pending request 表，使用 `read_blocks_nb()` / `complete_read_blocks()` 做 A/B。
2. 先只支持读请求并保持 fallback 到同步路径，避免一次性改写整个 block queue contract。
3. 用同一 `dd` workload 复测 `add_notify_wait_pop` 占比、host elapsed、guest executed instructions 和 guest real time。

### 3. net 的 copy/move 和 inflight 管理是当前最明确的数据面热点

证据：

- `net-wget-focused` 中 `memcpy` 聚合 9.984%，`memmove` 聚合 3.072%。
- `NetInner::reclaim_rx()` 明确执行 `copy_within()` 去除 virtio-net header。
- `submit_tx()` 每包创建 staging Vec 并复制 payload。
- `RxInflight`/`TxInflight` BTree 聚合 3.395%。

低风险验证方向：

1. 如果上层 `rd_net` API 允许，RX buffer 保留 headroom 或返回 packet offset，避免 `copy_within()`。
2. TX 侧改为复用 per-queue staging buffer 池，或让上层 DMA buffer 预留 virtio-net header 空间。
3. 将 QUEUE_SIZE=64 的 token 映射从 `BTreeMap` 替换为固定数组或 slab，A/B 观察 `RxInflight`/`TxInflight` 聚合占比。

### 4. PCI notify 有明确源码 TODO，但本轮不是最大热点

`virtio-drivers` PCI transport `notify()` 每次写 queue select 后读 `queue_notify_off`，源码中已有 `TODO: Consider caching this somewhere (per queue).`。本轮 profile 中 notify 没有单独成为 top symbol，但它是低风险、局部的优化候选。需要为 notify 次数增加计数或在高 PPS workload 下复测后再改。

### 5. qperf 目前不能替代硬件 PMU

本轮使用的 qperf 能回答“guest 内哪些函数/栈占用采样最多”，但不能直接回答：

- 精确 guest cycles。
- guest cache misses。
- guest PMU retired instructions。
- virtqueue 队列深度时间序列。
- notify/kick 次数。
- copy 字节数。
- alloc/free 次数。
- lock wait time。

这些指标需要后续在 qperf plugin、virtio-drivers 或 glue 层增加轻量计数器，或结合 host perf/ftrace/bpftrace。

## 结论

1. 本仓库当前 `virtio-drivers 0.13.0` 集成路径可被新的 harness/qperf 稳定采样，boot、virtio-blk、virtio-net 均生成了 `report.json`、folded stack 和 SVG 火焰图，采样 dropped/failures 均为 0。
2. virtio-blk 的首要瓶颈候选是同步 `add_notify_wait_pop` 路径和没有利用 queue depth 的 glue 层。`blk-read-focused` 中该路径聚合为 6.427%。
3. virtio-net 的首要瓶颈候选是 copy/move 和 inflight BTree 管理。`net-wget-focused` 中 `memcpy` 为 9.984%，`memmove` 为 3.072%，net inflight BTree 为 3.395%。
4. vsock 在当前 WSL2 host 上无法测试，阻塞点是 `/dev/vhost-vsock` 缺失。本报告不编造 vsock 数据面数字。
5. 下一步最值得做的小补丁是 net RX 去 `copy_within()` 或 token map 固定数组化；blk 的异步队列化收益可能更大，但改动面和接口风险也更高，应先做最小 pending-read 原型。

## 复现命令

boot:

```bash
docker run --rm -v "$PWD":/work -w /work \
  -e STARRY_SYSCALL_HARNESS_IN_DOCKER=1 \
  ghcr.io/rcore-os/tgoskits-container:latest bash -lc '
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --repo-root /work --arch riscv64 --timeout 20 --format all \
  --freq 99 --max-depth 64 --mode tb --top 50 --min-percent 0.5 \
  --host-time --output-dir target/qperf-virtio-drivers/boot --no-docker
'
```

blk:

```bash
docker run --rm -v "$PWD":/work -w /work \
  -e STARRY_SYSCALL_HARNESS_IN_DOCKER=1 \
  ghcr.io/rcore-os/tgoskits-container:latest bash -lc '
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --repo-root /work --arch riscv64 --timeout 25 --format all \
  --freq 99 --max-depth 64 --mode tb --top 80 --min-percent 0.3 \
  --host-time \
  --shell-init-cmd "echo QPERF_BLK_READ_FOCUSED; time dd if=/usr/bin/lto-dump of=/dev/null bs=64k; sync; poweroff -f" \
  --output-dir target/qperf-virtio-drivers/blk-read-focused --no-docker
'
```

net:

```bash
docker run --rm -v "$PWD":/work -w /work \
  -e STARRY_SYSCALL_HARNESS_IN_DOCKER=1 \
  ghcr.io/rcore-os/tgoskits-container:latest bash -lc '
python3 -m http.server 8000 --bind 0.0.0.0 >/tmp/qperf-http.log 2>&1 &
server=$!
trap "kill $server 2>/dev/null || true" EXIT
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --repo-root /work --arch riscv64 --timeout 25 --format all \
  --freq 99 --max-depth 64 --mode tb --top 80 --min-percent 0.3 \
  --host-time \
  --shell-init-cmd "echo QPERF_NET_WGET_FOCUSED; time wget -O /dev/null http://10.0.2.2:8000/tmp/axbuild/rootfs/rootfs-riscv64-alpine.img.tar.xz; sync; poweroff -f" \
  --output-dir target/qperf-virtio-drivers/net-wget-focused --no-docker
'
```

vsock probe:

```bash
docker run --rm -v "$PWD":/work -w /work \
  -e STARRY_SYSCALL_HARNESS_IN_DOCKER=1 \
  ghcr.io/rcore-os/tgoskits-container:latest bash -lc '
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --repo-root /work --arch riscv64 --timeout 12 --format folded \
  --freq 99 --max-depth 64 --mode tb --top 30 --min-percent 0.5 \
  --host-time \
  --qemu-arg=-device --qemu-arg=vhost-vsock-pci,guest-cid=3 \
  --output-dir target/qperf-virtio-drivers/vsock-probe --no-docker
'
```
