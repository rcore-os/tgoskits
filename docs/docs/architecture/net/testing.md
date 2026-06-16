---
sidebar_position: 10
sidebar_label: "测试与限制"
---

# 测试与限制

网络栈同时影响 ArceOS、StarryOS 和 Axvisor。修改 `net/ax-net` 时，应按改动范围选择验证集合。

## 单元测试

推荐至少覆盖：

- 接口 ID 与 `DeviceBinding` 语义。
- 多接口 route lookup。
- longest prefix 优先级。
- 同前缀 metric 选择。
- 同 metric 稳定插入顺序。
- 接口 down / 不可用时跳过 route。
- default route 查询。
- bounded RX/TX queue 满时行为。
- `QueuedPacket` 超 MTU 拒绝、队列路径无每包堆分配。
- loopback dispatch 直接注入 `Router.rx_buffer`，并在注入前触发 TCP SYN snoop。
- DHCP bootstrap 和 per-interface 状态。
- DHCP server Discover/Request 回复。
- TCP orphan 在 Closed、TIME_WAIT/FIN teardown 超时和 overflow 场景下的回收。
- TCP/UDP bind 到具体本地地址后保留接口绑定。
- 未绑定 TCP/UDP connect 按 route decision 选择接口。

当前 `ax-net` host 单测可用：

```bash
cargo test -p ax-net
```

`ax-net` host test 需要相关底层 crate 启用 host-test feature，避免链接内核 percpu 路径。

### 测试覆盖建议

| 模块 | 关键测试点 | 优先级 |
| --- | --- | --- |
| `RouteTable` | 添加/删除/替换规则、排序验证、默认路由查询、`select_route_if` with mock filter | 高 |
| `ListenTable` | port 冲突、per-address listen、wildcard 冲突、backlog 上限、`can_listen`/`listen`/`unlisten`、SYN 队列溢出、`can_accept`/`accept` | 高 |
| `SocketSetWrapper` | UDP bind 冲突仲裁（精确/wildcard）、add/remove；`SO_REUSEADDR` 跳过 side table 的行为由 `UdpSocket::bind()` 覆盖 | 中 |
| `StateLock` | Idle→Busy CAS、transit 成功/失败回退、状态读取一致性 | 中 |
| `GeneralOptions` | nonblock/send_timeout/recv_timeout、device_binding 读写、Atomic 一致性 | 中 |
| `TcpSocket` | `connect()` 成功/失败、`bind_device()` 无效接口、`poll_connect()` 状态转换、`tcp_info_snapshot()` 字段 | 高 |
| `UdpSocket` | `send()` with MSG_MORE corking、connected/disconnected send、`recv()` truncation | 高 |
| `RawSocket` | `send()` ICMP echo→loopback reply、TTL 读写、IP version 校验、`MSG_PEEK` 与 `deferred_rx` wire-packet 格式 | 中 |
| `UnixSocket` | stream pair send/recv、cmsg 传递、datagram pair、abstract namespace bind/connect | 中 |
| `VsockSocket` | bind/listen/connect/accept 状态转换、send/recv | 低（需 `vsock` feature） |
| `DhcpState` | Discovering→Offer→Requesting→ACK→Bound 状态机、NAK→reset、`process_packet` 校验 | 中 |
| `DhcpServer` | Discover→Offer、Request→Ack、错误 xid/mac/interface 过滤、单客户端租约覆盖 | 中 |
| `orphan` | Closed 立即回收、teardown 超时回收、overflow 只 warn 且保留仍在 teardown 的 socket | 高 |
| DNS | `dns_query_timeout` 超时、不可路由 server 过滤、`DnsSocketGuard` drop | 中 |

## 接口路由测试

建议对 `RouteTable` 进行以下专项测试：

```
Test: longest prefix match
  Add: 10.0.0.0/8 → dev0, metric=100
  Add: 10.0.2.0/24 → dev1, metric=200
  Query 10.0.0.1 → dev0 (只有 /8 匹配)
  Query 10.0.2.15 → dev1 (/24 比 /8 更长)

Test: metric tie-breaking
  Add: 0.0.0.0/0 → dev0, metric=100
  Add: 0.0.0.0/0 → dev1, metric=50
  Query 8.8.8.8 → dev1 (metric 更低)

Test: insert order stability (same prefix, same metric)
  Add rule_a: metric=100, order=0
  Add rule_b: metric=100, order=1
  rule_a 排在 rule_b 前面

Test: unavailable interface skip
  Add: 0.0.0.0/0 → dev0, metric=100
  Add: 0.0.0.0/0 → dev1, metric=200
  dev0 is not UP → select_route_if with is_usable → returns dev1

Test: replace_ipv4_rules_for_interface
  Add rules for interface=2
  Call replace with new rules → old rules removed, new rules in place
```

## DHCP 测试

DHCP 逻辑建议覆盖：

```
Test: Discovering → Offer → Requesting transition
  创建 DhcpState(phase=Discovering)
  构造 DHCPOFFER packet (unicast yiaddr, correct xid, correct mac)
  process_packet → phase=Requesting, offered_address=yiaddr
  验证未产生 DhcpEvent

Test: Requesting → ACK → Bound transition
  创建 DhcpState(phase=Requesting, offered_address=addr)
  构造 DHCPACK (subnet_mask 255.255.255.0, yiaddr=addr, router=x, dns=[y])
  process_packet → phase=Bound, 返回 DhcpEvent::Configured

Test: NAK → reset
  创建 DhcpState(phase=Bound, address=some_addr)
  process_packet(DHCPNAK) → phase=Discovering, 返回 DhcpEvent::Deconfigured

Test: packet filtering
  DhcpState for eth0 (interface_id=2) 忽略 interface_id=3 的包
  错误 xid 或 mac 的包被忽略
```

## TCP 测试

```
Test: SYN pre-create
  ListenTable.listen(port=8080, backlog=10)
  调用 incoming_tcp_packet(SYN, dst_port=8080)
  → SocketSet 中有新 socket，ListenTableEntryInner.syn_queue.len() == 1

Test: SYN queue overflow
  backlog=2, 压入 2 个 SYN, 第 3 个 SYN → 丢弃 (warn)

Test: accept existing connection
  压入 2 个 PendingTcp, 第一个 ESTABLISHED, 第二个 SYN_RECEIVED
  accept() → 返回第一个

Test: accept skip closed
  第一个 CLOSED without data, 第二个 ESTABLISHED
  accept() → 跳过第一个(移除), 返回第二个

Test: bind_device
  TcpSocket::new().bind_device(nonexistent_id) → NoSuchDevice
  TcpSocket::new().bind_device(valid_id) → Ok

Test: SO_REUSEADDR
  UDP: 设置 SO_REUSEADDR 后 UdpSocket::bind() 跳过 SocketSetWrapper 的 UDP side table
  TCP: 当前 TCP_BOUND_PORTS 仍做 wildcard/specific 地址冲突检查，不能简单断言两个 TCP socket 设 SO_REUSEADDR 后一定可 bind 同一端口

Test: per-address listen
  listen(127.0.0.1:8080) 与 listen(10.0.2.15:8080) 可以共存
  listen(0.0.0.0:8080) 与任一具体地址 listener 冲突
  SYN 目的地址只进入匹配 listener 的 syn_queue
```

## 已知 Debug 定位指南

### "no route to destination"

可能原因：

1. DHCP 未完成 — 检查 `dhcp_configured()` 返回值。
2. 默认路由未提交 — 检查 `default_routes()` 是否非空。
3. 接口未 UP — 检查 `interface_by_id(id).flags` 包含 `UP`。
4. RX worker 沉睡 — `device.rx_wake` 未被通知。
5. 设备 TX/RX queue 满 — 检查是否有 worker 未运行或 driver 长时间阻塞。

检查方法：

```rust
// 在出现错误前插入
info!("routes: {:?}", ax_net::default_routes());
info!("interfaces: {:?}", ax_net::interfaces());
info!("arp entries: {:?}", ax_net::arp_entries());
```

### "address already in use" (UDP)

1. 检查是否有其他 socket 绑定了同一 `(addr, port)`。
2. 检查 wildcard `(0.0.0.0, port)` 是否已存在。
3. 检查 `SO_REUSEADDR` 是否正确设置。

### DHCP 超时

1. `DHCP_BOOTSTRAP_ATTEMPTS = 200`，每次 interval 10ms → 总超时 ≈ 2 秒。
2. 检查 DHCP server 是否可达。
3. 检查 `process_packet()` 中的 xid/mac 校验是否误过滤。

### ARP pending 队列溢出

`ETHERNET_MAX_PENDING_PACKETS = 128`，当大量并发连接的目标 MAC 都未解析时触发。增加 `ETHERNET_MAX_PENDING_PACKETS` 或确保 ARP reply 在预期时间内到达。

### net-poll worker 不推进

1. 检查 `NET_POLL_REQUESTED` atomic 标志。
2. 检查 `NET_POLL_WAKE` 是否有竞争。
3. 检查 `poll_until_idle()` 中的 CAS 锁是否一直失败；正常情况下只有 `net-poll` worker 会持续驱动协议栈，socket 热路径只负责 `request_poll()`。
4. 检查 `next_poll_delay()` 返回值 — 如果 smoltcp `poll_at()` 返回 `None`，idle poll interval 为 100ms。
