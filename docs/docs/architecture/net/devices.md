---
sidebar_position: 5
sidebar_label: "多设备实现"
---

# 多设备实现

本文详述 `ax-net` 的 Router 多设备适配、设备驱动层、数据面 poll 流程和协议处理实现。架构总览见[总体架构](architecture.md)，Socket 层见 [Socket 系统](sockets.md)。

## Router 内部结构

`Router`（[router.rs](net/ax-net/src/router.rs)）是 smoltcp 与多设备之间的适配层。它的职责更接近 "MultiDevice adapter"：对 smoltcp 暴露一个 `phy::Device`，内部聚合 loopback 和多个 Ethernet 设备，并在 TX 路径根据路由选择真实出接口。

```rust
pub struct Router {
    rx_buffer: PacketBuffer,           // smoltcp Device RX buffer
    tx_buffer: PacketBuffer,           // smoltcp Device TX buffer
    queues: Arc<RouterQueues>,         // 全局 RX 队列（所有设备共享）
    devices: Vec<Arc<DeviceHandle>>,   // 设备列表（按 add_device() 顺序）
    table: SharedRouteTable,           // 路由表
}
```

### BoundedPacketQueue

`BoundedPacketQueue<T>` 是有界 MPSC 队列，容量默认 `SOCKET_BUFFER_SIZE=64`：

```rust
struct BoundedPacketQueue<T> {
    inner: Mutex<VecDeque<T>>,
    capacity: usize,
    len: AtomicUsize,      // 无锁长度读取，用于 is_empty() 检查
}
```

- `push()`：加锁，满则返回 `Err(T)`（上层丢弃并打 warning）。
- `pop()`：加锁，空返回 `None`。
- `is_empty()`：原子读 `len`，无锁。

### DeviceHandle

每设备一个 `DeviceHandle`，持有设备锁、收发队列和唤醒机制：

```rust
struct DeviceHandle {
    interface_id: InterfaceId,
    name: String,
    inner: Arc<Mutex<Box<dyn Device>>>,
    rx_queue: Arc<BoundedPacketQueue<RxPacket>>,  // 指向 RouterQueues::rx（共享）
    tx_queue: Arc<BoundedPacketQueue<TxPacket>>,  // 独立 TX 队列
    rx_wake: Arc<WaitQueue>,
    tx_wake: Arc<WaitQueue>,
    rx_waker: Waker,
}
```

RX 队列是所有设备共享的（`RouterQueues::rx`），因为 smoltcp 从同一个 `Router::rx_buffer` 消费；TX 队列每设备独立，由 `dispatch()` 按路由决策分发。

### TX Dispatch

`Router::dispatch()` 接收 `&mut SocketSet`，从 `tx_buffer` 取出 smoltcp 发出的 IP 包：

- **IPv4 广播**（dst=255.255.255.255）：复制到所有非 loopback Ethernet 设备。
- **IPv4 单播**：按 `select_route_for_source()` 选路由（要求源地址匹配）。如果出接口是 loopback，走快速路径 `inject_loopback_rx_direct()`（snoop TCP + 直接写入 `rx_buffer`）；否则推入对应设备 TX 队列。
- **IPv6 多播**：复制到所有非 loopback Ethernet 设备。
- **IPv6 单播**：按 `select_route_for_source()` 选路由，同样有 loopback 快速路径。

### RX 数据流

`Router::poll()` 从共享 RX 队列消费包到 smoltcp `rx_buffer`，每个包先调 `snoop_tcp_packet()` 检查 TCP SYN 预创建，再调 snoop 回调（DHCP 分发）。

## Ethernet 设备实现

`EthernetDevice`（[ethernet.rs](net/ax-net/src/device/ethernet.rs)）是 `Device` trait 的主要实现：

```rust
pub struct EthernetDevice {
    name: String,
    inner: Arc<EthernetIrqState>,
    neighbors: HashMap<IpAddress, Neighbor>,
    pending_neighbors: HashMap<IpAddress, PendingNeighbor>,
    ip: Option<Ipv4Cidr>,
    pending_packets: PacketBuffer<IpAddress>,
}
```

### ARP 处理

`EthernetDevice::recv()` 处理入站 Ethernet 帧：

1. 调用 `EthernetDriver::receive()` 从硬件获取一帧。
2. 解析 `EthernetFrame`，按 `EthernetProtocol` 分派：
   - **ARP**：更新 neighbor 表（reply 和 gratuitous request），从 `pending_packets` 释放等待的包。
   - **IPv4**：encapsulation 检查，将 payload 写入 smoltcp `PacketBuffer`。

`send()` 路径：

1. 查 `neighbors` 表，有则直接发送。
2. 无则检查 `pending_neighbors`：如果已发送 ARP request 未超时，将包写入 `pending_packets`。
3. 否则发送 ARP request，记录到 `pending_neighbors`。
4. Neighbor TTL = 300s（与 Linux 一致），ARP retry = 1s。

### IRQ 模型

`EthernetIrqState` 管理 IRQ 注册和驱动锁。支持两种 RX 就绪模式：

- **IRQ 驱动**（`new()`）：通过 `EthernetIrqRegistrar` 注册硬件 IRQ，IRQ 触发时 `handle_ethernet_irq()` → `poll_ready.wake()` 唤醒 RX worker。
- **OOB 驱动**（`new_oob_rx()`）：用于 SDIO Wi-Fi 等自带中断线程的设备。RX 就绪由驱动线程调用 `notify_oob_rx()` 唤醒独立 poll task（`{ifname}-oob-poll`），不经过 Ethernet IRQ 框架。

### RdNetDriver

`RdNetDriver`（[driver.rs](net/ax-net/src/device/driver.rs)）是 `EthernetDriver` trait 的 `rd-net` 适配实现。内部持有 `rd_net::TxQueue` 和 `rd_net::RxQueue`，通过预取 `RX_PREFETCH_TARGET=1` 个包到 `pending_rx: VecDeque` 减少锁竞争。

## Loopback 快速路径

`LoopbackDevice`（[loopback.rs](net/ax-net/src/device/loopback.rs)）是零状态占位类型（`pub struct LoopbackDevice;`），`Device` trait 全为 no-op。真正的 loopback 数据路径在 `Router` 中以快速路径实现：

- **TX 方向**（`Router::dispatch()`）：路由选中 loopback 时，`inject_loopback_rx_direct()` 直接将 IP 包写入 `Router.rx_buffer`，并在写入前执行 `snoop_tcp_packet()` 预创建 TCP listen socket。回环包在**同一个 `Service::poll()` 周期内**被 smoltcp 消费。
- **DHCP/特殊发包**（`Router::send_on_device()`）：对 loopback 调用 `inject_loopback_rx()` 写入共享 `RouterQueues::rx`，由下一轮 `Router::poll()` 消费。
- **Worker 跳过**：`start_tx_workers()` 和 `start_rx_workers()` 显式跳过 `InterfaceId::LOOPBACK`。

## 数据面 poll 流程

`Service::poll()` 在每个 poll 周期执行：

```rust
pub fn poll(&mut self, sockets: &mut SocketSet) -> bool {
    let timestamp = now();
    // 1. Router::poll()：drain RX queue → rx_buffer，snoop DHCP client/server
    // 2. 处理 DHCP client 事件（更新地址、路由、DNS）
    // 3. DHCP server 回复通过 send_on_device() 广播
    // 4. smoltcp Interface::poll()：协议状态机推进
    // 5. DHCP client 发包定时器
    // 6. 回收已完成 teardown 的 orphan TCP socket
    // 7. TX dispatch：路由到设备（含 loopback 快速路径）
}
```

关键：`dispatch()` 接收 `&mut SocketSet`，使 loopback 快速路径能在注入 RX buffer 前执行 `snoop_tcp_packet()`。`reap_orphans()` 在持有 SocketSet 锁时执行，删除 smoltcp socket 的动作放在 orphan 锁外执行。

## net-poll Worker

`net_poll_worker` 是协议核心的唯一驱动线程：

```rust
fn net_poll_worker() {
    loop {
        let delay = next_poll_delay();
        let timed_out = NET_POLL_WAKE.wait_timeout_until(delay,
            || NET_POLL_REQUESTED.load(Acquire));
        if !timed_out { NET_POLL_REQUESTED.store(false, Release); }
        poll_until_idle();
    }
}
```

`poll_until_idle()` 使用 `POLLING_INTERFACES` CAS 锁防止重入，循环内**不 yield**——net-poll worker 是专用线程，在有工作时持续批量处理以最大化吞吐。

## DHCP 状态机

`DhcpState`（[service.rs](net/ax-net/src/service.rs)）维护 per-interface DHCP 客户端：

```
Discovering ──Offer──→ Requesting ──ACK──→ Bound
    │                     │                    │
    └──timeout→retry──    └──timeout→retry──   └──NAK→Discovering (reset)
```

- **Discovering**：广播 DHCPDISCOVER，指数退避重试（1,2,4,8... 最多 16 秒间隔）。
- **Requesting**：收到 DHCPOFFER 后广播 DHCPREQUEST。
- **Bound**：收到 DHCPACK，状态转为 Bound。NAK 触发 reset → Discovering。

`process_packet()` 按 ingress `InterfaceId` 匹配，校验 `transaction_id` 和 `client_hardware_address` 防止误收。

## TCP Orphan Socket 回收

当用户关闭 TCP socket（`TcpSocket::drop()`）时，如果连接仍处于活跃 teardown 状态或有未发送数据，socket 转为 orphan 状态（[orphan.rs](net/ax-net/src/orphan.rs)）：

- **目的**：保证 RFC 793 合规的 FIN 四次挥手和 TIME_WAIT，避免对端连接悬挂。
- **回收**：`reap_orphans()` 在每个 `Service::poll()` 周期中调用：
  - `Closed` → 立即移除
  - `TimeWait/FinWait*/LastAck/Closing` → 最长 60 秒后强制移除
  - 其他意外状态 → 60 秒后强制移除并 warn
- **溢出保护**：`ORPHAN_MAX_SOCKETS = 1024`，超限时只 warn 不强制清除正在 teardown 的连接。

## DHCP 服务器（SoftAP 模式）

`DhcpServer`（[dhcp_server.rs](net/ax-net/src/dhcp_server.rs)）是最简 IPv4 DHCP 服务器，用于 Wi-Fi SoftAP 模式：

- 单客户端、单地址租约，处理 Discover→Offer、Request→Ack。
- 不依赖 smoltcp 的 DHCP socket，手工解析/封装 `DhcpRepr → UdpRepr → Ipv4Repr`。
- 入站包在 `Router::poll()` 的 snoop 回调中分发，与 DHCP client 路径完全独立。

## DNS 解析流程

`dns_query()`（[lib.rs](net/ax-net/src/lib.rs)）：

1. `dns_servers()` 获取按 (metric, interface_id, server_ip) 排序的去重 DNS server 列表。
2. 过滤不可路由的 server（通过 `select_route()` 检查可达性）。
3. 在 `SOCKET_SET` 中创建 `dns::Socket`，发起 A 记录查询。
4. 循环 `request_poll()` + `yield_now()`，检查 `get_query_result()`。
5. 默认 5 秒超时，超时返回 `ETIMEDOUT`。
6. `DnsSocketGuard` 在 drop 时从 `SOCKET_SET` 移除 DNS socket。

## 设备能力边界（Device trait）

`ax-net` 不直接依赖硬件驱动框架。所有设备实现统一的内部 `Device` trait（[device/mod.rs](net/ax-net/src/device/mod.rs)）：

```rust
pub trait Device: Send + Sync {
    fn name(&self) -> &str;
    fn recv(&mut self, interface_id: InterfaceId,
            buffer: &mut PacketBuffer<InterfaceId>, timestamp: Instant,
            snoop: &mut dyn FnMut(&[u8])) -> bool;
    fn send(&mut self, next_hop: IpAddress, packet: &[u8], timestamp: Instant) -> bool;
    fn set_ipv4_addr(&mut self, _addr: Option<Ipv4Cidr>) {}
    fn arp_entries(&self, _timestamp: Instant) -> Vec<ArpEntry> { Vec::new() }
    fn register_waker(&self, waker: &Waker);
    fn wake_rx(&self) {}
}
```

两个实现：

- `LoopbackDevice`：零状态占位类型，`Device` trait 全为 no-op。真正的回环在 `Router::dispatch()` 的快速路径中完成。
- `EthernetDevice`：维护 ARP 邻居表（`NEIGHBOR_TTL = 300s`）、pending packet 队列、Ethernet 帧封装/解析，并对接 IRQ 适配。支持 IRQ 驱动和 OOB 驱动两种 RX 模式。
