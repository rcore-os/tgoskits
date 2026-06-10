# 统一网络模块为 `ax-net` 的方案

## 1. 背景

当前仓库存在两套网络 crate：

- `ax-net`：老一代 ArceOS 网络模块，提供 `TcpSocket`、`UdpSocket`、`dns_query`、`poll_interfaces`、`bench_*`，主要被 `ax-api`、`ax-posix-api`、`axstd`、`axlibc` 使用。
- `ax-net-ng`：实际更完整的新网络栈，支持 TCP、UDP、raw ICMP、Unix domain socket、vsock、loopback、router、DHCP、socket options、poll/waker，StarryOS 已经通过依赖别名 `axnet = { package = "ax-net-ng" }` 使用它。

当前问题：

- `ax-runtime`、`ax-feat` 同时保留 `net` 和 `net-ng` 两套 feature/初始化路径。
- `ax-net` 又依赖 `ax-net-ng` 的驱动适配类型，形成“旧 API 壳 + 新组件片段”的混合状态。
- StarryOS 的对外依赖名是 `axnet`，但真实 package 是 `ax-net-ng`，命名和架构不一致。
- ArceOS 公共 API 仍走旧 `ax-net`，无法获得 `ax-net-ng` 的完整 socket 能力。

目标：

- 仅保留 `ax-net` 作为唯一网络 crate/package。
- 在仓库根目录新增 `net/ax-net`，以当前 `ax-net-ng` 为主体实现统一网络栈，并以清晰、统一的新 API 作为唯一对外接口。
- 移除 `os/arceos/modules/axnet` 和 `os/arceos/modules/axnet-ng` 两个 ArceOS-local 网络 crate，删除对应 workspace dependency、feature 路径和 runtime 分支。
- 统一 ArceOS 与 StarryOS 的网络能力入口，降低重复实现和 feature 组合风险。
- 将网络栈从 ArceOS modules 目录中提升为根目录通用组件，使其成为 ArceOS、StarryOS 以及后续其他系统可复用的统一 net 支持。
- 不保留旧 `ax-net` 的兼容 wrapper、旧 feature alias 或旧 benchmark stub；所有调用方在本轮同步迁移到统一 API。

## 2. 修改方案

### 2.1 总体架构

**目录位置**：`net/ax-net/`
- 在仓库根目录新增 `net/` 顶层目录，其中放置 `ax-net/` 网络模块
- 理由：网络栈作为跨 ArceOS、StarryOS、Axvisor 的核心基础设施，足够重要，值得独立的顶层目录
- 与 `drivers/`、`platforms/`、`components/` 等平级，突出其作为独立网络子系统的地位

重构后的 `ax-net` 本身分为五层：设备适配层、协议服务层、socket 抽象层、地址族实现层、公开入口层。

```text
net/ax-net
  public facade
    -> lib.rs
    -> init_network / init_vsock / poll_interfaces
    -> public socket, option, device, status APIs

  socket abstraction
    -> socket.rs: Socket, SocketOps, SocketAddrEx, SendOptions, RecvOptions
    -> options.rs: Configurable, GetSocketOption, SetSocketOption
    -> general.rs: common nonblocking, timeout, protocol/domain/type state

  address-family implementations
    -> tcp.rs
    -> udp.rs
    -> raw.rs
    -> unix/
    -> vsock/

  protocol service
    -> service.rs: smoltcp Interface owner, DHCP, DNS server state, waker registration
    -> router.rs: route table, device dispatch, packet rx/tx path
    -> listen_table.rs: TCP listen/accept queue
    -> wrapper.rs: smoltcp SocketSet access
    -> state.rs: socket state transitions

  device adaptation
    -> device/driver.rs: rd-net -> ax-net EthernetDriver adapter
    -> device/ethernet.rs: Ethernet device implementation
    -> device/loopback.rs: loopback device
    -> device/vsock.rs: rdif-vsock integration
```

落地目录：

```text
net/
  ax-net/
    Cargo.toml
    src/
      lib.rs
      device/
      router.rs
      service.rs
      socket.rs
      tcp.rs
      udp.rs
      raw.rs
      unix/
      vsock/
      options.rs
      wrapper.rs
      listen_table.rs
      state.rs
```

内部边界：

- `lib.rs` 只做 facade、初始化入口和必要 re-export，不承载协议状态机逻辑。
- `socket.rs` 定义跨地址族统一抽象；TCP、UDP、raw、Unix、vsock 都实现同一组 `SocketOps` / `Pollable` / `Configurable` 语义。
- `service.rs` 和 `router.rs` 是协议服务核心，负责把 smoltcp、路由、设备和 waker 串起来。
- `device/` 只负责把 `rd-net` / `rdif-vsock` 设备接入网络栈，不包含具体硬件驱动。
- `router`、`service`、`wrapper`、`listen_table`、`state` 默认 crate-private，避免外部系统绑定内部实现细节。
- 将当前 `ax-net-ng` 的实现作为 `net/ax-net` 主体。
- 删除旧 `os/arceos/modules/axnet/src/smoltcp_impl` 的 TCP/UDP/listen table/router/DNS/benchmark 实现。
- 删除 `os/arceos/modules/axnet` 和 `os/arceos/modules/axnet-ng` 目录，避免 ArceOS modules 下继续存在网络栈实现。
- 不迁移旧 `ax-net` 的 `smoltcp_impl`、旧 DNS socket wrapper、旧 `bench_transmit()` / `bench_receive()`；需要这些能力的调用方必须改到新 API 或独立工具。

### 2.2 依赖关系

`ax-net` 依赖的组件：

```text
ax-net
  -> smoltcp                 TCP/IP 协议栈
  -> axpoll                  readiness / Pollable / waker 语义
  -> ax-task                 阻塞等待、任务调度、DHCP/vsock 后台任务
  -> ax-sync / ax-kspin      锁与同步原语
  -> ax-io                   Read/Write/IoBuf 抽象
  -> ax-errno                AxError / AxResult / LinuxError 映射
  -> ax-config               IP、网关、前缀等构建期配置
  -> ax-hal                  时间等底层能力
  -> ax-fs-ng / axfs-ng-vfs  Unix domain socket 路径绑定
  -> rd-net                  Ethernet queue/DMA 设备抽象
  -> rdif-vsock              可选，vsock 设备抽象
```

依赖 `ax-net` 的组件：

```text
ax-runtime
  -> ax-net                  网络和 vsock 初始化

ax-feat
  -> ax-net                  net/vsock feature 装配

ax-api
  -> ax-net                  ArceOS Rust API 网络入口

ax-posix-api
  -> ax-net                  ArceOS POSIX socket 入口

ax-std / ax-libc
  -> ax-api / ax-posix-api
  -> ax-net                  间接使用统一网络栈

starry-kernel
  -> ax-net                  Linux syscall socket 底层实现

apps / test-suit
  -> ax-std / ax-libc / starry-kernel
  -> ax-net                  间接验证统一网络栈
```

不属于 `ax-net` 依赖方向的内容：

- `ax-driver` 不依赖 `ax-net`，只负责注册 `rd-net::Net` 和 `rdif-vsock::Interface`。
- 具体网卡/vsock 驱动位于 `drivers/`，不进入 `net/ax-net`。
- StarryOS 的 `AF_PACKET`、`AF_NETLINK`、Linux `sockaddr` 编解码、`ioctl(SIOCGIF*)`、syscall errno 细节保留在 `os/StarryOS/`。
- `os/arceos/modules/axnet`、`os/arceos/modules/axnet-ng` 从 workspace members 和 dependencies 中删除。

### 2.3 对外 API 设计

统一后的 `ax-net` 对外 API 分为三类：运行时初始化 API、socket API、设备/状态查询 API。

运行时初始化 API：

- `pub fn init_network(net_devs: EthernetDeviceList)`：由 `ax-runtime` 调用，完成 loopback、eth0、router、smoltcp service、DHCP/static IP 初始化。
- `#[cfg(feature = "vsock")] pub fn init_vsock(vsock_devs: VsockDeviceList)`：由 `ax-runtime` 调用，注册 vsock 设备并启动 vsock 事件处理。
- `pub fn poll_interfaces()`：推动网络栈收包、发包、协议状态机和 socket readiness 更新；供 runtime、poll/select/epoll 和 OS 适配层显式调用。

socket API：

- `pub mod tcp`，公开 `tcp::TcpSocket`。
- `pub mod udp`，公开 `udp::UdpSocket`。
- `pub mod raw`，公开 raw IPv4/ICMP socket 能力。
- `pub mod unix`，公开 Unix domain socket 地址、stream/dgram transport 与 `UnixSocket`。
- `#[cfg(feature = "vsock")] pub mod vsock`，公开 vsock 地址、stream transport 与 `VsockSocket`。
- `pub use socket::{Socket, SocketAddrEx, SocketOps, SendOptions, RecvOptions, SendFlags, RecvFlags, Shutdown, CMsgData}`，作为 StarryOS syscall 层和未来其他 OS 兼容层的统一 socket 抽象。
- `pub mod options`，公开 `Configurable`、`GetSocketOption`、`SetSocketOption`、`TcpInfo`、`UnixCredentials` 等 socket option 类型。

设备和状态查询 API：

- `pub use device::{EthernetDeviceList, EthernetDriver, NetRxBuffer, NetTxBuffer, NetDeviceError, NetDeviceResult, NetIrqEvents, RdNetDriver}`，供 `ax-runtime` 将 `ax-driver` 暴露的 `rd-net` 设备适配进 `ax-net`。
- `#[cfg(feature = "vsock")] pub use device::{VsockDevice, VsockDeviceList}`。
- `pub fn arp_entries() -> Vec<ArpEntry>`，供 `/proc/net/arp`、诊断工具或系统兼容层查询 ARP 状态。
- `pub fn dns_query(name: &str) -> AxResult<Vec<IpAddr>>`，正式 DNS 域名解析 API，供 `ax-api`、`axstd::net::ToSocketAddrs` 和 POSIX `getaddrinfo` 路径使用。
- `pub fn dns_servers() -> Vec<Ipv4Address>`，查询当前配置的 DNS server 列表，仅供诊断使用。

不作为 `ax-net` 对外 API 的内容：

- `AF_PACKET`、`AF_NETLINK`、Linux `sockaddr` 编解码、`ioctl(SIOCGIF*)`、syscall errno 细节仍属于 StarryOS Linux ABI 层。
- 具体网卡驱动、PCI/VirtIO 设备探测、DMA mapping 策略仍属于 `drivers/` 和 `ax-driver`。
- 旧 `ax-net` 的阻塞式 inherent methods、旧 DNS wrapper、旧 benchmark 入口不进入新 `ax-net` 公共 API。
- `router`、`service`、`wrapper`、`listen_table`、`state` 默认保持 crate-private，除非有明确跨 OS 复用需求。

### 2.4 ArceOS 调用方迁移

旧 ArceOS 上层直接调用 `TcpSocket` / `UdpSocket` 的 inherent methods。统一后不在 `ax-net` 中保留这套旧方法，而是迁移 ArceOS 调用方使用统一 socket API。

迁移原则：

- `ax-api::net` 从旧 handle wrapper 迁移为基于 `ax_net::{SocketOps, SocketAddrEx, SendOptions, RecvOptions, Shutdown}` 的实现。
- `ax-posix-api::imp::net` 不再直接依赖旧 `TcpSocket`/`UdpSocket` 方法，改为与 StarryOS 类似的统一 `Socket`/`SocketOps` 调用模型。
- `axstd::net` 和 `axlibc::net` 保持面向应用的 API 不变，但其底层实现通过 `ax-api` / `ax-posix-api` 迁移到新 `ax-net`。
- TCP/UDP 阻塞、非阻塞、poll 状态统一由 `Pollable`、`Configurable` 和 `axpoll` 语义提供，不再维护旧轮询式阻塞路径。
- `ax-api::net::ax_dns_query()` 改为调用 `ax_net::dns_query()`；`dns_query()` 是新 `ax-net` 的正式 DNS API，不是旧 `smoltcp_impl` DNS wrapper 的兼容保留。

### 2.5 feature 设计

统一后只保留语义主 feature：

- `ax-feat/net`：启用统一 `ax-net`。
- `ax-runtime/net`：启用 `dep:ax-net`、`dep:rd-net`、`dep:spin`、`dep:axklib`、`ax-driver/net`。
- `ax-feat/vsock`：启用 `ax-runtime/vsock`。
- `ax-runtime/vsock`：启用 `net`、`ax-net/vsock`、`ax-driver/vsock`。
- `ax-net/vsock`：启用 `dep:rdif-vsock`。
- `axstd/net`、`axlibc/net`、`ax-api/net`、`ax-posix-api/net`：继续依赖 `ax-feat/net` 和 `ax-net`。

处理旧 feature：

- 彻底删除 `net-ng` feature，不保留 alias、不保留 deprecated 兼容入口。
- 所有仓库内配置、Cargo.toml、文档、测试统一改为 `net`。
- 任何残留的 `net-ng` 引用都视为本轮重构必须修复的编译错误。

### 2.6 Cargo 与调用方修改

Workspace：

- 新增 workspace member `net/ax-net`。
- 删除 workspace members `os/arceos/modules/axnet` 和 `os/arceos/modules/axnet-ng`。
- 将 workspace dependency `ax-net` 改为 `path = "net/ax-net"`。
- 新增 workspace dependency alias `axnet = { package = "ax-net", path = "net/ax-net", ... }`，供 StarryOS 在不改 `use axnet::...` 的前提下继承统一网络栈。
- 删除 workspace dependency `ax-net-ng`。
- `ax-net` 版本可提升到当前 `ax-net-ng` 的版本线或项目约定的新版本。

ArceOS：

- `ax-feat` 删除 `dep:ax-net-ng` 的真实依赖路径。
- `ax-runtime` 删除 `init_dyn_net_ng`、`take_*_net_ng_drivers` 分支，所有网络初始化走 `ax_net`。
- `ax-api` / `ax-posix-api` 迁移为使用 `ax-net` 新统一 API，不保留旧 socket handle 到旧方法的适配层。
- `axstd` / `axlibc` 面向应用的公开 API 可保持不变，但内部必须通过迁移后的 `ax-api` / `ax-posix-api` 工作。

StarryOS：

- `os/StarryOS/kernel/Cargo.toml`（package name: `starry-kernel`）改为：
  - `axnet = { workspace = true }`（继承 workspace dependency alias，真实 package 为 `ax-net`）
  - feature 从 `ax-feat/net-ng` 改为 `ax-feat/net`
  - `vsock` 继续通过 `ax-feat/vsock`
- StarryOS syscall 代码中的 `use axnet::...` 保持不变。

测试与应用配置：

- `test-suit/starryos/**/build-*.toml` 中的 `starry-kernel/vsock` 保持。
- `ax-driver/virtio-net`、`ax-driver/virtio-socket` 保持。
- 若出现显式 `net-ng` 配置，全部改为 `net`。
- QEMU `virtio-net-pci` 和 `-netdev user` 配置不变。

### 2.7 DNS 与 DHCP

**关键发现**：当前已有依赖 DNS 的功能，必须在本轮实现以避免功能退化：
- `axstd::net::ToSocketAddrs` 中 `(&str, u16)` 需要 DNS 解析
- `ax-api::net::ax_dns_query()` 被 `axstd` 依赖
- `apps/arceos/httpclient` 在 `feature = "dns"` 下使用域名连接

**本轮实现方案（必须完成）**：

1. **保留 DNS 查询功能，迁移实现**
   - 在 `net/ax-net/src/lib.rs` 中提供正式 public API：`pub fn dns_query(name: &str) -> AxResult<Vec<IpAddr>>`
   - 基于 smoltcp 的 DNS socket 功能实现（`socket-dns` feature 已启用）
   - 复用旧 DNS 查询流程，但重写实现以接入新的 `SocketSetWrapper`、`Service` 和动态 DNS server 列表，不迁移旧 `ETH0` / 旧 `SOCKET_SET` 结构
   - 使用 DHCP 获取的 DNS server 或静态配置的 DNS server

2. **DNS server 配置管理**
   - `ax-net` 内部维护 DNS server 列表（从 DHCP 或显式静态配置获取）
   - 新增静态 DNS 配置项，例如 `AX_DNS`，接入 `ax-config` 或 `consts.rs`
   - 可提供查询接口：`pub fn dns_servers() -> Vec<Ipv4Address>`，仅作为诊断 API，不作为 resolver 配置修改入口
   - DNS server 来源优先级：
     - DHCP 获取的 DNS server（优先）
     - 显式静态 DNS 配置
     - 无配置时查询返回错误，不硬编码默认值

3. **DHCP 支持**
   - 保持 `ax-net-ng` 已有的 DHCP 实现（DHCP client、自动获取 IP/gateway/DNS）
   - 初始化时按配置选择静态网络或 DHCP：`AX_IP` 和 `AX_GW` 都存在时使用静态网络，否则启用 DHCP
   - DHCP bootstrap 超时保持 warning 行为，不新增隐式静态回退；如果需要 DHCP 失败回退静态配置，应作为额外设计单独实现

4. **API 映射**
   - `ax-api::net::ax_dns_query()` → `ax_net::dns_query()`
   - `axstd::net::ToSocketAddrs` 继续通过 `ax_api::net::ax_dns_query()` 工作
   - 不改变上层 API，只替换底层实现

**迁移注意事项**：
- 旧实现位于 `os/arceos/modules/axnet/src/smoltcp_impl/dns.rs`，仅作为 DNS query 流程参考，不完整搬迁旧结构
- 测试覆盖：`apps/arceos/httpclient` 的 `dns` feature 必须能正常工作
- 如果 smoltcp DNS socket 实现不足，需要在本轮补充完整

**不在本轮范围**：
- 复杂的 DNS 缓存机制（可选优化）
- IPv6 DNS（AAAA 记录）
- 自定义 DNS resolver 配置（`/etc/resolv.conf` 等）

## 3. 测试方案

必须运行：

- `cargo fmt`
- `cargo xtask clippy --package ax-net`
- `cargo xtask clippy --package ax-runtime`
- `cargo xtask clippy --package ax-feat`
- `cargo xtask clippy --package ax-api`
- `cargo xtask clippy --package ax-posix-api`
- `cargo xtask clippy --package ax-std`
- `cargo xtask clippy --package ax-libc`
- `cargo xtask clippy --package starry-kernel`

功能验证：

- ArceOS `net-loopback` 测试：确认 `axstd::net::{TcpListener,TcpStream,UdpSocket}` 仍可用。
- ArceOS `httpserver` / `httpclient` QEMU：确认 ArceOS 上层已迁移到新 `ax-net` API。
- **ArceOS `httpclient` DNS 测试**：使用 `--features dns` 运行，确认域名解析功能正常（`ToSocketAddrs` 和 `ax_dns_query` 工作）。
- StarryOS socket dataplane 测试：TCP/UDP bind/connect/send/recv。
- StarryOS epoll network 测试：确认 `axpoll` waker 路径未破坏。
- StarryOS AF_UNIX SCM_RIGHTS/socketpair 测试。
- StarryOS raw ICMP/packet socket/netlink 测试。
- StarryOS vsock build 测试，至少覆盖 `starry-kernel/vsock + ax-driver/virtio-socket` 的编译。
- DHCP/QEMU user networking 场景：确认 `eth0` 初始化、默认路由、DNS server 获取不退化。

## 4. 遗留问题

- `bench_transmit()` / `bench_receive()`：旧入口不迁入新 `ax-net`；若仍需要网络设备 benchmark，应单独放到 tools 或 test-suit，不进入核心网络栈 API。
- DNS：本轮必须迁移 `dns_query()` 实现，保持现有功能不退化；DNS 缓存、IPv6 AAAA 记录、自定义 resolver 配置等高级特性不在本轮范围。
- IPv6：StarryOS syscall 层支持 AF_INET6 入口，但实际会映射到 IPv4；统一网络栈仍以 IPv4 为主，完整 IPv6 支持不在本轮范围。
- 多 NIC：新栈已有 router/device 抽象，但初始化仍主要使用 `eth0`；多网卡策略、接口命名、真实 MAC 查询给 StarryOS ioctl 使用可后续增强。
- `net-ng` 名称清理：本轮必须删除代码、Cargo feature、测试配置和文档中的 `net-ng` 入口；若第三方分支仍依赖该 feature，需要自行迁移到 `net`。
- ArceOS modules 路径清理：本轮必须删除 `os/arceos/modules/axnet` 和 `os/arceos/modules/axnet-ng` 作为网络栈承载目录，后续网络栈实现只允许位于 `net/ax-net`。
- StarryOS `AF_PACKET` 和 `AF_NETLINK` 仍在 StarryOS 层，不迁入 `ax-net`；它们是 Linux 兼容层的一部分，不属于通用 ArceOS 网络栈。
