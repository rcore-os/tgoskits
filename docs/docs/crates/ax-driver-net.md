# `axdriver_net` 技术文档

> 路径：`components/axdriver_crates/axdriver_net`
> 类型：库 crate
> 分层：组件层 / 网络设备类别接口层
> 版本：`0.1.4-preview.3`
> 文档依据：`Cargo.toml`、`README.md`、`src/lib.rs`、`src/net_buf.rs`、`src/fxmac.rs`、`src/ixgbe.rs`、`components/axdriver_crates/axdriver_virtio/src/net.rs`、`os/arceos/modules/axdriver/src/drivers.rs`

`axdriver_net` 的定位是 NIC 驱动类别层，而不是网络栈。它一方面定义网卡驱动必须实现的 `NetDriverOps`，另一方面内建 `fxmac` 和 `ixgbe` 两个具体实现模块，并提供一套在当前 ArceOS 网络驱动栈里非常关键的缓冲区抽象 `NetBuf` / `NetBufPool` / `NetBufPtr`。上层 `ax-net`、`ax-net-ng` 依赖它消费网卡，下层具体设备和总线探测则由 `ax-driver`、`ax-driver-virtio` 等承担。

## 1. 架构设计分析
### 1.1 设计定位
这个 crate 同样是“类别契约 + 部分叶子实现”的混合结构：

- `NetDriverOps` 统一定义 NIC 的收发、队列和缓冲区契约。
- `net_buf.rs` 提供通用网络缓冲区模型。
- `fxmac.rs` 和 `ixgbe.rs` 在 feature 打开时提供具体网卡实现。

因此它既不是纯接口层，也不是完整网络子系统。真正的 TCP/IP、socket、协议栈和接口管理在 `ax-net` / `ax-net-ng` 中。

### 1.2 关键接口与对象
| 符号 | 作用 | 备注 |
| --- | --- | --- |
| `EthernetAddress` | MAC 地址类型 | 设备级身份信息 |
| `NetDriverOps` | NIC 统一 trait | 定义收发与队列接口 |
| `NetBufPtr` | 对已分配网络缓冲的轻量句柄 | 通过原始指针连接底层缓冲 |
| `NetBuf` | RAII 网络缓冲区 | Drop 时自动归还池 |
| `NetBufPool` | 固定长度缓冲池 | 提升收发缓冲分配效率 |

### 1.3 缓冲区模型
`NetBuf` 系列是本 crate 的一大实现重点：

- `NetBufPool` 把一大块内存切成定长槽位，池容量和每块长度在创建时固定。
- `NetBuf` 区分 header 和 packet 两个逻辑区域。
- `NetBuf::into_buf_ptr()` 把 RAII 对象转成 `NetBufPtr`，便于驱动把所有权交给底层队列。
- `NetBuf::from_buf_ptr()` 再把裸指针还原回来。

这套设计说明 `axdriver_net` 关注的不只是 trait 形状，还关注驱动与上层之间的缓冲所有权转移模型。

### 1.4 `NetDriverOps` 的核心语义
`NetDriverOps` 的方法大致可分为四组：

- 元信息：`mac_address()`。
- 队列状态：`can_transmit()`、`can_receive()`、`rx_queue_size()`、`tx_queue_size()`。
- 缓冲回收：`recycle_rx_buffer()`、`recycle_tx_buffers()`。
- 数据通路：`transmit()`、`receive()`、`alloc_tx_buffer()`。

其中最容易被误解的是 `receive()` / `recycle_rx_buffer()` 配对关系：驱动把收到的 `NetBufPtr` 交给上层后，上层必须在处理完成后归还给驱动，否则收包队列会耗尽。

### 1.5 当前内建实现
#### `fxmac`
`FXmacNic` 基于 `fxmac_rs`，主要特点是：

- 通过 `KernelFunc` 接口向外索取 `virt_to_phys`、`phys_to_virt`、DMA 分配和 IRQ 申请等平台能力。
- `receive()` 会调用 `FXmacRecvHandler()` 拉取批量报文，再封成 `NetBufPtr`。
- `transmit()` 直接转发给 `FXmacLwipPortTx()`。

#### `ixgbe`
`IxgbeNic` 基于 `ixgbe-driver`，主要特点是：

- 用 `MemPool` 管理 DMA 友好的网卡缓冲。
- `init(base, len)` 完成设备初始化。
- `receive_packets()` 支持批量接收并转成 `NetBufPtr`。
- `IxgbeHal` 由 `os/arceos/modules/axdriver/src/ixgbe.rs` 对接 `ax-dma`。

### 1.6 与 `ax-driver` 和 `ax-driver-virtio` 的接线关系
当前仓库中的三条主要接线路径是：

- `ax_driver_virtio::VirtIoNetDev`：用 `NetBufPool` 组织 VirtIO 队列收发。
- `ax-driver::drivers::IxgbeDriver`：在 PCI 探测路径中构造 `IxgbeNic`。
- `ax-driver::drivers::FXmacDriver`：在全局 probe 路径中构造 `FXmacNic`，并通过 `crate_interface` 实现 `KernelFunc`。

所以 `axdriver_net` 负责“网卡应该如何工作”，而不是“系统去哪里找到网卡”。

### 1.7 边界澄清
最重要的边界是：**`axdriver_net` 是 NIC 驱动类别层，不是网络栈、socket 层，也不是接口管理层。**

## 2. 核心功能说明
### 2.1 主要能力
- 定义统一的 NIC 驱动 trait `NetDriverOps`。
- 提供网络缓冲区池和所有权转换模型。
- 通过 feature 内建 `fxmac` 和 `ixgbe` 网卡实现。
- 作为 `virtio-net`、`ixgbe`、`fxmac` 等不同设备路径的共同上层接口。

### 2.2 当前实现特征
- `NetBufPool::new(capacity, buf_len)` 要求 `buf_len` 落在 `1526..=65535` 范围内。
- `VirtIoNetDev::try_new()` 会预填 RX 缓冲并预分配 TX 缓冲，说明当前网络接口假设底层驱动能长期持有这些缓冲。
- `ixgbe` 与 `fxmac` 的缓冲来源完全不同，但都收敛到 `NetBufPtr` 接口，这正是本 crate 的统一价值。

### 2.3 对上层的意义
对 `ax-net` / `ax-net-ng` 来说，它们并不关心底层设备是 VirtIO、Intel 82599 还是 Phytium FXmac，只需要按 `NetDriverOps` 拿到：

- 一个 MAC 地址；
- 一个可收发的网卡；
- 一组可回收的收发缓冲。

这正是类别层和网络栈之间的清晰分界。

## 3. 依赖关系图谱
### 3.1 直接依赖
| 依赖 | 作用 |
| --- | --- |
| `ax-driver-base` | 提供共性设备接口与错误类型 |
| `spin` | 保护 `NetBufPool` 空闲链表 |
| `fxmac_rs` | `fxmac` 实现的底层库 |
| `ixgbe-driver` | `ixgbe` 实现的底层库 |
| `log` | 初始化和错误日志 |

### 3.2 主要消费者
- `components/axdriver_crates/axdriver_virtio`
- `os/arceos/modules/axdriver`
- `os/arceos/modules/ax-net`
- `os/arceos/modules/axnet-ng`

### 3.3 分层关系总结
- 向下可接不同 NIC 实现。
- 向上统一暴露网卡语义。
- 由 `ax-driver` 决定设备探测与聚合，由 `ax-net`/`ax-net-ng` 决定协议栈语义。

## 4. 开发指南
### 4.1 何时应该改这里
适合修改 `axdriver_net` 的场景包括：

- 需要扩展所有 NIC 驱动都共有的接口契约。
- 需要调整通用网络缓冲区模型。
- 需要增加一个应被整个 ArceOS 生态共享的网卡实现模块。

如果只是处理某个网络协议或接口配置，不应改这里。

### 4.2 新增驱动时的建议
1. 先决定是实现 `NetDriverOps` 还是直接在本 crate 内新增 feature 模块。
2. 明确 `receive()` 返回后缓冲区如何归还。
3. 如果驱动依赖 DMA，需要同时明确 DMA 分配和地址转换由哪一层提供。
4. 若接入 `ax-driver` 探测主线，还需同步修改 `os/arceos/modules/axdriver/src/drivers.rs`。

### 4.3 常见坑
- `receive()` 返回的 `NetBufPtr` 不是普通切片，背后有明确的所有权和回收语义。
- `can_transmit()` / `can_receive()` 只是即时状态，不代表永远可用。
- 不要把本 crate 写成“网络子系统”；路由、socket、poll 接口都不在这里。

## 5. 测试策略
### 5.1 当前有效验证面
该 crate 没有独立测试目录，当前有效验证主要来自：

- `virtio-net`、`ixgbe`、`fxmac` 的整机 bring-up。
- `ax-net` / `ax-net-ng` 的网络收发路径。
- `NetBufPool` 在不同驱动上的缓冲回收行为。

### 5.2 建议补充的单元测试
- `NetBufPool` 的容量、长度边界和回收行为。
- `NetBuf` 的 header / packet 区域长度管理。
- `NetBufPtr` 与 `NetBuf` 的往返恢复。

### 5.3 集成测试重点
- QEMU `virtio-net` 网络冒烟。
- ixgbe PCI 探测和 DMA 路径。
- fxmac 平台初始化和报文收发。
- 长时间收发下的缓冲回收稳定性。

### 5.4 风险点
- 缓冲回收协议一旦写错，问题往往表现为“跑一段时间后 RX/TX 队列耗尽”。
- DMA 和物理地址转换错误通常不会只影响一个包，而会直接让整个 NIC 不稳定。

## 6. 跨项目定位分析
### 6.1 ArceOS
ArceOS 是当前仓库里最主要的直接消费者：`ax-driver` 负责把设备接进来，`ax-net` / `ax-net-ng` 负责把它们变成网络能力。

### 6.2 StarryOS
StarryOS 当前并没有把 `axdriver_net` 当成独立网络栈；若复用网络能力，也主要是通过共享的 ArceOS 底层模块链路间接使用。

### 6.3 Axvisor
当前仓库未显示 Axvisor 直接以 `axdriver_net` 作为其主线网卡框架。它不是虚拟化设备网络平面的核心抽象。
