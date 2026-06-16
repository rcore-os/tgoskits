---
sidebar_position: 10
sidebar_label: "测试与限制"
---

# 测试与限制

`ax-net` 横跨协议栈核心、设备 dataplane、系统调用 ABI 和运行时设备接入。验证时应按改动影响面选择测试层级：纯数据结构改动优先跑 crate 单测；socket、poll、ioctl、AF_PACKET、netlink 或 procfs 改动需要补 StarryOS QEMU 系统测试；涉及 runtime、架构或平台设备路径时再扩展到对应架构。

## 验证分层

| 层级 | 覆盖范围 | 典型触发条件 |
| --- | --- | --- |
| `ax-net` 单元测试 | 路由表、接口配置、socket bind 表、TCP listen/orphan、DHCP、DNS、raw deferred RX、队列边界 | 修改 `net/ax-net/src` 内部逻辑 |
| crate lint / fmt | Rust 风格、clippy 约束、feature 组合基础检查 | 修改 Rust 代码 |
| StarryOS syscall 测试 | Linux socket ABI、ioctl、AF_PACKET、netlink、procfs、poll/epoll 行为 | 修改 StarryOS 适配层或 `ax-net` public API |
| 跨架构 QEMU | riscv64、loongarch64、aarch64、x86_64 上的启动、调度、网络 syscall 回归 | 修改同步、原子、poll worker、架构相关接入 |
| 板级或真实设备验证 | IRQ、DMA、真实 NIC、Wi-Fi/SoftAP、链路状态 | 修改 driver/runtime/设备 worker/OOB RX |

## 推荐命令

### 文档改动

仅修改 `docs/docs/architecture/net` 文档时，不需要运行 clippy 或 QEMU。建议至少检查 Markdown 结构、代码块闭合和文档内旧术语：

```bash
rg -n "^##|^###" docs/docs/architecture/net
grep -R "^```" -n docs/docs/architecture/net | wc -l
rg -n "Rounte[r]|ServiceCor[e]|device_mas[k]|#L[0-9]+" docs/docs/architecture/net
```

### `ax-net` 逻辑改动

修改 `net/ax-net` Rust 逻辑后，优先执行：

```bash
cargo fmt
cargo xtask clippy --package ax-net
cargo test -p ax-net
```

`cargo test -p ax-net` 用于 host 单测。若某些 feature 组合依赖内核环境或底层 crate host-test 支持不足，应记录失败原因，并用更贴近目标环境的 `cargo xtask` 验证补足。

### StarryOS 系统测试

涉及 Linux ABI、socket syscall、poll/epoll、AF_PACKET、netlink 或 `/proc/net/*` 时，使用 `cargo xtask` 跑 StarryOS QEMU：

```bash
cargo xtask starry test qemu --arch riscv64
cargo xtask starry test qemu --arch loongarch64
```

跨架构回归可扩展到：

```bash
cargo xtask starry test qemu --arch aarch64
cargo xtask starry test qemu --arch x86_64
```

需要缩小范围时，优先使用 StarryOS case 选择参数运行 system 组：

```bash
cargo xtask starry test qemu --arch riscv64 -c qemu-smp1/system
```

## `ax-net` 单元测试矩阵

### 控制面与路由

| 模块 | 关键测试点 |
| --- | --- |
| `InterfaceId` / `DeviceBinding` | Linux ifindex 映射、无绑定/绑定接口读写、无效接口拒绝 |
| `RouteTable` | 添加、删除、替换规则；最长前缀；低 metric 优先；同 metric 插入顺序稳定 |
| `select_route_if()` | 接口 `UP` 过滤、`SO_BINDTODEVICE` 过滤、无路由返回错误 |
| `select_route_for_source()` | 多宿主场景下源地址和出接口一致 |
| DNS registry | DHCP/static/fallback 排序、去重、不可路由 DNS server 过滤 |

专项路由用例：

```text
longest prefix:
  10.0.0.0/8 -> dev0 metric=100
  10.0.2.0/24 -> dev1 metric=200
  query 10.0.2.15 => dev1

metric:
  0.0.0.0/0 -> dev0 metric=100
  0.0.0.0/0 -> dev1 metric=50
  query 8.8.8.8 => dev1

source route:
  eth0 source=10.0.2.15
  eth1 source=192.168.1.10
  packet source=192.168.1.10 => eth1

replace interface rules:
  replace IPv4 rules for interface=2
  old connected/default routes are removed
  new connected/default routes are installed
```

### 设备 dataplane

| 模块 | 关键测试点 |
| --- | --- |
| `BoundedPacketQueue` | 容量上限、满队列 drop、长度计数一致、wait/wake 行为 |
| `QueuedPacket` | 超 MTU 拒绝、inline buffer 无每包堆分配 |
| `Router::poll()` | 共享 RX queue drain、ingress `InterfaceId` 保留、RX buffer 满时停止 |
| `Router::dispatch()` | 普通 unicast route lookup、limited broadcast 多接口发送、TX queue 满时 drop/warn |
| loopback | TX 直接注入 `rx_buffer`，不经过设备队列，并在注入前执行 TCP SYN snoop |
| waker | 全局设备 waker 与绑定接口 waker 分离 |

需要重点验证 loopback 的两个性质：

```text
loopback fast path:
  socket send -> smoltcp tx_buffer
  Router::dispatch sees LOOPBACK route
  inject_loopback_rx_direct writes Router.rx_buffer
  next poll consumes packet in same protocol core

no queue allocation:
  loopback packet does not enter shared RX queue
  no Vec/to_vec/Box allocation is required for the loopback hop
```

### Socket 层

| 模块 | 关键测试点 |
| --- | --- |
| `SocketSetWrapper` | socket add/remove、UDP bind side table、wildcard/specific 地址冲突 |
| `ListenTable` | backlog、per-address listen、wildcard 冲突、SYN queue、accept wake |
| `GeneralOptions` | nonblocking、send/recv timeout、`SO_REUSEADDR`、`SO_BINDTODEVICE` |
| `TcpSocket` | bind/connect/listen/accept/send/recv/shutdown、`TCP_INFO`、orphan 转移 |
| `UdpSocket` | connected/unconnected send、`MSG_MORE` corking、truncation、`SO_REUSEADDR` |
| `RawSocket` | ICMP echo loopback reply、TTL、IP version 校验、`MSG_PEEK`、deferred RX wire-packet 格式 |
| Unix socket | pathname/abstract namespace、stream/datagram、cmsg、shutdown |
| vsock | bind/listen/connect/accept/send/recv、无设备初始化语义 |

TCP listen 建议覆盖：

```text
per-address listen:
  listen(127.0.0.1:8080)
  listen(10.0.2.15:8080)
  both succeed
  listen(0.0.0.0:8080) conflicts with either specific listener

accept queue:
  incoming SYN creates pending socket
  established pending socket is returned by accept()
  closed pending socket is skipped and removed
  queue overflow drops new SYN without corrupting existing entries
```

UDP bind 建议覆盖：

```text
wildcard conflict:
  bind(0.0.0.0:1234)
  bind(10.0.2.15:1234) => AddressInUse unless reuse policy allows it

device binding:
  bind(10.0.2.15:1234) derives eth0 binding
  send uses route allowed by that binding
  waker registration ignores unrelated interfaces
```

### DHCP 与动态状态

| 模块 | 关键测试点 |
| --- | --- |
| DHCP client | Discovering、Requesting、Bound、NAK reset、xid/mac/interface 过滤 |
| DHCP commit | smoltcp address list、interface snapshot、DNS、route table 一致更新 |
| DHCP server | Discover -> Offer、Request -> Ack、客户端 MAC/xid/interface 过滤、租约覆盖 |
| dynamic device | 分配新 `InterfaceId`、写入 registry、安装路由、启动 worker、唤醒 poll |

DHCP 状态机用例：

```text
Discovering -> Offer:
  DhcpState phase=Discovering
  receive DHCPOFFER with correct xid/mac/interface
  phase=Requesting
  no configured event yet

Requesting -> ACK:
  receive DHCPACK with yiaddr/subnet/router/dns
  phase=Bound
  emit Configured event
  commit updates interface IPv4, DNS and routes

NAK:
  Bound address exists
  receive DHCPNAK
  phase=Discovering
  emit Deconfigured event
```

### 生命周期与清理

| 模块 | 关键测试点 |
| --- | --- |
| orphan reaper | `Closed` 立即回收、TIME_WAIT/FIN teardown 超时回收、overflow 只回收 Closed |
| socket drop | TCP orphan 化后仍允许 FIN/TIME_WAIT 推进 |
| poll worker | socket drop/request_poll 后 net-poll worker 被唤醒 |
| DNS socket guard | 查询结束后 socket 被释放 |

orphan 测试应避免强制清理仍在 teardown 的 TCP socket。超过 orphan 上限时，优先回收 `Closed`；仍处于 TIME_WAIT、FIN_WAIT、LAST_ACK、CLOSING 的 socket 应保留到超时或自然关闭。

## StarryOS 系统测试矩阵

### Socket dataplane

重点覆盖：

- TCP loopback connect/send/recv/close。
- UDP send/recv、connected UDP、非阻塞错误。
- raw ICMP echo 路径。
- poll/select/epoll readiness。
- send 后及时唤醒 waiter，避免依赖应用线程同步 poll。

相关测试：

| 测试 | 覆盖点 |
| --- | --- |
| `syscall-test-socket-dataplane` | TCP/UDP/raw socket dataplane 基础行为 |
| `bugfix-bug-tcp-send-no-epoll-notify` | TCP send 后 epoll waiter 唤醒 |

### ioctl、netlink 与 procfs

重点覆盖：

- `SIOCGIFCONF` 返回非零接口列表。
- `SIOCGIFINDEX` 与 netlink `RTM_GETLINK` 的 ifindex 一致。
- `SIOCGIFADDR`、`SIOCGIFNETMASK`、`SIOCGIFBRDADDR` 与 `ax-net` 接口快照一致。
- `RTM_GETADDR` 至少返回 loopback IPv4。
- `/proc/net/arp` 行格式正确，device 字段来自真实接口名。

相关测试：

| 测试 | 覆盖点 |
| --- | --- |
| `bugfix-bug-netlink-getlink` | `RTM_GETLINK`、`SIOCGIFTXQLEN`、link 属性 |
| `bugfix-bug-netlink-getaddr` | `RTM_GETADDR`、loopback address、link/address dump |
| `syscall-test-netlink-recvmsg` | netlink recvmsg 基础语义 |
| `bugfix-bug-proc-net-arp` | `/proc/net/arp` 格式与内容 |

### AF_PACKET

重点覆盖：

- `socket(AF_PACKET, SOCK_DGRAM, ...)` 创建和权限语义。
- `bind(sockaddr_ll)` 按 ifindex 绑定接口。
- `getsockname()` 返回匹配的 `sockaddr_ll`。
- packet socket ioctl 返回 ifindex、flags 和 MAC。
- 模拟 gateway ARP reply 工作，loopback 或未知 peer 不产生错误格式响应。

相关测试：

| 测试 | 覆盖点 |
| --- | --- |
| `bugfix-bug-packet-arping` | AF_PACKET bind、SIOCGIFINDEX、RTM_GETLINK 一致性、模拟 ARP reply |

### Namespace 可见性

重点覆盖：

- root network namespace 能看到 loopback 和 Ethernet。
- 非 root namespace 只暴露 loopback 视图。
- StarryOS 可见性过滤不改变 `ax-net` 全局 route table。
- `AF_PACKET` 创建和绑定遵守 root namespace 限制。

## 回归场景

### 多接口路由

验证内容：

- 多个 Ethernet 接口各自有独立 IPv4、metric 和默认路由。
- 目的地址命中直连路由时优先使用直连接口。
- 多个默认路由按 metric 选择。
- `SO_BINDTODEVICE` 或绑定具体本地地址后，只使用匹配接口。
- smoltcp 已选源地址后，Router dispatch 不从错误接口发送。

### Loopback

验证内容：

- TCP loopback connect 可以在同一协议核心内推进 SYN。
- UDP loopback send/recv 不经过外部设备 worker。
- raw ICMP echo loopback reply 使用正确 wire packet 格式。
- loopback 路径不会因为共享 RX queue 满而丢包；它受 `Router.rx_buffer` 容量约束。

### Poll 与唤醒

验证内容：

- socket 热路径只请求 poll，不同步执行 smoltcp poll。
- RX worker 入队后唤醒 net-poll worker。
- TX worker 队列 drain 后不阻塞协议核心。
- accept queue、UDP recv、TCP send/recv 都能唤醒对应 waker。
- `poll_at()` 定时器可推进 TCP retransmit、TIME_WAIT、DHCP 等事件。

### DNS 与 DHCP

验证内容：

- DHCP ACK 后接口地址、默认路由和 DNS 同步可见。
- DHCP NAK 后旧地址、旧路由和 DHCP DNS 被清理。
- 静态 DNS、DHCP DNS、fallback DNS 按 metric 和来源去重排序。
- 不可路由 DNS server 被跳过，查询尝试下一个 server。

### ABI 兼容

验证内容：

- `SO_BINDTODEVICE` set/get 往返正确。
- `SIOCGIFINDEX`、`AF_PACKET sockaddr_ll.sll_ifindex`、netlink ifindex 使用同一 `InterfaceId`。
- `/proc/net/arp` 不暴露固定 QEMU gateway stub，也不写死 `eth0`。
- `TCP_INFO`、`SO_TYPE`、`FIONREAD` 从真实 socket 状态返回。

## 调试指南

### QEMU 只显示 `STARRY_GROUPED_TEST_FAILED`

`cargo xtask starry test qemu` 的汇总输出可能只显示匹配到失败模式。定位时应查更早的 test binary 输出：

```bash
rg -n "STARRY_GROUPED_TEST_FAILED|FAIL:|panic|assert|test-socket|bugfix-bug" target -g "*.log"
```

排查顺序：

1. 找到第一个打印 `FAIL:` 的 test binary。
2. 确认是否是网络 testcase，还是其它系统测试间接受网络超时影响。
3. 对照该 testcase 的源码，确认失败发生在 syscall 返回值、超时、内容不匹配还是权限语义。
4. 如果 riscv64 和 loongarch64 都失败，优先怀疑协议栈/ABI 逻辑；如果只在单架构失败，再检查原子、调度和定时器。

### `no route to destination`

常见原因：

- DHCP 未完成，接口没有 IPv4。
- default route 没有提交。
- 接口 flags 不包含 `UP`。
- socket 被 `SO_BINDTODEVICE` 限制到不匹配接口。
- smoltcp 选择的源地址和 route table 中的接口源地址不一致。

建议打印：

```rust
info!("interfaces: {:?}", ax_net::interfaces());
info!("routes: {:?}", ax_net::default_routes());
info!("dns: {:?}", ax_net::dns_servers());
```

### `address already in use`

排查方向：

- 是否已有 wildcard bind 占用同一端口。
- 是否已有具体地址 bind 与新 bind 冲突。
- TCP listen 是否被 `ListenTable` 的 wildcard/specific 规则拒绝。
- UDP 是否正确设置 `SO_REUSEADDR`，以及该路径是否应跳过 side table。
- 绑定具体本地地址时，该地址是否属于当前接口 registry。

### DHCP 超时

排查方向：

- DHCP packet 的 ingress `InterfaceId` 是否匹配对应 `DhcpState`。
- xid、client MAC、message type 是否被过滤。
- RX worker 是否把 DHCP reply 入队到共享 RX queue。
- net-poll worker 是否被唤醒。
- DHCP server 是否在同一二层网络可达。

### AF_PACKET / netlink 不一致

排查方向：

- `SIOCGIFINDEX` 是否来自 `InterfaceId::to_linux_ifindex()`。
- `RTM_GETLINK` 是否遍历同一份 `ax_net::interfaces()`。
- `sockaddr_ll.sll_ifindex` 是否能通过 `InterfaceId::from_linux_ifindex()` 反查接口。
- namespace 可见性过滤是否导致接口在某条路径可见、另一条路径不可见。

### `/proc/net/arp` 异常

排查方向：

- `ax_net::arp_entries()` 是否有对应 entry。
- ARP entry 的 interface_id 是否能映射回接口名。
- procfs 输出是否仍残留固定 gateway 或固定 `eth0` 字段。
- ARP pending queue 是否已满，导致解析请求被丢弃。

### net-poll worker 不推进

排查方向：

- `request_poll()` 是否被调用。
- `NET_POLL_REQUESTED` 是否被置位。
- `NET_POLL_WAKE` 是否唤醒 worker。
- `POLLING_INTERFACES` CAS 是否长时间被占用。
- `next_poll_delay()` 是否返回过长 idle interval。
- 设备 worker 是否卡在 driver `send()` / `receive()` 持锁路径。

## 已知限制

### 协议与特性范围

- IPv6 route、NDP、MLD 和完整 IPv6 socket 语义未作为主路径完善。
- IGMP/按接口 multicast membership 不完整。
- DHCP lease renew/rebind、租约过期回收和地址冲突检测仍需继续补齐。
- DNS 不包含 split DNS、search domain 和完整 `/etc/resolv.conf` 语义。
- `SO_REUSEPORT`、完整 Linux TCP option 集合和高级拥塞控制不在当前范围。

### 架构限制

- 协议核心仍是单 smoltcp `Interface + SocketSet`，TCP/UDP 状态机本身不多核并行。
- 多设备 dataplane 通过 worker 和有界队列解耦，但不是 RSS/NAPI 多队列模型。
- loopback 已有直接注入快路径，但普通设备 RX/TX 仍存在必要的 packet copy。
- 尚未实现端到端 zero-copy；这需要 rd-net buffer ownership、packet pool 和 smoltcp token 共同改造。

### 系统集成限制

- StarryOS network namespace 当前主要是可见性过滤，不是完整 per-namespace network stack。
- 动态 link down/up、接口热插拔、队列重建和 socket 错误传播仍需完善。
- 真实硬件 IRQ/DMA 行为需要板级验证，QEMU 不能覆盖全部 timing 和 cache 一致性问题。

## 变更验收清单

提交网络相关变更前，按影响面检查：

- 修改 `net/ax-net/src` 逻辑：`cargo fmt`、`cargo xtask clippy --package ax-net`、相关单测。
- 修改 socket readiness 或 poll：补 StarryOS poll/epoll 或 socket dataplane QEMU 验证。
- 修改接口、路由、DNS、ARP：补 ioctl、netlink、procfs、multi-interface route 验证。
- 修改 AF_PACKET：补 `bugfix-bug-packet-arping` 相关路径。
- 修改 runtime/driver/OOB RX：至少验证 riscv64 QEMU，真实设备改动还应补板级验证记录。
- 修改文档：检查标题层级、代码块闭合和过期术语。
