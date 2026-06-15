---
sidebar_position: 4
sidebar_label: "Socket 系统"
---

# Socket 系统

本文描述 `ax-net` 的 socket 抽象层：统一的 backend 分层、端口仲裁机制、状态机以及通用 poll 设施。

## Backend 分层

`Socket` 枚举（[socket.rs](net/ax-net/src/socket.rs)）聚合 5 类 backend，统一实现 `SocketOps` + `Configurable`：

| Backend | 类型 | 协议核心 | 源码 |
| --- | --- | --- | --- |
| `TcpSocket` | SOCK_STREAM / AF_INET | smoltcp TCP socket | [tcp.rs](net/ax-net/src/tcp.rs) |
| `UdpSocket` | SOCK_DGRAM / AF_INET | smoltcp UDP socket | [udp.rs](net/ax-net/src/udp.rs) |
| `RawSocket` | SOCK_RAW / AF_INET | smoltcp raw socket | [raw.rs](net/ax-net/src/raw.rs) |
| `UnixSocket` | SOCK_STREAM/SOCK_DGRAM / AF_UNIX | 自包含 `Transport`（不经 smoltcp） | [unix/mod.rs](net/ax-net/src/unix/mod.rs) |
| `VsockSocket` | SOCK_STREAM / AF_VSOCK | `rdif-vsock` 驱动 + ring buffer | [vsock/mod.rs](net/ax-net/src/vsock/mod.rs) |

IP 类 socket 通过 `SOCKET_SET.add()` 注册到全局 `SocketSet`，获得 `SocketHandle`；Unix / vsock 不依赖 smoltcp，各自维护连接状态。

## SocketSetWrapper

`SocketSetWrapper`（[wrapper.rs](net/ax-net/src/wrapper.rs)）在 smoltcp 原生 `SocketSet` 之上增加了 UDP 端口冲突仲裁：

```rust
pub(crate) struct SocketSetWrapper<'a> {
    pub inner: Mutex<SocketSet<'a>>,
    udp_binds: Mutex<HashMap<UdpBindKey, SocketHandle>>,
}
```

UDP bind 仲裁规则（`udp_bind_available()`）：

- **精确 bind**（如 `192.168.1.1:53`）：如果已有同地址同端口则拒绝；如果有 wildcard `0.0.0.0:53` 则拒绝。
- **Wildcard bind**（`0.0.0.0:53`）：如果任何地址已占用同一端口则拒绝。
- **`SO_REUSEADDR`**：设置后跳过仲裁。

`udp_port_available()` 暴露给 ephemeral port 分配器，避免分配到已占用的端口。

## TCP 端口仲裁

除了 `ListenTable`（管理 `listen()` 端口），`ax-net` 还有独立的 `TCP_BOUND_PORTS`（[tcp.rs](net/ax-net/src/tcp.rs)）来追踪已 bind 但尚未 listen 的端口：

```rust
static TCP_BOUND_PORTS: LazyLock<Mutex<HashMap<u16, Vec<Option<IpAddress>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn register_tcp_bound(endpoint: IpListenEndpoint) -> AxResult;
fn unregister_tcp_bound(endpoint: IpListenEndpoint);
```

两个系统协同工作：
- `LISTEN_TABLE`：管理已进入 `listen()` 状态的端口及 SYN 队列。
- `TCP_BOUND_PORTS`：追踪已 bind（可能尚未 listen）的端口，防止地址冲突的重复 bind。`register_tcp_bound()` 在 `bind()`、`listen()`、`connect()` 中被调用；`unregister_tcp_bound()` 在 `shutdown()`、`Drop` 和错误回退中被调用。
- `tcp_port_available(port)`：同时检查两个表，确保 `get_ephemeral_port()` 不会分配到已被占用的端口。

`listen_addrs_conflict(a, b)` 判断两个 `IpListenEndpoint` 是否冲突：任一为 `None`（wildcard）即冲突，或两者值相等即冲突。

## 状态机

`StateLock`（[state.rs](net/ax-net/src/state.rs)）为 socket 提供基于 CAS 的轻量状态转换锁：

```
Idle ──bind──→ Idle (不变，但注册 endpoint)
Idle ──listen──→ Listening
Idle ──connect──→ Connecting ──SYN/SYN+ACK──→ Connected
Listening ──accept──→ Listening (不变，但生成新 socket)
Connected ──close──→ Closed
```

核心 API：

```rust
pub fn lock(&self, expect: State) -> Result<StateGuard<'_>, State> {
    // CAS: expect → Busy，失败返回当前状态
}
pub fn transit<R>(self, new: State, f: impl FnOnce() -> AxResult<R>) -> AxResult<R> {
    // 执行 f()，成功则写 new 状态，失败则回退到原始状态
}
```

`StateLock` 保证绑定/监听/连接等操作的原子性：并发 bind+connect 在同一 socket 上只能有一个成功。

## ListenTable 与 SYN Pre-create

`ListenTable`（[listen_table.rs](net/ax-net/src/listen_table.rs)）是 TCP 监听的核心数据结构。它包含 65536 个端口 bucket，每个端口是一个 `Arc<Mutex<Vec<ListenTableEntryInner>>>`。同一端口可并存多个互不冲突的具体地址 listener，wildcard listener 会与该端口所有地址冲突：

```rust
struct ListenTableEntryInner {
    listen_endpoint: IpListenEndpoint,
    backlog: usize,                      // clamp 到 LISTEN_QUEUE_SIZE=512
    syn_queue: VecDeque<PendingTcp>,
    accept_poll: Arc<PollSet>,
}
```

### SYN 预创建

`snoop_tcp_packet()` 在 RX 路径中拦截 TCP SYN 包：

1. 解析 TCP 包头，检查 SYN 标志。
2. 按目的端口查 `LISTEN_TABLE`，匹配具体 listener。
3. 如果 SYN queue 未满，预创建 `TcpSocket` 并调用 `socket.listen()`，加入 `syn_queue`。
4. smoltcp `Interface::poll()` 完成三次握手。
5. `accept()` 时直接从 `syn_queue` 取出已完成握手的 socket。

SYN queue 满时丢弃 SYN 包（warning），由客户端重传机制保证可靠性。

### accept()

`ListenTable::accept()` 遍历 `syn_queue`，跳过已关闭且无未读数据的 socket，返回第一个可 accept 的连接（TCP state 为 Established/CloseWait/FinWait1/FinWait2 等）。

## 通用 Poll 与超时

`GeneralOptions`（[general.rs](net/ax-net/src/general.rs)）为所有 socket 类型提供通用设施：

```rust
pub(crate) struct GeneralOptions {
    nonblock: AtomicBool,             // O_NONBLOCK
    reuse_address: AtomicBool,        // SO_REUSEADDR
    send_timeout_nanos: AtomicU64,    // SO_SNDTIMEO
    recv_timeout_nanos: AtomicU64,    // SO_RCVTIMEO
    bound_if: AtomicU32,              // SO_BINDTODEVICE (InterfaceId)
    socket_type: AtomicI32,           // SOCK_STREAM/DGRAM/RAW
    domain: i32,                      // AF_INET/AF_UNIX/AF_VSOCK
    protocol: i32,                    // IPPROTO_TCP/UDP/ICMP
}
```

poll helper：

- `send_poller()` / `recv_poller()`：检查 `nonblock` → `register_waker()` → `block_on(poll_io())` 自旋等待 → 超时检查 → 执行操作。
- `send_poller_with()`：支持指定是否检测 HUP 事件。
- `register_waker()`：根据 `DeviceBinding` 将调用者 waker 注册到对应设备的 `PollSet`。

### socket 常量

每个 socket 创建时固化：

| socket 类型 | SOCK_\* | AF_\* | IPPROTO_\* |
| --- | --- | --- | --- |
| `TcpSocket` | 1 (STREAM) | 2 (INET) | 6 (TCP) |
| `UdpSocket` | 2 (DGRAM) | 2 (INET) | 17 (UDP) |
| `RawSocket` | 3 (RAW) | 2 (INET) | 根据 `IpProtocol` |
| `UnixSocket` (stream) | 1 (STREAM) | 1 (UNIX) | 0 |
| `UnixSocket` (dgram) | 2 (DGRAM) | 1 (UNIX) | 0 |
| `VsockSocket` | 1 (STREAM) | 40 (VSOCK) | 0 |
