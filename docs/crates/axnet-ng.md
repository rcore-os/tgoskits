# `axnet-ng` 技术文档

> 路径：`os/arceos/modules/axnet-ng`
> 类型：库 crate
> 分层：ArceOS 层 / 第二代统一 socket 服务层
> 版本：`0.3.0-preview.3`
> 文档依据：`Cargo.toml`、`src/lib.rs`、`src/socket.rs`、`src/tcp.rs`、`src/udp.rs`、`src/service.rs`、`src/router.rs`、`src/listen_table.rs`、`src/wrapper.rs`、`src/general.rs`、`src/options.rs`、`src/unix/*`、`src/vsock/*`、`os/arceos/modules/axruntime/src/lib.rs`、`os/StarryOS/Cargo.toml`、`os/StarryOS/kernel/Cargo.toml`、`os/StarryOS/kernel/src/syscall/net/socket.rs`

`axnet-ng` 是 ArceOS/StarryOS 体系中的第二代网络与 socket 服务层。它仍然复用 `smoltcp` 作为 IP/TCP/UDP 协议引擎，但在其上叠加了统一 socket 语义、路由与设备层、`axpoll` readiness/waker 协议、通用 socket 选项，以及 Unix domain socket / vsock 支持。对系统而言，`axnet-ng` 不再是“一个把 `smoltcp` 包起来的模块”，而是 socket 子系统本身的核心实现。

最需要明确的一条边界是：`smoltcp` 只负责 IP 协议栈；地址族统一、阻塞/非阻塞语义、超时、路由、loopback、Unix socket、vsock、waker 注册与系统调用友好接口，全部由 `axnet-ng` 自己承担。

## 1. 架构设计分析

### 1.1 设计定位

`axnet-ng` 的目标可以概括为两层：

- 对 IP 路径，它把驱动、路由、`smoltcp` 和任务等待机制接成一个可用的内核 socket 服务
- 对非 IP 路径，它把 Unix domain socket 与可选 vsock 纳入同一套 `SocketOps` / `Pollable` / `Configurable` 体系

因此，它真正的定位是“统一 socket 服务层”，而不是纯协议栈，也不是单一地址族的包装器。

### 1.2 模块划分

| 模块 | 作用 |
| --- | --- |
| `socket.rs` | 定义 `SocketAddrEx`、`SocketOps`、`Socket`、收发选项、关闭语义 |
| `tcp.rs` / `udp.rs` | 基于 `smoltcp` 的 IP socket 封装 |
| `options.rs` | 通用 socket 选项与 `Configurable` trait |
| `general.rs` | nonblocking、超时、device mask 以及与 `axpoll`/`axtask` 的桥接 |
| `wrapper.rs` | 全局 `SocketSet` 包装、端口占用检查、新 socket 事件 |
| `service.rs` | 持有 `smoltcp::Interface`，负责轮询、超时和设备唤醒整合 |
| `router.rs` | 多设备收发、路由查找、发送分发、接收缓冲 |
| `device/*` | loopback、ethernet、可选 vsock 设备实现 |
| `listen_table.rs` | TCP 监听队列，在首个 SYN 到来时预创建 socket |
| `unix/*` | Unix domain socket 传输层、绑定表与 `axfs-ng` 交互 |
| `vsock/*` | vsock 传输层、连接管理器、接收缓冲与驱动桥接 |
| `state.rs` | TCP/vsock 等对象共享的状态锁 |

### 1.3 初始化主线

`init_network()` 把第二代设计的分层意图表达得很清楚：

1. 先创建 `Router`
2. 无条件注册 `lo` loopback 设备，并加入 `127.0.0.0/8` 路由
3. 若存在 NIC，再注册 `eth0`，并依据 `AX_IP` / `AX_GW` 加入默认路由
4. 用该 `Router` 创建 `Service`
5. `poll_interfaces()` 再循环调用 `Service::poll()`，直到没有新事件

这里最关键的细节是：`Service::new()` 中构建的 `smoltcp::Interface` 使用的是 `HardwareAddress::Ip`。这说明在 `axnet-ng` 中，`smoltcp` 看到的已经不是原始以太网设备，而是经 `Router` 抽象后的 IP 设备视图。

### 1.4 路由与设备层

`router.rs` 是 `axnet-ng` 与旧一代 `axnet` 的最大分水岭之一：

- `Router` 自己实现了 `smoltcp::phy::Device`
- 收包时先从各底层设备拉数据，统一进入 `rx_buffer`
- 发包时依据 `Rule { filter, via, dev, src }` 做设备选择与下一跳选择
- `LoopbackDevice` 用内存缓冲 + `PollSet` 提供本地回环
- `EthernetDevice` 负责 ARP、邻居缓存、待发队列与二层封装

这意味着 `axnet-ng` 自己先做了一层“内核网络设备服务”，然后才把 IP 包交给 `smoltcp`。

### 1.5 统一 socket 语义层

`socket.rs` 把不同地址族统一在一组稳定接口上：

| 抽象 | 作用 |
| --- | --- |
| `SocketAddrEx` | 统一表示 IP、Unix、vsock 地址 |
| `SocketOps` | 统一 `bind`、`connect`、`listen`、`accept`、`send`、`recv`、`shutdown` |
| `Configurable` | 统一获取/设置 socket 选项 |
| `Socket` enum | 把 `TcpSocket`、`UdpSocket`、`UnixSocket`、`VsockSocket` 收敛到同一对象模型 |

这也是 StarryOS 可以在系统调用层只按地址族与类型分发，再统一挂到 `FileLike` 上的根本原因。

### 1.6 等待模型与 `axpoll` 集成

`axnet-ng` 的 blocking/timeout 语义已经不再是 `yield_now()` 忙等，而是显式建立在 `axpoll` 与 `axtask` 之上：

- `GeneralOptions` 记录 nonblocking、发送/接收超时和 device mask
- `send_poller()` / `recv_poller()` 直接调用 `axtask::future::poll_io`
- 各 socket 在 `register()` 中通过 `GeneralOptions::register_waker()` 把 waker 注册给 `Service`
- `Service::register_waker()` 会同时考虑 `iface.poll_at()` 产生的下一次协议栈超时，以及底层设备自己的唤醒源

因此，`axnet-ng` 的等待模型本质上是“readiness + future + timeout”的系统级组合。

### 1.7 IP、Unix、vsock 三条实现边界

- `tcp.rs` / `udp.rs`：把 `smoltcp` socket 状态机封装成更接近 POSIX 的系统接口
- `unix/*`：自行管理抽象命名空间与基于 `axfs-ng` 的路径绑定
- `vsock/*`：自行管理连接表、接收环形缓冲、驱动事件与 accept 队列

这三条实现线共用统一对外抽象，但内部并不是同一种协议引擎。

### 1.8 与 `axnet` 的代际差异

| 维度 | `axnet` | `axnet-ng` |
| --- | --- | --- |
| 总体定位 | 第一代同步 IP 网络模块 | 第二代统一 socket 服务层 |
| 地址族 | TCP / UDP / DNS over IP | IP + Unix domain + 可选 vsock |
| 设备视图 | `smoltcp` 直接面对 Ethernet 设备 | `Router`/`Device` 先处理路由、ARP、loopback |
| readiness 语义 | `axio::PollState` | `axpoll::IoEvents` |
| 等待方式 | 主动轮询 + `yield_now()` | `poll_io` + waker + timeout |
| 主要消费者 | ArceOS 老一代 API 层 | ArceOS `net-ng` 路径与 StarryOS 主 socket 层 |

## 2. 核心功能说明

### 2.1 主要能力

- 初始化 loopback + 可选 `eth0` 的统一网络服务
- 提供 TCP、UDP、Unix domain socket、可选 vsock 的统一对象模型
- 提供通用 socket 选项接口，包括 nonblocking、reuse address、timeout、TTL、NoDelay 等
- 为 IP socket 复用 `smoltcp`，为 Unix/vsock 维护独立传输实现
- 向上提供统一的 `Pollable` / `SocketOps` / `Configurable` 契约

### 2.2 IP socket 与 `smoltcp` 的边界

`tcp.rs` 与 `udp.rs` 并不把 `smoltcp` 直接暴露给上层，而是在其上重新组织出一套系统友好的接口：

- 更接近 POSIX 的 `bind` / `connect` / `listen` / `accept`
- `SendOptions` / `RecvOptions`
- `RecvFlags::PEEK`、`TRUNCATE` 等内核侧收发语义
- 统一 socket 选项访问
- 与 `axpoll` 相兼容的 readiness 注册方式

也就是说，`smoltcp` 在这里是内部协议引擎，不是公开的系统边界。

### 2.3 `wrapper.rs` 的隐藏基础设施角色

`wrapper.rs` 虽然不直接面对用户，但对整个子系统很关键：

- `SocketSetWrapper::bind_check()` 负责扫描现有 IP socket，做地址/端口冲突检查
- `new_socket: Event` 用于在新 socket 创建时通知等待方

这说明 `axnet-ng` 的系统化程度已经超出“协议封装”范畴，开始承担 socket 服务内部协调工作。

### 2.4 StarryOS 中的真实定位

StarryOS 的 `Cargo.toml` 明确把依赖名 `axnet` 绑定到 `package = "axnet-ng"`；`kernel/src/syscall/net/socket.rs` 直接按地址族与类型创建 TCP、UDP、Unix、vsock 对象。对 StarryOS 而言，`axnet-ng` 已经不是实验层，而是主 socket 子系统本体。

### 2.5 关键边界

- `axnet-ng` 不是协议栈本体；IP 协议状态机依然来自 `smoltcp`
- `axnet-ng` 不只做 IP；Unix socket 和 vsock 由它自身实现
- `axnet-ng` 不是应用层 HTTP/客户端库；它提供的是系统网络与 socket 语义
- `axnet-ng` 不直接等于 POSIX syscall 层，但它是 syscall 层下面最核心的 socket 服务实现

## 3. 依赖关系

### 3.1 关键直接依赖

| 依赖 | 作用 |
| --- | --- |
| `smoltcp` | IP/TCP/UDP 协议状态机与 `Interface`/`SocketSet` |
| `axdriver` | 网卡与可选 vsock 设备 |
| `axfs-ng`、`axfs-ng-vfs` | Unix domain socket 的路径绑定与 vnode 元数据 |
| `axio` | 统一收发对象的 I/O trait |
| `axpoll` | readiness 事件与 waker 协议 |
| `axtask` | `poll_io`、超时、等待与中断友好 future |
| `event-listener` | 新 socket 事件通知 |
| `ringbuf` | vsock 接收缓冲 |
| `hashbrown`、`lazy_static` | Unix 抽象命名空间与若干状态表 |

### 3.2 主要直接消费者

| 消费者 | 使用方式 |
| --- | --- |
| `axruntime` | 在 `net-ng` 路径中初始化网络与可选 vsock 子系统 |
| `starry-kernel` | 作为主 socket 实现，服务 `sys_socket`、`bind`、`connect`、`accept` 等系统调用 |
| ArceOS 上层应用链 | 经由运行时和更高层 API 间接复用 |

### 3.3 关键内部协作关系

| 内部层次 | 角色 |
| --- | --- |
| `device/*` | 底层设备事件、ARP、loopback、vsock 驱动桥接 |
| `router.rs` | 多设备收发与路由选择 |
| `service.rs` | `smoltcp::Interface` 驱动与超时/唤醒整合 |
| `tcp.rs` / `udp.rs` | IP socket 语义封装 |
| `unix/*` / `vsock/*` | 非 IP 地址族实现 |
| `socket.rs` | 统一对外对象模型 |

## 4. 开发指南

### 4.1 依赖方式

```toml
[dependencies]
axnet-ng = { workspace = true }
```

在系统镜像里，更常见的入口是启用 `axruntime` 的 `net-ng` feature，而不是手动直接调用初始化函数。

### 4.2 修改前先判断自己动的是哪一层

1. 改 `tcp.rs` / `udp.rs`：先区分这是系统 socket 语义问题，还是 `smoltcp` 协议问题。
2. 改 `router.rs` / `device/*`：同时考虑 loopback、ARP、设备掩码、默认路由和发包路径。
3. 改 `general.rs` / `service.rs`：把它视为等待模型级变更，会波及整个 socket 子系统。
4. 改 `unix/*`：必须同步考虑 `axfs-ng` 路径绑定和抽象命名空间。
5. 改 `vsock/*`：必须一起检查连接管理、accept 队列、环形缓冲与驱动事件。

### 4.3 高风险改动点

- `wrapper.rs::bind_check()`：影响地址/端口复用与冲突检查
- `Service::register_waker()`：影响协议栈超时唤醒和底层设备唤醒协同
- `ListenTable`：影响 TCP `accept()` 正确性与 SYN 队列压力
- Unix 路径绑定：影响 `axfs-ng` vnode 生命周期与命名空间一致性
- `VsockConnectionManager`：影响 accept 队列、接收缓冲与驱动事件对齐

## 5. 测试策略

### 5.1 当前测试现状

`axnet-ng` 目录内没有独立 `tests/`。它的正确性主要依赖系统级验证：

- `axruntime` 的 `net-ng` / `vsock` 初始化路径
- StarryOS 的 socket 系统调用与 `FileLike` 封装
- ArceOS/StarryOS 各类网络、Unix socket 与 vsock 场景

### 5.2 建议重点

- IP 侧至少覆盖 TCP client、TCP server、UDP connected/unconnected、非阻塞与超时
- Unix domain socket 至少覆盖路径绑定、抽象命名空间、`socketpair` 与 accept
- vsock 至少覆盖 listen、connect、accept、credit update、断连与缓冲耗尽
- 修改 `poll()` / `register()` 后，必须验证 `select` / `poll` / `epoll` 上层路径不会丢事件

### 5.3 推荐集成验证

- 用启用 `net-ng` 的 ArceOS 运行时做基础连通性验证
- 用 StarryOS 的 `sys_socket` / `sys_accept4` / `sys_socketpair` 路径做地址族回归
- 若改动 vsock，至少补一条真实驱动事件链验证，而不应只靠静态阅读

## 6. 跨项目定位

### 6.1 ArceOS

在 ArceOS 中，`axnet-ng` 是运行时 `net-ng` feature 对应的第二代网络与 socket 服务层。它相比 `axnet` 更靠近系统服务基础设施，而不是单纯 IP socket 包装器。

### 6.2 StarryOS

在 StarryOS 中，`axnet-ng` 实际上就是主 socket 子系统。虽然依赖名仍叫 `axnet`，但具体包已明确指向 `axnet-ng`，系统调用层直接消费它提供的 TCP、UDP、Unix、vsock 抽象。

### 6.3 Axvisor

当前没有看到 Axvisor 把 `axnet-ng` 当作核心子系统直接消费的证据。即使存在间接复用，也更可能经过 ArceOS 公共层，而不是直接操作这套统一 socket 服务框架。
