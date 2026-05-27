# 基于 qperf 的 StarryOS VirtIO 性能优化报告

## 1. 引言

本报告研究如何利用 qperf（QEMU TCG 插件式性能分析器）对 StarryOS 的 VirtIO 子系统（vsock、net、blk）进行非侵入式热点分析，并参考 x-kernel 项目的 VSOCK TX 性能优化经验，探讨 StarryOS 中的具体优化机会。

### 1.1 x-kernel 优化回顾

openKylin x-kernel 项目对 VSOCK TX 路径进行了系统性优化，吞吐量从 5 MB/s 提升至 42 MB/s（+740%），核心发现：

- **VSOCK_DEV Mutex 序列化**：占 send 总时间 55%，锁等待平均 310μs
- **逐包同步等待**：每次 credit update 同步等待 79μs
- **数据拷贝开销**：未利用 scatter-gather DMA 的零拷贝能力

x-kernel 的分析手段是在代码中手动插入时间探针（`Instant::now()` 差值），精确但侵入性高，且需要修改源码、重新编译。

### 1.2 qperf 的方法论优势

qperf 作为 QEMU TCG 插件运行，具有以下独特优势：

| 维度 | x-kernel 手动探针 | qperf 采样分析 |
|------|-------------------|----------------|
| 侵入性 | 需修改源码，插入计时代码 | 无需修改任何内核代码 |
| 编译依赖 | 每次探针变更需重新编译 | 仅需编译插件，内核不变 |
| 采样粒度 | 精确到单次操作 | 统计采样（频率可调） |
| 覆盖范围 | 仅覆盖插桩点 | 覆盖整个内核 .text 段 |
| 开锁等待分析 | 需手动记录锁持有时间 | 可通过热点占比间接推断 |
| 适用场景 | 已知瓶颈位置，量化延迟 | 发现未知瓶颈，全局热点分析 |

两者互补：qperf 用于快速发现热点函数，手动探针用于精确定量特定路径。

## 2. qperf 工作原理

### 2.1 架构

```
┌─────────────────────────────────────────────────┐
│                   QEMU (TCG)                     │
│  ┌───────────┐    ┌──────────────────────────┐   │
│  │ Guest OS  │    │    qperf Plugin (libqperf.so)   │
│  │ (StarryOS)│───>│  TB 级回调 → 采样 IP+栈帧 │   │
│  │  .text    │    │  → bounded channel → writer│   │
│  └───────────┘    └──────────────┬───────────┘   │
└──────────────────────────────────┼───────────────┘
                                   │ qperf.bin (原始采样)
                                   ▼
                        ┌──────────────────────┐
                        │   qperf-analyzer     │
                        │  符号解析(DWARF+symtab)│
                        │  折叠栈 + 热点聚合    │
                        │  火焰图生成          │
                        └──────────────────────┘
                                   │
                    ┌──────────────┼──────────────┐
                    ▼              ▼              ▼
              stack.folded   flamegraph.svg   summary.txt
              (热点排名)     (可视化)         (元数据)
```

### 2.2 关键技术

**地址范围过滤**：自动检测内核 ELF 的 `.text` 段地址范围，在 TCG 翻译阶段过滤掉 OpenSBI 固件指令。实测中，无过滤时 99.7% 采样落入 OpenSBI，仅 0.28% 落入内核；启用过滤后 100% 采样集中在内核空间。

**TB 级采样**：对每个 Translation Block 注册一次回调（而非每条指令），大幅降低 TCG 翻译和回调执行开销。支持 `tb`（默认）和 `insn` 两种模式。

**symtab 回退**：当 release 内核无 DWARF 调试信息时，自动回退到 ELF `.symtab` 符号表查找，输出 `symbol_name+0xoffset` 格式。

## 3. VirtIO 子系统锁竞争分析

通过代码审查，StarryOS 的 VirtIO 子系统存在以下锁竞争热点：

### 3.1 VSOCK — 最高竞争风险

VSOCK 路径有三层锁嵌套，是所有 VirtIO 设备中锁竞争最严重的子系统。

#### 3.1.1 全局设备锁

```rust
// os/arceos/modules/axnet-ng/src/device/vsock.rs:15
static VSOCK_DEVICE: Mutex<Option<AxVsockDevice>> = Mutex::new(None);
static PENDING_EVENTS: Mutex<VecDeque<VsockDriverEvent>> = Mutex::new(VecDeque::new());
```

`VSOCK_DEVICE` 是一个全局 Mutex，所有 send/recv/poll/connect 操作串行化。send 路径在重试循环中反复获取该锁（最多 10 次）。

#### 3.1.2 全局连接管理器锁

```rust
// os/arceos/modules/axnet-ng/src/vsock/connection_manager.rs:536
pub static VSOCK_CONN_MANAGER: Mutex<VsockConnectionManager> = ...;
```

连接管理器持有 `BTreeMap<VsockConnId, Arc<Mutex<Connection>>>`。每次数据接收、连接建立/关闭都需要获取此锁。在事件处理中，该锁与 `VSOCK_DEVICE` 构成嵌套持有。

#### 3.1.3 锁嵌套链

```
vsock_send:       VSOCK_DEVICE → VSOCK_CONN_MANAGER → per-connection Mutex
vsock_poll_loop:  VSOCK_DEVICE → VSOCK_CONN_MANAGER → per-connection Mutex
accept:           VSOCK_CONN_MANAGER → listen-queue Mutex → per-connection Mutex
connect:          state → VSOCK_CONN_MANAGER → self.connection → per-connection Mutex
```

**性能影响**：在多连接场景下，全局锁导致所有连接的操作串行执行。以 x-kernel 的经验类比，全局 Mutex 序列化可占 send 总时间 55% 以上。

### 3.2 Net — TX/RX 共享锁

```rust
// platform/axplat-dyn/src/drivers/net/mod.rs:51-61
struct NetState {
    tx_queue: rd_net::TxQueue,
    rx_queue: rd_net::RxQueue,
    pending_rx: VecDeque<NetBufBox>,
}
pub struct Net {
    state: Mutex<NetState>,  // TX 和 RX 共享一把锁
}
```

全双工网络流量中，TX 发送和 RX 接收互相阻塞。发送路径在 `state.lock()` 内执行数据拷贝（`copy_from_slice`），接收路径在同一个锁内处理 `pending_rx` 队列。

### 3.3 Block — 同步阻塞持锁

```rust
// platform/axplat-dyn/src/drivers/blk/mod.rs:16-19
pub struct Block {
    queue: Mutex<CmdQueue>,
}
```

`read_blocks_blocking` 和 `write_blocks_blocking` 在持有锁期间执行同步阻塞 I/O，锁持有时间等于整个块操作耗时。metadata 查询（`num_blocks`、`block_size`）也需要获取同一把锁。

### 3.4 竞争风险总结

| 锁 | 位置 | 作用域 | 竞争风险 | qperf 可检测 |
|---|------|-------|---------|------------|
| `VSOCK_DEVICE` | `device/vsock.rs:15` | 全局，序列化所有 vsock 操作 | **极高** | 热点集中在 send/recv 函数 |
| `VSOCK_CONN_MANAGER` | `connection_manager.rs:536` | 全局，序列化连接表 + 事件处理 | **极高** | 热点集中在事件处理函数 |
| `PENDING_EVENTS` | `device/vsock.rs:16` | 全局，缓冲 RX 事件 | 中等 | 在 poll 路径中出现 |
| per-connection `Mutex` | `connection_manager.rs:271` | 每连接，序列化状态 + 环形缓冲区 | 中等 | 多连接时热点分散 |
| `Net.state` | `net/mod.rs:61` | 每设备，TX+RX 共享 | **高** | 全双工时热点集中 |
| `Block.queue` | `blk/mod.rs:18` | 每设备，同步阻塞 | 中等 | 块 I/O 密集时 |
| PCI `Endpoint` | `net/virtio_pci.rs:84` | 每端点，配置空间访问 | 低 | 仅 probe/init 阶段 |

## 4. 使用 qperf 进行 VirtIO 热点分析

### 4.1 基本用法

```bash
# 构建 qperf 工具 + 内核，运行 QEMU 采样，分析输出
cargo starry perf --arch riscv64 --timeout 30 --format all

# 仅生成折叠栈（无需 inferno-flamegraph）
cargo starry perf --arch riscv64 --timeout 30 --format folded --top 20

# 指定输出目录
cargo starry perf --arch riscv64 --timeout 30 --out target/qperf/virtio-baseline

# 调高采样频率（精度更高，开销更大）
cargo starry perf --arch riscv64 --freq 499 --timeout 30
```

### 4.2 针对特定 VirtIO 设备的工作负载设计

qperf 采样的是整个内核 .text 段，热点分析的效果取决于运行期间活跃的代码路径。为定位特定 VirtIO 设备的瓶颈，需要设计针对性的工作负载。

#### 4.2.1 VSOCK 性能分析

在 StarryOS shell 中运行 VSOCK 吞吐测试：

```bash
# 步骤 1: 启动 qperf 采样（一个终端）
cargo starry perf --arch riscv64 --timeout 60 --freq 199 --format all \
  --out target/qperf/vsock-throughput

# 步骤 2: 在 StarryOS shell 中运行 vsock 负载（另一个终端或 shell_init_cmd）
# 例如：发送大量数据通过 vsock 连接
dd if=/dev/zero bs=1M count=100 | vsock-send <cid> <port>
```

预期热点集中在：
- `vsock_send` → `VSOCK_DEVICE.lock()` 等待
- `vsock_poll_loop` → `VSOCK_CONN_MANAGER.lock()` + `handle_vsock_event`
- `Connection::push_data` / `Connection::read_data` → 环形缓冲区操作

如果火焰图中 `ax_sync::Mutex::lock` 或相关的等待函数占比超过 20%，则确认 Mutex 序列化是主要瓶颈。

#### 4.2.2 Net 性能分析

```bash
# 运行网络吞吐测试
cargo starry perf --arch riscv64 --timeout 60 --freq 199 --format all \
  --out target/qperf/net-throughput

# 在 StarryOS 中触发网络流量
# iperf3、wget、或自定义 TCP 吞吐测试
```

预期热点：
- `Net::transmit` → `state.lock()` 内的 `copy_from_slice` + `tx_queue.prepare_send`
- `Net::receive` → `state.lock()` 内的 `prefetch_rx_packets` + `pending_rx`

如果 `transmit` 和 `receive` 在火焰图中占比较高且互相排斥，说明 TX/RX 共享锁是瓶颈。

#### 4.2.3 Block 性能分析

```bash
# 运行块设备 I/O 测试
cargo starry perf --arch riscv64 --timeout 60 --freq 199 --format all \
  --out target/qperf/blk-throughput

# 在 StarryOS 中触发块 I/O
dd if=/dev/vda of=/dev/null bs=4k count=10000
```

预期热点：
- `Block::read_block` → `queue.lock()` → `read_blocks_blocking`
- VirtIO 完成队列轮询 `CmdQueue::poll_completion`

### 4.3 对比分析（diff 模式）

对优化前后分别运行 qperf，然后使用 analyzer 的 diff 功能：

```bash
# 优化前基线
cargo starry perf --arch riscv64 --timeout 30 --format folded \
  --out target/qperf/baseline

# 应用优化后
cargo starry perf --arch riscv64 --timeout 30 --format folded \
  --out target/qperf/optimized

# 对比热点变化
qperf-analyzer diff \
  --baseline target/qperf/baseline/stack.folded \
  --compare target/qperf/optimized/stack.folded
```

### 4.4 采样模式选择

```bash
# TB 级采样（默认，低开销，适合整体热点发现）
cargo starry perf --arch riscv64 --mode tb --freq 199

# 指令级采样（高开销，高精度，适合精细分析特定函数）
cargo starry perf --arch riscv64 --mode insn --freq 49
```

TB 模式每个 Translation Block 一次回调，开销约为 insn 模式的 1/10 ~ 1/50。建议先用 TB 模式发现热点区域，再用 insn 模式对关键函数做精细分析。

## 5. 基于代码审查的优化建议

结合 x-kernel 的优化经验和 qperf 热点分析能力，提出以下优化方向：

### 5.1 VSOCK — 拆分全局锁（优先级：P0）

**问题**：`VSOCK_DEVICE` 和 `VSOCK_CONN_MANAGER` 是全局 Mutex，所有连接的所有操作串行执行。

**x-kernel 经验**：将 VSOCK_DEV 的单一 Mutex 拆分为 TX Mutex 和 RX Mutex，吞吐量提升 3.2x。

**建议优化路径**：

1. 将 `VSOCK_CONN_MANAGER` 中的 `BTreeMap` 改为 `DashMap` 或分片锁（sharded lock），减少连接查找时的锁竞争
2. 将 `VSOCK_DEVICE` 的 send 和 recv 操作分离为独立的锁：`VSOCK_TX_LOCK` 和 `VSOCK_RX_LOCK`
3. 将 `PENDING_EVENTS` 改为无锁队列（如 `crossbeam-queue`），避免 poll 路径中的 Mutex 获取

**qperf 验证方法**：
- 基线采样：运行 vsock 吞吐测试，记录 `stack.folded` 中 `Mutex::lock` 相关函数的采样占比
- 优化后采样：同样条件运行，对比占比变化
- 预期：`Mutex::lock` 占比从 >20% 降至 <5%

### 5.2 VSOCK — 异步 Credit 更新（优先级：P1）

**问题**：send 路径在 peer buffer 满时（`DevError::Again`），同步等待 `wait_for_tx()`，重试循环最多 10 次，每次都重新获取 `VSOCK_DEVICE` 锁。

**x-kernel 经验**：将 credit update 改为异步模式，send 路径不阻塞等待，吞吐量提升 2.5x。

**建议**：
- 将 `DevError::Again` 的处理从同步重试改为异步通知：注册一个回调，在 peer 通知有空间时唤醒等待的 send
- 减少 send 路径中的锁获取次数（从 3 次降为 1 次）

### 5.3 Net — TX/RX 锁分离（优先级：P1）

**问题**：`NetState` 的 `tx_queue` 和 `rx_queue` 共享一把 `Mutex<NetState>`，全双工流量互相阻塞。

**建议**：
```rust
// 当前（单锁）
struct NetState {
    tx_queue: TxQueue,
    rx_queue: RxQueue,
    pending_rx: VecDeque<NetBufBox>,
}

// 优化（TX/RX 分离）
struct Net {
    tx_state: Mutex<TxState>,  // TX 独立锁
    rx_state: Mutex<RxState>,  // RX 独立锁
}
```

**qperf 验证**：对比 `transmit` 和 `receive` 函数在火焰图中的栈深度和占比。优化后，两者应不再出现嵌套等待。

### 5.4 Block — 减少锁持有时间（优先级：P2）

**问题**：`queue.lock()` 在 `read_blocks_blocking` 和 `write_blocks_blocking` 期间持续持有，包括等待硬件完成的阻塞时间。

**建议**：
- 将请求提交和完成等待分离：提交请求后释放锁，轮询完成队列时单独获取锁
- metadata 查询（`num_blocks`、`block_size`）应使用 `OnceCell` 缓存，避免重复获取锁

### 5.5 Scatter-Gather DMA（优先级：P2）

**问题**：Net 的 transmit 路径在锁内执行 `copy_from_slice`，拷贝数据到 DMA 缓冲区。

**x-kernel 经验**：使用 scatter-gather DMA 零拷贝，避免数据拷贝，延迟降低 40%。

**建议**：
- 利用 VirtIO 的 descriptor chain 能力，直接传递应用层缓冲区的物理地址
- 需要确保缓冲区的 DMA 映射（`share`/`unshare`）在锁外完成

## 6. 实践验证计划

### 6.1 Phase 1：基线测量

```bash
# 1. 构建 debug 内核
cargo starry build --arch riscv64 --debug

# 2. 运行 qperf 基线采样（vsock 负载）
cargo starry perf --arch riscv64 --timeout 30 --freq 199 --format all \
  --out target/qperf/vsock-baseline --top 30

# 3. 检查热点分布
cat target/qperf/vsock-baseline/summary.txt
head -30 target/qperf/vsock-baseline/stack.folded | sort -t' ' -k2 -rn
```

### 6.2 Phase 2：优化迭代

每次优化后重复 Phase 1 的测量流程，使用不同的 `--out` 目录保存结果。使用 `qperf-analyzer diff` 对比前后变化。

### 6.3 Phase 3：结果量化

从 `summary.txt` 中提取以下指标：
- `folded_stack_lines`：唯一调用栈数量（越少说明热点越集中）
- `plugin_summary.dropped_samples`：采样丢失率（过高需降低 freq）
- 火焰图中 `Mutex::lock` / `wait` 相关函数的采样占比

## 7. qperf 使用指南速查

```bash
# 完整命令
cargo starry perf [OPTIONS]

# 参数说明
--arch <ARCH>         # 目标架构: riscv64, loongarch64
--freq <N>            # 采样频率 (Hz), 默认 999
--max-depth <N>       # 最大栈展开深度, 默认 128
--mode <tb|insn>      # 采样模式: tb=TB级(低开销), insn=指令级(高精度)
--timeout <SECONDS>   # QEMU 运行时长, 0=不限制
--format <folded|svg|all>  # 输出格式
--top <N>             # 热点函数排名, 默认 20
--out <DIR>           # 输出目录

# 输出文件
target/qperf/<arch>/<timestamp>/
├── qemu.toml           # QEMU 运行配置（含 plugin 参数）
├── qperf.bin           # 原始采样数据
├── stack.folded        # 折叠栈（可用于 flamegraph.pl / inferno）
├── flamegraph.svg      # 火焰图（需安装 inferno-flamegraph）
└── summary.txt         # 运行摘要（采样数、配置、文件路径）

# 独立使用 analyzer
qperf-analyzer resolve -e <elf-path> <input.bin> <output.folded> [--top N] [--flamegraph <svg>]
qperf-analyzer diff --baseline <folded1> --compare <folded2>
```

## 8. 结论

qperf 作为非侵入式 TCG 插件性能分析器，为 StarryOS VirtIO 性能优化提供了以下关键能力：

1. **热点发现**：无需修改内核代码，即可发现 VirtIO 路径中的高占用函数。地址范围过滤确保采样集中在内核空间，排除 OpenSBI 干扰。

2. **锁竞争定位**：通过火焰图中 `Mutex::lock` 及相关等待函数的采样占比，可以定量评估锁竞争的严重程度。VSOCK 子系统的全局锁结构预计是最大的性能瓶颈。

3. **优化验证**：每次优化后重新运行 qperf，通过 `diff` 模式或对比 `summary.txt` 量化优化效果。

4. **与手动探针互补**：qperf 用于快速发现瓶颈（回答"哪里慢"），手动计时代码用于精确定量（回答"慢多少"）。两者结合构成完整的性能优化工作流。

参考 x-kernel 的经验（VSOCK TX 从 5 MB/s 到 42 MB/s），StarryOS 的 VSOCK 全局锁拆分和异步 credit 更新预计可带来数倍的性能提升。建议按 P0 → P1 → P2 优先级逐步实施，每次优化后使用 qperf 验证效果。
