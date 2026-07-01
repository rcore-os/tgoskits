---
sidebar_position: 9
sidebar_label: "配置参考"
---

# 配置参考

`ax-net` 的配置由结构化 `NetworkConfig`、Cargo feature、运行时设备注册参数和一组集中常量组成。配置目标是明确表达每个接口的意图，避免旧式单网口全局变量和隐式 `eth0` 假设。

核心源码：

| 配置域 | 源码 |
| --- | --- |
| feature | [Cargo.toml](net/ax-net/Cargo.toml) |
| 接口配置模型 | [config.rs](net/ax-net/src/config.rs) |
| 初始化解析与校验 | [lib.rs](net/ax-net/src/lib.rs) `init_network()` |
| 缓冲区/队列常量 | [consts.rs](net/ax-net/src/consts.rs) |
| TCP keepalive / TCP_INFO 默认值 | [tcp.rs](net/ax-net/src/tcp.rs) |
| DHCP/DNS 默认值 | [lib.rs](net/ax-net/src/lib.rs), [service.rs](net/ax-net/src/service.rs) |
| Ethernet ARP 默认值 | [device/ethernet.rs](net/ax-net/src/device/ethernet.rs) |

## 构建配置

构建配置决定是否启用可选协议族。基础 TCP、UDP、raw、Unix domain socket、DNS、DHCP 和 Ethernet 能力不需要额外 feature。

### Cargo Feature

```toml
[features]
vsock = ["dep:rdif-vsock"]
```

| feature | 作用 |
| --- | --- |
| `vsock` | 启用 `rdif-vsock` 依赖、AF_VSOCK socket backend 和 vsock device 初始化 |

启用 `vsock` 后导出：

- `init_vsock(vsock_devs)`。
- `vsock` 模块。
- `Socket::Vsock` 变体。
- `VsockDevice` / `VsockDeviceList` 类型别名。

### smoltcp 能力

`ax-net` 固定启用的 smoltcp 能力包括：

- `alloc`
- `log`
- `async`
- `medium-ethernet`
- `medium-ip`
- `proto-ipv4`
- `proto-ipv6`
- `packetmeta-id`（携带 RX 侧 ingress 元数据，供 `rx_meta` 模块传递接收侧 QoS）
- `socket-raw`
- `socket-icmp`
- `socket-udp`
- `socket-tcp`
- `socket-dhcpv4`
- `socket-dns`
- `iface-max-addr-count-8`（允许 `Interface` 同时保存最多 8 个 IP 地址，支撑 loopback + 多 Ethernet 静态地址）

此外 `Cargo.toml` 中注释保留了 `fragmentation-buffer-size` / `reassembly-buffer-size` 等分片/重组能力，但当前未启用。

Router 对 smoltcp 暴露 `Medium::Ip`，Ethernet frame 处理在 `EthernetDevice` 中完成。

## 启动配置模型

启动配置通过 `NetworkConfig` 传入 `init_network()`。它描述“哪些设备应该成为哪些接口，以及接口如何获得 IPv4/DNS/metric”。

### NetworkConfig

```rust
#[derive(Debug, Clone, Default)]
pub struct NetworkConfig {
    pub interfaces: Vec<InterfaceConfig>,
    pub default_dns_servers: Vec<Ipv4Addr>,
}
```

语义：

- `interfaces` 是显式接口配置列表。
- 未显式匹配的 Ethernet 设备按默认策略注册。
- `default_dns_servers` 是 fallback DNS 来源，metric 为 `u32::MAX`。
- `lo` 固定由 `ax-net` 创建，不出现在 `NetworkConfig` 中。

### InterfaceConfig

```rust
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

字段语义：

| 字段 | 语义 |
| --- | --- |
| `name` | 对外接口名，例如 `eth0`、`uplink0` |
| `match_by` | 将配置绑定到某个探测到的 Ethernet driver |
| `static_ip` | 静态 IPv4 配置；与 `dhcp` 互斥 |
| `dhcp` | 是否启用 DHCP client |
| `metric` | 接口路由和接口级 DNS 优先级，值越小越优先 |
| `dns_servers` | 绑定到该接口的静态 DNS server |

### InterfaceMatcher

```rust
#[derive(Debug, Clone)]
pub enum InterfaceMatcher {
    ByOrder(usize),
    ByMac(EthernetAddress),
    ByDriverName(String),
}
```

匹配规则：

- `ByOrder(0)` 匹配第一个发现的 Ethernet device。
- `ByMac(mac)` 按 MAC 地址匹配。
- `ByDriverName(name)` 按 driver 暴露的设备名匹配。
- 同一设备不能被多个配置匹配。
- 每个显式配置必须匹配到一个设备。

### StaticIpConfig

```rust
#[derive(Debug, Clone)]
pub struct StaticIpConfig {
    pub ip: Ipv4Addr,
    pub prefix_len: u8,
    pub gateway: Ipv4Addr,
}
```

静态接口初始化会：

- 将 `ip/prefix_len` 加入 smoltcp `Interface` address list。
- 安装直连路由。
- 如果 `gateway != 0.0.0.0`，安装默认路由。
- 将 `dns_servers` 记录为 `DnsSource::Static`。

`gateway = 0.0.0.0` 表示不安装默认路由。

## 初始化校验

`init_network()` 对配置执行 fail-fast 校验。启动阶段配置错误直接 panic，避免系统在半初始化网络状态下运行。

### 校验规则

| 配置项 | 规则 |
| --- | --- |
| 接口名 | 不能是 `lo`，不能重复 |
| `static_ip` + `dhcp` | 不能同时启用 |
| 静态 IP | 不能是 `0.0.0.0` |
| prefix | 不能大于 32 |
| gateway | 可以是 `0.0.0.0`，表示无默认路由 |
| DNS server | 不能是 `0.0.0.0` |
| matcher | 每个显式配置必须匹配唯一设备 |

### 默认策略

未显式配置的 Ethernet 设备：

- 名称为 `eth{order}`。
- `InterfaceId = order + 2`。
- metric 为 `100`。
- 默认启用 DHCP。
- 无静态接口级 DNS。

loopback：

- 名称为 `lo`。
- `InterfaceId::LOOPBACK == 1`。
- 地址为 `127.0.0.1/8`。
- metric 为 `0`。
- flags 包含 `UP | RUNNING | LOOPBACK`。

## DNS 配置

DNS server 来源分三类，并按 metric 排序后去重。

| 来源 | 创建时机 | metric | interface_id |
| --- | --- | --- | --- |
| DHCP | DHCP ACK | 对应接口 metric | 对应接口 |
| Static | `InterfaceConfig::dns_servers` | 对应接口 metric | 对应接口 |
| Fallback | `NetworkConfig::default_dns_servers` | `u32::MAX` | loopback |

对外 `dns_servers()` 只返回地址列表。DNS 查询时还会过滤不可路由 server：

```text
dns_servers()
  -> sort by (metric, interface_id, server_ip)
  -> dedup
  -> dns_query_timeout()
  -> select_route(server) must succeed
```

## 路由与 Metric

路由表排序策略：

1. 最长前缀匹配。
2. 低 metric 优先。
3. 同 metric 按插入顺序稳定选择。

每个静态或 DHCP IPv4 接口会生成：

- 直连路由：`interface_cidr -> dev`。
- 默认路由：`0.0.0.0/0 -> gateway`，仅 gateway 存在时安装。

多网口场景下，metric 用于选择默认路由和 DNS server 优先级；socket 已绑定接口时，route lookup 还会叠加 `DeviceBinding` 过滤。

## 运行时设备配置

运行期可以注册静态 IPv4 Ethernet 设备，主要用于 Wi-Fi AP 等晚于启动阶段出现的设备。

### NetConfig

```rust
pub struct NetConfig {
    pub name: String,
    pub ip: [u8; 4],
    pub prefix_len: u8,
    pub dhcp_server_client_ip: Option<[u8; 4]>,
    pub dedicated_poll: bool,
}
```

### register_device_with_config

```rust
pub fn register_device_with_config(dev: Box<dyn EthernetDriver>, config: NetConfig);
pub fn wake_net_task_irq();
```

注册过程：

- 根据 `dedicated_poll` 创建普通或 OOB RX `EthernetDevice`。
- 分配新的 `InterfaceId`。
- 将静态 IPv4 加入 smoltcp address list。
- 添加接口 registry、route table 和 worker。
- `dhcp_server_client_ip` 存在时启用内置单客户端 DHCP server。
- 调用 `request_poll()` 让 net-poll worker 看到新状态。

`dedicated_poll = true` 时，驱动侧收到 out-of-band RX 事件后调用 `wake_net_task_irq()`。源码不会创建专门的 OOB poll 线程；该调用发布 IRQ-like pending 状态并唤醒 `net-poll` worker，随后 Router 唤醒对应设备 RX worker 重新 poll 设备。

## 资源预算

缓冲区和队列常量集中定义在 [consts.rs](net/ax-net/src/consts.rs)。这些值共同决定嵌入式目标上的默认内存占用。

### Socket Buffer

```rust
pub const TCP_RX_BUF_LEN: usize = 64 * 1024;
pub const TCP_TX_BUF_LEN: usize = 64 * 1024;
pub const UDP_RX_BUF_LEN: usize = 64 * 1024;
pub const UDP_TX_BUF_LEN: usize = 64 * 1024;
pub const RAW_RX_BUF_LEN: usize = 64 * 1024;
pub const RAW_TX_BUF_LEN: usize = 64 * 1024;
```

这些是每个 socket 的默认协议缓冲区大小。

### Router / Device Queue

```rust
pub const STANDARD_MTU: usize = 1500;
pub const SOCKET_BUFFER_SIZE: usize = 64;
pub const DEVICE_RX_QUEUE_SIZE: usize = 256;
pub const DEVICE_TX_QUEUE_SIZE: usize = 128;
pub const ETHERNET_MAX_PENDING_PACKETS: usize = 128;
pub const LISTEN_QUEUE_SIZE: usize = 512;
```

| 常量 | 含义 |
| --- | --- |
| `STANDARD_MTU` | Router 和 Ethernet 默认 MTU |
| `SOCKET_BUFFER_SIZE` | Router RX/TX smoltcp-facing packet buffer 槽位数 |
| `DEVICE_RX_QUEUE_SIZE` | 所有真实设备共享的 device-to-Router RX queue 槽位数 |
| `DEVICE_TX_QUEUE_SIZE` | 每设备 TX queue 槽位数 |
| `ETHERNET_MAX_PENDING_PACKETS` | ARP resolution pending packet 上限 |
| `LISTEN_QUEUE_SIZE` | TCP listen backlog clamp 上限 |

Router queue 中的 packet 使用 inline `[u8; STANDARD_MTU] + len`，不为每个 queued packet 分配 `Box<[u8]>`。
更完整的拷贝边界、队列满行为和内存预算见[内存与队列](memory.md)。

### Unix Stream Buffer

Unix stream transport 使用 `ringbuf::HeapRb<u8>`：

```rust
const BUF_SIZE: usize = 64 * 1024;
```

socketpair 两个方向各 64 KiB，总计约 128 KiB 数据缓冲区。

## 协议默认值

协议默认值集中在对应模块中，影响兼容行为和超时策略。

### TCP Keepalive

```rust
const TCP_KEEPIDLE_DEFAULT_SECS: u32 = 7200;
const TCP_KEEPINTVL_DEFAULT_SECS: u32 = 75;
const TCP_KEEPCNT_DEFAULT: u32 = 9;
const TCP_USER_TIMEOUT_DEFAULT_MS: u32 = 0;

const TCP_KEEPIDLE_MAX_SECS: u32 = 32767;
const TCP_KEEPINTVL_MAX_SECS: u32 = 32767;
const TCP_KEEPCNT_MAX: u32 = 127;
```

`TCP_USER_TIMEOUT_DEFAULT_MS = 0` 表示使用协议栈默认策略。

### TCP_INFO

```rust
const TCP_INFO_DEFAULT_MSS: u32 = 1460;
const TCP_INFO_DEFAULT_PMTU: u32 = 1500;
const TCP_INFO_INITIAL_RTO_MICROS: u32 = 1_000_000;
const TCP_INFO_DEFAULT_REORDERING: u32 = 3;
```

这些值用于填充 `TcpInfo` 中无法直接从 smoltcp 获得或需要 Linux 兼容默认值的字段。

### DHCP / DNS / ARP

| 常量 | 值 | 含义 |
| --- | --- | --- |
| `DNS_DEFAULT_TIMEOUT` | 5s | `dns_query()` 默认超时 |
| `DHCP_BOOTSTRAP_ATTEMPTS` | 200 | DHCP bootstrap 最大轮数 |
| `DHCP_BOOTSTRAP_POLL_INTERVAL` | 10ms | DHCP bootstrap 每轮 sleep |
| `DHCP_MAX_RETRY_SHIFT` | 4 | DHCP 指数退避上限，最大 16s |
| `DHCP_SERVER_LEASE_SECS` | 86400s | 内置 SoftAP DHCP server 返回的固定租约时间 |
| `NEIGHBOR_TTL` | 300s | ARP neighbor cache TTL |
| `ARP_REQUEST_RETRY` | 1s | ARP request 重试间隔 |

### Ephemeral Port

TCP 和 UDP 的 `bind(0)` 从 IANA dynamic/private port 下界开始分配：

```rust
const PORT_START: u16 = 0xc000; // 49152
const PORT_END: u16 = 0xffff;
```

TCP 分配会避开任何已 listen 或已 bind 的同端口；UDP 分配使用 UDP bind side table 检查 wildcard/specific-address 冲突。

## 配置示例

### 双静态网口

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

### DHCP 主接口 + fallback DNS

```rust
let config = NetworkConfig {
    interfaces: vec![InterfaceConfig {
        name: "eth0".to_string(),
        match_by: InterfaceMatcher::ByOrder(0),
        static_ip: None,
        dhcp: true,
        metric: 100,
        dns_servers: vec![],
    }],
    default_dns_servers: vec![Ipv4Addr::new(8, 8, 8, 8)],
};
```

## 配置建议

- 多网口默认路由通过 metric 控制，主出口使用较小 metric。
- `gateway = 0.0.0.0` 用于只有直连路由的静态接口。
- 需要稳定接口名时优先使用 `ByMac` 或 `ByDriverName`，避免依赖探测顺序。
- 通过 `ipv4_config(name)` 查询指定接口地址，避免固定 `eth0` 假设。
- 提高队列常量时应按“每 socket”或“每设备”的乘数估算内存，而不是只看单个 buffer。
