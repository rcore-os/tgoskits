---
sidebar_position: 5
sidebar_label: "多设备实现"
---

# 多设备实现

`ax-net` 使用 **single smoltcp Interface + Router as Device** 的数据面结构。smoltcp 只看到一个 `phy::Device`，这个虚拟设备在内部聚合 loopback、Ethernet 和运行期注册的静态设备，并通过共享路由表把 TX packet 分发到真实出接口。

核心源码：

| 源码 | 职责 |
| --- | --- |
| `router.rs` | `Router` 虚拟设备、route dispatch、bounded queue、loopback 快速路径、RX/TX worker |
| `device/mod.rs` | 内部 `Device` trait、ARP entry 对外模型 |
| `device/ethernet.rs` | Ethernet 帧封装/解析、ARP、IRQ/OOB readiness |
| `device/loopback.rs` | `lo` 接口占位设备，真实回环由 Router 快速路径完成 |
| `device/driver.rs` | `rd-net` 到 `EthernetDriver` 的适配 |
| `service.rs` | `Service::poll()` 调度 Router、smoltcp、DHCP、orphan |
| `lib.rs` | net-poll worker、`request_poll()`、设备注册入口 |

## 设计边界

多设备层的核心是 `Router`——它实现 smoltcp 的 `phy::Device` trait，对协议核心暴露 `Medium::Ip` 层的单一虚拟设备，内部聚合 loopback 和多个 Ethernet 设备。smoltcp 只通过 `Router::receive()`/`transmit()` 读写 IP packet，不感知真实网卡数量。每个 packet 携带 ingress `InterfaceId` 元数据，用于 TCP SYN snoop、DHCP 分发和诊断。

TX 方向由 `Router::dispatch()` 在每次 `Service::poll()` 周期中执行：解析 smoltcp 输出的 IP 包头，通过共享 `RouteTable` 的 `select_route_for_source()` 选择出接口和 next hop。Loopback 目的地址走直接注入快速路径（`inject_loopback_rx_direct()`），在同一 poll 周期内完成 TX→RX 回环；Ethernet 设备的 packet 推入 per-device 有界 TX queue，由专用 TX worker 调用 `Device::send()` 发出。

设备 worker（`device_rx_worker`/`device_tx_worker`）只和有界队列交互，不进入 `Service` 或 `SocketSet` 锁。RX worker 从硬件读取 packet 后推入共享 `RouterQueues::rx`，并调用 `request_poll()` 唤醒 net-poll worker；TX worker 从 per-device TX queue 取出 packet 调用设备发送。这种隔离确保硬件收发延迟不阻塞协议核心，协议核心锁也不阻塞设备收发。

典型关系如下：

```mermaid
flowchart TB
    Service["Service::poll()"] --> Router["Router as smoltcp Device"]
    Service --> Smol["smoltcp Interface + SocketSet"]

    subgraph RouterState["Router state"]
        RxBuf["rx_buffer"]
        TxBuf["tx_buffer"]
        Routes["SharedRouteTable"]
        Devices["Vec<DeviceHandle>"]
        RxQueue["shared bounded RX queue"]
    end

    Router --> RxBuf
    Router --> TxBuf
    Router --> Routes
    Router --> Devices
    Devices --> RxQueue

    DevRx["per-device RX worker"] --> RxQueue
    TxBuf --> Dispatch["Router::dispatch()"]
    Dispatch --> Loopback["loopback direct injection"]
    Dispatch --> DevTx["per-device TX queue"]
    DevTx --> TxWorker["per-device TX worker"]
    TxWorker --> Device["EthernetDevice / driver"]
    Device --> DevRx
```

## 设备抽象层

设备抽象层把硬件细节限制在 `device/*`，Router 只处理完整 IP packet 和 next-hop IP。这样 Ethernet、loopback、OOB Wi-Fi 等设备可以共享同一个 smoltcp 协议核心。

### Device Trait

内部 `Device` trait 是 Router 与具体设备之间的能力边界：

```rust
pub trait Device: Send + Sync {
    fn name(&self) -> &str;

    fn recv(
        &mut self,
        interface_id: InterfaceId,
        buffer: &mut PacketBuffer<InterfaceId>,
        timestamp: Instant,
        snoop: &mut dyn FnMut(&[u8]),
    ) -> bool;

    fn send(&mut self, next_hop: IpAddress, packet: &[u8], timestamp: Instant) -> bool;

    fn set_ipv4_addr(&mut self, _addr: Option<Ipv4Cidr>) {}

    fn arp_entries(&self, _timestamp: Instant) -> Vec<ArpEntry> {
        Vec::new()
    }

    fn wake_rx(&self) {}

    /// Returns the device readiness poll set when the device has a wake source.
    ///
    /// The router uses this to register the global [`NET_POLL_DEVICE_WAKER`]
    /// and to publish readiness to the per-device worker path. Pure-polling
    /// devices should return `None`.
    fn readiness_poll(&self) -> Option<Arc<PollSet>> {
        None
    }
}
```

约束：

- `recv()` 输出完整 IP packet，不输出 Ethernet frame。
- `send()` 输入完整 IP packet 和已选好的 `next_hop`。
- route lookup、source address selection、TCP/UDP/raw 分发都在设备层之上完成。
- 设备只负责链路层封装、邻居解析、硬件 RX/TX 和 readiness。

### LoopbackDevice

`LoopbackDevice` 是 `lo` 的控制面占位设备：

```rust
pub struct LoopbackDevice;

impl Device for LoopbackDevice {
    fn name(&self) -> &str {
        "lo"
    }

    fn recv(...) -> bool {
        false
    }

    fn send(&mut self, _next_hop: IpAddress, _packet: &[u8], _timestamp: Instant) -> bool {
        true
    }

    fn readiness_poll(&self) -> Option<Arc<PollSet>> {
        None
    }
}
```

真实 loopback 数据路径不走 `LoopbackDevice::send()/recv()`，而是在 `Router::dispatch()` 中直接把 smoltcp TX buffer 的 packet 注入 `Router.rx_buffer`。保留这个设备对象是为了让控制面、路由表和 Linux ifindex 能把 `lo` 作为普通接口处理。

### EthernetDevice

`EthernetDevice` 是主要真实设备实现：

```rust
pub struct EthernetDevice {
    name: String,
    inner: Arc<EthernetIrqState>,
    neighbors: HashMap<IpAddress, Neighbor>,
    pending_neighbors: HashMap<IpAddress, PendingNeighbor>,
    ip: Option<Ipv4Cidr>,
    pending_packets: PacketBuffer<'static, IpAddress>,
}
```

职责：

- 从 `EthernetDriver::receive()` 读取 Ethernet frame。
- 解析 ARP 和 IPv4。
- 把 IPv4 payload 上交为完整 IP packet。
- 根据 next hop 做 ARP/neighbor lookup。
- 封装 Ethernet frame 并通过 driver 发送。
- 导出 `/proc/net/arp` 所需的 ARP entry。

### RdNetDriver

`RdNetDriver` 是 `rd-net` 设备到 `EthernetDriver` trait 的适配层。它持有 `rd_net::TxQueue`、`rd_net::RxQueue` 和一个很小的 `pending_rx` 预取队列。Router 不直接依赖 `rd-net` 类型，只依赖内部 `Device` trait。

```text
rd_net::Net
  -> RdNetDriver
  -> EthernetDriver trait
  -> EthernetDevice
  -> Router DeviceHandle
```

适配策略：

- RX：`rd_net::RxQueue::receive()` 返回的 packet 被复制到 `VecRxBuffer`，放入 `pending_rx` 或直接交给 `EthernetDevice`。`RX_PREFETCH_TARGET = 1`，只预取一个 packet，避免形成新的缓存层。
- TX：`alloc_tx_buffer(size)` 返回 `VecTxBuffer`，实际长度为 `max(size, ETH_ZLEN)`，保证 Ethernet 最小帧长 60 字节。
- IRQ：`handle_irq()` 调用底层 irq handler 后尝试预取 RX packet，并根据结果返回 `NetIrqEvents::RX_READY`、`RX_ERROR` 或 `SPURIOUS`。
- 错误：`rd_net::NetError::Retry` 映射为 `NetDeviceError::Again`，`NoMemory` / `NotSupported` 保留语义，link down 或其它错误映射为 `Io`。

这个适配层仍然是 copy-based 的。它的目标是隔离 `rd-net` ownership 模型，而不是提供端到端 zero-copy。后续如果要做 zero-copy，需要同时改造 `rd-net` buffer ownership、`EthernetDevice` frame 封装和 smoltcp token 生命周期。

## Router as MultiDevice

`Router` 是 smoltcp `phy::Device` 的实现，也是单协议核心和多设备数据面之间的适配器。它不是传统意义上只维护 route table 的 router，而是一个 MultiDevice adapter。

### 核心结构

```rust
pub struct Router {
    rx_buffer: PacketBuffer,
    tx_buffer: PacketBuffer,
    queues: Arc<RouterQueues>,
    devices: Vec<Arc<DeviceHandle>>,
    table: SharedRouteTable,
}
```

字段语义：

- `rx_buffer`：smoltcp-facing RX packet buffer，由 `Router::receive()` 消费。
- `tx_buffer`：smoltcp-facing TX packet buffer，由 `TxToken::consume()` 写入。
- `queues.rx`：所有非 loopback 设备 worker 共享的有界 RX 队列。
- `devices`：Router 内部设备索引空间，和公开 `InterfaceId` 分离。
- `table`：与控制面共享的 route table。

### DeviceHandle

每个真实设备对应一个 `DeviceHandle`：

```rust
struct DeviceHandle {
    interface_id: InterfaceId,
    name: String,
    inner: Arc<Mutex<Box<dyn Device>>>,
    rx_queue: Arc<BoundedPacketQueue<RxPacket>>,
    tx_queue: Arc<BoundedPacketQueue<TxPacket>>,
    rx_wake: Arc<WaitQueue>,
    tx_wake: Arc<WaitQueue>,
    rx_waker: Waker,
}
```

RX queue 是所有设备共享的，因为 smoltcp 只能从一个 `Router.rx_buffer` 获取 packet；TX queue 是每设备独立的，因为 dispatch 已经决定了出接口。

`Router::send_on_device()` 允许调用方绕过路由表直接向指定设备发送 packet（如 DHCP 广播包）。该路径只用于控制面的协议辅助（DHCP client/server），不暴露给 socket 路径。

### smoltcp Device 实现

Router 对 smoltcp 暴露 `Medium::Ip`，即 smoltcp 看到的是 IP packet 设备，而不是 Ethernet frame 设备：

```rust
impl smoltcp::phy::Device for Router {
    type RxToken<'a> = RxToken<'a>;
    type TxToken<'a> = TxToken<'a>;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        if self.rx_buffer.is_empty() || self.tx_buffer.is_full() {
            None
        } else {
            let (interface_id, packet) = self.rx_buffer.dequeue().unwrap();
            Some((RxToken { interface_id, packet }, TxToken(&mut self.tx_buffer)))
        }
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        if self.tx_buffer.is_full() {
            None
        } else {
            Some(TxToken(&mut self.tx_buffer))
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = STANDARD_MTU;
        caps.max_burst_size = Some(SOCKET_BUFFER_SIZE);
        caps
    }
}
```

ingress `InterfaceId` 在 `Router::poll()` 阶段用于 TCP SYN snoop、DHCP 分发和后续诊断；进入 smoltcp `RxToken` 后只作为 Router 内部元数据保留，smoltcp 本身仍只消费 IP packet。`TxToken` 写入时先使用内部占位接口 ID，真实出接口由 `Router::dispatch()` 解析 IP header 后按 route table 决定。

## 队列与 Buffer

队列层的目标是有界、低分配和清晰所有权：设备 worker 不持有 `Router` 本体，Router 不直接阻塞在硬件收发上。

### BoundedPacketQueue

`BoundedPacketQueue<T>` 是 Router 和设备 worker 之间的有界 FIFO：

```rust
struct BoundedPacketQueue<T> {
    inner: Mutex<VecDeque<T>>,
    capacity: usize,
    len: AtomicUsize,
}
```

语义：

- `push()` 满时返回 `Err(packet)`，调用方丢包并记录 warning。
- `pop()` 空时返回 `None`。
- `is_empty()` 只读原子 `len`，用于 worker wait predicate。
- 共享 RX queue 容量由 `DEVICE_RX_QUEUE_SIZE` 控制；per-device TX queue 容量由 `DEVICE_TX_QUEUE_SIZE` 控制。

### QueuedPacket

队列中保存的是固定大小 packet buffer，而不是每包堆分配：

```rust
struct QueuedPacket {
    bytes: [u8; STANDARD_MTU],
    len: usize,
}
```

`QueuedPacket::new(packet)` 会拒绝超过 `STANDARD_MTU` 的 packet。这个设计牺牲了端到端 zero-copy，但给出了明确内存上限，并避免早期 loopback 队列路径中的 `to_vec()` 分配。

### RX/TX Packet

```rust
struct RxPacket {
    interface_id: InterfaceId,
    bytes: QueuedPacket,
}

struct TxPacket {
    next_hop: IpAddress,
    bytes: QueuedPacket,
}
```

RX 需要保存 ingress `InterfaceId`，用于 DHCP 分发、诊断和后续扩展；TX 保存的是 route table 已经选择好的 next hop。

## 数据路径

数据路径分为设备 RX、smoltcp poll、TX dispatch 和 loopback 快速路径。所有路径都围绕 `Service::poll()` 批量推进。
端到端的内存所有权、拷贝次数、队列满行为和预算估算见[内存与队列](memory.md)。

### RX Path

RX worker 从真实设备获取 packet，写入共享 RX queue：

```text
EthernetDriver RX
  -> EthernetDevice::recv()
  -> device_rx_worker local PacketBuffer
  -> shared RouterQueues.rx
  -> request_poll()
```

`Router::poll()` 在协议核心线程中把共享 RX queue drain 到 smoltcp-facing `rx_buffer`：

```rust
pub fn poll(
    &mut self,
    _timestamp: Instant,
    sockets: &mut SocketSet<'_>,
    mut snoop: impl FnMut(InterfaceId, &[u8]),
) -> bool {
    let mut moved_rx = false;
    while !self.rx_buffer.is_full() {
        let Some(packet) = self.queues.rx.pop() else {
            break;
        };
        let bytes = packet.bytes.as_slice();
        snoop_tcp_packet(bytes, sockets);
        snoop(packet.interface_id, bytes);
        let Ok(dst) = self.rx_buffer.enqueue(bytes.len(), packet.interface_id) else {
            break;
        };
        dst.copy_from_slice(bytes);
        moved_rx = true;
    }
    moved_rx || !self.queues.rx.is_empty()
}
```

`snoop_tcp_packet()` 在 smoltcp 消费 packet 前识别 TCP SYN，为 listen socket 预创建 child；`snoop(interface_id, bytes)` 用于 DHCP client/server 等按 ingress 接口分发的控制协议。

### TX Path

smoltcp 发送 packet 时只写入 `tx_buffer`，随后由 Router dispatch：

```text
smoltcp socket
  -> TxToken::consume()
  -> Router.tx_buffer
  -> Router::dispatch()
  -> route lookup by dst + source
  -> loopback direct RX or per-device TX queue
```

dispatch 规则：

- IPv4 limited broadcast：复制到所有非 loopback 设备。
- IPv4/IPv6 单播：使用 `select_route_for_source(dst, src)`，确保源地址和出接口一致。
- IPv6 multicast：Router 层会发往非 loopback 设备；完整 Ethernet IPv6/NDP 不在当前设备层完成范围。
- 无 route：记录 warning 并丢弃该 packet。

普通设备 TX 进入对应设备的 TX queue：

```rust
fn enqueue_tx(&self, next_hop: IpAddress, packet: &[u8]) -> bool {
    let Some(bytes) = QueuedPacket::new(packet) else {
        return false;
    };
    if self.tx_queue.push(TxPacket { next_hop, bytes }).is_err() {
        return false;
    }
    self.tx_wake.notify_one(true);
    true
}
```

TX worker 再调用具体设备：

```rust
fn device_tx_worker(device: Arc<DeviceHandle>) {
    loop {
        if let Some(packet) = device.tx_queue.pop() {
            let poll_next =
                device.inner.lock().send(packet.next_hop, packet.bytes.as_slice(), now());
            if poll_next {
                crate::request_poll();
            }
        } else {
            device.tx_wake.wait_until(|| !device.tx_queue.is_empty());
        }
    }
}
```

### Loopback Fast Path

loopback TX 不经过设备 worker，也不进入共享 RX queue。dispatch 选中 `InterfaceId::LOOPBACK` 后直接注入 smoltcp-facing RX buffer：

```rust
fn inject_loopback_rx_direct(
    rx_buffer: &mut PacketBuffer,
    dst_addr: IpAddress,
    packet: &[u8],
    sockets: &mut SocketSet<'_>,
) -> bool {
    snoop_tcp_packet(packet, sockets);
    let Ok(dst) = rx_buffer.enqueue(packet.len(), InterfaceId::LOOPBACK) else {
        warn!("Loopback: RX buffer full, dropping packet to {}", dst_addr);
        return false;
    };
    dst.copy_from_slice(packet);
    true
}
```

这个路径减少了一次队列 hop 和一次 packet 临时分配，并允许 loopback TCP SYN 在同一个 `Service::poll()` 周期内触发 child socket 预创建。

`send_on_device()` 的 loopback 分支仍使用 `inject_loopback_rx()` 写入共享 RX queue，主要用于指定设备发送的控制面 packet；普通 socket TX loopback 走 direct injection。

## Worker 与唤醒

设备 worker 是多设备层和硬件之间的异步边界。worker 不访问 `SocketSet`，也不进入 `Service`。

### Worker 启动

Router 为每个非 loopback 设备启动 RX/TX worker：

```rust
pub fn start_tx_workers(&self) {
    for dev in 0..self.devices.len() {
        self.start_device_tx_worker(dev);
    }
}

fn start_device_tx_worker(&self, dev: usize) {
    let Some(device) = self.devices.get(dev) else {
        return;
    };
    if device.interface_id == InterfaceId::LOOPBACK {
        return;
    }
    ax_task::ThreadBuilder::new(name)
        .spawn(move || device_tx_worker(device))
        .expect("failed to spawn device TX worker")
        .detach_permanent();
}
```

运行期新增静态设备时，`register_static_device()` 会调用 `router.start_device_workers(dev)`，走同一套 worker 模型。

### RX Worker

RX worker 持有设备锁调用 `Device::recv()`，然后把 packet 转成 `RxPacket` 推入共享 RX queue：

```rust
fn device_rx_worker(device: Arc<DeviceHandle>) {
    let mut rx_buffer = PacketBuffer::new(
        vec![PacketMetadata::EMPTY; DEVICE_RX_WORKER_BATCH],
        vec![0u8; STANDARD_MTU * DEVICE_RX_WORKER_BATCH],
    );

    loop {
        let mut received = false;
        {
            let mut device_inner = device.inner.lock();
            let mut snoop = |_packet: &[u8]| {};
            while !rx_buffer.is_full()
                && device_inner.recv(device.interface_id, &mut rx_buffer, now(), &mut snoop)
            {
                received = true;
            }
        }

        while let Ok((interface_id, packet)) = rx_buffer.dequeue() {
            let Some(bytes) = QueuedPacket::new(packet) else {
                continue;
            };
            if device.rx_queue.push(RxPacket { interface_id, bytes }).is_err() {
                break;
            }
            crate::request_poll();
            received = true;
        }

        if !received {
            device.inner.lock().register_waker(&device.rx_waker);
            device.rx_wake.wait();
        }
    }
}
```

### Waker 注册

Router 提供两类 waker 注册：

```rust
pub fn register_device_waker(&self, waker: &Waker) {
    for device in &self.devices {
        device.inner.lock().register_waker(&device.rx_waker);
        device.inner.lock().register_waker(waker);
    }
}

pub fn register_waker(&self, binding: DeviceBinding, waker: &Waker) {
    for device in &self.devices {
        if binding.bound_if.is_none_or(|id| id == device.interface_id) {
            device.inner.lock().register_waker(&device.rx_waker);
            device.inner.lock().register_waker(waker);
        }
    }
}
```

`register_device_waker()` 用于 net-poll worker 的全局设备 readiness；`register_waker(binding, waker)` 用于 socket readiness，只向 `SO_BINDTODEVICE` 或本地地址绑定允许的接口注册。

## Ethernet 链路层

Ethernet 设备在 IP packet 与真实 Ethernet frame 之间转换，并维护 ARP/neighbor 状态。

### RX 处理

`EthernetDevice::recv()` 的入站流程：

1. 从 `EthernetDriver::receive()` 读取一帧。
2. 解析 `EthernetFrame`。
3. ARP frame：更新 neighbor 表、处理 gratuitous request/reply、释放 pending packet。
4. IPv4 frame：校验链路层目标，取出 IP payload，写入 Router 提供的 packet buffer。
5. 其他协议：忽略或记录。

`recv()` 输出给 Router 的始终是 IP packet，而不是 Ethernet frame。

### TX 处理

`EthernetDevice::send(next_hop, packet)` 的出站流程：

1. 查询 `neighbors`。
2. 命中：封装 Ethernet frame 并发送。
3. 未命中但已有 pending ARP：把 packet 放入 `pending_packets`。
4. 未命中且需要重试：发送 ARP request，记录 `pending_neighbors`。

关键参数：

- neighbor TTL：300 秒。
- ARP retry：1 秒。
- pending packet buffer：有界。

### IRQ 与 OOB RX

Ethernet 支持两种 RX readiness 模式：

- IRQ 模式：`EthernetIrqRegistrar` 注册硬件 IRQ action，IRQ 到来后 action 持有驱动提供的 `EthernetIrqHandler` 调用 `handle_irq()`，`ethernet_irq_outcome()` 将 `RX_READY`/`RX_ERROR`/`TX_DONE` 转成 `wake_net_task_irq()`；随后 `net-poll` worker 通过 `wake_all_devices()` 唤醒设备 poll set 和 RX worker。
- OOB RX 模式：用于 SDIO Wi-Fi 等设备，RX 就绪由设备外部线程调用 `wake_net_task_irq()`，唤醒 `net-poll` worker；`register_device_waker()` 同时把设备 readiness poll set 连接到设备 RX worker，使 `{ifname}-rx` worker 重新检查设备。

`register_waker()` 只在存在 IRQ registration 或 OOB RX wake source 时注册：

```rust
fn register_waker(&self, waker: &Waker) {
    if self.inner.irq_registration.get().is_some() || self.inner.oob_rx {
        self.inner.poll_ready.register(waker);
    }
}
```

实际源码通过 `Device::readiness_poll()` 返回 `Option<Arc<PollSet>>` 表达这个条件：只有已注册 IRQ handler 或 `oob_rx = true` 的设备才返回 poll set。纯 polling 设备如果没有 wake source，不能把 waker 挂在永远不会被唤醒的 `poll_ready` 上。

## Service Poll 集成

`Service::poll()` 是 Router、smoltcp 和网络控制协议的汇合点。设备 worker 只负责把 packet 放入队列，真正协议推进由 net-poll worker 调用 `Service::poll()` 完成。

### Poll 顺序

```rust
pub fn poll(&mut self, sockets: &mut SocketSet) -> bool {
    let timestamp = now();
    // 1. router.poll(): drain device RX queue into smoltcp-facing rx_buffer
    // 2. process DHCP client/server snoop events
    // 3. iface.poll(timestamp, &mut router, sockets)
    // 4. DHCP client timers and sends
    // 5. orphan TCP reaping
    // 6. router.dispatch(): route smoltcp TX packets to devices or loopback
}
```

关键顺序：

- `router.poll()` 必须在 `iface.poll()` 前执行，让 smoltcp 能消费新 RX packet。
- DHCP client/server snoop 发生在 packet 进入 smoltcp 前，保留 ingress `InterfaceId`。
- orphan reaper 在持有 `SocketSet` 的 poll 上下文中运行，但删除列表在 orphan 锁外执行。
- `router.dispatch()` 在 smoltcp poll 后执行，把本轮产生的 TX packet 交给真实设备。

### net-poll Worker

socket 和设备路径都只调用轻量 `request_poll()`：

```rust
pub fn request_poll() {
    publish_poll_request(&NET_POLL_REQUESTED, || {
        NET_POLL_WAKE.notify_one(true);
    });
}
```

重复 `request_poll()` 会被 pending 标志合并，只有 `false -> true` 的第一次请求真正唤醒 worker。

设备 readiness 通过两类 waker 分流：

- `NET_POLL_DEVICE_WAKER`：全局设备 waker，用于告诉 net-poll worker 有协议栈工作需要推进。
- `register_waker(binding, waker)`：socket readiness waker，只注册到 `DeviceBinding` 允许的接口，避免绑定到 `eth1` 的 socket 被 `eth0` 的 readiness 无意义唤醒。

专用 net-poll worker 独占调用 `poll_until_idle()`：

```rust
fn poll_until_idle() {
    POLL_AGAIN.store(true, Ordering::Release);
    loop {
        if POLLING_INTERFACES
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        while POLL_AGAIN.swap(false, Ordering::AcqRel) {
            while get_service().poll(&mut SOCKET_SET.inner.lock()) {}
        }
        POLLING_INTERFACES.store(false, Ordering::Release);
        if !POLL_AGAIN.load(Ordering::Acquire) {
            return;
        }
    }
}
```

这个模型保持应用线程、设备 worker 和协议核心驱动线程分离。socket 热路径不会同步执行完整 smoltcp poll，设备 worker 也不会进入 socket set。

## 与控制协议的交界

设备文档只描述控制协议进入数据面的交界，协议状态机细节放在对应文档中。

### DHCP Client/Server

DHCP client 和 DHCP server 都依赖 Router RX snoop 拿到 ingress `InterfaceId`：

```text
device RX
  -> Router::poll()
  -> snoop(interface_id, packet)
  -> DHCP client/server packet classifier
```

DHCP client ACK 会通过 `NetworkStateUpdate` 更新 smoltcp address list、控制面接口快照、DNS 和 route table。DHCP server 的 Offer/Ack 使用 `Router::send_on_device(dev, next_hop, packet, timestamp)` 从指定接口发出。

#### DHCP Client

DHCP client 属于 `Service` 状态，每个启用 DHCP 的 Ethernet 接口对应一个 `DhcpState`。`Router::poll()` 在把 packet 放入 smoltcp RX buffer 前先做 snoop，DHCP UDP packet 会按 ingress `InterfaceId` 分发给对应 `DhcpState`：

```text
Ethernet RX frame
  -> EthernetDevice strips Ethernet/ARP
  -> Router::poll(packet, ingress_if)
  -> DhcpState::process_packet(ingress_if, packet)
  -> optional DhcpEvent
```

`Configured` 事件提交以下状态：

- smoltcp `Interface` 的 IPv4 address list。
- `NetControl` 中的接口 IPv4/gateway snapshot。
- DHCP DNS entries。
- 该接口的 connected route 和 default route。

`Deconfigured` 事件清理同一接口的 DHCP 地址、DNS 和 IPv4 route。这样某个接口 DHCP NAK 不会影响其它接口的静态地址或 DHCP 状态。

#### DHCP Server

内置 DHCP server 用于 SoftAP 或运行期注册的静态服务接口。它不是通用企业 DHCP server，而是一个轻量的 per-interface server：

```rust
pub struct DhcpServer {
    interface_id: InterfaceId,
    dev: usize,
    server_ip: Ipv4Address,
    client_ip: Ipv4Address,
    mac: EthernetAddress,
    enabled: bool,
}
```

设计语义：

- 只处理进入 `interface_id` 对应接口的 DHCP packet。
- 主要响应 Discover 和 Request，生成 Offer/Ack。
- server IP 来自 SoftAP/静态接口地址，client IP 来自 `NetConfig::dhcp_server_client_ip`。
- 使用固定轻量 lease 时间 `LEASE_SECS = 86400`，不维护复杂租约池。
- 发送不经过 smoltcp socket，而是直接通过 `Router::send_on_device(dev, next_hop, packet, timestamp)` 从绑定设备发出。
- 不参与 DHCP client 状态机，也不会更新控制面地址；它服务的是对端客户端。

这个边界避免 DHCP server 和 DHCP client 争抢同一个 UDP socket，也让 SoftAP 设备即使不依赖外部 DHCP 服务也能给对端分配一个简单地址。

### ARP Entries

`arp_entries()` 从所有设备收集邻居表快照：

```rust
pub fn arp_entries(&self, timestamp: Instant) -> Vec<ArpEntry> {
    let mut entries = Vec::new();
    for device in &self.devices {
        entries.extend(device.inner.lock().arp_entries(timestamp));
    }
    entries
}
```

Ethernet 设备返回 ARP entry，loopback 返回空列表。

## 并发边界

多设备层的并发边界以“worker 不进入协议核心，协议核心不阻塞硬件”为原则。

### 锁顺序

典型路径：

```text
net-poll path:
  SERVICE -> SOCKET_SET -> Router -> RouteTable/device queues

device RX worker:
  DeviceHandle.inner -> Device::recv -> shared RX queue -> request_poll()

device TX worker:
  per-device TX queue -> DeviceHandle.inner -> Device::send

socket readiness:
  GeneralOptions -> Service::register_waker -> Router::register_waker -> Device::register_waker
```

禁止的反向路径：

- 设备 worker 持设备锁进入 `Service` 或 `SocketSet`。
- socket 热路径直接调用完整 interface poll。
- Router dispatch 持 route table 锁时执行阻塞设备发送。
- loopback 普通 TX 重新绕到设备队列。

### 性能边界

该设计优先保证简洁和有界资源：

- 单 smoltcp `Interface` 保持 socket handle、wildcard listen 和动态 route 的一致性。
- per-device worker 解耦硬件收发和协议核心。
- 有界队列防止网络热路径无界增长。
- `QueuedPacket` 避免每包堆分配。
- loopback direct injection 避免额外 queue hop。

不承诺端到端 zero-copy。若后续要继续降低复制，需要同时调整 `rd-net` buffer ownership、smoltcp token 和 Router queue 的 packet 生命周期。
