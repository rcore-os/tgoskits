---
sidebar_position: 4
sidebar_label: "API 参考"
---

# 网络栈 Public API

本文汇总 `ax-net` 的正式 public API，并说明哪些 API 属于系统初始化、接口查询、socket、设备适配和 DNS。

## 初始化 API

定义在 [net/ax-net/src/lib.rs](net/ax-net/src/lib.rs)：

```rust
pub fn init_network(net_devs: EthernetDeviceList, config: NetworkConfig);  // L125
pub fn poll_interfaces();                                                   // L401
pub fn request_poll();                                                      // L408

#[cfg(feature = "vsock")]
pub fn init_vsock(vsock_devs: VsockDeviceList);                             // L360
```

说明：

- `init_network()` 由 `ax-runtime` 调用，只能调用一次（重复调用 panic，见 [lib.rs#L130-L133](net/ax-net/src/lib.rs#L130-L133)）。
- `request_poll()` 是轻量 poll 请求入口，socket 和设备路径应调用它。
- `poll_interfaces()` 保留为 public trigger/debug 入口，内部仅转调 `request_poll()`（[lib.rs#L401-L403](net/ax-net/src/lib.rs#L401-L403)）。
- `init_vsock()` 仅在 `vsock` feature 下存在。

## 接口查询 API

```rust
pub fn interfaces() -> Vec<InterfaceInfo>;            // L417
pub fn interface_by_name(name: &str) -> Option<InterfaceInfo>;  // L421
pub fn interface_by_id(id: InterfaceId) -> Option<InterfaceInfo>;  // L425
pub fn ipv4_config(name: &str) -> Option<Ipv4InterfaceConfig>;  // L429
pub fn default_routes() -> Vec<RouteInfo>;            // L433
pub fn arp_entries() -> Vec<ArpEntry>;                // L413
```

这些 API 返回只读快照。调用方不应保存快照后假设其永久有效；DHCP 更新或后续接口状态变化可能改变地址、路由和 DNS。

## 接口类型

`InterfaceId` 与 `InterfaceFlags` 定义在 [config.rs#L8-L55](net/ax-net/src/config.rs#L8-L55)：

```rust
pub struct InterfaceId(u32);

pub enum InterfaceKind {
    Loopback,
    Ethernet,
}

bitflags::bitflags! {
    pub struct InterfaceFlags: u32 {
        const UP = 1 << 0;
        const RUNNING = 1 << 1;
        const LOOPBACK = 1 << 2;
        const BROADCAST = 1 << 3;
        const MULTICAST = 1 << 4;
    }
}
```

`InterfaceInfo` 见 [config.rs#L58-L70](net/ax-net/src/config.rs#L58-L70)。`InterfaceId` 同时作为 StarryOS Linux ifindex 数值来源：

```rust
let linux_ifindex = info.id.to_linux_ifindex();
let id = InterfaceId::from_linux_ifindex(linux_ifindex).unwrap();
```

## Socket API

主要 socket 类型（见 [lib.rs#L38-L46](net/ax-net/src/lib.rs#L38-L46)）：

```rust
pub mod tcp;
pub mod udp;
pub mod raw;
pub mod unix;

#[cfg(feature = "vsock")]
pub mod vsock;
```

统一 socket trait `SocketOps`（[net/ax-net/src/socket.rs#L177-L202](net/ax-net/src/socket.rs#L177-L202)）：

```rust
pub trait SocketOps: Configurable {
    fn bind(&self, local_addr: SocketAddrEx) -> AxResult;
    fn connect(&self, remote_addr: SocketAddrEx) -> AxResult;
    fn listen(&self, _backlog: usize) -> AxResult { Err(AxError::OperationNotSupported) }
    fn accept(&self) -> AxResult<Socket> { Err(AxError::OperationNotSupported) }
    fn send(&self, src: impl Read + IoBuf, options: SendOptions) -> AxResult<usize>;
    fn recv(&self, dst: impl Write + IoBufMut, options: RecvOptions<'_>) -> AxResult<usize>;
    fn recv_available(&self) -> AxResult<usize> { Err(AxError::OperationNotSupported) }
    fn local_addr(&self) -> AxResult<SocketAddrEx>;
    fn peer_addr(&self) -> AxResult<SocketAddrEx>;
    fn shutdown(&self, how: Shutdown) -> AxResult;
}
```

`Socket` 枚举（[socket.rs#L253-L265](net/ax-net/src/socket.rs#L253-L265)）统一各 backend，并对 `SocketOps` / `Configurable` 做透传分派：

```rust
pub enum Socket {
    Udp(Box<UdpSocket>),
    Tcp(Box<TcpSocket>),
    Raw(Box<RawSocket>),
    Unix(Box<UnixSocket>),
    #[cfg(feature = "vsock")]
    Vsock(Box<VsockSocket>),
}
```

`SocketAddrEx`（[socket.rs#L26-L35](net/ax-net/src/socket.rs#L26-L35)）支持：

- IP socket address
- Unix domain socket address
- vsock address（`vsock` feature）

```rust
pub enum SocketAddrEx {
    Ip(SocketAddr),
    Unix(UnixSocketAddr),
    #[cfg(feature = "vsock")]
    Vsock(VsockAddr),
}
```

## Socket options

[options.rs](net/ax-net/src/options.rs) 通过 `define_options!` 宏（[options.rs#L148](net/ax-net/src/options.rs#L148)）一次性生成 `GetSocketOption<'a>` 和 `SetSocketOption<'a>` 两个枚举，覆盖 SO_\* / TCP_\* / IP_\* 三层选项：

```rust
define_options! {
    // ---- Socket level options (SO_*) ----
    ReuseAddress(bool),
    Error(i32),
    DontRoute(bool),
    SendBuffer(usize),
    ReceiveBuffer(usize),
    KeepAlive(bool),
    SendTimeout(Duration),
    ReceiveTimeout(Duration),
    SendBufferForce(usize),
    PassCredentials(bool),
    PeerCredentials(UnixCredentials),
    SocketType(i32),
    SocketProtocol(i32),
    SocketDomain(i32),
    BindToDevice(Option<InterfaceId>),

    // --- TCP level options (TCP_*) ----
    NoDelay(bool),
    MaxSegment(usize),
    TcpKeepIdle(u32),
    TcpKeepInterval(u32),
    TcpKeepCount(u32),
    TcpUserTimeout(u32),
    TcpInfo(TcpInfo),

    // ---- IP level options (IP_*) ----
    Ttl(u8),
    RecvErr(bool),

    // ---- Extra options ----
    NonBlocking(bool),
}
```

`Configurable` trait（[options.rs#L185-L208](net/ax-net/src/options.rs#L185-L208)）提供 `get_option` / `set_option`，未支持的选项返回 `ENOPROTOOPT`：

```rust
#[enum_dispatch]
pub trait Configurable {
    fn get_option_inner(&self, opt: &mut GetSocketOption) -> AxResult<bool>;
    fn set_option_inner(&self, opt: SetSocketOption) -> AxResult<bool>;
    fn get_option(&self, mut opt: GetSocketOption) -> AxResult { /* ... */ }
    fn set_option(&self, opt: SetSocketOption) -> AxResult { /* ... */ }
}
```

通用选项（非阻塞、超时、`SO_BINDTODEVICE` 等）由 `GeneralOptions`（[general.rs#L18-L31](net/ax-net/src/general.rs#L18-L31)）实现，各 socket backend 的 `get_option_inner` / `set_option_inner` 先尝试 `GeneralOptions`，再处理自身特有选项。`TcpInfo`（[options.rs#L58-L101](net/ax-net/src/options.rs#L58-L101)）提供 transport-independent 的 TCP_INFO 快照。

`SO_BINDTODEVICE` 在 `ax-net` 内表达为 `DeviceBinding`（[config.rs#L142-L146](net/ax-net/src/config.rs#L142-L146)）：

```rust
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct DeviceBinding {
    pub bound_if: Option<InterfaceId>,
}
```

语义：

- `None`：未绑定接口，按 route decision 选择出接口。
- `Some(id)`：只允许对应接口参与 route/waker/device 选择。

## DNS API

```rust
pub fn dns_servers() -> Vec<Ipv4Address>;                                    // L490
pub fn dns_query(name: &str) -> AxResult<Vec<IpAddr>>;                       // L496
pub fn dns_query_timeout(name: &str, timeout: Duration) -> AxResult<Vec<IpAddr>>;  // L500
```

说明：

- `dns_servers()` 返回按 (metric, interface_id, server_ip) 排序去重的 DNS server 列表。来源包括 DHCP、静态配置和 fallback。
- `dns_query()` 使用 5 秒默认超时（`DNS_DEFAULT_TIMEOUT`）。内部过滤不可路由的 DNS server，创建临时 `dns::Socket`，循环 poll 直到解析完成或超时。
- 查询结束后 `DnsSocketGuard` 自动从 `SOCKET_SET` 移除临时 socket。

## ARP API

```rust
pub fn arp_entries() -> Vec<ArpEntry>;                                       // L413
```

返回所有 Ethernet 设备上已解析的 ARP 条目快照（[ArpEntry](net/ax-net/src/device/mod.rs#L24-L31)）：

```rust
pub struct ArpEntry {
    pub ip_addr: [u8; 4],
    pub hw_type: u16,
    pub flags: u16,
    pub hw_addr: [u8; 6],
    pub device: String,        // 真实接口名
}
```

## 设备 Driver API

### EthernetDriver（能力边界 trait）

定义在 [device/driver.rs#L70-L82](net/ax-net/src/device/driver.rs#L70-L82)：

```rust
pub trait EthernetDriver: Send + Sync {
    fn device_name(&self) -> &str;
    fn irq_num(&self) -> Option<usize>;
    fn enable_irq(&mut self);
    fn disable_irq(&mut self);
    fn mac_address(&self) -> [u8; 6];
    fn alloc_tx_buffer(&mut self, size: usize) -> NetDeviceResult<Box<dyn NetTxBuffer>>;
    fn recycle_tx_buffers(&mut self) -> NetDeviceResult;
    fn transmit(&mut self, tx_buf: &mut dyn NetTxBuffer) -> NetDeviceResult;
    fn receive(&mut self) -> NetDeviceResult<Box<dyn NetRxBuffer>>;
    fn recycle_rx_buffer(&mut self, rx_buf: &mut dyn NetRxBuffer) -> NetDeviceResult;
    fn handle_irq(&mut self) -> NetIrqEvents;
}
```

`RdNetDriver` 是该 trait 的标准实现（基于 `rd-net`），`EthernetDeviceList` 即 `Vec<Box<dyn EthernetDriver>>`。

### IRQ 注册

```rust
pub fn set_ethernet_irq_registrar(registrar: &'static dyn EthernetIrqRegistrar);

pub trait EthernetIrqRegistrar: Send + Sync {
    fn register_shared(
        &self,
        name: &str,
        irq: usize,
        action: EthernetIrqAction,
    ) -> Result<Box<dyn EthernetIrqRegistration>, EthernetIrqRegistrationError>;
}
```

`EthernetIrqAction` 封装 `(data: NonNull<()>, handler: unsafe fn(NonNull<()>) -> EthernetIrqOutcome)` 对，由 `ax-runtime` 注册。IRQ 触发时 `handle_ethernet_irq()` → `driver.handle_irq()` → 返回 `RX_READY` 时 wake RX worker。

## Vsock API（`vsock` feature）

```rust
#[cfg(feature = "vsock")]
pub fn init_vsock(vsock_devs: VsockDeviceList);                              // L360

// types:
pub struct VsockAddr;
pub struct VsockConnId;
pub type VsockDevice = Box<dyn VsockDriver>;
pub type VsockDeviceList = Vec<VsockDevice>;
```

## bind_device 方法

每个 socket 类型提供独立的接口绑定方法（不通过 socket option）：

```rust
impl TcpSocket {
    pub fn bind_device(&self, interface_id: InterfaceId) -> AxResult;
}
impl UdpSocket {
    pub fn bind_device(&self, interface_id: InterfaceId) -> AxResult;
}
impl RawSocket {
    pub fn bind_device(&self, interface_id: InterfaceId) -> AxResult;
}
```

校验现有接口，不存在返回 `AxError::NoSuchDevice`。内部设置 `DeviceBinding { bound_if: Some(id) }`。

## Socket 构造 API

```rust
impl TcpSocket {
    pub fn new() -> Self;                          // 创建 Idle 状态 socket
    fn new_connected(handle, local_ep, remote_ep) -> Self;  // accept 内部使用
}

impl UdpSocket {
    pub fn new() -> Self;
}

impl RawSocket {
    pub fn new(ip_version: IpVersion, ip_protocol: IpProtocol) -> Self;
}

impl UnixSocket {
    pub fn new(transport: impl Into<Transport>) -> Self;
    // 其中 Transport::Stream(StreamTransport) 或 Transport::Dgram(DgramTransport)
}

impl VsockSocket {
    pub fn new(transport: impl Into<VsockTransport>) -> Self;
    // VsockTransport::Stream(VsockStreamTransport)
}
```

Unix stream transport 通过 `StreamTransport::new_pair(pid)` 创建 socketpair，Unix datagram transport 通过 `DgramTransport::new_pair(pid)` 创建 pair。

## Ephemeral Port 分配

未在 public API 中暴露，但 TCP bind 和 UDP bind 在 `port == 0` 时调用内部函数分配临时端口（从 `49152` 开始，即 IANA 动态端口范围的下界）。

## Unix Namespace API

```rust
pub fn register_unix_namespace(ns: Arc<dyn UnixNamespace>);
```

StarryOS 通过此 API 注入文件系统 namespace（路径解析和 inode 管理）。`UnixNamespace` trait 定义在 [unix/namespace.rs](net/ax-net/src/unix/namespace.rs)。

## TCP / UDP / raw 接口绑定

TCP、UDP 和 raw socket 提供 `bind_device(InterfaceId)`，例如 `TcpSocket::bind_device()`（[tcp.rs#L94-L102](net/ax-net/src/tcp.rs#L94-L102)）：

```rust
pub fn bind_device(&self, interface_id: InterfaceId) -> AxResult {
    if interface_by_id(interface_id).is_none() {
        return Err(AxError::NoSuchDevice);
    }
    self.general.set_device_binding(DeviceBinding {
        bound_if: Some(interface_id),
    });
    Ok(())
}
```

```rust
let eth1 = ax_net::interface_by_name("eth1").ok_or(AxError::NoSuchDevice)?;

let udp = ax_net::udp::UdpSocket::new();
udp.bind_device(eth1.id)?;
```

绑定具体本地地址时，`ax-net` 会根据地址所属接口设置 `DeviceBinding`。例如 `UdpSocket::bind()` 调用 `get_control().local_binding_for(&endpoint)?`（[udp.rs](net/ax-net/src/udp.rs)）从监听地址反查接口。未绑定接口的 connect/sendto 会按 route decision 自动选择源地址和出接口。

## 动态设备注册与 OOB RX

用于运行时添加 Ethernet 设备（如 Wi-Fi AP 模式启动后），以及 SDIO Wi-Fi 等 out-of-band RX 设备：

```rust
pub struct NetConfig {
    pub name: String,
    pub ip: [u8; 4],
    pub prefix_len: u8,
    pub dhcp_server_client_ip: Option<[u8; 4]>,
    pub dedicated_poll: bool,    // true = 使用 notify_oob_rx() 驱动 RX
}

pub fn register_device_with_config(dev: Box<dyn EthernetDriver>, config: NetConfig);
pub fn notify_oob_rx();          // SDIO Wi-Fi 收到数据后调用
pub fn eth0_ipv4_config() -> Option<Ipv4InterfaceConfig>;
```

`register_device_with_config()` 在 `Service` 中调用 `register_static_device()` 添加设备和路由，可选启用 DHCP server（`enable_dhcp_server()`）。`dedicated_poll = true` 时设备用 `EthernetDevice::new_oob_rx()` 创建，RX 就绪由驱动线程调用 `notify_oob_rx()` 唤醒独立 poll task（`{ifname}-oob-poll`），不经过 Ethernet IRQ 框架。

## Per-address Listen 支持

`ListenTable` 和 `SocketSetWrapper` 支持 Linux 语义的同端口不同地址 listen/bind：

- `LISTEN_TABLE.listen(endpoint, backlog)`：`ListenTableEntry` 是 `Vec<ListenTableEntryInner>`，每个 entry 存储完整的 `IpListenEndpoint`。
- `listen_addrs_conflict(a, b)`：wildcard（`None`）与所有地址冲突；两个 `Some(addr)` 仅当相等时冲突。
- `SocketSetWrapper::udp_bind_available()`：UDP 同端口不同地址允许共存，wildcard 与所有冲突。

## TCP listen/accept

TCP 的 listen/accept 由全局 `LISTEN_TABLE`（[lib.rs#L89](net/ax-net/src/lib.rs#L89)）管理。`ListenTable`（[listen_table.rs#L67-L103](net/ax-net/src/listen_table.rs#L67-L103)）为每个端口维护一个 SYN 队列。RX 路径中 `snoop_tcp_packet()`（[router.rs#L622](net/ax-net/src/router.rs#L622)）在收到首个 SYN 时预创建 smoltcp socket 并入队（[listen_table.rs#L165-L200](net/ax-net/src/listen_table.rs#L165-L200)），从而在 smoltcp `Interface::poll` 之前就让连接进入 accepted 候选状态。`accept()` 扫描 SYN 队列返回已建立的连接（[listen_table.rs#L133-L164](net/ax-net/src/listen_table.rs#L133-L164)）。

## DNS API

```rust
pub fn dns_servers() -> Vec<Ipv4Address>;                       // lib.rs L480
pub fn dns_query(name: &str) -> AxResult<Vec<IpAddr>>;          // lib.rs L486
pub fn dns_query_timeout(name: &str, timeout: Duration) -> AxResult<Vec<IpAddr>>;  // lib.rs L494
```

`dns_servers()` 只返回地址列表。内部仍保留 DNS 来源接口、metric 和来源类型，用于 DHCP 更新和接口状态变化。

## 设备适配 API

`ax-net` 对外导出 Ethernet 设备适配类型（[lib.rs#L67-L84](net/ax-net/src/lib.rs#L67-L84)）：

```rust
pub struct EthernetDeviceList;   // = Vec<Box<dyn EthernetDriver>>  driver.rs L84
pub trait EthernetDriver;        // driver.rs L70
pub struct RdNetDriver;          // driver.rs L128
```

IRQ adapter 类型（[device/ethernet.rs#L27-L81](net/ax-net/src/device/ethernet.rs#L27-L81)）让 `ax-runtime` 把 HAL IRQ registration 适配到网络领域，而不把 `ax-hal::irq` ABI 泄漏到 `ax-net` public trait：

```rust
pub struct EthernetIrqAction { /* data + handler fn pointer */ }
pub enum EthernetIrqOutcome { Handled, Wake }
pub trait EthernetIrqRegistrar: Send + Sync {
    fn register_shared(&self, name: &str, irq: usize, action: EthernetIrqAction)
        -> Result<Box<dyn EthernetIrqRegistration>, EthernetIrqRegistrationError>;
}
pub enum EthernetIrqRegistrationError { InvalidIrq, Busy, Unsupported, Other }
pub fn set_ethernet_irq_registrar(registrar: &'static dyn EthernetIrqRegistrar);
```

`EthernetDevice::new()`（[ethernet.rs#L136-L174](net/ax-net/src/device/ethernet.rs#L136-L174)）在创建时若 registrar 已安装且设备有 IRQ，会通过 `register_shared()` 注册共享中断处理函数 `handle_ethernet_irq`（[ethernet.rs#L116-L123](net/ax-net/src/device/ethernet.rs#L116-L123)），收到 RX/TX 事件时 `wake()` RX worker 与 `net-poll`。

## 已删除或不应使用的入口

以下旧入口不应再使用：

- `eth0_ipv4_config()`
- 旧全局单网口 env 配置语义
- `u32 device_mask`
- 旧 benchmark public API

所有调用方应迁移到接口 registry API 和 `DeviceBinding` / `InterfaceId`。
