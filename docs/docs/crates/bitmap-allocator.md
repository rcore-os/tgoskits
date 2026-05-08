# `bitmap-allocator` 技术文档

> 路径：`components/bitmap-allocator`
> 类型：库 crate
> 分层：组件层 / 位图分配算法组件
> 版本：`0.2.1`
> 文档依据：当前仓库源码、`Cargo.toml`、`README.md`、`src/lib.rs`、`components/axallocator/src/bitmap.rs`、`components/axallocator/Cargo.toml`

`bitmap-allocator` 的真实定位是一个**按位管理资源索引的分配算法组件**。它管理的是“哪些 bit 目前可用”，而不是直接管理页表、物理页或虚拟地址空间。当前仓库里，它被 `ax-allocator` 拿来实现页粒度分配器，但“页分配语义”是上层赋予它的，而不是这个 crate 自己具备的。

## 1. 架构设计分析

### 1.1 设计定位

从源码看，这个 crate 解决的问题可以概括为：

- 用位图表示资源可用性
- 快速找到一个空闲 bit
- 支持按连续区间分配
- 支持按对齐要求寻找可分配区间

因此它本质上是一个“索引分配器”，适用于：

- 页帧编号
- 资源槽位
- ID 区间

但它本身并不知道这些 bit 对应的真实物理含义。

### 1.2 核心抽象 `BitAlloc`

`BitAlloc` trait 定义了统一接口：

- `CAP`
- `DEFAULT`
- `alloc()`
- `alloc_contiguous(...)`
- `next()`
- `dealloc()`
- `dealloc_contiguous()`
- `insert()`
- `remove()`
- `is_empty()`
- `test()`

其中语义很重要：

- bit 为 `1` 表示**可用**
- bit 为 `0` 表示**不可用/已分配**

这一点在阅读上层消费者代码时必须牢牢记住，否则很容易把 `insert/remove` 理解反。

### 1.3 叶子节点 `BitAlloc16`

`BitAlloc16(u16)` 是整个结构的叶子层：

- 容量固定为 16 bit
- `alloc()` 通过 `trailing_zeros()` 找到第一个可用位
- `dealloc()` 把对应 bit 重新置为可用
- `insert()` / `remove()` 通过 `bit_field` 批量设置区间

它既是最小实现，也是上层级联结构的基础 building block。

### 1.4 级联结构 `BitAllocCascade16<T>`

更大的位图由 `BitAllocCascade16<T>` 递归组合而成。它包含：

- 一个 `u16 bitset`
- 一个长度为 16 的子分配器数组 `sub`

这里的 `bitset` 不是完整位图，而是“子树摘要”：

- 第 `i` 位为 1，表示第 `i` 个子分配器里还有可用位
- 第 `i` 位为 0，表示对应子分配器已空

因此 `alloc()` 的流程是：

1. 在摘要位图上找第一个非空子树
2. 进入对应子分配器继续分配
3. 分配后再回写该子树是否仍非空

这是一种非常典型的 16 叉级联位图/分段树思路。

### 1.5 预定义容量类型

源码提供了一组级联 type alias：

- `BitAlloc256`
- `BitAlloc4K`
- `BitAlloc64K`
- `BitAlloc1M`
- `BitAlloc16M`
- `BitAlloc256M`

这些类型只是容量不同，算法模型完全相同。当前仓库里，`ax-allocator` 会根据 feature 选择其中一种：

- `page-alloc-256m` -> `BitAlloc64K`
- `page-alloc-4g` -> `BitAlloc1M`
- `page-alloc-64g` -> `BitAlloc16M`
- `page-alloc-1t` -> `BitAlloc256M`

### 1.6 连续分配策略

`alloc_contiguous()` 并没有使用 buddy 之类的特殊结构，而是借助：

- `find_contiguous()`
- `check_contiguous()`
- `next()`

在逻辑位图空间里寻找满足：

- 大小 `size`
- 对齐 `align_log2`
- 可选固定基址 `base`

的连续区间。

这说明它的连续分配能力是“位图扫描 + 摘要加速”，不是专门的伙伴算法。

### 1.7 与 `ax-allocator` 的真实关系

当前仓库中的真实消费者是 `components/axallocator/src/bitmap.rs`。上层 `BitmapPageAllocator` 会：

- 把“页号”映射成 bit 索引
- 处理地址到 bit 索引的转换
- 处理页大小、1GB 对齐窗口和最大容量限制
- 调用 `alloc()` / `alloc_contiguous()` / `dealloc_contiguous()`

因此：

- `bitmap-allocator` 负责 bit 级算法
- `ax-allocator` 负责把 bit 解释成“页”

## 2. 核心功能说明

### 2.1 主要能力

- 分配单个空闲 bit
- 分配满足对齐要求的连续 bit 区间
- 回收单个 bit 或连续区间
- 批量插入/移除可用区间
- 查询下一个可用 bit
- 构建多种预设容量的级联位图分配器

### 2.2 当前仓库中的典型调用链

真实链路是：

1. `ax_allocator::BitmapPageAllocator` 根据 feature 选定一种 `BitAllocUsed`
2. `init()` 把可用页范围映射成可用 bit 范围并执行 `insert()`
3. 页分配时调用 `alloc()` 或 `alloc_contiguous()`
4. 页释放时调用 `dealloc()` 或 `dealloc_contiguous()`

因此 `bitmap-allocator` 是 `ax-allocator` 里“页级位图算法内核”，但并不等于整个页分配器。

### 2.3 最关键的边界澄清

`bitmap-allocator` 不负责：

- 地址与 bit 索引之间的映射
- 页大小、物理内存边界、地址翻译
- 多种分配器策略切换
- 全局内存管理接口

它只是一个**位图分配算法组件**。

## 3. 依赖关系图谱

### 3.1 直接依赖

| 依赖 | 作用 |
| --- | --- |
| `bit_field` | 读写单个位和位区间 |

### 3.2 主要消费者

当前仓库内可确认的直接消费者：

- `ax-allocator`

明确可见的传递链路：

- `bitmap-allocator` -> `ax-allocator` -> 各依赖通用分配器的系统组件

### 3.3 关系解读

| 层级 | 角色 |
| --- | --- |
| `bitmap-allocator` | bit 级空闲区管理算法 |
| `ax-allocator` | 把 bit 映射为页粒度分配语义 |
| ArceOS/StarryOS/Axvisor 上层组件 | 通过统一分配器接口间接使用 |

## 4. 开发指南

### 4.1 适合怎样使用

适合直接使用这个 crate 的场景是：

- 资源空间可以自然编号成 0..N
- 需要快速单点或连续区间分配
- 资源可用性适合用 bit 表示

如果你的问题是：

- 需要复杂伙伴合并语义
- 需要记录物理地址/NUMA/zone 信息
- 需要并发分配策略

那应在更上层封装，不要把这些语义硬塞到本 crate。

### 4.2 维护时最容易出错的点

- `1` 表示可用、`0` 表示不可用，这一点不能搞反
- `BitAllocCascade16` 修改后要同步维护子树摘要位
- `CAP` 与 `DEFAULT` 在各层级联里必须保持一致
- `insert()` / `remove()` 的区间应视为有效非空范围来使用，零长度区间不是当前实现重点覆盖场景

### 4.3 与上层分页语义的分工

- 地址对齐、页大小、容量窗口：上层 `ax-allocator`
- bit 级空闲搜索与连续分配：本 crate
- 这条边界一旦混淆，就会把文档写成“页分配子系统”，这是不准确的

## 5. 测试策略

### 5.1 当前覆盖情况

`src/lib.rs` 自带较完整单元测试，覆盖了：

- `BitAlloc16`
- `BitAlloc4K`
- 连续分配
- 对齐分配
- 回收与再次分配

这是当前这份 crate 最强的可信依据之一。

### 5.2 建议补充的测试

- 大容量 type alias 的边界测试
- 非法或极端 `align_log2` 输入测试
- `insert/remove` 边界区间测试
- 与 `ax-allocator` 的组合测试，验证地址与 bit 索引映射无偏差

### 5.3 风险点

- 容量和 bit 语义一旦理解错误，上层内存管理会整体错位
- 连续分配不是 buddy 语义，不能假设其碎片表现与伙伴系统相同
- 文档若把它写成“内存分配器”会掩盖它只是底层算法部件这一事实

## 6. 跨项目定位分析

| 项目 | 位置 | 角色 | 说明 |
| --- | --- | --- | --- |
| ArceOS | `ax-allocator` 页分配路径 | 位图算法内核 | 通过统一分配器间接支撑内存分配 |
| StarryOS | 共享分配器基础设施 | 位图算法内核 | 若复用 `ax-allocator` 位图路径则间接使用 |
| Axvisor | 共享分配器基础设施 | 位图算法内核 | 若选择同一页分配实现则会被间接带入 |

## 7. 总结

`bitmap-allocator` 是一块职责非常纯粹的算法积木：它只负责在位图空间里找空位、找连续空位、回收空位。页、地址、物理内存和系统分配策略都来自它的调用者。理解它时最重要的边界，就是把它看成**位图分配算法组件**，而不是完整的内存子系统。
