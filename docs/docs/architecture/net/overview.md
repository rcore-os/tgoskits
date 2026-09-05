---
sidebar_position: 1
sidebar_label: "概览"
---

# 网络栈概览
TGOSKits 的网络能力收敛在 `net/ax-net`。ArceOS 和 StarryOS 直接复用这一实现；Axvisor 仅在启用依赖 `ax-std/net` 的管理服务功能时通过 ArceOS 间接使用。它向上提供 TCP、UDP、raw/ICMP socket、Unix domain socket、可选 vsock、DNS、DHCP、ARP、接口查询、网卡统计和 readiness/poll 能力，向下消费 `rd_net::PreparedNetDevice`：设备一次性拆成 poll group、move-only DMA queue、hard IRQ、task rearm 与 control endpoint。

## 1. 源码边界
源码位于 `net/ax-net/src/`，入口 `lib.rs`。Socket backend 包括 IP 类（`tcp.rs`、`udp.rs`、`raw.rs`，基于 smoltcp）、`unix/`（自包含 stream/dgram，不经 smoltcp）和可选的 `vsock/`（基于 `rdif-vsock` 驱动，含 connection manager 与 ring buffer）。

| 模块 | 角色 | 关键类型 |
| --- | --- | --- |
| `lib.rs` | public facade，初始化唯一 protocol executor、导出 API | `init_network`, `request_poll`, `reconfigure_wifi` |
| `poll_runtime.rs` | protocol generation 与唯一 poll owner | `ProtocolPollRuntime`, `PollGeneration` |
| `queue_runtime/` | affinity domain、fixed-CPU queue executor、SPSC、IRQ 生命周期 | `NetworkRuntimeBuilder`, `NetworkQueueRuntime`, `PollGroupState` |
| `config.rs` | 配置与接口信息类型 | `InterfaceId`, `NetworkConfig`, `InterfaceInfo`, `DeviceBinding` |
| `service.rs` | 控制面 + 协议核心调度 | `Service`, `NetControl`, `DhcpState` |
| `router.rs` | 路由表、RX 元数据、网卡统计、smoltcp `Device` 适配 | `Router`, `RouteTable`, `RxMetadata`, `NetDevStats` |
| `wrapper.rs` | 全局 `SocketSet` 包装与端口冲突仲裁 | `SocketSetWrapper` |
| `socket.rs` | 统一 socket 抽象 | `SocketOps`, `Socket`, `SocketAddrEx` |
| `addr.rs` | 共享地址 helper：临时端口分配（0xc000–0xffff）、listen 地址冲突判定 | `allocate_ephemeral_port`, `listen_addrs_conflict` |
| `ip_tos.rs` | per-socket egress IP_TOS/traffic-class：smoltcp 不暴露 TOS 设置，在 Router 边界改写发出的 IP 包头 | `EgressIpTosKey` |
| `rx_meta.rs` | 利用 smoltcp `PacketMeta` id 携带接收侧 QoS 元数据，供 recvmsg cmsg 上报 | `ReceivedTrafficClass` |
| `options.rs` | socket 选项与 `Configurable` trait | `GetSocketOption`, `SetSocketOption`, `TcpInfo` |
| `general.rs` | 通用 socket 选项、非阻塞/超时/poll helper | `GeneralOptions` |
| `state.rs` | socket 状态机锁 | `StateLock`, `StateGuard` |
| `listen_table.rs` | TCP listen/accept 表与 SYN 预创建 | `ListenTable` |
| `tcp.rs` / `udp.rs` / `raw.rs` | IP socket 实现 | `TcpSocket`, `UdpSocket`, `RawSocket` |
| `orphan.rs` | TCP orphan socket 回收（RFC 793 TIME_WAIT） | `add_orphan`, `reap_orphans` |
| `dhcp_server.rs` | 最简 DHCP 服务器（SoftAP 模式） | `DhcpServer` |
| `unix/` | Unix domain socket | `UnixSocket`, `Transport` |
| `vsock/` | 可选 vsock 支持（`vsock` feature） | `VsockSocket`, `VsockStreamTransport` |
| `device/` | loopback、Ethernet frame 与 vsock 设备适配 | `Device`, `EthernetDevice`, `EthernetFramePort` |
| `consts.rs` | 缓冲区大小等常量 | `STANDARD_MTU`, `SOCKET_BUFFER_SIZE` |

源码边界表显示核心状态集中在 `ax-net`，而 runtime 与 StarryOS 只承担设备接入和 ABI 适配。能力矩阵将基于这些真实所有者区分主路径与限制，避免从依赖名称推断尚未接入的功能。

## 2. 能力矩阵

能力矩阵区分当前主路径、受限能力和仅由特定 transport 提供的功能，避免把 smoltcp 编译 feature 等同于完整 OS 支持。每一项都对应 `Service`、具体 socket backend、设备层或 StarryOS ABI 的代码锚点，扩展能力时应同步更新实现状态和限制说明。

| 能力 | 实现方式 | 状态 |
| --- | --- | --- |
| TCP | smoltcp `socket::tcp`，含 keep-alive、Nagle、保守的 `TCP_INFO`、SYN pre-create | 主路径可用 |
| UDP | smoltcp `socket::udp`，含 `MSG_MORE` corking、`SO_REUSEPORT` 共同绑定和 close 前 egress flush | 主路径可用 |
| Raw IP/ICMP | smoltcp `socket::raw`，含 IPv4 ping datagram 和 loopback ICMP echo reply 模拟 | 主路径可用 |
| Unix domain stream | 自包含，`ringbuf` 双向通道 + cmsg 管道 + peer credentials | 完整 |
| Unix domain datagram / seqpacket | 自包含，`async_channel` 无界消息队列 + cloneable cmsg + `SO_PASSCRED`/`SO_TIMESTAMP` | 主路径可用 |
| Vsock stream | `rdif-vsock` 驱动 + connection manager + ring buffer stream | 需要 `vsock` feature |
| DHCPv4 client | 内核态 `DhcpState` 状态机，per-interface，启动阻塞等待 | 基础完成 |
| DNS resolver | smoltcp `socket::dns`，自动过滤不可路由 server，5s 超时 | 完整 |
| ARP | `EthernetDevice` 内部 `neighbors: HashMap`（已解析条目, 300s TTL）+ `pending_neighbors: HashMap`（等待 ARP reply 条目, 1s 重试） + `pending_packets: PacketBuffer`（暂存待 ARP 解析后发送的包） | 完整 |
| 多 NIC 路由 | `RouteTable` 最长前缀匹配 + metric 排序 + per-interface 替换 | 完整 |
| IRQ 感知 | `PinnedNetIrqRegistrar` + `NetHardIrqEndpoint` + group-local schedule | fixed affinity 必需；能力不足时物理网络初始化失败 |
| Loopback | 零状态 `LoopbackDevice` + `Router::dispatch()` 快速路径 inline 注入 `rx_buffer`，不经硬件 queue domain 和 DMA ring | 完整 |
| TCP orphan 回收 | `orphan.rs`：Drop 后保留 smoltcp socket 直到 FIN/TIME_WAIT 完成，RFC 793 合规 | 完整 |
| DHCP 服务器（SoftAP） | `dhcp_server.rs`：最简单的单客户端 DHCP 服务器，仅支持 Discover→Offer、Request→Ack 交换，不维护租约数据库、不做冲突检测，仅回复配置接口收到的 DHCP 包 | 基础完成 |
| Queue-level NAPI | group-local `IDLE/SCHEDULED/POLLING/MISSED/DISABLED`、4×64 子预算、每 CPU 256 总预算、原子 rearm | 主路径；有线与 AIC/SDHCI 共用 |
| 动态设备注册 | 启动后新增/删除物理 NIC | 不支持；所有设备由 builder 一次性消费并原子发布 |
| Wi-Fi STA/AP 重配 | `WifiTransaction` 进入 owner-CPU 有界 control queue，成功后提交 STA DHCP 或 SoftAP 地址/DHCP server | 基础完成；DC/DW 变体 fail-closed |
| QoS/TOS 兼容 | `IP_TOS` 发包时在 Router 边界改写 IP header；`IP_RECVTOS`/`IPV6_RECVTCLASS` 通过 smoltcp `PacketMeta` 返回 cmsg；`SO_PRIORITY` 仅保存兼容值 | 基础完成 |
| 运行期 IPv4 地址 | `set_interface_ipv4()` / `remove_interface_ipv4()` 原子更新接口、connected route 和 DHCP 状态 | 每接口仅一个 IPv4，无运行期 gateway 参数 |
| TX checksum offload | `TxChecksumCapabilities` 与逐包 `TxSubmitOptions`；按所有已注册物理出口的共同能力向 smoltcp 公布 | 可选；RTL8125 支持，其他设备默认软件 checksum |
| 网卡统计 | `NetDevStats` 汇总 L2 包/字节、错误和丢包，供 StarryOS `/proc/net/dev` 使用 | 累计统计，不含硬件专属计数器 |

矩阵中的“支持”意味着存在可用实现路径，“受限”则需要结合后文章节理解范围。设计原则说明这些取舍为何围绕单协议核心、多设备 Router 和有界资源展开。

## 3. 设计原则

`ax-net` 的设计原则围绕单协议核心、多设备适配和明确所有权展开，目标是在保留普通多宿主 socket 语义的同时隔离可能阻塞的设备 I/O。以下原则分别落实到 `Service`、`Router`、`NetControl`、poll-group SPSC 和 `ProtocolPollRuntime`，是评审结构变更时的基本约束。

- **单协议栈语义优先**：所有 TCP/UDP/raw socket 共享一个 smoltcp `Interface` 和 `SocketSet`，端口冲突、listen 聚合、wildcard bind 等语义自然正确。
- **控制面与数据面分离**：接口查询（`interfaces()`、`interface_by_name()`）走只读 `NetControl`，不进入设备锁或 smoltcp poll。
- **唯一协议 owner**：普通 socket 操作只调用轻量 `request_poll()` 发布 generation；`flush_egress()` 等待完成，但调用者永远不执行 smoltcp poll。
- **同核 queue owner**：hard IRQ callback、mask/ack、queue poll、DMA reclaim/refill 和 rearm 必须等于该 affinity domain 的 `owner_cpu`。
- **能力边界隔离**：`ax-net` 通过 consumable parts 对接网卡驱动，不直接依赖 FDT、PCI 或 MMIO；DMA/IRQ ownership 由 typed endpoint 表达。
- **Linux ABI 友好**：`InterfaceId` 直接映射 Linux ifindex，`DeviceBinding` 对应 `SO_BINDTODEVICE`，socket option 覆盖主流 `getsockopt`/`setsockopt` 语义。

原则列表把状态所有权、队列和轮询请求放在同一个设计框架中，任何局部优化都不能破坏这些边界。线程锁模型进一步说明哪些部分实际并行，哪些部分仍必须串行推进。

### 3.1 线程锁模型

`ax-net` 使用多 CPU queue I/O 和串行协议推进的两级流水线：每个 IRQ affinity domain 由 fixed-CPU queue executor 拥有，而 smoltcp `Interface` 与全局 `SocketSet` 只由一个 protocol executor 访问。这个边界把 DMA/队列并行性留在设备层，同时避免协议状态机并发进入。

| 线程 | 职责 | 阻塞点 |
| --- | --- | --- |
| `net-protocol` | 固定 CPU 的唯一 smoltcp owner，处理 DHCP、DNS、socket、ARP/TX dispatch | generation notify 或协议 timer deadline |
| `net-queue-cpuN` | 固定 CPU，服务该 CPU 的 poll groups；预算 drain、DMA、backpressure、rearm | 本 CPU IRQ notify 或精准 task-side ring-space notify；无周期 timeout |
| 调用者线程 | 应用/内核线程调用 socket API | `StateLock::lock()`、`block_on(poll_io())` |
| `vsock-poll` worker | vsock 设备轮询，事件分发到 `VSOCK_CONN_MANAGER` | 自适应频率 sleep（100μs→10ms） |

物理 IRQ 只激活其 `NetPollGroup`；无关 group 和空闲 queue executor 不被唤醒。queue/protocol 之间仅通过预分配 SPSC move frame/token，queue executor 不直接进入 smoltcp。
完整锁类型、锁顺序和禁止模式见[锁与并发](locks.md)。

![ax-net 运行时所有权与数据通道](images/runtime-ownership.svg)

运行时图把普通唤醒与 UDP 同步 flush 区分开，并显示设备 I/O 不持有协议核心。锁顺序章节将在相同所有权边界上给出具体嵌套规则。

### 3.2 全局锁顺序

全局锁顺序从 `SERVICE`、`SocketSet` 向控制面和局部 side table 单向展开，queue executor 则独占硬件 queue endpoint。维护代码时需要检查任何新回调是否反向进入上层锁，尤其不能在 hard IRQ 或 queue ownership 内等待协议核心。

```
SERVICE (Mutex<Service>)
  → SOCKET_SET.inner (Mutex<SocketSet>)
    → TCP_BOUND_PORTS (Mutex<HashMap<...>>)
      → LISTEN_TABLE.tcp[port] (Mutex)
  → NET_CONTROL.state (RwLock<ControlState>)
```

这条主链描述协议核心与控制面的获取方向，局部 side table 和设备锁则在对应分支上继续细化。下面的要点说明每个锁实际保护的对象和禁止的反向调用，完整路径可在锁专题中核对。

- `SOCKET_SET.inner` 全局保护 smoltcp `SocketSet`，socket 创建/销毁/访问均需持有。
- `SERVICE` mutex 保护 smoltcp `Interface` 和 DHCP 状态机，poll 期间独占。
- `NET_CONTROL.state` 是独立 RwLock，接口查询（只读）可以在不持有 `SERVICE` 的情况下进行。
- `ListenTable` 条目锁在 `SOCKET_SET` 锁内获取，保证 accept/snoop 的一致性。
- queue endpoint 只由其 owner executor 持有，不需要和 protocol core 共享 queue mutex；hard IRQ 只访问 endpoint-owned snapshot/atomic state。
更细的控制面、Router、socket、Unix 和 vsock 锁划分见[锁与并发](locks.md)。

## 4. 核心方案

`ax-net` 采用 **单 smoltcp `Interface` + 多设备 `Router`** 架构。详细设计论证见`架构设计 — Single Interface + Multi-Device Router`。

### 4.1 Linux 对比

Linux 与 `ax-net` 都需要解决接口身份、路由、邻居和 socket 语义，但二者的并行规模与内部对象模型不同。对比表用于说明哪些概念可以借鉴 Linux 行为，哪些实现细节受单 smoltcp `Interface` 和静态资源预算限制而不能直接类比。

| 维度 | Linux | ax-net |
| --- | --- | --- |
| 协议栈实例 | 每 net namespace 独立协议栈 | 全局单实例，namespace 仅做可见性过滤 |
| poll 模型 | NAPI + 软中断，per-CPU backlog | fixed-CPU poll group + `SCHEDULED/MISSED` + budget/rearm；单 protocol executor |
| 多 NIC | 独立 netdev + per-device NAPI queue | 单 `Router` + 多 affinity domain；独立 IRQ source 可分布到不同 CPU |
| ARP/邻居发现 | 内核 neighbour table + GC | `EthernetDevice` 内部 `HashMap` + `NEIGHBOR_TTL=300s` |
| DHCP | 用户态 dhclient / systemd-networkd | 内核态 `DhcpState` 状态机，bootstrap 阻塞启动 |
| Socket 缓冲区 | 动态可调 sk_buff 链 | TCP 每方向固定 256 KiB；UDP/raw 使用 `PacketBuffer`，设备 FIFO 有界 |
| Zero-copy | `MSG_ZEROCOPY` / `io_uring` | DMA RX token 保留至 smoltcp 消费，TX 直接在 DMA buffer 组帧；不提供用户态端到端 zero-copy |

Linux 对比表说明 `ax-net` 复用行为概念但采用更小的单核心实现，并不尝试复制 Linux 内部子系统。下一节转向 smoltcp 原生模型，解释 Router、控制面和 socket 兼容层具体增加了什么。

### 4.2 smoltcp 对比

smoltcp 原生使用需要一个 `phy::Device` + 一个 `Interface`。`ax-net` 在此基础上增加了：

- **多设备路由**：`Router` 实现了 smoltcp 的 `phy::Device` trait，内部管理多个 `DeviceHandle` 和路由表，在 TX 路径解析 IP 包选择出接口。
- **控制面分离**：`NetControl` 独立持有接口 registry、路由表和 DNS 来源信息，socket 查询不需要持有 `Service` 锁。
- **设备队列解耦**：queue owner 与 protocol owner 通过有界 SPSC 连接；ring 满时保留 move-only token、保持 IRQ mask，并由空间释放精准再激活。
- **DHCP 集成**：内核态 DHCP 状态机在 bootstrap 阶段完成地址获取，而非依赖外部 DHCP client。
- **TCP SYN 预创建**：RX 路径在交付 smoltcp 前用 `snoop_tcp_packet()` 预创建 listen socket，加速 accept。

新增能力列表表明 `ax-net` 的主要工作位于 smoltcp 外围，而非重写 TCP/IP 状态机。与 lwIP 的比较进一步聚焦多接口组织方式和系统集成边界。

### 4.3 lwIP 对比

lwIP 常以多个 `netif` 连接一个协议栈，而 `ax-net` 用 `Router` 向 smoltcp 暴露单个虚拟设备，再在内部完成多接口聚合。对比这两种组织方式有助于理解为什么接口 registry 和路由表位于外围控制面，以及为什么 socket 不直接持有具体设备对象。

| 维度 | lwIP | ax-net |
| --- | --- | --- |
| 主要场景 | 嵌入式 MCU（RAM < 64 KiB） | 服务器级 unikernel（128 MiB+） |
| 线程模型 | NO_SYS 单线程 / SYS 多线程 | fixed-CPU queue executors + 单 protocol executor |
| socket API | 有限 POSIX 子集 | `SocketOps` trait + `Configurable`，覆盖 SO_\*/TCP_\*/IP_\* |
| 多接口 | 原生 `netif` + 全局 PCB/socket 管理 | 单 smoltcp 实例 + `Router` 聚合多 NIC + 路由表 + metric |

lwIP 对比表说明多接口协议栈可以采用不同内部拓扑，当前选择优先保持统一 socket 语义。对应代价集中在串行协议处理和复制路径，下面的限制章节会逐项界定这些边界。

## 5. 当前限制

当前限制来自单 smoltcp 协议核心、以 IPv4 为主的物理 Ethernet 路径和有界复制队列，而不是单个未实现函数。以下章节分别说明并行性、内存、控制协议、IPv6、命名空间与高性能数据面的边界，使用者应据此判断目标场景是否需要额外设计。

### 5.1 协议处理串行

单 smoltcp 实例意味着 TCP/UDP 协议状态机在同一 protocol executor 上串行执行。独立 IRQ domain、DMA 与 queue batch 可在多核流水并行，但不能让多核并行处理协议状态机。

### 5.2 收发路径拷贝

RX completion 先补 replacement，再以 `ProtocolRxFrame` 持有 DMA token，经 EthernetDevice、Router 传到 smoltcp `RxToken::consume`；消费结束后回收到原 queue。DMA 主路径不再复制整帧或中间 IP payload。TX 在可用 DMA buffer 中直接组帧，设备 FIFO 积压时才保留兼容 frame。非 DMA 端口继续使用复制路径，socket buffer 与用户 buffer 的复制仍然存在。
完整内存所有权和队列模型见[内存与队列](memory.md)。

### 5.3 DHCP 租约管理

当前重点覆盖 DHCP bootstrap（Discover → Offer → Request → ACK）和 per-interface 状态管理。完整 renew/rebind、租约过期回收和地址冲突检测仍需后续补齐。

### 5.4 IPv6 支持

smoltcp 和 Router 能解析 IPv6、保存 traffic-class 元数据并做 IPv6 route/multicast 选择，但当前 `EthernetDevice` 只解封装 IPv4/ARP，发送也固定封装为 IPv4 EtherType。因此外部 Ethernet IPv6 数据面尚不可用；SLAAC/DHCPv6、NDP、IPv6 route 配置、AAAA DNS 查询和 multicast scope 也不在当前范围。

### 5.5 组播管理

IPv4 multicast 只保证基础发送选择策略。IGMP/MLD snooping、按接口 membership 和 multicast routing 不在当前范围。

### 5.6 网络命名空间

StarryOS 目前只做初步可见性过滤（root namespace 可见全部接口）。完整 Linux net namespace 需要独立 route table、接口集合和 resolver 策略。

### 5.7 动态接口管理

动态 link down/up、接口热插拔、队列重建和已存在 socket 的错误传播仍属于后续工作。

### 5.8 高性能数据面

queue-level batch poll 已实现，但当前生产 backend 均只发布 queue-0 group。RTL8125 支持逐包 checksum offload 和批次 doorbell；不满足 descriptor 约束的包使用软件 checksum。真正启用 virtio/fxmac 多硬件队列、RSS/RPS/RFS、GRO、busy-poll 与用户态 zero-copy 仍是后续工作。checksum 能力在网络初始化时汇总，不支持运行期新增设备后的能力刷新。
