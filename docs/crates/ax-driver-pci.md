# `ax-driver-pci` 技术文档

> 路径：`components/axdriver_crates/axdriver_pci`
> 类型：库 crate
> 分层：组件层 / PCI 总线访问层
> 版本：`0.1.4-preview.3`
> 文档依据：`Cargo.toml`、`README.md`、`src/lib.rs`、`os/arceos/modules/axdriver/src/bus/pci.rs`、`os/arceos/modules/axdriver/src/virtio.rs`

`ax-driver-pci` 的定位非常清晰：它是 `ax-driver` 体系里的 PCI 总线访问辅助层。它不负责把设备按类别聚合，也不负责实现具体网卡、块设备或显示设备驱动；它提供的是 PCI 配置空间相关类型的统一来源，以及一个很小但很关键的 `PciRangeAllocator`，供上层为未分配地址的 PCI BAR 安排 MMIO 窗口。

## 1. 架构设计分析
### 1.1 设计定位
这个 crate 的源码很小，但边界十分明确：

- 绝大多数 PCI 相关类型直接从 `virtio_drivers::transport::pci::bus` 再导出。
- 本 crate 自己真正新增的核心对象只有 `PciRangeAllocator`。
- 真正的 PCI 枚举、BAR 配置和驱动派发逻辑在 `os/arceos/modules/axdriver/src/bus/pci.rs`。

因此它更像是 ArceOS 驱动栈里的“PCI 公共类型与分配辅助层”，而不是独立 PCI 子系统。

### 1.2 关键对象
| 符号 | 作用 | 来源 |
| --- | --- | --- |
| `PciRoot`、`DeviceFunction`、`BarInfo` 等 | 访问 PCI 配置空间和设备信息 | 由 `virtio-drivers` 再导出 |
| `PciRangeAllocator` | 为 BAR 分配 MMIO 地址空间 | 本 crate 自定义 |
| `PciError`、`Command`、`Status` | PCI 操作辅助类型 | 由 `virtio-drivers` 再导出 |

### 1.3 `PciRangeAllocator` 的作用
`PciRangeAllocator` 只做一件事：从一段 MMIO 地址窗口中分配满足对齐要求的连续区域。

其特点是：

- `new(base, size)` 以一段物理窗口初始化分配器。
- `alloc(size)` 要求 `size` 必须是 2 的幂。
- 返回地址会自动按 `size` 对齐。
- 若空间不足或参数不合法，返回 `None`。

它本质上是一个顺序递增分配器，适合 BAR 窗口这种“启动期一次性分配、运行期不回收”的场景。

### 1.4 在 `ax-driver` 中的真实调用方式
在 `os/arceos/modules/axdriver/src/bus/pci.rs` 里，PCI 探测主线如下：

1. 用 `PciRoot::new(..., Cam::Ecam)` 打开 ECAM。
2. 从 `ax_config::devices::PCI_RANGES` 取出 32 位 MMIO 窗口，创建 `PciRangeAllocator`。
3. 枚举每个 bus 上的设备。
4. 对 BAR 逐个调用 `config_pci_device()`：
   - 若 BAR 未分配地址，调用 `PciRangeAllocator::alloc()` 分配。
   - 再把 `IO_SPACE`、`MEMORY_SPACE`、`BUS_MASTER` 打开。
5. 最后再把设备交给 `Driver::probe_pci()` 做类别识别和具体驱动创建。

这说明：

- `ax-driver-pci` 负责的是“访问和分配”。
- `ax-driver` 才负责“枚举和派发”。

### 1.5 与 VirtIO 路径的关系
`ax_driver_virtio::probe_pci_device()` 和 `os/arceos/modules/axdriver/src/virtio.rs` 会继续基于 `PciRoot`、`DeviceFunction`、`DeviceFunctionInfo` 做 VirtIO PCI 设备识别。因此 `ax-driver-pci` 也是 VirtIO PCI 探测路径的底层依赖。

### 1.6 边界澄清
最关键的边界是：**`ax-driver-pci` 是 PCI 总线访问辅助层，不是通用驱动聚合层，也不是完整的 PCI 设备管理子系统。**

## 2. 核心功能说明
### 2.1 主要能力
- 统一提供 PCI 配置空间访问相关类型。
- 为 BAR 资源分配提供最小分配器 `PciRangeAllocator`。
- 为上层 `ax-driver` 的 PCI 枚举和 VirtIO PCI 探测提供公共底座。

### 2.2 当前实现特征
- crate 自身没有 Cargo feature，也没有内部子模块拆分。
- 本地逻辑极少，绝大部分能力直接复用 `virtio-drivers`。
- 设计上故意不引入设备类别概念，从而保持总线层的中立性。

### 2.3 不负责的事情
- 不负责扫描 bus 范围和设备树。
- 不负责 IRQ 路由。
- 不负责设备类别识别。
- 不负责 DMA、IOMMU 或驱动生命周期管理。

## 3. 依赖关系图谱
### 3.1 直接依赖
| 依赖 | 作用 |
| --- | --- |
| `virtio-drivers` | 提供 PCI bus 访问类型和实现 |

### 3.2 主要消费者
- `os/arceos/modules/axdriver/src/bus/pci.rs`
- `os/arceos/modules/axdriver/src/virtio.rs`

### 3.3 分层关系总结
- 向下：依赖 `virtio-drivers` 的 PCI bus 实现。
- 向上：服务 `ax-driver` 的 PCI 探测与 VirtIO PCI 路径。
- 横向：保持对设备类别中立，不直接依赖 `axdriver_block`、`axdriver_net` 等类别 crate。

## 4. 开发指南
### 4.1 何时应改这里
适合修改 `ax-driver-pci` 的场景包括：

- 需要给所有 PCI 设备路径新增通用辅助逻辑。
- 需要补充 BAR 分配或 PCI 配置访问相关能力。
- 需要统一仓库内对 `virtio-drivers` PCI bus API 的适配点。

如果只是新增某类 PCI 设备驱动，通常应修改 `ax-driver` 或对应设备 crate，而不是这里。

### 4.2 修改时要同步检查的地方
1. `os/arceos/modules/axdriver/src/bus/pci.rs` 的枚举和 BAR 配置逻辑。
2. `ax_config::devices::PCI_ECAM_BASE`、`PCI_RANGES`、`PCI_BUS_END` 等平台配置。
3. `ax-driver-virtio` 的 PCI 探测路径是否仍与类型再导出保持一致。

### 4.3 常见坑
- 不要把 `PciRangeAllocator` 当成通用内存分配器；它只适合启动期 BAR 窗口。
- 不要在这里引入按设备类别分支的逻辑；那会污染总线层边界。
- 升级 `virtio-drivers` 时，要注意其 PCI 类型 API 变更可能直接影响本 crate 的对外接口。

## 5. 测试策略
### 5.1 当前有效验证面
该 crate 没有独立测试目录，当前有效验证主要来自：

- QEMU/真实平台上的 PCI 枚举。
- BAR 地址自动分配。
- VirtIO PCI 和 ixgbe PCI 设备是否能被后续驱动正确接管。

### 5.2 建议补充的单元测试
- `PciRangeAllocator::alloc()` 的对齐、越界和非法参数处理。
- 不同 BAR 大小组合下的顺序分配结果。

### 5.3 集成测试重点
- `PCI_ECAM_BASE` 有效性与总线枚举。
- 未初始化 BAR 的自动分配与命令寄存器启用。
- `virtio-net`、`virtio-blk`、`ixgbe` 等不同 PCI 设备的后续探测。

### 5.4 风险点
- BAR 分配错误会直接导致后续驱动映射错误，通常很难在更上层快速定位。
- 若把设备类别识别逻辑下沉到这里，会破坏整个驱动栈的层次边界。

## 6. 跨项目定位分析
### 6.1 ArceOS
ArceOS 是当前仓库里唯一明确的直接主线消费者。`ax-driver` 的 PCI 探测路径完全建立在本 crate 暴露的类型和分配器之上。

### 6.2 StarryOS
StarryOS 若复用 ArceOS 底层驱动栈，会间接依赖本 crate；但当前仓库中没有看到它作为 StarryOS 独立 PCI 子系统直接存在。

### 6.3 Axvisor
当前仓库里没有看到 Axvisor 直接依赖 `ax-driver-pci`。它不是 Axvisor 的宿主 PCI 管理核心，也不是虚拟 PCI 设备框架。
