# `ax-net` 多网口支持与性能优化方案

## 1. 背景

`net/ax-net` 已经完成统一网络栈收敛，核心能力包括 TCP、UDP、raw socket、Unix domain socket、可选 vsock、DNS、DHCP、ARP、poll/waker 和 `rd-net` 设备适配。为了进一步支持多网口、多路由和更稳定的数据面性能，需要在现有单 smoltcp `Interface` 架构上补齐控制面、多设备数据面和 socket 绑定语义。

当前需要解决的主要问题：

- 初始化流程偏向第一个 NIC，接口语义容易隐含为 `eth0`。
- `NetworkConfig` 不能完整表达每个接口的地址、网关、DNS、DHCP、metric。
- DHCP、DNS、路由更新等状态偏单接口模型。
- StarryOS ioctl、AF_PACKET、`/proc/net/arp` 等路径容易出现固定 `eth0` 假设。
- 旧设备绑定如果使用 `u32 device_mask`，接口集合语义不清晰，扩展性差。
- socket 热路径不应同步推进完整协议栈，应只请求专用 poll worker 工作。
- 设备收发、smoltcp poll、`SocketSet` 访问需要清晰锁边界，避免设备路径和协议核心互相阻塞。

本方案目标：

- 注册并管理所有可用 NIC，形成 `lo`、`eth0`、`eth1` 等接口。
- 支持每个接口独立静态 IPv4 或 DHCP 配置。
- 支持多条直连路由、默认路由、metric、源地址选择和下一跳选择。
- 提供统一接口 registry API，供 ArceOS、StarryOS、Axvisor 和诊断工具使用。
- 保持单 `smoltcp::iface::Interface + SocketSet` 架构，避免 multi-smoltcp domain 带来的 socket 绑定、动态路由和 wildcard listen 聚合复杂度。
- 通过设备队列解耦、异步 poll、控制面快照、有界队列和减少热路径分配提升性能。
- 删除旧单网口假设和旧 `u32 device_mask` 兼容路径，保持代码简洁。

非目标：

- 不拆分为多个 smoltcp 实例。
- 不实现完整 IPv6。
- 不实现 Linux net namespace 的完整隔离。
- 不承诺固定性能提升比例，性能效果以 benchmark 为准。
- 不为了过渡兼容保留复杂 wrapper 或多套状态。

## 2. 修改方案

### 2.1 总体架构

方案保持 **单协议栈核心 + 多设备 Router** 模型：

```text
public API
  -> tcp / udp / raw / unix / vsock
  -> interfaces() / default_routes() / dns_query()

control plane
  -> interface registry
  -> shared RouteTable
  -> DNS registry
  -> DeviceBinding
  -> network state transaction commit

single protocol core
  -> Service
       -> smoltcp Interface
       -> Router as smoltcp Device
       -> DHCP states
       -> DHCP server
       -> orphan reaper
  -> SocketSetWrapper
  -> ListenTable

multi-device data plane
  -> Router.rx_buffer / Router.tx_buffer
  -> shared bounded RX queue
  -> per-device bounded TX queues

device workers
  -> per-device RX worker
  -> per-device TX worker
  -> OOB RX poll task for dedicated-poll devices
```

核心原则：

- `Router` 是 smoltcp `Device` 适配层，由 `Service` 独占，不作为全局对象被外部直接调用。
- 设备 worker 不进入 smoltcp `Interface` 或 `SocketSet`。
- 设备 worker 与协议核心之间通过有界队列传递 packet。
- `SocketSetWrapper` 仍然是全局 socket 集合，但 socket 热路径只请求 poll，不同步推进完整网络栈。
- 控制面状态使用短锁和快照；数据面 poll 只在必要时进入 smoltcp 临界区。
- 不需要额外拆出 `ServiceCore` / `ServiceHandle` 层级，除非后续出现明确的所有权或锁边界收益。

### 2.2 配置模型

`NetworkConfig` 改为接口级配置：

```rust
pub struct NetworkConfig {
    pub interfaces: Vec<InterfaceConfig>,
    pub default_dns_servers: Vec<Ipv4Addr>,
}

pub struct InterfaceConfig {
    pub name: String,
    pub match_by: InterfaceMatcher,
    pub static_ip: Option<StaticIpConfig>,
    pub dhcp: bool,
    pub metric: u32,
    pub dns_servers: Vec<Ipv4Addr>,
}

pub enum InterfaceMatcher {
    ByOrder(usize),
    ByMac(EthernetAddress),
    ByDriverName(String),
}
```

配置语义：

- 未显式配置的 Ethernet 接口默认启用 DHCP。
- `dhcp = true` 与 `static_ip.is_some()` 互斥，配置冲突直接 panic。
- 显式配置必须能匹配到唯一设备，否则初始化失败。
- 接口名必须唯一。
- `lo` 由 `ax-net` 固定创建，不允许外部配置覆盖。
- 删除旧全局 `AX_IP`、`AX_GW`、`AX_PREFIX_LEN`、`AX_DNS` 单网口语义。
- `ax-runtime` 只负责把结构化接口配置传入 `ax_net::init_network()`。

### 2.3 接口与控制面模型

新增统一接口标识和对外快照：

```rust
pub struct InterfaceId(u32);

pub struct InterfaceInfo {
    pub id: InterfaceId,
    pub name: String,
    pub kind: InterfaceKind,
    pub mac: Option<EthernetAddress>,
    pub ipv4: Option<Ipv4InterfaceConfig>,
    pub mtu: usize,
    pub flags: InterfaceFlags,
    pub metric: u32,
}
```

`InterfaceId` 是 `ax-net` 内部和对外统一的接口标识。StarryOS 的 Linux `ifindex` 直接由 `InterfaceId` 的数值映射得到，不再维护单独的 `IfIndex`。

控制面建议使用一个 `NetControl` 聚合接口 registry、DNS entries 和共享路由表：

```rust
struct ControlState {
    interfaces: Vec<NetInterface>,
    dns: Vec<DnsServerEntry>,
}

struct NetControl {
    state: RwLock<ControlState>,
    routes: SharedRouteTable,
}
```

设计要求：

- 查询 API 返回只读快照，不暴露内部锁。
- 接口、DNS、路由更新需要事务化。
- 对外查询使用 `default_routes()`，不提供含糊的 `routes()` API。
- 控制面不直接收发 packet，也不推进 smoltcp poll。

### 2.4 路由模型

路由规则：

```rust
pub struct Rule {
    pub filter: IpCidr,
    pub via: Option<IpAddress>,
    pub dev: usize,
    pub interface_id: InterfaceId,
    pub src: IpAddress,
    pub metric: u32,
    pub order: u64,
}

pub struct RouteDecision {
    pub dev: usize,
    pub interface_id: InterfaceId,
    pub source: IpAddress,
    pub next_hop: IpAddress,
    pub metric: u32,
}
```

查找规则：

- 先按最长前缀匹配。
- 同前缀按 metric 从小到大。
- metric 相同按插入顺序稳定选择。
- 接口 down 时跳过该接口路由。
- 查不到路由返回错误，不 panic。
- `select_route_if()` 用于带接口可用性过滤的普通路由查询。
- `select_route_with_binding()` 用于 `SO_BINDTODEVICE` 或本地地址推导出的接口约束。
- `select_route_for_source(dst, source)` 用于 TX dispatch，保证 smoltcp 已选择的源地址和出接口一致。

调用方迁移：

- TCP `connect()` 使用路由选择本地地址和出接口。
- UDP `connect()` / `sendto()` 按目标地址和 `DeviceBinding` 查 route，不强制长期缓存完整 `RouteDecision`，避免 DHCP/route 更新后缓存失效。
- raw socket 查不到路由返回错误。
- Router TX dispatch 根据 IP 包的源地址和目标地址选择 TX queue。

### 2.5 设备队列与 smoltcp Device 适配

设备并行优化不能破坏 smoltcp `Device` token 模型。正确边界是：

```text
device worker
  -> RouterQueues

Router as smoltcp Device
  -> receive() consumes Router.rx_buffer
  -> transmit() returns TxToken
  -> TxToken::consume() writes Router.tx_buffer

Service::poll()
  -> Router::poll() drains worker RX queue into rx_buffer
  -> iface.poll(now, &mut router, &mut sockets)
  -> Router::dispatch() routes tx_buffer to loopback or per-device TX queue
```

建议结构：

```rust
struct Router {
    rx_buffer: PacketBuffer,
    tx_buffer: PacketBuffer,
    queues: Arc<RouterQueues>,
    devices: Vec<Arc<DeviceHandle>>,
    table: SharedRouteTable,
}

struct RouterQueues {
    rx: Arc<BoundedPacketQueue<RxPacket>>,
}

struct DeviceHandle {
    interface_id: InterfaceId,
    name: String,
    inner: Arc<Mutex<Box<dyn Device>>>,
    rx_queue: Arc<BoundedPacketQueue<RxPacket>>,
    tx_queue: Arc<BoundedPacketQueue<TxPacket>>,
    rx_wake: Arc<WaitQueue>,
    tx_wake: Arc<WaitQueue>,
}
```

设计要求：

- 所有真实设备共享一个 RX queue。
- 每个真实设备有独立 TX queue。
- 设备 worker 不访问 `Router` 本体。
- `Router::poll()` 从 RX queue drain 到 smoltcp `rx_buffer`，并保留 ingress `InterfaceId`。
- `Router::dispatch()` 从 smoltcp `tx_buffer` 取包，按 route 分发到 loopback 或 per-device TX queue。
- 不需要单独的 `RouteSnapshot`；控制面和 Router 可以共享同一个 `SharedRouteTable`。
- 不需要把所有 TX queue 放在 `RouterQueues.tx: Vec<_>` 中；TX queue 放进 `DeviceHandle` 更直接。

### 2.6 有界队列和 packet buffer

队列要求：

- RX/TX 队列必须有界。
- RX 满时丢包并记录 warning/统计。
- TX 满时按 socket/packet 语义返回 `WouldBlock`、重试或丢包。
- 不使用无界 `SegQueue` 作为网络热路径队列。
- 第一版可以使用 copy queue，但必须避免无界分配。

建议实现：

```rust
struct BoundedPacketQueue<T> {
    inner: Mutex<VecDeque<T>>,
    capacity: usize,
    len: AtomicUsize,
}

struct QueuedPacket {
    bytes: [u8; STANDARD_MTU],
    len: usize,
}
```

说明：

- inline `QueuedPacket` 可以避免每包 `Box<[u8]>` / `Vec::to_vec()` 分配。
- 不强制引入 `PacketBufPool`；除非后续需要更复杂的 buffer ownership。
- 端到端 zero-copy 不属于第一阶段目标，后续需要配合 `rd-net` buffer ownership 和 smoltcp token 适配。

### 2.7 设备 worker 与 poll 唤醒

设备 worker 不直接进入 smoltcp：

```text
RX IRQ / RX worker
  -> receive from device
  -> push RxPacket into bounded rx queue
  -> request_poll()

net-poll worker
  -> take Service lock
  -> take SocketSet lock
  -> Service::poll()

TX worker
  -> pop TxPacket from per-device tx queue
  -> send through device
```

要求：

- 每个非 loopback 设备可以启动一个 RX worker 和一个 TX worker。
- IRQ handler 只唤醒对应 RX worker 或设置 queue event，不做重工作。
- 无设备 IRQ 时，由平台轮询任务或 OOB RX poll task 唤醒设备。
- RX worker 不忙等。
- 设备 waker 唤醒对应 RX worker。
- socket waker 根据 `DeviceBinding` 只注册到允许的设备路径。

### 2.8 net-poll worker

`poll_interfaces()` 不再作为 socket 热路径同步入口。新的主路径是 `net-poll` worker：

```text
socket send/connect/drop
  -> update SocketSet state
  -> request_poll()

device RX
  -> enqueue RxPacket
  -> request_poll()

timer
  -> request_poll()

net-poll worker
  -> wait wake or deadline
  -> poll_until_idle()
```

建议 API：

```rust
pub fn poll_interfaces() {
    request_poll();
}

pub fn request_poll() {
    NET_POLL_REQUESTED.store(true, Ordering::Release);
    NET_POLL_WAKE.notify_one(true);
}
```

要求：

- `poll_interfaces()` 保留为 public trigger/debug API，但不再同步执行 `Service::poll()`。
- `net_poll_worker()` 独占调用 `poll_until_idle()`。
- `poll_until_idle()` 使用全局原子标志防重入。
- `poll_until_idle()` 在有工作时批量 poll，不在每次成功 poll 后主动 `yield_now()`。
- socket send/recv/connect/accept/drop 热路径只 `request_poll()`。

### 2.9 控制面状态与事务更新

DHCP ACK 等运行期更新不能暴露半更新状态。

建议更新对象：

```rust
struct NetworkStateUpdate {
    interface_id: InterfaceId,
    dev: usize,
    metric: u32,
    old_ipv4: Option<Ipv4Cidr>,
    ipv4: Option<Ipv4Cidr>,
    gateway: Option<Ipv4Address>,
    dns_source: DnsSource,
    dns_servers: Vec<Ipv4Address>,
}
```

提交规则：

- DHCP 解析在 `Service::poll()` 中完成。
- DHCP ACK/NAK 先生成 `NetworkStateUpdate`。
- smoltcp `Interface` 的 IP 地址、接口 registry、DNS entries、route table 必须作为一个逻辑事务更新。
- 对外查询 API 要么看到旧状态，要么看到新状态，不暴露半更新状态。
- 如果控制面更新逻辑变重，再进一步缩窄写锁范围。

### 2.10 Socket 绑定索引与快路径

目标：

- TCP/UDP bind 冲突检查不在热路径扫描整个 `SocketSet`。
- wildcard bind 与具体地址 bind 的冲突语义统一。
- 普通 socket 使用 `DeviceBinding { bound_if: Option<InterfaceId> }`，不再使用 `u32 device_mask`。

建议：

```rust
pub struct DeviceBinding {
    pub bound_if: Option<InterfaceId>,
}
```

语义：

- `None`：未绑定接口，按 route decision 或 wildcard 语义工作。
- `Some(id)`：由 `SO_BINDTODEVICE`、绑定具体本地地址或 AF_PACKET ifindex 得到。

要求：

- `SO_BINDTODEVICE` 存入通用 socket options。
- 绑定具体本地地址时，通过控制面反查接口并设置 `DeviceBinding`。
- TCP 使用独立 bind/listen 侧表维护 wildcard/specific 地址冲突。
- UDP 使用 `SocketSetWrapper` 内部 side table 维护 bind 冲突。
- `ListenTable` 支持 per-address listen 和 accept waker。
- 不承诺完整 `reuseport`；当前重点是 `SO_REUSEADDR` 和 bind/listen 冲突语义。
- 不必新增统一 `SocketRegistry` 类型，避免重复维护 TCP/UDP 生命周期。

### 2.11 DNS 与 DHCP

DHCP：

- 每个 Ethernet 接口独立 `DhcpState`。
- DHCP packet 根据 ingress `InterfaceId` 分发。
- DHCP ACK 生成 `NetworkStateUpdate`。
- DHCP NAK 或失败只影响对应接口。
- DHCP bootstrap 不应要求所有 DHCP 接口成功，避免断开的网卡阻塞系统启动。

DNS：

- DHCP DNS、接口级静态 DNS、全局 fallback DNS 都记录为 `DnsServerEntry`。
- `DnsServerEntry` 保留 server、interface_id、metric 和 source。
- `dns_servers()` 返回按 metric 排序、去重后的地址列表。
- `dns_query_timeout()` 选择 DNS server 前检查可路由性，不可路由则尝试下一个。
- 不实现 DNS cache、split DNS、`/etc/resolv.conf`。

### 2.12 广播、组播与 loopback

广播：

- limited broadcast 可发送到所有非 loopback Ethernet 设备。
- 绑定具体接口时，应尽量限制到该接口。
- 子网广播按接口 IPv4/prefix 选择接口，后续补齐。

组播：

- IPv4 multicast 至少按绑定接口或 route decision 选择出接口。
- 未绑定接口的 multicast 使用默认 route 或接口集合策略。
- IGMP/MLD membership 不在本轮范围。

Loopback：

- `LoopbackDevice` 只作为 `lo` 接口占位。
- loopback 不启动 RX/TX worker。
- 普通 smoltcp TX 选中 loopback 时，`Router::dispatch()` 应直接注入 `Router.rx_buffer`，不走共享 RX queue 和设备 worker。
- loopback 注入前应执行 TCP SYN snoop，保证回环 listen/accept 能在同一 poll 周期推进。

### 2.13 对外 API

正式 API：

```rust
pub fn init_network(net_devs: EthernetDeviceList, config: NetworkConfig);
pub fn poll_interfaces();
pub fn request_poll();

pub fn interfaces() -> Vec<InterfaceInfo>;
pub fn interface_by_name(name: &str) -> Option<InterfaceInfo>;
pub fn interface_by_id(id: InterfaceId) -> Option<InterfaceInfo>;
pub fn ipv4_config(name: &str) -> Option<Ipv4InterfaceConfig>;
pub fn default_routes() -> Vec<RouteInfo>;
pub fn arp_entries() -> Vec<ArpEntry>;

pub fn dns_query(name: &str) -> AxResult<Vec<IpAddr>>;
pub fn dns_query_timeout(name: &str, timeout: Duration) -> AxResult<Vec<IpAddr>>;
pub fn dns_servers() -> Vec<Ipv4Address>;
```

说明：

- `poll_interfaces()` 是轻量触发入口，不作为 socket 热路径同步 poll。
- `eth0_ipv4_config()` 这类 convenience helper 可以保留，但新代码应优先使用 `ipv4_config(name)` 和接口 registry API。
- 不提供 `u32 device_mask` 兼容转换。

### 2.14 StarryOS、ArceOS 与 Axvisor 接入

StarryOS 网络 ABI 层必须从 `lo + eth0` 固定模型迁移到 `ax-net` 接口 registry。

涉及方向：

- `SIOCGIFCONF` 遍历 `ax_net::interfaces()`。
- `SIOCGIFADDR`、`SIOCGIFBRDADDR`、`SIOCGIFNETMASK` 按接口名查询。
- `SIOCGIFHWADDR` 返回真实 MAC 或 loopback 类型。
- `SIOCGIFINDEX` 返回 `InterfaceInfo::id.to_linux_ifindex()`。
- AF_PACKET bind 使用 ifindex 映射 `InterfaceId`。
- `/proc/net/arp` 使用 `ax_net::arp_entries()`，device 字段必须是真实接口名。
- `SO_BINDTODEVICE` 映射到 `DeviceBinding { bound_if: Some(id) }`。
- StarryOS 不缓存第二套 IPv4/gateway/MAC 状态。

ArceOS：

- `ax-runtime` 构造结构化 `NetworkConfig` 并调用 `ax_net::init_network()`。
- `ax-api`、`ax-posix-api` 使用统一 socket API。
- 面向应用的 API 不增加 `eth0` 假设。

Axvisor：

- 复用 `ax-net` 接口 registry、route decision 和 socket API。
- 管理面、VM 服务面通过接口级配置表达网络意图。

## 3. 测试方案

单元测试：

- 多接口 route lookup。
- 默认路由 metric 选择。
- route 查不到返回错误，不 panic。
- DHCP packet 按 `InterfaceId` 分发。
- `NetworkStateUpdate` commit 前后状态一致。
- bounded RX/TX queue 满时行为正确。
- `Router` RX metadata 保留 ingress `InterfaceId`。
- `DeviceBinding` route/waker 过滤语义正确。
- TCP/UDP socket 绑定索引冲突检查正确。
- loopback direct injection 能在同一 poll 周期推进 TCP SYN。

集成测试：

- QEMU 单网卡 DHCP。
- QEMU 多网卡静态配置。
- TCP/UDP bind 到 `eth1` 地址后 route decision 选择 `eth1`。
- StarryOS `SIOCGIFCONF` 能看到多个接口。
- StarryOS `SIOCGIFADDR`、`SIOCGIFHWADDR`、`SIOCGIFINDEX` 可按接口名查询。
- StarryOS AF_PACKET bind 到指定 ifindex 后返回正确 `sockaddr_ll`。
- `/proc/net/arp` device 字段正确。
- DNS server 来自 DHCP/静态/fallback 时查询路径不退化。

性能验证：

- 对比同步 `poll_interfaces()` 与 `request_poll()` 的 socket 热路径耗时。
- 多 NIC 同时 RX 时验证 bounded queue、公平性和丢包行为。
- 长时间 UDP/TCP 压测验证队列不会无界增长。
- 验证 route/interface/DNS 查询不会长时间阻塞 `net-poll` worker。

## 4. 遗留问题

- smoltcp 协议栈处理仍然是单实例串行。设备队列解耦不能让 TCP/UDP 协议处理本身多核并行。
- Zero-copy RX/TX 需要继续改造 `rd-net` buffer ownership、packet pool 和 smoltcp token 适配。
- 完整 DHCP lease renew/rebind、过期回收、地址冲突检测仍需后续补齐。
- 完整 IPv6、邻居发现、IPv6 route、AAAA DNS 查询不在本轮范围。
- IGMP/MLD、按接口 multicast membership 不在本轮范围。
- split DNS、`/etc/resolv.conf`、net namespace resolver 策略不在本轮范围。
- 动态 link down/up、接口热插拔、队列重建和 socket 错误传播仍需后续完善。
- RSS、多队列网卡、per-queue poll、NAPI 类批量调度属于后续更底层 dataplane 优化。
