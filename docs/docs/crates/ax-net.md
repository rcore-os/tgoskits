# `ax-net` 技术文档

> 路径：`os/arceos/modules/axnet`
> 类型：库 crate
> 分层：ArceOS 层 / 第一代 IP 网络模块
> 版本：`0.3.0-preview.3`
> 文档依据：`Cargo.toml`、`src/lib.rs`、`src/smoltcp_impl/mod.rs`、`src/smoltcp_impl/tcp.rs`、`src/smoltcp_impl/udp.rs`、`src/smoltcp_impl/dns.rs`、`src/smoltcp_impl/listen_table.rs`、`src/smoltcp_impl/bench.rs`、`os/arceos/modules/axruntime/src/lib.rs`、`os/arceos/api/ax-api/src/imp/net.rs`、`os/arceos/api/arceos_posix_api/src/imp/net.rs`

`ax-net` 是 ArceOS 老一代网络封装层。它围绕 `smoltcp` 构建一个全局、单接口、同步阻塞风格的 IP socket API，把 TCP、UDP 和 DNS 能力接到 ArceOS 运行时与 API 层上。它关心的是“把一块 NIC 和一份 `smoltcp` 实例变成可用的系统网络能力”，而不是构建跨地址族、跨设备、跨等待模型的通用 socket 服务框架。

最核心的边界是：`ax-net` 只是第一代 IP 网络包装层，不是统一 socket 服务层。它的重点是 TCP/UDP/DNS over IP，而不是 Unix domain socket、vsock、复杂路由服务或细粒度事件管理。

## 1. 架构设计分析

### 1.1 设计定位

`ax-net` 的设计目标非常直接：把 `smoltcp` 变成 ArceOS 可直接消费的同步网络 API。当前实现采用三层收敛：

- 设备层：从 `ax-driver` 取出一个 `AxNetDevice`，直接适配成 `smoltcp::phy::Device`
- 接口层：使用单个全局 `InterfaceWrapper` 管理唯一网络接口 `eth0`
- socket 层：提供同步的 `TcpSocket`、`UdpSocket` 和 `dns_query()`

这是一种典型的早期内核集成模型：单 NIC、全局状态、围绕一个 `smoltcp::Interface` 运转。

### 1.2 模块划分

`ax-net` 的主要实现集中在 `smoltcp_impl`：

| 模块 | 作用 |
| --- | --- |
| `mod.rs` | 全局接口、`SocketSet`、设备适配、轮询主线 |
| `tcp.rs` | TCP socket 的同步封装、连接、监听、接受 |
| `udp.rs` | UDP socket 的绑定、连接、收发与轮询 |
| `dns.rs` | 基于 `smoltcp::socket::dns` 的同步 DNS 查询 |
| `listen_table.rs` | TCP 监听队列表，在首个 SYN 到来时预创建 socket |
| `bench.rs` | 原始帧发送/接收吞吐基准入口 |
| `addr.rs` | 地址类型辅助 |

### 1.3 初始化模型与全局对象

`src/lib.rs` 暴露的入口非常少，但已经把整个设计说透了：

- `init_network(net_devs)`：从设备容器里取第一个 NIC 作为 `eth0`
- `poll_interfaces()`：推动底层 `smoltcp` 协议栈前进
- `TcpSocket` / `UdpSocket` / `dns_query()`：供上层实际使用
- `bench_transmit()` / `bench_receive()`：导出带宽基准路径

内部关键全局对象包括：

- `ETH0`：唯一网络接口
- `SOCKET_SET`：全局 `smoltcp::SocketSet`
- `LISTEN_TABLE`：监听端口到 SYN 队列的映射

这组对象共同说明：`ax-net` 不是多实例、按命名空间拆分的服务，而是一个单体式全局网络模块。

### 1.4 设备与接口模型

`smoltcp_impl/mod.rs` 中的 `DeviceWrapper` 直接把 `AxNetDevice` 适配给 `smoltcp`，并把介质类型固定为 `Medium::Ethernet`。这意味着：

- `smoltcp` 直接面对以太网设备，不经过更高一级的路由器抽象
- 发包、收包、tx/rx buffer 回收都由驱动对象承担
- 接口地址和默认网关依赖 `AX_IP` / `AX_GW` 环境变量
- 默认 DNS 服务器写死为 `8.8.8.8`

这一模型足够简单，也正因此不承担多设备转发、loopback 独立建模或更复杂的路由策略。

### 1.5 TCP 监听队列的特别设计

`listen_table.rs` 与 `RxToken::preprocess()` 组合出 `ax-net` 最有代表性的实现细节：当底层收到首个 TCP SYN 时，`snoop_tcp_packet()` 会提前在 `SocketSet` 中创建一个监听对应的新 socket，并把 handle 塞入端口对应的 SYN 队列。等上层 `accept()` 时，再从队列中取出已经建立连接的 handle。

这不是额外的协议实现，而是为了在保持同步 `accept()` 语义的同时，适配 `smoltcp` 原生 socket 集合工作方式。

### 1.6 与 `ax-net-ng` 的代际差异

| 维度 | `ax-net` | `ax-net-ng` |
| --- | --- | --- |
| 总体定位 | 第一代同步 IP 网络模块 | 第二代统一 socket 服务层 |
| 地址族 | IP/TCP/UDP/DNS | IP + Unix domain + vsock |
| 设备视图 | `smoltcp` 直接面对 Ethernet 设备 | `Router`/`Device` 先做路由、loopback、ARP，再把 IP 包交给 `smoltcp` |
| readiness 语义 | `ax_io::PollState` | `axpoll::IoEvents` |
| 等待方式 | 轮询接口并 `yield_now()` | `poll_io` + waker + timeout |
| 主要消费者 | `ax-api`、`ax-posix-api`、老一代 ArceOS 路径 | `ax-runtime net-ng` 与 StarryOS 主 socket 层 |

所以，`ax-net` 不是“`ax-net-ng` 的轻量别名”，而是更早一代、边界更窄的 IP 网络封装。

## 2. 核心功能说明

### 2.1 主要能力

- 初始化单个网络接口并配置 IP / 默认网关
- 提供同步 `TcpSocket` 与 `UdpSocket`
- 提供同步 `dns_query()` 能力
- 通过 `poll_interfaces()` 驱动 `smoltcp` 协议栈前进
- 提供原始帧吞吐基准入口，供系统级性能验证使用

### 2.2 同步阻塞模型

`ax-net` 的阻塞语义不是建立在独立事件层之上，而是建立在“反复推动协议栈 + 让出 CPU”之上：

1. 先调用 `smoltcp` 的 nonblocking socket API
2. 若当前不可完成，则返回 `AxError::WouldBlock`
3. 在 blocking 模式下，循环调用 `poll_interfaces()` 并穿插 `ax-task::yield_now()`
4. 条件满足后再返回真实结果

这意味着 `ax-net` 的“阻塞”本质上是一种同步轮询式阻塞，而不是基于 `axpoll` 的等待/唤醒模型。

### 2.3 上层调用关系

仓库里的主要调用链很明确：

- `ax-runtime` 在启用老一代 `net` 路径时调用 `axnet::init_network(all_devices.net)`
- `ax-api` 直接导出 TCP、UDP、DNS 与 `ax_poll_interfaces()`
- `ax-posix-api` 用它实现 socket、`select`、`epoll` 等 POSIX 兼容接口
- `ax-std` 再经由更高层 API 把网络能力暴露给 ArceOS 应用

与之相对，当运行时选择 `net-ng` 时，初始化入口会切换到 `axnet_ng::init_network()`，这正好体现了两代实现的并存关系。

### 2.4 与带宽基准工具的关系

`bench_transmit()` / `bench_receive()` 直接走 `DeviceWrapper` 中的原始帧收发基准逻辑：

- 发送侧会构造固定 1500 字节帧，并把 EtherType 置为 IPv4
- 接收侧只统计收到的字节数

它们不是普通 socket API，而是给系统带宽基准使用的专门入口，设计上就是为了和宿主机侧 `bwbench-client` 配对验证吞吐。

### 2.5 关键边界

- `ax-net` 不重新实现 TCP/IP 协议状态机，真正的协议引擎仍然是 `smoltcp`
- `ax-net` 不提供 Unix domain socket、vsock 或统一地址族抽象
- `ax-net` 不提供细粒度事件注册机制；它只向上暴露 `PollState`
- `ax-net` 不是应用级 HTTP/DNS 客户端库，而是更低层的网络原语层

## 3. 依赖关系

### 3.1 关键直接依赖

| 依赖 | 作用 |
| --- | --- |
| `ax-driver` | 提供 NIC 设备对象 |
| `ax-hal` | 提供时间接口，驱动 `smoltcp::Instant` |
| `axio` | 提供 `PollState` 与通用 I/O trait |
| `ax-sync` / `spin` / `ax-lazyinit` | 管理全局接口、`SocketSet` 与监听表 |
| `ax-task` | blocking 路径中的 `yield_now()` 与任务协作 |
| `smoltcp` | 真正的 TCP/UDP/DNS/IP 协议实现 |

### 3.2 主要直接消费者

| 消费者 | 使用方式 |
| --- | --- |
| `ax-runtime` | 在老一代 `net` 路径中初始化网络子系统 |
| `ax-api` | 暴露 ArceOS 级 TCP/UDP/DNS 接口 |
| `ax-posix-api` | 实现 POSIX 风格 socket 与多路复用路径 |
| `ax-feat` | 通过 feature 传播把网络能力装入最终镜像 |

### 3.3 与样例程序的关系

虽然 `ax-httpclient`、`ax-httpserver` 不直接依赖 `ax-net` 的源码 API，但它们经由 `ax-std`、`ax-api`、`ax-runtime` 间接走的正是这条网络装配链。因此，这些示例更适合作为 `ax-net` 的系统行为样例，而不是 `ax-net` 自身的 API 示例。

## 4. 开发指南

### 4.1 依赖方式

```toml
[dependencies]
axnet = { workspace = true }
```

在实际系统镜像里，更常见的接入方式不是手动直接依赖，而是通过 `ax-runtime` / `ax-feat` / `ax-api` 的 feature 传播把它装进系统。

### 4.2 修改时必须同步检查的前提

1. 当前实现只取第一个 NIC 作为 `eth0`。
2. 接口地址和网关来自 `AX_IP` / `AX_GW`。
3. `smoltcp` 看到的是 `Medium::Ethernet` 设备，而不是更高层 IP 设备。
4. 监听队列依赖 `RxToken::preprocess()` 对首个 SYN 进行预处理。
5. DNS 服务器默认固定为 `8.8.8.8`。

只要改动触碰这些前提，就已经不是简单 bugfix，而是在改变整个一代网络模块的边界。

### 4.3 高风险改动点

- `listen_table.rs`：直接影响 `accept()` 是否能拿到正确连接
- TCP/UDP 的 blocking 路径：影响 blocking / nonblocking 行为和 CPU 让出策略
- `dns.rs`：当前是同步轮询实现，容易受 socket 生命周期与超时处理影响
- `bench.rs`：与宿主机基准工具的口径绑定较紧，不能按普通 socket 功能随意改

## 5. 测试策略

### 5.1 当前测试现状

`ax-net` 目录内没有独立的 crate 内 `tests/`。它的正确性主要依赖系统级验证：

- `ax-api` / `ax-posix-api` 的网络调用路径
- 启用 `net` 的 ArceOS 示例与应用
- 宿主机侧网络连通和吞吐验证

### 5.2 建议重点

- 至少验证一条 TCP client、一条 TCP server、一条 UDP 收发和一条 DNS 查询
- 修改监听或接收路径时，必须覆盖 `listen` / `accept` / `shutdown` 回归
- 修改带宽基准相关逻辑时，要同时验证与 `bwbench-client` 的对端行为

### 5.3 推荐集成验证

- 用 `ax-httpclient` / `ax-httpserver` 验证 TCP 主路径
- 用 POSIX socket 调用路径验证 `ax-posix-api`
- 用 `bench_transmit` / `bench_receive` + `bwbench-client` 验证吞吐基准路径

## 6. 跨项目定位

### 6.1 ArceOS

`ax-net` 是 ArceOS 老一代网络装配链的重要组成部分。它直接服务 `ax-api`、`ax-posix-api` 和运行时 `net` 路径，是早期同步 IP 网络能力的核心封装。

### 6.2 StarryOS

当前仓库中的 StarryOS 并不直接使用这个 `ax-net` crate。相反，`os/StarryOS/Cargo.toml` 与 `os/StarryOS/kernel/Cargo.toml` 都把依赖名 `ax-net` 绑定到了 `package = "ax-net-ng"`，说明 StarryOS 已经切换到第二代网络层。

### 6.3 Axvisor

当前没有看到 Axvisor 直接消费 `ax-net` 的证据。即便存在间接复用，也更可能经由 ArceOS 公共 API 层，而不是把这个老一代同步网络模块当成独立基础设施。
