# TGOSKits 底层内存架构与实施方案

> 状态：设计草案
>
> 基线：`dev`（`edd793e51`）与当前启动内存收敛 PR #1978
>
> 更新时间：2026-08-12

## 1. 结论

TGOSKits 不应在公共底层实现一套接近 Linux 的统一内存管理器。ArceOS、Axvisor 和 StarryOS 对内存的需求不同，合理的共同底座只有三类能力：

1. `someboot` 在启动阶段发现、规范化和预留物理内存，完成后发布只读事实；
2. `memory/` 提供架构无关、OS 无关且已有多个消费者的运行期机制；
3. ArceOS、Axvisor 和 StarryOS 分别拥有自己的地址空间与内存策略。

这里的“多个消费者”按独立系统/子系统语义判断，而不是按 Cargo 依赖计数判断。`buddy-slab-allocator` 作为 `ax-alloc` 的私有算法边界、`dma-api`/`mmio-api` 作为多驱动 capability 边界，虽然形态不同，也都满足明确的复用职责。

目标架构如下：

```text
Firmware/FDT/UEFI
        |
        v
someboot
  boot memory map + early bump + boot page-table frames
        |
        | freeze: usable RAM / reserved memory / MMIO
        v
somehal -> axplat-dyn -> ax-plat::MemIf -> axhal/axruntime
                                      |
                                      v
                            ax-alloc + shared mechanisms
                                      |
                +---------------------+---------------------+
                |                     |                     |
             ArceOS                Axvisor               StarryOS
       simple heap/pages       guest-memory policy   Linux-compatible policy
                              in axaddrspace/AxVM     in Starry kernel/mm
```

本方案保留现有的固定容量启动内存图、Buddy-Slab、`page-table-generic` 和 `ax-memory-set`，不新增公共 `BootArena`、统一 page owner、全局 reclaim、通用事务引擎或通用 TLB batch。只有代码中已经存在且会导致错误、panic、跨层依赖或错误统计的问题进入主线实施计划。

## 2. 产品边界

### 2.1 三种运行形态

| 系统 | 主要需求 | 不应由公共底层承担的能力 |
| --- | --- | --- |
| ArceOS / Unikernel | 启动后建立内核堆和页分配器；可选简单用户地址空间；小镜像和可预测路径 | 进程级 overcommit、全局回收、swap、复杂 VMA 策略 |
| Axvisor | host 页/堆分配；客户机物理地址空间；Stage-2 映射和设备直通 | Linux 进程 VM、page cache reclaim、host 侧通用 OOM 策略 |
| StarryOS | Linux ABI 地址空间、匿名页和文件页、COW、fault、资源限制、可选回收 | 将 Linux 语义反向塞入 `ax-alloc`、`ax-memory-set` 或 ArceOS/Axvisor 默认构建 |

StarryOS 可以在公共机制之上继续扩展，但它的复杂度不能成为其他两个系统的基础成本。上层策略只有出现第二个具有相同语义的系统消费者后，才重新评估是否下沉。

### 2.2 成功标准

- 启动内存只有 `someboot` 一个可变所有者；进入 runtime 前冻结，之后只读；
- `memory/` 中不存在 StarryOS、VFS、任务调度或具体架构寄存器依赖；
- `ax-alloc` 的一次调用只执行请求的分配或释放，不隐式进入回收策略；
- 平台交接的可用 RAM、保留内存和 MMIO 语义唯一、排序、无重叠，并由 `usable_ram_size()` 明确汇总可分配 RAM；
- 共享地址空间代码不吞掉 backend 错误、不在可恢复错误上 panic，也不谎报成功；
- ArceOS 和 Axvisor 的最小配置不携带 StarryOS 内存策略；
- 不增加运行期堆分配、后台线程、全局 pool manager 或新的常驻锁。

### 2.3 非目标

公共底层不实现以下能力：

- NUMA、memory hotplug、swap、compaction、多代回收、KSM 和通用 OOM killer；
- Linux 式 zone hierarchy、watermark、LRU、overcommit 和进程 RSS 策略；
- 统一 Stage-1/Stage-2/boot 页表格式或统一 TLB shootdown 策略；
- 为未来假设创建 allocator backend、builder、provider trait 或公共 owner 层；
- 因目录名或 `no_std` 属性搬迁只有一个消费者的代码；
- 在本轮架构收敛中重做 DMA descriptor 表示、IOMMU 或驱动队列协议。

保持现状也不是可接受方案：当前仍有平台内存语义不一致、早期地址运算溢出、allocator 反向回调 Starry 策略、可达的 `unimplemented!()`，以及 `ax-memory-set` 吞错或 panic 的确定问题。

## 3. 当前实现分析

### 3.1 启动内存

当前 PR 已将启动内存描述和区间规范化从 `kernutil`、`ranges-ext` 收敛到 `someboot::mem`：

- `BootMemoryMap` 使用固定容量 `heapless::Vec<_, 512>`；
- BSP 在内核、早期 RAM、MMIO 和 CPU-local 预留完成后调用 `freeze()`；
- `Release` 发布与 `Acquire` 读取保证 runtime 只观察完整的只读内存图；
- `somehal` 只重导出冻结后的视图；
- 区间插入已经检查冲突、容量和 descriptor 末端溢出。

这个方向符合启动阶段需求，不需要再公开 `BootArena` 或 `BootMemoryHandoff`。现有交接链已经是 handoff：

```text
someboot::mem::memory_map()
    -> somehal::mem
    -> axplat-dyn::MemIfImpl
    -> ax-plat::mem::MemIf
    -> axhal::mem::memory_regions()
    -> axruntime::init_allocator()
```

仍需修正的代码事实：

- `ram::alloc` 的 `align_up` 和 `start + size` 未使用 checked arithmetic；
- `ram::used_range` 的页对齐也可能溢出；
- early RAM 选择依赖固定的 `> 8 MiB` 阈值，而不是已知启动对象下界和架构地址限制；
- UEFI `page_count * page_size` 仍可能溢出；
- `MemoryDescriptor::new_aligned` 等构造器仍有未检查的加法/对齐；
- `axplat-dyn` 使用 32/32/16 三个独立容量，并通过 `unwrap()` 处理输入；`axhal` 又使用容量 128 重新归一化。合法的 512 项 boot map 可能在 handoff 中 panic；
- 当前 `MemIf::phys_ram_ranges()` 文档定义为“全部 RAM”，动态实现却只返回 `MemoryType::Free`；`total_ram_size()` 因而实际统计 runtime 可用 RAM，而不是接口声明的全部物理 RAM；
- `reserved_phys_ram_ranges()` 的文档要求排除 kernel image，动态实现却包含 `KImage`。这会让统计、映射和新平台 adapter 对同一个接口作出不同解释。

### 3.2 运行期 allocator

`ax-alloc` 是 ArceOS、Axvisor host 和 StarryOS 共用的运行期分配边界；`buddy-slab-allocator` 是其实现依赖。当前 Buddy-Slab 已经支持 `add_region()` 和不连续内存区，相关底层测试也已覆盖新增 region 后的页分配。因此不需要再设计 multi-region allocator。

当前真正的问题是：

- `alloc_pages()` 和 `alloc_dma32_pages()` 失败后会调用全局 `PageReclaimFn`，最多重试四次；
- 只有 StarryOS 注册 `ax_fs_ng::vfs::page_cache_reclaim`，但回调位于所有系统共享的 allocator 内；
- 任意页分配调用方，包括页表、DMA、任务栈或客户机 host 页，都可能在未知锁和上下文中进入 VFS 回收；
- ArceOS/Axvisor 没有注册回调时，失败路径仍会做一次没有收益的重新分配；
- `alloc_pages_at()` 在 Buddy-Slab 和 TLSF backend 中是公开可达的 `unimplemented!()`；TLSF 的 DMA32 路径也会 panic；
- `AllocatorOps` 目前仅由 crate 内 backend 实现，workspace 没有泛型或 trait-object 消费者；但 TLSF 仍是 `axklib` 暴露的公开 feature，不能在没有下游兼容性结论时顺手删除；
- `axruntime::init_allocator()` 的注释仍称新增 region 只进入 byte allocator，与当前 Buddy-Slab 实现不符。

公共 allocator 应维持简单接口：页数、对齐、用途和必要的 DMA32 限制已经足够。现在没有证据表明引入 `PageRequest`、`MemoryZone`、强制 RAII page owner 或统一统计对象能解决新的真实问题；这类公共 API 还会造成大范围迁移和版本破坏，因此不纳入本方案。

### 3.3 页表与地址空间机制

`page-table-generic` 已由 someboot、axcpu/axhal 和 AxVM 共享，通用层负责 walk/map/unmap 等执行逻辑，具体 PTE、页表层级、地址规范化和 TLB 指令由实现 `TableMeta`/`PageTableEntry` 的架构组件提供。当前依赖方向正确。

`TableMeta::flush()` 虽然是回调，但它是由架构实现提供的必要 capability，不是 OS 策略反向依赖。当前没有故障、镜像膨胀或性能数据证明必须改成固定容量 invalidation batch，因此保留现状。

`ax-memory-set` 被 `ax-mm`、StarryOS 和 `axaddrspace` 共同使用，保留在 `memory/` 合理。不过现有错误路径需要修正：

- `MappingBackend::{map, unmap, protect}` 用 `bool` 压平所有错误；
- `MemoryArea::protect_area()` 忽略 backend 返回值并始终返回成功；
- `MemorySet::unmap()` 在删除完整区域时对 backend 结果 `unwrap()`，可恢复失败会 panic；
- `split()` 返回 `Option`，但通用层在多个路径直接 `unwrap()`；
- 多区域操作失败时，接口没有说明已经完成的前缀和剩余元数据状态；
- 覆盖映射先 unmap 再 map，新映射失败时不会恢复旧映射。

这里需要的是“错误真实、状态一致”，不是恢复已经删除过的通用事务框架。历史提交 `efef4f84a` 已把 prepare/commit/rollback 协议替换为直接 backend 操作；重新引入统一 undo log 会再次扩大所有 backend、owner 和测试模型。

公共层采用更小的保证：每一次 backend 原子操作要么成功，要么保持该操作前状态；`MemorySet` 只在该操作成功后更新对应元数据。跨多个区域的调用可以返回“已完成前缀”的错误，但页表与 VMA 必须始终一致。StarryOS 某个 Linux syscall 若要求更强的原子性，由 Starry 在调用前准备资源或在自身策略层回滚。

### 3.4 系统策略

#### ArceOS

ArceOS 使用 `axruntime` 初始化共享页/堆分配器，`ax-mm` 提供内核映射和可选的简单地址空间。它不需要回收线程、进程级 overcommit 或 Linux VMA 策略。

#### Axvisor

Axvisor 的 host 内存来自 ArceOS/`ax-alloc`，客户机地址空间策略位于 `axaddrspace`，Stage-2 架构格式位于 AxVM 架构模块。客户机 RAM 布局、直通窗口和 VMID/TLB 策略不能下沉到通用 allocator。

#### StarryOS

StarryOS 的 COW、file-backed mapping、fault、RSS/commit accounting、resource limit、`/proc` 和 reclaim 都是 Linux 兼容策略，应保留在 `os/StarryOS/kernel/src/mm` 及相关 syscall/VFS 层。只有 Starry 内部拆分后确实改善独立测试和维护时，才考虑 `os/StarryOS/crates/` 下的本地 crate；不能再放入公共 `memory/`。

涉及 `mmap`、`mremap`、`brk`、fork/COW、fault、resource limit 或 `/proc` 语义时，必须单独按 `book/guideline/starry/syscall.md` 对照目标 Linux 行为，不作为公共内存架构 PR 的附带内容。

### 3.5 DMA 与 MMIO

`dma-api` 和 `mmio-api` 有多个驱动消费者，且表达的是架构无关的设备内存能力，因此可以保留在 `memory/`。它们不属于普通 RAM allocator，也不应决定进程或客户机内存策略。

当前 `unsafe impl<T: Copy> DmaPod for T`、可复制的 DMA allocation/map handle，以及 `mmio-api` 的 `static mut` 初始化发布都需要独立 soundness 审核。但这些问题不会因引入 Linux 式内存管理而解决，也不应与启动交接或 allocator 策略 PR 捆绑。

DMA 修正必须作为明确的 breaking-change 设计处理：先列出真实 typed-buffer 调用方和迁移方式，再决定使用 `dma-api` 内部的受限基础类型实现、显式的本地 `unsafe impl`，还是表示层派生。不能只为了消除手写 impl，强制所有使用 `dma-api` 的 crate 直接依赖 `zerocopy`；也不能继续以任意 `Copy` 证明可被设备写入。该工作属于设备 capability 安全线，不进入第 7 节的公共 RAM 主线顺序。

## 4. RTOS 与 Unikernel 对照

本方案的主要参照不是 Linux，而是可裁剪 RTOS、Unikernel 和 capability-oriented kernel：

| 项目 | 相关做法 | TGOSKits 采用的结论 |
| --- | --- | --- |
| FreeRTOS Kernel（官方文档，访问于 2026-08-12） | 提供 heap_1 到 heap_5 等可选实现；heap_1 甚至不释放；多不连续区域由 heap_5 按需提供；一个应用只选择一个实现 | 公共层保持最小分配语义，复杂能力按构建和系统需求选择，不把回收做成所有分配的隐式行为 |
| Zephyr（latest 官方文档，访问于 2026-08-12） | `sys_heap` 只提供低层分配且不内置同步；`k_heap` 在上层增加锁和等待；multi-heap 是显式可选能力 | 机制层不隐藏调度、阻塞或回收；需要等待/策略的系统在上层包装 |
| Apache NuttX（latest 官方文档，访问于 2026-08-12） | 常见 flat build 使用简单 heap；kernel/user heap、多 heap 和 page allocator 随配置启用 | ArceOS/Axvisor 默认保持平坦、简单；Starry 按 Linux 兼容需求叠加策略 |
| Unikraft（官方 boot/concepts 文档，访问于 2026-08-12） | bootloader 传递内存布局，运行时组件和 allocator 可替换组合 | 启动只交接事实；运行时 allocator 与 OS 策略分层，不要求一个全能 manager |
| seL4 tutorials（官方 Untyped 文档，访问于 2026-08-12） | 启动信息描述可用物理资源，root task 决定后续 retype 和资源策略 | handoff 传递经过验证的资源事实，消费方拥有策略；底层不猜测上层用途 |

这些项目的共同点不是某个具体 allocator 算法，而是：最小机制可独立成立，复杂度由需要它的配置显式选择，启动事实与运行期策略分离。TGOSKits 继续使用现有 Buddy-Slab 是项目选择，不需要为了“像 RTOS”改成 FreeRTOS 或 Zephyr 的 allocator。

参考资料：

- [FreeRTOS memory management](https://www.freertos.org/Documentation/02-Kernel/02-Kernel-features/09-Memory-management/01-Memory-management)
- [Zephyr heaps](https://docs.zephyrproject.org/latest/kernel/memory_management/heap.html)
- [NuttX memory management](https://nuttx.apache.org/docs/latest/components/mm/index.html)
- [NuttX memory configurations](https://nuttx.apache.org/docs/latest/implementation/memory_configurations.html)
- [Unikraft booting](https://unikraft.org/docs/internals/booting)
- [Unikraft concepts](https://unikraft.org/docs/concepts)
- [seL4 untyped memory tutorial](https://docs.sel4.systems/Tutorials/untyped.html)

### 4.1 方案比较

| 方案 | 优点 | 主要代价 | 结论 |
| --- | --- | --- | --- |
| 保持当前实现 | 没有迁移成本 | 保留 boot arithmetic、handoff 语义、隐式 reclaim 和 mapping 错误路径等确定问题 | 不采用 |
| 建立统一 memory manager、typed page owner、通用事务和 TLB batch | 表面上提供更强的统一接口 | 没有共同消费者证明；扩大公共 API、状态机、回滚模型和破坏性迁移；ArceOS/Axvisor 为 Starry 需求付费 | 不采用 |
| 在现有 owner 边界做最小修正 | 复用已经工作的 Buddy-Slab、页表和区域机制；每项修改都有当前代码证据；最小系统不增加常驻复杂度 | 部分高级策略仍由各系统分别实现，跨系统不会得到一个“全能”入口 | 采用 |

该选择的核心代价是接受系统策略存在差异。这个代价符合 TGOSKits 的产品形态，也比在底层维护一套所有系统都无法完整使用的统一策略更容易验证和回滚。

## 5. 目标职责

| 层/组件 | 保留职责 | 明确不负责 |
| --- | --- | --- |
| `someboot::mem` | 固件内存图、规范化、早期 bump、boot 页表帧、启动资源预留、冻结发布 | runtime 页分配、reclaim、进程/客户机策略 |
| `somehal` | 转发冻结的 boot facts；拥有运行期平台硬件状态 | 再维护一份可变内存图 |
| `axplat-dyn` / `ax-plat::MemIf` | 将 boot facts 适配为 runtime 可用 RAM、reserved、MMIO 和地址转换/cache capability | allocator 算法和 OS 统计策略 |
| `axhal` / `axruntime` | 构造 runtime region view，初始化 allocator，完成系统接线 | 隐式修正含糊的平台语义 |
| `ax-memory-addr` | 共享的物理/虚拟地址、页大小、对齐和 checked range 基础类型 | 资源所有权、分配和映射策略 |
| `ax-alloc` | 内核 heap、连续页、DMA32 物理限制、per-CPU slab、基础 usage 统计 | VFS reclaim、overcommit、fault、客户机策略 |
| `buddy-slab-allocator` | Buddy/Slab 算法和不连续 region 管理 | TGOSKits 平台和 OS glue |
| `page-table-generic` | 架构无关的页表 walk/map/unmap 执行机制 | PTE bits、MAIR、TLB 指令、VMID/ASID 策略 |
| `ax-memory-set` | 地址区间查找、拆分、合并和 backend 调度 | Linux VMA 语义、客户机布局、全局事务策略 |
| `ax-mm` | ArceOS kernel/user mapping 策略 | Starry/Linux 与 Stage-2 策略 |
| `axaddrspace` / AxVM | guest memory layout、Stage-2 policy 和架构实现 | host 进程内存管理 |
| Starry `mm` | Linux-compatible COW、fault、accounting、limits、reclaim | 向公共 allocator 注册策略 callback |
| `dma-api` / `mmio-api` | 设备内存约束、ownership 和映射能力 | 普通 RAM 或 OS VM policy |

依赖只允许从策略层指向机制层。机制层可以调用由架构/平台实现的小 capability，但不能通过全局 callback 进入 VFS、Starry 或 VM manager。

## 6. 最小契约

### 6.1 启动与 runtime 交接

不新增公共 handoff 对象，继续使用现有冻结内存图和 `MemIf`。需要收紧的契约为：

1. 固件输入先转换为不重叠 descriptor；所有 start/size/end/align 运算 checked；
2. early bump 仍只选择一个连续 region，优先最低的满足 region；x86_64 等架构通过私有常量限制末端地址；
3. region 最低容量由已知 CPU-local、stack、FDT copy 和必要 boot metadata 下界计算，未知页表增长仍由每次 `alloc` 的 `None` 明确失败；
4. boot allocator 所有已用区间在 freeze 前写回 memory map；冻结后没有任何修改入口；
5. `MemIf` 保持三个列表，但在一次 major 迁移中改为语义真实的命名：
   - `usable_ram_ranges()`：已经排除 kernel、boot allocations 和 firmware/platform reserved 的 runtime allocation candidates；
   - `reserved_memory_ranges()`：需要保留或映射、但绝不能交给 runtime allocator 的非 MMIO 区域，允许包含 kernel image；
   - `mmio_ranges()`：设备地址空间；
6. 三组 range 各自有序、无重叠；可用 RAM 与 reserved/MMIO 互斥；`usable_ram_size()` 返回第一组之和；
7. `axhal` 不再对已经规范化的可用 RAM 重复做 reserved subtraction；adapter 容量至少覆盖 boot map 上限和平台固定项；私有的 handoff preparation 在 allocator 初始化前完成验证，失败由启动入口报告具体类别，不在 lazy query 中裸 `unwrap()`；
8. handoff 不分配堆、不引入锁，继续使用固定容量存储。

这里选择在同一次 breaking PR 中替换含糊的旧名称和全部 workspace 调用方，不增加新旧并存的兼容双入口，也不新增统一 `MemoryRegionManager` 或动态 `Vec`。当前消费者实际需要的是可分配 RAM；Starry `MemTotal`、`sysinfo.totalram` 和 `_SC_PHYS_PAGES` 也应使用经过平台保留后的 usable RAM。若未来确实需要报告固件安装容量，应新增独立且有数据来源的 platform fact，不能用 reserved 列表反推。

### 6.2 Runtime allocator

公共规则只有：

```text
try_alloc(request) -> success | typed error
dealloc(allocation facts)
```

- 成功路径不调用 OS、VFS 或 VM policy；
- 失败立即返回，不隐式 reclaim 或循环重试；
- Starry 可在允许睡眠、未持关键锁的路径显式执行一次 `alloc -> bounded reclaim -> retry`；
- 页表、DMA、IRQ、任务栈和 Axvisor host 页等路径默认不回收；
- `add_memory()` 的所有 region 同时进入 Buddy-Slab 的 heap 和 page 能力，调用方不再维护“最大 region 才能分配页”的过期假设；
- 无 workspace 消费者的 `alloc_pages_at()` 从公共 trait 删除；TLSF 等 backend 无法提供但仍需保留的能力返回 `Unsupported`，不能 `unimplemented!()`；非平凡公开错误使用 `thiserror::Error`；破坏性调整必须使用 major version 和迁移说明；
- TLSF、stub 和 `AllocatorOps` 是否删除由 workspace/下游消费者和发布兼容性单独决定，不夹带到策略修正中。

这会让 Starry 的回收入口更显式，也会使某些以前被全局回调“偶然救活”的非用户页分配直接返回 `NoMemory`。这是有意的上下文收紧；迁移 PR 必须逐个分类 Starry 页分配调用点，不能机械地在所有调用处增加 reclaim。

### 6.3 Shared mapping mechanism

`ax-memory-set` 维持直接操作模型：

- backend 的 map/unmap/protect/split 返回可匹配的 `MappingResult`，不返回 `bool`/`Option`；
- backend 负责单次操作的失败一致性；失败前若修改了页表，必须在返回前恢复；
- `MemorySet` 只在 backend 成功后修改对应 area metadata；
- `MemorySet::map` 只接受不重叠的新区域，删除 `unmap_overlap` 布尔控制；只有 ax-mm 使用的 replacement 流程在 ax-mm 内显式组织；
- 多区域 unmap/protect 逐区提交；中途失败可以保留已完成前缀，但必须返回错误，且所有已处理和未处理区域的页表与 metadata 一致；
- API 文档明确 partial-progress 语义，需要全成或回滚的 Starry syscall 在 Starry 层 prepare/rollback；
- 测试 backend 在首个、中间和最后一个操作注入失败，验证无 panic、无假成功、无 metadata/PTE 分叉。

不新增 `MappingPlan`、`CommitState`、owner undo token 或容器抽象。`BTreeMap` 是否替换只有在真实 VMA 数量和 benchmark 证明必要时讨论。

### 6.4 页表、DMA 与 MMIO

- `page-table-generic` 保持现有 capability 注入，不进行架构格式或 TLB policy 重构；
- boot、runtime Stage-1 和 guest Stage-2 可以共享执行引擎，但各自拥有 PTE/TLB/MAIR/VMID 语义；
- DMA/MMIO 继续作为独立设备 capability；它们的 safety/ownership 修复单独设计、单独发布、单独验证，不成为普通 RAM 架构的依赖；
- 驱动只能经 `dma-api`、`mmio-api` 或明确 OS adapter 获取设备内存，不能直接依赖 allocator backend。

## 7. 实施顺序

主线只保留 4 个可以独立审核和验证的 PR，其中第一个是当前 PR。每个 PR 只建立一个主要不变量；不为凑阶段拆分机械迁移，也不把独立的 DMA/MMIO soundness 工作混入。

### 7.1 PR 1：启动内存所有权收敛（当前 PR）

- 分支：`mem1`，对应 PR #1978；
- 问题：`kernutil::StaticCell` 的发布/别名风险、启动内存分散所有权、`ranges-ext` 单一领域消费者；
- 修改：将启动内存类型和规范化迁入 `someboot`，删除 `kernutil`/`ranges-ext`，按 owner 改用 `OnceLock` 或私有状态，加入 boot map freeze；
- 收益：启动阶段可变、runtime 只读成为可检查的不变量；减少两个无充分公共职责的 crate；
- 边界：不改变 runtime allocator、VM policy、DMA 或页表契约；
- 验证：现有 someboot 单元测试、跨架构 check、目标 crate clippy 和 Starry system QEMU 回归。

### 7.2 PR 2：启动交接契约与算术加固

- 分支：`mem/boot-handoff-contract`；
- 问题：early bump/UEFI 未检查运算、固定 8 MiB 阈值、`MemIf` 文档与动态实现矛盾、handoff 容量可能 panic；
- 修改：全面使用 checked range/alignment；按已知启动下界和架构地址限制选择 early RAM；将 `MemIf` 的含糊接口一次迁移为 usable/reserved/MMIO 三类真实语义；让 `axhal` 直接消费已规范化可用 RAM；使 adapter 容量和失败路径可验证；
- 收益：固件异常或碎片化内存图不会 wrap 或在 lazy query 中 panic，统计和 allocator 看到同一事实；
- 兼容：`MemIf` 语义收紧属于公开契约变化，PR 必须评估版本提升并明确 adapter 迁移；
- 验证：旧实现必失败的 overflow、阈值边界、最大 descriptor 数、reserved/kernel、列表互斥和 `usable_ram_size` 测试；someboot/axplat-dyn/axhal clippy；四架构相关构建和至少一个动态 UEFI QEMU 启动。

### 7.3 PR 3：allocator 与回收策略分层

- 分支：`mem/allocator-policy-boundary`；
- 问题：公共 allocator 反向回调 Starry VFS 并多次重试；unsupported API 会 panic；runtime 对 multi-region 能力的注释和集成过期；
- 修改：删除 `PageReclaimFn`、注册入口和内部重试；Starry 只在明确允许的用户内存路径增加 bounded reclaim + 单次 retry；unsupported 返回 typed error；runtime 明确将全部 region 加入 Buddy-Slab；
- 收益：ArceOS/Axvisor 的 allocator 保持确定性，Starry 回收上下文可审核，OOM 错误不再触发隐藏控制流；成功分配热路径不增加开销；
- 代价：Starry 非安全回收上下文会更早得到 `NoMemory`，需要按调用场景确认预期；公开错误/API 调整可能需要 major version；
- 验证：证明 allocator 失败不调用外部 callback；Starry 仅显式路径回收且最多一次；IRQ/页表/DMA/VM host 路径不回收；新增 region 可用于页分配；unsupported 不 panic；目标 crate clippy 与 ArceOS/Axvisor/Starry QEMU 回归。

### 7.4 PR 4：共享映射错误与一致性契约

- 分支：`mem/mapping-error-contract`；
- 问题：backend 错误被 `bool` 压平，protect 假成功，unmap/split 可 panic，多区域失败语义不明；
- 修改：`MappingBackend` 使用 typed result；删除通用 map 的 `unmap_overlap` 开关并把唯一 replacement 策略移回 ax-mm；所有调用方机械迁移；`MemorySet` 在 backend 成功后更新 metadata；文档化 partial progress；为 ax-mm、Starry 和 axaddrspace backend 增加失败一致性测试；
- 收益：公共层不再吞错或在资源失败时 panic，页表和 area metadata 保持一致，同时不恢复复杂通用事务系统；
- 兼容：公共 trait 是 breaking change，需要 major version、迁移说明和全部 workspace 消费者同 PR 更新；
- 验证：首个/中间/最后操作故障注入；map/unmap/protect/split 无 panic、无假成功、无 PTE/metadata 分叉；ax-memory-set、ax-mm、Starry、axaddrspace、AxVM clippy/测试和 QEMU 回归。

### 7.5 不进入上述顺序的工作

以下工作只有各自问题和迁移方案成熟后单独立项，不阻塞 7.1—7.4 的依赖顺序：

| 工作 | 立项条件 |
| --- | --- |
| `dma-api` ownership/representation safety | 明确 soundness 反例、全部 typed DMA 调用方、breaking version 与不强制无关消费者依赖表示层 crate 的迁移方案 |
| `mmio-api` 初始化发布 | 明确初始化/并发契约，并用 owner-specific once initialization 替换 `static mut`，不与 DMA descriptor 迁移合并 |
| Starry COW/fault/accounting/reclaim | 对应 Linux ABI 或真实 workload 问题已复现；每个 PR 按 syscall guideline 验证 |
| `starry-mm` 本地 crate | Starry 策略已与 task/VFS/具体页表解耦，且独立构建/测试收益大于 crate 成本 |
| page-table invalidation batch | SMP/guest 正确性问题或测量证明逐页 flush 是瓶颈 |
| per-CPU page cache / allocator 替换 | 目标板数据证明 Buddy 锁或当前算法超过明确预算 |
| `ax-memory-set` 容器替换或通用事务 | 真实 VMA benchmark 或第二个消费者提出相同的全事务语义 |

## 8. 验收与性能预期

### 8.1 架构验收

- `cargo tree` 证明 `buddy-slab-allocator` 的生产入口由 `ax-alloc` 统一；
- `rg` 证明 `ax-alloc` 不再包含 reclaim callback，也不依赖 VFS/Starry/AxVM；
- `someboot` freeze 后没有可达的 memory-map 修改 API；
- `memory/` 不出现具体 PTE bits、MAIR/TLB 指令、Linux syscall policy 或 guest layout；
- Starry reclaim、COW 和 Linux accounting 只存在于 Starry 边界；
- 所有公开 unsupported 路径返回错误，不存在可达 `unimplemented!()`。

### 8.2 正确性验收

- 所有物理 range 运算覆盖零长度、最大地址、对齐溢出、重叠和容量上限；
- handoff 的 usable/reserved/MMIO 列表排序、互斥且和冻结 boot map 一致；
- 所有可用 region 都能进入 Buddy-Slab 页分配；
- allocator 分配失败不会进入未知 callback；
- shared mapping 故障注入后 PTE 与 area metadata 一致；
- 各 bug 修复先证明测试在旧实现上失败，再验证修复后通过；
- 代码修改后运行 `cargo fmt` 和对应 `cargo xtask clippy --package <crate>`；ArceOS、StarryOS 和 Axvisor 使用 `cargo xtask` 验证。

### 8.3 性能与资源

本方案不声明未经测量的性能提升。设计上的预期是：

- boot map 仍是最多数百项的一次性固定容量处理，允许简单的排序/区间操作；
- allocator 成功热路径不增加锁、trait object、owner allocation 或策略判断；
- 删除隐式 reclaim 不影响成功路径，只缩短并明确失败路径；
- typed mapping error 在内联后不应增加堆分配；
- handoff 扩大固定容量可能增加少量 `.bss`，PR 2 必须报告前后镜像/静态容量差异，不能仅称“无性能下降”；
- 若后续性能优化需要新增 per-CPU cache、后台回收或更复杂容器，必须先给出目标 workload、前后数据和最坏路径影响。

## 9. 后续决策规则

任何新的公共内存抽象必须同时回答：

1. 现在有哪些至少两个真实消费者共享完全相同的语义？
2. 不实现会导致什么可复现问题？
3. 现有 owner 局部修复为什么不能解决？
4. 对 ArceOS、Axvisor 和 StarryOS 最小构建分别增加多少 API、状态、锁和镜像成本？
5. 错误、并发、IRQ 和释放语义如何验证？
6. 如果公开 API 需要破坏性迁移，版本与下游迁移路径是什么？

回答不了这些问题时，默认保持现有简单边界。TGOSKits 的目标不是在公共层预先实现所有 OS 可能需要的内存能力，而是让简单系统拥有低成本底座，让 StarryOS 等复杂系统能够在不污染底层的前提下继续扩展。
