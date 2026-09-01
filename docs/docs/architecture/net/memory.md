---
sidebar_position: 6
sidebar_label: "内存与队列"
---

# 内存与队列

网络数据面使用有界、预分配、move-only 的所有权流水线。当前不承诺端到端
zero-copy；目标是让 DMA buffer 在 queue CPU 与唯一 protocol CPU 之间的每次转移都
可证明，并让 ring 满、submit retry、stop/rollback 不会复制或泄漏 token。

## 1. 内存域

| 内存域 | 对象 | owner |
| --- | --- | --- |
| hardware/DMA | `DmaBuffer`、descriptor/virtqueue | fixed-CPU `QueueGroupExecutor` |
| queue/protocol boundary | 四条 SPSC ring + `pending_*` slot | 每条唯一 producer/consumer |
| Ethernet frame | `ProtocolEthernetFrame` | 唯一 protocol executor |
| Router packet buffer | `Router.rx_buffer` / `tx_buffer` | 唯一 protocol executor |
| smoltcp socket buffer | TCP `SocketBuffer`、UDP/raw `PacketBuffer` | `SocketSet` / protocol executor |
| user buffer | syscall `IoBuf`/`IoBufMut` | 调用者 |

hard IRQ 不拥有 DMA payload。它只拥有 endpoint-local status snapshot，并发布对应 group。

## 2. Move-only DMA token

`DmaBuffer` 不实现 `Clone`/`Copy`。buffer 包含 CPU mapping、bus address、capacity、
effective length 与 DMA sync 能力。合法 RX 状态：

```text
pool
  -> IRxQueue::submit / initial_refill
  -> device DMA ownership
  -> IRxQueue::reclaim -> RxCompletion { buffer, packet_len }
  -> RX-ready SPSC
  -> protocol read/copy
  -> RX-recycle SPSC
  -> IRxQueue::recycle
```

合法 TX 状态：

```text
pool
  -> TX-free SPSC
  -> protocol fills frame
  -> TX-ready SPSC
  -> ITxQueue::submit
  -> device DMA ownership
  -> ITxQueue::reclaim
  -> TX-free SPSC
```

`SubmitError` 必须携带原 buffer。`Retry` 时 runtime 保存在 `pending_tx` 或
`pending_rx_recycle`；terminal error 也先恢复可回收 ownership，再决定 group failure。

## 3. SPSC memory ordering

每条 ring 预分配 `capacity + 1` 个 `UnsafeCell<MaybeUninit<T>>` slot，以保留一个 slot
区分 full/empty。

- producer 独占 tail slot，写 payload 后 Release publish `tail`；
- consumer Acquire observe `tail`，read payload 后 Release publish `head`；
- producer Acquire observe `head` 后才能复用 slot；
- endpoint 非 `Sync`，不能产生第二 producer/consumer；
- `T: Send` 是 token 跨 CPU 的最低条件。

ring Drop 只在两个 endpoint 都不再并发访问时遍历剩余 live slot。unsafe safety 依赖
唯一 endpoint 与 Acquire/Release 配对，不依赖外部大锁。

## 4. Ring capacity

RX-ready/recycle capacity 来自 driver RX queue capacity，TX-ready/free capacity 来自 TX
queue capacity。runtime 不再用 protocol crate 的固定“设备 RX/TX queue size”覆盖硬件
ring 深度。

容量与 token 数量相等，因此初始化可以把全部 TX pool token 放入 TX-free ring；RX
initial refill 使用 driver capacity。`spsc_ring()` 额外保留内部 sentinel slot，不减少
对调用者承诺的有效容量。

## 5. Backpressure

ring full 不会 busy-wait 或 drop token：

| 边界 | owner 保留 |
| --- | --- |
| RX-ready full | `pending_rx: RxCompletion` |
| RX recycle submit retry | `pending_rx_recycle: DmaBuffer` |
| TX-free full | `pending_tx_free: DmaBuffer` |
| TX submit retry | `pending_tx: DmaBuffer` |
| protocol recycle full | protocol `pending_recycle` vector |
| protocol TX-ready full | token 回到 protocol `tx_spares` |

queue group 在 blocked 时保持 IRQ mask。protocol owner 消费或释放 ring 空间后精准
schedule 该 group；没有周期 retry timer。

## 6. DMA synchronization

CPU 读取 RX payload 前使用 `read_with_cpu()`，CPU 写 TX payload时使用
`write_with_cpu()`。driver 在 submit/reclaim 边界完成 device-direction sync 和 doorbell/
completion ordering。非一致 DMA 平台必须在这些 API 中实现 cache maintenance，不能
假设 Rust atomic fence 会刷新设备 cache。

ownership publish 顺序：

```text
RX completion observed
  -> device-to-CPU sync
  -> Release SPSC publish
  -> protocol Acquire + read

protocol write
  -> Release SPSC publish
  -> queue Acquire
  -> CPU-to-device sync
  -> descriptor/doorbell publish
```

## 7. Protocol copies

RX 在 protocol owner 上从 DMA buffer 复制为 `ProtocolEthernetFrame`，随后立即归还
token。Ethernet 解封装后 IP payload进入 `Router.rx_buffer`，再由 smoltcp token 复制/
引用到 socket buffer。TX 方向从 socket buffer 经 smoltcp/Router 生成 IP packet，
Ethernet framing 后写入 DMA token。

这些复制是当前单协议 core 与 portable DMA boundary 的明确成本。loopback 直接注入
Router RX buffer，不分配物理 DMA token。

### 7.1 Device TX queue discipline

每个物理设备的 `QueueFramePort` 都持有一个显式 `TxQueueDiscipline`。`NoQueue` 直接
提交，busy 时返回 `Again`，其 `pending_tx` 始终为空且 `VecDeque` 不分配 backing。
`Fifo { max_frames }` 在 busy 后复制完整 `ProtocolEthernetFrame` 并按序重试；构造时
同样使用 `VecDeque::new()`，第一次真实入队前 capacity 为零。

64 位目标上的一个 `ProtocolEthernetFrame` 是 2048 字节 payload 加 8 字节长度，
因此 64 个 frame 的内容为 `64 * 2056 = 131584` 字节，即 128.5 KiB，不含 allocator
metadata。当前 `axruntime` 为每个生产网卡显式选择 64 帧 FIFO；这部分内存只在该设备
实际形成 backlog 后增长，不再由所有网卡在启动时预留。不同设备的 limit 和 backlog
彼此独立；hardware ring、SPSC token pool 与 AIC queue size 仍按各自所有者另外计算。

## 8. Protocol/socket budgets

协议常量仍集中在 `consts.rs`：

```text
TCP RX/TX: 64 KiB each per socket
UDP RX/TX: 64 KiB each per socket plus packet metadata
RAW RX/TX: 64 KiB each per socket plus packet metadata
LISTEN_QUEUE_SIZE: 512
SOCKET_BUFFER_SIZE: 64 packet slots
ETHERNET_MAX_PENDING_PACKETS: 128
```

这些是 protocol/ARP 预算，不是 hardware queue capacity。提高 socket 预算会按 socket
数量放大；提高 driver queue capacity 会按 group 增加 DMA pool 与 SPSC slot。

## 9. Queue budget

一个 group poll 对 RX recycle、RX reclaim、TX completion、TX submit 各最多处理 64
项；一个 CPU round 最多 256 项。预算用尽不会 rearm IRQ，而是保留 group ownership、
yield/repoll。这限制单个 burst 对同 CPU 其它 group 的占用，同时合并 IRQ。

## 10. Stop 与失败回收

runtime teardown 先拒绝控制请求，disable/synchronize IRQ，再 stop/join executor。只有
确认 callback 与 queue task 不再运行后才 drop ring、queue 和 DMA pool。

driver shutdown 必须返回或隔离仍可被设备 DMA 的 token。无法确认 DMA 已停止时，
宁可 quarantine buffer，也不能把 mapping 归还 allocator 后让设备继续访问。

## 11. 验证

确定性测试至少断言：

- ring bounded/order 与 move-only token exactly once；
- submit error 返回相同 bus-address token；
- RX/TX backpressure 保留 pending token；
- stop 后拒绝新 schedule/request；
- budget exhaustion 保持 IRQ mask；
- non-coherent sync 顺序由目标 driver/board 测试确认。

真正 zero-copy、GRO、page-pool、scatter-gather protocol token 和 user zero-copy 不在本次
架构范围内。
