---
sidebar_position: 11
sidebar_label: "StarryOS 内存"
---

# StarryOS Linux 兼容内存管理

StarryOS 在公共内存机制之上实现 Linux 兼容虚拟内存。当前源码没有独立的 `memory/starry-mm` crate；相关策略、记账、后端、syscall 接线和 procfs 展示集中在 `os/StarryOS/kernel/src/mm/`、`os/StarryOS/kernel/src/syscall/mm/` 和 `os/StarryOS/kernel/src/pseudofs/proc.rs`。

StarryOS 与 ArceOS `ax-mm` 并列使用 `ax-memory-set` 和主机 Stage-1 页表机制，但不把 Linux 虚拟内存区域包装在 ArceOS `AddrSpace` 外面。StarryOS 自己维护 Linux VMA backend、写时复制、文件映射、共享页、RSS/VSS 统计和 fault/syscall 结果转换。

## 1. 当前边界

### 1.1 主要对象

`AddrSpace` 是 StarryOS 进程地址空间的核心 owner：

```rust
pub struct AddrSpace {
    va_range: VirtAddrRange,
    areas: MemorySet<Backend>,
    pt: PageTable,
    process_slots: AtomicUsize,
    pub vm_stat: ProcessVmStat,
    rss: MemoryAccounting,
}
```

| 字段 | 当前职责 |
| --- | --- |
| `va_range` | 用户虚拟地址上限 |
| `MemorySet<Backend>` | VMA 查找、拆分、收缩、metadata-only 操作 |
| `PageTable` | Starry 用户 Stage-1 页表 |
| `process_slots` | 引用该地址空间的 live process slot 数 |
| `ProcessVmStat` | 当前 VSS、VmPeak、当前近似 VmHWM |
| `MemoryAccounting` | Anon/File/Shmem RSS bucket、hiwater 和 COW charge map |

外层 `Arc<Mutex<AddrSpace>>` 负责进程共享和同步。`ax-memory-set` 自身不含锁，也不提供跨 VMA 的通用事务日志；StarryOS 需要回滚时由具体 backend 或 `AddrSpace` 专用流程维护局部记录。

### 1.2 与公共机制的关系

| 公共机制 | StarryOS 使用方式 | StarryOS 保留策略 |
| --- | --- | --- |
| `ax-memory-set` | 保存 VMA、调用 backend map/unmap/protect | Linux mmap/mprotect/mremap 规则、reported flags |
| `page-table-generic`/`axcpu::paging` | Stage-1 页表和 frame allocator | fault access、signal/errno 转换 |
| `ax-alloc` | `UsageKind::VirtMem`、`PageTable`、`PageCache`、`Dma` | RSS 分类、COW ref、page-cache owner |
| `dma-api` | dma-buf、device mmap/import owner | fd、mmap retainer、设备操作期 owner |

当前 StarryOS 在 `kernel/src/entry.rs` 注册 `ax_fs_ng::vfs::page_cache_reclaim`。因此页分配不足时，`ax-alloc` 的 page allocation 路径会在 allocator 锁外调用已注册回收函数并最多重试 4 轮，而不是由 `AddrSpace::handle_page_fault()` 外层实现“一次 reclaim/retry”包装。

## 2. 映射后端

`os/StarryOS/kernel/src/mm/aspace/backend/` 使用 enum dispatch 统一四种 backend：

| Backend | 物理来源 | 典型 mapping | 释放/记账 |
| --- | --- | --- | --- |
| `Linear` | 已知连续物理地址 | 设备、ION 或 kernel-provided range | 不释放外部物理区 |
| `Cow` | anonymous page 或私有 file page | `MAP_PRIVATE`、匿名 heap/stack | frame table 引用计数，RSS Anon/File |
| `Shared` | `Arc<SharedPages>` | shared anonymous、SysV SHM、borrowed/imported pages | 最后一个 `Arc` 决定 allocated 页释放 |
| `File` | VFS/page cache | `MAP_SHARED` file mapping | page cache owner 与 dirty/writeback 语义 |

每个 `MemoryArea` 同时保存 actual flags 和 reported flags。写时复制页表项可以保持只读以捕获第一次写 fault，而 VMA 仍向用户报告原始可写属性。

### 2.1 map/unmap/protect

`AddrSpace::map()` 先验证虚拟地址范围和 4 KiB 对齐，再进入 `MemorySet::map()`。backend 直接修改页表，成功后 `vm_stat.on_map()` 增加 VSS。`unmap()` 会先计算实际移除的 VMA 页数，调用 memfd listener，再由 `MemorySet::unmap()` 调 backend，最后 `vm_stat.on_unmap()`。

`protect_with_reported_flags()` 通过 `MemorySet::protect_with_reported_flags()` 拆分相交 VMA，并在同一次 backend 调用中更新 actual flags 与 reported flags。File-backed private COW backend 可把实际 PTE 写权限清除，同时保留用户可见写权限。

### 2.2 metadata-only 操作

`unmap_metadata()` 和 `replace_area_metadata()` 用于“页表项已经由专用流程移动或分离，只更新 VMA 元数据”的路径。例如 COW fork 由 backend `clone_map()` 安装 child 页表项和 RSS charge，再用常规 `map`（不允许覆盖）发布 child VMA metadata；mremap 失败恢复用 `replace_area_metadata()` 还原源区域描述。公共 `MemorySet` 不重复保存这些页表项的 undo 日志。

## 3. 写时复制

当前 COW frame table 位于 `os/StarryOS/kernel/src/mm/aspace/backend/cow.rs`：

```rust
struct FrameRefCnt {
    count: u8,
}

struct FrameTableRefCount {
    table: BTreeMap<PhysAddr, Arc<IrqMutex<FrameRefCnt>>>,
}
```

这表示当前实现仍是 `u8` 引用计数和每 frame `Arc<IrqMutex<_>>`，不是独立 crate 中的 checked `u32` 引用规则。新增 fork/COW 逻辑必须显式考虑引用计数上限和失败清理，不能依赖文档假设的 `u32` 设计。

### 3.1 fault-in 与写故障

`CowBackend::populate()` 根据访问类型决定 RSS 分类：

| fault | 物理页内容 | PTE flags | RSS |
| --- | --- | --- | --- |
| anonymous read/write | 新清零页 | VMA flags | `Anon` |
| private file read | 从 file offset 读取并尾部清零 | 通常保持只读 | `File` |
| private file first write | COW 新页或重分类 | 可写 | `Anon` |

文件私有映射第一次写入时，`cow_file_write_to_anon()` 会在地址空间锁内把 charge 从 File 迁移为 Anon；找不到旧 charge 时会尝试 adopt 为 Anon 并减少一个 File bucket。

### 3.2 clone

`AddrSpace::try_clone()` 创建未发布 child 地址空间，逐个 parent VMA 调用 backend `clone_map()`。COW backend 会修改父子页表项、增加 frame 引用并给 child 建立 RSS charge。随后 child 用 `MemoryArea::new_with_reported_flags()` 发布 metadata。

失败时，fresh child 尚未发布，已建立的 child 映射和 frame 引用依靠 backend 清理与 child clear/drop 路径释放。父进程的页表 flags 恢复和 frame 引用对称释放是 COW 代码的关键审计点。

## 4. RSS 与 VSS

StarryOS 当前把虚拟映射规模和 resident page 分开维护。

### 4.1 `MemoryAccounting`

`MemoryAccounting` 使用三个 relaxed atomic bucket 保存 `rss_anon`、`rss_file`、`rss_shmem`，并维护 `hiwater_rss`。此外，它用 `UnsafeCell<BTreeMap<VirtAddr, RssKind>>` 保存 COW resident charge；所有 charge map 访问要求调用方持有 `AddrSpace` 锁。

减少操作当前使用 `fetch_sub`，debug build 会断言下溢，发布构建不会返回 checked error。因此文档、测试和评审不能声称 RSS 减少在发布构建中会转换为 `AxError::BadState`。

### 4.2 `ProcessVmStat`

`ProcessVmStat` 位于 `os/StarryOS/kernel/src/mm/vm_stat.rs`。它用 `AtomicI64` 保存当前 VSS 页数，用 `AtomicU64` 保存 `peak_vss_pages` 和 `peak_rss_pages`。当前 `peak_rss_pages` 仍在 map 时按 VSS 更新，是近似高水位；真实 RSS 当前值和 hiwater 由 `MemoryAccounting` 提供。

### 4.3 proc 展示

`ProcessMemStats::collect()` 遍历 `AddrSpace::areas()`，按 path、flags、shared 标志和 stack range 计算 VmSize/Text/Data/Stack/Exe 等 VMA 分类，再合并 `MemoryAccounting` 的 RSS bucket。

| 展示 | 数据来源 |
| --- | --- |
| `/proc/<pid>/statm` | `ProcessMemStats::format_statm()` |
| `/proc/<pid>/status` Vm/RSS 行 | `format_status_vm_lines()` |
| `VmRSS` | Anon + File + Shmem resident |
| `VmPeak` | `ProcessVmStat::peak_vss_pages()` 与当前 VSS 的最大值 |
| `/proc/meminfo Committed_AS` | 当前固定展示 `0 kB` |
| `/proc/sys/vm/overcommit_memory` | 当前固定展示 `0` |

这意味着当前 procfs 还没有完整 Linux committed-memory ledger。不要把 `Committed_AS`、overcommit mode 或 CommitLimit 描述成已经由 Starry mm 策略精确维护。

## 5. 缺页路径

`AddrSpace::handle_page_fault(vaddr, access_flags)` 返回 `bool`：`true` 表示 fault 被处理，`false` 交给 trap 层转换为用户可见 fault。它的主流程是：

```text
check vaddr in user range
  -> find containing MemoryArea
  -> compare VMA actual flags with requested access
  -> align fault address to backend page size
  -> backend.populate(range, flags, access, Some(&rss), &mut pt)
  -> run optional populate callback
  -> success only if populated page count > 0
```

File backend 可能返回 deferred callback，用于处理 page-cache eviction 期间需要在 `AddrSpace` 上执行的 unmap/TLB flush 等清理。调用方不能丢弃 callback，否则可能让用户 PTE 继续指向已经回收的 page-cache frame。

内存不足时，backend 最终来自 `ax_alloc::global_allocator().alloc_pages(..., UsageKind::VirtMem)`。是否尝试回收由 `ax-alloc` 的注册 callback 决定，不由 `handle_page_fault()` 直接区分 `FaultOutcome::NoMemory` 这类结果枚举。

## 6. 文件、共享页和 dma-buf

### 6.1 文件映射

`FileBackend` 仍留在 Starry kernel。私有 file mapping 走 COW backend，shared file mapping 走 File backend/page cache。syscall 层在 destructive `MAP_FIXED` unmap 前先验证 file mmap 权限和 memfd seals，避免 `EACCES` / `EPERM` 时提前撕掉旧映射。

### 6.2 `SharedPages`

`SharedPages` 当前有两种 owner：

| Owner | Drop 行为 | 使用场景 |
| --- | --- | --- |
| `Allocated` | 逐页 `dealloc_frame()` | shared anonymous / SysV SHM |
| `Borrowed(retainer)` | 只释放 retainer，不释放物理页 | dma-buf、设备或外部 owner mapping |

`SharedPages::new(size, page_size)` 中途分配失败时，当前构造路径没有显式回滚已经 push 到 `phys_pages` 的页；因为 `result` 的 Drop 会释放已保存的 allocated pages，最终仍依赖 RAII 清理。

### 6.3 dma-buf

Starry `/dev/dma_heap` 和设备 import 使用 `dma-api` 高层 owner。fd、mmap 和设备操作通过 `Arc` retainer 保持同一 allocation live；设备 backend 不能释放 imported buffer，也不能把 fd 生命周期当作唯一 owner。

## 7. 当前限制

| 限制 | 当前状态 |
| --- | --- |
| 独立 `starry-mm` crate | 未存在，策略在 Starry kernel 内 |
| committed-memory ledger | 未实现为独立对象，`Committed_AS` 固定 0 |
| overcommit mode | procfs 固定展示 `0`，不是完整 Linux mode 实现 |
| swap | 未实现，proc 多项固定 0 |
| COW ref count | `u8`，不是 checked `u32` |
| RSS decrement | debug assert，下溢在发布构建不返回 typed error |
| fault result enum | `handle_page_fault()` 返回 `bool`，没有公共 `FaultOutcome` |
| dirty-page writeback reclaim | page-cache reclaim 由注册 callback 提供，需保持锁外和有界 |

增加用户可见内存语义时必须按 `docs/guideline/starry_syscall.md` 对齐 Linux 行为；unsupported 路径应返回明确错误，不能静默成功或仅靠 proc 占位值误报完整支持。

## 8. 源码检查点

| 源码 | 审计重点 |
| --- | --- |
| `os/StarryOS/kernel/src/mm/aspace/mod.rs` | `AddrSpace`、VMA/page-table/RSS/VSS 编排、fault、clone |
| `os/StarryOS/kernel/src/mm/aspace/accounting.rs` | RSS bucket、COW charge map、fork charge reconciliation |
| `os/StarryOS/kernel/src/mm/aspace/backend/cow.rs` | COW frame table、file private fault、clone/unmap cleanup |
| `os/StarryOS/kernel/src/mm/aspace/backend/shared.rs` | allocated/borrowed `SharedPages` owner |
| `os/StarryOS/kernel/src/mm/aspace/backend/file.rs` | shared file mmap、page-cache dirty/writeback 回调 |
| `os/StarryOS/kernel/src/mm/vm_stat.rs` | VSS 和 peak 统计 |
| `os/StarryOS/kernel/src/mm/stats.rs` | proc 进程内存聚合 |
| `os/StarryOS/kernel/src/syscall/mm/` | Linux mmap/brk/mprotect/mremap/mincore syscall 语义 |
| `os/StarryOS/kernel/src/pseudofs/proc.rs` | meminfo、statm、status、overcommit sysctl 展示 |
| `os/StarryOS/kernel/src/entry.rs` | page-cache reclaim callback 注册 |

StarryOS 的系统调用、跨 VMA 失败注入、写时复制回滚和 RSS 统计用例集中在[内存管理测试与验收](./testing.md)。
