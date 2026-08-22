---
sidebar_position: 4
sidebar_label: "Socket 系统"
---

# Socket 系统

`ax-net` 的 socket 层向上提供统一的 POSIX-like socket facade，向下分别连接 smoltcp IP socket、内核 Unix domain socket transport 和可选 vsock transport。IP 类 socket 共享单个 smoltcp `SocketSet`，但 TCP 监听、UDP bind 冲突、raw packet 过滤、Unix namespace 和 vsock connection manager 都由 `ax-net` 在协议核心外补齐。

核心源码：

| 源码 | 职责 |
| --- | --- |
| `socket.rs` | 统一地址、send/recv 选项、`SocketOps` trait、`Socket` 枚举分发 |
| `general.rs` | 通用 socket 选项、超时、nonblocking、`SO_BINDTODEVICE`、poll helper |
| `wrapper.rs` | 全局 smoltcp `SocketSet` 包装和 UDP bind side table |
| `state.rs` | TCP 等 socket 的轻量状态门禁 |
| `listen_table.rs` | TCP listen bucket、SYN/accept 队列、accept waker |
| `tcp.rs` | TCP stream socket、端口仲裁、connect/listen/accept、orphan 接入 |
| `udp.rs` | UDP datagram socket、connected peer、MSG_MORE corking、route-aware source selection |
| `raw.rs` | Raw IP socket、ICMP loopback、peer filter、deferred RX |
| `unix/` | Unix stream/datagram transport、abstract/path namespace |
| `vsock/` | 可选 AF_VSOCK facade 和 stream transport |

源码表将公共外观、IP backend、本地 transport 和辅助状态分别定位到模块所有者。设计边界据此强调统一 API 只共享调用形状，各 backend 仍独立拥有协议状态、端口仲裁和清理规则。

## 1. 设计边界

socket 层通过 `SocketOps` trait 和 `Socket` 枚举把系统调用语义映射到协议栈内部对象。它将 AF_INET、AF_UNIX、AF_VSOCK 的地址统一为 `SocketAddrEx`，将 `bind/connect/listen/accept/send/recv/shutdown` 统一为 trait 方法，并通过 `GeneralOptions` 维护 `O_NONBLOCK`、`SO_REUSEADDR`/`SO_REUSEPORT`、超时、`IP_MTU_DISCOVER` 和 `SO_BINDTODEVICE` 等通用选项。

IP 类 socket（TCP/UDP/raw）持有 smoltcp `SocketHandle`，注册到全局 `SocketSetWrapper`。socket 层补齐 smoltcp 不直接提供的 POSIX 语义——TCP accept queue（`ListenTable`）、UDP wildcard bind 冲突（`udp_binds` side table）、raw connected-peer 过滤。出接口选择由控制面 `NetControl` 在 bind/connect 时决策，实际发包由 `Router::dispatch()` 在 net-poll worker 中完成；socket 操作本身不同步推进 `Interface::poll()`，只调用 `request_poll()` 请求 worker 推进。

Unix domain socket 和 vsock 不经过 smoltcp，各自维护独立的 transport、namespace 和连接状态，但共享 `SocketOps`/`Configurable`/`Pollable` 入口，向上层呈现一致的 socket facade。

典型关系如下：

![ax-net socket 子系统架构](images/socket-subsystem-architecture.svg)

架构图显示 TCP、UDP 与 Raw Socket 共享 `SocketSet`、控制面和网络轮询，而 Unix 与 vsock 使用各自 transport。公共外观层只负责把调用路由到这些所有者，不应把协议专属状态提升为所有 socket 的共同字段。

## 2. 公共外观层

公共 facade 定义跨协议族共享的数据形状和操作入口。系统调用层只需要持有 `Socket`，不需要知道底层是 smoltcp、Unix transport 还是 vsock connection manager。

### 2.1 地址选项

`SocketAddrEx` 是 syscall 层与 backend 之间的跨地址族枚举，分别承载 IP endpoint、Unix 路径或抽象地址以及 vsock CID/port。统一类型只收敛调用形状，不放宽地址族校验；每个 backend 仍需在 bind、connect 和 name 查询入口拒绝错误变体。

```rust
// socket.rs
pub enum SocketAddrEx {
    Ip(SocketAddr),
    Unix(UnixSocketAddr),
    #[cfg(feature = "vsock")]
    Vsock(VsockAddr),
}
```

send/recv 选项保留 Linux `MSG_*` 语义：

```rust
pub struct SendOptions {
    pub to: Option<SocketAddrEx>,
    pub flags: SendFlags,
    pub cmsg: Vec<CMsgData>,
    pub sender_credentials: Option<UnixCredentials>,
}

pub struct RecvOptions<'a> {
    pub from: Option<&'a mut SocketAddrEx>,
    pub flags: RecvFlags,
    pub cmsg: Option<&'a mut Vec<CMsgData>>,
    pub truncated: Option<&'a mut bool>,
}
```

其中 `MSG_DONTWAIT` 只影响当前调用，不修改 socket 自身的 `O_NONBLOCK`；`MSG_PEEK`、`MSG_TRUNC`、`MSG_OOB`、`MSG_MORE` 由具体 transport 按协议语义解释。`sender_credentials` 由 OS syscall 层在真实发送点填写，供 Unix `SO_PASSCRED` 使用。

### 2.2 Socket 能力

`SocketOps` 定义所有 backend 对系统调用层承诺的生命周期、地址操作、I/O 和 readiness 能力。TCP、UDP、raw、Unix 与 vsock 通过同一 trait 被 `Socket` 外观层持有，但错误语义和支持的 flags 仍由具体实现负责。

```rust
pub trait SocketOps: Configurable {
    fn bind(&self, local_addr: SocketAddrEx) -> NetResult;
    fn connect(&self, remote_addr: SocketAddrEx) -> NetResult;
    fn listen(&self, _backlog: usize) -> NetResult {
        Err(NetError::OperationNotSupported)
    }
    fn is_listening(&self) -> bool { false }
    fn accept(&self) -> NetResult<Socket> {
        Err(NetError::OperationNotSupported)
    }
    fn send(&self, src: impl Read + IoBuf, options: SendOptions) -> NetResult<usize>;
    fn recv(&self, dst: impl Write + IoBufMut, options: RecvOptions<'_>) -> NetResult<usize>;
    fn recv_available(&self) -> NetResult<usize> {
        Err(NetError::OperationNotSupported)
    }
    fn local_addr(&self) -> NetResult<SocketAddrEx>;
    fn peer_addr(&self) -> NetResult<SocketAddrEx>;
    fn shutdown(&self, how: Shutdown) -> NetResult;
}
```

默认实现只给出“不支持”的语义，具体 backend 再按协议覆盖。例如 TCP、Unix stream/seqpacket 和 vsock stream 支持 `listen/accept`，UDP/raw/Unix datagram 不支持。

### 2.3 后端分发

`Socket` 枚举是封闭的运行期分发边界，把统一 API 转交给具体 backend，同时保留穷尽匹配带来的可审计性。新增 transport 时需要在该枚举中显式覆盖地址、I/O、poll、option 和清理路径，避免出现仅能创建却无法完整关闭的半实现状态。

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

枚举变体只表达运行期 transport 选择，每个 backend 仍拥有不同协议核心、side table 和本地状态。下表把地址族与关键所有者对应起来，新增变体时可以据此检查是否补齐了创建、I/O、poll 和清理路径。

| Backend | 地址族/类型 | 协议核心 | 关键状态 |
| --- | --- | --- | --- |
| `TcpSocket` | AF_INET / SOCK_STREAM | smoltcp TCP socket | `StateLock`、`TCP_BOUND_PORTS`、`ListenTable`、orphan |
| `UdpSocket` | AF_INET / SOCK_DGRAM | smoltcp UDP socket | UDP bind side table、connected peer、cork |
| `RawSocket` | AF_INET / SOCK_RAW | smoltcp raw socket | local/peer filter、loopback RX、deferred RX |
| `UnixSocket` | AF_UNIX / stream,dgram | in-kernel transport | abstract/path namespace、stream/dgram transport |
| `VsockSocket` | AF_VSOCK / stream | rdif-vsock transport | connection manager、stream ring buffers |

backend 对照表说明“共享”只发生在外观和若干辅助能力层，不意味着所有 transport 进入 smoltcp。下面的共享状态章节专门描述跨 IP backend 或跨全部 socket 复用的选项、handle 与状态门禁。

## 3. 共享 Socket 状态

共享状态层包含三类内容：通用 socket 选项、smoltcp handle 空间，以及状态转换门禁。它们不表达某个协议的完整语义，只提供所有 backend 复用的基础设施。

### 3.1 通用选项状态

`GeneralOptions` 被 TCP、UDP、raw、Unix、vsock transport 复用，用来维护通用 socket option 和阻塞等待入口：

```rust
// general.rs
pub(crate) struct GeneralOptions {
    nonblock: AtomicBool,
    reuse_address: AtomicBool,
    reuse_port: AtomicBool,
    send_timeout_nanos: AtomicU64,
    recv_timeout_nanos: AtomicU64,
    bound_if: AtomicU32,
    ip_tos: AtomicU8,
    ip_mtu_discover: AtomicU8,
    recv_tos: AtomicBool,
    recv_traffic_class: AtomicBool,
    priority: AtomicI32,
    socket_type: AtomicI32,
    domain: i32,
    protocol: i32,
}
```

构造时固定 `(SOCK_*, AF_*, IPPROTO_*)`，后续 `getsockopt()` 直接从这里读取：

| socket | SOCK_* | AF_* | protocol |
| --- | --- | --- | --- |
| TCP | `SOCK_STREAM` | `AF_INET` | `IPPROTO_TCP` |
| UDP | `SOCK_DGRAM` | `AF_INET` | `IPPROTO_UDP` |
| Raw | `SOCK_RAW` | `AF_INET` | 创建时指定的 `IpProtocol` |
| Unix stream | `SOCK_STREAM` | `AF_UNIX` | `0` |
| Unix dgram | `SOCK_DGRAM` | `AF_UNIX` | `0` |
| Unix seqpacket | `SOCK_SEQPACKET` | `AF_UNIX` | `0` |
| Vsock stream | `SOCK_STREAM` | `AF_VSOCK` | `0` |

`bound_if` 保存的是稳定的 `InterfaceId`，不是 Router 内部设备索引：

```rust
pub fn set_device_binding(&self, binding: DeviceBinding) {
    self.bound_if.store(
        binding.bound_if.map_or(0, InterfaceId::get),
        Ordering::Release,
    );
}

pub fn device_binding(&self) -> DeviceBinding {
    let raw = self.bound_if.load(Ordering::Acquire);
    DeviceBinding {
        bound_if: (raw != 0).then_some(InterfaceId::new(raw)),
    }
}
```

QoS 相关选项也集中在这里保存：

- `ip_tos`：`setsockopt(IP_TOS)` 写入时会清掉 ECN 两位；TCP/UDP 通过 `ip_tos.rs` 注册 per-socket egress policy，Router dispatch 时改写 IPv4 DSCP/ECN 或 IPv6 traffic class；raw socket 在构造 IP header 后直接改写。
- `recv_tos` / `recv_traffic_class`：UDP `recvmsg()` 根据 `rx_meta.rs` 放在 smoltcp `PacketMeta.id` 中的 ingress metadata 生成 `IpCmsg::Ipv4Tos` 或 `IpCmsg::Ipv6TrafficClass`。
- `priority`：`SO_PRIORITY` 仅接受 Linux 普通非特权范围 `0..=6` 并保存数值；当前不参与设备队列调度。

通用选项列表中只有一部分字段直接影响协议 backend，其余字段用于等待、身份或 ABI 回显。`SocketSetWrapper` 则只服务 IP socket 的 smoltcp handle 与 UDP bind 表，不被 Unix 或 vsock transport 使用。

### 3.2 SocketSet 包装器

TCP、UDP 和 raw socket 都注册到同一个 smoltcp `SocketSet`，由 `SocketSetWrapper` 持有：

```rust
pub(crate) struct SocketSetWrapper<'a> {
    pub inner: Mutex<SocketSet<'a>>,
    udp_binds: Mutex<HashMap<u16, Vec<UdpBoundEntry>>>,
}
```

统一 `SocketSet` 的意义：

- TCP/UDP/raw 共享同一 handle 空间。
- net-poll worker 可以一次性推进所有 IP socket。
- TCP listen table 和 orphan reaper 可以通过 handle 操作 child socket。
- 不需要为每个接口复制 socket set，wildcard listen 和动态 route 选择保持简单。

`SocketSetWrapper` 只封装 smoltcp socket 访问，不持有 `Service` 锁，也不直接唤醒任务：

```rust
pub fn with_socket_mut<T: AnySocket<'a>, R, F>(&self, handle: SocketHandle, f: F) -> R
where
    F: FnOnce(&mut T) -> R,
{
    let mut set = self.inner.lock();
    let socket = set.get_mut(handle);
    f(socket)
}
```

SocketSet 包装代码表明 handle 操作和 UDP bind 表更新都在同一包装边界内完成，避免 drop 后残留占用。状态锁位于单个 socket 外观层，用于阻止并发生命周期转换，而不替代全局 handle 锁。

### 3.3 状态锁

`StateLock` 是 TCP 等 socket 的轻量状态门禁，避免同一个 socket 上并发 `bind/connect/listen` 进入不一致状态：

```rust
#[repr(u8)]
pub(crate) enum State {
    Idle = 0,
    Busy = 1,
    Connecting = 2,
    Connected = 3,
    Listening = 4,
    Closed = 5,
}

pub struct StateLock(AtomicU8);
```

`lock(expect)` 通过 CAS 把期望状态临时切到 `Busy`，`StateGuard::transit()` 在操作成功后提交新状态，失败时回退旧状态：

```rust
pub fn lock(&self, expect: State) -> Result<StateGuard<'_>, State> {
    match self.0.compare_exchange(
        expect as u8,
        State::Busy as u8,
        Ordering::Acquire,
        Ordering::Acquire,
    ) {
        Ok(_) => Ok(StateGuard(self, expect as u8)),
        Err(old) => Err(old.try_into().expect("invalid state")),
    }
}
```

典型 TCP 公共状态流：

```text
Idle --bind--> Idle
Idle --listen--> Listening
Idle --connect--> Connecting --established--> Connected
Listening --accept--> Listening
Connected --shutdown/drop--> Closed or orphaned smoltcp socket
```

状态转换示例说明 `StateLock` 只在检查与提交 public state 时短暂持有，协议等待发生在锁外。端口监听仲裁增加跨 socket 的全局冲突规则，需要使用独立 side table。

## 4. 端口监听仲裁

端口仲裁是 POSIX 兼容语义的一部分，不能完全交给 smoltcp。`ax-net` 使用 TCP 和 UDP 各自的 side table 表达 wildcard/specific-address 冲突关系。

### 4.1 UDP 绑定表

UDP bind side table 位于 `SocketSetWrapper`，用于补齐 Linux 风格的 wildcard bind 冲突：

```rust
#[derive(Clone, Copy)]
struct UdpBoundEntry {
    addr: Option<IpAddress>,
    reuse_port: bool,
    handle: SocketHandle,
}
```

side table entry 保存的是冲突仲裁所需的最小 bind 身份，而不是完整 `UdpSocket` 引用。下表说明 wildcard、具体地址与 reuse 选项如何组合，所有参与共同绑定的 owner 都必须满足相同规则。

| bind 类型 | 示例 | 冲突规则 |
| --- | --- | --- |
| 精确地址 | `192.168.1.10:53` | 同地址同端口冲突；同端口 wildcard 已存在也冲突 |
| Wildcard | `0.0.0.0:53` | 任意地址已占用该端口即冲突 |
| `SO_REUSEADDR` | socket option | 当前只保存/读回，不绕过 side table 冲突检查 |
| `SO_REUSEPORT` | socket option | 只有新旧 owner 都设置且 address 完全相同（两者均 wildcard 或相同具体地址）时允许共同绑定；wildcard 与具体地址仍冲突 |

UDP 冲突表反映当前实现只让地址完全相同的 `SO_REUSEPORT` owner 共同绑定，wildcard 与具体地址不会合组。TCP 使用不同的端口表和监听对象模型，因此下一节单独说明其 bind 所有权。

### 4.2 TCP 端口表

TCP 除了 listen table，还需要记录“已经 bind 但还没有 listen/connect 完成”的端口所有权：

```rust
static TCP_BOUND_PORTS: LazyLock<Mutex<HashMap<u16, Vec<TcpBoundEntry>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn listen_addrs_conflict(a: Option<IpAddress>, b: Option<IpAddress>) -> bool {
    a.is_none() || b.is_none() || a == b
}

struct TcpBoundEntry {
    addr: Option<IpAddress>,
    reuse_port: bool,
}
```

语义是 wildcard 与具体地址冲突，两个具体地址仅在相等时冲突；相同 address（包括均为 wildcard）只有全部 owner 都启用 `SO_REUSEPORT` 才能共同 bind。ephemeral TCP 端口分配仍保守地避开任何 bound/listen owner。

```rust
fn tcp_port_available(port: u16) -> bool {
    LISTEN_TABLE.can_listen(IpListenEndpoint { addr: None, port })
        && !TCP_BOUND_PORTS.lock().contains_key(&port)
}
```

这里用 wildcard endpoint 检查 listen table 是有意的保守策略：自动分配 ephemeral port 时，只要该端口已经存在任何 listen entry，就不再分配给主动连接 socket。

### 4.3 监听表

`ListenTable` 是 TCP passive open 的核心数据结构。smoltcp 没有“一个 public listen socket 管理多个 child socket”的 POSIX 对象模型，所以 `ax-net` 在外部维护 accept queue：

```rust
struct ListenTableEntryInner {
    listen_endpoint: IpListenEndpoint,
    backlog: usize,
    syn_queue: VecDeque<AcceptedTcp>,
    accept_poll: Arc<PollSet>,
    reuse_port: bool,
}

pub struct ListenTable {
    tcp: Mutex<HashMap<u16, ListenTableEntry>>,
}
```

`tcp` 按端口懒创建 listen bucket，每个 bucket 存放该端口下的多个具体地址 listener。`listen()` 检查 wildcard/specific 冲突后插入 entry：

```rust
pub fn listen(
    &self,
    listen_endpoint: IpListenEndpoint,
    backlog: usize,
    reuse_port: bool,
) -> NetResult {
    let port = listen_endpoint.port;
    let entries = self.listen_entry_or_create(port);
    let mut entries = entries.lock();
    if entries
        .iter()
        .any(|entry| listen_entries_conflict(entry, listen_endpoint, reuse_port))
    {
        return Err(NetError::AddrInUse);
    }
    entries.push(ListenTableEntryInner::new(listen_endpoint, backlog));
    Ok(())
}
```

backlog 被 clamp 到 `1..=512`。reuseport listener 可以共享相同 endpoint，但 incoming SYN 当前使用 `.find()` 选择第一个匹配 entry，没有实现 Linux 的 flow hash 或负载均衡；共同监听只表示“允许注册”，不是公平分发承诺。

### 4.4 SYN 预创建

`Router::poll()` 在 RX 路径 snoop TCP SYN 包，`incoming_tcp_packet()` 匹配 listen endpoint 后预创建 child smoltcp socket，并推入 listener 的 `syn_queue`。这样每条 pending 连接都有自己的 smoltcp TCP 状态机，可以独立完成 SYN-RECEIVED 到 ESTABLISHED 的推进。

`accept()` 遍历 `syn_queue`，清理已经关闭且无数据的 child，返回第一个可接受 socket：

```rust
pub fn accept(
    &self,
    listen_endpoint: IpListenEndpoint,
    sockets: &mut SocketSet<'_>,
) -> NetResult<AcceptedTcp> {
    let Some(entries) = self.listen_entry(listen_endpoint.port) else {
        return Err(NetError::InvalidInput);
    };
    let mut table = entries.lock();
    let Some(entry) = table
        .iter_mut()
        .find(|entry| entry.listen_endpoint == listen_endpoint)
    else {
        return Err(NetError::InvalidInput);
    };

    let syn_queue: &mut VecDeque<AcceptedTcp> = &mut entry.syn_queue;
    let mut idx = 0;
    while idx < syn_queue.len() {
        let handle = syn_queue[idx].handle;
        if is_closed_without_data(sockets, handle) {
            syn_queue.swap_remove_front(idx);
            sockets.remove(handle);
            continue;
        }
        if is_acceptable(sockets, handle) {
            return Ok(syn_queue.swap_remove_front(idx).unwrap());
        }
        idx += 1;
    }
    Err(NetError::WouldBlock)
}
```

可接受状态包括已经建立以及已经进入关闭流程但仍可被 userspace 观察的 child，例如 `Established`、`CloseWait`、`FinWait*`、`Closing`、`LastAck`、`TimeWait`。

## 5. IP Socket 后端

TCP、UDP 和 raw socket 都持有 smoltcp `SocketHandle`，但它们在 public 语义、side table 和 packet 格式上差异很大。

### 5.1 TCP Socket

`TcpSocket` 包装 smoltcp stream socket，并维护 public TCP 状态、端口注册、peer endpoint、keepalive/TCP_INFO 选项和 readiness poll set：

```rust
pub struct TcpSocket {
    state: StateLock,
    handle: SocketHandle,
    bound_endpoint: Mutex<IpListenEndpoint>,
    peer_endpoint: Mutex<Option<IpEndpoint>>,
    tos_key: Mutex<Option<EgressIpTosKey>>,
    bound_registered: AtomicBool,
    general: GeneralOptions,
    pending_error: AtomicI32,
    keep_idle_secs: AtomicU32,
    keep_interval_secs: AtomicU32,
    keep_count: AtomicU32,
    user_timeout_millis: AtomicU32,
    rx_closed: AtomicBool,
    poll_rx: Arc<PollSet>,
    poll_tx: Arc<PollSet>,
    poll_rx_closed: PollSet,
}
```

TCP socket 的主要职责：

- `bind()`：通过控制面校验本地地址并注册 `TCP_BOUND_PORTS`。
- `connect()`：选择 route/source，绑定 ephemeral port，启动 smoltcp connect，然后 `request_poll()`。
- `listen()`：把 endpoint 移入 `ListenTable`。
- `accept()`：从 `ListenTable` 取出 child handle，构造已连接 `TcpSocket`。
- `send/recv()`：只操作 smoltcp socket buffer，不同步驱动完整 interface poll。
- `drop()`：必要时把未完全关闭的 smoltcp socket移入 orphan reaper。

TCP 能力列表依赖 stream 状态机、端口表和 orphan reaper，无法复用于 datagram。UDP backend 采用独立 endpoint、bind side table 与消息边界，下一节按这些差异展开。

### 5.2 UDP Socket

`UdpSocket` 是 datagram backend，保留本地 endpoint、connected peer 和 `MSG_MORE` cork 状态：

```rust
struct CorkState {
    buf: Vec<u8>,
    remote: IpEndpoint,
    source: IpAddress,
}

pub struct UdpSocket {
    handle: SocketHandle,
    bind_lock: Mutex<()>,
    local_addr: Mutex<Option<IpEndpoint>>,
    peer_addr: Mutex<Option<(IpEndpoint, IpAddress)>>,
    general: GeneralOptions,
    tos_keys: Mutex<Vec<EgressIpTosKey>>,
    cork: Mutex<Option<CorkState>>,
}
```

设计要点：

- bind 时通过 `SocketSetWrapper::udp_bind()` 记录 wildcard/specific ownership。
- connect/sendto 时通过控制面 route decision 选择源地址。
- connected UDP 保存 `(peer endpoint, selected source)`，recv 时过滤不匹配 peer 的 datagram。
- `MSG_MORE` 会把多次 send 合并为一个 datagram，并固定第一次 send 的 remote/source，避免后续调用改变目标。
- drop 时先同步 `flush_egress()`：等待 `Required` poll ownership，把 smoltcp UDP TX queue 排空到 Router/设备队列，再从 UDP bind side table 和 `SocketSet` 移除 handle。

UDP 列表中的 connected peer 只是默认收发对端，不改变 datagram 的 packet-oriented 本质。Raw Socket 位于更低的 IP 协议层，因而使用协议 filter 和不同的 packet 格式约定。

### 5.3 Raw Socket

`RawSocket` 暴露 IP 层以上、TCP/UDP 以下的 packet-oriented 接口，并保存协议 filter、TTL、traffic class 及接收控制消息选项。ping datagram 等兼容路径也在这一后端转换 ICMP 语义，因此不能把它当作无状态的字节透传接口。

```rust
pub struct RawSocket {
    handle: SocketHandle,
    ip_version: IpVersion,
    mode: RawSocketMode,
    local_addr: RwLock<Option<IpAddress>>,
    peer_addr: RwLock<Option<IpAddress>>,
    loopback_rx: Mutex<Option<(IpAddress, Vec<u8>)>>,
    deferred_rx: Mutex<Option<(IpAddress, Vec<u8>)>>,
    ttl: RwLock<Option<u8>>,
    recv_ttl: AtomicBool,
    rx_closed: AtomicBool,
    tx_closed: AtomicBool,
    general: GeneralOptions,
}
```

raw socket 有两个特别路径：

- `loopback_rx` 保存本地快速路径产生、尚未被 recv 取走的 loopback packet。
- `deferred_rx` 保存 connected-peer 过滤时暂存的非当前可交付 packet，格式保持为一致的 wire packet，避免 peek/filter 后破坏 smoltcp receive queue 语义。
- `RawSocketMode::PingDatagram` 由 `new_ipv4_ping()` 创建，Linux-visible 类型为 `SOCK_DGRAM`，接收只返回 ICMP payload；普通 IPv4 raw 接收返回完整 IP packet。
- `recv_ttl` 对应 `IP_RECVTTL`，启用后生成 `IpCmsg::Ipv4Ttl`。

发送时，如果没有显式本地地址，raw socket 通过控制面按 remote 选择 source；loopback 目的地址走本地路径，非 loopback 目的地址交给 smoltcp raw socket 和 Router dispatch。

## 6. 本地传输后端

Unix 和 vsock 不经过 smoltcp `SocketSet`，但共享 `SocketOps`、`Configurable` 和 `Pollable` 入口。它们的状态机和缓冲区由各自 transport 管理。

### 6.1 Unix Socket

Unix socket facade 维护公共 local/remote 地址，具体 stream/datagram/seqpacket 语义交给 `Transport`：

```rust
pub enum UnixSocketAddr {
    Unnamed,
    Abstract(Arc<[u8]>),
    Path(Arc<str>),
}

pub enum Transport {
    Stream(StreamTransport),
    Dgram(DgramTransport),
}

pub struct UnixSocket {
    transport: Transport,
    local_addr: Mutex<UnixSocketAddr>,
    remote_addr: Mutex<UnixSocketAddr>,
}
```

namespace 分两类：

- abstract namespace：`ABSTRACT_BINDS: HashMap<Arc<[u8]>, BindSlot>`，完全位于内存。
- path namespace：通过 `register_unix_namespace()` 注入外部 VFS namespace provider。

`BindSlot` 同时容纳 stream、datagram 和 seqpacket endpoint，因此同一路径下三类 ownership 分别仲裁：

```rust
pub struct BindSlot {
    stream: Mutex<Option<stream::Bind>>,
    dgram: Mutex<Option<dgram::Bind>>,
    seqpacket: Mutex<Option<dgram::SeqBind>>,
}
```

Unix socket 的 accept 使用 transport 自己的 `Pollable`，不涉及 `request_poll()` 或 smoltcp：

```rust
let (transport, peer_addr) =
    block_on(poll_io(&self.transport, IoEvents::IN, nonblocking, || {
        self.transport.try_accept()
    }))?;
```

namespace 绑定代码只建立地址到 socket 的可见关联，实际 payload 传输仍由具体 `Transport` 拥有。Unix Stream 使用成对 ring buffer，和路径解析或文件系统节点生命周期分离。

#### 6.1.1 Unix Stream

Unix stream 使用两组单向 `ringbuf::HeapRb` 组成全双工连接，每一端分别持有对向 producer 与本地 consumer。连接状态、shutdown 和 readiness 由 transport 对象协调，因此读写背压不经过 smoltcp 或网络轮询 Worker。

```text
endpoint A tx -> endpoint B rx
endpoint B tx -> endpoint A rx
```

每个方向还带一条 cmsg side channel。stream 的 ancillary data 不按“单个字节”保存，而是绑定到一次 send 调用产生的字节区间：

```rust
struct PendingCmsg {
    start_byte: u64,
    end_byte: u64,
    cmsg: Vec<CMsgData>,
}
```

接收端在读到 `start_byte` 后交付该 cmsg，并可在 `end_byte` 处截断一次 recv，使下一次 `recvmsg()` 从下一个带 cmsg 的消息边界开始。这避免了 `MSG_PEEK` 或分段读取时把 ancillary data 和 payload 的对应关系打散。

stream listener 的 bind 状态是 `stream::Bind`：

```text
bind/listen
  -> install stream::Bind into BindSlot.stream
connect
  -> create paired channels
  -> enqueue server-side ConnRequest
  -> wake listener poll set
accept
  -> receive ConnRequest
  -> wrap server-side channel as accepted UnixSocket
```

stream 调用链说明两端以字节 ring 和 readiness 交互，不保留消息边界。Unix Datagram 改用 message queue，并为每个 packet 保存发送端地址和控制消息数据。

#### 6.1.2 Unix Datagram

Unix datagram 使用 message queue，而不是 byte stream。每个 packet 保存 payload、发送端地址和 cmsg：

```text
DgramTransport::send
  -> resolve peer BindSlot.dgram
  -> enqueue Packet { bytes, addr, cmsg }
  -> wake receiver poll set
```

datagram 的消息边界天然保留；`MSG_TRUNC`、接收缓冲区不足和 cmsg 交付都按单个 packet 处理。path namespace 与 abstract namespace 的 ownership 仍通过同一个 `BindSlot` 管理。

同一个 `DgramTransport` 也承载 connection-oriented `SOCK_SEQPACKET`：它使用独立的 `BindSlot.seqpacket`，支持 bind/listen/connect/accept 和 socketpair，同时保持一发一收的消息边界。datagram/seqpacket 的 channel 当前是 `async_channel::unbounded()`，因此 transport queue 本身没有字节/消息上限；这与 IP Router 的有界队列模型不同，必须由调用方流量控制或后续配额机制约束。

#### 6.1.3 凭据传递

Unix transport 在 message metadata 中保存可 clone 的发送端凭据，并根据接收端 `SO_PASSCRED` 等选项生成控制消息。凭据来自 socket identity 和发送时上下文，不能在接收时用当前任务身份重新构造，否则跨进程传递会失真。

- `PassCredentials(bool)` 启用后，datagram/seqpacket 接收端才会获得 `SocketCmsg::Credentials`；发送者 credentials 由 StarryOS 在每次 send 时注入。
- `PeerCredentials(UnixCredentials)` 返回 transport 保存的 pid/uid/gid 和可选稳定进程 identity；StarryOS 会把 identity 投影到调用者的 PID namespace。
- `ReceiveTimestamp(bool)` 在 Unix datagram/seqpacket 入队时记录 wall-clock 时间并返回 `SocketCmsg::Timestamp`；`MSG_PEEK` 不消费它。
- stream channel 在连接建立时保存双方 credentials；datagram endpoint 保存创建者 credentials。

`CMsgData` 是 `Box<dyn CMsgPayload>`。payload 必须可 clone，使 Unix datagram/seqpacket 的 `MSG_PEEK` 能复制 ancillary data 而不消费队首消息；StarryOS 仍负责 Linux `cmsghdr` 二进制编解码。`into_any()` 在 ABI 层恢复具体 payload 类型。

### 6.2 Vsock Socket

vsock 是可选 feature，不属于 IP 协议，也不通过 smoltcp poll。facade 只把 `SocketOps` 映射到 stream transport：

```rust
pub struct VsockSocket {
    transport: VsockStreamTransport,
}
```

核心连接状态位于 `vsock::connection_manager`，设备事件由 vsock 设备层推进。

#### 6.2.1 Vsock 连接管理

`vsock::connection_manager` 是 AF_VSOCK stream 的全局状态表，维护监听、连接中、已连接和关闭对象以及对应 waiters。设备事件通过独立 poll Worker 写入这一管理器，无法立即交付的事件保留在 pending queue，而不会进入 IP `SocketSet`。

```rust
pub enum ConnectionState {
    Idle,
    Listening,
    Connecting,
    Connected,
    Closed,
}

pub struct VsockConnectionManager {
    connections: BTreeMap<VsockConnId, Arc<Mutex<Connection>>>,
    listen_queues: BTreeMap<u32, Arc<Mutex<ListenQueue>>>,
}
```

核心对象：

| 对象 | 职责 |
| --- | --- |
| `Connection` | 保存 state、local/peer address、RX ring、TX wait queue、RX/connect poll set、半关闭标志和统计 |
| `AcceptQueue` | listener 的已完成连接队列，容量为 `VSOCK_ACCEPT_QUEUE_SIZE` |
| `ListenQueue` | 绑定一个 local port，持有 `AcceptQueue` 和 accept poll set |
| `VSOCK_CONN_MANAGER` | 全局 manager，处理 listen/connect/accept/disconnect 和设备事件 |

每条连接拥有 `VSOCK_RX_BUFFER_SIZE = 64 KiB` 的 RX ring。设备收到数据后写入对应 connection 的 RX ring 并唤醒 `rx_wakers`；socket `recv()` 从 ring 消费。发送路径调用 `device::vsock_send()`，当 peer credit 或设备侧压力不足时通过 `tx_wait_queue` 短暂等待。

vsock 设备层还有一个临时 RX buffer 和 pending event queue：

- `VSOCK_RX_TMPBUF_SIZE = 4 KiB`：poll task 从 `rdif_vsock::Interface` 拉取事件时使用的临时接收缓冲。
- `PENDING_EVENTS`：当事件暂时无法完整交付给 manager（例如目标连接 RX ring 空间不足）时保存事件，后续 poll 周期继续处理，避免直接丢弃设备事件。

连接管理器列表定义 vsock 全局状态与事件所有者，但这些状态需要设备事件持续推进。独立轮询 Worker 负责完成这一工作，并通过引用计数避免为每个 socket 重复创建后台任务。

#### 6.2.2 Vsock 轮询 Worker

vsock 设备不进入 smoltcp poll。`start_vsock_poll()` / `stop_vsock_poll()` 使用引用计数控制一个独立 poll task：

```text
first active vsock socket
  -> start_vsock_poll()
  -> spawn vsock-poll task

last active vsock socket dropped
  -> stop_vsock_poll()
  -> poll task observes refcount=0 and exits
```

poll task 从 `rdif_vsock::Interface` 拉取事件，并分发到 `VSOCK_CONN_MANAGER`：

| 事件 | manager 动作 |
| --- | --- |
| connection request | 查找 `ListenQueue`，创建 server-side connection，压入 accept queue |
| connected | 将 outgoing connection 置为 `Connected` 并唤醒 connect waker |
| received data | 写 RX ring，唤醒 recv waker |
| credit update | 唤醒 TX wait queue |
| disconnect | 标记 close，唤醒 RX/connect waiters |

poll 频率自适应：有事件时降低 sleep interval，长时间 idle 时逐步退回较长 interval，避免空轮询占用 CPU。

## 7. 轮询唤醒

socket 阻塞语义基于 `Pollable` + `poll_io()`。应用线程通常只注册 waker 并等待 readiness；协议栈推进由 net-poll worker 或本地 transport 自己的 poll set 完成。UDP drop 的同步 egress flush 会暂时申请 `Required` poll ownership，是生命周期路径上的例外。

### 7.1 通用轮询辅助

`GeneralOptions` 提供 send/recv 两类阻塞辅助，把 nonblocking、timeout、信号中断和 `PollSet` 注册收敛为各 backend 可复用的等待规则。helper 只协调用户线程何时重试操作，不负责推进 smoltcp；IP socket 仍需通过 `request_poll()` 唤醒专用 Worker。

```rust
pub fn send_poller_with<P: Pollable, F: FnMut() -> NetResult<T>, T>(
    &self,
    pollable: &P,
    extra_nonblocking: bool,
    f: F,
) -> NetResult<T> {
    block_on(timeout(
        self.send_timeout(),
        poll_io(
            pollable,
            IoEvents::OUT,
            self.nonblocking() || extra_nonblocking,
            f,
        ),
    ))?
}
```

`poll_io()` 的流程：

1. 先执行一次 socket 操作闭包。
2. 如果成功，直接返回。
3. 如果返回 `WouldBlock` 且是 nonblocking 或 `MSG_DONTWAIT`，立即返回错误。
4. 否则调用 `Pollable::register()` 注册 waker，挂起当前任务。
5. 被唤醒或 timeout 后重试闭包。

通用 helper 列表只规定等待和重试机制，不定义某个 transport 何时可读写。IP Socket readiness 由 smoltcp buffer 与连接状态产生，并额外通过设备事件推动下一轮协议处理。

### 7.2 IP Socket 就绪状态

TCP/UDP/raw 的 `poll()` 都会先 `request_poll()`，表示需要专用 net-poll worker 推进 smoltcp：

```rust
impl Pollable for UdpSocket {
    fn poll(&self) -> IoEvents {
        request_poll();
        if self.local_addr.lock().is_none() {
            return IoEvents::empty();
        }
        let mut events = IoEvents::empty();
        self.with_smol_socket(|socket| {
            events.set(IoEvents::IN, socket.can_recv());
            events.set(IoEvents::OUT, socket.can_send());
        });
        events
    }
}
```

注册 waker 时分两层：

- 向 smoltcp socket 注册 recv/send waker，等待协议 socket buffer 状态变化。
- 通过 `GeneralOptions::register_waker()` 向匹配 `DeviceBinding` 的设备注册 waker，等待设备 RX 触发下一轮 net-poll。

两层 waker 分别观察协议 buffer 和设备 readiness，避免 socket 只等待其中一侧而错过进展。下面的入口把设备绑定一起交给 `Service`，使显式绑定的 socket 不会被无关网卡事件反复唤醒。

```rust
pub fn register_waker(&self, waker: &Waker) {
    get_service().register_waker(self.device_binding(), waker);
}
```

TCP listener 还有额外 accept waker：`ListenTable::register_accept_waker()` 会把 userspace waker 放到 listener 的 `accept_poll`，并把 `accept_poll` 转成 waker 注册到 pending child 的 recv/send readiness 上。

### 7.3 本地传输就绪状态

Unix/vsock 不调用 `request_poll()`。它们的 `Pollable` 由 transport 内部 `PollSet`、channel 或 connection manager 状态驱动。这样 AF_UNIX/AF_VSOCK 的等待路径不会依赖 IP net-poll worker。

## 8. 生命周期清理

socket drop 需要清理 public side table，但不能破坏协议栈还需要推进的状态。

### 8.1 TCP 孤儿回收

TCP drop 时，如果 smoltcp socket 已经进入需要继续关闭或 TIME-WAIT 的状态，socket 不会立即从 `SocketSet` 删除，而是交给 orphan reaper：

```text
TcpSocket::drop
  -> unregister_tcp_bound / unlisten if needed
  -> if smoltcp socket still needs protocol cleanup:
       orphan::add_orphan(handle, timestamp)
     else:
       SOCKET_SET.remove(handle)
```

这样 FIN、LAST-ACK、TIME-WAIT 等状态仍由 net-poll worker 推进，避免应用对象释放后协议状态被过早销毁。

### 8.2 UDP 与 Raw 清理

UDP drop 先调用 `flush_egress()`，等待 smoltcp UDP TX queue 为空，随后调用 `SOCKET_SET.remove(handle)`；wrapper 在 remove 中清除 UDP bind side table：

```rust
pub fn remove(&self, handle: SocketHandle) {
    self.udp_unbind(handle);
    self.inner.lock().remove(handle);
}
```

raw drop 会先 `shutdown(Shutdown::Both)`，再移除 smoltcp raw socket。raw 的 `loopback_rx` 和 `deferred_rx` 是 socket 本地暂存状态，随对象释放。

### 8.3 监听清理

TCP listener unlisten 时会从 listen table 删除 entry，并销毁尚未 accept 的 child socket handle：

```rust
pub fn unlisten(&self, listen_endpoint: IpListenEndpoint) {
    let handles = {
        let Some(entries) = self.listen_entry(listen_endpoint.port) else {
            return;
        };
        let mut entries = entries.lock();
        let Some(idx) = entries
            .iter()
            .position(|entry| entry.listen_endpoint == listen_endpoint)
        else {
            return;
        };
        entries.swap_remove(idx).into_handles()
    };
    for handle in handles {
        SOCKET_SET.remove(handle);
    }
}
```

监听清理代码在移除 public entry 后回收未 accept child，并把仍需协议关闭的连接交给相应生命周期路径。并发边界汇总这些清理操作与正常 I/O 使用的锁顺序，防止 drop 引入反向嵌套。

## 9. 并发边界

socket 层并发边界围绕三类锁：`SERVICE`、`SOCKET_SET.inner`、协议 side table。原则是 socket 操作只在必要范围内持锁，并通过 `request_poll()` 交给 net-poll worker 推进协议核心。

### 9.1 锁顺序

Socket 层锁顺序从全局 `SocketSet` 向协议专属 side table 和单 socket 局部状态单向展开，用户数据复制应尽量放在释放全局锁之后。以下典型路径用于检查 bind、listen、accept、UDP 复用和本地 transport 是否引入反向获取。

```text
net-poll path:
  SERVICE -> SOCKET_SET.inner -> smoltcp sockets

TCP listen/accept path:
  SOCKET_SET.inner -> LISTEN_TABLE bucket

TCP bind path:
  TCP_BOUND_PORTS -> LISTEN_TABLE check

UDP bind path:
  SOCKET_SET.inner / udp_binds

control-assisted bind/send path:
  NetControl.state -> RouteTable
```

需要避免的反向路径：

- 持设备锁时进入 `SocketSet` 或 `Service`。
- 持 `SocketSet` 时执行可能阻塞的用户 buffer IO。
- socket 热路径直接调用完整 interface poll。

典型锁列表表明 IP socket、Unix 和 vsock 使用不同局部同步路径，但都避免在持局部锁时执行外部阻塞操作。热路径原则将这些约束收敛为 send、connect 和 recv 实现应遵守的共同模式。

### 9.2 热路径原则

TCP、UDP 与 Raw Socket 的 send、connect 和 recv 热路径只修改局部或 smoltcp socket 状态、注册 readiness，并提交轻量轮询请求。完整协议推进留给 `net-poll` Worker，使调用者不会在持有 socket 局部锁时同步进入 `Service::poll()`。

1. 操作对应 smoltcp socket 或本地 socket 状态。
2. 调用 `request_poll()` 请求专用 net-poll worker 推进协议栈。
3. 在 `WouldBlock` 时通过 `Pollable::register()` 注册 waker 并让出当前任务。

这个模型保持应用线程和协议栈驱动线程分离，避免 socket 调用者临时成为 smoltcp interface owner。
