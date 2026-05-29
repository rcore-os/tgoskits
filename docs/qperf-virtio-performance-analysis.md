# qperf VirtIO 性能分析报告

## 背景与目标

本报告记录一次在本仓库内使用本地 qperf 工具对 StarryOS VirtIO 路径进行的性能分析实践。目标不是给出泛泛方案，而是先确认本仓库 qperf/qpef 的实际入口、运行约束和可采样范围，再在可运行范围内建立 baseline，并据此选择最小可验证补丁。

参考资料：

- <https://gitee.com/openkylin/x-kernel/blob/debin/vsock-tx-limit/docs/vsock-tx-performance-analysis.md>
- <https://gitee.com/openkylin/x-kernel/blob/debin/vsock-tx-limit/docs/vsock-tx-optimization-report.md>

参考文档的方法可以概括为：

- 固定环境、命令、参数和重复次数，保留原始结果。
- 先用采样和阶段统计定位热点，再讨论 tx/rx、credit、queue、copy、alloc、notify/kick、锁竞争等具体瓶颈。
- 优化以小补丁为单位做 A/B 对比，允许记录无效或回退的尝试。
- 对 vsock 这类异步/信用流控路径，优先关注 buffer 大小、credit update、事件通知频率、锁拆分、批处理和不必要分配。

本仓库未发现独立名为 `qpef` 的工具；实际可用入口是 `tools/qperf` 和 `tools/starry-syscall-harness/harness.py perf-profile`。下文按本仓库的 `qperf` 记录。

## 本地 qperf/qpef 使用方法

仓库内 qperf 相关入口：

- `tools/qperf/`：QEMU TCG plugin 与 analyzer。
- `scripts/axbuild/src/starry/perf.rs`：`cargo xtask starry perf` 的实现，负责构建 qperf、构建 StarryOS、准备 rootfs、生成 QEMU 参数、注入 `-plugin`、运行 analyzer。
- `tools/starry-syscall-harness/harness.py perf-profile`：推荐入口，会在 Docker 中运行 StarryOS/qperf 并生成 `report.json`、`report.md`、`hotspots.csv`。
- `tools/starry-syscall-harness/harness.py perf-diff`：比较两次 folded stack。

已确认的 help 输出：

```text
python3 tools/starry-syscall-harness/harness.py perf-profile --help

--arch {riscv64,loongarch64}
--timeout TIMEOUT
--format {folded,svg,pprof,all}
--freq FREQ
--max-depth MAX_DEPTH
--mode {tb,insn}
--top TOP
--min-percent MIN_PERCENT
--output-dir OUTPUT_DIR
--debug
--kernel-filter
```

容器内 `cargo xtask starry perf --help`：

```text
Build and profile StarryOS with qperf

Usage: tg-xtask starry perf [OPTIONS]

Options:
      --arch <ARCH>
      --freq <FREQ>            [default: 99]
      --out <OUT>
      --format <FORMAT>        [default: all] [possible values: folded, svg, pprof, all]
      --max-depth <MAX_DEPTH>  [default: 64]
      --timeout <SECONDS>      [default: 20]
      --mode <MODE>            [default: tb] [possible values: tb, insn]
      --top <TOP>              [default: 20]
      --debug
      --kernel-filter
```

本次 baseline 命令：

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

本次 patched 命令与 baseline 参数一致，仅输出目录改为 `patched-run${i}`。

## 实验环境

| 项目 | 值 |
| --- | --- |
| 日期 | 2026-05-29 Asia/Shanghai |
| 仓库 commit | `6e748d6b7` |
| 宿主系统 | `Linux LAPTOP-SAOPKIGH 5.15.167.4-microsoft-standard-WSL2` |
| CPU | Intel Core i7-14650HX, 24 logical CPUs |
| 虚拟化 | WSL2, Microsoft hypervisor, VT-x |
| Docker image | `ghcr.io/rcore-os/tgoskits-container:latest` |
| 容器工具 | `/usr/sbin/debugfs`, `riscv64-linux-musl-gcc`, `/opt/qemu-10.2.1/bin/qemu-system-riscv64` |
| Cargo | `cargo 1.97.0-nightly (2026-04-24)` in container |
| Starry target | `riscv64gc-unknown-none-elf`, release build |
| qperf 参数 | `freq=99`, `mode=tb`, `max_depth=64`, `queue_size=4096`, `timeout=20s`, `format=folded` |
| QEMU | `-machine virt -m 512M -nographic -cpu rv64`, 1 vCPU |

`harness.py doctor` 结果：

```text
docker: ok
ghcr.io/rcore-os/tgoskits-container:latest: ok
container-tools: ok
repo_root: /home/cg24/tgoskits
```

QEMU 设备配置来自 `os/StarryOS/configs/qemu/qemu-riscv64.toml`：

```toml
args = [
  "-m", "512M",
  "-nographic",
  "-cpu", "rv64",
  "-device", "virtio-blk-pci,drive=disk0",
  "-drive", "id=disk0,if=none,format=raw,file=${workspace}/tmp/axbuild/rootfs/rootfs-riscv64-alpine.img",
  "-device", "virtio-net-pci,netdev=net0",
  "-netdev", "user,id=net0",
]
```

StarryOS riscv64 build feature 包含：

```toml
"ax-driver/virtio-blk"
"ax-driver/virtio-net"
"ax-driver/virtio-socket"
```

但本次 qperf QEMU 参数没有 `virtio-vsock` 或 `vhost-vsock` 设备，因此 vsock 代码被编译，但没有被本次 riscv64 qperf profile 运行时覆盖。

## 相关代码路径

本次检查的路径：

- virtio-vsock driver：`drivers/ax-driver/src/virtio/vsock.rs`
- vsock runtime/poll：`os/arceos/modules/axnet-ng/src/device/vsock.rs`
- vsock connection manager：`os/arceos/modules/axnet-ng/src/vsock/connection_manager.rs`
- virtio-net：`drivers/ax-driver/src/virtio/net.rs`
- virtio-blk device：`drivers/ax-driver/src/virtio/block.rs`
- block binding/queue lock：`drivers/ax-driver/src/block/binding.rs`
- block async queue：`drivers/blk/rd-block/src/lib.rs`
- qperf Starry runner：`scripts/axbuild/src/starry/perf.rs`

初步风险点：

- vsock：poll loop 在空闲路径中的临时 buffer 分配、`VSOCK_DEVICE` 与 connection manager 锁、credit update 频率。
- net/vnet：TX staging copy、packet buffer 零填充、TX/RX 共享 raw device 访问、queue size 64。
- blk：`Block` 外层 `SpinNoIrq` 锁、每次请求的 block size 查询、同步等待路径、32 sector DMA buffer 上限。
- qperf：guest 物理低地址和 high-half virtual text 的符号解析一致性。

## 实验设计与可运行性

| 路径 | 计划实验 | 当前本地结果 |
| --- | --- | --- |
| vsock | 配置 guest CID，host/guest 做 send/recv，采 tx/rx、credit、copy、锁热点 | 当前 riscv64 qperf QEMU 参数无 vsock 设备，harness 也没有 workload/shell 注入入口；未能实测 |
| net/vnet | DHCP 后运行 ping/iperf/wget 或 guest 内收发包循环，采 TX/RX data path | QEMU 有 virtio-net/user net，启动时 DHCP 成功；当前 qperf 只能采默认 boot/profile，未能注入网络吞吐 workload |
| blk | rootfs boot read baseline，进一步运行 guest 内读写 microbench | QEMU 有 virtio-blk/rootfs，boot 会覆盖 init/read 路径；当前 qperf 无专门 block workload 注入，未测吞吐/延迟 |

以上是首次分析时的状态。随后 qperf/harness 已补齐 `--shell-init-cmd`、`--shell-prefix` 和 `--qemu-arg`，并修复 timeout 退出时 plugin summary 可能丢失的问题。因此本报告后续增加一轮基于 workload 注入的复测，用于重新判断 virtio-blk 和 virtio-net 数据面热点。vsock 仍受宿主 `/dev/vhost-vsock` 缺失阻塞。

## Baseline 数据

baseline 三次结果目录：

- `target/qperf-virtio-experiment/baseline-run1/perf/riscv64/latest`
- `target/qperf-virtio-experiment/baseline-run2/perf/riscv64/latest`
- `target/qperf-virtio-experiment/baseline-run3/perf/riscv64/latest`

运行结果：

| run | result | samples | top1 | top2 | top3 |
| --- | --- | ---: | --- | --- | --- |
| baseline-1 | ok | 1976 | `0x8001359c` 76 / 3.8462% | `0x8000b04e` 72 / 3.6437% | `0x8000b05a` 63 / 3.1883% |
| baseline-2 | ok | 1979 | `0x8001359c` 94 / 4.7499% | `0x8000b04e` 73 / 3.6887% | `0x8000b05a` 58 / 2.9308% |
| baseline-3 | ok | 1979 | `0x8001359c` 91 / 4.5983% | `0x8000b04e` 72 / 3.6382% | `0x8000b05a` 58 / 2.9308% |

采样总数平均值：`1978.00`，标准差：`1.73`。

baseline 的 `profile.stderr` 显示：

```text
qperf: detected kernel .text virtual range: 0xffffffff80001000..0xffffffff802a4900
QPerf arguments: filter_alias_start: None, filter_alias_end: None, filter_alias_offset: None
qemu-system-riscv64: terminating on signal 15 ... (timeout)
qperf: QEMU ended with exit status: 124 after producing samples
qperf-analyzer: stopped after 1976 records (0 bad records): UnexpectedEof
```

`timeout=20s` 是命令参数，QEMU 被 timeout 结束但已经产出 samples，harness 报告 `result: ok`。`UnexpectedEof` 来自 timeout 截断最后一条 raw record，analyzer 报告 `0 bad records`，本次按可用 profile 处理。

## 瓶颈分析

### 1. 首要问题是 qperf 符号化不可信

baseline top samples 主要是 `0x800...` 裸地址。StarryOS 运行后内核 text 位于 high-half virtual range，例如 `0xffffffff80001000..`，而 QEMU/plugin 采到的是低地址别名，例如 `0x8001359c`。baseline qperf 只检测 `.text` 的虚拟范围，没有传入物理别名，导致 analyzer 无法把这些地址解析到函数名。

这会直接阻断后续 VirtIO 优化判断：即使 top 地址稳定，也无法确定是 virtio-blk、virtio-net、vsock、调度、显示还是 PCI probe。

### 2. qperf boot profile 没有形成明确 VirtIO data path 热点

默认 boot profile 的明显事件包括：

- virtio-blk rootfs 镜像作为磁盘设备挂载。
- virtio-net user net 启动并 DHCP 成功：`eth0: DHCP acquired address 10.0.2.15/24`。
- StarryOS 进入 shell。

但 baseline top 30 中没有可解释的 virtio-blk/net/vsock data path 符号。原因有两个：

- baseline 符号化缺少物理别名。
- 当前 qperf 没有 workload 注入，20s profile 大部分时间是启动、控制台、调度等待和空闲路径，而不是网络/块设备吞吐路径。

### 3. vsock 当前无法在本地 qperf riscv64 配置下实测

`qemu-riscv64` feature 包含 `ax-driver/virtio-socket`，但 QEMU args 没有 vsock device。要实测 vsock，需要至少补齐：

- host 侧 `/dev/vhost-vsock` 和 `vhost_vsock` 模块。
- QEMU args，例如 `-device vhost-vsock-pci,guest-cid=3`。
- guest 内 AF_VSOCK workload 或 shell init 命令。
- host/guest 对应 server/client。

这些步骤可能需要 root、内核模块和 VM 参数修改；当前环境没有直接执行。

## 对比口径

本次先做 measurement 修正补丁，再做 patched profile。由于修正了 qperf 的符号映射，`perf-diff` 中 baseline 的裸地址和 patched 的符号名会表现为一组地址消失、一组符号出现。这不是纯性能变化，不能直接解释为优化收益。

因此本报告采用两个口径：

- 原始 qperf 结果：逐次列出 samples 和 top functions。
- 人工归类对比：将 baseline 裸地址与 patched 符号化结果按别名归为同一类，观察大体波动。

patched 三次结果：

| run | result | samples | top1 | top2 | top3 |
| --- | --- | ---: | --- | --- | --- |
| patched-1 | ok | 1979 | `probe_generic_ecam+0x710` 90 / 4.5478% | `RawMutex::lock_after_prepare+0x45a` 70 / 3.5371% | `RawMutex::lock_after_prepare+0x466` 63 / 3.1834% |
| patched-2 | ok | 1979 | `probe_generic_ecam+0x710` 84 / 4.2446% | `RawMutex::lock_after_prepare+0x45a` 52 / 2.6276% | `RawMutex::lock_after_prepare+0x466` 44 / 2.2233% |
| patched-3 | ok | 1979 | `probe_generic_ecam+0x710` 103 / 5.2046% | `RawMutex::lock_after_prepare+0x466` 68 / 3.4361% | `RawMutex::lock_after_prepare+0x45a` 67 / 3.3855% |

patched 的 `profile.stderr` 显示物理别名已经生效：

```text
qperf: detected kernel text virtual range: 0xffffffff80000000..0xffffffff802a4dc2
qperf: detected kernel text physical alias: 0x80000000..0x802a4dc2
filter_alias_start=0x80000000
filter_alias_end=0x802a4dc2
filter_alias_offset=0xffffffff00000000
```

归类对比：

| group | baseline avg samples | baseline avg % | patched avg samples | patched avg % | 结论 |
| --- | ---: | ---: | ---: | ---: | --- |
| qperf total samples | 1978.00 +/- 1.73 | n/a | 1979.00 +/- 0.00 | n/a | 采样量稳定 |
| PCI/FDT probe alias | 87.00 +/- 9.64 | 4.3981 +/- 0.4840 | 92.33 +/- 9.71 | 4.6657 +/- 0.4907 | boot 阶段热点，非 VirtIO data path |
| display RawMutex wait alias | 132.00 +/- 2.65 | 6.6735 +/- 0.1396 | 127.00 +/- 12.17 | 6.4173 +/- 0.6147 | 波动内变化，不能证明优化 |
| `_head+0x4f8` alias | 36.33 +/- 3.51 | 1.8370 +/- 0.1791 | 40.00 +/- 3.00 | 2.0212 +/- 0.1516 | 启动低层路径，非优化目标 |
| preempt check | 95.00 +/- 8.54 | 4.8026 +/- 0.4286 | 109.33 +/- 8.96 | 5.5247 +/- 0.4529 | 调度/等待波动，非 VirtIO 结论 |

## workload 注入复测

复测日期：`2026-05-29T02:23:16+08:00`。

复测使用已修复的入口：

```bash
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --arch riscv64 \
  --timeout 30 \
  --format folded \
  --freq 99 \
  --max-depth 64 \
  --mode tb \
  --top 40 \
  --min-percent 0.5 \
  --shell-init-cmd '<guest workload>' \
  --output-dir target/qperf-virtio-rerun/<case>
```

复测结果目录：

- `target/qperf-virtio-rerun/boot-run{1,2,3}/perf/riscv64/latest`
- `target/qperf-virtio-rerun/blk-read-run{1,2,3}/perf/riscv64/latest`
- `target/qperf-virtio-rerun/net-wget-run{1,2,3}/perf/riscv64/latest`
- `target/qperf-virtio-rerun/vsock-probe/perf/riscv64/latest`

所有成功样本的 qperf plugin summary 均为 `dropped_samples = 0`、`sample_failures = 0`。`boot` 三次 samples 均为 `2969`；`blk-read` 为 `2968/2969/2969`；`net-wget` 为 `2968/2969/2968`。

### blk-read workload

guest 命令：

```sh
echo BLK_READ_BEGIN
time dd if=/usr/bin/lto-dump of=/dev/null bs=64k
sync
echo BLK_READ_END
sleep 1
```

原始吞吐输出：

| run | bytes | dd seconds | dd throughput | real |
| --- | ---: | ---: | ---: | ---: |
| blk-read-1 | 53,601,104 | 4.299533s | 11.9MB/s | 4.35s |
| blk-read-2 | 53,601,104 | 4.369256s | 11.7MB/s | 4.41s |
| blk-read-3 | 53,601,104 | 4.160124s | 12.3MB/s | 4.21s |

`real` 平均值 `4.323s`，标准差 `0.103s`。

qperf 归类统计（基于完整 folded stack 中的函数名子串匹配）：

| category | boot avg | blk-read avg | blk-read range | 说明 |
| --- | ---: | ---: | ---: | --- |
| `memcpy` | 0.348% | 4.199% | 3.806%-4.412% | 文件读出和 block buffer copy 成本明显上升 |
| `VirtQueue::add_notify_wait_pop` | 未显著出现 | 3.795% | 3.402%-4.178% | virtio-blk 同步提交、notify、等待完成路径成为稳定热点 |
| `display wait lock` | 11.294% | 8.837% | 8.690%-8.993% | 仍有控制台/显示等待噪声，但占比低于 boot |

定位到的代码路径：

- `drivers/ax-driver/src/virtio/block.rs` 中 `submit_request()` 直接调用 `VirtIOBlk::read_blocks()` / `write_blocks()`，当前完成语义是同步的。
- `drivers/ax-driver/src/block/binding.rs` 中 `read_block()` 持有 queue lock 后调用 `read_blocks_blocking()`，随后把每个 `BlockData` copy 到用户提供 buffer。

结论：当前 blk-read 的一阶瓶颈不是块设备节点吞吐工具本身，而是同步 virtqueue request/complete 路径与读后 copy。后续值得优先验证的方向是批量/异步提交、减少 per-block copy，以及更大的 DMA buffer 或顺序读合并。

### net-wget workload

宿主容器内启动 HTTP server：

```bash
python3 -m http.server 8000 --bind 0.0.0.0
```

guest 命令：

```sh
echo NET_WGET_BEGIN
time wget -O /dev/null http://10.0.2.2:8000/tmp/axbuild/rootfs/rootfs-riscv64-alpine.img.tar.xz
echo NET_WGET_END
sleep 1
```

原始结果：

| run | transfer | real |
| --- | ---: | ---: |
| net-wget-1 | 60.6M | 5.65s |
| net-wget-2 | 60.6M | 5.57s |
| net-wget-3 | 60.6M | 5.73s |

`real` 平均值 `5.650s`，标准差 `0.080s`。

qperf 归类统计：

| category | boot avg | net-wget avg | net-wget range | 说明 |
| --- | ---: | ---: | ---: | --- |
| `memcpy` | 0.348% | 5.345% | 5.086%-5.526% | RX 数据搬运成为最明显数据面成本 |
| `memmove` | 未显著出现 | 1.381% | 1.145%-1.550% | packet/header 移动成本可见 |
| `NetRxQueue::submit` | 未显著出现 | 2.010% | 1.819%-2.190% | RX buffer 重新投递路径被 workload 稳定打中 |
| `RxInflight` | 4.861% | 3.919% | 3.605%-4.109% | boot 和 net 都有 BTree/RxInflight 管理成本，需进一步拆分启动噪声 |
| `VirtQueue::add_notify_wait_pop` | 未显著出现 | 0.247% | 0.168%-0.337% | RX 主要不是同步 notify/wait 热点 |

定位到的代码路径：

- `drivers/ax-driver/src/virtio/net.rs` 使用 `BTreeMap<u16, RxInflight>` 管理 RX inflight buffer。
- `NetRxQueue::submit()` 进入 `with_task()` 后调用 `submit_rx()`，最终 `receive_begin()` 投递 DMA buffer。
- `reclaim_rx()` 在收到包后执行 `buffer.copy_within(header_len..header_len + packet_len, 0)`，这解释了 net-wget 中 `memmove`/copy 类热点。

结论：virtio-net RX 的主要可优化点是 RX packet 从 virtio header 后移到 packet 起点的 copy/move，以及 inflight buffer 管理结构和投递路径。TX 路径本次 workload 不敏感，之前的 TX staging 微优化不能用 net-wget 证明收益。

### vsock 探测

vsock 设备探测命令使用：

```bash
--qemu-arg=-device \
--qemu-arg=vhost-vsock-pci,guest-cid=3
```

结果：`target/qperf-virtio-rerun/vsock-probe/perf/riscv64/latest/profile.stderr` 中 QEMU 直接失败：

```text
qemu-system-riscv64: -device vhost-vsock-pci,guest-cid=3: Could not open '/dev/vhost-vsock': No such file or directory
```

因此当前本地环境无法进入 virtio-vsock/vhost-vsock 数据面。宿主存在 `/dev/vsock`，但没有 `/dev/vhost-vsock`，Docker 容器内也未提供 vhost-vsock 设备。vsock 只能记录为“设备环境阻塞”，不能给出性能结论。

## 结论与下一步

本次能够闭环验证的关键结论是：qperf 默认 boot profile 可运行、可重复；原 baseline 存在物理地址别名导致的符号化问题，修正后能把 `0x800...` low alias 映射回 high-half kernel text，热点解释性显著提升。进一步修复 workload 注入后，blk-read 和 net-wget 已经可以打到对应 VirtIO 数据面。

对 VirtIO 路径的性能结论需要谨慎：

- virtio-blk：`dd` 冷读 51.1MB 平均 `4.323s`，`memcpy` 平均 `4.199%`，`VirtQueue::add_notify_wait_pop` 平均 `3.795%`；同步 virtqueue 提交/完成和读后 copy 是主要瓶颈。
- virtio-net/vnet：`wget` RX 60.6M 平均 `5.650s`，`memcpy` 平均 `5.345%`，`NetRxQueue::submit` 平均 `2.010%`，`memmove` 平均 `1.381%`；RX copy/move 和 buffer 重新投递路径值得继续优化。
- virtio-vsock：代码编译但当前 Docker/宿主缺少 `/dev/vhost-vsock`，QEMU 无法创建设备，本地 qperf 未覆盖运行时路径。

建议下一步基于已经可运行的 workload 做更小粒度的 A/B：

- blk：验证更大 DMA buffer、顺序读合并、异步/批量提交是否能降低 `add_notify_wait_pop` 和 copy 占比。
- net：验证 RX header offset 处理是否能避免 `copy_within()`，以及用数组/slab 替换 `BTreeMap` inflight 管理是否降低 RX submit/reclaim 成本。
- vsock：先在宿主启用并透传 `/dev/vhost-vsock`，再建立 host/guest send/recv workload。
- 在 workload 中补充 queue depth、kick/notify 次数、alloc/free、copy 字节数、IRQ/poll 次数等结构化计数，再结合 qperf flamegraph 做小补丁 A/B。
