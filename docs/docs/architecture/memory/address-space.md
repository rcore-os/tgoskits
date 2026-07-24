---
sidebar_position: 8
sidebar_label: "地址空间"
---

# 虚拟内存区域与地址空间

`ax-memory-set` 提供与操作系统策略无关的 Virtual Memory Area（虚拟内存区域）集合。它保存连续虚拟范围、权限和 backend，不拥有物理页分配器，也不实现 Linux 系统调用级事务。

ArceOS、StarryOS 和 Axvisor 分别在 `ax-mm`、StarryOS kernel 与 `axaddrspace` 中实现自己的物理页、文件映射、写时复制和客户机内存策略。

## 1. 组件边界

```text
                         ax-memory-set
                MemoryArea + MemorySet + MappingBackend
                    /                |                 \
                   /                 |                  \
              ax-mm        StarryOS kernel/aspace     axaddrspace
          内核虚拟地址       Linux 进程虚拟地址       客户机物理地址
                 \                  |                  /
                  \                 |                 /
                       各自页表实现与页帧所有权
```

| 组件 | 保存的状态 | 不承担的职责 |
| --- | --- | --- |
| `ax-memory-set` | 有序虚拟内存区域、实际权限、报告权限、backend | 物理页分配、回收、跨核页表刷新、Linux 记账 |
| `ax-mm` | ArceOS 内核页表、线性映射和按需分配映射 | 文件虚拟内存、写时复制、客户机第二阶段策略 |
| StarryOS `AddrSpace` | 进程页表、常驻页统计、commit accounting、Linux 虚拟内存区域策略 | 通用 allocator 和架构页表项编码 |
| `axaddrspace` | 客户机物理地址范围、线性或分配型客户机 RAM backend | Linux 虚拟内存区域、宿主内核 iomap |
| `axcpu::paging` / `axvm` | 所属上下文的页表项、映射、权限修改和失效能力 | 虚拟内存区域策略和物理页回收策略 |

## 2. 数据模型

### 2.1 MemoryArea

源码：`memory/memory_set/src/area.rs`。

`MemoryArea<B>` 描述一个半开区间 `[start, end)`：

```rust
pub struct MemoryArea<B: MappingBackend> {
    va_range: AddrRange<B::Addr>,
    flags: B::Flags,
    reported_flags: B::Flags,
    backend: B,
}
```

| 字段 | 含义 |
| --- | --- |
| `va_range` | 连续虚拟地址范围 |
| `flags` | backend 和页表实际使用的权限 |
| `reported_flags` | StarryOS 等上层向用户报告的权限 |
| `backend` | 线性映射、分配映射、写时复制、文件映射等策略 |

实际权限和报告权限分离是为了支持写时复制。父子页表项可以暂时移除写权限，而 `/proc` 和 Linux 虚拟内存语义仍报告原始可写属性。

`split(pos)` 只在 `start < pos < end` 时成功。它同时调用 backend 的 `split(align_diff)`，因此范围元数据和 backend 内部偏移不会分离。

### 2.2 MemorySet

源码：`memory/memory_set/src/set.rs`。

```rust
pub struct MemorySet<B: MappingBackend> {
    areas: BTreeMap<B::Addr, MemoryArea<B>>,
}
```

键是虚拟内存区域起始地址。核心复杂度如下：

| 操作 | 复杂度 | 说明 |
| --- | --- | --- |
| `find(addr)` | O(log n) | 查找最后一个不大于地址的起点，再检查 containment |
| `overlaps(range)` | O(log n) | 只检查前驱和第一个后继 |
| 插入/删除 | O(log n) | 不移动其他虚拟内存区域 |
| `find_free_area` | O(log n + k) | 从 hint 前驱开始扫描后续 gap |
| 跨区域 unmap/protect | O(k log n) 或 O(n) | k 为受影响区域数量 |

当前实现保留 `BTreeMap`。没有代表性 StarryOS 多虚拟内存区域基准前，不用排序 `Vec` 替换它，也不为了预留树节点引入第二套元数据 allocator。

## 3. MappingBackend

源码：`memory/memory_set/src/backend.rs`。

```rust
pub trait MappingBackend: Clone {
    type Addr: MemoryAddr;
    type Flags: Copy;
    type PageTable;

    fn map(
        &self,
        start: Self::Addr,
        size: usize,
        flags: Self::Flags,
        page_table: &mut Self::PageTable,
    ) -> bool;

    fn unmap(
        &self,
        start: Self::Addr,
        size: usize,
        page_table: &mut Self::PageTable,
    ) -> bool;

    fn protect(
        &self,
        start: Self::Addr,
        size: usize,
        new_flags: Self::Flags,
        page_table: &mut Self::PageTable,
    ) -> bool;

    fn split(&mut self, align_diff: usize) -> Option<Self>;
}
```

该 trait 是直接页表操作边界，不是事务框架。公共层不定义：

- `MappingOperation`；
- `MapPrecondition`；
- `MappingPlan` 或 `CommitState`；
- `prepare/abort/commit/rollback/finalize`；
- 通用逐页 `SavedMapping`。

这样 ArceOS 和 Axvisor 的普通映射不会因为 Linux 系统调用级回滚语义创建动态数组或扫描整个旧映射。

当前 trait 仍使用 `bool` 表示 backend 成败。公共 `MemorySet` 把失败转换为 `MappingError::BadState`。这是保留原接口以控制修改范围的明确限制；需要细分 `NoMemory`、参数错误和页表损坏时，应先证明所有调用方都能稳定处理这些错误，再单独修改接口。

## 4. 映射流程

### 4.1 新建映射

```text
MemorySet::map(area)
  │
  ├─ 检查空范围
  ├─ 检查与已有区域是否重叠
  │    ├─ 不允许覆盖：返回 AlreadyExists
  │    └─ 允许覆盖：先执行 unmap
  ├─ area.map_area()
  │    └─ backend.map()
  └─ backend 成功后插入 BTreeMap
```

不重叠映射不会构造操作计划或撤销日志。backend 必须保证单次 map 中途失败时清理本次已经建立的前缀。

例如 `ax-mm` 的 allocation backend 在 `populate=true` 时逐页分配。任一页帧申请或页表写入失败后，`rollback_alloc_mapping` 只遍历本次已安装页面并释放对应页帧。

### 4.2 解除映射

`unmap(start, size)` 按三类相交方式处理：

```text
原区域完全位于目标范围：调用 unmap_area 后删除
目标切除区域尾部：       shrink_right
目标切除区域头部：       shrink_left
目标位于区域中间：       split + shrink_right
```

`shrink_left` 和 `shrink_right` 先调用 backend unmap，成功后才修改该区域的边界和 backend 偏移。

跨多个虚拟内存区域的直接 unmap 不是公共事务。前面区域已经成功解除后，后续 backend 失败不会由 `ax-memory-set` 建立逐页日志恢复。调用方不得把低层直接接口描述为全成或回滚。

### 4.3 权限修改

`protect_with_reported_flags` 遍历相交区域，并按需要把一个区域拆成左、中、右三部分。中间部分调用 backend `protect`，随后更新实际权限和报告权限。

StarryOS 写时复制 backend 可以把页表实际写权限清除，同时保留对用户报告的可写权限。

### 4.4 metadata-only 操作

| API | 用途 |
| --- | --- |
| `map_metadata` | 页表项和 owner 已由专用流程建立后发布新区域 |
| `unmap_metadata` | 页表项已移动或分离后只删除区域描述 |
| `replace_area_metadata` | 在已有区域内部替换一段描述而不修改页表 |

StarryOS fork 的写时复制流程先建立 child 页表项、引用计数和常驻页统计，然后调用 `map_metadata`。如果元数据发布失败，StarryOS 专用回滚逻辑撤销 child 页表项和引用；公共组件不重复保存同一份撤销状态。

## 5. 覆盖映射和原子性边界

`MemorySet::replace` 验证新区域位于 replacement range 内，然后直接执行：

```rust
self.unmap(replace_range.start, replace_range.size(), page_table)?;
self.map(area, page_table, false)
```

因此新 map 失败时，已经解除的旧范围不会由公共层自动恢复。该接口只适合：

- 上层已经保存专用恢复信息；
- 失败后允许调用方重建映射；
- 不要求 Linux 系统调用原子性的内部路径。

StarryOS 的 `AddrSpace` 在调用低层操作前负责地址范围、`RLIMIT_AS`、commit delta、文件映射状态和页表移动预检。写时复制 clone 和 mremap 分别维护自己的有限回滚记录，不把通用逐页快照施加给所有地址空间消费者。

普通跨区域 unmap/protect 当前不承诺 all-or-nothing。这一限制必须保留在设计、测试和错误报告中，不能仅通过文档声称已经具备事务保证。

## 6. 12 GiB 映射示例

12 GiB 范围包含：

```text
12 GiB / 4 KiB = 3,145,728 个基础页
```

旧五阶段实现曾在 prepare 阶段为每个页表项保存 `SavedMapping`，即使操作只需要建立线性映射，也可能先消耗数十至上百 MiB 临时堆。

当前直接实现：

```text
MemoryArea 元数据：1 项
通用操作计划：    0 项
通用页表快照：    0 项
backend 额外状态：常数
页表建立时间：    由实际页表映射粒度决定
```

大范围映射不再因为通用事务快照在真正写页表前返回 `NoMemory`。如果架构和 backend 支持大页，页表层可以选择大页；`ax-memory-set` 不展开或复制页表项。

## 7. 三类消费者

### 7.1 ArceOS ax-mm

`os/arceos/modules/axmm/src/backend/` 提供：

- `Linear`：虚拟地址按固定差值换算为物理地址；
- `Alloc`：立即填充或缺页时分配物理页。

页帧使用 `MemoryZone::Normal` 和 `UsageKind::VirtMem`。释放时传回原页数和用途，不需要保存 zone。

### 7.2 StarryOS

`os/StarryOS/kernel/src/mm/aspace/` 保留 Linux 专属状态：

- 写时复制；
- anonymous、file、shared mapping；
- 常驻内存集大小和虚拟内存大小；
- commit accounting；
- mremap、fork 和缺页恢复；
- signal/errno 转换。

`AddressSpacePageTable` 把具体页表和 `MemoryAccounting` 交给同一次 backend 调用。这个结构体维护“页表变化必须同步更新常驻页统计”的 StarryOS 不变量，不是通用事务 plan。

### 7.3 Axvisor axaddrspace

`virtualization/axaddrspace/src/address_space/backend/` 使用 `GuestPhysAddr` 和 `NestedPageTableOps`。Linear backend 映射外部客户机 RAM，Alloc backend 按需取得宿主页帧。

客户机生命周期、第二阶段页表失效和设备 DMA 停止顺序由 AxVM 管理，不进入 `ax-memory-set`。

## 8. 锁与并发

`MemorySet` 自身不包含锁。所有修改要求调用方持有地址空间锁：

| 消费者 | 外层同步 |
| --- | --- |
| ArceOS kernel address space | `kernel_aspace()` 外层锁 |
| StarryOS process address space | `Arc<Mutex<AddrSpace>>` |
| Axvisor guest address space | VM/地址空间所有者的外层锁 |

锁内可能执行页表操作和页帧分配，因此不能从硬中断上下文调用，也不能在持锁期间执行虚拟文件系统回调或不可控回收。

跨 CPU Translation Lookaside Buffer（地址转换后备缓冲区）失效由页表和操作系统层协调，不由 `MemorySet` 发起。AArch64 硬件广播和其他架构的处理器间中断路径见[多架构内存实现](./architecture-support.md)。

## 9. 源码索引

| 文件 | 内容 |
| --- | --- |
| `memory/memory_set/src/area.rs` | 虚拟内存区域、权限和 split/shrink/grow |
| `memory/memory_set/src/set.rs` | `BTreeMap` 索引和 map/unmap/protect |
| `memory/memory_set/src/backend.rs` | 直接 backend 能力边界 |
| `os/arceos/modules/axmm/src/backend/` | ArceOS Linear/Alloc backend |
| `os/StarryOS/kernel/src/mm/aspace/` | StarryOS Linux 虚拟内存策略和专用恢复 |
| `virtualization/axaddrspace/src/address_space/` | 客户机地址空间策略 |

测试和验收命令统一见[内存管理测试与验收](./testing.md)。
