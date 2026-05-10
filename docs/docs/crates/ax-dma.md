# `ax-dma` 技术文档

> 路径：`os/arceos/modules/axdma`
> 类型：库 crate
> 分层：ArceOS 层 / DMA 内存服务层
> 版本：`0.3.0-preview.3`
> 文档依据：`Cargo.toml`、`src/lib.rs`、`src/dma.rs`、`os/arceos/modules/axdriver/src/ixgbe.rs`、`os/arceos/modules/axdriver/src/drivers.rs`、`os/arceos/api/ax-api/src/imp/mem.rs`、`platform/axplat-dyn/src/drivers/mod.rs`、`os/axvisor/src/driver/blk/mod.rs`

`ax-dma` 不是驱动聚合层，也不是某类设备驱动。它的真实职责是为 ArceOS 内核提供一套全局一致的 DMA 一致性内存分配服务：从页分配器拿到内存、把页表属性改成 `UNCACHED`、给设备返回可用的总线地址，并在释放时尽可能恢复映射属性。它位于 `ax-alloc` / `ax-mm` / `ax-hal` 等内存基础设施之上，位于需要软件管理 DMA 缓冲的驱动之下。

## 1. 架构设计分析
### 1.1 设计定位
`ax-dma` 解决的是这样一个问题：

- CPU 侧需要拿到一块可访问的虚拟地址。
- 设备侧需要拿到与之对应的总线地址。
- 这块内存还需要满足 DMA 使用场景下的一致性或缓存属性要求。

因此它承担的是 **DMA 内存服务**，而不是设备驱动逻辑。它不会告诉系统“有哪些设备”，也不会教某个网卡如何编程发送描述符。

### 1.2 核心公开对象
| 符号 | 作用 |
| --- | --- |
| `BusAddr` | 设备侧看到的总线地址包装类型 |
| `DMAInfo` | 同时携带 CPU 虚拟地址与总线地址 |
| `phys_to_bus()` | 按平台偏移把物理地址转成总线地址 |
| `alloc_coherent()` | 分配 DMA 一致性内存 |
| `dealloc_coherent()` | 释放 DMA 一致性内存 |

### 1.3 内部对象与主线
内部实现集中在 `src/dma.rs`：

- `ALLOCATOR: SpinNoIrq<DmaAllocator>`：全局 DMA 分配器。
- `DmaAllocator { alloc: DefaultByteAllocator }`：既支持页级分配，也支持小块字节分配。
- `update_flags()`：通过 `ax-mm::kernel_aspace().protect()` 修改页表属性。

`alloc_coherent(layout)` 的主线有两条：

1. 若 `layout.size() >= 4 KiB`，走 `alloc_coherent_pages()`：
   - 直接从全局页分配器申请页；
   - 把页属性改成 `READ | WRITE | UNCACHED`；
   - 通过 `virt_to_bus()` 返回总线地址。
2. 若小于 4 KiB，走 `alloc_coherent_bytes()`：
   - 先尝试从内部字节分配器分配；
   - 若空间不够，则从全局页分配器按 4 页扩容一段 DMA 内存；
   - 再把这段内存加入字节分配器。

释放时：

- 大块分配会归还页，并尽量把页属性恢复到 `READ | WRITE`。
- 小块分配则归还给内部字节分配器。

### 1.4 `hv` feature 的行为差异
开启 `hv` 时，内部字节分配器会改用 `buddy-slab-allocator` 提供的接口，并且小块分配路径不再尝试 `add_memory` 动态扩容。这意味着：

- 非 `hv` 模式更像“按需从页分配器借内存扩展字节池”。
- `hv` 模式更像“在已有 slab 空间里分配，失败就直接报错”。

### 1.5 地址模型
`phys_to_bus()` 的实现非常直接：

- 先得到物理地址；
- 再加上 `ax_config::plat::PHYS_BUS_OFFSET`；
- 最终得到总线地址。

这说明当前 `ax-dma` 假设平台满足一种简单的线性总线地址模型，而不是依赖 IOMMU 做复杂映射。

### 1.6 与驱动栈的真实接线关系
当前仓库里，`ax-dma` 的最明确直接使用者是 `os/arceos/modules/axdriver/src/ixgbe.rs`：

- `IxgbeHalImpl::dma_alloc()` 调用 `ax_dma::alloc_coherent()`；
- `dma_dealloc()` 调用 `ax_dma::dealloc_coherent()`。

但要特别注意一个实现事实：

- `ax-driver/Cargo.toml` 中 `fxmac` feature 也声明了 `dep:ax-dma`；
- 可是 `os/arceos/modules/axdriver/src/drivers.rs` 里的 `FXmacDriver` glue 实际使用的是 `ax-alloc::global_allocator().alloc_pages(..., UsageKind::Dma)`，并没有直接调用 `ax-dma` 的 coherent allocator。

这说明在当前代码树里，**`ax-dma` 不是所有 DMA 驱动的唯一后端**，而是其中一条已被明确使用的 DMA 服务路径。

### 1.7 与其它 DMA 实现的边界
仓库里至少还有两条独立 DMA 路径：

- `platform/axplat-dyn/src/drivers/mod.rs`：提供 `dma_api::DmaOp` 实现，服务动态平台块设备探测。
- `os/axvisor/src/driver/blk/mod.rs`：提供 Axvisor 自己的 `rdif_block::dma_api::DmaOp` 实现。

因此不能把 `ax-dma` 写成“整个仓库唯一的 DMA 抽象层”；它是 ArceOS 主线中的一个内核 DMA 内存服务模块。

### 1.8 边界澄清
最关键的边界是：**`ax-dma` 负责分配一致性 DMA 内存并给出总线地址，但不负责设备探测、描述符编程、总线枚举或 IOMMU 策略。**

## 2. 核心功能说明
### 2.1 主要能力
- 分配和释放 DMA 一致性内存。
- 把 CPU 虚拟地址和总线地址绑定成 `DMAInfo`。
- 通过页表属性修改保证这段内存按 DMA 需要工作。
- 为 ArceOS API 层和部分驱动提供统一的 DMA 内存入口。

### 2.2 对上层 API 的关系
`os/arceos/api/ax-api/src/imp/mem.rs` 会在 `cfg_dma!` 下直接重导出：

- `ax_alloc_coherent()`
- `ax_dealloc_coherent()`
- `DMAInfo`

这说明 `ax-dma` 不只是驱动内部工具，也被设计成可向更高层 API 暴露的系统能力。

### 2.3 与 `ax-runtime` / `ax-feat` 的关系
需要区分两个概念：

- `ax-runtime` 的 `dma` feature 只是 `["paging"]`，它并不直接依赖 `ax-dma` crate。
- `ax-feat` 的 `dma` feature 表示整机要具备 DMA 所需的内存和分页能力。

也就是说，`ax-dma` 更像一个“可被 API 或驱动选用的具体实现模块”，而不是 runtime 自动初始化出来的独立子系统。

## 3. 依赖关系图谱
### 3.1 直接依赖
| 依赖 | 作用 |
| --- | --- |
| `ax-alloc` | 全局页分配器与 DMA 用页申请 |
| `axallocator` / `buddy-slab-allocator` | 字节级分配器实现 |
| `axconfig` | 提供 `PHYS_BUS_OFFSET` |
| `ax-hal` | 提供物理地址与页表标志相关能力 |
| `ax-mm` | 修改内核地址空间页表属性 |
| `ax-kspin` | 保护全局分配器 |
| `memory_addr` | 地址与页大小辅助 |
| `log` | 错误与调试日志 |

### 3.2 主要消费者
- `os/arceos/modules/axdriver/src/ixgbe.rs`
- `os/arceos/api/ax-api/src/imp/mem.rs`

### 3.3 分层关系总结
- 向下依赖内存分配和页表管理。
- 向上服务需要 DMA 内存的驱动或 API。
- 不参与具体设备类别聚合，也不参与最终网络/存储协议。

## 4. 开发指南
### 4.1 适合修改这里的场景
应修改 `ax-dma` 的情况主要包括：

- DMA 一致性内存分配策略需要调整。
- 平台总线地址换算规则需要统一变更。
- 页表缓存属性或 `hv` 模式策略需要重构。

如果只是某个设备需要特殊 DMA map/unmap 协议，不一定应改这里；可能更适合在设备或平台 glue 层实现。

### 4.2 修改时必须同步检查的地方
1. 页级与字节级两条分配路径是否都正确设置 `UNCACHED`。
2. `dealloc_coherent()` 是否和分配路径对称。
3. `PHYS_BUS_OFFSET` 的平台假设是否仍成立。
4. 直接使用者，例如 `ixgbe` 和 `ax-api`，是否仍满足其地址与生命周期假设。

### 4.3 常见坑
- 不要把 `ax-dma` 当成 IOMMU 管理器；它只做简单地址换算和一致性内存管理。
- 不要假设仓库中所有 DMA 设备都走这里；当前动态平台块设备和 Axvisor 就有独立实现。
- 对小块分配路径的扩容与页属性修改不能遗漏，否则问题会非常隐蔽。

## 5. 测试策略
### 5.1 当前有效验证面
当前主要验证路径包括：

- `ixgbe` 驱动的 DMA 分配与回收。
- `ax-api` 的 `ax_alloc_coherent()` / `ax_dealloc_coherent()`。
- 小块分配和页级分配两条路径的正常工作。

### 5.2 建议补充的单元测试
- `phys_to_bus()` 对 `PHYS_BUS_OFFSET` 的换算。
- 小于 4 KiB 和大于等于 4 KiB 两条分配路径。
- 分配后页属性变更与释放后属性恢复。
- `hv` 与非 `hv` 两种模式的行为差异。

### 5.3 集成测试重点
- `ixgbe` 真实收发路径。
- API 层分配后由驱动消费的端到端场景。
- 在页分配器压力较大时的小块扩容与失败分支。

### 5.4 风险点
- DMA 内存属性不正确时，错误通常不是立即 panic，而是表现为设备行为不稳定或数据损坏。
- 若平台总线地址模型不再满足 `phys + offset`，本 crate 的地址换算将整体失效。

## 6. 跨项目定位分析
### 6.1 ArceOS
`ax-dma` 是 ArceOS 主线中的 DMA 内存服务模块，当前最明确服务于 `ixgbe` 和 API 层。

### 6.2 StarryOS
当前仓库里没有看到 StarryOS 直接依赖 `ax-dma` 的证据，因此不应把它描述为 StarryOS 常规 DMA 子系统。

### 6.3 Axvisor
Axvisor 在本仓库中有自己的块设备 DMA 实现，不直接依赖 `ax-dma`。因此它不是 Axvisor 的统一 DMA 中枢。
