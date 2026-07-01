---
sidebar_position: 12
sidebar_label: "测试与限制"
---

# 测试与限制

本文说明 `ax-net` 现有测试资产、运行方式、覆盖范围和当前限制。测试分三层：`net/ax-net` crate 内单元测试验证协议栈内部数据结构和路由/绑定语义；StarryOS system 测试验证 Linux ABI 观测面；`apps/starry/qemu/dual-net` 验证双网口 DHCP、路由和并发数据面。

## 测试资产

| 层级 | 位置 | 作用 |
| --- | --- | --- |
| crate 单元测试 | [net/ax-net/src](net/ax-net/src) 内各 `#[cfg(test)]` 模块 | 验证 `RouteTable`、`NetControl`/`Service`、TCP/UDP 设备绑定、UDP bind 表、TCP listen 表和通用 socket option |
| StarryOS system 测试 | [test-suit/starryos/qemu-smp1/system](test-suit/starryos/qemu-smp1/system) | 验证 Linux socket syscall、ioctl、AF_PACKET、netlink、procfs 等 ABI |
| dual-net 集成测试 | [apps/starry/qemu/dual-net](apps/starry/qemu/dual-net) | 验证两张 virtio-net、双 DHCP、接口绑定下载和并发数据面 |
| xtask 结构自检 | [scripts/axbuild/src/starry/test/tests/asset_network_tests.rs](scripts/axbuild/src/starry/test/tests/asset_network_tests.rs) | 验证 `dual-net` app 配置必须包含双网卡、host HTTP fixture 和 guest probe |

## `ax-net` 单元测试

### 运行方式

```bash
cargo test -p ax-net
```

`ax-net` 单元测试是 host-side Rust 测试，主要覆盖不依赖真实 QEMU 设备的内部逻辑。部分测试会使用 [lib.rs](net/ax-net/src/lib.rs) 中的 `test_support` 构造一个 split-route 测试网络：

```text
LOCAL_IF = InterfaceId(2), LOCAL_ADDR = 10.0.2.15
PEER_IF  = InterfaceId(3), PEER_ADDR  = 10.0.3.15
```

`network_test_guard()` 用全局 mutex 串行化会初始化全局网络状态的测试，避免 `SERVICE`、`NET_CONTROL`、`SOCKET_SET` 这类全局单例在并发 host test 中互相污染。

### RouteTable

位置：[router.rs](net/ax-net/src/router.rs)

| 测试 | 覆盖点 |
| --- | --- |
| `route_lookup_uses_longest_prefix` | 最长前缀优先，`10.0.1.0/24` 优先于默认路由 |
| `route_lookup_uses_metric_for_same_prefix` | 同前缀按 metric 小者优先 |
| `route_lookup_keeps_stable_order_for_equal_metric` | 同前缀、同 metric 时保持插入顺序 |
| `route_lookup_skips_unusable_interface` | `select_route_if()` 可通过闭包跳过不可用接口 |
| `default_routes_only_reports_zero_prefix_ipv4_rules` | `default_routes()` 只导出 IPv4 `0.0.0.0/0` 规则 |
| `bounded_packet_queue_reports_full_and_preserves_order` | 有界队列满时返回错误，并保持 FIFO |

这些测试对应多网口 route decision 的核心排序规则：最长前缀、metric、稳定顺序和接口可用性过滤。

### Service / DHCP 地址状态

位置：[service.rs](net/ax-net/src/service.rs)

| 测试 | 覆盖点 |
| --- | --- |
| `dhcp_configured_is_true_once_any_interface_has_address` | 多 DHCP 接口中只要任一接口已获得地址，bootstrap 状态即可视为完成 |
| `interface_address_table_handles_loopback_and_two_ethernet_addresses` | smoltcp `Interface` address list 能同时保存 loopback、eth0、eth1 IPv4 |

这组测试防止网络初始化重新退化为“只看第一个网卡”或“接口地址表只能容纳单 Ethernet 地址”的模型。

### TCP 设备绑定

位置：[tcp.rs](net/ax-net/src/tcp.rs)

| 测试 | 覆盖点 |
| --- | --- |
| `tcp_info_reports_default_socket_metrics` | `TCP_INFO` 在 closed socket 上返回稳定默认字段 |
| `connect_preserves_bound_interface` | TCP bind 到具体本地地址后，connect 不会被 peer route 改写绑定接口 |
| `connect_uses_peer_route_when_unbound` | wildcard bind 的 TCP connect 根据目的地址 route decision 选择接口 |
| `connect_rejects_unroutable_bound_device` | 显式绑定到不可达接口后，connect 返回错误并保留原绑定 |

这组测试覆盖 `SO_BINDTODEVICE` 和本地地址推导出的 `DeviceBinding` 对 TCP connect 的影响。

### UDP 设备绑定

位置：[udp.rs](net/ax-net/src/udp.rs)

| 测试 | 覆盖点 |
| --- | --- |
| `connect_preserves_bound_interface` | UDP bind 到具体本地地址后，connect 不会改写绑定接口 |
| `connect_uses_peer_route_when_unbound` | wildcard bind 的 UDP connect 根据目的地址 route decision 选择接口 |
| `connect_rejects_unroutable_bound_device` | 显式绑定到不可达接口后，connect 返回错误并保留原绑定 |

UDP 的测试与 TCP 对齐，重点是 datagram socket 的 connected peer 不应破坏本地地址绑定语义。

### UDP Bind Side Table

位置：[wrapper.rs](net/ax-net/src/wrapper.rs)

| 测试 | 覆盖点 |
| --- | --- |
| `udp_bind_rules_allow_distinct_specific_addresses` | 相同端口可绑定到不同具体本地地址；相同地址冲突；wildcard 与具体地址冲突 |
| `udp_bind_rules_reject_specific_after_wildcard` | 已存在 wildcard bind 时拒绝后续具体地址 bind |

这些测试补齐 smoltcp UDP socket 之外的 Linux 风格 wildcard/specific bind 仲裁。

### TCP ListenTable

位置：[listen_table.rs](net/ax-net/src/listen_table.rs)

| 测试 | 覆盖点 |
| --- | --- |
| `allows_same_port_on_distinct_specific_addresses` | 同端口可以在不同具体地址上 listen |
| `wildcard_listener_conflicts_with_specific_addresses` | wildcard listener 与任一具体地址 listener 冲突 |

这组测试覆盖 per-address listen 的冲突规则，是 wildcard listen、`0.0.0.0:port` 和多本地地址共存语义的基础。

### GeneralOptions

位置：[general.rs](net/ax-net/src/general.rs)

| 测试 | 覆盖点 |
| --- | --- |
| `device_binding_round_trips_none_and_some_interface` | `DeviceBinding` 在 `GeneralOptions` 中可以从 none 到指定接口再回到 none |

`DeviceBinding` 使用 atomic raw ifindex 保存，这个测试验证 public 语义不会因为内部原子编码而丢失。

## StarryOS system 测试

### 运行方式

完整 QEMU system 组：

```bash
cargo xtask starry test qemu --arch riscv64 -c qemu-smp1/system
```

常用跨架构回归：

```bash
cargo xtask starry test qemu --arch riscv64
cargo xtask starry test qemu --arch loongarch64
```

system 测试使用 StarryOS guest 内的 Linux 用户态程序验证 syscall/ABI 层。网络相关用例主要覆盖以下几类。

### Socket Dataplane

| 测试 | 位置 | 覆盖点 |
| --- | --- | --- |
| `syscall-test-socket-dataplane` | [test-suit/starryos/qemu-smp1/system/syscall-test-socket-dataplane](test-suit/starryos/qemu-smp1/system/syscall-test-socket-dataplane) | TCP/UDP/raw socket 数据面基础行为 |
| `bugfix-bug-tcp-send-no-epoll-notify` | [test-suit/starryos/qemu-smp1/system/bugfix-bug-tcp-send-no-epoll-notify](test-suit/starryos/qemu-smp1/system/bugfix-bug-tcp-send-no-epoll-notify) | TCP send 后 epoll waiter 唤醒 |

### ioctl / netlink / procfs

| 测试 | 位置 | 覆盖点 |
| --- | --- | --- |
| `bugfix-bug-netlink-getlink` | [test-suit/starryos/qemu-smp1/system/bugfix-bug-netlink-getlink](test-suit/starryos/qemu-smp1/system/bugfix-bug-netlink-getlink) | `RTM_GETLINK`、`SIOCGIFTXQLEN`、link 属性 |
| `bugfix-bug-netlink-getaddr` | [test-suit/starryos/qemu-smp1/system/bugfix-bug-netlink-getaddr](test-suit/starryos/qemu-smp1/system/bugfix-bug-netlink-getaddr) | `RTM_GETADDR`、loopback address、link/address dump |
| `syscall-test-netlink-recvmsg` | [test-suit/starryos/qemu-smp1/system/syscall-test-netlink-recvmsg](test-suit/starryos/qemu-smp1/system/syscall-test-netlink-recvmsg) | netlink recvmsg 基础语义 |
| `bugfix-bug-proc-net-arp` | [test-suit/starryos/qemu-smp1/system/bugfix-bug-proc-net-arp](test-suit/starryos/qemu-smp1/system/bugfix-bug-proc-net-arp) | `/proc/net/arp` 格式、device 字段和固定 gateway stub 回归 |

### AF_PACKET

| 测试 | 位置 | 覆盖点 |
| --- | --- | --- |
| `bugfix-bug-packet-arping` | [test-suit/starryos/qemu-smp1/system/bugfix-bug-packet-arping](test-suit/starryos/qemu-smp1/system/bugfix-bug-packet-arping) | `AF_PACKET` bind、`SIOCGIFINDEX`、`RTM_GETLINK` 一致性、模拟 gateway ARP reply |

这些 system 测试验证的是 StarryOS Linux ABI 层是否正确使用 `ax_net::interfaces()`、`InterfaceId`、`arp_entries()` 和 socket facade。它们不替代 `ax-net` crate 单元测试；两者覆盖层级不同。

## dual-net 集成测试

`apps/starry/qemu/dual-net` 是双网卡集成测试，用于验证多设备初始化、双 DHCP、route table、接口绑定、并发收发和较大 APK 下载校验。它是 Starry app 级 QEMU 场景，不属于 `test-suit/starryos` system 分组。

### 运行方式

列出 case：

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
| aarch64 | [apps/starry/qemu/dual-net/qemu-aarch64.toml](apps/starry/qemu/dual-net/qemu-aarch64.toml) |
| loongarch64 | [apps/starry/qemu/dual-net/qemu-loongarch64.toml](apps/starry/qemu/dual-net/qemu-loongarch64.toml) |
| riscv64 | [apps/starry/qemu/dual-net/qemu-riscv64.toml](apps/starry/qemu/dual-net/qemu-riscv64.toml) |
| x86_64 | [apps/starry/qemu/dual-net/qemu-x86_64.toml](apps/starry/qemu/dual-net/qemu-x86_64.toml) |

### 拓扑

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

脚本来自 [apps/starry/qemu/dual-net/c/dual-net-tests.sh](apps/starry/qemu/dual-net/c/dual-net-tests.sh)。[prebuild.sh](apps/starry/qemu/dual-net/c/prebuild.sh) 会安装 `curl`，[CMakeLists.txt](apps/starry/qemu/dual-net/c/CMakeLists.txt) 会把 `curl` 和 `dual-net-tests.sh` 安装进 guest rootfs。`apk` 和 `sha256sum` 来自 Alpine rootfs 的基础工具集。

### Guest 检查项

`dual-net-tests.sh` 执行以下检查：

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

### 覆盖范围

`dual-net` 覆盖：

- runtime 能收集两张 virtio-net 设备。
- `NetworkConfig` 默认 DHCP 策略能应用到未显式配置的 Ethernet 接口。
- `eth0` 和 `eth1` 能通过独立 DHCP 获取不同网段地址。
- route table 同时存在 `10.0.2.0/24` 和 `10.0.3.0/24` connected route。
- `curl --interface` 通过 Linux ABI 映射到接口绑定，限制 route lookup。
- 串行和并发下载验证 per-device TX queue、共享 RX queue、device worker 和 net-poll worker 可以持续推进。
- `apk fetch -R` 下载较大的包集合并写入磁盘，验证较长 TCP 流、DNS、默认路由和文件写入路径的组合稳定性。
- `apk verify` 验证 APK 内置签名/完整性元数据，`sha256sum -c` 验证落盘文件再次读取后的内容一致性。

### xtask 结构自检

[scripts/axbuild/src/starry/test/tests/asset_network_tests.rs](scripts/axbuild/src/starry/test/tests/asset_network_tests.rs) 中的 `dual_net_qemu_case_exercises_two_interfaces_and_parallel_fetches` 会静态检查 `dual-net` case 的结构：

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

## 常见失败定位

### `DUAL_NET_TEST_FAILED`

| 现象 | 优先检查 |
| --- | --- |
| `eth1 did not get 10.0.3.15` | 第二个 virtio-net 是否被 runtime 收集；默认 DHCP 是否应用到未显式配置接口；DHCP packet ingress `InterfaceId` 是否分发正确 |
| eth0 成功、eth1 curl 失败 | `SO_BINDTODEVICE` / `curl --interface` 是否映射到 eth1；route table 是否有 `10.0.3.0/24` connected route |
| 串行成功、并发失败 | RX/TX worker 是否被正确唤醒；队列是否满；net-poll worker 是否持续 poll |
| 下载字节数小于 1 MiB | TCP receive/send readiness、host HTTP server 暴露、QEMU user net 或 curl 超时 |
| `apk fetch too small` | APK package 依赖集合是否变化；`APK_STRESS_MIN_BYTES` 是否需要随 Alpine 版本调整 |
| `apk verify failed` 或 `sha256sum -c` 失败 | 长连接下载、TCP 重组、文件写入或读回路径存在数据损坏 |
| `apk update` 失败 | guest 默认路由、DNS、外网连通性、Alpine mirror 可达性 |
| 出现 `DUAL_NET_RETRY` 后最终通过 | 外部 APK 下载路径发生过短暂 I/O error，但最终文件完整性校验通过 |
| QEMU timeout | 是否缺少 `curl`、`ip`、`ifconfig`；shell init command 是否执行到 `DUAL_NET_TEST_PASSED` |

### `STARRY_GROUPED_TEST_FAILED`

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

### AF_PACKET / netlink 不一致

排查方向：

- `SIOCGIFINDEX` 是否来自 `InterfaceId::to_linux_ifindex()`。
- `RTM_GETLINK` 是否遍历同一份 `ax_net::interfaces()`。
- `sockaddr_ll.sll_ifindex` 是否能通过 `InterfaceId::from_linux_ifindex()` 反查接口。
- namespace 可见性过滤是否导致接口在某条路径可见、另一条路径不可见。

## 当前限制

### 测试覆盖限制

- crate 单元测试主要覆盖纯 Rust 数据结构和 route/bind 语义，不启动真实 smoltcp 端到端 TCP 会话。
- `dual-net` 使用 QEMU user networking，不覆盖 tap/bridge、真实 NIC IRQ/DMA、RSS 或多队列网卡。
- `dual-net` 验证双 DHCP 和接口绑定下载，但不验证 link down/up、热插拔和运行期 route 删除。
- StarryOS system 测试覆盖 Linux ABI 观测面，不直接检查 `Router` 内部队列长度或每包分配情况。
- vsock、Unix cmsg、DHCP server、OOB RX 仍需要更多专门测试资产。

### 协议与功能限制

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
- StarryOS network namespace 当前主要是可见性过滤，不是完整 per-namespace network stack。
