# `ax-net`

> 路径：`net/ax-net`
> 类型：库 crate
> 分层：根目录通用网络子系统
> 版本：`0.7.1`
> 文档依据：`net/ax-net/Cargo.toml`、`net/ax-net/src/lib.rs`、`net/ax-net/src/socket.rs`、`net/ax-net/src/tcp.rs`、`net/ax-net/src/udp.rs`、`net/ax-net/src/raw.rs`、`net/ax-net/src/unix/*`、`net/ax-net/src/vsock/*`、`net/ax-net/src/service.rs`、`net/ax-net/src/router.rs`

`ax-net` 是仓库唯一的网络栈 crate。它位于根目录 `net/ax-net`，同时服务 ArceOS、StarryOS 以及后续其他系统。旧的 ArceOS-local 网络实现已经不再作为可用入口存在；所有网络 feature 都收敛到 `net`。

## 架构设计

`ax-net` 内部分成五层：

| 层次 | 主要模块 | 职责 |
| --- | --- | --- |
| public facade | `lib.rs` | 暴露初始化、轮询、socket、设备和状态查询 API |
| socket abstraction | `socket.rs`、`options.rs`、`general.rs` | 定义 `Socket`、`SocketOps`、`SocketAddrEx`、socket option、nonblocking/timeout 等公共语义 |
| address-family implementations | `tcp.rs`、`udp.rs`、`raw.rs`、`unix/`、`vsock/` | 实现 TCP、UDP、raw ICMP、Unix domain socket 和可选 vsock |
| protocol service | `service.rs`、`router.rs`、`wrapper.rs`、`listen_table.rs`、`state.rs` | 管理 smoltcp interface、SocketSet、路由、DHCP/DNS 状态和 waker |
| device adaptation | `device/` | 将 `rd-net` / `rdif-vsock` 设备接入网络栈，不包含具体硬件驱动 |

`router`、`service`、`wrapper`、`listen_table`、`state` 默认保持 crate-private，外部系统不应绑定这些内部实现细节。

## Public API

运行时初始化入口：

- `init_network(net_devs)`：由 `ax-runtime` 调用，接入 Ethernet 设备、loopback、router、smoltcp service，并完成静态网络或 DHCP 初始化。
- `init_vsock(vsock_devs)`：在 `vsock` feature 下启用，由 `ax-runtime` 调用。
- `request_poll()`：唤醒 net-poll worker 推动收包、发包、协议状态机和 readiness 更新。

socket 入口：

- `tcp::TcpSocket`
- `udp::UdpSocket`
- `raw`
- `unix`
- `vsock`（可选）
- `Socket`、`SocketOps`、`SocketAddrEx`、`SendOptions`、`RecvOptions`、`Shutdown`
- `options::{Configurable, GetSocketOption, SetSocketOption, TcpInfo, UnixCredentials}`

设备与状态查询：

- `EthernetDeviceList`、`EthernetDriver`、`RdNetDriver` 等设备适配类型。
- `arp_entries()`：查询 ARP 状态。
- `dns_query(name)`：基于 smoltcp DNS socket 的正式 DNS 查询 API。
- `dns_servers()`：只读查询当前 DNS server 列表。

## 网络配置

`ax-net` 支持静态网络和 DHCP：

- 当 `AX_IP` 和 `AX_GW` 都存在时，使用静态 IP / 网关配置。
- 否则启用 DHCP。
- DNS server 优先使用 DHCP 下发值；其次使用静态 `AX_DNS`。
- 如果没有 DNS server，`dns_query()` 返回错误，不硬编码默认 DNS。

## 依赖关系

直接依赖：

| 依赖 | 作用 |
| --- | --- |
| `smoltcp` | TCP/IP、DHCP、DNS 等协议核心 |
| `axpoll` | readiness、waker 与 `Pollable` 语义 |
| `ax-task` | 阻塞等待、超时、后台任务 |
| `ax-sync` / `ax-kspin` | 锁与同步 |
| `ax-io` | I/O buffer 与读写抽象 |
| `ax-errno` | `AxError` / `AxResult` |
| `ax-config` | `AX_IP`、`AX_GW`、`AX_DNS` 等构建期配置 |
| `ax-hal` | 时间等底层能力 |
| `ax-fs-ng` / `axfs-ng-vfs` | Unix domain socket 路径绑定 |
| `rd-net` | Ethernet queue/DMA 设备抽象 |
| `rdif-vsock` | 可选 vsock 设备抽象 |

主要消费者：

- `ax-runtime`：网络和 vsock 初始化。
- `ax-feat`：`net` / `vsock` feature 装配。
- `ax-api`、`ax-posix-api`：ArceOS 网络 API 和 POSIX socket 路径。
- `ax-std`、`ax-libc`：通过上层 API 间接消费网络能力。
- `starry-kernel`：Linux syscall socket 底层实现。

具体网卡驱动、VirtIO/PCI 探测和 DMA 策略仍属于 `drivers/` 与 `ax-driver`；StarryOS 的 `AF_PACKET`、`AF_NETLINK`、Linux sockaddr 编解码和 ioctl 细节仍属于 StarryOS Linux ABI 层。

## 测试建议

- `cargo xtask clippy --package ax-net`
- ArceOS TCP/UDP/DNS 示例与测试。
- StarryOS TCP/UDP、epoll、AF_UNIX、raw ICMP、vsock build 测试。
- DHCP/QEMU user networking 场景，确认 `eth0`、默认路由和 DNS server 获取正常。
