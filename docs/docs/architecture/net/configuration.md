---
sidebar_position: 7
sidebar_label: "配置参考"
---

# 配置参考

`ax-net` 的配置目标是用结构化数据表达接口意图，而不是依赖旧的全局单网口环境变量。每个 Ethernet 接口可以独立使用静态 IPv4 或 DHCP，并携带 metric 和 DNS 配置。

## Cargo Feature

`ax-net` 的 feature 定义在 [net/ax-net/Cargo.toml#L10-L12](net/ax-net/Cargo.toml#L10-L12)：

```toml
[features]
vsock = ["dep:rdif-vsock"]
```

| feature | 作用 |
| --- | --- |
| `vsock` | 启用 `rdif-vsock` 依赖和 vsock socket / device 支持 |

启用后，`lib.rs` 中通过 `#[cfg(feature = "vsock")]` 条件编译导出 `init_vsock()`（[lib.rs#L360](net/ax-net/src/lib.rs#L360)）、`vsock` 模块（[lib.rs#L45](net/ax-net/src/lib.rs#L45)）以及 `Socket::Vsock` 变体（[socket.rs#L263-L264](net/ax-net/src/socket.rs#L263-L264)）。基础 TCP、UDP、raw、Unix domain socket、DNS、DHCP 和 Ethernet 能力不需要额外 feature。

smoltcp 在 [Cargo.toml#L34-L52](net/ax-net/Cargo.toml#L34-L52) 中固定启用以下能力：

- `alloc`
- `log`
- `async`
- `medium-ethernet`
- `medium-ip`
- `proto-ipv4`
- `proto-ipv6`
- `socket-raw`
- `socket-icmp`
- `socket-udp`
- `socket-tcp`
- `socket-dhcpv4`
- `socket-dns`

## 缓冲区常量

socket 与设备缓冲区大小集中定义在 [net/ax-net/src/consts.rs](net/ax-net/src/consts.rs)：

```rust
pub const STANDARD_MTU: usize = 1500;
pub const TCP_RX_BUF_LEN: usize = 64 * 1024;
pub const TCP_TX_BUF_LEN: usize = 64 * 1024;
pub const UDP_RX_BUF_LEN: usize = 64 * 1024;
pub const UDP_TX_BUF_LEN: usize = 64 * 1024;
pub const RAW_RX_BUF_LEN: usize = 64 * 1024;
pub const RAW_TX_BUF_LEN: usize = 64 * 1024;
pub const LISTEN_QUEUE_SIZE: usize = 512;
pub const SOCKET_BUFFER_SIZE: usize = 64;
pub const ETHERNET_MAX_PENDING_PACKETS: usize = 128;
pub const DEVICE_TX_QUEUE_SIZE: usize = 128;
```

`ETHERNET_MAX_PENDING_PACKETS` 取 128 而非 32，是为了容纳应用启动时多个并发 TCP 连接的首个 SYN 突发，以及长连接在 `NEIGHBOR_TTL` 过期后重新进入 ARP-pending 队列的 burst（见源码注释 [consts.rs#L14-L27](net/ax-net/src/consts.rs#L14-L27)）。

## NetworkConfig

定义在 [net/ax-net/src/config.rs#L80-L97](net/ax-net/src/config.rs#L80-L97)：

```rust
#[derive(Debug, Clone, Default)]
pub struct NetworkConfig {
    /// Per-interface configuration.
    pub interfaces: Vec<InterfaceConfig>,
    /// DNS servers used when no interface-level DNS server is available.
    pub default_dns_servers: Vec<Ipv4Addr>,
}

#[derive(Debug, Clone)]
pub struct InterfaceConfig {
    pub name: String,
    pub match_by: InterfaceMatcher,
    pub static_ip: Option<StaticIpConfig>,
    pub dhcp: bool,
    pub metric: u32,
    pub dns_servers: Vec<Ipv4Addr>,
}
```

`NetworkConfig` 由 `ax-runtime` 构造并传入 `ax_net::init_network()`。`ax-net` 不再读取旧的全局 `AX_IP`、`AX_GW`、`AX_PREFIX_LEN`、`AX_DNS` 单网口语义。

## InterfaceMatcher

显式接口配置通过 `InterfaceMatcher` 匹配真实设备，定义在 [config.rs#L72-L76](net/ax-net/src/config.rs#L72-L76)：

```rust
#[derive(Debug, Clone)]
pub enum InterfaceMatcher {
    ByOrder(usize),
    ByMac(EthernetAddress),
    ByDriverName(String),
}
```

匹配规则：

- `ByOrder(0)` 匹配第一个发现的 Ethernet 设备，通常命名为 `eth0`。
- `ByMac(mac)` 用 MAC 地址匹配设备。
- `ByDriverName(name)` 用 driver 暴露的设备名匹配。
- 显式配置必须匹配唯一设备。
- 未显式配置的 Ethernet 设备默认启用 DHCP。

匹配逻辑在 `find_interface_config()`（[lib.rs#L328-L355](net/ax-net/src/lib.rs#L328-L355)），多配置匹配同一设备会 panic。

## 静态 IPv4

`StaticIpConfig` 定义在 [config.rs#L100-L104](net/ax-net/src/config.rs#L100-L104)：

```rust
#[derive(Debug, Clone)]
pub struct StaticIpConfig {
    pub ip: Ipv4Addr,
    pub prefix_len: u8,
    pub gateway: Ipv4Addr,
}
```

静态接口初始化时会：

- 设置接口 IPv4 地址。
- 安装直连路由。
- 如果 gateway 有效，安装默认路由。
- 将接口级 DNS server 记录为静态 DNS 来源。

配置错误会在初始化时 panic，包括：

- 接口名为保留名 `lo`。
- `dhcp = true` 且 `static_ip.is_some()`。
- 静态 IP 为 unspecified。
- prefix 大于 32。
- gateway 可以为 unspecified，表示不安装默认路由。
- DNS server 为 unspecified。
- 显式配置没有匹配任何设备。
- 接口名冲突。

这些校验集中在 `init_network()` 开头（[lib.rs#L127-L170](net/ax-net/src/lib.rs#L127-L170)）。

## DHCP

未显式配置的 Ethernet 接口默认 DHCP。显式配置也可以设置 `dhcp = true`。

DHCP 行为：

- 每个 DHCP 接口独立 `DhcpState`。
- DHCP packet 按 ingress `InterfaceId` 分发。
- DHCP ACK 更新对应接口 IPv4、gateway、路由和 DNS。
- 启动等待只要求任一 DHCP 接口成功配置，避免一个断开的接口阻塞系统启动。
- DHCP 租约续期、过期回收和地址冲突检测仍属于后续增强。

## DNS 配置

DNS server 来源分三类：

| 来源 | 说明 |
| --- | --- |
| DHCP | DHCP ACK 下发，绑定来源接口和 metric |
| Static | `InterfaceConfig::dns_servers` |
| Fallback | `NetworkConfig::default_dns_servers` |

内部记录为 `DnsServerEntry`，对外 `dns_servers()` 返回按 metric 和接口 ID 排序后的去重地址列表。

DNS 查询时会先检查 DNS server 是否可路由，不可路由则尝试下一个 server。

## 配置示例

```rust
use alloc::{string::ToString, vec};
use core::net::Ipv4Addr;

use ax_net::{InterfaceConfig, InterfaceMatcher, NetworkConfig, StaticIpConfig};

let config = NetworkConfig {
    interfaces: vec![
        InterfaceConfig {
            name: "eth0".to_string(),
            match_by: InterfaceMatcher::ByOrder(0),
            static_ip: Some(StaticIpConfig {
                ip: Ipv4Addr::new(10, 0, 2, 15),
                prefix_len: 24,
                gateway: Ipv4Addr::new(10, 0, 2, 2),
            }),
            dhcp: false,
            metric: 100,
            dns_servers: vec![Ipv4Addr::new(10, 0, 2, 3)],
        },
        InterfaceConfig {
            name: "eth1".to_string(),
            match_by: InterfaceMatcher::ByOrder(1),
            static_ip: Some(StaticIpConfig {
                ip: Ipv4Addr::new(192, 168, 100, 10),
                prefix_len: 24,
                gateway: Ipv4Addr::new(192, 168, 100, 1),
            }),
            dhcp: false,
            metric: 200,
            dns_servers: vec![],
        },
    ],
    default_dns_servers: vec![],
};
```

## 接口命名与 ID 分配

> 以下为运行时常量与默认值。

`InterfaceId` 数值的分配规则：

| InterfaceId | 接口 | 说明 |
| --- | --- | --- |
| 0 | `TX_INTERFACE_PLACEHOLDER` | 内部占位符（[router.rs](net/ax-net/src/router.rs#L76)），不出现在任何 public API 中 |
| 1 | `lo` | Loopback，固定 ID |
| 2+ | `eth0`, `eth1`, ... | Ethernet 设备，按发现顺序（`net_devs.drain(..)`）递增 |

默认命名 `"eth{order}"`，但配置中的 `name` 字段会覆盖默认名称。接口名不允许冲突，且 `"lo"` 为保留名。

## 路由 Metric 行为

- 直连路由的 metric 与接口配置相同。
- 默认路由的 metric 与接口配置相同。
- 路由表排序：最长前缀匹配 > 低 metric 优先 > 同 metric 稳定插入顺序。
- 未显式配置的 Ethernet 接口默认启用 DHCP，metric 默认为 100；fallback DNS 来源使用 `u32::MAX`，因此会排在接口级 DNS 之后。
- 同一目的地址存在多个匹配路由时，优先选择 metric 较低且接口 `UP` 的路由。

## TCP Keep-Alive 默认值

TCP keep-alive 相关常量定义在 [tcp.rs](net/ax-net/src/tcp.rs#L45-L52)：

```rust
const TCP_KEEPIDLE_DEFAULT_SECS: u32 = 7200;   // 2小时空闲后开始探测
const TCP_KEEPINTVL_DEFAULT_SECS: u32 = 75;    // 探测间隔 75s
const TCP_KEEPCNT_DEFAULT: u32 = 9;            // 最多 9 次探测
const TCP_USER_TIMEOUT_DEFAULT_MS: u32 = 0;    // 默认不限制
```

上限约束：

```rust
const TCP_KEEPIDLE_MAX_SECS: u32 = 32767;      // ~9.1 小时
const TCP_KEEPINTVL_MAX_SECS: u32 = 32767;
const TCP_KEEPCNT_MAX: u32 = 127;
```

`TCP_USER_TIMEOUT_DEFAULT_MS = 0` 表示使用 smoltcp 内置默认超时。

## TCP_INFO 默认值

`TcpInfo` 快照中的默认/估计值（[tcp.rs](net/ax-net/src/tcp.rs#L48-L52)）：

```rust
const TCP_INFO_DEFAULT_MSS: u32 = 1460;           // 1500 - 20(IP) - 20(TCP)
const TCP_INFO_DEFAULT_PMTU: u32 = 1500;
const TCP_INFO_INITIAL_RTO_MICROS: u32 = 1_000_000;  // 1s 初始 RTO
const TCP_INFO_DEFAULT_REORDERING: u32 = 3;
```

## Ephemeral Port 范围

自动端口分配（TCP/UDP bind 时 `port == 0`）从 `49152`（IANA 动态端口范围下界，`0xC000`）开始，逐个尝试直到找到未占用端口。

## 超时与 Poll 间隔

| 常量 | 位置 | 值 | 说明 |
| --- | --- | --- | --- |
| `DNS_DEFAULT_TIMEOUT` | [lib.rs#L488](net/ax-net/src/lib.rs#L488) | 5s | DNS 查询超时 |
| `DHCP_BOOTSTRAP_ATTEMPTS` | [lib.rs#L107](net/ax-net/src/lib.rs#L107) | 200 | DHCP bootstrap 最大重试次数 |
| `DHCP_BOOTSTRAP_POLL_INTERVAL` | [lib.rs#L108](net/ax-net/src/lib.rs#L108) | 10ms | DHCP bootstrap poll 间隔 |
| `DHCP_MAX_RETRY_SHIFT` | [service.rs#L283](net/ax-net/src/service.rs#L283) | 4 | DHCP 指数退避最大位移（最大 16s） |
| `NEIGHBOR_TTL` | [ethernet.rs#L118](net/ax-net/src/device/ethernet.rs#L118) | 300s | ARP neighbor 缓存 TTL |
| `ARP_REQUEST_RETRY` | [ethernet.rs#L119](net/ax-net/src/device/ethernet.rs#L119) | 1s | ARP 请求重试间隔 |
| Idle poll interval | [lib.rs#L438](net/ax-net/src/lib.rs#L438) | 100ms | net-poll worker 空闲轮询间隔 |

## Router 缓冲区配置

| 名称 | 位置 | 容量 |
| --- | --- | --- |
| `Router::rx_buffer` | [router.rs#L326](net/ax-net/src/router.rs#L326) | `SOCKET_BUFFER_SIZE` 个 MTU 槽位 × 1500 字节 |
| `Router::tx_buffer` | [router.rs#L330](net/ax-net/src/router.rs#L330) | 同上 |
| `RouterQueues::rx` | [router.rs](net/ax-net/src/router.rs) | `SOCKET_BUFFER_SIZE` 个 inline `RxPacket`（每包最多 MTU） |
| `DeviceHandle::tx_queue` | [router.rs](net/ax-net/src/router.rs) | `DEVICE_TX_QUEUE_SIZE` 个 inline `TxPacket`（每包最多 MTU） |
| `EthernetDevice::pending_packets` | [ethernet.rs#L123](net/ax-net/src/device/ethernet.rs#L123) | `ETHERNET_MAX_PENDING_PACKETS`（128）个 IP 包 |

`LoopbackDevice` 不维护独立 buffer。普通 smoltcp loopback TX 在 `Router::dispatch()` 中直接注入 `Router::rx_buffer`；`send_on_device()` 的 loopback 特殊发包才进入共享 `RouterQueues::rx`，且同样使用 inline `QueuedPacket`。

## Unix Stream 缓冲区

Unix stream transport 使用 `ringbuf::HeapRb<u8>` 作为双向通道：

```rust
const BUF_SIZE: usize = 64 * 1024;     // 64 KiB 每方向
```

一对 socketpair 的总内存开销为 `2 × 64 KiB = 128 KiB`。
