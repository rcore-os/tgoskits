# `ax-driver-virtio` 技术文档

> 路径：`components/axdriver_crates/axdriver_virtio`
> 类型：库 crate
> 分层：组件层 / VirtIO 传输与设备适配层
> 版本：`0.1.4-preview.3`
> 文档依据：`Cargo.toml`、`README.md`、`src/lib.rs`、`src/blk.rs`、`src/net.rs`、`src/gpu.rs`、`src/input.rs`、`src/socket.rs`、`os/arceos/modules/axdriver/src/virtio.rs`、`platform/axplat-dyn/src/drivers/blk/mod.rs`

`ax-driver-virtio` 负责把 `virtio-drivers` crate 中的具体设备包装成 `axdriver_*` 系列接口。它既不是全局驱动聚合层，也不是总线枚举层，而是位于两者之间的 “VirtIO 设备适配层”：上接 `axdriver_block` / `axdriver_net` / `axdriver_display` / `axdriver_input` / `axdriver_vsock` 的类别 trait，下接 `virtio-drivers` 的 MMIO/PCI transport 与设备对象。

## 1. 架构设计分析
### 1.1 设计定位
本 crate 的职责可以概括为两部分：

- **设备包装**：把 VirtIO block/net/gpu/input/socket 设备包装成 `*DriverOps` 实现。
- **传输探测辅助**：提供 `probe_mmio_device()` 和 `probe_pci_device()`，识别 VirtIO 设备类型并构造 transport 对象。

它在分层中的位置很明确：

- 不是 `ax-driver`：不负责枚举所有设备，也不维护 `AllDevices`。
- 不是 `ax-driver-pci`：不负责扫描总线或分配 BAR。
- 也不是 `virtio-drivers` 本身：它提供的是 ArceOS 风格的包装接口，而非原始 VirtIO API。

### 1.2 模块划分
| 模块 | feature | 作用 |
| --- | --- | --- |
| `blk` | `block` | `VirtIoBlkDev`，实现 `BlockDriverOps` |
| `net` | `net` | `VirtIoNetDev`，实现 `NetDriverOps` |
| `gpu` | `gpu` | `VirtIoGpuDev`，实现 `DisplayDriverOps` |
| `input` | `input` | `VirtIoInputDev`，实现 `InputDriverOps` |
| `socket` | `socket` | `VirtIoSocketDev`，实现 `VsockDriverOps` |
| `lib.rs` | 始终存在 | 探测辅助、错误映射、transport 与 HAL 类型导出 |

### 1.3 关键对象
| 符号 | 作用 |
| --- | --- |
| `VirtIoHal` | `virtio_drivers::Hal` 的别名，要求平台提供 DMA 与地址转换 |
| `MmioTransport` / `PciTransport` | VirtIO 设备的两条 transport 路径 |
| `probe_mmio_device()` | 识别一段 MMIO 区间是否为支持的 VirtIO 设备 |
| `probe_pci_device()` | 识别 PCI 设备是否为支持的 VirtIO 设备，并计算 IRQ |
| `as_dev_type()` | 把 VirtIO 设备类型映射到 `ax_driver_base::DeviceType` |

### 1.4 设备包装方式
每个具体设备模块都做两件事：

1. 保存底层 `virtio-drivers` 设备对象。
2. 把底层接口翻译成 `axdriver_*` 对应的 trait。

例如：

- `VirtIoBlkDev` 把 `read_blocks()` / `write_blocks()` 翻译成 `BlockDriverOps`。
- `VirtIoNetDev` 用 `NetBufPool` 预分配 RX/TX 缓冲，并实现 `receive()` / `transmit()` / 回收逻辑。
- `VirtIoGpuDev` 用 `setup_framebuffer()` 建立帧缓冲并对接 `flush()`。
- `VirtIoInputDev` 通过 `query_config_select()` 暴露事件位图，通过 `pop_pending_event()` 取事件。
- `VirtIoSocketDev` 把底层 vsock 事件翻译成 `VsockDriverEvent`。

### 1.5 与 `ax-driver` 的配合方式
在 `os/arceos/modules/axdriver/src/virtio.rs` 中，ArceOS 进一步定义了：

- `VirtIoDevMeta`：为每种 VirtIO 设备绑定 `DeviceType`、具体 `Device` 类型和 `try_new()`。
- `VirtIoDriver<D>`：实现 `DriverProbe`，把 MMIO/PCI 识别结果转成 `AxDeviceEnum`。
- `VirtIoHalImpl`：对接 `ax-alloc`、`ax-hal`，为 `virtio-drivers` 提供 DMA 和地址转换。

因此本 crate 只做到“把一个已经识别出来的 VirtIO 设备变成类别驱动”；真正把它纳入系统初始化流程的是 `ax-driver`。

### 1.6 与动态平台路径的关系
`platform/axplat-dyn` 的块设备动态探测会直接使用本 crate：

- 先探测 `virtio,mmio` 设备；
- 再构造 `VirtIoBlkDev`；
- 最终把它包装成实现 `BlockDriverOps` 的动态块设备。

这说明 `ax-driver-virtio` 不是只服务 `os/arceos/modules/axdriver` 一家，它也可以被其它平台 glue 层直接拿来做设备包装。

### 1.7 当前实现中的现实细节
- `probe_pci_device()` 会按架构计算一个 `PCI_IRQ_BASE + (bdf.device & 3)` 的 IRQ 号，这是一套仓库内约定，而不是 VirtIO 规范本身。
- `gpu.rs` 和 `input.rs` 内部有若干 `unwrap()`，说明初始化失败在某些分支上会比 `DevError` 更早暴露为 panic。
- `socket.rs` 当前使用 `VsockConnectionManager` 并为缓冲区固定分配 32 KiB 容量。

### 1.8 边界澄清
最关键的边界是：**`ax-driver-virtio` 负责“把 VirtIO 设备包装成 ArceOS 驱动接口”，但它不负责全局设备探测编排，也不负责 PCI/MMIO 总线扫描。**

## 2. 核心功能说明
### 2.1 主要能力
- 识别支持的 VirtIO MMIO/PCI 设备类型。
- 为 block/net/gpu/input/socket 提供统一包装。
- 向外导出 `VirtIoHal`、`Transport` 等关键类型，便于平台提供 HAL glue。
- 把底层 `virtio-drivers::Error` 映射到统一的 `DevError`。

### 2.2 feature 矩阵
| Feature | 作用 |
| --- | --- |
| `alloc` | 打开 `virtio-drivers/alloc` 支持 |
| `block` | 编译 `VirtIoBlkDev`，依赖 `axdriver_block` |
| `net` | 编译 `VirtIoNetDev`，依赖 `axdriver_net` |
| `gpu` | 编译 `VirtIoGpuDev`，依赖 `axdriver_display` |
| `input` | 编译 `VirtIoInputDev`，依赖 `axdriver_input` |
| `socket` | 编译 `VirtIoSocketDev`，依赖 `axdriver_vsock` |

### 2.3 当前支持的设备映射
`as_dev_type()` 当前只把以下 VirtIO 设备映射进 ArceOS 驱动类别：

- `Block` -> `DeviceType::Block`
- `Network` -> `DeviceType::Net`
- `GPU` -> `DeviceType::Display`
- `Input` -> `DeviceType::Input`
- `Socket` -> `DeviceType::Vsock`

这也意味着本 crate 不是“所有 VirtIO 设备的统一外壳”，而是只覆盖当前仓库已接入的那几个类别。

## 3. 依赖关系图谱
### 3.1 直接依赖
| 依赖 | 作用 |
| --- | --- |
| `virtio-drivers` | 提供底层 transport 和设备实现 |
| `ax-driver-base` | 提供统一设备类型和错误模型 |
| `axdriver_block` / `display` / `input` / `net` / `vsock` | 提供各类别 trait |
| `log` | 初始化和错误日志 |

### 3.2 主要消费者
- `os/arceos/modules/axdriver`
- `platform/axplat-dyn`

### 3.3 分层关系总结
- 向下连接 `virtio-drivers`。
- 向上输出 `axdriver_*` 兼容设备对象。
- 由 `ax-driver` 决定这些对象何时、如何进入 `AllDevices`。

## 4. 开发指南
### 4.1 新增一种 VirtIO 设备支持时要改哪些地方
1. 在本 crate 中新增对应模块，实现目标 `*DriverOps`。
2. 在 `lib.rs` 中加 feature、导出和 `as_dev_type()` 映射。
3. 在 `os/arceos/modules/axdriver/src/virtio.rs` 中补 `VirtIoDevMeta`。
4. 在 `os/arceos/modules/axdriver/src/drivers.rs` 中注册对应类别驱动。
5. 若需要顶层 feature，还要同步 `ax-driver/Cargo.toml` 和 `ax-feat/Cargo.toml`。

### 4.2 HAL 接入注意事项
- `VirtIoHal` 要正确实现 DMA 分配、回收、MMIO 地址转换、share/unshare。
- ArceOS 当前 `VirtIoHalImpl` 主要基于 `ax-alloc::global_allocator()` 和 `ax-hal::mem::{phys_to_virt, virt_to_phys}`。
- 不同平台若总线地址不等于物理地址，必须在 HAL 层处理，不要把这个问题推给设备包装层。

### 4.3 常见坑
- 仅仅识别到设备类型并不代表驱动初始化一定成功；具体 `try_new()` 仍可能失败。
- `probe_pci_device()` 计算 IRQ 的方式带有平台/架构假设，迁移时需要重新检查。
- `gpu.rs` / `input.rs` 的 `unwrap()` 表明某些错误分支还未完全软化为 `DevError`。

## 5. 测试策略
### 5.1 当前有效验证面
该 crate 没有独立测试目录，当前主要依赖：

- QEMU/平台上的 VirtIO MMIO 或 PCI 启动。
- `ax-driver` 对 `virtio-*` 设备的探测与初始化。
- `ax-display`、`ax-input`、`ax-net`、`ax-fs`、`ax-net-ng` 对包装后设备的实际消费。

### 5.2 建议补充的单元测试
- `as_dev_type()` 和 `as_dev_err()` 的映射。
- `probe_mmio_device()` / `probe_pci_device()` 对不同设备类型的识别。
- `VirtIoNetDev` 的缓冲回收流程。

### 5.3 集成测试重点
- `virtio-blk` 文件系统挂载。
- `virtio-net` 网络收发。
- `virtio-gpu` framebuffer 刷新。
- `virtio-input` 事件读取。
- `virtio-vsock` 连接、收发与事件轮询。

### 5.4 风险点
- HAL 地址转换或 DMA 错误会影响所有 VirtIO 设备。
- 设备初始化中的 `unwrap()` 会让某些异常以 panic 形式暴露，而不是优雅错误返回。

## 6. 跨项目定位分析
### 6.1 ArceOS
ArceOS 是当前最主要的主线消费者：`ax-driver` 通过它把 VirtIO 设备接入块、网、显、输入和 vsock 各类别路径。

### 6.2 StarryOS
StarryOS 若通过共享 ArceOS 驱动栈获得显示、输入或存储能力，会间接使用本 crate；但它并不把本 crate 当作独立的 VirtIO 管理框架。

### 6.3 Axvisor
当前仓库里没有看到 Axvisor 直接把 `ax-driver-virtio` 作为其虚拟设备框架使用。这里处理的是宿主侧/内核侧 VirtIO 设备包装，而不是 VMM 侧 VirtIO 仿真。
