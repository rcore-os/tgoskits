---
sidebar_position: 4
sidebar_label: "Socket 系统"
---

# Socket 系统

`ax-net` 的 socket 抽象层向上暴露统一 API，向下对接 smoltcp（IP 类 socket）或自包含传输（Unix/vsock）。本文从源码层描述 backend 分层、端口仲裁、状态机和通用 poll 设施。

## Backend 分层

`Socket` 枚举（[socket.rs](net/ax-net/src/socket.rs)）聚合 5 类 backend，统一实现 `SocketOps` + `Configurable`：

```rust
// socket.rs
pub enum Socket {
    Tcp(Box<TcpSocket>),
    Udp(Box<UdpSocket>),
    Raw(Box<RawSocket>),
    Unix(Box<UnixSocket>),
    #[cfg(feature = "vsock")]
    Vsock(Box<VsockSocket>),
}
```

| Backend | 类型 | 协议核心 | 源码 |
| --- | --- | --- | --- |
| `TcpSocket` | SOCK_STREAM / AF_INET | smoltcp TCP socket | [tcp.rs](net/ax-net/src/tcp.rs) |
| `UdpSocket` | SOCK_DGRAM / AF_INET | smoltcp UDP socket | [udp.rs](net/ax-net/src/udp.rs) |
| `RawSocket` | SOCK_RAW / AF_INET | smoltcp raw socket + ICMP loopback | [raw.rs](net/ax-net/src/raw.rs) |
| `UnixSocket` | SOCK_STREAM/SOCK_DGRAM / AF_UNIX | 自包含 `Transport`（不经 smoltcp） | [unix/mod.rs](net/ax-net/src/unix/mod.rs) |
| `VsockSocket` | SOCK_STREAM / AF_VSOCK | `rdif-vsock` 驱动 + ring buffer | [vsock/mod.rs](net/ax-net/src/vsock/mod.rs) |

IP 类 socket（TCP/UDP/Raw/DNS）通过 `SOCKET_SET.add()` 注册到全局 `SocketSet`，获得 `SocketHandle`。Unix 和 vsock 不依赖 smoltcp，各自维护连接状态。

`SocketAddrEx` 统一地址类型：

```rust
// socket.rs
pub enum SocketAddrEx {
    Ip(SocketAddr),
    Unix(UnixSocketAddr),
    #[cfg(feature = "vsock")]
    Vsock(VsockAddr),
}
```

`SocketOps` trait 是所有 backend 的统一接口：

```rust
// socket.rs
pub trait SocketOps: Configurable {
    fn bind(&self, local_addr: SocketAddrEx) -> AxResult;
    fn connect(&self, remote_addr: SocketAddrEx) -> AxResult;
    fn listen(&self, _backlog: usize) -> AxResult { Err(AxError::OperationNotSupported) }
    fn accept(&self) -> AxResult<Socket> { Err(AxError::OperationNotSupported) }
    fn send(&self, src: impl Read + IoBuf, options: SendOptions) -> AxResult<usize>;
    fn recv(&self, dst: impl Write + IoBufMut, options: RecvOptions<'_>) -> AxResult<usize>;
    fn local_addr(&self) -> AxResult<SocketAddrEx>;
    fn peer_addr(&self) -> AxResult<SocketAddrEx>;
    fn shutdown(&self, how: Shutdown) -> AxResult;
}
```

TCP/UDP 实现完整接口；Raw socket 不实现 `listen`/`accept`；Unix/vsock 有各自独立的 transport trait。

## SocketSetWrapper

全局 socket 容器（[wrapper.rs](net/ax-net/src/wrapper.rs)）在 smoltcp `SocketSet` 之上增加 UDP 端口冲突仲裁：

```rust
// wrapper.rs
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct UdpBindKey {
    addr: Option<IpAddress>,
    port: u16,
}

pub(crate) struct SocketSetWrapper<'a> {
    pub inner: Mutex<SocketSet<'a>>,
    udp_binds: Mutex<HashMap<UdpBindKey, SocketHandle>>,
}
```

`add()` 和 `with_socket_mut()` 是对 smoltcp API 的薄封装；`remove()` 额外调用 `udp_unbind()`。

### UDP 端口仲裁

`udp_bind_available()` 实现 Linux `SO_REUSEADDR` 语义：

```rust
// wrapper.rs
fn udp_bind_available(binds: &HashMap<UdpBindKey, SocketHandle>, key: UdpBindKey) -> bool {
    let wildcard = UdpBindKey { addr: None, port: key.port };
    // 精确 bind：同地址同端口冲突 || wildcard 已占用冲突
    if binds.contains_key(&key) || (key.addr.is_some() && binds.contains_key(&wildcard)) {
        return false;
    }
    // Wildcard bind：任何地址已占用同一端口则冲突
    key.addr.is_some() || !binds.keys().any(|bind| bind.port == key.port)
}
```

| bind 类型 | 示例 | 拒绝条件 |
| --- | --- | --- |
| 精确 bind | `192.168.1.1:53` | 同地址同端口已存在，或 wildcard `0.0.0.0:53` 已存在 |
| Wildcard bind | `0.0.0.0:53` | 任何地址已占用同一端口 |
| `SO_REUSEADDR` | — | 跳过所有仲裁 |

`udp_port_available()` 暴露给 ephemeral port 分配器使用。

## TCP 端口仲裁

除了 `ListenTable`（管理 `listen()` 端口），`ax-net` 还有独立的 `TCP_BOUND_PORTS`（[tcp.rs](net/ax-net/src/tcp.rs)）追踪已 bind 但尚未 listen 的端口：

```rust
// tcp.rs
static TCP_BOUND_PORTS: LazyLock<Mutex<HashMap<u16, Vec<Option<IpAddress>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn register_tcp_bound(endpoint: IpListenEndpoint) -> AxResult {
    if endpoint.port == 0 { return Ok(()); }
    let mut bound_ports = TCP_BOUND_PORTS.lock();
    let bound_addrs = bound_ports.entry(endpoint.port).or_default();
    if bound_addrs.iter().any(|&addr| listen_addrs_conflict(addr, endpoint.addr)) {
        return Err(AxError::AddrInUse);
    }
    bound_addrs.push(endpoint.addr);
    Ok(())
}

fn unregister_tcp_bound(endpoint: IpListenEndpoint) {
    if endpoint.port != 0 {
        let mut bound_ports = TCP_BOUND_PORTS.lock();
        if let Some(bound_addrs) = bound_ports.get_mut(&endpoint.port) {
            if let Some(idx) = bound_addrs.iter().position(|&addr| addr == endpoint.addr) {
                bound_addrs.swap_remove(idx);
            }
            if bound_addrs.is_empty() { bound_ports.remove(&endpoint.port); }
        }
    }
}
```

地址冲突判定：

```rust
fn listen_addrs_conflict(a: Option<IpAddress>, b: Option<IpAddress>) -> bool {
    a.is_none() || b.is_none() || a == b
}
```

即 wildcard 与所有地址冲突，两个具体地址仅相等时冲突。这与 `ListenTable` 的同端口多地址 listen 语义一致。

`tcp_port_available()` 同时检查两表：

```rust
fn tcp_port_available(port: u16) -> bool {
    LISTEN_TABLE.can_listen(IpListenEndpoint { addr: None, port })
        && !TCP_BOUND_PORTS.lock().contains_key(&port)
}
```

## Socket 状态机

`StateLock`（[state.rs](net/ax-net/src/state.rs)）提供基于 CAS 的轻量状态转换锁，所有 socket 类型共享：

```rust
// state.rs
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State { Idle = 0, Busy = 1, Connecting = 2,
                         Connected = 3, Listening = 4, Closed = 5 }

pub struct StateLock(AtomicU8);

impl StateLock {
    pub fn get(&self) -> State {
        self.0.load(Ordering::Acquire).try_into().expect("invalid state")
    }

    pub fn lock(&self, expect: State) -> Result<StateGuard<'_>, State> {
        match self.0.compare_exchange(expect as u8, State::Busy as u8,
                                       Ordering::Acquire, Ordering::Acquire) {
            Ok(_) => Ok(StateGuard(self, expect as u8)),
            Err(old) => Err(old.try_into().expect("invalid state")),
        }
    }
}
```

状态转换图：

```
Idle ──bind──→ Idle (不变，但注册 endpoint)
Idle ──listen──→ Listening
Idle ──connect──→ Connecting ──SYN/SYN+ACK──→ Connected
Listening ──accept──→ Listening (不变，但生成新 socket)
Connected ──close──→ Closed
```

`StateGuard::transit()` 在执行操作时临时置为 `Busy`，成功提交新状态，失败回退：

```rust
// state.rs
#[must_use]
pub struct StateGuard<'a>(&'a StateLock, u8);

impl StateGuard<'_> {
    pub fn transit<R>(self, new: State, f: impl FnOnce() -> AxResult<R>) -> AxResult<R> {
        match f() {
            Ok(result) => { self.0.0.store(new as u8, Ordering::Release); Ok(result) }
            Err(err) => { self.0.0.store(self.1, Ordering::Release); Err(err) }
        }
    }
}
```

并发 `bind`/`connect`/`listen` 在同一 socket 上只有第一个成功——后续操作在 `lock(expect)` 时 CAS 失败。

## ListenTable

`ListenTable`（[listen_table.rs](net/ax-net/src/listen_table.rs)）是 TCP 监听的核心数据结构。65536 个端口 bucket，每端口一个 `Arc<Mutex<Vec<ListenTableEntryInner>>>`，支持同端口不同地址 listen：

```rust
// listen_table.rs
struct ListenTableEntryInner {
    listen_endpoint: IpListenEndpoint,
    backlog: usize,                      // clamp 到 LISTEN_QUEUE_SIZE=512
    syn_queue: VecDeque<PendingTcp>,
    accept_poll: Arc<PollSet>,
}

pub struct ListenTable {
    tcp: Box<[ListenTableEntry]>,        // [Arc<Mutex<Vec<Inner>>>; 65536]
}
```

### listen/unlisten

```rust
pub fn can_listen(&self, endpoint: IpListenEndpoint) -> bool {
    self.tcp[endpoint.port as usize].lock().iter()
        .all(|entry| !listen_addrs_conflict(entry.listen_endpoint.addr, endpoint.addr))
}

pub fn listen(&self, endpoint: IpListenEndpoint, backlog: usize) -> AxResult {
    let mut entries = self.tcp[endpoint.port as usize].lock();
    if entries.iter().any(|e| listen_addrs_conflict(e.listen_endpoint.addr, endpoint.addr)) {
        return Err(AxError::AddrInUse);
    }
    entries.push(ListenTableEntryInner::new(endpoint, backlog));
    Ok(())
}
```

### SYN 预创建

`snoop_tcp_packet()` 在 `Router::poll()` 中拦截 TCP SYN 包（`tcp_packet.syn() && !tcp_packet.ack()`），调用 `incoming_tcp_packet()` 预创建 socket。

`incoming_tcp_packet()` 在 `syn_queue` 未满时创建新 `TcpSocket` 并调用 `socket.listen()`，推入队列供后续 `accept()` 获取。

### accept()

```rust
pub fn accept(&self, endpoint: IpListenEndpoint, sockets: &mut SocketSet) -> AxResult<AcceptedTcp> {
    let entries = self.tcp[endpoint.port as usize].clone();
    let mut table = entries.lock();
    let entry = table.iter_mut()
        .find(|e| e.listen_endpoint == endpoint)
        .ok_or(AxError::InvalidInput)?;

    // 遍历 syn_queue，跳过 Closed 且无数据的 socket
    let mut idx = 0;
    while idx < entry.syn_queue.len() {
        let handle = entry.syn_queue[idx].accepted.handle;
        if is_closed_without_data(sockets, handle) {
            entry.syn_queue.swap_remove_front(idx);
            sockets.remove(handle);
            continue;
        }
        if is_acceptable(sockets, handle) {
            return Ok(entry.syn_queue.swap_remove_front(idx).unwrap().accepted);
        }
        idx += 1;
    }
    Err(AxError::WouldBlock)
}
```

可 accept 的状态：`Established | CloseWait | FinWait1 | FinWait2 | Closing | LastAck | TimeWait`。

## 通用 Poll 与超时

`GeneralOptions`（[general.rs](net/ax-net/src/general.rs)）为所有 socket 提供通用的非阻塞、超时和 poll 设施：

```rust
// general.rs
pub(crate) struct GeneralOptions {
    nonblock: AtomicBool,             // O_NONBLOCK
    reuse_address: AtomicBool,        // SO_REUSEADDR
    send_timeout_nanos: AtomicU64,    // SO_SNDTIMEO (0 = 无限)
    recv_timeout_nanos: AtomicU64,    // SO_RCVTIMEO
    bound_if: AtomicU32,              // SO_BINDTODEVICE (InterfaceId, 0 = 未绑定)
    socket_type: AtomicI32,           // SOCK_STREAM(1)/DGRAM(2)/RAW(3)
    domain: i32,                      // AF_INET(2)/AF_UNIX(1)/AF_VSOCK(40)
    protocol: i32,                    // IPPROTO_TCP(6)/UDP(17)/ICMP(1)
}
```

构造函数按 socket 类型固化 `(socket_type, domain, protocol)`：

| socket | SOCK_* | AF_* | IPPROTO_* |
| --- | --- | --- | --- |
| `TcpSocket` | 1 (STREAM) | 2 (INET) | 6 (TCP) |
| `UdpSocket` | 2 (DGRAM) | 2 (INET) | 17 (UDP) |
| `RawSocket` | 3 (RAW) | 2 (INET) | 根据 `IpProtocol` |
| `UnixSocket` stream | 1 (STREAM) | 1 (UNIX) | 0 |
| `UnixSocket` dgram | 2 (DGRAM) | 1 (UNIX) | 0 |
| `VsockSocket` | 1 (STREAM) | 40 (VSOCK) | 0 |

### 核心 Poller

`send_poller_with()` 和 `recv_poller_with()` 是通用的 poll 循环：

```rust
// general.rs
pub fn send_poller_with<P: Pollable, F: FnMut() -> AxResult<T>, T>(
    &self, pollable: &P, extra_nb: bool, mut f: F) -> AxResult<T>
{
    loop {
        // 尝试执行 f()，如果成功返回结果
        match f() {
            Ok(result) => return Ok(result),
            Err(AxError::WouldBlock) => {
                // 检查 nonblock 模式 -> 直接返回 WouldBlock
                if self.nonblock.load(Relaxed) || extra_nb { return Err(AxError::WouldBlock); }
            }
            Err(e) => return Err(e),
        }
        // 注册 waker 到对应设备
        self.register_waker(context.waker());
        // block_on 等待 I/O 事件或超时
        // ...
    }
}
```

流程：`f()` 返回 `WouldBlock` → `register_waker()` 注册到 `Service` → `block_on(poll_io())` 等待 → 超时返回 `TimedOut` → 重新循环。

`register_waker()` 根据 `DeviceBinding` 选择性注册到匹配的设备：

```rust
pub(crate) fn register_waker(&self, waker: &Waker) {
    get_service().register_waker(self.device_binding(), waker);
}
```

绑定 `DeviceBinding { bound_if: Some(eth0) }` 时只注册到 eth0 的 waker，避免被无关设备唤醒。
