---
sidebar_position: 1
sidebar_label: "概览"
---

# 网络栈概览
TGOSKits 的网络能力收敛在 `net/ax-net`。它是 ArceOS、StarryOS 和 Axvisor 共享的统一网络栈，向上提供 TCP、UDP、raw socket、Unix domain socket、可选 vsock、DNS、DHCP、ARP、接口查询和 readiness/poll 能力，向下通过 `EthernetDriver` 能力边界适配真实网卡；当前标准实现是基于 `rd-net` 的 `RdNetDriver`。

## 源码
源码位于 [net/ax-net/src/](net/ax-net/src/)，入口 [lib.rs](net/ax-net/src/lib.rs)。Socket backend 包括 IP 类（[tcp.rs](net/ax-net/src/tcp.rs)、[udp.rs](net/ax-net/src/udp.rs)、[raw.rs](net/ax-net/src/raw.rs)，基于 smoltcp）、[unix/](net/ax-net/src/unix/)（自包含 stream/dgram，不经 smoltcp）和可选的 [vsock/](net/ax-net/src/vsock/)（基于 `rdif-vsock` 驱动，含 connection manager 与 ring buffer）。

| 模块 | 角色 | 关键类型 |
| --- | --- | --- |
| [lib.rs](net/ax-net/src/lib.rs) | public facade，初始化网络、启动 poll worker、导出 API | `init_network`, `request_poll`, `net_poll_worker` |
| [config.rs](net/ax-net/src/config.rs) | 配置与接口信息类型 | `InterfaceId`, `NetworkConfig`, `InterfaceInfo`, `DeviceBinding` |
| [service.rs](net/ax-net/src/service.rs) | 控制面 + 协议核心调度 | `Service`, `NetControl`, `DhcpState` |
| [router.rs](net/ax-net/src/router.rs) | 路由表、有界队列、smoltcp `Device` 适配 | `Router`, `RouteTable`, `RouteDecision` |
| [wrapper.rs](net/ax-net/src/wrapper.rs) | 全局 `SocketSet` 包装与端口冲突仲裁 | `SocketSetWrapper` |
| [socket.rs](net/ax-net/src/socket.rs) | 统一 socket 抽象 | `SocketOps`, `Socket`, `SocketAddrEx` |
| [options.rs](net/ax-net/src/options.rs) | socket 选项与 `Configurable` trait | `GetSocketOption`, `SetSocketOption`, `TcpInfo` |
| [general.rs](net/ax-net/src/general.rs) | 通用 socket 选项、非阻塞/超时/poll helper | `GeneralOptions` |
| [state.rs](net/ax-net/src/state.rs) | socket 状态机锁 | `StateLock`, `StateGuard` |
| [listen_table.rs](net/ax-net/src/listen_table.rs) | TCP listen/accept 表与 SYN 预创建 | `ListenTable` |
| [tcp.rs](net/ax-net/src/tcp.rs) / [udp.rs](net/ax-net/src/udp.rs) / [raw.rs](net/ax-net/src/raw.rs) | IP socket 实现 | `TcpSocket`, `UdpSocket`, `RawSocket` |
| [orphan.rs](net/ax-net/src/orphan.rs) | TCP orphan socket 回收（RFC 793 TIME_WAIT） | `add_orphan`, `reap_orphans` |
| [dhcp_server.rs](net/ax-net/src/dhcp_server.rs) | 最简 DHCP 服务器（SoftAP 模式） | `DhcpServer` |
| [unix/](net/ax-net/src/unix/) | Unix domain socket | `UnixSocket`, `Transport` |
| [vsock/](net/ax-net/src/vsock/) | 可选 vsock 支持（`vsock` feature） | `VsockSocket`, `VsockTransport` |
| [device/](net/ax-net/src/device/) | loopback、Ethernet、rd-net、vsock 设备适配 | `Device`, `EthernetDevice`, `RdNetDriver` |
| [consts.rs](net/ax-net/src/consts.rs) | 缓冲区大小等常量 | `STANDARD_MTU`, `SOCKET_BUFFER_SIZE` |

## 能力矩阵

| 能力 | 实现方式 | 状态 |
| --- | --- | --- |
| TCP | smoltcp `socket::tcp`，含 keep-alive、Nagle、TCP_INFO、SYN pre-create | 完整 |
| UDP | smoltcp `socket::udp`，含 MSG_MORE corking、端口冲突仲裁 | 完整 |
| Raw IP/ICMP | smoltcp `socket::raw`，含 loopback ICMP echo reply 模拟 | 完整 |
| Unix domain stream | 自包含，`ringbuf` 双向通道 + cmsg 管道 + peer credentials | 完整 |
| Unix domain datagram | 自包含，`async_channel` 无界队列 + cmsg + SO_PASSCRED | 完整 |
| Vsock stream | `rdif-vsock` 驱动 + connection manager + ring buffer stream | 需要 `vsock` feature |
| DHCPv4 client | 内核态 `DhcpState` 状态机，per-interface，启动阻塞等待 | 基础完成 |
| DNS resolver | smoltcp `socket::dns`，自动过滤不可路由 server，5s 超时 | 完整 |
| ARP | `EthernetDevice` 内部 `neighbors: HashMap`（已解析条目, 300s TTL）+ `pending_neighbors: HashMap`（等待 ARP reply 条目, 1s 重试） + `pending_packets: PacketBuffer`（暂存待 ARP 解析后发送的包） | 完整 |
| 多 NIC 路由 | `RouteTable` 最长前缀匹配 + metric 排序 + per-interface 替换 | 完整 |
| IRQ 感知 | `EthernetIrqRegistrar` + `EthernetIrqAction` + IRQ→wake 转换 | 完整 |
| Loopback | 零状态 `LoopbackDevice` + `Router::dispatch()` 快速路径 inline 注入 `rx_buffer`，不经设备 worker 和队列分配 | 完整 |
| TCP orphan 回收 | `orphan.rs`：Drop 后保留 smoltcp socket 直到 FIN/TIME_WAIT 完成，RFC 793 合规 | 完整 |
| DHCP 服务器（SoftAP） | `dhcp_server.rs`：最简单客户端 DHCP 服务器，Discover→Offer、Request→Ack | 完整 |
| OOB RX（SDIO Wi-Fi） | `EthernetDevice::new_oob_rx()` + `notify_oob_rx()` + 独立 poll task | 完整 |
| 动态设备注册 | `register_device_with_config()` 运行时添加静态 IP 设备（Wi-Fi AP） | 完整 |

## 设计原则

- **单协议栈语义优先**：所有 TCP/UDP/raw socket 共享一个 smoltcp `Interface` 和 `SocketSet`，端口冲突、listen 聚合、wildcard bind 等语义自然正确。
- **控制面与数据面分离**：接口查询（`interfaces()`、`interface_by_name()`）走只读 `NetControl`，不进入设备锁或 smoltcp poll。
- **异步 poll 解耦热路径**：socket 操作只调用轻量 `request_poll()` 唤醒 worker，不在调用者上下文同步驱动整个协议栈。
- **能力边界隔离**：`ax-net` 通过 `EthernetDriver` trait 对接网卡驱动，不直接依赖 FDT、PCI、MMIO、DMA 或平台 IRQ ABI。
- **Linux ABI 友好**：`InterfaceId` 直接映射 Linux ifindex，`DeviceBinding` 对应 `SO_BINDTODEVICE`，socket option 覆盖主流 `getsockopt`/`setsockopt` 语义。

### 线程与锁模型

`ax-net` 使用多线程模型，但协议核心串行：

| 线程 | 职责 | 阻塞点 |
| --- | --- | --- |
| `net-poll` worker | 驱动 smoltcp poll、DHCP 状态机、DNS socket 和 TX dispatch；ARP 解析由发送路径中的 Ethernet 设备完成 | `NET_POLL_WAKE.wait_timeout_until()` |
| `{ifname}-rx` worker | 每网卡一个，从 driver 收包压入 `RouterQueues::rx` 有界队列 | `device.rx_wake.wait()` |
| `{ifname}-tx` worker | 每网卡一个，从 `DeviceHandle::tx_queue` 取包调用 driver send | `device.tx_wake.wait_until()` |
| 调用者线程 | 应用/内核线程调用 socket API | `StateLock::lock()`、`block_on(poll_io())` |
| `vsock-poll` worker | vsock 设备轮询，事件分发到 `VSOCK_CONN_MANAGER` | 自适应频率 sleep（100μs→10ms） |
| `{ifname}-oob-poll` | OOB RX 设备（如 SDIO Wi-Fi）的专用 poll task | `OOB_RX_SIGNAL.wait()` |

`NET_POLL_DEVICE_WAKER` 是全局设备 readiness waker。Router 会把它注册给所有允许触发全局协议栈推进的设备；设备 RX/IRQ/OOB 路径只唤醒 worker 和设置 poll 请求，不直接进入 smoltcp `Interface::poll()`。

### 全局锁顺序

严格的锁嵌套顺序，防止死锁：

```
SERVICE (Mutex<Service>)
  → SOCKET_SET.inner (Mutex<SocketSet>)
    → TCP_BOUND_PORTS (Mutex<HashMap<...>>)
      → LISTEN_TABLE.tcp[port] (Mutex)
  → NET_CONTROL.state (RwLock<ControlState>)
```

- `SOCKET_SET.inner` 全局保护 smoltcp `SocketSet`，socket 创建/销毁/访问均需持有。
- `SERVICE` mutex 保护 smoltcp `Interface` 和 DHCP 状态机，poll 期间独占。
- `NET_CONTROL.state` 是独立 RwLock，接口查询（只读）可以在不持有 `SERVICE` 的情况下进行。
- `ListenTable` 条目锁在 `SOCKET_SET` 锁内获取，保证 accept/snoop 的一致性。
- 设备锁（`DeviceHandle.inner`）主要由 `{ifname}-rx` / `{ifname}-tx` worker 独立获取。worker 不应在持有设备锁时反向进入 `SERVICE` 或 `SOCKET_SET`，避免设备路径与协议核心互相阻塞。

## 核心方案

`ax-net` 采用 **单 smoltcp `Interface` + 多设备 `Router`** 架构。详细设计论证见[架构设计 — Single Interface + Multi-Device Router](architecture.md#single-interface--multi-device-router)。

### 与 Linux 实现对比

| 维度 | Linux | ax-net |
| --- | --- | --- |
| 协议栈实例 | 每 net namespace 独立协议栈 | 全局单实例，namespace 仅做可见性过滤 |
| poll 模型 | NAPI + 软中断，per-CPU backlog | 单 `net-poll` worker + `request_poll()` 唤醒 |
| 多 NIC | 独立 netdev + per-device NAPI queue | 单 `Router`（smoltcp `Device`）+ per-device 有界 RX/TX queue |
| ARP/邻居发现 | 内核 neighbour table + GC | `EthernetDevice` 内部 `HashMap` + `NEIGHBOR_TTL=300s` |
| DHCP | 用户态 dhclient / systemd-networkd | 内核态 `DhcpState` 状态机，bootstrap 阻塞启动 |
| Socket 缓冲区 | 动态可调 sk_buff 链 | 固定大小 `PacketBuffer` + 有界 inline packet queue |
| Zero-copy | `MSG_ZEROCOPY` / `io_uring` | 不支持端到端 zero-copy；Router 队列无每包堆分配，loopback 快速路径少一次队列 hop |

### 与 smoltcp 原生使用对比

smoltcp 原生使用需要一个 `phy::Device` + 一个 `Interface`。`ax-net` 在此基础上增加了：

- **多设备路由**：`Router` 实现了 smoltcp 的 `phy::Device` trait，内部管理多个 `DeviceHandle` 和路由表，在 TX 路径解析 IP 包选择出接口。
- **控制面分离**：`NetControl` 独立持有接口 registry、路由表和 DNS 来源信息，socket 查询不需要持有 `Service` 锁。
- **设备队列解耦**：RX/TX worker 通过有界队列连接设备 driver 和 `Router` token 模型，避免 poll 直接阻塞在设备上。
- **DHCP 集成**：内核态 DHCP 状态机在 bootstrap 阶段完成地址获取，而非依赖外部 DHCP client。
- **TCP SYN 预创建**：RX 路径在交付 smoltcp 前用 `snoop_tcp_packet()` 预创建 listen socket，加速 accept。

### 与 lwIP 对比

| 维度 | lwIP | ax-net |
| --- | --- | --- |
| 主要场景 | 嵌入式 MCU（RAM < 64 KiB） | 服务器级 unikernel（128 MiB+） |
| 线程模型 | NO_SYS 单线程 / SYS 多线程 | 多线程，per-device worker + net-poll worker |
| socket API | 有限 POSIX 子集 | `SocketOps` trait + `Configurable`，覆盖 SO_\*/TCP_\*/IP_\* |
| 多接口 | 原生 `netif` + 全局 PCB/socket 管理 | 单 smoltcp 实例 + `Router` 聚合多 NIC + 路由表 + metric |

## 当前限制

### 协议处理串行

单 smoltcp 实例意味着 TCP/UDP 协议状态机在同一 `net-poll` worker 上串行执行。设备队列解耦可以减少收发阻塞，但不能让多核并行处理协议状态机。Linux 的 per-CPU softirq 在这里没有对应物。

### 收发路径仍有拷贝

RX worker 从 driver buffer 复制到有界队列中的 inline packet，Router 再复制到 smoltcp `PacketBuffer`；TX 方向从 smoltcp `PacketBuffer` 复制到 per-device inline queue，再由 driver 发送。当前队列不再为每个包分配 `Box<[u8]>`，loopback dispatch 也直接写 `rx_buffer`，但端到端 zero-copy 仍需要 `rd-net` buffer ownership、packet pool 和 smoltcp token 适配改造。
完整内存所有权和队列模型见[内存与队列](memory.md)。

### DHCP 租约管理不完整

当前重点覆盖 DHCP bootstrap（Discover → Offer → Request → ACK）和 per-interface 状态管理。完整 renew/rebind、租约过期回收和地址冲突检测仍需后续补齐。

### IPv6 支持最小

多接口能力主要保证 IPv4 正确性。完整 IPv6 地址配置（SLAAC/DHCPv6）、邻居发现、IPv6 route、AAAA DNS 查询和 multicast scope 不在当前范围。

### 无 IGMP/MLD

IPv4 multicast 只保证基础发送选择策略。IGMP/MLD snooping、按接口 membership 和 multicast routing 不在当前范围。

### 无完整 net namespace 隔离

StarryOS 目前只做初步可见性过滤（root namespace 可见全部接口）。完整 Linux net namespace 需要独立 route table、接口集合和 resolver 策略。

### 无动态接口管理

动态 link down/up、接口热插拔、队列重建和已存在 socket 的错误传播仍属于后续工作。

### 无高性能 dataplane

RSS、多队列 NIC、per-queue poll、NAPI 类 batch 调度和 zero-copy dataplane 是后续更底层优化，不由当前 `ax-net` 架构自然获得。
