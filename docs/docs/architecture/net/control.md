---
sidebar_position: 3
sidebar_label: "控制面"
---

# 控制面

控制面是 `ax-net` 的接口管理和路由决策层，与数据面完全分离。所有接口查询、路由查找和 DNS 查询通过 `NetControl` 进行，只读操作仅持 `RwLock` 读锁，不进入设备锁也不推进 smoltcp poll。

核心类型定义在 [config.rs](net/ax-net/src/config.rs)，逻辑在 [service.rs](net/ax-net/src/service.rs) 和 [router.rs](net/ax-net/src/router.rs)。

## 接口标识

`InterfaceId(u32)` 贯穿整个网络栈，也是 StarryOS Linux ABI 的 `ifindex` 来源：

```rust
// net/ax-net/src/config.rs
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct InterfaceId(u32);

impl InterfaceId {
    pub const LOOPBACK: Self = Self(1);
    pub const fn new(raw: u32) -> Self { Self(raw) }
    pub const fn get(self) -> u32 { self.0 }
    pub const fn to_linux_ifindex(self) -> i32 { self.0 as i32 }
    pub const fn from_linux_ifindex(ifindex: i32) -> Option<Self> {
        if ifindex > 0 { Some(Self(ifindex as u32)) } else { None }
    }
}
```

- `InterfaceId::LOOPBACK == 1`，固定对应 `lo`。
- Ethernet 接口从 `2` 开始，`InterfaceId::new((order as u32) + 2)` 递增。
- `InterfaceId(0)` 是内部占位符 `TX_INTERFACE_PLACEHOLDER`，不对外暴露。

`InterfaceInfo` 是对外只读快照，由内部 `NetInterface` 通过 `to_info()` 转换：

```rust
// config.rs
pub struct InterfaceInfo {
    pub id: InterfaceId,
    pub name: String,
    pub kind: InterfaceKind,           // Loopback | Ethernet
    pub mac: Option<EthernetAddress>,
    pub ipv4: Option<Ipv4InterfaceConfig>,
    pub mtu: usize,
    pub flags: InterfaceFlags,         // UP | RUNNING | LOOPBACK | BROADCAST | MULTICAST
    pub metric: u32,
}

// service.rs (crate-internal)
pub(crate) struct NetInterface {
    pub id: InterfaceId,
    pub name: String,
    pub kind: InterfaceKind,
    pub mac: Option<EthernetAddress>,
    pub ipv4: Option<Ipv4Cidr>,        // smoltcp native type
    pub gateway: Option<Ipv4Address>,
    pub mtu: usize, pub metric: u32,
    pub flags: InterfaceFlags,
}
```

## NetControl

`NetControl` 在 `init_network()` 中构建，早于 `SERVICE` 初始化，通过 `Arc` 共享：

```rust
// service.rs
struct ControlState {
    interfaces: Vec<NetInterface>,
    dns: Vec<DnsServerEntry>,
}

pub struct NetControl {
    state: RwLock<ControlState>,
    pub(crate) routes: SharedRouteTable,  // = Arc<RwLock<RouteTable>>
}

// init_network() 中的构造顺序:
let control = Arc::new(NetControl::new(interfaces, routes, dns));
let mut service = Service::new(router, control.clone());
NET_CONTROL.call_once(|| control);
SERVICE.call_once(|| Mutex::new(service));
```

`state` 使用 `spin::RwLock`，查询只持读锁。`routes` 与 `Service` 共享同一 `SharedRouteTable`。

### 接口查询

返回只读快照，调用者不应假设快照永久有效（DHCP 可能更新地址/路由/DNS）：

```rust
// service.rs
pub fn interfaces(&self) -> Vec<InterfaceInfo> {
    let state = self.state.read();
    state.interfaces.iter().map(NetInterface::to_info).collect()
}

pub fn interface_by_name(&self, name: &str) -> Option<InterfaceInfo> {
    let state = self.state.read();
    state.interfaces.iter().find(|i| i.name == name).map(NetInterface::to_info)
}

pub fn interface_by_id(&self, id: InterfaceId) -> Option<InterfaceInfo> {
    let state = self.state.read();
    state.interfaces.iter().find(|i| i.id == id).map(NetInterface::to_info)
}
```

### 路由决策

`select_route()` 按目的地址查路由，`is_usable` 闭包跳过未 `UP` 的接口：

```rust
// service.rs
pub fn select_route(&self, dst_addr: &IpAddress) -> AxResult<RouteDecision> {
    let state = self.state.read();
    let routes = self.routes.read();
    let route = routes.select_route_if(dst_addr, |interface_id| {
        state.interfaces.iter()
            .find(|i| i.id == interface_id)
            .is_some_and(|i| i.flags.contains(InterfaceFlags::UP))
    }).ok_or_else(|| ax_err_type!(NoSuchDeviceOrAddress,
        format!("no route to destination {dst_addr}")))?;
    Ok(route)
}
```

`RouteDecision` 返回选中的设备索引、源地址、下一跳和 metric：

```rust
// router.rs
#[derive(Debug, Clone, Copy)]
pub struct RouteDecision {
    pub dev: usize,               // Router::devices 索引
    pub interface_id: InterfaceId,
    pub source: IpAddress,        // smoltcp 注入 IP 包头的 src_ip
    pub next_hop: IpAddress,      // 直连时 = dst，有 gateway 时 = gateway
    pub metric: u32,
}
```

### 接口-地址绑定

`local_binding_for()` 遍历接口，将监听地址映射为 `DeviceBinding`：

```rust
// service.rs
pub fn local_binding_for(&self, endpoint: &IpListenEndpoint) -> AxResult<DeviceBinding> {
    match endpoint.addr {
        Some(addr) => {
            let state = self.state.read();
            let bound_if = state.interfaces.iter().find_map(|i| {
                (i.ipv4.is_some_and(|ip| IpAddress::Ipv4(ip.address()) == addr))
                    .then_some(i.id)
            });
            bound_if.map(|id| DeviceBinding { bound_if: Some(id) })
                .ok_or_else(|| ax_err_type!(NoSuchDeviceOrAddress))
        }
        None => Ok(DeviceBinding::default()),  // wildcard → 不绑定接口
    }
}
```

### 运行期更新

`commit_interface_update()` 在 DHCP ACK 后以事务方式更新接口状态、DNS 和路由：

```rust
// service.rs
fn commit_interface_update(&self, update: &NetworkStateUpdate,
                            routes: Vec<crate::router::Rule>) {
    let mut state = self.state.write();
    // 1. 更新接口 IPv4/gateway
    if let Some(i) = state.interfaces.iter_mut()
        .find(|i| i.id == update.interface_id)
    {
        i.ipv4 = update.ipv4;
        i.gateway = update.gateway;
    }
    // 2. 替换该接口同来源的 DNS 条目
    state.dns.retain(|e| e.interface_id != update.interface_id
                       || e.source != update.dns_source);
    state.dns.extend(update.dns_servers.iter().copied().map(|s| DnsServerEntry {
        server: s, interface_id: update.interface_id,
        metric: update.metric, source: update.dns_source,
    }));
    // 3. 原子替换路由规则
    self.routes.write().replace_ipv4_rules_for_interface(update.interface_id, routes);
}
```

三步在同一写锁内完成，外部不会看到半更新状态。

## RouteTable

`RouteTable` 在 [router.rs](net/ax-net/src/router.rs) 中实现，由 `Arc<RwLock<RouteTable>>`（`SharedRouteTable`）包装，被 `NetControl` 和 `Service` 共享。

```rust
// router.rs
#[derive(Debug)]
pub struct Rule {
    pub filter: IpCidr,             // 目标网段
    pub via: Option<IpAddress>,     // gateway（None = 直连）
    pub dev: usize,                 // Router::devices 索引
    pub interface_id: InterfaceId,
    pub src: IpAddress,             // 源地址
    pub metric: u32,
    pub order: u64,                 // 插入顺序（next_order 自增）
}

pub struct RouteTable {
    rules: Vec<Rule>,
    next_order: u64,
}
```

### 排序策略

`sort_rules()` 在每次 add/replace 后调用：

```rust
fn sort_rules(&mut self) {
    self.rules.sort_by(|a, b| {
        b.filter.prefix_len().cmp(&a.filter.prefix_len())   // 1. 最长前缀优先
            .then_with(|| a.metric.cmp(&b.metric))           // 2. 低 metric 优先
            .then_with(|| a.order.cmp(&b.order))             // 3. 插入顺序稳定
    });
}
```

### 查询

`select_route_if` 带 `is_usable` 闭包，`find()` 返回首个匹配项即优先级最高：

```rust
pub fn select_route_if(&self, dst: &IpAddress,
                        mut is_usable: impl FnMut(InterfaceId) -> bool)
    -> Option<RouteDecision>
{
    self.rules.iter()
        .find(|rule| rule.filter.contains_addr(dst) && is_usable(rule.interface_id))
        .map(|rule| RouteDecision {
            dev: rule.dev, interface_id: rule.interface_id,
            source: rule.src, next_hop: rule.via.unwrap_or(*dst), metric: rule.metric,
        })
}
```

`select_route_for_source` 在 TX dispatch 时额外要求源地址匹配（多宿主场景）：

```rust
pub fn select_route_for_source(&self, dst: &IpAddress, source: &IpAddress)
    -> Option<RouteDecision>
{
    self.rules.iter()
        .find(|rule| rule.filter.contains_addr(dst) && &rule.src == source)
        .map(|rule| RouteDecision { /* ... */ })
}
```

### 规则管理

```rust
pub fn add_rule(&mut self, mut rule: Rule) {
    rule.order = self.next_order; self.next_order = self.next_order.saturating_add(1);
    self.rules.push(rule); self.sort_rules();
}

pub fn default_routes(&self) -> Vec<RouteInfo> {
    self.rules.iter()
        .filter(|rule| matches!(rule.filter,
            IpCidr::Ipv4(cidr) if cidr.address() == Ipv4Address::UNSPECIFIED
                                   && cidr.prefix_len() == 0))
        .map(Rule::to_info).collect()
}

pub fn replace_ipv4_rules_for_interface(&mut self, interface_id: InterfaceId,
                                         mut new_rules: Vec<Rule>) {
    self.remove_ipv4_rules_for_interface(interface_id);
    for rule in &mut new_rules {
        rule.order = self.next_order; self.next_order = self.next_order.saturating_add(1);
    }
    self.rules.extend(new_rules); self.sort_rules();
}
```

## DeviceBinding

对应 Linux `SO_BINDTODEVICE`，存储在 `GeneralOptions::bound_if: AtomicU32`：

```rust
// config.rs
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct DeviceBinding {
    pub bound_if: Option<InterfaceId>,
}
```

- `None`：不绑定接口，route decision 选择出接口。
- `Some(id)`：只允许对应接口参与 route/waker/device 选择。

设置/读取通过 `set_device_binding()` / `device_binding()` 操作 `AtomicU32`。

## DNS 注册

DNS server 分三类来源，记录为 `DnsServerEntry`：

```rust
// config.rs
pub(crate) struct DnsServerEntry {
    pub server: Ipv4Address,
    pub interface_id: InterfaceId,
    pub metric: u32,
    pub source: DnsSource,        // Dhcp | Static | Fallback
}
```

| 来源 | 创建时机 | metric |
| --- | --- | --- |
| DHCP | DHCP ACK 后 `commit_interface_update` | 对应接口 metric |
| Static | `init_network()` 从 `InterfaceConfig::dns_servers` | 对应接口 metric |
| Fallback | `init_network()` 从 `NetworkConfig::default_dns_servers` | `u32::MAX` |

`dns_servers()` 排序去重返回纯地址列表：

```rust
// service.rs
pub fn dns_servers(&self) -> Vec<Ipv4Address> {
    let state = self.state.read();
    let mut entries = state.dns.clone();
    entries.sort_by_key(|e| (e.metric, e.interface_id.get(), e.server.octets()));
    let mut servers = Vec::new();
    for e in entries { if !servers.contains(&e.server) { servers.push(e.server); } }
    servers
}
```
