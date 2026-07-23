---
sidebar_position: 3
sidebar_label: "运行时分配器"
---

# 运行时页与堆分配器

`memory/ax-alloc` 是运行期物理页、内核 byte allocation 和 Rust `GlobalAlloc` 的公共入口。它固定使用 `buddy-slab-allocator`：Buddy 管理多个物理内存 section，per-CPU Slab 服务小对象，显式页 API 通过 zone、usage 和 RAII owner 表达约束。

## 1. 初始化与内存布局

运行时 allocator 只能接收已经从固件 RAM 中扣除 KImage、reserved、MMIO 和 early allocations 的 `Free` 区间。所有区间在交接后由 allocator 独占，调用方不得继续直接使用其中的字节。

### 1.1 多 section 初始化

`os/arceos/modules/axruntime/src/lib.rs::init_allocator()` 找到最大的 free region 调用 `ax_alloc::global_init()`，其余 free region 逐个调用 `global_add_memory()`。两个入口最终分别调用 `buddy_slab_allocator::GlobalAllocator::init()` 和 `add_region()`。

| 入口 | 使用场景 | 失败条件 |
| --- | --- | --- |
| `global_init(start_vaddr, size)` | 建立第一个 Buddy section | 已初始化、范围溢出、metadata/layout 无效 |
| `global_add_memory(start_vaddr, size)` | 增加后续不连续 section | 未初始化、重叠、范围溢出 |
| `init_percpu_slab(cpu_id)` | CPU bring-up 时初始化本地 Slab | CPU id 超过 `u16` 或重复初始化 |

`add_region()` 对不足以容纳 metadata 和 2 MiB heap 对齐的短 region 会记录日志并跳过。平台验收不能只统计输入 free bytes，还应比较实际 `managed_bytes()`。

### 1.2 Section metadata

每个 region 的前缀存放 `BuddySection` 和与页数相关的 `PageMeta[]`，随后将可管理 heap 起点按 `REGION_GRANULE = 2 MiB` 对齐。metadata 和对齐 padding 不再作为可分配页返回。

```mermaid
flowchart LR
    Start["region start"] --> Section["BuddySection"]
    Section --> Meta["PageMeta array"]
    Meta --> Pad["alignment padding"]
    Pad --> Heap["managed heap\n2 MiB aligned"]
    Heap --> End["region end"]
```

多个 region 形成多个独立 section。Buddy 可以在分配时扫描 section，但一个连续 allocation 不会跨越 section 或物理 hole。

## 2. Byte allocation

普通 Rust 容器和内核对象通过 `GlobalAlloc` 进入 `ax_alloc::GlobalAllocator::alloc(Layout)`。实现依据 size 和 alignment 选择 Slab 或 Buddy，不暴露可切换的 allocator backend feature。

### 2.1 Slab 热路径

满足 `size <= 2048` 且 `align <= 2048` 的 allocation 进入 per-CPU Slab。`SizeClass` 使用固定九档，避免运行期生成动态 class 或复杂 size tree。

| Size class | 对象大小 | Slab backing 规模 |
| --- | --- | --- |
| `Bytes8` 至 `Bytes256` | 8、16、32、64、128、256 B | 每个新 Slab 1 页 |
| `Bytes512`、`Bytes1024` | 512 B、1024 B | 每个新 Slab 2 页 |
| `Bytes2048` | 2048 B | 当前公式最多 4 页 |

当本 CPU 对应 class 没有对象时，Slab 返回 `NeedsSlab`，全局实现从 Buddy 申请 backing pages、标记 `PageFlags::Slab`，再交给本 CPU class。空 Slab 可以将 backing pages 返回 Buddy。

### 2.2 大对象与跨 CPU释放

超过 Slab 上限的 byte allocation 被向上取整为 4 KiB 页数，由 Buddy 直接完成。它仍以请求的 `Layout` 通过 `GlobalAlloc` 对称释放，不应和显式页 API 混用。

| 路径 | 锁与并发行为 | 统计分类 |
| --- | --- | --- |
| 本 CPU Slab alloc/free | CPU-local `SpinNoIrq<SlabAllocator>` | `Normal × RustHeap` |
| Slab 扩容/归还 | 短时进入全局 Buddy 锁 | `Normal × RustHeap` 的请求字节数 |
| 跨 CPU Slab free | CAS 压入 owner slab page 的 remote-free stack | 释放仍归原 byte allocation |
| 大对象 alloc/free | 全局 Buddy 锁 | `Normal × RustHeap` |

Remote free 不是单独 allocator 或固定容量 manager。释放对象自身保存链表节点，owner CPU 在后续分配或回收 Slab 时通过 Acquire/Release 操作 drain 该栈。

## 3. 显式页 API

页表、用户 VM、page cache、DMA 和其他需要页粒度所有权的代码使用 `PageRequest` 与 `UsageKind`。公共 RAII 入口是 `alloc_pages()`，少数实现层可使用隐藏的 raw 对称 API。

### 3.1 请求模型

请求包含连续页数、字节对齐和物理地址约束。当前 base page 固定为 4 KiB，`count` 必须非零，乘法和地址范围都执行 overflow 检查。

```rust
pub struct PageRequest {
    pub count: usize,
    pub align: usize,
    pub zone: MemoryZone,
}

pub fn alloc_pages(
    request: PageRequest,
    usage: UsageKind,
) -> AllocResult<GlobalPage>;
```

`MemoryZone` 只表达物理可达性，`UsageKind` 只表达用途统计。两者不得组合成大量 page class，也不改变分配失败时立即返回 `NoMemory` 的规则。

### 3.2 Normal 与 Dma32

`Normal` 调用 Buddy 的普通 `alloc_pages()`；`Dma32` 调用 `alloc_pages_lowmem()`，只接受物理地址完全位于 4 GiB 以下的结果。当前两者扫描同一组 Buddy section。

| Zone | 地址约束 | 是否独立保留池 | 典型消费者 |
| --- | --- | --- | --- |
| `Normal` | allocator 可管理的任意物理地址 | 否 | 页表、用户页、Guest RAM、内核大对象 |
| `Dma32` | allocation 末地址不超过 32-bit DMA window | 否 | `dma_mask <= u32::MAX` 的设备 |

因为 `Normal` 也能消费低于 4 GiB 的页，Dma32 不是 Linux 式永久 DMA zone reserve。低地址紧张的平台应在启动期规划容量或预分配关键 DMA ring，而不是假设后期请求必然成功。

### 3.3 GlobalPage 所有权

`GlobalPage` 保存 `start_vaddr`、原始 `PageRequest` 和 `UsageKind`。它不实现复制，Drop 根据原 zone 和 usage 返回 Buddy。

| 方法 | 行为 | 所有权影响 |
| --- | --- | --- |
| `GlobalPage::alloc()` | 分配一个 Normal 4 KiB 页 | 返回 live RAII owner |
| `GlobalPage::alloc_zero()` | 分配并清零一个页 | 返回 live RAII owner |
| `GlobalPage::alloc_contiguous()` | 分配 Normal 连续页 | 返回同一 owner |
| `as_slice()` / `as_slice_mut()` | 借用完整 allocation | 不转移所有权 |
| `Drop::drop()` | 按 zone 和 usage 归还 | owner 生命周期结束 |

需要把页交给 PTE 或外部对象长期持有的代码必须明确转移或封装生命周期。不能丢弃 `GlobalPage` 后继续使用其地址，否则 Drop 已经把页返回 allocator。

## 4. 统计与失败语义

统计和错误都集中在 `ax-alloc`，消费者不应维护第二份 allocator usage truth。procfs、sysinfo 或诊断接口应从快照派生展示值。

### 4.1 单一统计矩阵

`AllocatorStats` 是 `AllocationSource × UsageKind` 的二维字节计数表。每次成功 allocation 只增加一个 bucket，释放只减少原 bucket。

| 维度 | 当前枚举 | 含义 |
| --- | --- | --- |
| `AllocationSource` | `Normal`、`Dma32` | 请求由哪种物理地址约束满足 |
| `UsageKind` | `RustHeap`、`VirtMem`、`PageCache`、`PageTable`、`Dma`、`Global` | allocation 的逻辑用途 |
| backend occupancy | `used_bytes()` / `available_bytes()` | Buddy 页级占用，不等于请求 layout 精确和 |

每个底层 bucket 使用一个 Relaxed 原子计数；一次分配只写对应 bucket，不再用统计全局锁串行化 per-CPU Slab 命中。`stats()` 读取这些 bucket 生成值快照，`source()`、`usage()` 和 `total()` 都从快照聚合。展示代码不应在读取后反向修改 allocator 状态，也不应把 Dma32 计数误解为静态分区大小。

### 4.2 立即失败

`AllocError` 区分参数、初始化状态、重叠、无内存和错误释放。allocator 内部没有 reclaim callback、VFS callback、阻塞等待或隐藏重试。

| 错误 | 触发示例 | 上层处理 |
| --- | --- | --- |
| `InvalidParam` | `count == 0`、乘法溢出、region range 无效 | 修正调用或返回 `EINVAL` 类错误 |
| `NotInitialized` / `AlreadyInitialized` | 启动顺序错误 | 作为系统状态错误处理 |
| `MemoryOverlap` | 重复交接同一物理区 | 启动失败并检查内存图 |
| `NoMemory` | 没有满足 size/align/zone 的 section | 由调用方决定返回、回收或终止操作 |

Starry 的 clean-page reclaim 位于 fault 外层：只有操作返回 `AxError::NoMemory` 时回收一次，并最多重新执行一次。这个策略不会进入 `ax-alloc` 锁内。

## 5. 实时与 CPU bring-up

嵌入式实时约束通过预分配、具体路径审计和构建配置实现，而不是在 allocator 中加入复杂优先级或可睡眠回收。当前没有公共 RT guard，文档只对已审计并接入固定资源的路径作确定性承诺。

### 5.1 Per-CPU Slab 初始化顺序

BSP 和每个 AP 都必须在本 CPU per-CPU storage 可用之后、scheduler/IPI/IRQ 可能分配之前调用 `init_percpu_slab(cpu_id)`。未初始化时访问本地 Slab 会触发明确失败，而不是回退到不安全的共享路径。

| CPU 阶段 | 必须完成的动作 | 此后允许 |
| --- | --- | --- |
| someboot BSP | 预分配全部 CPU metadata/stack/data | 建立 per-CPU 地址 |
| ax-runtime BSP | 初始化全局 Buddy，再初始化 CPU0 Slab | 启动 scheduler/driver |
| ax-runtime AP | 绑定本 CPU per-CPU data，初始化本地 Slab | 开启 IRQ、进入 scheduler |

Slab backing 页仍来自共享 Buddy；因此“per-CPU Slab”降低小对象热路径争用，但不意味着首次扩容在 IRQ 或 RT critical 中安全。

### 5.2 IRQ 与硬实时路径

IRQ 和硬实时路径必须由具体消费者在启动或 probe 阶段预分配 ring、descriptor 或固定对象池。当前没有通用的 RT guard 或 EmergencyReserve 公共接口；只有出现明确消费者、容量依据和耗尽测试后才增加相应能力，避免为未接线的策略保留公共 API 和静态状态。

## 6. 源码入口

allocator 的公共契约、实现和底层算法分属三个集中位置。修改时应保持公共类型不泄露底层 Buddy/Slab 内部结构。

### 6.1 公共接口文件

下面的文件决定消费者可见行为。API 或 feature 改动必须同步更新本页和组件文档。

| 源码 | 关键内容 |
| --- | --- |
| `memory/ax-alloc/src/lib.rs` | zone、request、usage、source、stats、typed error |
| `memory/ax-alloc/src/page.rs` | `GlobalPage` 和 Drop |
| `memory/ax-alloc/src/buddy_slab.rs` | 公共 wrapper、统计、per-CPU Slab 接线 |

上层 crate 应依赖 `ax-alloc`，不应直接依赖 `buddy-slab-allocator` 的 `BuddyAllocator`、`SlabAllocator` 或 metadata 类型。

### 6.2 底层实现文件

底层 crate 负责 allocator 算法，不负责 OS policy。性能回归通常应先定位是 Buddy、Slab 还是平台内存交接问题。

| 源码 | 关键内容 |
| --- | --- |
| `memory/buddy-slab-allocator/src/global.rs` | Buddy + Slab 选择、section 添加、large alloc |
| `memory/buddy-slab-allocator/src/buddy/mod.rs` | 多 section Buddy、lowmem filter、合并与拆分 |
| `memory/buddy-slab-allocator/src/slab/size_class.rs` | 九个固定 size class |
| `memory/buddy-slab-allocator/src/slab/page.rs` | bitmap、owner CPU、remote-free CAS stack |
| `memory/buddy-slab-allocator/src/slab/cache.rs` | partial/full Slab 和 remote drain |

默认优化顺序是先预分配固定池、减少 RT/IRQ 动态分配并测量 Buddy 锁，再决定是否需要有限的 per-CPU order-0 cache。当前实现没有该 page cache，也不应在没有基准证据时添加。
