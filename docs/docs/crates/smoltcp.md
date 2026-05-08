# `smoltcp` 技术文档

> 路径：`components/starry-smoltcp`
> 类型：库 crate
> 分层：组件层 / TCP/IP 协议栈本体
> 版本：`0.12.0`
> 文档依据：`Cargo.toml`、`README.md`、`src/lib.rs`、`tests/netsim.rs`、`examples/*`、`benches/bench.rs`、`fuzz/fuzz_targets/*`

`smoltcp` 是仓库中引入并维护的一份独立 TCP/IP 协议栈源码。它提供的是事件驱动的协议状态机、接口层、设备抽象和报文表示，而不是 ArceOS/StarryOS 直接面向系统调用或应用的网络模块。在本仓库里，真正把它接到驱动、等待模型、地址族抽象和系统接口上的，是 `ax-net` 与 `ax-net-ng`。

最关键的边界是：`smoltcp` 负责协议与报文，不负责操作系统策略。路由装配、阻塞/非阻塞等待、超时、Unix socket、vsock、文件描述符映射与系统调用兼容，都不属于它的职责。

## 1. 架构设计分析

### 1.1 设计定位

从 `README.md` 与 `src/lib.rs` 顶层文档可见，`smoltcp` 的目标十分稳定：

- 独立、事件驱动
- 面向 bare-metal / 实时系统
- 结构尽量简单、显式、可文档化
- 大量能力通过编译期特性控制，而不是运行时探测

它想解决的是“没有完整宿主操作系统时，如何提供一个可用且可裁剪的 TCP/IP 协议栈”，而不是“如何给应用提供 Berkeley socket 兼容层”。

### 1.2 分层结构

`src/lib.rs` 已把其分层描述得很清楚：

| 层次 | 模块 | 作用 |
| --- | --- | --- |
| socket layer | `socket` | TCP/UDP/ICMP/raw/DNS 等 socket 状态机与缓冲 |
| interface layer | `iface` | 地址配置、邻居发现、路由与 socket 分发 |
| physical layer | `phy` | `Device`、`RxToken`、`TxToken` 与若干中间件 |
| wire layer | `wire` | 报文解析、表示、emit 与 pretty print |
| support | `storage`、`time` | ring buffer、packet buffer、时间表示等基础设施 |

对上层系统来说，这些层次共同组成了一套“网络原语工具箱”：越往下越接近报文本身，越往上越接近可被内核封装复用的协议状态机。

### 1.3 编译期配置是第一公民

`smoltcp` 的另一个核心特征，是它强依赖编译期配置。`Cargo.toml` 中的 feature 大体分为三类：

- 功能类：`proto-ipv4`、`proto-ipv6`、`socket-tcp`、`socket-udp`、`socket-dns`、`async` 等
- 介质类：`medium-ethernet`、`medium-ip`、`medium-ieee802154`
- 规模类：`iface-max-route-count-*`、`fragmentation-buffer-size-*`、`assembler-max-segment-count-*` 等

此外，`README.md` 明确说明可以用 `SMOLTCP_*` 环境变量在构建时覆盖这些容量参数。也就是说，分析 `smoltcp` 行为不能只看源码，还必须看 feature 组合和构建配置。

### 1.4 在本仓库中的实际启用方式

`ax-net` 和 `ax-net-ng` 都没有使用 `smoltcp` 的默认 feature，而是显式打开了一组更贴近内核场景的能力：

- `alloc`
- `log`
- `async`
- `medium-ethernet`
- `medium-ip`
- `proto-ipv4`
- `proto-ipv6`
- `socket-raw`
- `socket-icmp`
- `socket-udp`
- `socket-tcp`
- `socket-dns`

这说明在本仓库里，`smoltcp` 的定位不是 host-side 演示库，而是内核内 IP/TCP/UDP 协议引擎。

### 1.5 与 `ax-net` / `ax-net-ng` 的边界

在 ArceOS / StarryOS 中，`smoltcp` 的职责到以下范围为止：

- 提供 `tcp::Socket`、`udp::Socket`、`dns::Socket` 等状态机
- 要求上层提供 `phy::Device`、时钟和 `SocketSet`
- 提供 `Interface::poll`、`poll_at` 等协议推进机制
- 不提供 `bind` / `accept4` / `sendmsg` 一类系统接口语义
- 不理解 `axpoll`、`ax-task`、`ax-fs-ng`、Unix socket、vsock 这些系统层概念

因此，`ax-net` / `ax-net-ng` 不是“薄薄一层壳”，而是在把协议栈本体接到操作系统语义上。

## 2. 核心功能说明

### 2.1 主要能力

- 提供 IPv4、IPv6、6LoWPAN、DNS、ICMP、UDP、TCP 等协议支持
- 提供 `Interface` + `SocketSet` + `phy::Device` 的核心运行时模型
- 提供报文解析、构造与 pretty print 能力
- 提供 tracer、fault injector、pcap writer 等设备中间件
- 提供适合嵌入式/裸机环境的编译期容量与功能裁剪机制

### 2.2 典型上层调用链

本仓库中的真实调用链可以概括为：

1. `ax-driver` 暴露 NIC 或更高层网络设备
2. `ax-net` / `ax-net-ng` 将其适配成 `smoltcp::phy::Device`
3. `smoltcp::iface::Interface` 与 `SocketSet` 负责推进协议状态机
4. `ax-net` / `ax-net-ng` 把 `smoltcp` socket 封装成系统友好的同步 socket 接口
5. `ax-api`、`ax-posix-api`、StarryOS socket 子系统继续向上暴露用户可见语义

### 2.3 质量保障资产也是实现的一部分

在这个仓库中，`smoltcp` 不只是协议源码，还保留了完整的开发期验证资产：

- `tests/netsim.rs`：网络仿真回归
- `examples/*`：host-side 与 bare-metal 示例
- `benches/bench.rs`：基准
- `fuzz/fuzz_targets/*`：模糊测试入口

这些资产是理解和维护 `smoltcp` 时必须一并考虑的组成部分，而不是附属材料。

### 2.4 关键边界

- `smoltcp` 不是 ArceOS/StarryOS 的 socket 服务层
- `smoltcp` 不提供 POSIX fd、系统调用接口与进程语义
- `smoltcp` 的 examples 不是 ArceOS 示例程序，而是协议栈自带的 host-side / bare-metal 演示
- `smoltcp` 不处理 Unix domain socket 或 vsock

## 3. 依赖关系

### 3.1 关键直接依赖

| 依赖 | 作用 |
| --- | --- |
| `managed` | 管理无堆/可选堆的内部存储结构 |
| `byteorder` | 报文字节序读写 |
| `heapless` | 固定容量数据结构 |
| `bitflags` | 协议与 socket 标志位 |
| `cfg-if` | feature 组合分支 |
| `log` / `defmt` | 可选日志后端 |
| `libc` | host-side 原始 socket / tuntap 支持（启用对应 feature 时） |

### 3.2 仓库内主要消费者

| 消费者 | 使用方式 |
| --- | --- |
| `ax-net` | 第一代 IP socket 封装 |
| `ax-net-ng` | 第二代统一 socket 服务层中的 IP 协议引擎 |
| `smoltcp-fuzz` | 直接针对本仓库这份协议栈源码做 fuzz 验证 |

### 3.3 与上层模块的职责边界

| 模块 | 负责什么 |
| --- | --- |
| `smoltcp` | 协议状态机、报文解析/构造、接口轮询模型 |
| `ax-net` | 把 `smoltcp` 封装成第一代同步 TCP/UDP/DNS 接口 |
| `ax-net-ng` | 在 `smoltcp` 之上实现统一 socket 语义、路由、Unix/vsock、poll/waker |

## 4. 开发指南

### 4.1 依赖方式

在普通 Rust 项目中，典型用法仍然是：

```toml
[dependencies]
smoltcp = { version = "0.12", default-features = false, features = ["alloc"] }
```

但在本仓库中，更常见的做法不是让业务代码直接依赖 `smoltcp`，而是通过 `ax-net` / `ax-net-ng` 间接消费。

### 4.2 修改前先判断自己在动哪一层

1. 报文解析 / emit 问题：优先看 `wire/*`
2. socket 状态机问题：优先看 `socket/*`
3. 接口与路由问题：优先看 `iface/*`
4. 设备或中间件问题：优先看 `phy/*`
5. 如果问题涉及阻塞、超时、fd、Unix socket、vsock，那通常已经超出 `smoltcp` 边界，应去 `ax-net*` 排查

### 4.3 高风险改动点

- TCP 重传、窗口与拥塞控制相关实现
- 分片 / 重组 / assembler 缓冲区相关逻辑
- `Interface::poll`、`poll_at` 等事件推进路径
- `wire` 层 parse/emit 面对非法报文的容错逻辑
- 任意 feature 组合变化导致的编译面扩张或收缩

## 5. 测试策略

### 5.1 当前已有测试与验证资产

仓库中可直接看到：

- `tests/netsim.rs`：仿真双端口、时延、丢包与 buffer 大小组合
- 大量分布在 `iface`、`socket`、`wire`、`storage` 等模块中的单元测试
- `examples/` 中多组示例，包括 `httpclient`、`server`、`dns`、`loopback`、`benchmark`、`sixlowpan` 等
- `benches/bench.rs`
- `packet_parser`、`tcp_headers`、`dhcp_header`、`ieee802154_header`、`sixlowpan_packet` 五个 fuzz target

### 5.2 建议重点

- 协议或报文层改动优先补单元测试与 fuzz corpus
- 接口层改动至少跑一条 netsim 回归
- 修改编译期参数或 feature 时，同时检查默认组合与 ArceOS 实际使用组合
- 若变更会影响 `ax-net` / `ax-net-ng`，还必须补系统级回归

### 5.3 推荐验证命令

```bash
cargo test -p smoltcp
cargo test -p smoltcp --test netsim --features _netsim
```

若要继续验证解析健壮性，再进入 `smoltcp-fuzz` 路径。

## 6. 跨项目定位

### 6.1 ArceOS

在 ArceOS 中，`smoltcp` 不是用户直接面对的网络 API，而是 `ax-net` / `ax-net-ng` 下方的协议引擎。它决定 TCP/UDP/DNS/IP 行为，但不直接决定系统调用或应用接口语义。

### 6.2 StarryOS

StarryOS 同样不会把 `smoltcp` 直接暴露给上层，而是通过 `ax-net-ng` 间接使用它。对 StarryOS 来说，`smoltcp` 是主 socket 子系统下方的协议核心，而不是 socket 子系统本身。

### 6.3 Axvisor

当前没有看到 Axvisor 把 `smoltcp` 当作独立组件直接接入的证据。它即使间接受益，也更可能经由 ArceOS 公共网络层，而不是直接操作协议栈本体。
