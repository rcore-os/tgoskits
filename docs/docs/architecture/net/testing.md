---
sidebar_position: 12
sidebar_label: "测试与限制"
---

# 测试与限制

`ax-net` 测试分三层：`net/ax-net` crate 内单元测试覆盖协议栈内部数据结构和路由/绑定语义；StarryOS system 测试覆盖 Linux ABI 观测面；`apps/starry/qemu/dual-net` 覆盖双网口 DHCP、路由和并发数据面。

## 1. 测试资产

网络测试按纯数据结构、host-test、StarryOS system case 和双网卡集成场景分层，每层验证不同故障面。下表把测试位置与职责对应起来，选择验证命令时应优先使用能覆盖原始行为边界的最低层测试，再补充跨系统路径。

| 层级 | 位置 | 作用 |
| --- | --- | --- |
| crate 单元测试 | `net/ax-net/src` 内各 `#[cfg(test)]` 模块 | 验证 `RouteTable`、`NetControl`/`Service`、TCP/UDP 设备绑定、UDP bind 表、TCP listen 表和通用 socket option |
| host 集成测试 | `net/ax-net/tests/std.rs` | 验证 public 配置/快照、option dispatch、credentials 和 route 数据类型；需要 `host-test` |
| StarryOS system 测试 | `test-suit/starryos/qemu/system` | 验证 Linux socket syscall、ioctl、AF_PACKET、netlink、procfs 等 ABI |
| dual-net 集成测试 | `apps/starry/qemu/dual-net` | 验证两张 virtio-net、双 DHCP、接口绑定下载和并发数据面 |
| xtask 结构自检 | `scripts/axbuild/src/starry/test/tests/asset_network_tests.rs` | 验证 `dual-net` app 配置必须包含双网卡、host HTTP fixture 和 guest probe |

资产表说明每层测试使用不同运行环境和观察面，不能用单元测试替代 ABI 或双网卡验证。单元测试首先固定纯状态结构的确定语义，为更高层失败提供可快速排除的基础。

## 2. 单元测试

`ax-net` 单元测试围绕无需设备或 QEMU 即可确定验证的状态结构组织，包括路由排序、DHCP 提交、端口仲裁和通用选项。各小节给出测试所在模块和必须保持的行为契约，便于在最低层快速定位回归。

### 2.1 运行方式

`ax-net` 单元测试通过 `host-test` feature 编译可在宿主机执行的路由、绑定、控制面、poll-group 状态机与 generation 逻辑，不启动 QEMU 或真实 NIC。该命令适合快速验证纯 Rust 状态转换，但不能替代 StarryOS ABI 或 queue executor/真实 IRQ 的端到端覆盖。

```bash
cargo test -p ax-net --features host-test
```

`ax-net` 在 std 白名单中，完整仓库入口是 `cargo xtask test`；该入口会为 `ax-net` 自动选择 `host-test` profile。上面的命令用于单独迭代这个 crate。单元/集成测试主要覆盖不依赖真实 QEMU 设备的内部逻辑。部分测试会使用 `lib.rs` 中的 `test_support` 构造一个 split-route 测试网络：

```text
LOCAL_IF = InterfaceId(2), LOCAL_ADDR = 10.0.2.15
PEER_IF  = InterfaceId(3), PEER_ADDR  = 10.0.3.15
```

`network_test_guard()` 用全局 mutex 串行化会初始化全局网络状态的测试，避免 `SERVICE`、`NET_CONTROL`、`SOCKET_SET` 这类全局单例在并发 host test 中互相污染。

### 2.2 路由表

路由表测试位于 `router.rs`，直接构造 `Rule` 与 `RouteTable` 验证最长前缀、metric、源地址和设备绑定过滤。它们是多网卡选路的最低层契约，失败时应先修正规则实现，而不是调整上层集成测试预期。

| 测试 | 覆盖点 |
| --- | --- |
| `route_lookup_uses_longest_prefix` | 最长前缀优先，`10.0.1.0/24` 优先于默认路由 |
| `route_lookup_uses_metric_for_same_prefix` | 同前缀按 metric 小者优先 |
| `route_lookup_keeps_stable_order_for_equal_metric` | 同前缀、同 metric 时保持插入顺序 |
| `route_lookup_skips_unusable_interface` | `select_route_if()` 可通过闭包跳过不可用接口 |
| `default_routes_only_reports_zero_prefix_ipv4_rules` | `default_routes()` 只导出 IPv4 `0.0.0.0/0` 规则 |
| `bounded_packet_queue_reports_full_and_preserves_order` | 有界队列满时返回错误，并保持 FIFO |
| `rx_backpressure_preserves_frame_len_pairing` | shared RX queue 背压时，本地 batch 保留 packet 与 L2 frame 长度的 1:1 配对 |
| `no_route_does_not_count_interface_tx_dropped` | IP 层无路由不错误归入某个网卡 `tx_dropped` |
| `stats_reflects_current_counters_after_counting` | `NetDevStats` 快照反映累计原子计数 |

这些测试对应多网口 route decision 的核心排序规则：最长前缀、metric、稳定顺序和接口可用性过滤。

### 2.3 DHCP 地址状态

DHCP 地址状态测试位于 `service.rs`，验证 lease 提交或清理时 smoltcp 地址、接口快照、路由和 DNS 来源保持同步。测试应覆盖 ACK、NAK、更新与删除等确定状态转换，防止控制面出现部分更新。

| 测试 | 覆盖点 |
| --- | --- |
| `dhcp_configured_is_true_once_any_interface_has_address` | 多 DHCP 接口中只要任一接口已获得地址，bootstrap 状态即可视为完成 |
| `interface_address_table_handles_loopback_and_two_ethernet_addresses` | smoltcp `Interface` address list 能同时保存 loopback、eth0、eth1 IPv4 |

这组测试防止网络初始化重新退化为“只看第一个网卡”或“接口地址表只能容纳单 Ethernet 地址”的模型。

### 2.4 TCP 设备绑定

TCP 设备绑定测试位于 `tcp.rs`，检查具体本地地址、`SO_BINDTODEVICE` 和路由结果如何约束 connect、listen 与 readiness。测试重点是拒绝不匹配接口，而不是仅确认正常主接口可以连接。

| 测试 | 覆盖点 |
| --- | --- |
| `tcp_info_reports_default_socket_metrics` | `TCP_INFO` 在 closed socket 上返回稳定默认字段 |
| `connect_preserves_bound_interface` | TCP bind 到具体本地地址后，connect 不会被 peer route 改写绑定接口 |
| `connect_uses_peer_route_when_unbound` | wildcard bind 的 TCP connect 根据目的地址 route decision 选择接口 |
| `connect_rejects_unroutable_bound_device` | 显式绑定到不可达接口后，connect 返回错误并保留原绑定 |

这组测试覆盖 `SO_BINDTODEVICE` 和本地地址推导出的 `DeviceBinding` 对 TCP connect 的影响。

### 2.5 UDP 设备绑定

UDP 设备绑定测试位于 `udp.rs`，验证 connected 与 unconnected datagram 在显式接口约束下选择本地地址和出口。用例还需要区分 bind endpoint、peer endpoint 与每次 sendto 目标，避免把 TCP 的连接语义错误套用到 UDP。

| 测试 | 覆盖点 |
| --- | --- |
| `connect_preserves_bound_interface` | UDP bind 到具体本地地址后，connect 不会改写绑定接口 |
| `connect_uses_peer_route_when_unbound` | wildcard bind 的 UDP connect 根据目的地址 route decision 选择接口 |
| `connect_rejects_unroutable_bound_device` | 显式绑定到不可达接口后，connect 返回错误并保留原绑定 |

UDP 的测试与 TCP 对齐，重点是 datagram socket 的 connected peer 不应破坏本地地址绑定语义。

### 2.6 UDP 绑定表

UDP 绑定 side table 测试位于 `wrapper.rs`，覆盖 wildcard 与具体地址冲突、设备约束以及 `SO_REUSEPORT` 组规则。`SO_REUSEADDR` 只保存选项而不会跳过 side table，相关负向用例应保持这一实现边界。

| 测试 | 覆盖点 |
| --- | --- |
| `udp_bind_rules_allow_distinct_specific_addresses` | 相同端口可绑定到不同具体本地地址；相同地址冲突；wildcard 与具体地址冲突 |
| `udp_bind_rejects_specific_after_wildcard` | 已存在 wildcard bind 时拒绝后续具体地址 bind |
| `udp_reuseport_group_shares_a_port_while_plain_binders_conflict` | 只有双方 `SO_REUSEPORT` 且 endpoint 完全相同才允许共同绑定 |

这些测试补齐 smoltcp UDP socket 之外的 Linux 风格 wildcard/specific bind 仲裁。

### 2.7 TCP 监听表

TCP 监听表测试位于 `listen_table.rs`，验证 wildcard/具体地址 listener 的查找、SYN child 预创建和 accept queue 生命周期。它们保证单协议核心上的 passive open 语义不会因设备数量增加而复制 listener 或遗漏端口冲突。

| 测试 | 覆盖点 |
| --- | --- |
| `allows_same_port_on_distinct_specific_addresses` | 同端口可以在不同具体地址上 listen |
| `wildcard_listener_conflicts_with_specific_addresses` | wildcard listener 与任一具体地址 listener 冲突 |
| `reuseport_group_shares_a_listen_endpoint` | reuseport listener 可共享完全相同 endpoint |
| `plain_listener_rejects_reuseport_join` | 普通 listener 与 reuseport group 不能混合加入 |

这组测试覆盖 per-address listen 的冲突规则，是 wildcard listen、`0.0.0.0:port` 和多本地地址共存语义的基础。

### 2.8 通用选项

通用选项测试位于 `general.rs`，检查 nonblocking、timeout、reuse、设备绑定和 socket identity 的原子读写及默认值。只回显但不影响后端行为的选项也必须被明确识别，不能通过测试一个 setter/getter 就声称完整支持。

| 测试 | 覆盖点 |
| --- | --- |
| `device_binding_round_trips_none_and_some_interface` | `DeviceBinding` 在 `GeneralOptions` 中可以从 none 到指定接口再回到 none |
| `reuse_address_and_reuse_port_are_independent_flags` | 两个 reuse option 独立保存 |
| `socket_priority_matches_unprivileged_linux_range` | `SO_PRIORITY` 只接受 `0..=6` |
| `ip_tos_storage_masks_user_controlled_ecn_bits` | `IP_TOS` 清除用户可控 ECN 位 |

queue runtime 与统计还有专门覆盖：protocol generation 测试验证同步 flush 不取得第二 ownership；状态机穷举验证 `MISSED`、rearm window 与 `DISABLED` 不可复活；source affinity 测试验证 shared IRQ 同 CPU、独立 source 可分布；源码契约测试确认 queue executor 没有 periodic timeout。Ethernet 的 padding、ARP deferred frame、malformed frame 和 pending buffer 测试继续验证 `/proc/net/dev` 的 L2 长度/error/drop 口径。`net/ax-net/tests/std.rs` 的 6 个 public API 集成测试仅在 `host-test` feature 下构建。

`DeviceBinding` 使用 atomic raw ifindex 保存，这个测试验证 public 语义不会因为内部原子编码而丢失。

### DMA 与批次提交回归

`queue_runtime/tests.rs` 覆盖 RX token 消费前不归还、回收 ring 满时保留所有权、
直接填充 TX DMA buffer，以及提交选项跨 FIFO 重试的保留。检查 frame 内容时也核对
buffer 地址，避免一次额外复制仍通过相同内容断言。`rd-net` 测试检查 replacement
分配和 `SubmitError` 返回原 token；RTL8125 测试检查 checksum descriptor 编码及约束。

板端验证还需要确认每轮退出前的 `flush()` 真正推动已发布发送、replacement refill
不会饿死 RX、checksum offload 下接收端数据正确。Orange Pi 5 Plus 的 iperf3 矩阵
使用 `apps/starry/iperf3/iperf-bench.sh`，记录构建提交、FIT 与脚本 SHA-256、链路速率、
每轮 receiver 结果。吞吐数据单独记录，不能替代 token 生命周期与 IRQ 状态机断言。

## 3. StarryOS 系统测试

StarryOS 系统测试在 QEMU 中运行真实用户态程序和 syscall 路径，覆盖单元测试无法观察的 ABI 编解码、fd 生命周期和 proc/netlink 输出。测试分组位于 `test-suit/starryos/qemu/system`，应通过 xtask 入口运行以保持镜像、参数和成功正则一致。

### 3.1 运行方式

完整 QEMU system 组会构建 StarryOS、启动 QEMU 并按 case 配置执行用户态测试，适合验证网络改动的主要 ABI 回归。定向调试时可以缩小 case，但最终仍应回到相同 xtask 流程，避免原生命令遗漏 feature 或运行参数。

```bash
cargo xtask starry test qemu --arch riscv64 -c qemu-smp1/system
```

常用跨架构回归：

```bash
cargo xtask starry test qemu --arch riscv64
cargo xtask starry test qemu --arch loongarch64
```

system 测试使用 StarryOS guest 内的 Linux 用户态程序验证 syscall/ABI 层。网络相关用例主要覆盖以下几类。

### 3.2 Socket 数据面

Socket 数据面用例从 StarryOS 用户态调用真实 syscall，覆盖连接、收发、poll 和选项语义，再由 `ax-net` 与 smoltcp 完成协议推进。表中的测试位置用于定位失败属于 syscall 翻译、socket backend 还是底层数据路径。

| 测试 | 位置 | 覆盖点 |
| --- | --- | --- |
| `syscall-test-socket-dataplane` | `test-suit/starryos/qemu/system/syscall-test-socket-dataplane` | TCP/UDP/raw socket 数据面基础行为 |
| `bugfix-bug-tcp-send-no-epoll-notify` | `test-suit/starryos/qemu/system/bugfix-bug-tcp-send-no-epoll-notify` | TCP send 后 epoll waiter 唤醒 |
| `test-tcp-napi-runtime` | `test-suit/starryos/qemu/system/test-tcp-napi-runtime` | blocking/nonblocking connect+accept、send/recv、poll/epoll、peer close、socket wait 的 signal/EINTR |
| `bugfix-bug-ip-mtu-discover-udp-flush` | `test-suit/starryos/qemu/system/bugfix-bug-ip-mtu-discover-udp-flush` | `IP_MTU_DISCOVER` 读回和 UDP send 后立即 close 的交付 |
| `syscall-test-so-reuseport` | `test-suit/starryos/qemu/system/syscall-test-so-reuseport` | TCP/UDP `SO_REUSEPORT` 共同绑定边界 |
| `c-regression-test-icmp-ping-socket` | `test-suit/starryos/qemu/system/c-regression-test-icmp-ping-socket` | IPv4 ICMP ping datagram socket |

Socket 数据面表覆盖真实 syscall 到 smoltcp 的主路径，但不检查管理接口是否读取同一控制面。下一节通过 ioctl、netlink 与 procfs 补充这些只读和运行期更新视图。

### 3.3 Linux 管理 ABI

Linux 管理 ABI 用例验证 ioctl、rtnetlink 和 procfs 是否观察到同一份 `NetControl` 与设备统计快照。这里不仅检查结构体编码，还要确认运行期地址更新会实际改变路由和后续发包行为。

| 测试 | 位置 | 覆盖点 |
| --- | --- | --- |
| `bugfix-bug-netlink-getlink` | `test-suit/starryos/qemu/system/bugfix-bug-netlink-getlink` | `RTM_GETLINK`、`SIOCGIFTXQLEN`、link 属性 |
| `bugfix-bug-netlink-getaddr` | `test-suit/starryos/qemu/system/bugfix-bug-netlink-getaddr` | `RTM_GETADDR`、loopback address、link/address dump |
| `c-regression-test-netlink-rtnetlink` | `test-suit/starryos/qemu/system/c-regression-test-netlink-rtnetlink` | route dump、IPv4 `RTM_NEWADDR/DELADDR` 及错误映射 |
| `c-regression-test-socket-device-ioctl` | `test-suit/starryos/qemu/system/c-regression-test-socket-device-ioctl` | 跨 socket family 的 `SIOCGIFNAME`/device ioctl |
| `syscall-test-netlink-recvmsg` | `test-suit/starryos/qemu/system/syscall-test-netlink-recvmsg` | netlink recvmsg 基础语义 |
| `bugfix-bug-proc-net-arp` | `test-suit/starryos/qemu/system/bugfix-bug-proc-net-arp` | `/proc/net/arp` 格式和真实 device 字段 |
| `syscall-test-procstats` | `test-suit/starryos/qemu/system/syscall-test-procstats` | `/proc/net/dev` 列格式与 loopback 真实计数增长 |

管理 ABI 表要求多个接口共享 `InterfaceId` 和状态来源，能够捕获结构编码正确但数据源分裂的问题。AF_PACKET 使用二层地址与 frame 语义，因此需要在相同身份基础上另行验证。

### 3.4 AF_PACKET

AF_PACKET 测试关注接口选择、二层地址和 frame 收发，与普通 `SocketOps` 的 IP 数据路径不同。用例需要同时核对 `sockaddr_ll`、ifindex 转换和 StarryOS packet socket 的 namespace 可见性，避免只验证模拟返回值。

| 测试 | 位置 | 覆盖点 |
| --- | --- | --- |
| `bugfix-bug-packet-arping` | `test-suit/starryos/qemu/system/bugfix-bug-packet-arping` | `AF_PACKET` bind、`SIOCGIFINDEX`、`RTM_GETLINK` 一致性、模拟 gateway ARP reply |

Unix 辅助数据与 record 语义由 `syscall-test-seqpacket`、`bugfix-unix-passcred`、`bugfix-socket-timestamp`、`test-unix-msg-peek`、`test-unix-scm-rights` 和 `test-unix-cmsg-byte-marks` 覆盖，均位于 `test-suit/starryos/qemu/system/`。

这些 system 测试验证的是 StarryOS Linux ABI 层是否正确使用 `ax_net::interfaces()`、`InterfaceId`、`arp_entries()` 和 socket facade。它们不替代 `ax-net` crate 单元测试；两者覆盖层级不同。

### 3.5 E1000 SMP4 queue runtime

`qemu-e1000/system` 是独立的 x86_64 分组，使用 `ax-driver/intel-net`、四个 vCPU 和一块 QEMU E1000，避免默认 virtio-net 掩盖 E1000 的 IRQ、mask/rearm 与 queue-0 poll group 路径。

```bash
cargo xtask starry test qemu --arch x86_64 -c qemu-e1000/system
```

guest 用例 `test-e1000-napi-runtime` 等待 DHCP，然后 fork 两个进程并发从 QEMU user-network gateway 下载 payload。每个进程必须读取并逐字节校验完整的 4 MiB body；成功标记为 `E1000_NAPI_TEST_PASSED`。这能捕获只完成 DHCP、短包可用但 burst 期间 IRQ 未正确 rearm，或者两个 socket 并发时 queue/protocol generation 停止推进的问题。

rootfs 参数补全按 `-device` 的 `netdev=net0` 连接关系识别已有网卡，而不是维护 virtio/E1000 型号白名单。`ensure_disk_boot_net_preserves_custom_network_device_bound_to_net0` 固定该契约，防止测试配置被额外注入同名默认 NIC 后在 QEMU 启动前失败。

## 4. 双网卡集成测试

`apps/starry/qemu/dual-net` 是双网卡集成测试，用于验证多设备初始化、双 DHCP、route table、接口绑定、并发收发和较大 APK 下载校验。它是 Starry app 级 QEMU 场景，不属于 `test-suit/starryos` system 分组。

### 4.1 运行方式

双网卡 case 由 Starry xtask 读取 `qemu-smp1/system` 配置并注入两块网卡及测试脚本。运行前可以先列出 case 确认选择器命中，再执行定向命令；这样可以区分配置发现失败和 guest 内网络断言失败。

```bash
cargo xtask starry app list --kind qemu | rg "qemu/dual-net"
```

运行 riscv64：

```bash
cargo xtask starry app qemu -t qemu/dual-net --arch riscv64
```

运行 aarch64：

```bash
cargo xtask starry app qemu -t qemu/dual-net --arch aarch64
```

运行 x86_64：

```bash
cargo xtask starry app qemu -t qemu/dual-net --arch x86_64
```

运行 loongarch64：

```bash
cargo xtask starry app qemu -t qemu/dual-net --arch loongarch64
```

QEMU 配置：

| 架构 | 配置文件 |
| --- | --- |
| aarch64 | `apps/starry/qemu/dual-net/qemu-aarch64.toml` |
| loongarch64 | `apps/starry/qemu/dual-net/qemu-loongarch64.toml` |
| riscv64 | `apps/starry/qemu/dual-net/qemu-riscv64.toml` |
| x86_64 | `apps/starry/qemu/dual-net/qemu-x86_64.toml` |

case 表确认选择器、架构和分组位置，实际网络含义仍由 QEMU 设备参数与 guest 地址共同决定。拓扑章节把这些运行参数展开为两条可区分的数据路径，便于核对测试是否真正使用双网卡。

### 4.2 拓扑

双网卡场景为 guest 提供两个独立 Ethernet 设备和可区分的远端网络，用来验证 metric、源地址和 `SO_BINDTODEVICE` 是否真正影响 `Router::dispatch()`。拓扑中的地址角色必须与 QEMU 参数和 guest 检查脚本保持一致，否则测试可能退化为同一路径上的重复连通性检查。

```text
guest eth0
  -> virtio-net-pci net0
  -> QEMU user net 10.0.2.0/24
  -> DHCP address 10.0.2.15
  -> host gateway 10.0.2.2

guest eth1
  -> virtio-net-pci net1
  -> QEMU user net 10.0.3.0/24
  -> DHCP address 10.0.3.15
  -> host gateway 10.0.3.2

host HTTP server
  -> 127.0.0.1:18382 on host
  -> exposed through each QEMU user net gateway
  -> payload size 1 MiB, byte value 68

Alpine APK repositories
  -> accessed from guest through QEMU user networking
  -> apk fetch -R downloads package files and dependencies
  -> apk verify + sha256sum -c validates downloaded files
```

`qemu-*.toml` 会启动 host HTTP server：

```toml
[host_http_server]
bind = "127.0.0.1"
port = 18382
body_size = 1048576
body_byte = 68
```

guest 启动后自动执行：

```text
/usr/bin/dual-net-tests.sh
```

脚本来自 `apps/starry/qemu/dual-net/c/dual-net-tests.sh`。`prebuild.sh` 会安装 `curl`，`CMakeLists.txt` 会把 `curl` 和 `dual-net-tests.sh` 安装进 guest rootfs。`apk` 和 `sha256sum` 来自 Alpine rootfs 的基础工具集。

### 4.3 Guest 检查项

`dual-net-tests.sh` 在 guest 内从接口、路由、绑定和并行收发四个角度验证双网卡行为，并把每个断言的失败输出保留给宿主测试框架。检查项必须依赖两个不同接口和远端地址，才能证明 Router 的多设备路径而非单接口连通性。

- `ifconfig eth0` 或 `ip addr show eth0` 能看到 `10.0.2.15`。
- `ifconfig eth1` 或 `ip addr show eth1` 能看到 `10.0.3.15`。
- `curl --interface eth0 http://10.0.2.2:18382/payload.bin?...` 能下载至少 1 MiB。
- `curl --interface eth1 http://10.0.3.2:18382/payload.bin?...` 能下载至少 1 MiB。
- 串行下载完成后，再并发从 eth0/eth1 下载。
- `apk update` 能从 guest 访问 Alpine APK repository。
- `apk fetch -R -o /tmp/dual-net-apk-fetch python3` 能下载 `python3` 及依赖包。
- `apk update` 和 `apk fetch` 默认最多重试 3 次，避免外部 mirror 或 QEMU user networking 的短暂抖动导致误报。
- 下载到本地的 `.apk` 总大小必须不少于 8 MiB。
- 每个 `.apk` 必须通过 `apk verify`。
- 生成下载文件的 sha256 清单后，必须通过 `sha256sum -c` 回读校验。

成功输出包含：

```text
DUAL_NET_ETH0_ADDR_OK
DUAL_NET_ETH1_ADDR_OK
DUAL_NET_FETCH_ETH0_SINGLE_MS=... BYTES=1048576
DUAL_NET_FETCH_ETH1_SINGLE_MS=... BYTES=1048576
DUAL_NET_FETCH_ETH0_PARALLEL_MS=... BYTES=1048576
DUAL_NET_FETCH_ETH1_PARALLEL_MS=... BYTES=1048576
DUAL_NET_APK_FETCH_MS=... BYTES=... PACKAGES=... PACKAGE=python3
DUAL_NET_TEST_PASSED
```

失败输出以以下格式开始：

```text
DUAL_NET_TEST_FAILED: ...
```

guest 检查代码在同一 case 中验证接口、路由、绑定和并行请求，失败会保留对应命令输出。覆盖范围总结这些断言跨越的实现层，并指出它们不等同于压力或板卡测试。

### 4.4 覆盖范围

`dual-net` 覆盖从 runtime 设备注册、控制面 route metric 到 socket 设备绑定和并行数据面的完整链路。它不替代协议压力或物理板卡测试，但能够捕获接口列表正确而实际 TX 仍固定走单设备的集成错误。

- runtime 能收集两张 virtio-net 设备。
- `NetworkConfig` 默认 DHCP 策略能应用到未显式配置的 Ethernet 接口。
- `eth0` 和 `eth1` 能通过独立 DHCP 获取不同网段地址。
- route table 同时存在 `10.0.2.0/24` 和 `10.0.3.0/24` connected route。
- `curl --interface` 通过 Linux ABI 映射到接口绑定，限制 route lookup。
- 串行和并发下载验证独立 poll group、SPSC frame/token pipeline 与唯一 protocol executor 可以持续推进。
- `apk fetch -R` 下载较大的包集合并写入磁盘，验证较长 TCP 流、DNS、默认路由和文件写入路径的组合稳定性。
- `apk verify` 验证 APK 内置签名/完整性元数据，`sha256sum -c` 验证落盘文件再次读取后的内容一致性。

覆盖列表说明双网卡 case 同时验证控制面与真实 TX 路径，任一环节缺失都会降低测试价值。xtask 结构自检在运行前静态确认 case 仍包含两接口和并行传输等关键构件。

### 4.5 xtask 结构自检

`scripts/axbuild/src/starry/test/tests/asset_network_tests.rs` 中的 `dual_net_qemu_case_exercises_two_interfaces_and_parallel_fetches` 会静态检查 `dual-net` case 的结构：

- `c/dual-net-tests.sh`、`c/prebuild.sh`、`c/CMakeLists.txt` 必须存在。
- riscv64 和 x86_64 都必须有 `qemu-*.toml`。
- QEMU args 必须包含 `net0`、`net1` 两个 virtio-net-pci。
- net0 必须是 `10.0.2.0/24` 且 DHCP 起始地址为 `10.0.2.15`。
- net1 必须是 `10.0.3.0/24` 且 DHCP 起始地址为 `10.0.3.15`。
- `shell_init_cmd` 必须是 `/usr/bin/dual-net-tests.sh`。
- host HTTP server 必须监听 18382，payload 至少 1 MiB。
- `dual-net-tests.sh` 必须包含 `apk fetch -R`、APK 重试、`apk verify`、`sha256sum -c` 和 `DUAL_NET_APK_FETCH_MS`。
- QEMU timeout 必须足够覆盖 APK 下载校验流程。

这个结构测试防止 app 配置被误删、改成单网卡或失去自动 guest probe。

## 5. 常见失败定位

失败定位应从测试输出中的首个具体断言出发，再映射到配置、控制面、socket 或设备层，不应只根据最终汇总标记修改正则。以下小节把常见症状与代码锚点和检查命令对应起来，便于保留可重复的根因证据。

### 5.1 双网卡测试失败

`DUAL_NET_TEST_FAILED` 是集成脚本汇总标记，必须结合此前输出的接口、路由和具体连接断言定位根因。下表按可观察现象给出优先检查点，避免通过放宽成功正则掩盖配置或选路错误。

| 现象 | 优先检查 |
| --- | --- |
| `eth1 did not get 10.0.3.15` | 第二个 virtio-net 是否被 runtime 收集；默认 DHCP 是否应用到未显式配置接口；DHCP packet ingress `InterfaceId` 是否分发正确 |
| eth0 成功、eth1 curl 失败 | `SO_BINDTODEVICE` / `curl --interface` 是否映射到 eth1；route table 是否有 `10.0.3.0/24` connected route |
| 串行成功、并发失败 | 目标 group 是否被正确 schedule；SPSC 是否 backpressure；protocol generation 是否持续完成 |
| 下载字节数小于 1 MiB | TCP receive/send readiness、host HTTP server 暴露、QEMU user net 或 curl 超时 |
| `apk fetch too small` | APK package 依赖集合是否变化；`APK_STRESS_MIN_BYTES` 是否需要随 Alpine 版本调整 |
| `apk verify failed` 或 `sha256sum -c` 失败 | 长连接下载、TCP 重组、文件写入或读回路径存在数据损坏 |
| `apk update` 失败 | guest 默认路由、DNS、外网连通性、Alpine mirror 可达性 |
| 出现 `DUAL_NET_RETRY` 后最终通过 | 外部 APK 下载路径发生过短暂 I/O error，但最终文件完整性校验通过 |
| QEMU timeout | 是否缺少 `curl`、`ip`、`ifconfig`；shell init command 是否执行到 `DUAL_NET_TEST_PASSED` |

故障表将汇总标记拆回接口、路由、绑定和远端服务等具体观察点。若 Starry 分组框架只报告最终失败模式，下一节说明如何从更早的 test binary 输出恢复原始断言。

### 5.2 分组测试失败

`cargo xtask starry test qemu` 的汇总输出可能只显示匹配到失败模式。定位时应查更早的 test binary 输出：

```bash
rg -n "STARRY_GROUPED_TEST_FAILED|FAIL:|panic|assert|test-socket|bugfix-bug" target -g "*.log"
```

排查顺序：

1. 找到第一个打印 `FAIL:` 的 test binary。
2. 确认是否是网络 testcase，还是其它系统测试间接受网络超时影响。
3. 对照该 testcase 的源码，确认失败发生在 syscall 返回值、超时、内容不匹配还是权限语义。
4. 如果 riscv64 和 loongarch64 都失败，优先怀疑协议栈/ABI 逻辑；如果只在单架构失败，再检查原子、调度和定时器。

分组失败排查列表要求回到具体 test binary 输出和 case 配置，而不是修改汇总正则。路由缺失是其中最常见的网络状态错误，需要从接口、地址与共享规则逐层定位。

### 5.3 路由缺失

`no route to destination` 表明 socket 预选路或 Router dispatch 没有找到满足目标、源地址与设备绑定的规则。排查时应同时查看接口 IPv4 快照和 default/connected route，确认 DHCP 或静态提交已经更新同一 `SharedRouteTable`。

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

路由缺失示例强调测试应断言结构化 route decision，而不是解析源码或仅匹配日志。地址冲突需要转向 TCP/UDP side table 和 reuse 条件，属于另一类状态仲裁问题。

### 5.4 地址冲突

`address already in use` 来自 TCP/UDP 端口仲裁或重复接口配置，需要区分 wildcard、具体地址、设备绑定和 reuse 组合。排查应先核对 side table 中参与者的完整 bind key，再判断是否是测试清理遗漏或实现错误。

- 是否已有 wildcard bind 占用同一端口。
- 是否已有具体地址 bind 与新 bind 冲突。
- TCP listen 是否被 `ListenTable` 的 wildcard/specific 规则拒绝。
- 共同绑定是否由所有参与者都设置 `SO_REUSEPORT`，且 address/port 完全相同；`SO_REUSEADDR` 不会跳过 side table。
- 绑定具体本地地址时，该地址是否属于当前接口 registry。

地址冲突排查列表区分真实端口占用、reuse 规则和测试清理问题，修复应落在最低层可复现契约。接口视图不一致则属于数据源或 ABI 过滤问题，需要使用 `InterfaceId` 交叉核对多个接口。

### 5.5 接口视图不一致

AF_PACKET、ioctl、netlink 与 procfs 视图不一致通常意味着某个 ABI 路径绕过了 `ax-net` 快照或使用了不同 namespace 过滤。应以 `InterfaceId` 为共同身份逐项比对名称、地址、flags 和统计，而不是依赖列表顺序。

- `SIOCGIFINDEX` 是否来自 `InterfaceId::to_linux_ifindex()`。
- `RTM_GETLINK` 是否遍历同一份 `ax_net::interfaces()`。
- `sockaddr_ll.sll_ifindex` 是否能通过 `InterfaceId::from_linux_ifindex()` 反查接口。
- namespace 可见性过滤是否导致接口在某条路径可见、另一条路径不可见。

接口视图排查列表要求 ioctl、netlink、procfs 和 AF_PACKET 共享同一接口身份与控制面快照。当前限制章节说明即使这些一致性测试通过，仍有哪些协议、硬件和并行场景未被覆盖。

## 6. 当前限制

测试限制说明哪些结论尚不能从当前自动化覆盖中推出，并把协议功能缺口与测试基础设施缺口分开。新增测试时应优先覆盖确定的最低层状态转换，再补充 QEMU 或物理板卡证据，避免只增加宽泛的成功字符串。

### 6.1 测试覆盖限制

当前测试组合对 route/bind 数据结构和 StarryOS 常用路径覆盖较强，但对长期运行、真实硬件中断和高并发压力覆盖有限。以下限制意味着单元测试通过只能证明局部状态机契约，不能推出所有设备和时序组合均已验证。

- crate 单元测试主要覆盖纯 Rust 数据结构和 route/bind 语义，不启动真实 smoltcp 端到端 TCP 会话。
- `dual-net` 使用 QEMU user networking，不覆盖 tap/bridge、真实 NIC IRQ/DMA、RSS 或多队列网卡。
- `dual-net` 验证双 DHCP 和接口绑定下载，但不验证 link down/up、热插拔和运行期 route 删除。
- StarryOS system 测试覆盖 Linux ABI 观测面，不直接检查 `Router` 内部队列长度或每包分配情况。
- vsock、DHCP server 和真实板卡 IRQ/DMA 路径仍需要更多专门测试资产；Unix cmsg 已有 system 回归，但 crate 内部单元覆盖仍较少。

覆盖限制列表表明物理 IRQ、长期 lease 和压力场景仍需要板卡或专门测试，不能从 QEMU system 结果推断。协议功能限制进一步界定当前主路径本身尚未承诺的 IPv6 与组播能力。

### 6.2 协议功能限制

协议功能限制来自当前物理 Ethernet 主路径、smoltcp 配置和外围兼容层的共同边界。测试计划应围绕已声明支持的 IPv4、ARP、TCP/UDP/raw 与控制协议编写，不能把编译可用但尚未接入设备路径的能力当作稳定契约。

- IPv6 route、NDP、MLD 和完整 IPv6 socket 语义未作为主路径完善。
- IGMP/按接口 multicast membership 不完整。
- DHCP lease renew/rebind、租约过期回收和地址冲突检测仍需继续补齐。
- DNS 不包含 split DNS、search domain 和完整 `/etc/resolv.conf` 语义。
- `SO_REUSEPORT` 已支持共同 bind/listen，但 TCP incoming 只选择第一个匹配 listener，没有 Linux reuseport hash/load balancing；完整 Linux TCP option 集合和高级拥塞控制仍不在当前范围。

协议限制列表用于防止为未支持能力编写只检查字符串的伪测试，也提示新增功能需要相应最低层行为覆盖。架构限制则解释现有测试中的性能和并行结果受单协议核心约束。

### 6.3 架构限制

架构限制决定压力测试结果应如何解释：独立 IRQ domain 的 queue/DMA 可以并行，但协议核心和全局 `SocketSet` 仍由单个推进者串行访问。以下约束提示性能回归定位应区分 queue budget、SPSC backpressure、拷贝与 protocol poll，而不是简单归因于某个 driver。

- 协议核心仍是单 smoltcp `Interface + SocketSet`，TCP/UDP 状态机本身不多核并行。
- 多设备 dataplane 已使用 queue-level NAPI 状态机；当前生产 backend 仍只发布 queue-0 group，未启用 RSS 或真实硬件多队列。
- DMA RX 已覆盖 token 保留到 smoltcp 消费后的回收，TX 可直接组帧；非 DMA 端口、FIFO 积压以及 socket/user buffer 仍有复制。
- 不提供用户态端到端 zero-copy；现有 DMA token 测试不能替代真实硬件的 cache maintenance 和 descriptor ordering 验证。
- StarryOS network namespace 当前主要是可见性过滤，不是完整 per-namespace network stack。

这些限制使当前测试更适合作为功能与边界回归，而不是完整性能或硬件兼容认证。扩展覆盖时应保持失败可定位、命令可复现，并让新增用例真正编译和运行目标实现。
