---
sidebar_position: 9
sidebar_label: "配置参考"
---

# 配置参考

`ax-net` 的配置由结构化 `NetworkConfig`、Cargo feature、运行时设备注册参数和一组集中常量组成。配置目标是明确表达每个接口的意图，避免旧式单网口全局变量和隐式 `eth0` 假设。

核心源码：

| 配置域 | 源码 |
| --- | --- |
| feature | `Cargo.toml` |
| 接口配置模型 | `config.rs` |
| 初始化解析与校验 | `lib.rs` `init_network()` |
| 缓冲区/队列常量 | `consts.rs` |
| TCP keepalive / TCP_INFO 默认值 | `tcp.rs` |
| DHCP/DNS 默认值 | `lib.rs`, `service.rs` |
| Ethernet ARP 默认值 | `device/ethernet.rs` |

这些源码锚点分别拥有编译 feature、启动输入、运行期常量和协议默认值，配置变更应在对应所有者处完成。跨越多行的调整还需要验证 `init_network()` 如何把输入转换为控制面和设备运行时，而不是只修改文档中的示例值。

## 1. 构建配置

构建配置决定是否启用可选协议族。基础 TCP、UDP、raw、Unix domain socket、DNS、DHCP 和 Ethernet 能力不需要额外 feature。

### 1.1 Cargo Feature

Cargo feature 决定哪些 transport 和集成适配会进入编译结果，并直接影响 `ax-net` 的依赖边界。维护 feature 组合时需要同时核对 `Cargo.toml`、条件编译模块和上层 runtime，因为启用一个协议后端并不自动提供对应平台设备或 syscall ABI。

```toml
[features]
host-test = ["ax-hal/host-test", "axpoll/host-test", "ax-sync/host-test", "ax-task/host-test"]
vsock = ["dep:rdif-vsock"]
```

feature 声明只控制编译依赖与条件模块，实际对外能力还取决于 runtime 是否提供相应设备和初始化调用。下表进一步说明每个 feature 在测试或产品构建中的作用范围，避免把 `host-test` 带入 bare-metal 配置，或在没有 vsock 设备时假定后端可用。

| feature | 作用 |
| --- | --- |
| `host-test` | 在宿主机启用 ArceOS 调度、同步和 poll 测试后端，并开放 `tests/std.rs` 集成测试；不用于 bare-metal 产品构建 |
| `vsock` | 启用 `rdif-vsock` 依赖、AF_VSOCK socket backend 和 vsock device 初始化 |

启用 `vsock` 后导出：

- `init_vsock(vsock_devs)`。
- `vsock` 模块。
- `Socket::Vsock` 变体。
- `VsockDevice` / `VsockDeviceList` 类型别名。

导出项列表说明 `vsock` feature 同时改变初始化 API 和 `Socket` 枚举，因此上层必须在相同条件下编译调用代码。smoltcp feature 属于 IP 协议核心的固定能力，和这个可选 transport 边界分开维护。

### 1.2 smoltcp 能力

`ax-net` 固定启用的 smoltcp feature 决定协议核心能够编译哪些 socket 与介质能力，但不代表外围设备和 Linux ABI 已完整接入。维护这组能力时需要同时检查 `Service`、`Router` 和具体 backend，下面列出的 feature 才是当前构建实际依赖的集合。

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
- `auto-icmp-echo-reply`
- `iface-max-addr-count-8`（允许 `Interface` 同时保存最多 8 个 IP 地址，支撑 loopback + 多 Ethernet 静态地址）

此外 `Cargo.toml` 中注释保留了 `fragmentation-buffer-size` / `reassembly-buffer-size` 等分片/重组能力，但当前未启用。

Router 对 smoltcp 暴露 `Medium::Ip`，Ethernet frame 处理在 `EthernetDevice` 中完成。

## 2. 启动配置模型

启动配置通过 `NetworkConfig` 传入 `init_network()`。它描述“哪些设备应该成为哪些接口，以及接口如何获得 IPv4/DNS/metric”。

### 2.1 网络配置

`NetworkConfig` 是启动阶段的顶层输入，按接口顺序或匹配规则组织静态地址、DHCP、metric 与 DNS 来源。`init_network()` 会先校验这一结构再构造全局控制面，因此错误配置必须在发布 `SERVICE` 前失败，不能留给 queue executor 运行时猜测。

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

顶层配置列表定义全局 fallback 和接口集合，单接口的名字、匹配与地址策略由 `InterfaceConfig` 进一步收敛。这样全局默认不会覆盖已经明确配置的设备角色。

### 2.2 接口配置

`InterfaceConfig` 描述单个发现设备应采用的名字、匹配条件和 IPv4 策略，并把路由优先级与 DNS 来源绑定到同一接口。该结构用于产生 `NetInterface` 和 `RouteTable` 初始规则，字段默认值变化会直接改变多网卡启动行为。

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

字段表说明 `InterfaceConfig` 把身份匹配、地址策略和路由偏好作为一个接口角色维护。下一节的 `InterfaceMatcher` 只负责找到设备，不应承载 IP 或 DNS fallback 等配置语义。

### 2.3 接口匹配

`InterfaceMatcher` 决定一项接口配置如何关联到实际 driver，可使用发现顺序、MAC 或驱动名等稳定属性。匹配结果必须唯一且可诊断，否则同一配置可能被多个设备消费，或设备落入默认 DHCP 策略而掩盖部署错误。

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

匹配规则只回答“配置属于哪个设备”，其优先级和唯一性在初始化校验中确定。设备匹配成功后，静态地址结构才决定本地 CIDR、gateway 和 DNS 等网络属性。

### 2.4 静态地址配置

`StaticIpConfig` 把 CIDR、可选 gateway 和 DNS server 作为一个完整静态网络角色提交。`Router::ipv4_rules()` 根据这些字段生成 connected/default route，因而 prefix、gateway 和本地地址必须在初始化校验阶段保持同一子网语义。

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

## 3. 初始化校验

`init_network()` 对配置执行 fail-fast 校验。启动阶段配置错误直接 panic，避免系统在半初始化网络状态下运行。

### 3.1 校验规则

初始化校验用于在后台 Worker 启动前拒绝歧义配置，覆盖接口匹配、地址前缀、重复名字和静态路由等约束。下表把输入字段与失败条件对应起来，维护 `NetworkConfig::validate()` 时应让错误信息保留具体接口和冲突来源。

| 配置项 | 规则 |
| --- | --- |
| 接口名 | 不能是 `lo`，不能重复 |
| `static_ip` + `dhcp` | 不能同时启用 |
| 静态 IP | 不能是 `0.0.0.0` |
| prefix | 不能大于 32 |
| gateway | 可以是 `0.0.0.0`，表示无默认路由 |
| DNS server | 不能是 `0.0.0.0` |
| matcher | 每个显式配置必须匹配唯一设备 |

校验表中的规则都在全局状态发布前执行，因此失败不会留下部分接口或后台 Worker。通过校验后，未命中显式配置的设备才会进入下一节描述的统一默认策略。

### 3.2 默认策略

未显式匹配 `InterfaceConfig` 的 Ethernet 设备会落入确定的默认策略，而不是被忽略或猜测静态地址。该策略由初始化配置逻辑统一生成，保证新增普通 NIC 至少能以 DHCP 角色进入接口 registry，并具有可预测的名字和 metric。

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

默认策略确保未配置 NIC 仍能进入控制面，但不会凭空提供 fallback DNS 或静态 gateway。DNS 配置必须保留来源和 metric，才能与这些 DHCP 接口的动态 server 正确合并。

## 4. DNS 配置

DNS server registry 同时接收静态接口配置、DHCP lease 和 fallback 来源，并保留来源接口与 metric。`NetControl::dns_servers()` 会按路由优先级排序后去重，因此解析顺序与多网卡出口策略一致，而不是简单按配置出现顺序覆盖。

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

调用链说明 server 排序与可达性检查是两个步骤：metric 决定尝试顺序，route lookup 决定某个地址当前是否可用。路由 Metric 同时影响这两类决策以及普通 socket 出口，因而需要统一解释。

## 5. 路由 Metric

`RouteTable` 使用前缀长度、metric 和插入顺序形成稳定的选路优先级，socket 查询与 `Router::dispatch()` 读取同一份规则。配置 metric 时应把数值视为接口出口偏好；它不会替代源地址和 `DeviceBinding` 对候选路由的过滤。

1. 最长前缀匹配。
2. 低 metric 优先。
3. 同 metric 按插入顺序稳定选择。

每个静态或 DHCP IPv4 接口会生成：

- 直连路由：`interface_cidr -> dev`。
- 默认路由：`0.0.0.0/0 -> gateway`，仅 gateway 存在时安装。

多网口场景下，metric 用于选择默认路由和 DNS server 优先级；socket 已绑定接口时，route lookup 还会叠加 `DeviceBinding` 过滤。

## 6. 设备与运行期配置

物理设备只能在网络启动阶段一次性发布。runtime 收集全部
`NetworkDeviceInput { name, device, irq_sources, tx_queue_discipline }`，
`NetworkRuntimeBuilder` 完成 affinity domain、worker pin、DMA refill、IRQ
registration/rearm 后，`init_network()` 才分配接口 ID 并发布 `Service`。启动后
新增/删除物理 NIC、无 IRQ 设备和周期 poll 模式不在当前配置面中。

### 6.1 TX queue discipline

`tx_queue_discipline` 是每个设备必须显式选择的 protocol TX 策略，没有 `Default`：

```rust
pub enum TxQueueDiscipline {
    NoQueue,
    Fifo { max_frames: NonZeroUsize },
}
```

- `NoQueue` 对应 Linux `noqueue` 的边界：只尝试直接提交，设备 busy 时立即返回
  `Again`，不保留 frame，也不分配 backlog。
- `Fifo` 对应 packet-limited FIFO qdisc：设备 busy 后按提交顺序保留 frame，达到
  `max_frames` 后拒绝新 frame；backing storage 从零容量开始，在第一次入队时按需分配。

当前一个 `QueueFramePort` 对应一个设备，所以 discipline 也按设备所有；它不是全局
queue，也不表示已经实现 per-hardware-queue qdisc。`axruntime` 当前为生产网卡显式
选择 `Fifo { max_frames: 64 }`，保持短暂 TX token 耗尽时的重试语义。该值不属于
`NetworkConfig` 的 IP/DNS 配置，也不能与驱动 `QueueConfig::ring_size`、AIC
`aic,queue-size` 或 DMA token 数量互相替代。

### 6.2 Wi-Fi startup transaction

Wi-Fi 驱动可以在 owned `WifiControl` 中提供一个 `startup_transaction()`。它不是
probe 期间的直接 SDIO 调用：builder 等待 queue worker affinity-ready、注册并 enable
固定 CPU IRQ 后，把 transaction 提交给相同 owner executor；transaction 成功和 MAC
刷新完成后才发布接口。SoftAP 的初始静态地址与 DHCP server policy 同步写入 protocol
配置。

### 6.3 运行期 Wi-Fi transaction

`reconfigure_wifi(ifname, WifiTransaction)` 只改变已发布 Wi-Fi 设备的 link policy。
owner executor quiesce 所属 group、执行 STA connect/disconnect 或 open AP、原子
rearm；随后唯一 protocol executor 提交 DHCP/static-address/DHCP-server 变化。该 API
不能新增设备，也不能让调用者直接借用 SDIO/MMIO control handle。

### 6.4 运行期 IPv4 地址

已注册的 Ethernet 接口还可通过 `set_interface_ipv4()` / `remove_interface_ipv4()` 修改地址。当前控制面有意保持单地址模型：

- 每个接口最多一个 IPv4；已有地址时再次设置返回 `AlreadyExists`。
- 设置操作验证 `prefix_len <= 32`，关闭该接口 DHCP，安装 IPv4 地址和 connected route。
- API 没有 gateway 参数，因此不会增加 default route；需要默认网关的场景仍应使用启动期 `StaticIpConfig` 或 DHCP。
- 删除必须精确匹配现有 IP/prefix，并会移除该接口的地址、路由、DHCP 状态和 DHCP DNS。

StarryOS 的 `RTM_NEWADDR` / `RTM_DELADDR` 直接映射到这两个入口。

## 7. 资源预算

缓冲区和队列常量集中定义在 `consts.rs`。这些值共同决定嵌入式目标上的默认内存占用。

### 7.1 Socket 缓冲区

`SOCKET_BUFFER_SIZE` 影响 Router 协议侧 packet buffer 以及多个 socket 后端的默认容量，是内存预算和吞吐之间的全局权衡。修改该常量时需要区分字节流缓冲区与 packet metadata 容量，不能仅根据 MTU 线性推断所有队列占用。

```rust
pub const TCP_RX_BUF_LEN: usize = 64 * 1024;
pub const TCP_TX_BUF_LEN: usize = 64 * 1024;
pub const UDP_RX_BUF_LEN: usize = 64 * 1024;
pub const UDP_TX_BUF_LEN: usize = 64 * 1024;
pub const RAW_RX_BUF_LEN: usize = 64 * 1024;
pub const RAW_TX_BUF_LEN: usize = 64 * 1024;
```

这些是每个 socket 的默认协议缓冲区大小。

### 7.2 设备队列

硬件 RX/TX queue 与 queue/protocol SPSC 的容量来自 driver `QueueConfig`，不由
`ax-net::consts` 重复定义。这样每个 poll group 的 DMA token 数、descriptor 深度与
跨 CPU ring 容量保持一致。

```rust
pub const STANDARD_MTU: usize = 1500;
pub const SOCKET_BUFFER_SIZE: usize = 64;
pub const ETHERNET_MAX_PENDING_PACKETS: usize = 128;
pub const LISTEN_QUEUE_SIZE: usize = 512;
```

这些常量同时包含 MTU 字节上限、packet slot 数和监听 backlog 上限，不能按同一单位比较。下表把每个值映射到具体所有者和拥塞行为，便于评估修改会增加全局、每设备还是每 listener 的资源占用。

| 常量 | 含义 |
| --- | --- |
| `STANDARD_MTU` | Router 和 Ethernet 默认 MTU |
| `SOCKET_BUFFER_SIZE` | Router RX/TX smoltcp-facing packet buffer 槽位数 |
| `ETHERNET_MAX_PENDING_PACKETS` | ARP resolution pending packet 上限 |
| `LISTEN_QUEUE_SIZE` | TCP listen backlog clamp 上限 |

protocol frame port 使用预分配 SPSC move `DmaBuffer`；ring full 时 token 保留在
`pending_*`，不会产生额外无界 queue。设备级 TX `Fifo` 是另一层明确有界且按需分配
的 frame backlog；`NoQueue` 不建立该 backlog。Ethernet ARP pending queue 保存二层帧，
单槽容量为 `STANDARD_MTU + 14`；因此估算 pending 内存时不能只按 1500 B 计算。
更完整的拷贝边界、队列满行为和内存预算见[内存与队列](memory.md)。

### 7.3 Unix 流缓冲区

Unix stream transport 使用 `ringbuf::HeapRb<u8>` 作为每个方向的字节流缓冲区，两组 ring 共同组成全双工连接。容量直接影响本地 socket 的背压和每连接堆内存占用，但不占用 smoltcp `SocketSet` 或 Router packet queue。

```rust
const BUF_SIZE: usize = 64 * 1024;
```

socketpair 两个方向各 64 KiB，总计约 128 KiB 数据缓冲区。

## 8. 协议默认值

协议默认值分布在 TCP、DHCP、DNS、ARP 和端口分配模块中，控制未显式设置选项时的超时、重试和资源边界。修改这些值会改变用户可见时序或启动行为，因而需要结合对应状态机和集成测试评估，而不能只把它当作常量整理。

### 8.1 TCP Keepalive

TCP keepalive 默认值由 socket 后端在启用选项时转换为 smoltcp 定时参数，并影响失活连接的探测频率。配置只在真正启用 keepalive 后生效；仅保存用户值而未更新 socket 状态会造成 `getsockopt()` 与线上行为不一致。

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

### 8.2 TCP 状态信息

TCP 状态信息常量用于把 smoltcp 连接状态和计时数据转换为 Linux `TCP_INFO` 可观察字段。该映射是 ABI 兼容层而非拥塞控制实现，新增字段时需要区分精确来源、近似值和当前无法提供的统计。

```rust
const TCP_INFO_DEFAULT_MSS: u32 = 1460;
const TCP_INFO_DEFAULT_PMTU: u32 = 1500;
const TCP_INFO_INITIAL_RTO_MICROS: u32 = 1_000_000;
const TCP_INFO_DEFAULT_REORDERING: u32 = 3;
```

这些值用于填充 `TcpInfo` 中无法直接从 smoltcp 获得或需要 Linux 兼容默认值的字段。

### 8.3 控制协议参数

DHCP、DNS 与 ARP 参数决定控制协议的超时、重试和缓存上限，会同时影响启动等待、运行期恢复及内存占用。下表列出的常量属于行为契约，调整时应结合唯一 protocol executor 的 deadline 驱动和 generation 完成语义验证；queue executor 不提供任何周期兜底。

| 常量 | 值 | 含义 |
| --- | --- | --- |
| `DNS_DEFAULT_TIMEOUT` | 5s | `dns_query()` 默认超时 |
| `DHCP_BOOTSTRAP_ATTEMPTS` | 200 | DHCP bootstrap 最大轮数 |
| `DHCP_BOOTSTRAP_POLL_INTERVAL` | 10ms | DHCP bootstrap 每轮 sleep |
| `DHCP_MAX_RETRY_SHIFT` | 4 | DHCP 指数退避上限，最大 16s |
| `DHCP_SERVER_LEASE_SECS` | 86400s | 内置 SoftAP DHCP server 返回的固定租约时间 |
| `NEIGHBOR_TTL` | 300s | ARP neighbor cache TTL |
| `ARP_REQUEST_RETRY` | 1s | ARP request 重试间隔 |

控制协议常量共同决定超时与缓存上限，但各自状态机仍拥有独立生命周期。临时端口分配属于 socket 端口仲裁，不受 DHCP、DNS 或 ARP 定时参数影响，因此单独维护其范围与冲突规则。

### 8.4 临时端口

TCP 和 UDP 的 `bind(0)` 从 IANA dynamic/private 范围下界开始分配，并通过各自的全局端口仲裁状态避免与现有绑定冲突。分配器必须覆盖 wildcard、具体地址和 reuse 规则，不能只检查 smoltcp socket 当前是否已经建立连接。

```rust
const PORT_START: u16 = 0xc000; // 49152
const PORT_END: u16 = 0xffff;
```

TCP 分配会避开任何已 listen 或已 bind 的同端口；UDP 分配使用 UDP bind side table 检查 wildcard/specific-address 冲突。

## 9. 配置示例

配置示例展示 `NetworkConfig` 如何把接口身份、地址来源、metric 和 DNS 组合成可验证的启动输入。示例重点是字段之间的约束和生成的控制面结果，实际部署仍应使用稳定的 MAC 或驱动属性匹配设备，而不是照搬发现顺序。

### 9.1 双静态网口

双静态网口示例展示如何用不同 metric 建立主备默认出口，同时保留每个接口的 connected route。示例中的匹配器和 DNS 来源应与实际设备稳定属性绑定，避免发现顺序变化后把地址角色交换到另一块网卡。

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

该示例最终产生两个 connected route 和按 metric 排序的默认路由/DNS 候选，具体选择仍由共享 `RouteTable` 在运行时完成。下一示例改用 DHCP 来源，展示动态 DNS 与最低优先级 fallback 的组合。

### 9.2 DHCP 主接口与备用 DNS

DHCP 主接口可以从 lease 获得地址、gateway 和 DNS，而备用 DNS 仅在动态来源缺失或排序靠后时参与解析。该组合依赖 `DnsServerEntry` 的来源接口和 metric 排序，因此 fallback 不应被描述为无条件覆盖 DHCP server。

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

DHCP 成功后，动态 server 以接口 metric 排在 fallback 前；没有可用 lease 时，fallback 仍需通过 route lookup 才能使用。配置建议将这类来源与身份约束总结为部署时的检查规则。

## 10. 配置建议

配置选择应围绕接口身份、路由优先级和资源预算建立明确约束，而不是依赖发现顺序或隐式 fallback。以下建议对应 `InterfaceMatcher`、route metric、队列常量和运行期配置 API，可作为部署配置评审时的最低检查项。

- 多网口默认路由通过 metric 控制，主出口使用较小 metric。
- `gateway = 0.0.0.0` 用于只有直连路由的静态接口。
- 需要稳定接口名时优先使用 `ByMac` 或 `ByDriverName`，避免依赖探测顺序。
- 通过 `ipv4_config(name)` 查询指定接口地址，避免固定 `eth0` 假设。
- 提高队列常量时应按“每 socket”或“每设备”的乘数估算内存，而不是只看单个 buffer。

这些建议共同要求配置来源可追踪、接口身份稳定且资源变化可量化。若部署需求超出单 IPv4 运行期 API，应先扩展 `NetworkConfig` 或控制面提交模型，再让 ABI 层调用新的稳定边界。
