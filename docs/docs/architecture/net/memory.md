---
sidebar_position: 6
sidebar_label: "内存与队列"
---

# 内存与队列

`ax-net` 的数据面内存模型以有界队列和明确拷贝边界为核心。当前实现不承诺端到端 zero-copy；它优先保证嵌入式/unikernel 场景下的内存上限、锁边界和协议栈所有权清晰。RX 方向从驱动 buffer 进入 Router 队列，再进入 smoltcp socket buffer，最终复制到用户 buffer；TX 方向从用户 buffer 写入 smoltcp socket buffer，再进入 Router TX buffer、设备 TX queue，最后交给驱动发送。

核心源码：

| 源码 | 职责 |
| --- | --- |
| `consts.rs` | socket buffer、Router packet buffer、设备 RX/TX queue 容量 |
| `router.rs` | `BoundedPacketQueue`、`QueuedPacket`、`Router.rx_buffer` / `tx_buffer`、RX/TX worker |
| `service.rs` | `Service::poll()` 中的 RX drain、smoltcp poll、TX dispatch 顺序 |
| `device/driver.rs` | `rd-net` buffer 适配、`VecRxBuffer` / `VecTxBuffer`、RX prefetch |
| `device/ethernet.rs` | Ethernet 解封装、ARP pending packet、driver TX buffer 写入 |
| `tcp.rs`、`udp.rs`、`raw.rs` | 用户 buffer 与 smoltcp socket buffer 之间的收发拷贝 |

源码表把常量、队列实现、协议 buffer 和 driver adapter 分别定位到其所有者。理解这些入口后，下面的总体模型可以用所有权域解释内存，而不是按文件或函数调用顺序罗列复制点。

## 1. 总体模型

数据面把 packet 所有权划分到用户缓冲区、smoltcp socket buffer、串行 Router 核心、设备 Worker 队列和驱动 ring。每次跨域都由明确的复制或拥有型队列消息完成，下面的架构图用于识别容量配置、背压和统计分别发生在哪一层。

| 内存域 | 典型对象 | 所有者 | 生命周期 |
| --- | --- | --- | --- |
| Driver buffer | `NetRxBuffer` / `NetTxBuffer`、`rd_net::RxQueue` / `TxQueue` | 具体网卡驱动或 `RdNetDriver` | 单个收发操作或驱动队列周期 |
| Router queue / packet buffer | `QueuedPacket`、`RxPacket`、`TxPacket`、`Router.rx_buffer`、`Router.tx_buffer` | `Router` / device worker | packet 在设备 worker 与 net-poll worker 之间流转期间 |
| smoltcp socket buffer | TCP `SocketBuffer`、UDP/raw `PacketBuffer` | 全局 `SocketSet` 中的具体 socket | socket 生命周期内固定分配 |
| 用户 buffer | syscall 传入的 `Read` / `Write` / `IoBufMut` | 调用者线程 | 单次 `send()` / `recv()` 调用 |

总体关系如下：

![ax-net packet 内存与所有权边界](images/memory-ownership-architecture.svg)

图中的 IP 数据面 queue 都是有界队列。真实设备 RX 先由设备 worker 入队，再由当前 poll owner drain；真实设备 TX 先 dispatch 到 per-device TX queue，再由设备 TX worker 送入驱动。普通用户线程只读写 socket buffer 并请求 poll；UDP 析构会临时取得 required ownership 以同步排空 egress。

关键原则：

- 设备 worker 不持有 `Service` 或 `SocketSet` 锁。
- `PollOwnership` 保证同一时刻只有一个线程推进 smoltcp `Interface`；通常是 net-poll worker，UDP close flush 是例外 owner。
- Router queue 使用 inline `[u8; STANDARD_MTU]`，避免每包堆分配。
- shared RX queue 满时 worker 保留最多 16 项本地批次并重试；TX queue/ARP pending 等其它有界队列按各自策略丢弃并计数。
- loopback 普通 TX 直接注入 `Router.rx_buffer`，少一次队列 hop。

内存域列表说明同一个 packet 在不同阶段由不同对象拥有，不能用 driver ring 容量代表协议缓冲区容量。容量常量将把这些域的默认上限和实例化范围具体化。

## 2. 容量常量

默认容量集中在 `consts.rs`，共同约束协议缓冲区、共享 RX queue、每设备 TX queue 和 Worker 批处理的内存上限。这些常量的单位并不完全相同，维护时必须区分字节、packet 数与 metadata slot，避免用单一乘法错误估算总占用。

```rust
pub const STANDARD_MTU: usize = 1500;

pub const TCP_RX_BUF_LEN: usize = 64 * 1024;
pub const TCP_TX_BUF_LEN: usize = 64 * 1024;
pub const UDP_RX_BUF_LEN: usize = 64 * 1024;
pub const UDP_TX_BUF_LEN: usize = 64 * 1024;
pub const RAW_RX_BUF_LEN: usize = 64 * 1024;
pub const RAW_TX_BUF_LEN: usize = 64 * 1024;

pub const SOCKET_BUFFER_SIZE: usize = 64;
pub const LISTEN_QUEUE_SIZE: usize = 512;
pub const DEVICE_RX_QUEUE_SIZE: usize = 256;
pub const DEVICE_TX_QUEUE_SIZE: usize = 128;
pub const ETHERNET_MAX_PENDING_PACKETS: usize = 128;
```

常量定义给出编译期上限，但实际预算还取决于它被全局、每设备或每 socket 实例化的次数。下表按所有权范围换算默认量级，并明确未包含的 metadata，避免把理论 data buffer 大小当作完整 heap 占用。

| 常量 | 作用范围 | 默认内存预算 |
| --- | --- | --- |
| `SOCKET_BUFFER_SIZE` | `Router.rx_buffer` 和 `Router.tx_buffer` 的 packet metadata 槽位；每个 data buffer 是 `STANDARD_MTU * SOCKET_BUFFER_SIZE` | RX 约 96 KiB，TX 约 96 KiB |
| `DEVICE_RX_QUEUE_SIZE` | 所有真实设备共享的 device-to-Router RX queue | 256 × 1500B，约 384 KiB |
| `DEVICE_TX_QUEUE_SIZE` | 每个真实设备独立的 TX queue | 每设备 128 × 1500B，约 192 KiB |
| `TCP_RX_BUF_LEN` / `TCP_TX_BUF_LEN` | 每个 TCP socket 的 smoltcp byte buffer | 每连接约 128 KiB |
| `UDP_RX_BUF_LEN` / `UDP_TX_BUF_LEN` | 每个 UDP socket 的 smoltcp packet data buffer | 每 socket 约 128 KiB，外加 metadata |
| `RAW_RX_BUF_LEN` / `RAW_TX_BUF_LEN` | 每个 raw socket 的 smoltcp packet data buffer | 每 socket 约 128 KiB，外加 metadata |
| `ETHERNET_MAX_PENDING_PACKETS` | ARP 解析期间暂存的待发送 Ethernet frame | 每 Ethernet device 至少 `128 × 1514B`，约 189 KiB，另加 metadata |
| `LISTEN_QUEUE_SIZE` | TCP `ListenTable` 每个 listen 端口的 accept/SYN 预创建队列容量 | 每 listen 端口 512 项 |

`DEVICE_RX_QUEUE_SIZE` 有意大于 `SOCKET_BUFFER_SIZE`。前者吸收设备 RX worker 与 net-poll worker 调度之间的短 burst；后者是 smoltcp-facing 的单轮 packet buffer。APK index 下载、TCP slow start 或 QEMU user networking burst 都可能在短时间内产生超过 64 个 MTU packet 的入站积压，因此 RX worker 共享队列需要更大的缓冲。

## 3. RX 内存路径

RX 从真实设备到用户态 `recv()` 依次经过 driver buffer、设备 Worker 本地 batch、共享 RX queue、`Router.rx_buffer` 和协议 socket buffer。流程图标出每次复制及所有权转移，尤其用于解释共享队列拥塞为何由 Worker 保留本地 packet 后重试。

```mermaid
flowchart TB
    DriverRx["驱动 RX 内存<br/>rd-net RxQueue / NetRxBuffer"]
    EthRecv["EthernetDevice::recv()<br/>解析 Ethernet/ARP/IPv4"]
    LocalBuf["RX worker 本地 PacketBuffer<br/>16 * STANDARD_MTU"]
    SharedRx["shared RX queue<br/>RxPacket + QueuedPacket<br/>DEVICE_RX_QUEUE_SIZE"]
    RouterRx["Router.rx_buffer<br/>PacketBuffer InterfaceId<br/>SOCKET_BUFFER_SIZE"]
    SmolPoll["smoltcp Interface::poll()"]
    SocketRx["TCP/UDP/raw socket RX buffer"]
    UserRecv["用户 recv/read buffer"]

    DriverRx -->|"copy or adapter receive"| EthRecv
    EthRecv -->|"copy IP payload"| LocalBuf
    LocalBuf -->|"copy inline packet"| SharedRx
    SharedRx -->|"copy drain"| RouterRx
    RouterRx --> SmolPoll
    SmolPoll -->|"protocol copy/store"| SocketRx
    SocketRx -->|"copy to user"| UserRecv
```

流程图按对象展示 RX 所有权域，下面的调用链则把这些域对应到具体 Rust buffer 类型和函数入口。两种视图结合后，可以区分 driver adapter 的首次复制、Worker inline queue 的第二次复制和 smoltcp 对 socket payload 的存储。

```text
NIC / virtqueue / rd-net RX memory
  -> NetRxBuffer / VecRxBuffer
  -> EthernetDevice::recv()
  -> device_rx_worker local PacketBuffer<InterfaceId>
  -> BoundedPacketQueue<RxPacket> (QueuedPacket inline copy)
  -> Router.rx_buffer (PacketBuffer<InterfaceId>)
  -> smoltcp Interface::poll()
  -> TCP/UDP/raw socket RX buffer in SocketSet
  -> socket recv()
  -> user Write / IoBufMut
```

调用链把总体 RX 图映射到实际 buffer 类型，并显示用户 socket 之前至少存在 driver、Worker queue 和 Router 三个所有权转换。驱动接收边界从第一个转换开始说明何时必须复制和释放底层 buffer。

### 3.1 驱动接收边界

`RdNetDriver` 把 `rd-net` 的 RX queue 适配成 `EthernetDriver::receive()`。当前适配层是 copy-based：

```text
rd_net::RxQueue::receive()
  -> VecRxBuffer { data: Vec<u8> }
  -> EthernetDevice::recv()
```

`RX_PREFETCH_TARGET = 1`，只允许一个小的预取窗口，避免在 driver adapter 中形成新的大缓存层。`EthernetDevice::recv()` 解析 Ethernet frame：

- ARP frame：更新 neighbor / pending packet 状态，不进入 smoltcp socket。
- IPv4 frame：校验链路层目标后，把 IP payload 写入调用方提供的 `PacketBuffer<InterfaceId>`。
- 其它 frame：忽略或返回没有可交付 packet。

驱动接收边界中的复制会在释放 driver buffer 前完成，使后续处理不依赖 DMA/ring 生命周期。RX Worker 随后把这些拥有型 packet 组织成本地 batch，并处理共享队列背压。

### 3.2 RX Worker 队列

`device_rx_worker` 用一个本地 `PacketBuffer` 暂存从 `Device::recv()` 得到的 IP packet：

```rust
let mut rx_buffer = PacketBuffer::new(
    vec![PacketMetadata::EMPTY; DEVICE_RX_WORKER_BATCH],
    vec![0u8; STANDARD_MTU * DEVICE_RX_WORKER_BATCH],
);
```

`DEVICE_RX_WORKER_BATCH = 16`，所以单个 RX worker 一轮最多先从设备搬 16 个 packet 到本地 `PacketBuffer`/`VecDeque`，再逐个复制到共享 RX queue。这个 batch 属于每个 worker 的持久任务状态，是容量 16 的有界背压，不是无界全局 backlog。

随后把 packet 复制进共享 RX queue：

```text
local PacketBuffer slice
  -> QueuedPacket { bytes: [u8; STANDARD_MTU], len }
  -> RxPacket { interface_id, bytes }
  -> RouterQueues::rx.push()
```

共享 RX queue 是 `Arc<BoundedPacketQueue<RxPacket>>`，所有非 loopback 设备共用一个队列。队列项保存 ingress `InterfaceId`；`Router::poll()` drain 时才从 IP header 生成包含 traffic-class 的 `RxMetadata`。队列满时：

- 未入队项及其 L2 frame 长度留在 worker 的 `local_batch`。
- 打印 `"{ifname}: RX queue is full, delaying packet"`。
- 调用 `request_poll()` 并 `yield_now()`，给 poll owner 机会 drain backlog，然后重试。

Worker 列表强调本地 batch 在共享队列满时仍由当前任务拥有，因而可以安全 yield 后重试。进入共享队列后，packet 的下一任所有者是 `Router::poll()`，它负责构造协议 metadata。

### 3.3 Router 接收缓冲区

`Service::poll()` 首先调用 `Router::poll()`，把共享 RX queue drain 到 smoltcp-facing `Router.rx_buffer`：

```rust
while !self.rx_buffer.is_full() {
    let Some(packet) = self.queues.rx.pop() else {
        break;
    };
    let bytes = packet.bytes.as_slice();
    snoop_tcp_packet(bytes, sockets);
    snoop(packet.interface_id, bytes);
    let metadata = rx_metadata(packet.interface_id, bytes);
    let Ok(dst) = self.rx_buffer.enqueue(bytes.len(), metadata) else {
        break;
    };
    dst.copy_from_slice(bytes);
}
```

这一步又发生一次 copy：`QueuedPacket` 的 inline bytes 复制到 `Router.rx_buffer`。随后 smoltcp `Interface::poll()` 通过 `Router::receive()` 获取 `RxToken` 并解析 IP/TCP/UDP/raw，最后写入具体 socket 的 RX buffer：

- TCP：写入 TCP socket 的 byte stream RX buffer。
- UDP：写入 UDP packet buffer 和 metadata。
- raw：写入 raw packet buffer；connected peer 不匹配时，`raw.rs` 可把 packet 暂存到 `deferred_rx`。

Router 接收步骤把 ingress 身份和 IP traffic class 转为 smoltcp-facing metadata，并在同一串行 poll 中交给 socket。用户接收只观察协议 socket buffer，不需要了解前面的设备队列或 Router slot。

### 3.4 用户接收边界

用户执行 `recv()` 时，不直接接触 Router queue。IP socket 从 smoltcp socket buffer 复制到 syscall 提供的用户 buffer：

```text
TCP recv:
  smoltcp TCP SocketBuffer
  -> socket.recv(|buf| dst.write(buf))
  -> user buffer

UDP recv:
  smoltcp UDP PacketBuffer
  -> socket.recv() / socket.peek()
  -> dst.write(payload)
  -> user buffer

raw recv:
  smoltcp raw PacketBuffer or deferred_rx / loopback_rx
  -> parse/filter
  -> dst.write(payload)
  -> user buffer
```

阻塞等待由 `GeneralOptions::recv_poller_with()` 处理：如果 socket RX buffer 为空，当前调用注册 waker 并等待；等待期间协议推进仍由 `net-poll` worker 完成。

## 4. TX 内存路径

TX 从用户态 `send()` 写入协议 socket buffer 开始，经 smoltcp 生成 IP packet、`Router.tx_buffer` 选路、per-device TX queue 和 Ethernet framing 后进入驱动。流程中的队列边界让协议核心不阻塞在硬件发送，但队列满时会形成明确的 drop 统计。

```mermaid
flowchart TB
    UserSend["用户 send/write buffer"]
    SocketTx["TCP/UDP/raw socket TX buffer"]
    SmolPoll["smoltcp Interface::poll()"]
    RouterTx["Router.tx_buffer<br/>PacketBuffer placeholder ifindex 0<br/>SOCKET_BUFFER_SIZE"]
    Dispatch["Router::dispatch()<br/>route by dst + src"]
    DevTx["per-device TX queue<br/>TxPacket + QueuedPacket<br/>DEVICE_TX_QUEUE_SIZE"]
    EthSend["EthernetDevice::send()<br/>ARP / Ethernet header"]
    Pending["ARP pending PacketBuffer<br/>ETHERNET_MAX_PENDING_PACKETS"]
    DriverTx["驱动 TX 内存<br/>NetTxBuffer / rd-net TxQueue"]

    UserSend -->|"copy to socket"| SocketTx
    SocketTx --> SmolPoll
    SmolPoll -->|"generate IP packet"| RouterTx
    RouterTx --> Dispatch
    Dispatch -->|"copy selected packet"| DevTx
    DevTx --> EthSend
    EthSend -->|"ARP unresolved"| Pending
    Pending -->|"ARP resolved"| EthSend
    EthSend -->|"copy Ethernet frame"| DriverTx
```

TX 图强调 route dispatch 与 ARP pending 的分支，下面的调用链进一步标出实际 packet 容器从用户 `IoBuf` 到 `NetTxBuffer` 的变化。队列中的 `QueuedPacket` 拥有独立 inline bytes，因此 driver 发送失败不会引用已经释放的 smoltcp buffer。

```text
user Read / IoBuf
  -> smoltcp TCP/UDP/raw socket TX buffer
  -> smoltcp Interface::poll()
  -> Router.tx_buffer
  -> Router::dispatch()
  -> per-device BoundedPacketQueue<TxPacket> (QueuedPacket inline copy)
  -> device_tx_worker
  -> EthernetDevice::send()
  -> NetTxBuffer / VecTxBuffer
  -> driver transmit / rd-net TX queue
```

TX 调用链对应图中的主分支，ARP unresolved 时还会在 `EthernetDevice` 内增加 pending 所有权。用户发送边界只负责把数据交给 socket buffer，不承诺 packet 已进入 Router 或 driver。

### 4.1 用户发送边界

socket `send()` 只写协议 socket buffer，并请求 net-poll worker：

```text
send()
  -> socket.can_send()
  -> socket.send(|buffer| src.read(buffer))
  -> request_poll()
```

TCP 的用户 bytes 进入 TCP TX byte buffer。UDP/raw 发送会申请一个 packet-sized smoltcp buffer，然后把用户 payload 写进去；UDP `MSG_MORE` corking 会在 socket 层暂存第一次 send 的 endpoint/source，最终 flush 时一次性写入 smoltcp UDP packet buffer。

### 4.2 Router 发送缓冲区

net-poll worker 执行 `Interface::poll()` 时，smoltcp 根据 TCP/UDP/raw socket 状态生成完整 IP packet。`Router::transmit()` 返回 `TxToken`，`TxToken::consume()` 把 packet 写入 `Router.tx_buffer`：

```rust
fn consume<R, F>(self, len: usize, f: F) -> R
where
    F: FnOnce(&mut [u8]) -> R,
{
    f(self
        .0
        .enqueue(len, TX_INTERFACE_PLACEHOLDER)
        .expect("This was checked before creating the TxToken"))
}
```

这里的 metadata 使用内部占位 `InterfaceId(0)`，因为真实出接口必须等 IP header 生成后才能按 `(dst, src)` 查 route table。

### 4.3 设备发送队列

`Router::dispatch()` 从 `Router.tx_buffer` 取完整 IP packet：

- loopback：直接复制到 `Router.rx_buffer`。
- IPv4 limited broadcast：复制到所有非 loopback device 的 TX queue。
- 普通单播：解析 `src/dst`，调用 `select_route_for_source(dst, src)`，把 packet 复制到选中设备的 `tx_queue`。

普通 Ethernet TX 入队形态：

```text
Router.tx_buffer packet slice
  -> QueuedPacket { bytes: [u8; STANDARD_MTU], len }
  -> TxPacket { next_hop, bytes }
  -> DeviceHandle.tx_queue
```

per-device TX queue 满时，当前 packet 丢弃并 warning。TX queue 是每个真实设备独立的，避免一个慢设备阻塞其它接口的发送 backlog。

### 4.4 驱动发送边界

`device_tx_worker` 从 per-device queue 取 `TxPacket`，持有设备锁调用 `Device::send(next_hop, packet)`。对 Ethernet 设备而言：

```text
TxPacket IP payload
  -> EthernetDevice::send(next_hop)
  -> neighbor cache / ARP
  -> alloc_tx_buffer(frame_len)
  -> copy Ethernet header + IP payload
  -> driver.transmit(tx_buf)
```

如果 ARP 未解析，`EthernetDevice` 会把 IP packet 暂存在 `pending_packets`，发送 ARP request，等 ARP reply 后再 flush。`pending_packets` 也是有界 `PacketBuffer`，上限由 `ETHERNET_MAX_PENDING_PACKETS` 控制。

`RdNetDriver::alloc_tx_buffer()` 按请求帧长创建 `VecTxBuffer`；`transmit()` 在提交前把短帧补齐到 Ethernet 最小帧长 `ETH_ZLEN = 60`。因此当前普通 Ethernet TX 至少包含两次 copy：用户 buffer 到 smoltcp socket buffer、Router/设备队列到 driver TX buffer；不同协议还可能有额外的协议封装 copy。

## 5. Loopback 内存路径

loopback 是普通 socket TX 的特殊快速路径，`Router::dispatch()` 选择 `InterfaceId::LOOPBACK` 后直接把 IP packet 注入协议侧 RX buffer。它跳过设备 Worker、Ethernet header 和驱动 ring，因此统计口径与真实网卡不同，也不会消耗 per-device queue 容量。

```mermaid
flowchart LR
    UserA["发送端用户 buffer"]
    SockA["发送端 socket TX buffer"]
    RouterTx["Router.tx_buffer"]
    Inject["inject_loopback_rx_direct()<br/>snoop TCP SYN"]
    RouterRx["Router.rx_buffer"]
    Poll["同一轮 Interface::poll()"]
    SockB["接收端 socket RX buffer"]
    UserB["接收端用户 buffer"]

    UserA -->|"copy"| SockA
    SockA --> Poll
    Poll --> RouterTx
    RouterTx --> Inject
    Inject -->|"copy direct"| RouterRx
    RouterRx --> Poll
    Poll --> SockB
    SockB -->|"copy"| UserB
```

loopback 图省略了真实设备域，下面的调用链用具体函数名说明 packet 如何在同一 `Router` 内从 TX buffer 回到 RX buffer。虽然路径更短，它仍需要后续 `Interface::poll()` 才会把数据交付接收 socket。

```text
user send()
  -> smoltcp socket TX buffer
  -> Router.tx_buffer
  -> Router::dispatch()
  -> inject_loopback_rx_direct()
  -> Router.rx_buffer
  -> smoltcp Interface::poll()
  -> peer socket RX buffer
  -> user recv()
```

这个路径不进入 `DeviceHandle.tx_queue`，也不进入共享 `RouterQueues::rx`。它仍会把 IP packet 从 `Router.tx_buffer` 复制到 `Router.rx_buffer`，但避免了早期实现中的 `to_vec()` 分配和额外 RX queue hop。`inject_loopback_rx_direct()` 在写入 `rx_buffer` 前调用 `snoop_tcp_packet()`，因此 loopback TCP SYN 能在同一轮 poll 中预创建 accept child socket。

`send_on_device()` 的 loopback 分支仍可能使用共享 RX queue，这是控制面指定设备发送路径；普通 socket loopback TX 走 direct injection。

## 6. 满队列背压

当前普通 Ethernet 数据面不把 Router queue 满直接映射为用户态 `EAGAIN`，因为 socket send 成功只表示数据进入协议缓冲区。共享 RX queue 和 per-device TX queue 分别采用重试与丢弃策略，维护容量或错误传播时必须保持两种语义可区分。

| 满的位置 | 行为 | 用户可见性 |
| --- | --- | --- |
| smoltcp socket TX buffer 满 | `send()` 返回 `WouldBlock` 或阻塞等待 | 直接可见 |
| smoltcp socket RX buffer 满 | smoltcp 按协议窗口/丢包策略处理 | 间接可见 |
| shared RX queue 满 | RX worker 保留本地 batch，warning，request poll + yield 后重试 | 形成有界背压；不在此处增加 `rx_dropped` |
| Router.rx_buffer 满 | 停止 drain，下一轮继续；直接注入失败时丢包 warning | TCP 通过重传恢复，UDP/raw 可能丢包 |
| per-device TX queue 满 | 丢弃出站 packet，warning | TCP 通过重传恢复，UDP/raw 可能丢包 |
| ARP pending queue 满 | 丢弃等待 ARP 的出站 packet，warning | 连接建立或首包可能超时/重传 |
| driver TX buffer 分配失败 | `Device::send()` 返回失败，packet 已离开 Router queue | 协议层后续重传或应用超时 |

这种策略与很多嵌入式协议栈一致：内部队列保持有界，不把所有链路层瞬时拥塞都反馈到已完成的 socket send 调用。TCP 正确性依赖重传和窗口控制；UDP/raw 本身允许丢包。

## 7. 内存预算示例

内存预算示例以两个 Ethernet 设备和默认容量估算 Router 与 Worker 可见的静态上限，不包含驱动自身 ring、DMA descriptor、socket payload 和 allocator 元数据。它用于比较配置量级，而不是承诺运行期一定预分配或占用该数值。

| 项目 | 估算 |
| --- | --- |
| `Router.rx_buffer` | `64 * 1500` ≈ 96 KiB |
| `Router.tx_buffer` | `64 * 1500` ≈ 96 KiB |
| shared RX queue | `256 * 1500` ≈ 384 KiB |
| per-device TX queue | `2 * 128 * 1500` ≈ 384 KiB |
| ARP pending packets | `2 * 128 * 1514` ≈ 379 KiB |
| 每条 TCP 连接 | RX 64 KiB + TX 64 KiB |
| 每个 UDP/raw socket | RX 64 KiB + TX 64 KiB + metadata |

实际内存还包括 metadata、每设备最多 16 项 RX local batch、`VecDeque` 元素开销、socket 对象、route/DNS/interface registry、Unix/vsock buffer 和驱动队列。特别地，Unix datagram/seqpacket 使用 `async_channel::unbounded()`，不受上述 Router queue 常量限制。调整常量时应按“共享一次”“每设备”“每 socket”“每连接”分别乘算。

![包长度、背压与网卡统计口径](images/packet-accounting.svg)

统计图说明复制次数与计数口径是两个不同问题：Ethernet 字节数按实际二层帧累计，loopback 则按 IP packet 累计。RX 队列短暂满不会立即增加 drop，而 TX queue 或 ARP pending 的有界失败会进入对应丢弃统计。

## 8. 模型边界

当前实现选择有界复制队列和单协议核心，尚未提供零拷贝、page loan 或跨设备 scatter-gather 等更复杂的内存模型。下面的边界用于防止文档把驱动内部 DMA 优化误写成端到端零拷贝，也为后续设计评审明确需要新增的所有权契约。

- 端到端 zero-copy。
- DMA buffer 直接挂入 smoltcp socket buffer。
- RSS / 多队列 NIC / per-queue poll。
- Linux `sk_buff` 类动态链式 backlog。
- `MSG_ZEROCOPY` 或 `io_uring` send/recv path。

如果后续要实现 zero-copy，需要同时改造 `rd-net` buffer ownership、`EthernetDevice` frame 封装、Router queue 生命周期和 smoltcp token/socket buffer 接口。单独把某个队列改成 `Arc<[u8]>` 只能减少局部 copy，不能形成完整 zero-copy 数据面。
