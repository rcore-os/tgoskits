# `range-alloc-arceos` 技术文档

> 路径：`components/range-alloc-arceos`
> 类型：库 crate
> 分层：组件层 / 区间分配算法组件
> 版本：`0.1.4`
> 文档依据：当前仓库源码、`Cargo.toml`、`README.md`、`src/lib.rs`、`tests/test.rs`、`components/axdevice/src/device.rs`、`components/axvm/src/vm.rs`、`os/axvisor/src/vmm/hvc.rs`

`range-alloc-arceos` 的真实定位是一个**泛型连续区间分配器**。它管理的是一个初始范围上的“哪些子区间空闲、哪些子区间已占用”，并提供 best-fit 分配与回收合并逻辑。它不是页分配器，不处理地址映射，不理解设备，不负责对齐策略，更不是完整的内存管理子系统。

## 1. 架构设计分析

### 1.1 设计定位

这个 crate 的问题模型非常明确：

- 有一个初始区间 `start..end`
- 需要不断从中分配连续子区间
- 需要在释放时把相邻空闲区间重新合并
- 希望尽量降低碎片化

因此它选择的核心数据结构不是树或位图，而是：

- 一个有序的 `Vec<Range<T>> free_ranges`

并把已分配区间看作“初始区间减去空闲区间”的补集。

### 1.2 核心数据结构

`RangeAllocator<T>` 内部只有两项状态：

| 字段 | 作用 |
| --- | --- |
| `initial_range` | 整个分配器负责管理的总范围 |
| `free_ranges` | 当前空闲区间表，按起始地址升序排列且互不重叠 |

这两个不变量非常关键：

- `free_ranges` 必须有序
- `free_ranges` 之间不能重叠

几乎所有接口都围绕维护这两个不变量展开。

### 1.3 分配策略：best-fit

`allocate_range(length)` 的实现会遍历所有空闲区间，并选择：

- 能满足请求的最小空闲区间

这是一种典型的 best-fit 策略。它的好处是：

- 优先消耗最贴合请求的空闲块
- 尽量保留大块连续空间

若找不到足够大的单个区间，则返回：

- `RangeAllocationError { fragmented_free_length }`

其中 `fragmented_free_length` 表示把所有空闲区间长度加起来后，总共还有多少空闲量。这个字段能帮助调用者区分：

- 是真的没空间了
- 还是只是碎片太多，凑不出连续区间

### 1.4 回收策略：邻接合并

`free_range(range)` 的逻辑重点不在简单插入，而在合并：

- 如果与左邻接，则与左合并
- 如果与右邻接，则与右合并
- 如果左右都邻接，则把三段合成一段
- 否则在有序位置插入

源码中还有断言确保：

- 释放区间必须位于 `initial_range` 内
- 区间必须非空
- 插入后不会与现有空闲区间重叠

这意味着：

- 双重释放或越界释放不是被“静默忽略”，而是会触发断言

### 1.5 扩容与观察接口

除了分配/回收，这个 crate 还提供了几个非常实用的辅助接口：

- `grow_to(new_end)`：扩展管理上界
- `allocated_ranges()`：通过空闲表反推出当前已分配区间
- `reset()`：恢复到初始全空闲状态
- `is_empty()`：检查是否尚未分配任何区间
- `total_available()`：统计所有空闲区间的总长度

尤其是 `allocated_ranges()`，说明这个分配器不仅能“给空间”，也适合做状态观察和调试。

### 1.6 与 Axvisor 当前实现的真实关系

当前仓库里的真实调用链非常清晰：

1. `axdevice::AxVmDevices` 在 `IVCChannel` 设备配置初始化时，创建 `RangeAllocator<usize>`
2. 这个区间的范围来自 IVC 共享内存的 guest physical address 空间
3. `axvm::VM::alloc_ivc_channel()` 先把请求大小向上对齐到 4K
4. 然后调用 `devices.alloc_ivc_channel()`，最终落到 `RangeAllocator::allocate_range()`
5. `os/axvisor/src/vmm/hvc.rs` 在处理 IVC hypercall 时使用这套分配/回收能力

这条链路很重要，因为它清楚地说明了：

- 对齐是 `axvm` 做的，不是本 crate 做的
- 地址映射是 `axdevice` / `axvm` / `axvisor` 做的，不是本 crate 做的
- 本 crate 只是区间分配算法核心

## 2. 核心功能说明

### 2.1 主要能力

- 用初始区间构造分配器
- 按 best-fit 分配连续区间
- 回收区间并自动合并相邻空闲块
- 动态扩展管理范围上界
- 枚举已分配区间
- 查询空闲总量与是否为空

### 2.2 当前仓库中的典型调用链

真实调用链可以概括为：

`range-alloc-arceos` -> `axdevice::AxVmDevices` -> `axvm::VM::{alloc_ivc_channel, release_ivc_channel}` -> `os/axvisor::vmm::hvc`

也就是说，它当前主要服务于：

- Axvisor 的 IVC 共享内存 GPA 区间管理

### 2.3 最关键的边界澄清

`range-alloc-arceos` 不负责：

- 地址对齐
- 物理页分配
- 页表映射
- 内存清零
- 共享内存元数据管理

它只是一个**泛型区间分配算法组件**。

## 3. 依赖关系图谱

### 3.1 直接依赖

该 crate 没有额外三方依赖，主要依靠：

- `alloc::vec::Vec`
- `core::ops::Range`

完成实现。

### 3.2 主要消费者

当前仓库内可确认的直接消费者：

- `axdevice`

明确可见的间接链路：

- `range-alloc-arceos` -> `axdevice` -> `axvm` -> `os/axvisor`

### 3.3 关系解读

| 层级 | 角色 |
| --- | --- |
| `range-alloc-arceos` | 连续区间分配算法 |
| `axdevice` | 为 VM 设备层提供 IVC GPA 区间池 |
| `axvm` | 负责对齐请求并包装返回值 |
| `os/axvisor` | 在 HyperCall 处理中真正消费分配结果 |

## 4. 开发指南

### 4.1 适合什么时候使用

适合使用该 crate 的场景是：

- 资源天然可以表达为一个线性区间
- 分配单位是“连续范围”而不是单点
- 希望能观察碎片化情况
- 对齐策略可以由上层单独处理

不适合直接把它当成：

- 伙伴系统
- 物理页分配器
- 完整地址空间管理器

### 4.2 维护时的关键注意事项

- `free_ranges` 的有序、不重叠不变量必须始终保持
- `free_range()` 的断言意味着重叠回收会直接失败，不是容错逻辑
- 若要新增“带对齐分配”接口，应明确这是否属于本 crate 的职责扩张
- `T` 的 trait bound 是经过精简设计的，不要轻易扩大或改变数值语义

### 4.3 与上层系统的职责分工

- 大小 4K 对齐：`axvm`
- GPA 空间来源：`axdevice`
- IVC 映射与共享内存语义：`axvisor`
- 连续区间 best-fit 分配/合并：本 crate

这条分工线非常清楚，文档里不能把它写成“IVC 管理模块”。

## 5. 测试策略

### 5.1 当前覆盖情况

当前测试已经相对充分：

- `src/lib.rs` 中有较多单元测试
- `tests/test.rs` 还有额外集成测试

现有测试覆盖了：

- 基本分配/释放
- 空间耗尽
- `grow_to()`
- 中间空洞
- best-fit 选择
- 邻接合并
- 碎片化行为

### 5.2 建议补充的测试

- double free / 重叠 free 的 `should_panic` 测试
- 与 `axdevice` / `axvm` 结合的 4K 对齐集成测试
- 更大整数类型或边界类型参数测试

### 5.3 风险点

- 如果文档忽略“对齐由上层完成”，调用者很容易误用
- `free_range()` 的断言语义需要明确，否则回收错误会在运行时直接崩溃
- best-fit 不是唯一策略，今后若替换策略需要同步评估上层碎片行为

## 6. 跨项目定位分析

| 项目 | 位置 | 角色 | 说明 |
| --- | --- | --- | --- |
| ArceOS | 当前仓库未见直接接线 | 通用区间算法组件 | 目前不在 ArceOS 主线路径上 |
| StarryOS | 当前仓库未见直接接线 | 通用区间算法组件 | 尚未看到直接使用 |
| Axvisor | `axdevice` / `axvm` / `hvc` IVC 链路 | IVC GPA 连续区间分配算法 | 当前最明确、最真实的使用场景 |

## 7. 总结

`range-alloc-arceos` 的价值在于把“连续区间 best-fit 分配 + 合并回收”这件事做成了一个独立、泛型、`no_std` 友好的算法组件。当前仓库里它主要服务于 Axvisor 的 IVC GPA 区间管理，但它本身并不理解 IVC、设备或内存映射。理解它时最重要的边界，就是把它看成**区间分配算法**，而不是更高层的资源子系统。
