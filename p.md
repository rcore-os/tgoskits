# TGOSKits 内存架构收敛方案

## 1. 目的

TGOSKits 的内存能力同时服务 ArceOS、StarryOS、Axvisor、启动固件和设备驱动。本方案解决以下问题：

- 同一资源存在多个公共入口，所有权和释放方式不统一；
- allocator 会隐式进入上层回收策略，分配时延和调用上下文不可控；
- 虚拟内存区域和页表更新缺少事务，失败时可能留下部分状态；
- StarryOS 的写时复制、记账和回收策略与 kernel glue 混合；
- DMA token、缓冲区类型和零长度请求缺少足够的类型约束；
- 启动内存、运行时 allocator 和页表的交接边界不明确。

目标用户是 TGOSKits 内核、虚拟机、平台和驱动开发者。完成后的成功标准是：资源只有一个事实来源，依赖方向可解释，错误路径可回滚，关键路径时延可测，最小构建不携带无关策略。

### 1.1 非目标

本方案不引入 Linux 完整内存管理复杂度，不实现 NUMA、swap、compaction、多代回收、通用 OOM killer 或通用 pool manager。也不为满足目录名称而重写已经统一的实现。

## 2. 设计原则

1. **资源与策略分离**：allocator 只尝试分配；回收、重试和 overcommit 属于 OS 策略。
2. **单一所有权**：页、DMA mapping、MMIO mapping 和页表帧都有唯一 owner，释放由 owner 或 move-only token 驱动。
3. **失败保持一致**：跨多个虚拟内存区域或页表项的操作必须全成或回滚。
4. **能力向下传递**：底层通过小型 capability 接收页帧、TLB invalidation、文件页和 DMA domain，不回调上层全局对象。
5. **按证据抽象**：进入 `memory/` 的共享 crate 必须具有多个系统消费者；单系统的稳定边界留在所属子系统，容器和缓存策略由测试与测量决定。
6. **实时路径显式**：中断和声明为实时的路径不得隐式扩容、回收、执行文件 I/O 或阻塞。

## 3. 整体架构

```text
OS/VM policy                    Architecture/platform adapter
ax-mm | Starry MM | axaddrspace axcpu | someboot | axvm::arch | platform glue
              \                         /
               \                       /
                v                     v
        memory/ architecture- and OS-independent mechanisms
        ax-memory-addr | ax-alloc | page-table-generic | ax-memory-set
        buddy-slab-allocator | dma-api | mmio-api

Firmware memory map --> someboot::BootArena --> ax-runtime --> ax-alloc
```

`memory/` 只存放架构无关、OS 无关且由多个系统共享的内存机制。具体 PTE、页表级数、地址宽度、TLB 指令、MAIR、启动寄存器和 Stage-2 架构格式分别留在 `axcpu`、`someboot::arch` 和 `axvm::arch`；OS 虚拟内存策略留在对应 OS 内部。`no_std` 或可独立测试本身不构成迁入 `memory/` 的理由。

`ax-memory-addr` 提供共享地址与 checked arithmetic。`dma-api` 和 `mmio-api` 是架构无关的设备内存能力边界，因此可以保留在 `memory/`，但不属于普通页分配或进程虚拟内存层。

### 3.1 组件职责

| 组件 | 唯一职责 |
| --- | --- |
| `someboot::mem` | 固件内存描述、区间规范化、启动期 bump 分配、boot 页表帧供应和运行时交接 |
| `ax-alloc` | Normal/Dma32 物理页、内核堆、GlobalAlloc、统计和 per-CPU Slab 接线 |
| `buddy-slab-allocator` | Buddy 与 Slab 算法；只作为 `ax-alloc` 的实现依赖 |
| `page-table-generic` | 唯一页表遍历和修改引擎 |
| `axcpu`、`someboot::arch`、`axvm::arch` | 具体 PTE、页表层级、架构属性和 TLB invalidation |
| `ax-memory-set` | 虚拟内存区域组织及元数据/页表事务 |
| `ax-mm` | ArceOS kernel mapping、iomap 和地址空间策略 |
| Starry `mm` | Linux 进程虚拟内存、COW、记账、fault 和回收策略 |
| `axaddrspace` | 客户机地址空间和第二阶段转换策略 |
| `dma-api` | coherent/streaming DMA 的设备约束、domain 和生命周期 |
| `mmio-api` | 设备寄存器映射生命周期 |
| `ax-runtime`、`axhal`、`axklib` | 平台接线和能力 adapter，不拥有公共策略 |

### 3.2 依赖规则

- 策略层依赖公共机制，公共机制依赖基础类型或注入的 capability。
- `ax-alloc` 不依赖页表、VFS、StarryOS、DMA 或虚拟机策略。
- 页表核心不依赖 `ax-alloc`，页帧由 `PageFrameProvider` 注入。
- `ax-memory-set` 不依赖具体 OS 和具体页表类型。
- 驱动不直接依赖 allocator backend，只使用 `dma-api`、`mmio-api` 或明确的 adapter。
- 底层 crate 不通过全局回调进入上层策略。
- 迁移完成后删除无消费者的旧入口，不长期保留兼容 facade。

## 4. 核心设计

### 4.1 启动内存与交接

`someboot` 保留简单的 bump allocator，但将状态封装为 `BootArena`：

- 所有地址、大小和对齐运算使用 checked arithmetic；
- 状态只有 `Active` 和 `Frozen`；冻结后分配返回 typed error；
- arena 容量根据页表、CPU-local、启动栈和平台保留项计算，不使用固定阈值猜测；
- x86_64 需要低地址对象时，在选择阶段验证整个目标区间满足地址限制；
- `finish()` 消费活动 arena，生成 `BootMemoryHandoff`，由 runtime 初始化 `ax-alloc`；
- 引导处理器启动其他 CPU 前预留其必要的 CPU-local 和启动资源。

启动内存描述和区间规范化只服务 `someboot` 的 BSP 构造流程，不建立额外公共 crate。BSP 在完成内核、早期分配、MMIO 和 CPU-local 保留后冻结内存图；`somehal` 只转发冻结后的只读视图，`axplat-dyn` 再将它转换为运行时 `ax-plat::mem::MemIf` 能力。运行时内存组件不反向修改启动内存图。

### 4.2 运行时分配器

`ax-alloc` 使用一个稳定的 typed page API。建议的领域类型为：

```rust
enum MemoryZone {
    Normal,
    Dma32,
}

struct PageRequest {
    count: NonZeroUsize,
    align: PageAlignment,
    zone: MemoryZone,
    usage: UsageKind,
}

struct PageBlock {
    start: PhysAddr,
    count: NonZeroUsize,
    zone: MemoryZone,
    usage: UsageKind,
}
```

具体 API 应满足：

- `try_alloc_pages(PageRequest)` 成功时返回唯一 owner，失败立即返回 `AllocError`；
- 释放信息来自 owner，不要求调用者重新拼接 count、zone 或用途；
- 只在页表、外部 trait 等无法持有 owner 的 adapter 提供受限 raw alloc/free pair；
- 删除可达的 panic stub 和 `unimplemented!()`；不支持的能力返回 `Unsupported`；
- allocator 不调用 reclaim callback，也不在内部分配失败后重试；
- `GlobalAlloc`、页分配和 Slab 使用同一统计事实源，按来源和 `UsageKind` 派生只读视图；
- per-CPU Slab 保留跨 CPU 释放协议，CPU 启动时显式初始化。

若支持的系统配置全部使用 Buddy-Slab，应删除 TLSF、空 backend 和仅用于转发的 `AllocatorOps`。若某个 host 工具仍需要其他 backend，应将其隔离在工具或测试边界，不扩大内核运行时 API。

#### 4.2.1 回收策略

普通分配失败直接返回 `NoMemory`。允许回收的 Starry 线程上下文可以显式执行：

```text
try_alloc -> reclaim_clean_pages(budget) -> retry_once
```

回收预算必须包含最大扫描页数或最大处理批次。中断、持有关键锁、页表提交和 DMA 关键路径不得进入回收。

### 4.3 页表核心

保留当前已经统一的 `page-table-generic` 作为唯一执行引擎，不把改名为 `ax-page-table` 作为架构完成条件。

页表核心负责：

- 定义 `PageTableEntry`、`TableMeta` 等架构注入契约；
- 提供 walk、query、map、unmap、protect 和批量执行算法；
- 通过 `PageFrameProvider` 获取和释放页表帧；
- 返回 typed paging error；
- 生成固定容量的 invalidation batch。

页表核心不包含具体架构的 PTE bit、页表级数、地址宽度或寄存器操作，也不决定本地 TLB、跨 CPU shootdown 或 guest VMID 刷新。stage1、stage2 和 boot adapter 根据上下文消费 invalidation batch；batch 溢出时退化为明确的全量 invalidation，而不是静默丢失。

具体实现按使用场景归属：Stage-1 PTE 与 TLB 操作位于 `axcpu`，boot 实现位于 `someboot::arch`，Stage-2 实现位于 `axvm::arch`。只有构建数据证明存在镜像污染时，才增加 feature 边界。

AArch64 的 PTE 属性、MAIR layout 和 TLB 指令由架构组件维护，不能进入 `memory/page-table-generic`。boot、runtime 或 guest 共享页表语义时，由架构侧公共定义或显式 handoff 校验保证 AttrIndx 契约兼容；通用页表核心只接收 opaque `PteConfig`。

### 4.4 地址空间事务

`ax-memory-set` 的核心要求是事务正确性，不预先锁死使用排序 `Vec` 或 `BTreeMap`。内部通过私有 `AreaMap` 隔离容器选择。

一次 map、unmap 或 protect 分为：

1. `prepare`：校验区间、计算 split/merge、预留元数据容量、准备页表资源和 undo 信息；
2. `commit`：按计划修改 backend 和元数据；
3. `rollback`：任一步失败时恢复已修改的页表项、owner 和区域元数据。

约束如下：

- prepare 失败不得修改可观察状态；
- commit 成功后虚拟内存区域与页表项一致；
- backend 在事务完成前保留被移除的 owner 或返回 undo token，成功后才最终释放资源；
- backend 使用 typed error，不用 `bool` 压平 `NoMemory`、`NotMapped` 和 `Unsupported`；
- metadata-only 操作不得绕过 backend 事务；
- `clear`、覆盖映射和跨多个区域的 protect 同样遵循全成或回滚；
- 使用故障注入覆盖首次、中间和最后一次 backend 操作失败。

容器选择由真实 VMA 数量和操作分布决定。小规模地址空间可以使用排序 `Vec`；大量 VMA 场景可以保留树结构，但两者都必须满足同一事务契约。

### 4.5 StarryOS 内存策略

Starry 内存策略保留在 StarryOS 内部，先在 kernel `mm` 形成稳定边界，不迁入 `memory/`。如果独立构建和 host tests 能明显改善维护，可以在 `os/StarryOS/crates/` 下拆出 `starry-mm`，但不能创建只包含转发 trait 的 crate。拆分前必须满足：

- 不依赖具体 task、VFS、syscall dispatch 和硬件页表类型；
- COW、commit accounting、fault decision 和回收策略可由 host tests 独立验证；
- kernel adapter 只负责文件页、页表和进程资源接线；
- crate 仍属于 StarryOS 策略，不因 `no_std` 或可复用若干算法而成为公共 memory 组件。

必须先完成的语义包括：

- COW 引用计数在修改前检查上限；
- 父页表、子页表、引用计数和 RSS/accounting 任一步失败均可回滚；
- `RLIMIT_AS` 在地址空间扩张前检查，溢出返回正确错误；
- `/proc` 报告与真实 overcommit 行为一致；未实现 heuristic 模式时不得报告模式 0；
- 先支持 Always（报告模式 1）；Strict 作为可选策略（报告模式 2），启用时必须维护一致的 commit accounting；
- fault 返回结构化 `FaultOutcome`，由 kernel adapter 决定 signal、retry 或错误转换；
- reclaim 只回收允许的 clean page，使用显式预算且最多重试一次。

涉及 `mmap`、`mremap`、`brk`、`fork/clone`、fault、resource limit 和 `/proc` 的行为时，必须记录目标 Linux 版本、错误优先级和直接 syscall 回归证据。

### 4.6 DMA 与 MMIO

#### 4.6.1 DMA

- `DmaPod` 不得对任意 `Copy` 类型 blanket implementation；整数、浮点标量及合法元素的数组通过表示层 trait 自动满足契约，本地 wire-format 类型必须派生 `FromBytes + IntoBytes + Immutable`，由编译器拒绝非法 bit pattern、隐式 padding 和 interior mutability。
- 手写 `unsafe impl DmaPod` 只允许用于受孤儿规则限制、无法派生表示层 trait 的外部类型；必须同时提供来源版本、字段表示、bit-pattern、padding、所有权及 size/alignment 的审计证据。
- `DmaAllocHandle`、`DmaMapHandle` 不实现 `Copy` 或 `Clone`；dealloc/unmap 消费 token。
- coherent、contiguous 和 streaming API 在调用 backend 前拒绝零长度请求。
- `DeviceDma` 持有 address mask、alignment、boundary、segment 和 domain 约束。
- RAII owner 保存完整释放信息；dma-buf backing 由最后一个 owner 释放一次。
- Dma32 只描述物理可达范围，不替代 DMA mapping、cache maintenance、bounce buffer 或 IOMMU domain。
- identity mapping 只能由平台 adapter 明确选择；不支持的 domain 返回 `Unsupported`。

#### 4.6.2 MMIO

驱动通过 `mmio-api` 获取设备寄存器映射 owner。裸地址转换仅存在于平台 adapter 内，普通驱动不直接调用 allocator 或修改 kernel mapping flags。

## 5. 并发和上下文约束

- Buddy 保持简单的全局锁，只有测量证明锁竞争是主要瓶颈时才考虑 order-0 per-CPU cache。
- Slab、统计和区域事务使用明确锁顺序；持有 allocator 或地址空间广域锁时不调用未知 callback。
- IRQ handler 不使用 `GlobalAlloc`、Buddy、Slab 扩容、文件 I/O 或 reclaim。
- 对象确实需要中断期创建且存在容量上限时，使用子系统本地固定池；不新增全局 pool manager。
- 原子引用计数和跨 CPU free list 使用 Acquire/Release 发布与观察；`Relaxed` 只用于不承担同步的计数。
- TLB shootdown、DMA cache ownership 和页表发布必须在接口或安全说明中记录顺序。

## 6. 实施顺序

本节每个三级子章节严格对应一个独立 PR 和一个 `mem/<topic>` 分支。PR 从最新 `dev` 创建；只有依赖前一阶段新增 API 时才使用堆叠分支，并在前置 PR 合并后 rebase 到 `dev`。

每个 PR 必须满足：

- 只修改一个主要不变量或一个公共契约；跨 crate 修改只能是该契约的机械迁移；
- 修复 bug 时先加入在旧实现上必然失败的确定性测试，再实现修复；
- 同一 PR 完成对应测试、文档和调用方更新，不提交不可构建的中间状态；
- 合并后的 PR 如果出现两个可独立失败的非机械改造，或必须分别验证两套无关故障模型，应在开始编码前拆回两个连续子章节；纯机械且必须原子完成的调用方迁移不按文件数或行数强拆；
- 新旧 allocator API 只允许在 6.3 的内部迁移提交之间短期共存，PR 合并时必须删除旧入口。

主线收敛为以下 8 个 PR。较宽的 allocator 和页表 PR 使用多个可单独审查、保持可构建的提交组织，但整个子章节仍作为一个 PR 合并；Starry 准入与 fault 结果因 Linux ABI 故障语义不同而保持独立。

### 6.1 DMA 安全契约

- 分支：`mem/dma-safety`
- 范围：将 `DmaAllocHandle`、`DmaMapHandle` 改为 move-only，让 coherent、contiguous 和 streaming API 在 backend 前拒绝零长度；同时删除任意 `Copy` 的 `DmaPod` blanket implementation，以 `FromBytes + IntoBytes + Immutable + Copy` 统一证明基础类型、数组和本地驱动 wire-format 的表示安全，仅为无法派生的外部类型保留审计后的手写例外。
- 合并条件：compile-fail 证明 token 不可复制，且非法 bit pattern、裸指针和带隐式 padding 的类型不能成为 `DmaPod`；mock backend 证明零长度调用次数为 0；DMA 资源只释放一次；本地 wire-format 均使用表示层派生并固定协议 size/alignment，外部类型的每个手写 unsafe impl 都有版本、布局、bit-pattern、padding 和所有权安全说明；受影响驱动逐 crate clippy。
- 边界：`dma-api` 的契约修改与全部 descriptor 迁移必须原子完成，因此允许机械修改多个驱动 crate，但不得夹带队列、IRQ 或设备行为重构。

### 6.2 COW 克隆原子性

- 分支：`mem/cow-clone-atomic`
- 范围：封装 COW 引用获取和释放，在递增前检查上限；为单次 `clone_map` 建立 undo log，统一回滚父子 PTE、引用计数和 child accounting。
- 合并条件：在第一页、中间页、最后一页及引用计数上限注入失败时，父子状态均恢复到调用前；Starry kernel axtest 通过。
- 边界：仅修复现有 COW 克隆路径，不同时重构通用 `ax-memory-set`。

### 6.3 运行时 allocator 契约与迁移

- 分支：`mem/allocator-contract`
- 范围：在 `ax-alloc` 一次完成 typed page API、页 owner、typed error、单一统计事实源和公共可达 panic/stub 清理；随后机械迁移 axhal、ax-mm、ax-runtime、axtask、AxVM host、设备 adapter 及 Starry 页消费者；最后改为显式 `try_alloc -> bounded reclaim -> retry_once`，并删除 callback、旧 facade、无消费者抽象和 backend stub。
- 提交组织：依次提交核心契约、通用调用方、Starry 调用方、显式回收与清理；每个提交保持可构建，PR 合并前不保留兼容层。
- 合并条件：所有权、zone、对齐、失败、统计守恒和回收预算测试通过；allocator 失败不隐式回收，禁止回收上下文不进入 VFS；所有目标 crate clippy，ArceOS、Axvisor、Starry 构建、kernel axtest 和现有内存 QEMU case 通过。
- 边界：不改变 Buddy/Slab 分配算法；若支持配置仍需要 TLSF，则保留隔离实现并记录消费者。

### 6.4 启动内存交接

- 分支：`mem/boot-memory-handoff`
- 范围：将 someboot bump 状态封装为带 checked arithmetic 和 `Active/Frozen` 状态的 `BootArena`；按启动需求选择 arena，并实现 `finish()`、CPU-local 预留和 runtime handoff。
- 合并条件：overflow、越界、对齐、冻结后分配、多段内存图和容量不足测试通过；boot/runtime 不重复拥有内存；相关 someboot/ax-runtime 构建通过。

### 6.5 地址空间事务

- 分支：`mem/memory-set-transactions`
- 范围：将 `MappingBackend` 改为 typed error，加入可配置失败的测试 backend；为 map、覆盖映射、unmap、protect 和 clear 建立统一 prepare/commit/rollback 协议、undo token、提交前容量预留和 removed owner 延迟释放。
- 合并条件：错误转换覆盖 `NoMemory`、`NotMapped`、`Unsupported`；所有操作在首次、中间和最后一次 backend 失败时都不改变 VMA、PTE 和 owner；跨多区域测试通过。
- 边界：这些操作共享同一事务状态与故障注入模型，因此合并实现；`AreaMap` 容器替换必须在事务稳定并取得 benchmark 后单独决策。

### 6.6 页表执行契约

- 分支：`mem/page-table-contract`
- 范围：在 `page-table-generic` 引入固定容量 invalidation batch，依次迁移 axcpu/axhal Stage-1、someboot boot 和 AxVM/axaddrspace Stage-2 adapter，并删除旧 `TableMeta::flush`；同时在 AArch64 架构侧统一或显式校验 boot、runtime、guest 的 AttrIndx 与 MAIR 契约。
- 提交组织：依次提交通用 invalidation 契约、Stage-1 adapter、boot/Stage-2 adapter、AArch64 属性校验和旧入口清理；具体 PTE、TLB 指令和 MAIR bit 始终保留在各自架构组件。
- 合并条件：单核、SMP、boot 和 guest 测试覆盖单页、批量、batch 溢出全刷、VMID/全量刷新；属性测试覆盖 Device、Normal 和 NonCacheable；相关 QEMU 启动、AArch64 与 Axvisor 构建通过。
- 边界：不修改 boot 或 guest 的上层策略，不向 `memory/` 移入架构 bit；stage feature 裁剪和 crate 改名仍为延后决策。

### 6.7 Starry 地址空间准入

- 分支：`mem/starry-admission`
- 范围：建立单一地址空间准入入口，实现 Always 模式 commit accounting；使 `/proc/meminfo`、`overcommit_memory` 与实际策略一致，并在 mmap、mremap、brk、stack 和 fork 扩张点统一检查 `RLIMIT_AS`。
- 合并条件：成功与释放路径记账守恒；边界、溢出、失败不改状态和 errno 优先级测试通过；记录目标 Linux 版本及逐 syscall 证据，直接 syscall 与 `/proc` QEMU 回归通过。
- 边界：只实现 Always 模式和现有 syscall 的统一准入，不引入 Strict commit。

### 6.8 Starry fault 结果边界

- 分支：`mem/starry-fault-outcome`
- 范围：让 fault 内部返回结构化 `FaultOutcome`，由 kernel adapter 统一转换 retry、signal 和 errno。
- 合并条件：匿名页、文件页、COW、权限错误和内存不足测试通过；Linux ABI 对照证据完整。
- 边界：不在本 PR 抽取 `starry-mm` crate。

Strict commit、StarryOS-local `starry-mm` crate、容器替换、stage feature、per-CPU page cache 和性能优化都属于条件触发项，不在主线 8 个 PR 中。每个条件满足后仍必须单独提交一个 PR，不能捆绑到相邻阶段。

## 7. 验收

### 7.1 架构

- 运行时页和堆分配只有 `ax-alloc` 一个公共事实来源；
- `buddy-slab-allocator` 的生产消费者只有 `ax-alloc`；
- boot、stage1 和 stage2 共用一个页表执行引擎；
- `memory/` 不包含具体架构 PTE、TLB/MAIR 实现或 OS 专用虚拟内存策略；
- `ax-memory-set` 是共享虚拟内存区域机制，OS 和虚拟机只保留策略；
- 驱动只经 `dma-api`、`mmio-api` 或明确 adapter 获取设备内存能力；
- 生产依赖图没有反向 policy callback、双 backend 或循环依赖。

### 7.2 正确性

- DMA allocation/map token 不可复制，资源只释放一次，零长度不进入 backend；本地 DMA wire-format 的 bit-pattern、padding 和 interior mutability 约束由编译期派生证明，外部类型仅保留有完整布局证据的手写例外；
- allocator 失败立即返回 typed error，不执行隐式回收；
- map/unmap/protect/clear 在所有故障注入点保持元数据、页表项和 owner 一致；
- COW clone 在任意页、页表或 accounting 失败时恢复父子状态；
- BootArena 冻结后不能继续分配；交接内存不被 boot 和 runtime 重复拥有；
- TLB invalidation 在单核、SMP、guest 和 boot 场景分别执行正确策略；
- Starry 用户可观察的 overcommit、resource limit、fault 和 `/proc` 行为与声明一致。

### 7.3 工程与性能

- 修改 crate 后运行 `cargo fmt` 和目标 crate 的 `cargo xtask clippy --package <crate>`；
- ArceOS、StarryOS 和 Axvisor 使用对应 `cargo xtask` 构建或测试流程；
- bug 修复测试必须先证明旧实现失败，再证明修复后通过；
- 物理板和自托管流程按可用性单独验收，不以 host 测试替代；
- 每个目标定义可复现的 P99/max、锁等待和镜像预算；没有数据时不宣称 hard-RT 或性能达标。

常用依赖检查：

```sh
cargo metadata --format-version 1
cargo tree --workspace -i buddy-slab-allocator
cargo tree --workspace -i page-table-generic
cargo tree --workspace -i ax-memory-set
cargo tree --workspace -i dma-api
```

## 8. 延后决策

以下事项不作为当前重构的前置条件：

| 决策 | 触发条件 |
| --- | --- |
| 将 `page-table-generic` 改名 | 对外 API、发布或仓库命名一致性产生明确收益 |
| stage1/stage2/boot feature 化 | 构建数据证明未使用代码或静态状态进入目标镜像 |
| `AreaMap` 使用排序 `Vec` 或树 | 事务完成后取得真实 VMA 数量和操作 benchmark |
| 在 StarryOS 内拆分 `starry-mm` crate | 纯策略已与 task、VFS 和具体页表实现解耦，且独立构建或测试有明确收益 |
| 启用 Strict commit 模式 | Always 模式记账已稳定，且目标配置明确需要 strict admission |
| 增加 per-CPU order-0 cache | 目标板测量证明 Buddy 锁超过预算，且预分配无法解决 |

这些决策必须通过独立设计或测量完成，并各自作为一个 PR，不与正确性修复或相邻主线阶段捆绑。
