# qperf VirtIO 优化实践报告

## 背景与目标

本报告记录基于 `docs/qperf-virtio-performance-analysis.md` 的实际优化尝试。目标是保持补丁小、风险低、可回滚，并尽量用本地 qperf 结果做 A/B 验证。

参考的 openKylin vsock 文档强调：优化前先把实验环境、命令、重复次数和指标固定；优化项要围绕真实瓶颈，例如 credit update、buffer、锁、copy、alloc、notify/kick 和队列行为；如果优化无效，也应记录原因。

首次分析时，本地 qperf 的主要限制是：`perf-profile` 只能跑 StarryOS 默认 boot profile，不能注入专门的 vsock/net/blk workload。因此早期补丁分为两类：

- 已用 qperf 直接验证效果的 measurement 修正。
- 已编译和 clippy 验证，但缺少专门 workload，暂不能声明吞吐/延迟收益的 data path 微优化。

随后已修复 qperf/harness 功能：`perf-profile` 支持 `--shell-init-cmd`、`--shell-prefix`、`--qemu-arg`，qperf plugin 在 QEMU timeout 退出时也能生成 `qperf.summary.txt`。本报告追加一轮 workload 复测，用于重新判断已有微优化是否有可量化依据。

## 实验环境

| 项目 | 值 |
| --- | --- |
| 日期 | 2026-05-29 Asia/Shanghai |
| 仓库 commit | `6e748d6b7` |
| 宿主系统 | `Linux LAPTOP-SAOPKIGH 5.15.167.4-microsoft-standard-WSL2` |
| CPU | Intel Core i7-14650HX, 24 logical CPUs |
| Docker image | `ghcr.io/rcore-os/tgoskits-container:latest` |
| QEMU | `/opt/qemu-10.2.1/bin/qemu-system-riscv64` |
| qperf 参数 | `--arch riscv64 --timeout 20 --format folded --freq 99 --max-depth 64 --mode tb --top 30 --min-percent 2.0` |
| QEMU 设备 | `virtio-blk-pci`, `virtio-net-pci`, user net |
| vsock 设备 | 未配置 |

baseline 与 patched 均重复 3 次，输出位于：

- `target/qperf-virtio-experiment/baseline-run{1,2,3}/perf/riscv64/latest`
- `target/qperf-virtio-experiment/patched-run{1,2,3}/perf/riscv64/latest`
- `target/qperf-virtio-experiment/diff-run{1,2,3}/perf-diff`

## qperf/qpef 使用方法

baseline：

```bash
for i in 1 2 3; do
  python3 tools/starry-syscall-harness/harness.py perf-profile \
    --arch riscv64 \
    --timeout 20 \
    --format folded \
    --freq 99 \
    --max-depth 64 \
    --mode tb \
    --top 30 \
    --min-percent 2.0 \
    --output-dir "target/qperf-virtio-experiment/baseline-run${i}"
done
```

patched：

```bash
for i in 1 2 3; do
  python3 tools/starry-syscall-harness/harness.py perf-profile \
    --arch riscv64 \
    --timeout 20 \
    --format folded \
    --freq 99 \
    --max-depth 64 \
    --mode tb \
    --top 30 \
    --min-percent 2.0 \
    --output-dir "target/qperf-virtio-experiment/patched-run${i}"
done
```

diff：

```bash
for i in 1 2 3; do
  python3 tools/starry-syscall-harness/harness.py perf-diff \
    --baseline "target/qperf-virtio-experiment/baseline-run${i}/perf/riscv64/latest" \
    --compare "target/qperf-virtio-experiment/patched-run${i}/perf/riscv64/latest" \
    --top 20 \
    --output-dir "target/qperf-virtio-experiment/diff-run${i}"
done
```

注意：本仓库未发现单独的 `qpef` 命令，本报告按 `qperf` 工具记录。

## Baseline 数据

| run | result | samples | top1 | top2 | top3 |
| --- | --- | ---: | --- | --- | --- |
| baseline-1 | ok | 1976 | `0x8001359c` 76 / 3.8462% | `0x8000b04e` 72 / 3.6437% | `0x8000b05a` 63 / 3.1883% |
| baseline-2 | ok | 1979 | `0x8001359c` 94 / 4.7499% | `0x8000b04e` 73 / 3.6887% | `0x8000b05a` 58 / 2.9308% |
| baseline-3 | ok | 1979 | `0x8001359c` 91 / 4.5983% | `0x8000b04e` 72 / 3.6382% | `0x8000b05a` 58 / 2.9308% |

baseline 暴露出测量瓶颈：top 热点多为 `0x800...` 裸地址，qperf 没有把 QEMU 采到的低地址物理别名映射到 StarryOS high-half kernel text。

## 瓶颈分析

本次 qperf 直接证明的问题：

- qperf 只检测 `.text` high-half virtual range，缺少 `.head.text` 和低地址 physical alias，导致 baseline 不能解析主要热点。
- boot profile 的稳定热点集中在启动、PCI/FDT probe、显示锁等待、调度检查，不是 VirtIO data path workload。

本次代码审阅发现但尚未被 workload 定量验证的问题：

- virtio-net TX 路径先调用一次 `fill_buffer_header` 计算 header 长度，再分配并零填充 `header + packet`，随后再调用一次 `fill_buffer_header`，会带来额外 header 查询和 packet 大小的零填充。
- block binding 的 `read_block`/`write_block` 先通过 `self.block_size()` 锁一次 queue，再为实际 I/O 锁一次 queue，每次调用多一次 `SpinNoIrq` lock/unlock。
- vsock poll loop 每次 `poll_vsock_interfaces` 都先分配 4KiB 临时 buffer，即使没有 pending event 且 `poll_event()` 返回 `None`。

## 优化尝试

### Patch 1: qperf text/alias 检测修正

文件：`scripts/axbuild/src/starry/perf.rs`

改动：

- qperf kernel text range 从只看 `.text` 改为合并 `.head.text` 和 `.text`。
- 如果 `AX_CONFIG_PATH` 没有提供物理基址，则为 high-half virtual text 增加低 32 位 physical alias fallback。
- 生成 QEMU plugin 参数时传入 `filter_alias_start`、`filter_alias_end`、`filter_alias_offset`。

动机：

- baseline 热点 `0x8001359c`、`0x8000b04e`、`0x8000b05a` 无法符号化，导致 qperf 输出不能指导 VirtIO 优化。

实测结果：

- patched stderr 显示：

```text
qperf: detected kernel text virtual range: 0xffffffff80000000..0xffffffff802a4dc2
qperf: detected kernel text physical alias: 0x80000000..0x802a4dc2
filter_alias_offset=0xffffffff00000000
```

- patched top 热点从裸地址变为符号：

| baseline 地址 | patched 符号 |
| --- | --- |
| `0x8001359c` | `ax_driver::pci::fdt::probe_generic_ecam+0x710` |
| `0x8000b04e` / `0x8000b05a` | `ax_task::wait_queue::WaitQueue::wait_until(... ax_display ... RawMutex::lock_after_prepare)` |
| `0x800004f8` | `_head+0x4f8` |

收益：

- qperf 报告从“地址列表”变成可定位的函数热点列表，这是后续 VirtIO 优化的前置条件。

风险：

- low-32-bit alias 是 fallback，仅在 config 没有物理基址时启用；对非 high-half 或特殊映射平台可能不适用。
- 该补丁只影响 qperf 测量路径，不改变内核运行逻辑。

### Patch 2: virtio-net TX staging 减少零填充和重复 header 查询

文件：`drivers/ax-driver/src/virtio/net.rs`

改动：

- 删除 `raw_header_len()`。
- TX staging 使用 `Vec::with_capacity(16 + packet_len)`，先只 resize 16 字节给 `fill_buffer_header()` 写 header。
- `truncate(header_len)` 后 `extend_from_slice(packet)`，避免把整个 packet 区域先零填充。

动机：

- TX 数据路径中 packet payload 本来马上会被 copy 覆盖，按 `header + packet_len` 全量 `vec![0; ...]` 的零填充没有必要。
- 原逻辑为了计算 header 长度额外调用一次 `fill_buffer_header()`。

本地验证：

- 编译和 clippy 通过。
- 当前 qperf 没有网络 TX workload，不能声明吞吐收益。

风险：

- 依赖 virtio-net header 不超过 16 字节；旧实现的 `raw_header_len()` 也使用 `[0_u8; 16]`，因此行为边界与旧代码一致。
- 仍然保留 staging copy，因为 `VirtIONetRaw::transmit_begin(&staging)` 后需要在 complete 时用同一 staging buffer。

### Patch 3: virtio-blk binding 避免重复 queue lock

文件：`drivers/ax-driver/src/block/binding.rs`

改动：

- `read_block` 和 `write_block` 先取 `use_irq_completion()`，再锁 `self.queue`。
- 在同一把 queue lock 内读取 `block_size()` 并提交 I/O，避免 `self.block_size()` 单独锁一次。

动机：

- 每次 block read/write 少一次 `SpinNoIrq` lock/unlock。
- 改动不改变请求切分、等待和错误处理。

本地验证：

- 编译和 clippy 通过。
- qperf 默认 boot profile 没有显著 virtio-blk data path top 热点，无法量化该补丁对吞吐/延迟的收益。

风险：

- buffer 长度校验时 queue lock 持有时间略提前；校验本身很短，风险低。

### Patch 4: vsock poll loop 懒分配 RX 临时 buffer

文件：`os/arceos/modules/axnet-ng/src/device/vsock.rs`

改动：

- `poll_vsock_interfaces()` 不再进入函数就分配 4KiB buffer。
- 改为 `Option<Vec<u8>>`，只有处理 pending event 或 `poll_event()` 返回 event 时才分配一次，并在本轮事件处理内复用。

动机：

- 空闲 poll 是高频路径，旧逻辑在没有事件时仍分配 4KiB 临时 buffer。

本地验证：

- 编译和 clippy 通过。
- 当前 riscv64 qperf QEMU 没有 vsock device，无法运行 vsock workload 验证。

风险：

- 只改变 buffer 分配时机，不改变 `handle_vsock_event()` 和 `dev.recv()` 的数据语义。

## A/B 对比结果

patched 三次运行：

| run | result | samples | top1 | top2 | top3 |
| --- | --- | ---: | --- | --- | --- |
| patched-1 | ok | 1979 | `probe_generic_ecam+0x710` 90 / 4.5478% | `RawMutex::lock_after_prepare+0x45a` 70 / 3.5371% | `RawMutex::lock_after_prepare+0x466` 63 / 3.1834% |
| patched-2 | ok | 1979 | `probe_generic_ecam+0x710` 84 / 4.2446% | `RawMutex::lock_after_prepare+0x45a` 52 / 2.6276% | `RawMutex::lock_after_prepare+0x466` 44 / 2.2233% |
| patched-3 | ok | 1979 | `probe_generic_ecam+0x710` 103 / 5.2046% | `RawMutex::lock_after_prepare+0x466` 68 / 3.4361% | `RawMutex::lock_after_prepare+0x45a` 67 / 3.3855% |

由于 Patch 1 改变了符号化，直接 `perf-diff` 主要显示“裸地址消失、符号名出现”。例如 run1：

```text
+4.55% probe_generic_ecam+0x710 (0.00% -> 4.55%)
-3.85% 0x8001359c (3.85% -> 0.00%)
-3.64% 0x8000b04e (3.64% -> 0.00%)
+3.54% RawMutex::lock_after_prepare+0x45a (0.00% -> 3.54%)
```

因此下面按同一热点别名分组比较：

| group | baseline avg samples | baseline avg % | patched avg samples | patched avg % | 结论 |
| --- | ---: | ---: | ---: | ---: | --- |
| qperf total samples | 1978.00 +/- 1.73 | n/a | 1979.00 +/- 0.00 | n/a | 采样稳定 |
| PCI/FDT probe alias | 87.00 +/- 9.64 | 4.3981 +/- 0.4840 | 92.33 +/- 9.71 | 4.6657 +/- 0.4907 | 无稳定性能变化 |
| display RawMutex wait alias | 132.00 +/- 2.65 | 6.6735 +/- 0.1396 | 127.00 +/- 12.17 | 6.4173 +/- 0.6147 | 变化在波动内 |
| `_head+0x4f8` alias | 36.33 +/- 3.51 | 1.8370 +/- 0.1791 | 40.00 +/- 3.00 | 2.0212 +/- 0.1516 | 非 VirtIO 目标 |
| preempt check | 95.00 +/- 8.54 | 4.8026 +/- 0.4286 | 109.33 +/- 8.96 | 5.5247 +/- 0.4529 | 调度波动，非 data path 结论 |

结论：

- Patch 1 有明确收益：qperf 符号化可解释性提升，后续分析可以定位到函数。
- Patch 2/3/4 是低风险微优化，但当前 boot-only qperf 不能证明吞吐或延迟收益。
- 本次没有观察到 virtio-blk/net/vsock data path 在 top profile 中成为稳定瓶颈。

## workload 复测结果

复测命令使用新的 workload 注入入口，所有成功样本均为 `--timeout 30 --format folded --freq 99 --max-depth 64 --mode tb --top 40 --min-percent 0.5`，输出位于 `target/qperf-virtio-rerun`。成功样本的 `qperf.summary.txt` 均显示 `dropped_samples = 0`、`sample_failures = 0`。

### boot/idle

| run | result | samples | top1 |
| --- | --- | ---: | --- |
| boot-1 | ok | 2969 | `probe_generic_ecam+0x710` 129 / 4.3449% |
| boot-2 | ok | 2969 | `probe_generic_ecam+0x710` 150 / 5.0522% |
| boot-3 | ok | 2969 | `probe_generic_ecam+0x710` 143 / 4.8164% |

boot/idle 仍主要反映启动探测、显示 wait queue、调度和空闲路径，不适合作为 VirtIO 数据面优化收益依据。

### virtio-blk: rootfs 文件冷读

guest workload：

```sh
time dd if=/usr/bin/lto-dump of=/dev/null bs=64k
```

原始结果：

| run | bytes | dd seconds | throughput | real |
| --- | ---: | ---: | ---: | ---: |
| blk-read-1 | 53,601,104 | 4.299533s | 11.9MB/s | 4.35s |
| blk-read-2 | 53,601,104 | 4.369256s | 11.7MB/s | 4.41s |
| blk-read-3 | 53,601,104 | 4.160124s | 12.3MB/s | 4.21s |

平均 `real = 4.323s`，标准差 `0.103s`。

qperf 归类结果：

| category | avg | range | 判断 |
| --- | ---: | ---: | --- |
| `memcpy` | 4.199% | 3.806%-4.412% | 读后 copy 成本稳定可见 |
| `VirtQueue::add_notify_wait_pop` | 3.795% | 3.402%-4.178% | 同步 virtqueue submit/notify/wait/pop 是 blk-read 主要热点 |
| display wait lock | 8.837% | 8.690%-8.993% | 仍有控制台/显示噪声，但低于 boot |

对已有 Patch 3 的判断：避免一次重复 queue lock 是正确的小优化，但这轮数据表明更大的成本在 `VirtIOBlk::read_blocks()` 内部的同步 virtqueue 完成路径，以及 `read_block()` 把 `BlockData` 再 copy 到目标 buffer 的路径。后续不应继续围绕单次 lock 微调，应优先评估批量/异步提交和减少 block copy。

### virtio-net/vnet: HTTP RX

宿主容器启动：

```bash
python3 -m http.server 8000 --bind 0.0.0.0
```

guest workload：

```sh
time wget -O /dev/null http://10.0.2.2:8000/tmp/axbuild/rootfs/rootfs-riscv64-alpine.img.tar.xz
```

原始结果：

| run | transfer | real |
| --- | ---: | ---: |
| net-wget-1 | 60.6M | 5.65s |
| net-wget-2 | 60.6M | 5.57s |
| net-wget-3 | 60.6M | 5.73s |

平均 `real = 5.650s`，标准差 `0.080s`。

qperf 归类结果：

| category | avg | range | 判断 |
| --- | ---: | ---: | --- |
| `memcpy` | 5.345% | 5.086%-5.526% | RX 数据搬运是一阶热点 |
| `memmove` | 1.381% | 1.145%-1.550% | header 去除/packet 移动成本可见 |
| `NetRxQueue::submit` | 2.010% | 1.819%-2.190% | RX buffer 重新投递路径稳定出现 |
| `RxInflight` | 3.919% | 3.605%-4.109% | inflight 管理成本存在，但和 boot 噪声需继续拆分 |

对已有 Patch 2 的判断：Patch 2 只优化 TX staging；本次 `wget` 是 RX 主导，不能证明 TX patch 的收益。新的数据指向 RX：`reclaim_rx()` 中 `buffer.copy_within(header_len..header_len + packet_len, 0)` 以及 `BTreeMap<u16, RxInflight>` 管理值得优先做小补丁验证。

### virtio-vsock

使用新 `--qemu-arg` 探测：

```bash
--qemu-arg=-device \
--qemu-arg=vhost-vsock-pci,guest-cid=3
```

QEMU 失败：

```text
Could not open '/dev/vhost-vsock': No such file or directory
```

结论：当前宿主/Docker 环境不能创建 `vhost-vsock-pci`，vsock 数据面仍无法本地量化。已有 Patch 4 只能维持为“代码审阅支持的低风险空闲路径优化”，不能声明性能收益。

## 正确性验证

格式：

```bash
cargo fmt --check
```

结果：通过。

clippy：

```bash
docker run --rm -v "$PWD":/work -w /work ghcr.io/rcore-os/tgoskits-container:latest bash -lc '
set -euo pipefail
cargo xtask clippy --package ax-driver
cargo xtask clippy --package ax-net-ng
cargo xtask clippy --package tg-xtask
'
```

结果：

- `ax-driver`：39 个 clippy checks 全部通过。
- `ax-net-ng`：2 个 clippy checks 全部通过。
- `tg-xtask`：1 个 clippy check 通过。

qperf：

- baseline 3 次 `result: ok`。
- patched 3 次 `result: ok`。
- patched 3 次 samples 均为 `1979`。
- workload 复测中 boot/blk/net 共 9 次 `result: ok`，samples 为 `2968-2969`，`dropped_samples = 0`，`sample_failures = 0`。
- blk-read 三次均完成 53,601,104 bytes 冷读；net-wget 三次均完成 60.6M HTTP RX。
- vsock probe 未进入 guest，QEMU 失败原因为宿主/容器缺少 `/dev/vhost-vsock`。
- QEMU 到达 StarryOS shell，`eth0` DHCP 成功。
- 未发现 panic/oops/BUG/hang。
- baseline 和 patched 都有既有的 `kprobe selftest failed` / `kretprobe selftest failed` 日志，非本次补丁新增。

宿主机直接运行 `cargo xtask starry perf --help` 时遇到 `~/.cargo` registry cache permission 问题；按项目约束，Starry/qperf 命令改在 Docker 内确认并执行。

## 代码改动清单

本次没有创建 git commit，改动保留为工作区 patch，便于继续拆分：

1. `scripts/axbuild/src/starry/perf.rs`
   - qperf measurement 修正。
2. `drivers/ax-driver/src/virtio/net.rs`
   - virtio-net TX staging 微优化。
3. `drivers/ax-driver/src/block/binding.rs`
   - virtio-blk/block binding lock 微优化。
4. `os/arceos/modules/axnet-ng/src/device/vsock.rs`
   - vsock poll lazy allocation 微优化。

建议提交拆分：

- `fix(starry): map qperf low text aliases for StarryOS`
- `perf(ax-driver): reduce virtio-net tx staging work`
- `perf(ax-driver): avoid duplicate block queue locking`
- `perf(ax-net-ng): lazily allocate vsock poll buffer`

## 结论和下一步建议

值得继续优化的路径：

- qperf/harness 本身：workload 注入和 extra QEMU args 已可用；下一步应把常用 blk/net/vsock workload 固化为可复用 case，并记录 copy bytes、submit/completion、kick/notify 等结构化计数。
- virtio-net RX：`wget` RX 负载已证明 copy/move、`NetRxQueue::submit` 和 `RxInflight` 管理是优先方向；TX staging patch 仍需要 TX workload 单独验证。
- virtio-blk：`dd` 冷读已证明同步 `VirtQueue::add_notify_wait_pop` 和读后 copy 是主要热点；应优先评估批量提交、异步完成和减少中间 block copy。
- virtio-vsock：先解决宿主/Docker `/dev/vhost-vsock` 透传，再建立 host/guest workload，之后评估 credit update、poll interval、buffer 和锁竞争。

本次无效或未能证明有效的点：

- 默认 boot profile 不能证明 net/vsock/blk throughput 优化；必须使用 workload 注入后的 profile。
- 当前 `perf-diff` 对 measurement 修正前后的比较会被符号化变化污染，不能直接作为性能收益。
- vsock 路径在当前环境仍没有 runtime 覆盖，因为 QEMU 无法打开 `/dev/vhost-vsock`。

下一步最小可复现实验建议：

1. 为 blk/net/vsock 各加一个不会引入新依赖的 guest microbench 或 busybox 命令脚本。
2. 增加 `qemu-riscv64-vsock` 配置，文档化 `modprobe vhost_vsock`、`/dev/vhost-vsock` 和 guest CID。
3. 在 driver 层临时加入可开关统计项：submit/completion 次数、kick/notify 次数、copy bytes、alloc count、poll idle/event 次数。
4. 针对 net RX 的 `copy_within()` 和 `BTreeMap<u16, RxInflight>` 做 1-2 个最小补丁，并用 net-wget workload 3 次 A/B。
5. 针对 blk 的同步 virtqueue wait/pop 和读后 copy 做 1-2 个最小补丁，并用 blk-read workload 3 次 A/B。
