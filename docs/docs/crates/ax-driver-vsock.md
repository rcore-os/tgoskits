# `axdriver_vsock` 技术文档

> 路径：`components/axdriver_crates/axdriver_vsock`
> 类型：库 crate
> 分层：组件层 / vsock 设备类别接口层
> 版本：`0.1.4-preview.3`
> 文档依据：`Cargo.toml`、`README.md`、`src/lib.rs`、`components/axdriver_crates/axdriver_virtio/src/socket.rs`、`os/arceos/modules/axruntime/src/lib.rs`

`axdriver_vsock` 用来定义 vsock 驱动的统一接口。它既不是通用 socket API，也不是网络协议栈，而是把“面向 host/guest 通道的设备驱动”抽象成一组统一操作，让 `virtio-vsock` 之类的具体实现能被 `ax-driver` 聚合层和 `ax-net-ng` 上层消费。

## 1. 架构设计分析
### 1.1 设计定位
这个 crate 的职责集中在三个方面：

- 定义 `VsockAddr` 和 `VsockConnId` 这两个连接标识类型。
- 定义 `VsockDriverEvent`，把底层设备事件统一成驱动层事件模型。
- 定义 `VsockDriverOps`，为监听、连接、收发、断开和事件轮询提供统一接口。

它的层次位置是：

- 向下承接具体 vsock 设备驱动，例如 `VirtIoSocketDev`。
- 向上被 `ax-driver::prelude` 和 `ax-net-ng` 使用。
- 不承担 BSD socket、poll 语义、地址解析等更高层网络职责。

### 1.2 关键对象
| 符号 | 作用 |
| --- | --- |
| `VsockAddr` | `(cid, port)` 形式的对端地址 |
| `VsockConnId` | 以 `peer_addr + local_port` 标识一条连接 |
| `VsockDriverEvent` | 驱动上报的连接/接收/断开/credit 事件 |
| `VsockDriverOps` | 统一定义监听、连接、收发和轮询接口 |

### 1.3 事件与连接模型
`VsockDriverOps` 暴露的操作分为几组：

- 基本信息：`guest_cid()`。
- 连接管理：`listen()`、`connect()`、`disconnect()`、`abort()`。
- 数据通路：`send()`、`recv()`、`recv_avail()`。
- 事件获取：`poll_event()`。

`VsockConnId::listening(local_port)` 还提供了一个特殊构造，用于表达“仅监听某个本地端口”的连接标识。

### 1.4 当前主要实现路径
当前仓库里，`ax_driver_virtio::VirtIoSocketDev` 是主要实现：

- 它内部使用 `virtio_drivers::device::socket::VsockConnectionManager`。
- `connect()` / `send()` / `recv()` 等操作都被翻译到底层 VirtIO socket 管理器。
- `poll_event()` 会把底层 `VsockEvent` 转成 `VsockDriverEvent`。
- `recv()` 后还会显式 `update_credit()`，说明 credit 流控是当前实现的重要一环。

再往上一层：

- `ax-driver` 把 vsock 设备放入 `AllDevices.vsock`。
- `ax-runtime` 仅在 `net-ng` + `vsock` feature 组合下调用 `ax-net_ng::init_vsock(all_devices.vsock)`。

这说明该 crate 不是独立的网络主线，而是 `ax-net-ng` 可选能力的一部分。

### 1.5 源码中的现实细节
`src/lib.rs` 里还残留了明显的文案复制错误，例如把顶层注释写成 “device drivers (i.e. disk)” 、把 trait 注释写成 “block storage device”。这些注释并不代表真实定位，真实接口完全围绕 vsock 连接和事件设计。

### 1.6 边界澄清
最关键的边界是：**`axdriver_vsock` 定义的是 vsock 设备驱动契约，不是用户可见的 socket API，也不是通用网络栈。**

## 2. 核心功能说明
### 2.1 主要能力
- 统一表达 vsock 地址、连接和驱动事件。
- 为设备驱动提供一套面向连接管理和事件轮询的 trait。
- 让 `virtio-vsock` 这样的实现可以被 `ax-driver` 聚合和 `ax-net-ng` 消费。

### 2.2 当前接口特征
- 连接标识以 `peer_addr + local_port` 为主，而不是文件描述符或句柄。
- `poll_event()` 返回 `Option<VsockDriverEvent>`，支持“暂时无事件”的轮询模式。
- `recv_avail()` 把“当前可读字节数”单独抽成接口，说明上层可能需要先探测再读。

### 2.3 当前实现范围
本 crate 目前只定义契约，不内建任何具体设备实现。当前仓库里的实际实现来自 `ax_driver_virtio::VirtIoSocketDev`，也就是说，它本身是纯类别层。

## 3. 依赖关系图谱
### 3.1 直接依赖
| 依赖 | 作用 |
| --- | --- |
| `ax-driver-base` | 提供统一设备元信息和错误类型 |
| `log` | 为实现 crate 预留日志依赖环境 |

### 3.2 主要消费者
- `components/axdriver_crates/axdriver_virtio`
- `os/arceos/modules/axdriver`
- `os/arceos/modules/axnet-ng`

### 3.3 分层关系总结
- 向下不耦合任何具体总线。
- 向上作为 `ax-net-ng` 的一个设备能力来源。
- 真正的设备探测和 transport 建立仍由 `ax-driver` 与 `ax-driver-virtio` 负责。

## 4. 开发指南
### 4.1 何时修改这里
适合修改 `axdriver_vsock` 的情况包括：

- 需要为所有 vsock 设备增加共同元信息或事件类型。
- 需要调整连接标识或流控相关的公共接口。
- 需要让不同实现共享统一事件模型。

如果只是新增具体 VirtIO vsock 功能，应优先改实现 crate。

### 4.2 实现新驱动时的建议
1. 明确 `listen()`、`connect()`、`disconnect()`、`abort()` 的状态机语义。
2. 保持 `poll_event()` 返回事件和 `recv()` / `recv_avail()` 的状态一致。
3. 若底层协议存在 credit/窗口控制，最好在驱动实现里显式维护，而不要隐藏成无状态接口。

### 4.3 常见坑
- 不要把它和 TCP/UDP socket API 混为一谈；这里处理的是设备级 vsock 通道。
- 不要把连接 ID 当成持久化资源句柄；它只是驱动层连接标识。
- 不要在这里引入系统调用或文件描述符语义。

## 5. 测试策略
### 5.1 当前有效验证面
该 crate 没有独立测试目录。当前有效验证主要依赖：

- `virtio-vsock` 设备初始化。
- `ax_net_ng::init_vsock()` 是否能接管驱动。
- host/guest 之间的实际 vsock 通信。

### 5.2 建议补充的单元测试
- `VsockConnId::listening()` 构造行为。
- 事件枚举与连接标识的基本映射。
- mock 驱动上 `poll_event()`、`recv_avail()` 的契约测试。

### 5.3 集成测试重点
- 建立连接、发送、接收、断开和强制关闭。
- `CreditUpdate` 与读写窗口变化。
- 无事件轮询与异常断开分支。

### 5.4 风险点
- 事件模型和连接状态机一旦不一致，上层通常很难区分是驱动 bug 还是协议对端问题。
- 当前主线只看到 VirtIO 实现，若将来接入其它 vsock 设备，必须重新检查事件语义是否兼容。

## 6. 跨项目定位分析
### 6.1 ArceOS
ArceOS 通过 `ax-driver` 和 `ax-net-ng` 使用它，是当前仓库中唯一明确的主线落点。

### 6.2 StarryOS
当前仓库中没有看到 StarryOS 直接消费 `axdriver_vsock` 的证据，因此不应把它描述成 StarryOS 的常规网络接口层。

### 6.3 Axvisor
当前仓库中也没有看到 Axvisor 把 `axdriver_vsock` 当作虚拟化通信主接口使用。它不是 hypervisor 侧的 VM 通信控制层。
