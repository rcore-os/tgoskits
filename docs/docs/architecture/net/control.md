---
sidebar_position: 3
sidebar_label: "控制面"
---

# 控制面

控制面是 `ax-net` 的接口管理和路由决策层，与数据面完全分离，查询不进入设备锁也不推进 smoltcp poll。

## NetControl

控制面由 `NetControl`（[service.rs](net/ax-net/src/service.rs)）统一持有：

```rust
pub struct NetControl {
    state: RwLock<ControlState>,
    pub(crate) routes: SharedRouteTable,
}
```

内部 `ControlState` 保存 `Vec<NetInterface>`（接口 registry）与 `Vec<DnsServerEntry>`（DNS 来源信息）。

### 接口查询

- `interfaces()` / `interface_by_name()` / `interface_by_id()`：返回只读快照，不持锁。
- `ipv4_config(name)`：按名称查接口 IPv4 配置。
- `default_routes()`：返回当前默认路由列表。

### 路由决策

`select_route(dst_addr)`：按目的地址查路由，校验目标接口 `UP`。底层调用 `RouteTable::select_route_if()`，传入 `is_usable` 闭包过滤未 `UP` 的接口。

`select_route_for_source(dst, src)`：在 TX dispatch 时同时匹配目的地址和源地址。多宿主主机（两个接口有不同源 IP）发送到同一目的地时，需根据 smoltcp 注入的源地址选择正确的出接口。

```rust
pub fn select_route_if(&self, dst, mut is_usable: impl FnMut(InterfaceId) -> bool)
    -> Option<RouteDecision>
{
    self.rules.iter()
        .find(|rule| rule.filter.contains_addr(dst) && is_usable(rule.interface_id))
        .map(|rule| RouteDecision { /* ... */ })
}
```

### 接口-地址绑定

`local_binding_for(endpoint)`：把本地监听地址映射为 `DeviceBinding`。若 `endpoint.addr` 是某个接口的 IP，返回 `bound_if = Some(interface_id)`；若是 wildcard，返回 `DeviceBinding::default()`。

### 运行期更新

`commit_interface_update(update, routes)`：DHCP ACK 等运行期更新通过一次性事务提交接口地址、smoltcp IP 地址、路由和 DNS，避免外部看到半更新状态。接口查询只需持有 `NetControl.state` 读锁；更新时获取写锁。

## RouteTable

`RouteTable`（[router.rs](net/ax-net/src/router.rs)）是路由表的实现：

```rust
#[derive(Debug)]
pub struct Rule {
    pub filter: IpCidr,
    pub via: Option<IpAddress>,
    pub dev: usize,
    pub interface_id: InterfaceId,
    pub src: IpAddress,
    pub metric: u32,
    pub order: u64,
}
```

### 排序策略

`sort_rules()` 的三级排序：

1. **最长前缀优先**：`b.filter.prefix_len().cmp(&a.filter.prefix_len())`
2. **低 metric 优先**：同前缀长度时，`a.metric.cmp(&b.metric)`
3. **插入顺序稳定**：同前缀同 metric 时，`a.order.cmp(&b.order)`

`select_route_if` 使用 `find()` 返回第一个匹配项（即最高优先级的路由）。

### 规则管理

- `add_rule(rule)`：添加单条规则，自动分配 `order` 并重排。
- `remove_ipv4_rules_for_interface(id)`：移除某个接口的所有 IPv4 规则。
- `replace_ipv4_rules_for_interface(id, new_rules)`：原子替换某个接口的 IPv4 规则（DHCP ACK 后使用）。
- `default_routes()`：返回所有默认路由（`0.0.0.0/0`）。

## 接口标识

`InterfaceId(u32)` 是内部接口 ID，同时也是 StarryOS Linux ABI 的 ifindex 来源：

```rust
pub struct InterfaceId(u32);

impl InterfaceId {
    pub const LOOPBACK: Self = Self(1);
    pub const fn new(raw: u32) -> Self { Self(raw) }
    pub const fn to_linux_ifindex(self) -> i32 { self.0 as i32 }
    pub const fn from_linux_ifindex(ifindex: i32) -> Option<Self> { /* ... */ }
}
```

- `InterfaceId::LOOPBACK == 1`，固定对应 `lo`。
- Ethernet 接口从 `2` 开始，按发现顺序递增。

`InterfaceInfo` 是对外只读快照，包含 ID、名称、类型、MAC、IPv4、MTU、flags 和 metric。

```rust
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

## DeviceBinding

`DeviceBinding`（[config.rs](net/ax-net/src/config.rs)）对应 Linux `SO_BINDTODEVICE`：

```rust
pub struct DeviceBinding {
    pub bound_if: Option<InterfaceId>,
}
```

- `None`：未绑定接口，按 route decision 选择出接口。
- `Some(id)`：只允许对应接口参与 route/waker/device 选择。

## DNS 注册

DNS server 来源分三类，内部记录为 `DnsServerEntry`：

| 来源 | 说明 |
| --- | --- |
| DHCP | DHCP ACK 下发，绑定来源接口和 metric |
| Static | `InterfaceConfig::dns_servers` |
| Fallback | `NetworkConfig::default_dns_servers` |

`dns_servers()` 返回按 (metric, interface_id, server_ip) 排序的去重地址列表。DHCP ACK 后 `commit_interface_update` 会替换对应接口的 DNS entry。
