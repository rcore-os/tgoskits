# TGOSKits 内存管理整改方案

本文只直接使用 DMA、CPU、MMU、MMIO、RAM 和 API 等内存与系统领域中普遍使用的缩写。其他术语第一次出现时同时给出英文全称和中文名称，后续优先使用中文名称；源码类型、Cargo feature、命令和路径保持代码中的原名。

## 0. 术语约定

术语约定用于区分可以直接阅读的通用缩写和必须解释的领域缩写，避免架构与实施条目依赖读者预先掌握 Linux、虚拟化或硬件术语。

### 0.1 保留缩写

下列缩写在处理器、内存和驱动文档中使用频率高，正文可以直接使用，但首次定义仍给出完整含义。

| 缩写 | 英文全称 | 中文名称 |
| --- | --- | --- |
| DMA | Direct Memory Access | 直接内存访问 |
| CPU | Central Processing Unit | 中央处理器 |
| MMU | Memory Management Unit | 内存管理单元 |
| MMIO | Memory-Mapped Input/Output | 内存映射输入输出 |
| RAM | Random-Access Memory | 随机存取内存 |
| API | Application Programming Interface | 应用程序编程接口 |
| OS | Operating System | 操作系统 |

这些缩写可以出现在架构图和表格中；涉及具体所有权、地址或页表语义时，仍必须说明对象和约束，不能只写缩写。

### 0.2 展开术语

下列术语不作为无解释的通用缩写使用。正文优先使用中文名称，必须保留缩写以对应源码或外部规范时，采用“英文全称（中文名称，缩写）”的形式。

| 缩写 | 英文全称 | 中文名称 |
| --- | --- | --- |
| NUMA | Non-Uniform Memory Access | 非统一内存访问 |
| RAII | Resource Acquisition Is Initialization | 资源获取即初始化 |
| COW | Copy-on-Write | 写时复制 |
| PTE | Page Table Entry | 页表项 |
| TLB | Translation Lookaside Buffer | 地址转换后备缓冲区 |
| VMA | Virtual Memory Area | 虚拟内存区域 |
| RSS | Resident Set Size | 常驻内存集大小 |
| VSS | Virtual Memory Size | 虚拟内存大小 |
| IOMMU | Input-Output Memory Management Unit | 输入输出内存管理单元 |
| IOVA | Input-Output Virtual Address | 输入输出虚拟地址 |
| IOTLB | Input-Output Translation Lookaside Buffer | 输入输出地址转换后备缓冲区 |
| LRU | Least Recently Used | 最近最少使用 |
| PCP | Per-CPU Page Cache | 每处理器页缓存 |
| BSP | Bootstrap Processor / Board Support Package | 引导处理器 / 板级支持包；必须按上下文写明中文含义 |
| AP | Application Processor | 应用处理器 |
| IRQ | Interrupt Request | 中断请求 |
| RTOS | Real-Time Operating System | 实时操作系统 |
| ABI | Application Binary Interface | 应用程序二进制接口 |
| VFS | Virtual File System | 虚拟文件系统 |
| HAL | Hardware Abstraction Layer | 硬件抽象层 |
| PA / VA | Physical Address / Virtual Address | 物理地址 / 虚拟地址 |
| OOM killer | Out-of-Memory Killer | 内存不足终止器 |
| RT | Real-Time | 实时 |
| SMP | Symmetric Multiprocessing | 对称多处理；正文按语义写作“多核” |
| IPI | Inter-Processor Interrupt | 处理器间中断 |
| NPT | Nested Page Table | 嵌套页表 |
| ISR | Interrupt Service Routine | 中断服务程序 |
| FDT | Flattened Device Tree | 扁平设备树 |
| DTB | Device Tree Blob | 设备树二进制对象 |
| MPU | Memory Protection Unit | 内存保护单元 |
| SMMU | System Memory Management Unit | 系统内存管理单元 |
| GPA / HPA / IPA | Guest / Host / Intermediate Physical Address | 客户机 / 主机 / 中间物理地址 |
| VM | Virtual Memory / Virtual Machine | 虚拟内存 / 虚拟机；必须按上下文写明中文含义 |

`PTE`、`TlbInvalidator`、`CowFrameReferences` 等源码符号不翻译；符号之外的设计说明分别使用“页表项”“地址转换后备缓冲区失效器”和“写时复制页引用”等中文名称。

## 1. 当前问题

本节的“当前”和“现有”均指整改前基线 `origin/dev@7725edd6372b`。下面的路径和行号固定到该提交，不表示已完成迁移的工作区仍保留这些实现。

### 1.1 组件边界和依赖混乱

整改前的内存能力分散在启动、分配器、页表、地址空间和设备适配组件中；同一资源存在多个入口或机械包装，导致依赖方向与所有权来源无法由组件名直接判断。

| 范围 | 整改前组件或位置 | 具体代码示例 | 问题与直接影响 |
| ---- | ---------------- | ------------ | ------------ |
| 运行时分配 | ax-alloc、ax-allocator、buddy-slab-allocator、bitmap-allocator、TLSF/rlsf | workspace `Cargo.toml:138` 将 `ax-alloc` 指向 `os/arceos/modules/axalloc`；`os/StarryOS/kernel/Cargo.toml` 和 `drivers/ax-driver/Cargo.toml` 直接依赖它，`virtualization/axvm/src/host/arceos.rs:38-71` 也通过 ArceOS host 调用它 | 共享运行时组件位于 ArceOS 私有目录，目录归属与消费者不一致；修改该模块会同时影响 StarryOS、AxVM host、驱动和 ArceOS |
| 运行时分配 | ax-alloc、ax-allocator、ax-dma | workspace `Cargo.toml:138-149` 同时注册三个 crate；`os/arceos/modules/axdma/Cargo.toml` 又同时依赖 `ax-alloc` 与 `ax-allocator` | 页、字节和 DMA 分配存在多个公共入口，形成两套所有权、扩容、统计和错误模型 |
| 启动内存 | someboot::mem、kernutil::memory、ranges-ext | 整个生产树只有 `components/kernutil/src/memory.rs:76` 实现 `ranges_ext::RangeOp`，以及 `platforms/someboot/src/mem/mod.rs:7` 使用 `ranges_ext::*` | ranges-ext 没有形成跨领域复用边界，却需要独立维护一个通用 crate 和一组未使用 API |
| 页表 | ax-page-table-entry、ax-page-table-multiarch、page-table-generic | `memory/page_table_multiarch/src/bits64.rs` 提供 `query/walk/map/unmap/protect/copy_from`；`memory/page-table-generic/src/table.rs` 另行提供 `map/unmap/walk/is_mapped` | 两套引擎重复实现遍历和映射语义，但使用不同 API、错误和 frame trait，消费者不能共享 adapter |
| 页表 | StarryOS/ax-hal 与 someboot/somehal/AxVM/axaddrspace | 前者依赖 `ax-page-table-multiarch`，后者依赖 `page-table-generic` | walker、错误处理或架构属性的修复必须分别修改并验证两条链路 |
| 地址空间 | ax-memory-set、ax-mm、StarryOS mm/aspace、axaddrspace | `ax-mm::AddrSpace`、Starry `mm::aspace::AddrSpace` 和 `axaddrspace::AddrSpace` 都公开 `map/unmap/protect`，其 backend 都再次实现 `MappingBackend` | 虚拟内存区域 split/shrink、错误转换和页表更新规则分散在公共机制与三个策略层 |
| DMA | dma-api + KlibDma、ax-dma + DMAInfo | RGA 核心已使用 dma-api；`os/StarryOS/kernel/src/file/dmabuf.rs` 仍持有 `DMAInfo`，RGA/JPEG/NPU feature 均继续依赖 ax-dma | 同一 dma-buf 调用链使用两套 DMA owner 和释放接口，feature 裁剪也无法消除 legacy 路径 |
| MMIO/DMA RAM | mmio-api、ax-mm/axklib iomap、legacy ax-dma | `os/arceos/modules/axdma/src/dma.rs:127-133` 直接调用 `ax_mm::kernel_aspace().protect(..., UNCACHED)`；MMIO 经 `components/axklib/src/lib.rs:109` 的 `mem_iomap(PhysAddr, size)` 进入 `ax-mm::iomap` | 两类资源最终都表现为裸 `PhysAddr/VirtAddr + MappingFlags`，调用边界不能表达“设备寄存器”或“由 DMA owner 持有的 RAM” |

### 1.2 分配器职责过重

整改前的运行时分配器同时承担算法选择、回收回调、统计和不受限公共抽象，单次分配的行为与最坏执行时间因此取决于上层策略。

| 问题 | 整改前代码示例 | 直接影响 |
| ---- | -------------- | -------- |
| backend 选择进入公共组件 | `os/arceos/modules/axalloc/Cargo.toml:10-15` 同时定义 `tlsf` 和 `buddy-slab`；同目录 `build.rs:5-10` 决定 cfg；`src/lib.rs:204-220` 还保留 `stub_impl` | 每个接口要在 TLSF、Buddy-Slab 和 stub 三处保持一致，未选择 backend 仍能构建空实现 |
| 无消费者的公共抽象 | `os/arceos/modules/axalloc/src/lib.rs:139-202` 暴露包含 14 个方法的 `AllocatorOps`，`DefaultByteAllocator` 也被公开；基线全树没有生产调用方通过该 trait 或别名工作 | 公共 API 只机械转发具体 `GlobalAllocator`，扩大维护面而没有隔离真实多实现边界 |
| unsupported 能力以 panic 表达 | `os/arceos/modules/axalloc/src/buddy_slab.rs:259-266` 和 `src/tlsf_impl.rs:154-161` 的 `alloc_pages_at` 都执行 `unimplemented!()`，但该方法仍位于公共 trait | 调用方可以通过合法公共 API 触发不可恢复 panic，而不是收到 typed unsupported error |
| allocator 反向调用上层回收 | `os/arceos/modules/axalloc/src/lib.rs:22-42` 保存 `PageReclaimFn`；`os/StarryOS/kernel/src/entry.rs:34` 将 `ax_fs_ng::vfs::page_cache_reclaim` 注册进去 | allocator 依赖运行时注册顺序，并可在未知锁和上下文中进入虚拟文件系统/page-cache 逻辑 |
| 内存不足 内部执行多次策略重试 | `os/arceos/modules/axalloc/src/buddy_slab.rs:207-226` 分配失败后最多 4 次调用 `try_page_reclaim(num_pages.max(16))` 并重新获取 allocator 锁 | 单次页分配的最大执行时间包含文件页扫描和四次重试，无法由 allocator 算法本身界定 |
| DMA 又建立字节 allocator | `os/arceos/modules/axdma/src/dma.rs:14-23` 内含 `SlabByteAllocator`，不足时在 `:41-70` 从全局页 allocator 扩容，再把页面改为 uncached | DMA 小块和普通 Slab 分别维护 free list、扩容和统计，释放路径还必须按 layout 猜测来源 |
| 上下文约束没有进入接口 | `alloc_pages(num_pages, alignment, UsageKind)` 和 `GlobalAlloc::alloc(Layout)` 都没有 context/capability；后者失败直接进入 `handle_alloc_error` | 中断请求、bottom-half 与普通线程看到相同入口，类型和运行时检查都不能阻止实时路径进入扩容或 内存不足 处理 |

### 1.3 页表和地址空间重复

整改前存在两套页表执行器和三个地址空间包装，页表项、错误、页帧来源与映射事务无法共享同一契约，失败时还会产生部分提交。

| 问题 | 整改前代码示例 | 直接影响 |
| ---- | -------------- | -------- |
| 页表项与页表执行器被拆成不对称边界 | `memory/page_table_entry/src/lib.rs:41` 定义 `GenericPTE`；真正的 `PagingError`、`PagingHandler` 和 cursor 位于 `memory/page_table_multiarch/src/lib.rs`、`bits32.rs`、`bits64.rs` | entry crate 不能独立完成映射，使用者仍必须了解另一 crate 的内部契约 |
| generic 重新定义相同领域概念 | `memory/page-table-generic/src/lib.rs:64-91` 定义 `PageTableEntry/PageTableOp`，`src/def.rs:129` 再定义 `PagingError`，`src/table.rs:87-219` 再实现 map/unmap/walk | 同一个架构页表项、错误和 frame provider 需要不同 adapter，行为对照只能依靠人工维护 |
| 三个地址空间层重复包装 | `os/arceos/modules/axmm/src/aspace.rs:17`、`os/StarryOS/kernel/src/mm/aspace/mod.rs:45`、`virtualization/axaddrspace/src/address_space/mod.rs:29` 各有 `AddrSpace`；分别再次提供 map/unmap/protect | 公共虚拟内存区域机制的修复需要逐个迁移三个 wrapper，策略差异和机械转发混在一起 |
| overwrite map 不是事务 | `MemorySet::map` 在 `memory/memory_set/src/set.rs:161-169` 先执行 `self.unmap(...)`，随后才调用 `area.map_area(...)` | 新 backend 映射失败时，原重叠虚拟内存区域/页表项已经删除，调用方得到错误但状态无法恢复 |
| unmap 可产生部分提交或 panic | `memory/memory_set/src/set.rs:194-202` 在 `BTreeMap::retain` 中逐个 `unmap_area(...).unwrap()` 并立即删除；之后 `:204-230` 才处理边界虚拟内存区域 | 任一后续 backend 失败时，前面的区域已释放；整区失败还会由 `unwrap` 直接 panic |
| protect 可产生页表项/虚拟内存区域不一致 | `memory/memory_set/src/set.rs:353-402` 边遍历边 split、protect 和修改 flags，没有 undo；`src/area.rs:121-123` 甚至丢弃 backend `protect` 的 bool 返回并固定返回 `Ok(())` | 前半段成功、后半段失败时无法回滚；backend 失败还可能被报告为成功 |
| backend 错误信息被压扁 | `MappingBackend` 的 `map/unmap/protect` 在 `memory/memory_set/src/backend.rs:18-37` 都返回 `bool` | `NoMemory`、`NotMapped`、`Unsupported` 和页表冲突只能统一转换成 `BadState`，上层无法选择恢复动作 |

### 1.4 StarryOS 内存策略未独立

整改前的 Linux 兼容策略直接位于 StarryOS kernel，并同时依赖文件系统、硬件适配和页表实现，写时复制及进程记账无法独立测试。

| 问题 | 整改前代码示例 | 直接影响 |
| ---- | -------------- | -------- |
| Linux 虚拟内存策略直接依赖 kernel 实现 | `os/StarryOS/kernel/src/mm/aspace/backend/cow.rs:8-20` 同时导入 `ax_fs_ng::FileBackend`、`ax_runtime::hal`、`ax_sync::Mutex` 和 kernel `AddrSpace/MemoryAccounting` | 写时复制规则无法在不构建 Starry kernel、虚拟文件系统和硬件抽象层 adapter 的情况下独立测试或复用 |
| 写时复制 refcount 上限处理破坏状态 | `os/StarryOS/kernel/src/mm/aspace/backend/cow.rs:23-25` 使用 `u8`；`clone_map` 在 `:581-587` 先执行 `frame.count += 1`，加到 `u8::MAX` 后才返回错误 | 失败返回时当前页计数已经改变；若错误发生在多页 clone 中间，之前页面也没有统一回滚 |
| fork 提交顺序没有回滚 | `os/StarryOS/kernel/src/mm/aspace/backend/cow.rs:588-594` 依次修改父页表项、映射子页表项、复制 accounting；任一步之后的操作失败都直接 `?` 返回 | 父页表项可能已只读、子页表项可能已存在、refcount 可能已增加，但 fork 对外报告失败 |
| procfs 声明与实际策略不一致 | `os/StarryOS/kernel/src/pseudofs/proc.rs:1859-1861` 固定返回 `overcommit_memory=0`，`render_meminfo():145-146` 固定 `Committed_AS: 0` | 用户看到 Linux heuristic overcommit，但 mmap/brk/fork 没有相应 admission 与 commit 记账 |
| 回收入口位于 allocator 而不是 Starry 策略层 | `os/StarryOS/kernel/src/entry.rs:34` 注册虚拟文件系统 page-cache callback；随后任何 ax-alloc 页分配失败都可能触发它 | fault、普通内核页和 DMA32 页申请共享同一隐式回收入口，调用方不能限制只回收一次或禁止当前上下文回收 |

### 1.5 DMA 所有权和类型安全不足

整改前的 DMA 路径混用 `dma-api` 与 legacy `ax-dma`，可复制释放令牌和过宽的 `DmaPod` 实现无法阻止重复释放或无效设备写入类型。

| 问题 | 整改前代码示例 | 直接影响 |
| ---- | -------------- | -------- |
| `DmaPod` blanket 过宽 | `memory/dma-api/src/def.rs:156-158` 为所有 `T: Copy` 实现 `DmaPod` | `&'static u8`、包含裸资源 token 的 Copy 类型或零位模式无效的类型都能进入设备可写 buffer，违反 trait 自己的 Safety 合约 |
| 释放 token 可复制 | `DmaAllocHandle` 在 `memory/dma-api/src/def.rs:160`、`DmaMapHandle` 在 `:201` 都派生 `Clone, Copy`；legacy `DMAInfo` 在 `os/arceos/modules/axdma/src/lib.rs:138` 也派生 `Clone, Copy` | 同一 allocation/map token 可被复制后分别传给 consume-on-free/unmap，类型系统不能阻止 double free 或 double unmap |
| dma-buf 手工配对 legacy API | `os/StarryOS/kernel/src/file/dmabuf.rs:35-39` 在 `Drop` 中手工重建 `Layout` 并调用 `ax_dma::dealloc_coherent_pages`，分配端在 `:50-65` 调用 DMA32 变体 | owner 必须同步保存 size/align/zone 和正确释放函数，任何字段或分支不一致都会走错释放路径 |
| 同一设备链使用两套 DMA API | `drivers/gpu/rockchip-rga/src/buffer.rs` 使用 `dma_api::ContiguousArray`，但 Starry RGA/JPEG/NPU 共享的 dma-buf 由 `ax-dma::DMAInfo` 持有；对应 kernel feature 都依赖 `ax-dma` | 驱动内部与 OS 导入层使用不同 owner/token，零拷贝生命周期必须在两套模型之间人工维持 |
| legacy DMA 缺少设备/domain 约束 | `DMAInfo` 只有 `cpu_addr` 和 `bus_addr`；`ax-dma::phys_to_bus` 固定 identity；另一侧 `KlibDma::domain_id()` 固定 `legacy_global()` | DMA mask、设备 domain、direction 和 mapping owner 不随 legacy token 传递，不能表达不同设备或输入输出内存管理单元域 |
| cache 属性恢复失败被忽略 | `os/arceos/modules/axdma/src/dma.rs:154-162` 先把页面退回全局 allocator，再以 `let _ = self.update_flags(...)` 忽略恢复 cacheable 映射的错误 | 页面可能已经重新可分配，但仍保留 DMA uncached 属性；释放成功与映射状态没有原子所有权 |

### 1.6 不符合嵌入式实时约束

整改前的通用分配入口没有区分普通线程、中断请求与实时关键区，固定容量对象也可能经可增长容器进入堆分配，不能给出可验证的延迟和耗尽行为。

| 问题 | 整改前代码示例 | 直接影响 |
| ---- | -------------- | -------- |
| 页分配最坏路径无固定上界 | `os/arceos/modules/axalloc/src/buddy_slab.rs:207-226` 的一次失败最多触发 4 次虚拟文件系统回收和 4 次重新分配 | Buddy 操作本身即使有界，外层调用时间仍取决于 page cache 数量、锁竞争和回收结果 |
| API 不区分执行上下文 | `GlobalAlloc::alloc(Layout)`、`alloc_pages(..., UsageKind)` 及 Slab 扩容入口都没有中断请求/实时 状态或受限 capability | 同一 `Box/Vec/Arc` 调用在普通线程和实时区间走相同全局入口，审计只能依赖调用约定 |
| 固定对象仍由可增长容器持有 | `components/timer_list/src/lib.rs:32-33,71-72` 用 `BinaryHeap::push` 保存定时事件，`:113-121` 为回调分配 `Box`；`components/irq-framework/src/registry.rs:27-35,51-59` 用可增长 `Vec` 和 `Box<Action>` 注册中断请求 | timer/中断请求对象没有静态容量和耗尽行为；要求 no-fail 或硬实时 profile 时不能证明注册路径不扩容 |
| 缺少系统级基线 | `os/arceos/modules/axalloc` 没有 alloc/free 延迟、锁等待、碎片和镜像大小 benchmark；现有测试主要验证 allocator 算法功能 | 无法判断是否需要 per-CPU page cache，也无法验证整改后的 P99/max 是否退化 |

## 2. 整改方案

### 2.1 目标

目标按当前实现、新实现和直接收益建立一一对应关系；每项必须能够落到组件边界、可执行测试或可测量性能指标，不能只描述期望状态。

| 当前实现                                                                                                                            | 新实现                                                                                                                                                         | 收益                                                                          |
| ----------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------- |
| someboot::mem::ram 已使用 O(1) 指针 bump 和 no-op dealloc，但没有显式冻结状态；ranges-ext 的直接生产消费者只有 kernutil 和 someboot | 保留现有 bump 和 Ram frame provider，补 checked arithmetic 与交接后冻结；仅将 MemoryDescriptor 实际需要的固定容量 range 操作并入 kernutil::memory              | 启动和运行期边界可检查；在不复制无关通用 API 的前提下减少一个单用途 crate     |
| TLSF、Buddy-Slab、stub 和 backend feature 并存                                                                                      | 物理页固定使用 Buddy，仅保留 Normal/Dma32 两个 zone；小对象使用 per-CPU Slab，中断请求/no-fail 对象按实际消费者使用固定池                                             | 删除选择、兼容分支、空 profile 和多余页分类，减小代码体积，分配延迟可测且有界 |
| ax-alloc 位于 ArceOS 目录，ax-allocator、ax-dma 等提供其他分配入口                                                                  | ax-alloc 移到`memory/ax-alloc`，成为页、内核堆和 GlobalAlloc 的唯一公共入口                                                                                  | ArceOS、StarryOS、AxVM 共用同一所有权和统计模型，避免重复 allocator           |
| 现有 allocator 只有 UsageKind 用途统计，缺少资源来源视图                                                                            | 使用单一 AllocationSource × UsageKind 计数表派生所有 AllocatorStats 视图                                                                                      | 补齐诊断信息，同时避免每次分配重复更新两套计数器                              |
| 中断请求、bottom-half、实时 critical section 可触达通用分配路径                                                                            | 先审计具体目标；已纳入 hard-实时 profile 的中断请求/实时 路径只使用固定池或预留资源，禁止 GlobalAlloc、Slab 扩容、Buddy、回收、文件 IO 和阻塞                         | 形成可验收的实时路径约束，不把尚未审计的默认构建误报为 hard-实时                 |
| task、timer、wait、中断请求和驱动对象的生命周期不同，不能统一套用一种池                                                                 | 仅有固定 task 上限的 hard-实时 目标使用静态表；timer/wait 优先嵌入 owner；只为实际中断请求/no-fail 或实测热点实例化本地固定池                                       | 保持确定性且不增加无用 pool、静态内存和公共管理层                             |
| allocator 失败后可隐式回调 page cache 并多次重试                                                                                    | ax-alloc 失败立即返回 NoMemory；仅 starry-mm 在允许上下文显式回收并最多重试一次                                                                                | 底层不依赖虚拟文件系统，最坏执行时间和回收上下文可验证                                |
| RGA 核心已使用 dma-api，但 StarryOS`/dev/dma_heap` 及 RGA/JPEG/NPU feature 仍依赖 ax-dma/DMAInfo                                  | `DmaBufFile` 改持有 dma-api 的 DMA32 `CoherentArray<u8>`；保留 fd/mmap/import 的 `Arc` 生命周期，删除三个设备 feature 的 ax-dma 依赖                     | 收敛到一个 DMA 分配入口，不改变 Linux dma-buf 可见语义和加速器零拷贝路径      |
| ax-page-table-multiarch 与 page-table-generic 是能力不同的两套引擎，ax-page-table-entry 单独提供页表项；另有多套 AddrSpace 包装       | ax-page-table 统一公共契约，内部拆分 common/stage1/stage2/boot；stage2 由 feature 裁剪；ax-memory-set 统一虚拟内存区域事务                                            | Stage-1 热路径不携带 Stage-2 代码，同时统一地址、页表项、错误和 frame 契约       |
| Linux 虚拟内存策略与 StarryOS kernel、虚拟文件系统、task、signal 耦合，overcommit_memory=0 与实际总是接受的行为不一致                             | 独立 `no_std` 的 `starry-mm` 承载不依赖具体操作系统的 Linux 虚拟内存策略、记账和能力；具体虚拟文件系统与页表后端留在 kernel adapter；默认 Always overcommit，Strict 可选 | 避免再造通用 Linux mm 框架，同时使可复用策略和 Linux 可见行为一致             |
| 旧 crate、旧 feature、re-export、类型别名和转发模块长期兼容                                                                         | 迁移完成即删除旧源码、依赖、feature、shim 和非历史引用                                                                                                         | 依赖图单一，避免双实现和后续维护成本                                          |
| 缺少明确复杂度边界                                                                                                                  | 不引入非统一内存访问、复杂 zone、页迁移、compaction、多代最近最少使用、匿名页 swap、通用内存不足终止器、Maple Tree 或 读-复制-更新 虚拟内存区域                                                      | 保持嵌入式实现规模和内存占用，避免引入 Linux 级复杂度                         |

### 2.2 方案对比

对比基线为 2026-07-23（UTC+08:00）抓取时的官方文档和默认分支；`git ls-remote` 查询到 AtomGit `refs/heads/master` 为 Rust-Shyper `80b69c1b`，GitHub `refs/heads/main` 为 Asterinas `d971fee6`。表内源码链接固定到对应完整 commit，避免默认分支移动后内容失配。

| 操作系统/方案 | 物理内存组织 | 页与连续内存分配 | 内核堆、小对象与固定池 | 分配上下文与实时约束 | 页表与地址空间 | 内存压力与 overcommit | DMA、内存属性与隔离 |
| ------- | ------------ | ---------------- | ---------------------- | -------------------- | -------------- | ---------------------- | ------------------- |
| TGOSKits 当前整改方案 | 固件内存图由 kernutil/someboot 规范化；运行期只保留 Normal、Dma32 两个物理 zone | `ax-alloc` 统一入口；每个 zone 使用 Buddy，连续页由带 count/align/zone 的 `PageRequest` 请求；启动期 bump 在交接后冻结 | 固定 size-class per-CPU Slab；只有存在实际消费者时增加中断请求/no-fail 固定池，不建立通用 pool manager | 普通分配立即返回 `NoMemory`，不阻塞、不隐式回收；中断请求/实时 路径禁止 GlobalAlloc、Slab 扩容和 Buddy | `ax-page-table` 统一 boot/stage1/stage2 契约；`ax-memory-set` 负责事务；`ax-mm`、`starry-mm`、`axaddrspace` 分别承载 ArceOS、Linux、Guest 策略 | allocator 不回收；仅 `starry-mm` 在允许上下文做一次有界 clean-page 回收；默认 Always overcommit、Strict 可裁剪；无 swap/compaction/内存不足终止器 | `dma-api::DeviceDma` 持有 mask/domain/cache 约束和 move-only 资源获取即初始化 owner；Dma32 仅表示物理可达范围，不替代 map/unmap、cache 或输入输出内存管理单元语义 |
| [Zephyr](https://docs.zephyrproject.org/latest/kernel/memory_management/index.html) | 单 heap 或 [Shared Multi Heap](https://docs.zephyrproject.org/latest/kernel/memory_management/shared_multi_heap.html)；后者基于 `sys_multi_heap` 按内存属性选择独立 heap | 以 `sys_heap/k_heap` 的可变长块分配为主；没有统一的 Linux 式 zone Buddy 页层 | [`k_mem_slab`](https://docs.zephyrproject.org/latest/kernel/memory_management/slabs.html) 使用固定块、固定容量；`sys_heap` 处理一般小对象 | `sys_heap` 用编译期循环上限约束搜索；`k_heap` 增加同步和超时，中断服务程序路径不能等待 | 提供内存保护单元和 MMU、用户态内存域及按配置启用的虚拟内存能力；不提供 Linux 进程虚拟内存区域、写时复制和文件映射体系 | heap 分配失败或按超时等待；低层 heap 不执行页缓存回收、交换或内存不足策略 | 多 heap 可区分 DMA 与非缓存区；设备 DMA 映射、缓存和生命周期仍属于设备或板级支持包边界 |
| [FreeRTOS](https://www.freertos.org/Documentation/02-Kernel/02-Kernel-features/09-Memory-management/01-Memory-management) | 应用选择一个 heap 实现；`heap_5` 可把多个不连续 RAM 区域组成系统 heap | 无独立物理页分配器；`heap_1` 只分配，`heap_4/5` 使用可合并空闲块的可变长分配，`heap_2` 为旧实现 | task、queue、semaphore 等支持应用提供静态内存；不要求通用 Slab | 动态分配不执行内核回收或阻塞等待；确定性路径可关闭动态对象分配 | 内核不提供通用 MMU 页表、进程虚拟内存区域、写时复制或文件映射 | 分配失败返回空指针并可调用失败钩子；无回收、交换、超额承诺和内存不足终止器 | DMA 地址、缓存、一致性和专用内存由移植层、板级支持包或驱动处理，heap 不拥有设备 DMA 生命周期 |
| [RT-Thread](https://www.rt-thread.io/document/site/programming-manual/memory/memory/) | small memory、Slab 或 memheap 按构建选择；memheap 可管理多个独立或不连续内存区 | 实时操作系统核心不建立统一 zone/page 层；连续内存能力取决于选定 heap 和板级支持包 | Slab、memory pool 或静态对象承担固定大小分配；不同时保留所有通用 heap 后端 | 中断服务程序中禁止可能导致挂起的动态分配或释放；普通分配失败直接返回 | 实时操作系统核心不提供 Linux 级进程虚拟内存区域、写时复制和页缓存虚拟内存策略 | 底层分配器不回调文件系统，不执行交换、内存规整或通用内存不足恢复 | 特殊内存区由 memheap 或板级支持包划分，DMA 地址掩码、缓存和所有者由设备层维护 |
| [ThreadX](https://github.com/eclipse-threadx/rtos-docs-asciidoc/blob/main/rtos-docs/threadx/modules/ROOT/pages/chapter3.adoc) | 应用可为不同物理区域分别创建多个 byte pool 或 block pool | [byte pool](https://github.com/eclipse-threadx/rtos-docs-asciidoc/blob/main/rtos-docs/threadx/modules/ROOT/pages/chapter3.adoc#memory-byte-pools) 使用 first-fit 可变长块；没有独立物理页 Buddy | [block pool](https://github.com/eclipse-threadx/rtos-docs-asciidoc/blob/main/rtos-docs/threadx/modules/ROOT/pages/chapter3.adoc#memory-block-pools) 使用固定大小块和空闲链表，避免外部碎片 | block/byte pool 可让线程按 timeout 等待；byte pool 不允许从 中断服务程序 调用，block pool 路径更确定 | 内核不提供 Linux 进程虚拟内存区域、写时复制、匿名页或文件页策略 | 资源不足时立即失败或等待 timeout；没有内核级 reclaim、swap、overcommit 或内存不足终止器 | 可用独立 pool 放置高速或设备专用内存；DMA 地址和 cache 所有权仍由应用/驱动维护 |
| [Linux](https://docs.kernel.org/mm/physical_memory.html) | 非统一内存访问 node + DMA/DMA32/Normal 等 zone；启动期 memblock，运行期按 zone 管理 page frame | zone Buddy 提供高阶连续页，per-CPU pageset 缓存 order-0 页；高阶失败可进入 compaction/reclaim | SLUB/kmalloc 负责小对象，vmalloc 负责虚拟连续区，mempool 为关键对象保留最低容量 | GFP flags 表达可阻塞、可回收、NOIO/NOFS 和 atomic 上下文；PREEMPT_RT 仍不是最小硬实时 allocator | 完整虚拟内存区域、页表、写时复制、anonymous/file/shared mapping、THP、非统一内存访问和并发虚拟内存机制 | direct/background reclaim、writeback、swap、compaction、overcommit 和内存不足终止器组合运行 | 完整 DMA API、输入输出内存管理单元 domain、swiotlb/bounce、cache maintenance 和设备 mask 模型 |
| [Rust-Shyper](https://atomgit.com/iSureSystem/rust-shyper) | 平台 RAM 静态配置；全局 heap 初始使用 4 MiB 区域，首次耗尽后扩展到其余 RAM | 连续 [`PageFrame`](https://atomgit.com/iSureSystem/rust-shyper/blob/80b69c1b2d3bf2f65a5164433e7066add0311643/src/mm/page_frame.rs) 从全局 heap 零化分配，没有独立 zone/page 分配器 | [`LockedHeapWithRescue`](https://atomgit.com/iSureSystem/rust-shyper/blob/80b69c1b2d3bf2f65a5164433e7066add0311643/src/mm/heap.rs) Buddy 同时承担全局堆和页帧后备；无独立 Slab 或每 CPU 固定池 | 没有中断请求或实时分配能力；扩展后的 heap 再次耗尽时触发不可恢复错误 | 架构目录直接实现客户机第二阶段页表；`PageFrame` 资源获取即初始化所有者持有页表目录和中间页，不提供 Linux 进程虚拟内存 | 无通用回收、交换、超额承诺或内存不足恢复，主要依赖静态虚拟机资源配置 | 面向客户机 RAM、系统内存管理单元和设备直通；隔离目标集中在虚拟机监控器与客户机，而非 Linux dma-buf 生命周期 |
| [Asterinas](https://github.com/asterinas/asterinas) | OSTD 以 frame capability 和 metadata 表达物理页所有权；frame allocator 使用 Buddy 与 per-CPU pool/cache | [frame allocator](https://github.com/asterinas/asterinas/tree/d971fee615cd0952b3e1bdf31064183bd287bea0/osdk/deps/frame-allocator) 提供页和连续 frame，CPU-local cache 降低共享 Buddy 锁竞争 | [heap allocator](https://github.com/asterinas/asterinas/tree/d971fee615cd0952b3e1bdf31064183bd287bea0/osdk/deps/heap-allocator) 使用固定 size-class Slab 和 CPU-local object cache | CPU-local cache 优化并发延迟；设计目标是安全通用 OS，不承诺实时操作系统式禁止分配路径 | OSTD 提供 `VmSpace`、安全页表边界和 range cursor；kernel 在其上实现 VMAR、VMO、page cache、fault 和 Linux 应用程序二进制接口 | Linux 兼容策略位于 kernel service，OSTD frame allocator 不反向调用具体文件回收策略 | OSTD 将 IO/DMA、页表和 frame 的 `unsafe` 收敛到小型可信边界，以 capability/owner 表达资源生命周期 |

TGOSKits 行描述整改目标，不代表所有硬实时约束已经由当前默认构建实现；当前已落地项与后续工作以第 3 节实施方案和第 4 节验收条件为准。

### 2.3 整体架构

最终只保留五个逻辑层：

```text
1. Boot Memory
   someboot + kernutil::memory
                    |
                    v
2. Runtime Allocator
   ax-alloc                         唯一公共入口
   └── buddy-slab-allocator         内部 Buddy/Slab 算法
                    |
          +---------+---------+
          |                   |
          v                   v
3. PageTable Core       4. AddressSpace Core
   ax-page-table          ax-memory-set
          |                   |
          +---------+---------+
                    v
5. 操作系统策略
   ax-mm          starry-mm          axaddrspace
   ArceOS 虚拟内存      Linux 进程虚拟内存   客户机第二阶段地址转换
```

这五层是职责层级，不要求每个内部机制都成为 crate。

- `ax-memory-addr` 是共享基础类型，不增加逻辑层。
- `buddy-slab-allocator` 是 ax-alloc 的内部算法依赖，不是第二个公共 allocator。
- `dma-api`、`mmio-api` 是设备侧能力边界，不增加内存管理层。
- `ax-hal`、`axklib`、虚拟文件系统、task、signal 是 adapter，不是内存管理核心；`starry-vm` 只是用户指针访问 shim，不是 StarryOS kernel mm 或 Linux 进程虚拟内存。
- PageTable Core 和 AddressSpace Core 是并列公共机制；操作系统策略组合两者，不要求 ax-memory-set 直接依赖具体页表实现。

### 2.4 最终组件和唯一职责

最终组件按资源事实、公共机制、系统策略和设备能力划分；每个组件只维护一种主要不变量，算法实现不能绕过公共入口成为第二套接口。

| 分类             | 组件                 | 唯一职责                                                                                                | 状态                                                         |
| ---------------- | -------------------- | ------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------ |
| 基础类型         | ax-memory-addr       | host 物理/虚拟地址、通用地址 trait、地址范围和 checked arithmetic                                       | 保留                                                         |
| 客户机类型       | axvm-types           | `GuestPhysAddr`、虚拟机配置和虚拟化领域类型                                                            | 保留，不承载分配逻辑                                         |
| 启动内存         | kernutil::memory     | 内存描述、MemoryDescriptor 专属区间规则、reserved/free 计算                                             | 保留并吸收 ranges-ext 的最小必需逻辑                         |
| 启动分配         | someboot::mem::ram   | 调度器和中断请求启用前的现有 bump、Ram frame provider 及启动内存交接                                       | 保留现有实现，补 checked arithmetic 和冻结状态               |
| 运行时分配       | ax-alloc             | Normal/Dma32 页、内核堆、GlobalAlloc、单一统计和 per-CPU Slab 初始化                                   | 移到`memory/ax-alloc`                                      |
| 分配算法         | buddy-slab-allocator | Buddy 和 Slab 算法                                                                                      | 保留为 ax-alloc 内部依赖                                     |
| 页表公共核心     | ax-page-table        | common 契约及独立 stage1/stage2/boot 模块；三个执行模块分别 feature-gated                               | 统一 multiarch 与 generic，吸收 entry crate 后保留一个 crate |
| 地址空间公共核心 | ax-memory-set        | 虚拟内存区域查找、重叠检查、split/shrink/grow 和元数据/页表项事务                                                 | 保留并修正                                                   |
| ArceOS 策略      | ax-mm                | kernel direct map、iomap、基础 ArceOS 地址空间和平台 glue                                               | 保留                                                         |
| Linux 虚拟内存策略 | starry-mm          | 写时复制引用规则、共享页 owner、常驻内存集大小/虚拟内存大小、overcommit/commit、fault outcome、有界回收和文件/页来源 capability | 新建；保持薄策略层，strict commit 可选                       |
| 客户机虚拟机策略 | axaddrspace          | `GuestPhysAddr`、第二阶段标志、嵌套页表、虚拟机标识符和客户机地址空间                                    | 保留                                                         |
| 用户访问边界     | starry-vm            | 用户指针检查和 copy 接口；不是 kernel/src/mm 的别名，也不拥有进程地址空间                               | 保留为 shim                                                  |
| DMA 能力         | dma-api              | coherent/streaming DMA 生命周期和 typed ownership                                                       | 保留为唯一驱动 DMA API                                       |
| MMIO 能力        | mmio-api             | 设备寄存器映射能力                                                                                      | 保留                                                         |
| 平台适配         | axklib、ax-hal       | DMA、MMIO、cache、地址转换后备缓冲区和地址转换 glue                                                                   | 保留为 adapter                                               |
| 运行时接线       | ax-runtime           | 接收启动内存、初始化 ax-alloc、初始化 CPU-local Slab 和安装平台 adapter                                 | 保留为启动/接线代码                                          |

最终公共内存核心只有 `ax-memory-addr`、`ax-alloc`、`ax-page-table` 和 `ax-memory-set`。`ax-mm`、`starry-mm` 和 `axaddrspace` 是三个并列消费者，不再增加公共包装层。

### 2.5 依赖规则

依赖必须从系统策略指向公共机制，再指向基础类型或算法实现；底层组件不能通过回调、全局注册或转发 facade 反向进入上层策略。

| 组件                 | 允许的主线依赖或运行时能力                                                                                      |
| -------------------- | --------------------------------------------------------------------------------------------------------------- |
| kernutil::memory     | ax-memory-addr                                                                                                  |
| someboot::mem        | kernutil::memory、ax-memory-addr、ax-page-table boot adapter                                                    |
| buddy-slab-allocator | ax-kspin；算法不依赖 ax-alloc 或 OS policy                                                                      |
| ax-alloc             | ax-memory-addr、buddy-slab-allocator、ax-plat memory、ax-percpu、ax-kspin、ax-kernel-guard                      |
| ax-page-table        | ax-memory-addr；其余仅为固定容量容器、架构位定义和错误支持                                                       |
| ax-memory-set        | ax-memory-addr；可选 ax-errno 仅用于 adapter 错误转换                                                            |
| ax-mm                | ax-memory-addr、ax-alloc、ax-page-table、ax-memory-set                                                          |
| starry-mm            | ax-memory-addr、ax-page-table 的 PageSize、ax-errno；文件和页来源通过 capability 注入                          |
| axaddrspace          | ax-memory-addr、axvm-types、ax-memory-set；页分配和页表通过 NestedPageTableOps 注入                             |
| dma-api              | ax-kspin、mbarrier 和架构 cache helper；不直接依赖 ax-alloc                                                     |
| mmio-api             | 独立 MMIO 领域类型和错误；不直接依赖 ax-alloc                                                                   |

- 操作系统、虚拟机、驱动和 adapter 不得直接依赖 buddy-slab-allocator。
- ax-alloc 不依赖页表、地址空间、DMA、虚拟文件系统、StarryOS 或 AxVM。
- ax-page-table 不依赖 ax-alloc，只定义 `PageFrameProvider`，由 boot 或 OS adapter 注入。
- ax-memory-set 不依赖具体 OS 和具体页表类型，只依赖 `MappingBackend` 能力。
- dma-api 和 mmio-api 不依赖 ax-alloc、ax-mm 或具体驱动。
- axklib/运行时 glue 同时依赖能力 API 和实现组件，完成 DMA/MMIO 接线。
- starry-mm 不依赖 StarryOS kernel、syscall dispatch、具体虚拟文件系统、task 或 signal 实现。
- 底层 crate 不导入或回调上层 OS 实现；依赖图不得成环。
- 不重导出内部 backend 类型，不允许通过 facade 保留被删除的 crate 名称。

### 2.6 分配器设计

#### 2.6.1 ax-alloc 公共边界

ax-alloc 只提供：

- 全局 allocator 的一次性初始化和追加 RAM section。
- `PageRequest` 对应的物理页分配和释放。
- `GlobalPage` 等具有明确所有权的页封装。
- `PageFrameProvider`、task stack 和外部 split alloc/free trait 所需的低层 raw page pair；只允许 adapter 使用，调用方必须保存由原请求派生的 `PageRelease { count, zone }` 和 `UsageKind` 并成对释放，对齐参数不进入释放契约。
- 内核 Rust `GlobalAlloc`。
- per-CPU Slab 的 CPU 启动初始化。
- 单一 AllocatorStats；从同一底层计数派生按来源和按 UsageKind 的只读视图。

ax-alloc 不提供：

- allocator backend 选择和 backend 类型重导出。
- 虚拟内存区域、页表项、fault、写时复制、commit accounting。
- DMA map/unmap、输入输出内存管理单元、cache maintenance 或 MMIO 映射。
- 虚拟文件系统/page-cache reclaim callback、阻塞等待或内部重试。
- 没有消费者的通用 allocator trait。

页分配接口只描述物理约束：

```rust
pub enum MemoryZone {
    Normal,
    Dma32,
}

pub struct PageRequest {
    pub count: usize,
    pub align: usize, // physical-address alignment in bytes
    pub zone: MemoryZone,
}
```

所有 ax-alloc 分配失败立即返回 typed `NoMemory`，不携带 `ReclaimAllowed` 等 OS 压力策略。

MemoryZone 表示物理可达范围，UsageKind 表示用途。PageTable、fault、exit 和 reclaim 都是 UsageKind 或调用场景，不再成为物理页分类：

```rust
pub fn alloc_pages(request: PageRequest, usage: UsageKind) -> AllocResult<GlobalPage>;
```

当前没有能够证明需要保证前向进展的 reserve 消费者，因此不预置 EmergencyReserve、capability、feature 或静态页栈。若后续出现明确消费者，必须先给出容量、耗尽行为和所有权测试，再作为独立变更引入，不能成为普通 `alloc_pages` 的 内存不足 fallback。

AllocatorStats 使用一个按 `AllocationSource × UsageKind` 索引的底层计数表；一次 alloc/free 只更新一个 bucket。`source`、`usage`、总量和 procfs/sysinfo 输出均从同一快照派生，不在热路径维护两套统计。实现可以为每个底层 bucket 使用一个 Relaxed 原子计数，但不得再按 source、usage 和 total 分别重复写计数，也不得用一把统计全局锁串行化 per-CPU Slab 命中路径。

原 AllocatorOps API 迁移如下：

| 原接口                                          | 新接口或处理                                                                                             |
| ----------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| `name`                                        | 删除动态 backend 选择；如启动日志需要，使用固定 backend 常量或`AllocatorInfo`                          |
| `init`、`add_memory`                        | 保留具体的`global_init`、`global_add_memory` 生命周期接口                                            |
| `alloc`、`dealloc`                          | 保留具体`GlobalAllocator::alloc/dealloc`，不为更名增加转发接口；`GlobalAlloc` 委托该实现              |
| `alloc_pages`                                 | 改为具体`alloc_pages(PageRequest, UsageKind)`                                                          |
| `alloc_dma32_pages`                           | 合并为`PageRequest { zone: MemoryZone::Dma32, .. }`                                                    |
| `alloc_pages_at`                              | 无消费者，删除且不提供替代接口                                                                           |
| `dealloc_pages`                               | 保留具体释放接口；必须恢复原 MemoryZone 和 UsageKind，资源获取即初始化页对象保存请求元数据                         |
| `used_bytes/pages`、`available_bytes/pages` | 保留为 allocator backend 容量/占用查询；用途归因由`AllocatorStats`提供                               |
| `usages`、`UsageKind`                       | `UsageKind` 保留；`usages()` 改为 `AllocatorStats::usage` 视图，继续服务 procfs、sysinfo 和诊断    |
| `DefaultByteAllocator`                        | 删除公共别名；Slab 类型只在 ax-alloc 内部使用                                                            |

#### 2.6.2 启动分配和交接

启动期只使用无堆、固定容量且可冻结的内存路径，冻结点同时结束早期线性分配并把剩余可用区间交给运行时分配器。

```text
firmware/扁平设备树/UEFI memory map
        -> kernutil::memory normalize
        -> 排除 kernel image、reserved、MMIO
        -> someboot bump allocator
        -> 分配 boot page table、percpu、stack、early object
        -> 生成最终 free RAM sections
        -> ax-alloc init/add_region
        -> 初始化引导处理器 Slab
        -> 将 someboot ram allocator 标记为 Frozen
```

- 保留现有基于 RAM_CURRENT 的 O(1) bump 和 Ram no-free frame provider。
- 最终直接映射建立前，先以全部 CPU 区域的精确布局大小、经校验的设备树二进制对象大小及 2 MiB 页表/对齐元数据余量计算启动工作集，再从固件内存图中选择满足该容量和架构启动地址约束、物理地址最低的 Free range 作为单一 early arena；不能只按最大容量选择尚不可达的高地址 RAM，也不能选择无法容纳启动对象的最低小段。选择不依赖描述符顺序，也不跨物理 hole 拼接。x86_64 通过 `ArchTrait` 将完整 early arena 限制在 4 GiB 以下，以满足应用处理器 trampoline 的 32 位 CR3 装载约束，高端 Free range 仍交给运行时 Buddy；后续 bump 继续使用 checked arithmetic，空间不足立即失败。
- 地址对齐和 `start + size` 使用 checked arithmetic。
- 仅在引导处理器、单核、调度器和中断请求启用前使用。
- 引导处理器在冻结前通过现有 someboot 多核 prealloc 为全部配置 CPU 分配 PerCpuMeta、secondary stack 和 per-CPU data，并写入启动内存描述。
- secondary CPU 不调用 bump；进入 ax-runtime 后只绑定预分配区域并初始化本 CPU Slab。
- 延迟启动的 CPU 必须使用已有预分配 slot；不支持的运行期 CPU hotplug 返回 Unsupported，不隐式重新打开 bump。
- 已用区间必须写回启动内存描述。
- 新增 Active/Frozen 状态；交接后调用 someboot ram allocator 必须返回错误或在 debug 构建触发断言。

#### 2.6.3 物理页和储备

物理页请求通过地址区域、页数、对齐和用途表达来源；当前没有已证明需要保证前向进展的消费者，因此不预置紧急储备页。

| MemoryZone/来源       | 使用者                              | 失败行为                                 | 外部恢复                          |
| --------------------- | ----------------------------------- | ---------------------------------------- | --------------------------------- |
| Normal                | 内核、页表、用户页、Guest RAM       | 立即返回 NoMemory                        | 仅 starry-mm 可显式回收并重试一次 |
| Dma32                 | 有低地址约束的 DMA adapter          | 立即返回 NoMemory                        | 无                                |

- Buddy 使用固定 MAX_ORDER，支持有限数量的不连续 section。
- 页元数据存放在 section 前缀。
- 不实现非统一内存访问、页迁移和通用 compaction。
- 设备连续预留内存由启动内存描述管理，不建立第二个通用页 allocator。

#### 2.6.4 小对象和实时路径

普通小对象由每 CPU Slab 加速，实时或中断路径只能使用已经审计并预分配的固定容量资源，不能在耗尽后回退到通用分配器。

- 普通小对象使用固定 size-class 的 per-CPU Slab。
- Slab 命中路径不扫描全堆；扩容只允许在启动或普通线程上下文。
- 跨 CPU 释放进入以已释放对象自身保存 next 指针的无锁 remote-free 栈；容量受 Slab 对象总数约束，由 owner CPU 批量 drain，不另设队列存储。
- 单核构建仍复用同一 Slab 实现，owner CPU 固定为 0，不引入另一套 allocator；无需为裁剪少量 remote-free 字段增加双实现。
- 固定池只用于中断请求/no-fail 路径或基准确认的热点，不按对象名称批量创建通用 pool。
- 消费者审计可以评估 IrqEventPool、DmaDescriptorPool 和按驱动配置的 IoRequestPool；未被目标板使用的池不实例化。
- 只有明确配置固定 task 上限的 embedded-hard-rt 目标才使用编译期静态 task slot/stack；embedded-default 和 StarryOS 动态 task 使用 Slab，不强制固定上限。
- timer 和 wait node 优先嵌入其 owner；无法嵌入且确有动态生命周期时使用 Slab。
- 已审计并启用固定池的中断请求路径只访问 irq-safe 固定池；池耗尽立即返回错误，不回退到 GlobalAlloc、Slab 扩容或 Buddy。
- 仅对实际实例化的池记录当前使用量、峰值和耗尽次数。

#### 2.6.5 系统配置组合

系统配置只组合已经存在的分配、回收和固定池能力，不为尚未实现或没有消费者的策略创建空 feature 与常驻静态状态。

| 系统配置         | 分配方式                                                      | 回收                   |
| ---------------- | ------------------------------------------------------------- | ---------------------- |
| embedded-default | Buddy + Slab + 目标板实际使用的固定池                         | 不启用                 |
| embedded-hard-rt | 实时 路径只用固定池；非 实时 控制面使用 Buddy + Slab              | 不启用                 |
| starry           | Buddy + per-CPU Slab                                             | starry-mm 显式有界回收 |
| hypervisor       | Buddy + per-CPU Slab；Guest RAM 预留或显式分配                | 默认不启用             |

这些是操作系统与板级支持包的配置组合，不是 ax-alloc feature 名称；当前没有经过消费者审计的 embedded-hard-rt 构建项时，不创建同名空 feature。后续引入具体组合时，未启用的能力和静态状态不得进入链接结果。

#### 2.6.6 StarryOS 有界回收

内存压力处理位于 StarryOS 缺页策略外层，分配器本身立即返回 `NoMemory`；允许回收的路径最多执行一次干净页回收和一次重新分配。

```text
ax-alloc returns NoMemory
        -> starry-mm 检查当前上下文允许回收
        -> 通过 page-cache adapter 回收干净文件页
        -> 异步触发脏页回写，不等待 IO
        -> 最多重试一次 ax-alloc
        -> 失败返回 MemoryError::NoMemory
```

- ax-alloc 不注册 `PageReclaimFn`，不回调虚拟文件系统或 page cache。
- 不在中断请求、实时 critical section 或持不可睡眠锁时回收。
- 不执行匿名页 swap、同步写回、全系统虚拟内存区域扫描、无上界重试或通用 内存不足 victim 评分。

### 2.7 页表设计

ax-page-table 是唯一页表 crate，但不是单一巨型实现。统一工作先建立共同契约；stage1 使用独立热路径，stage2 和 boot 通过不同 feature/API 视图复用无 OS 策略的 flexible 引擎：

```text
ax-page-table
├── common     Address/PteConfig/PagingError/PageFrameProvider
├── entry      architecture 页表项 + memory attribute index
├── stage1     4K、cursor、protect、copy、地址转换后备缓冲区 batching     [feature = stage1]
├── flexible   可变层级/页大小的共享实现，不直接公开
├── stage2     Guest 页表 API 视图                        [feature = stage2]
└── boot       no-free provider 启动 API 视图              [feature = boot]
```

| 能力                     | 边界                                                                        |
| ------------------------ | --------------------------------------------------------------------------- |
| 公共类型                 | common，只依赖 ax-memory-addr                                               |
| Frame allocation         | PageFrameProvider；不依赖 ax-alloc                                          |
| Stage-1                  | stage1；ArceOS/StarryOS 启用 feature                                        |
| Stage-2                  | stage2；仅 Axvisor/AxVM 启用 feature                                        |
| Boot                     | boot；仅 someboot/somehal 启用 feature                                      |
| 地址转换后备缓冲区 invalidation         | TlbInvalidator，由架构 adapter 选择 local、hardware broadcast 或 remote 处理器间中断 |
| AArch64 memory attribute | entry::aarch64 的唯一 MemAttrLayout；平台写 MAIR，页表项只引用 index          |
| 输入输出内存管理单元 page table         | 不属于 ax-page-table；由输入输出内存管理单元 driver/domain adapter 管理                   |

- walker 不进行堆分配。
- 页表热路径使用泛型静态分发。
- 遍历次数只与固定页表层级和请求页数相关。
- 地址转换后备缓冲区待刷新地址使用固定容量数组，溢出时执行全量 flush。
- boot adapter 使用现有 `someboot::mem::ram::Ram` no-free provider，并保留 `Ram` 原名。
- stage1 不依赖 flexible；stage2 和 boot 可以复用 flexible 实现，但不得互相调用或暴露对方的领域类型，也不通过布尔参数混合 Guest 与 boot 语义。
- stage1、stage2 和 boot 关闭后，对应执行代码、页表项类型和静态状态不得进入链接结果。
- ax-page-table 不知道 frame 来自 someboot Ram 还是 Normal Buddy。
- 不支持的架构、stage 或页大小在编译期不可见或返回 typed `Unsupported`，不得 panic。

多核 地址转换后备缓冲区规则：

- AArch64 优先使用 Inner Shareable hardware broadcast，不再叠加逐 CPU 处理器间中断 flush。
- 没有硬件 broadcast 的架构由 ax-hal 提供同步 remote 处理器间中断 shootdown；单核只执行 local flush。
- primary 处理器间中断 ready 之前只执行本地 flush；primary 发布 ready 后显式启用 remote shootdown。secondary CPU 必须在发布 ready 前安装 kernel root 并执行 full local flush，不能向未就绪 CPU 发送同步 处理器间中断。
- 多核 构建必须在编译或初始化时确认存在 broadcast 或 处理器间中断 实现，禁止静默退化为仅本 CPU flush。
- hard 实时 critical section 不执行需要跨 CPU 同步的页表修改；普通线程路径允许有界 batching 和同步完成。

AArch64 MAIR 规则：

- `MemAttrLayout` 是 Device、NormalCached、NormalUncached 等 index 和 MAIR value 的唯一来源。
- axcpu、someboot 和 somehal 使用同一 layout 写 MAIR_EL1/MAIR_EL2。
- Stage-1/boot 页表项 adapter 只编码 layout 中的 index，不自行定义另一组常量。

统一后的能力必须同时覆盖：

- ax-page-table-multiarch 的 ax-memory-addr 地址类型、4K Stage-1 路径、cursor、protect、copy 和地址转换后备缓冲区 batching；
- page-table-generic 的可变层级、可变页大小、Stage-2 和独立 PageTableEntry/PteConfig 能力；
- ax-page-table-entry 的 MappingFlags、GenericPTE 和各架构页表项位编码。

先定义统一 Address、PteConfig、PagingError、PageFrameProvider、TlbInvalidator 和 MemAttrLayout 契约，再为两套旧引擎建立行为对照测试。迁移期间不允许把一种引擎不具备的能力静默降级。

### 2.8 地址空间设计

#### 2.8.1 ax-memory-set

ax-memory-set 使用按起始地址排序的紧凑 `Vec<MemoryArea>`：find/overlap 使用二分查找，map/unmap/protect 在 commit 前通过 `try_reserve` 预留最终虚拟内存区域容量，提交后只移动已有元素。该实现避免不可预留的树节点分配和额外虚拟内存区域 allocator，同时保持 page-fault 查找为 O(log n)；虚拟内存区域修改为 O(n)，后续只有基准证明其成为瓶颈时才替换索引。ax-memory-set 只负责：

- checked range validation；
- 虚拟内存区域 find/overlap；
- split/shrink/grow；
- map/unmap/protect；
- clear/drop 规则；
- backend typed error；
- 虚拟内存区域元数据和 backend/页表项的事务顺序。

事务规则：

- map、unmap、protect 先完成整段范围检查，只克隆相交虚拟内存区域并准备 remove/insert 元数据差量，再完成 backend validation 和所有可失败资源预留；此阶段不得修改 live 页表项/虚拟内存区域，也不得克隆整个 `MemorySet`。
- map plan 使用 `MapPrecondition::Vacant` 或 `Replacing` 明确区分全新映射与同事务覆盖；重叠替换的 new map prepare 可以观察并保存即将由前序 unmap 删除的旧页表项，普通 map 发现旧页表项必须返回 `AlreadyExists`。
- commit 执行已经 prepare 的 backend/页表项操作；全部 backend 成功后发布已预留容量的虚拟内存区域元数据，再 finalize 不再需要的旧资源。
- 跨多个虚拟内存区域的操作必须 all-or-nothing；不能出现前几个虚拟内存区域已删除页表项、后续失败而元数据仍描述原映射的状态。
- backend 应将资源申请和可预检失败放入 prepare。当前页表操作仍可能返回错误，因此 commit 返回 typed result，并由该 backend 在返回错误前恢复当前操作；MemorySet 逆序回滚此前已经提交的操作。
- map 成功后插入虚拟内存区域；unmap 成功后删除或拆分全部目标虚拟内存区域；protect 成功后统一提交页表项、实际 flags 和 reported flags。
- `MappingBackend` prepare 返回 typed `MappingResult<Plan>`，至少区分参数错误、冲突、内存不足和状态损坏；具体页表/虚拟文件系统错误由 OS adapter 转换，不再使用 bool 丢失失败类别。
- `map_metadata` 只允许专用事务已经安装页表项并取得对应 owner 后发布虚拟内存区域。Starry 写时复制 fork 使用该入口，发布失败时先由写时复制 backend 撤销当前 child 页表项、引用和常驻内存集大小；普通 map 禁止绕过 backend 事务。
- 可恢复错误不得使用 panic、expect、retain closure 中的 unwrap 或提前修改元数据。

#### 2.8.2 操作系统策略

ArceOS、StarryOS 与 Axvisor 分别拥有内核虚拟内存、Linux 进程虚拟内存和客户机地址空间策略，三者并列消费公共页表与区间事务机制。

| 组件        | 保留职责                                                                                                      | 禁止职责                                                                    |
| ----------- | ------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------- |
| ax-mm       | kernel direct map、iomap、linear/allocated mapping、内核 root 和地址转换后备缓冲区 glue                                     | Linux 写时复制与文件虚拟内存、客户机第二阶段地址转换                              |
| starry-mm   | 与操作系统实现无关的 Linux 虚拟内存策略、写时复制引用、共享页 owner、常驻内存集大小/虚拟内存大小、commit、fault outcome、回收编排和 capability 数据 | 具体页表项 cursor、虚拟文件系统 cache/listener、memfd、task/signal 和 syscall dispatch |
| axaddrspace | GuestPhysAddr、Stage-2 flags、嵌套页表/VMID、Guest region                                                          | Linux 虚拟内存区域和 host kernel iomap                                              |
| starry-vm   | 用户指针检查和 copy                                                                                           | 虚拟内存区域/页表项所有权                                                              |

不建立包含所有 OS 语义的巨型 `AddrSpace` trait。

#### 2.8.3 starry-mm 边界

starry-mm 是薄的 `no_std` 策略 crate，直接依赖：

- ax-memory-addr；
- ax-page-table 的共享 PageSize；
- ax-errno；
- 必要的同步原语。

物理页分配通过 `PageSource` capability 注入，不为了调用 ax-alloc 而增加直接依赖。

starry-mm 对外使用小型 capability：

- `VmFile`/`PageSource`：文件页读取和物理页来源；
- page-cache eviction adapter：干净页回收；
- `FaultOutcome`：由 kernel 转换为 resume、signal 或 errno。

StarryOS kernel 保留 `AddrSpace`、具体 PageTableCursor backend、虚拟文件系统/page-cache/memfd listener adapter、syscall 应用程序二进制接口、task/process 挂接和 signal/errno 转换。不得在 kernel 重复实现已进入 starry-mm 的记账、写时复制计数、overcommit 或 fault/reclaim policy。

overcommit policy：

| 模式   | 构建配置                 | 行为                                                                                     | Linux 可见值            |
| ------ | ------------------------ | ---------------------------------------------------------------------------------------- | ----------------------- |
| Always | starry 默认              | 维护全局 commit 计数但不按 CommitLimit 拒绝；仍检查地址范围、RLIMIT_AS 和元数据资源      | `overcommit_memory=1` |
| Strict | `starry-strict-commit` | 无 swap 条件下按平台 CommitLimit 严格预留，超限返回 ENOMEM                               | `overcommit_memory=2` |

- 不实现 Linux heuristic mode 0，未实现时不得继续向 `/proc/sys/vm/overcommit_memory` 报告 0。
- RLIMIT_AS 始终实现，默认值为 unlimited；mmap、brk、mremap 和 fork 扩大虚拟内存大小前检查。
- AddrSpace 在现有锁内只维护私有可写匿名/写时复制和私有可写文件映射的 committed bytes；只读私有匿名映射不计费。共享匿名内存由 allocator-owned `SharedPages` 在创建时持有一次全局资源获取即初始化 charge，最后一个 `Arc` 释放时归还，fork 和重复虚拟内存区域不重复计费。
- 全局 Committed_AS 使用单一原子增减，在虚拟内存区域事务或共享匿名 owner 创建/销毁时更新，不进入 page fault 热路径；普通文件、linear/device 和 imported `SharedPages` 不计费。
- Always 模式只用于准确报告 Committed_AS，不因 commit limit 拒绝请求。
- Strict 模式复用同一计数，在扩大私有可写匿名/写时复制、私有可写文件承诺或创建 shared-anonymous owner 前执行原子 reserve；失败不修改虚拟内存区域或泄漏已分配页。
- unmap、exec 和进程退出归还计数；fork/写时复制、mremap 和失败路径必须事务回滚。
- `/proc/meminfo` 的 CommitLimit/Committed_AS 和 `/proc/sys/vm/overcommit_memory` 必须反映实际配置，不保留固定占位值。
- 当前写时复制的 u8 计数上限检查保留为回归条件，但实现改为锁内 u32 或合适的原子计数。
- clone 前先检查全部目标页和计数容量，再提交引用计数、父页表项、子页表项和 accounting；任一步失败必须按逆序回滚本次已提交页面。
- 写时复制 clone 已安装 child 页表项后只发布虚拟内存区域 metadata，不重复执行普通 map；未发布 child 的失败清理必须同时覆盖当前虚拟内存区域和此前已发布虚拟内存区域。

### 2.9 DMA 和 MMIO 设计

#### 2.9.1 DMA

DMA 边界负责设备可达性、地址转换域、缓存所有权和单次释放令牌；物理页仍由 `ax-alloc` 提供，驱动只持有 `dma-api` 所有者。

- dma-api 是驱动唯一可见 DMA API。
- `DeviceDma` 携带 mask、alignment、boundary 和 domain。
- identity 平台使用 identity adapter；输入输出内存管理单元平台实现 domain-specific map/unmap。
- 输入输出内存管理单元 controller 和 I/O page table 归属 `drivers/iommu/<controller>` 及其 domain adapter，不进入 CPU ax-page-table::stage2。
- 输入输出内存管理单元 driver 可以复用 ax-memory-addr、PageFrameProvider 和无 OS 语义的低层 helper，但独立定义 IOPTE 格式、输入输出虚拟地址、domain 生命周期、输入输出地址转换后备缓冲区 invalidation 和 device attach/detach。
- dma-api 只持有 `DmaDomain` capability 并调用 map/unmap；axklib/OS glue 为输入输出内存管理单元页表注入 Normal 页，用途记为 UsageKind::PageTable。
- 当前输入输出内存管理单元-bypass 平台继续使用 identity adapter；未实现的输入输出内存管理单元 controller 返回 typed Unsupported，不伪装成 identity 映射。
- 保留现有不可复制且实现 Drop 的 coherent、contiguous、streaming 和 CpuDmaBuffer 资源获取即初始化 wrapper，驱动只持有这些 owner。
- StarryOS `/dev/dma_heap` 使用 `axklib::dma::device_with_mask(u32::MAX as u64)` 和 `coherent_array_zero_with_align::<u8>(size, PAGE_SIZE_4K)` 创建 DMA32 `CoherentArray<u8>`；`DmaBufFile` 的 `Arc` 同时作为 fd、mmap 和设备导入的唯一 lifetime retainer，不再手工保存或释放 DMAInfo。
- 驱动自有 buffer 直接持有 dma-api owner；OS 导入的 dma-buf 只向驱动传递 device address、长度和操作期 owner，驱动不得释放外部 buffer。RGA 现有 `RgaBufferBacking::Owned/Imported` 边界保留，不再增加包装 crate。
- 外部导入必须在提交前验证 mask、长度和访问范围；coherent heap 无需显式 cache sync，非 coherent 导入必须通过 dma-api ownership transition 执行 cache maintenance。
- DmaAllocHandle 和 DmaMapHandle 是 backend token，不是新的资源获取即初始化 wrapper；删除其 Copy/Clone，dealloc/unmap 按值消费 token，查询方法只借用。
- 零长度 DMA allocation 在进入 backend 前返回 `DmaError::ZeroSizedBuffer`；backend 不接收也不释放零长度 token。
- legacy DMAInfo 随 ax-dma 删除，不迁入 dma-api。
- 整改后新增的 DMA adapter 通过 `PageRequest { zone: MemoryZone::Dma32, .. }` 获取页；当前 `alloc_dma32_pages` 在调用方迁移完成后删除。
- descriptor/ring 在 probe 或启动阶段预分配；中断请求 completion 只归还预分配 descriptor。
- bounce buffer 的分配和 copy-in 在 submit 前完成，copy-out 和释放在非中断请求路径完成。
- scatter-gather 作为独立可选 feature。
- 本地 DMA 描述符优先派生 `bytemuck::Pod + Zeroable`，由编译器检查固定布局、padding 和零值有效性；只有无法派生的外部硬件记录 wrapper 或特殊布局允许经过逐类型审查的显式 `DmaPod` 实现，并必须附带安全契约和布局断言。

#### 2.9.2 MMIO

MMIO 边界只表示设备寄存器窗口的映射与易失性访问，不拥有窗口背后的物理存储，也不形成第二套 DMA 或页分配器。

- mmio-api 只描述设备寄存器映射和 typed MMIO access。
- axklib/ax-mm adapter 建立 CPU 虚拟地址到设备物理区的 device mapping。
- MMIO 物理区不进入 Buddy，不获得 RAM 所有权，unmap 只释放虚拟地址/页表项。
- DMA RAM 不通过 MMIO API 或 iomap allocator 管理。

### 2.10 所有权和并发约束

每类资源必须能够指出唯一所有者、释放入口和并发保护方式；不可复制令牌、地址空间事务和锁顺序共同阻止重复释放与半提交状态。

| 资源                | 唯一所有者/状态来源                                                              | 约束                                                                                                                                   |
| ------------------- | -------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| 普通页和 DMA32 页   | buddy-slab-allocator::Buddy，公共入口为 ax-alloc                                 | 每页只属于 free list、Slab 或一个 live owner                                                                                           |
| Slab backing page   | buddy-slab-allocator::Slab                                                       | 空 Slab 返回原 Buddy section                                                                                                           |
| Starry 匿名/写时复制页  | Starry kernel backend 持有具体页表项/页，starry-mm 提供引用规则和 PageSource owner | 引用归零后由注入的 PageSource 释放                                                                                                     |
| Guest RAM           | axaddrspace/AxVM                                                                 | 虚拟机销毁后通过 ax-alloc 释放                                                                                                         |
| DMA buffer          | dma-api 资源获取即初始化 owner/domain；StarryOS dma-buf 由`Arc` 保留该 owner               | 自有 buffer 不可复制；导入方只借用 address/length 并持有 operation-lifetime retainer；底层 token move-only；map/unmap、alloc/free 成对 |
| MMIO 区域           | 平台设备资源                                                                     | 只管理虚拟地址/页表项，不释放设备物理区                                                                                                        |

锁顺序固定为：

1. AddrSpace/虚拟内存区域 lock；
2. page-table cursor；
3. 短时 PageAllocator/Reserve lock。

- Buddy 使用一个 `SpinNoIrq` 和固定 MAX_ORDER；锁内不执行回收或 callback。
- per-CPU Slab 使用 CPU-local lock；remote free 使用固定队列或链表。
- page cache 回收在 allocator 锁外执行，不持 AddrSpace 广锁等待文件 IO。
- reclaim、虚拟文件系统、signal、task wakeup 和驱动 callback 不得在 allocator、reserve 或页表内部锁中执行。

### 2.11 上下文约束

允许的内存操作由执行上下文决定，普通线程、中断请求、实时关键区和缺页路径不能共享没有时延边界的隐式回收或扩容行为。

| 上下文               | 允许                            | 禁止                                |
| -------------------- | ------------------------------- | ----------------------------------- |
| 启动期               | bump、Buddy 初始化、静态池构建  | 调度等待、运行期回收                |
| 中断请求                  | irq-safe 固定池、预分配 ring    | GlobalAlloc、Slab 扩容、Buddy、回收 |
| 未线程化 bottom-half | 固定池                          | 阻塞和通用分配                      |
| 实时 critical section  | 固定池、预留资源                | 回收、文件 IO、通用堆扩容           |
| 普通内核线程         | Buddy、Slab 扩容                | 持不可睡眠锁回收                    |
| Starry 用户缺页      | starry-mm；允许时在外层有界回收 | 中断请求锁内 fault handling             |

## 3. 实施方案

### 3.1 组件处理清单

组件处理以保留、收窄、合并、新建或删除五类动作落实；旧名称、兼容转发和无消费者抽象不作为迁移后的长期边界保留。

| 组件                    | 操作                 | 具体结果                                                                                                              |
| ----------------------- | -------------------- | --------------------------------------------------------------------------------------------------------------------- |
| ax-memory-addr          | 保留                 | 成为唯一 host 地址和范围类型来源                                                                                      |
| axvm-types              | 保留并收窄           | 只保留 Guest 地址、配置和虚拟化领域类型，不实现 allocator                                                             |
| kernutil::memory        | 保留并合并           | 接收 MemoryDescriptor 实际使用的固定容量 range 操作，不复制无关通用 API                                               |
| ranges-ext              | 删除                 | 当前直接生产消费者只有 kernutil 和 someboot；迁移 MemoryDescriptor 所需最小逻辑后删除，不以“存在重复算法”为删除理由 |
| someboot::mem::ram      | 保留并增强           | 保留现有 O(1) bump 和 Ram no-free provider，补 checked arithmetic、Active/Frozen 状态和内存交接检查                   |
| ax-alloc                | 移动并收窄           | `os/arceos/modules/axalloc` 移到 `memory/ax-alloc`；成为唯一公共 allocator                                        |
| buddy-slab-allocator    | 保留并内部化         | 只允许 ax-alloc 直接依赖，不向上层重导出类型                                                                          |
| TLSF/rlsf               | 删除                 | 删除 backend、feature、依赖、build.rs 分支和测试                                                                      |
| ax-allocator            | 删除                 | 删除 crate 和全部调用方                                                                                               |
| bitmap-allocator        | 删除                 | 删除 crate 和全部引用                                                                                                 |
| ax-page-table           | 新建并统一           | 一个 crate 内建立 common/entry/stage1/stage2/boot；三个执行模块分别 feature-gated                                     |
| ax-page-table-entry     | 合并后删除           | 页表项迁入 ax-page-table::entry                                                                                         |
| ax-page-table-multiarch | 合并后删除           | cursor、protect、copy 和地址转换后备缓冲区 batching 迁入 ax-page-table                                                              |
| page-table-generic      | 合并后删除           | 可变层级、页大小和 Stage-2 能力迁入 ax-page-table                                                                     |
| ax-memory-set           | 保留并修正           | 统一虚拟内存区域操作和事务，使用 typed MappingResult                                                                         |
| ax-mm                   | 保留并收窄           | 只保留 ArceOS 内核虚拟内存和平台 glue                                                                                 |
| starry-mm               | 新建                 | 接收与操作系统实现无关的 Linux 虚拟内存策略、accounting、owner 和 capability；默认 Always overcommit，strict commit 可选 |
| StarryOS kernel/src/mm  | 拆分并删除重复       | 保留具体虚拟文件系统/PageTableCursor backend 和应用程序二进制接口 glue，删除已迁移 policy、重复状态和无意义 re-export                       |
| axaddrspace             | 保留并收窄           | 只保留 Guest Stage-2 策略                                                                                             |
| starry-vm               | 保留并收窄           | 只保留用户指针访问边界                                                                                                |
| dma-api                 | 保留并统一           | 成为唯一驱动 DMA API                                                                                                  |
| axklib::KlibDma         | 保留为 adapter       | 将 dma-api 接到 ax-alloc 和平台 cache/输入输出内存管理单元操作                                                                      |
| 输入输出内存管理单元 page table        | 不新建公共内存 crate | 实际控制器支持加入`drivers/iommu/<controller>` 私有模块和 DmaDomain adapter，不进入 CPU ax-page-table               |
| ax-dma                  | 迁移消费者后删除     | 删除 crate、feature、re-export、测试和文档引用                                                                        |
| mmio-api                | 保留                 | 只提供寄存器映射能力                                                                                                  |
| ax-runtime              | 保留并收窄           | 只执行内存交接、ax-alloc 初始化、CPU-local Slab 初始化和 adapter 接线                                                 |
| ax-hal、somehal、axklib | 保留为 adapter       | 使用 ax-page-table、dma-api 和 mmio-api，不形成新的内存管理层                                                         |

不创建 deprecated package、兼容 facade、旧模块转发、旧 feature alias 或旧名称兼容类型别名。

迁移只替换失效的 crate/API 边界。类型职责和语义没有变化时保留原名称；只有原名表达错误、与新公共接口冲突或职责确实变化时才更名。

保留现有 `PagingHandlerImpl`、`HostPagingHandler`、`Ram`、`PageTable` 和 `NestedPageTable` 等领域名称。`PageFrameProvider` 是统一页表核心新增的 capability trait，不作为批量重命名具体 provider 类型的理由。

### 3.2 实施顺序

#### 3.2.1 阶段 0：建立失败回归

先增加并确认旧实现失败：

- 非法 `DmaPod` compile-fail；
- DmaAllocHandle/DmaMapHandle 不可 Copy/Clone 的 compile-fail；
- 现有 DMA 资源获取即初始化 wrapper 的 Drop 只释放一次；
- 写时复制已有 u8 上限检测、跨页 clone、页表/accounting 失败和并发 fault 的回滚；
- backend unmap 失败后虚拟内存区域/页表项不丢失；
- 跨多个虚拟内存区域的 unmap/protect 在中间 backend 失败时完全不提交；
- protect 后虚拟内存区域/页表项 flags 一致；
- unsupported allocator 能力不 panic；
- ax-alloc 分配失败不调用 reclaim；

#### 3.2.2 阶段 1A：消除确定性崩溃和所有权缺陷

本阶段先修复能够由合法公共调用触发的崩溃、重复释放和计数溢出，为后续事务与组件迁移建立可靠所有权基础。

1. 删除无消费者的 alloc_pages_at；其他不支持能力在编译期不可见或返回 typed Unsupported，消除可达 `unimplemented`。
2. 收紧 DmaPod。
3. 保留现有 DMA 资源获取即初始化 wrapper；删除 DmaAllocHandle/DmaMapHandle 的 Copy/Clone，使 dealloc/unmap 消费底层 token。
4. 将写时复制计数改为锁内 u32 或合适原子类型，在修改前 checked increment。
5. 为跨页 clone 建立 preflight/commit/rollback，覆盖引用计数、父子页表项和 accounting。

#### 3.2.3 阶段 1B：修复 MappingBackend 事务

本阶段把映射、解除映射和权限修改改为可预检、可提交和可回滚的事务，任何故障注入点都不得改变操作前状态。

1. 将 MappingBackend 拆为返回 typed `MappingResult<Plan>` 的 prepare 和携带回滚状态的 commit。
2. 逐个迁移 ax-mm、axaddrspace、StarryOS 和测试 backend，保留具体错误原因。
3. MemorySet 在任何修改前为整段范围生成 MappingPlan，并预留虚拟内存区域 split 所需元数据。
4. 重写 unmap，禁止在 retain closure 中 unwrap；跨多个虚拟内存区域的页表项和元数据一次性提交。
5. 重写 protect，使 backend/页表项、actual flags 和 reported flags 一次性提交。
6. commit 失败的 backend 必须携带 undo 数据，在返回错误前恢复当前操作；MemorySet 回滚此前操作。
7. 使用逐虚拟内存区域故障注入验证 map/unmap/protect 要么全成功，要么页表项/虚拟内存区域完全保持原状。

#### 3.2.4 阶段 2：统一 DMA

本阶段把 StarryOS、ArceOS API 和设备驱动迁移到同一 `dma-api` 所有权模型，消费者迁完后立即删除 legacy DMA crate 与转发接口。

1. 将 StarryOS `DmaBufAlloc` 从 DMAInfo 和手工 Drop 迁移为 dma-api 的 DMA32 `CoherentArray<u8>`；保留 `DmaBufFile`、mmap retainer、`resolve_contiguous_dmabuf` 和各设备 import 的 `Arc` 生命周期语义。
2. 保持 rockchip-rga 驱动核心直接依赖 dma-api；RGA/JPEG/NPU OS glue 只解析 fd、校验范围并保留外部 owner，不新增 DMA facade 或包装 crate。
3. 将 sg2002-tpu ION 和 arceos_api 迁移到 dma-api/DeviceDma。
4. 删除 arceos_api 旧 DMAInfo、旧 alloc/dealloc 和 ax_dma re-export。
5. 删除 StarryOS `rga`、`jpeg`、`rknpu`、TPU feature 及其他调用方的 ax-dma feature、依赖和导入。
6. 删除 ax-dma、ax-allocator 和 bitmap-allocator。
7. 明确现有平台使用 identity/输入输出内存管理单元-bypass；未支持的输入输出内存管理单元 domain 返回 Unsupported，不新增空壳输入输出内存管理单元 crate。
8. 运行现有 `qemu-rga/system/rga-lifecycle`，补 `/dev/dma_heap` fd/mmap/import 最后引用释放测试，再更新 workspace、Cargo.lock、CI 和非历史文档。

#### 3.2.5 阶段 3：收敛运行时分配器

本阶段建立 `memory/ax-alloc` 唯一公共入口，固定 Buddy 与 Slab 实现，并删除算法选择、隐式回收和重复统计路径。

1. 移动 ax-alloc 到 `memory/ax-alloc` 并更新全部依赖。
2. 固定使用 buddy-slab-allocator，删除 TLSF、rlsf、stub、backend build.rs 和切换 feature。
3. 按 2.6.1 的 API 映射逐项迁移调用方后，删除 AllocatorOps、DefaultByteAllocator 和未使用 re-export。
4. 保留具体 byte API、MemoryZone PageRequest + UsageKind 页 API、GlobalAlloc 和 AllocatorStats。
5. 删除 PageReclaimFn、register_page_reclaim_fn、try_page_reclaim 和 allocator 内部重试。
6. 引入只含 Normal/Dma32 物理约束的 PageRequest，失败立即返回 NoMemory。
7. 不增加没有生产消费者的 ReserveAccess、EmergencyReserve 或通用 实时 guard，不新增按用途拆分的页 class。
8. 将现有 UsageKind 计数并入单一 AllocationSource × UsageKind 表，一次分配只更新一个 bucket；procfs/sysinfo 使用派生视图。
9. 记录 ranges-ext 当前直接生产消费者只有 kernutil 和 someboot；只将 MemoryDescriptor 所需固定容量 range 操作合并到 kernutil::memory 后删除 ranges-ext。
10. 引导处理器冻结 bump 前确认全部 CPU 的 meta、stack、per-CPU data 已由 someboot 多核 prealloc 分配；应用处理器只初始化本地 Slab。
11. 限制 buddy-slab-allocator 的生产反向依赖只能是 ax-alloc。

#### 3.2.6 阶段 4：统一页表

本阶段先以行为对照测试固定旧实现语义，再按页表项、主机页表、客户机页表和启动页表的顺序迁移，消费者完成后删除旧 crate。

1. 为 multiarch 和 generic 的共同能力建立 map/unmap/query、huge page、drop 和错误行为对照测试。
2. 新建 ax-page-table/common，定义统一 Address、PteConfig、PagingError、PageFrameProvider、TlbInvalidator 和 MemAttrLayout；已有调用方类型职责不变时保留原名。
3. 将 ax-page-table-entry 的 MappingFlags、GenericPTE 和架构页表项编码迁入 entry 模块。
4. 建立 feature-gated stage1 模块，迁移 multiarch 的 cursor、protect、copy、4K 路径和地址转换后备缓冲区 batching。
5. 建立 feature-gated stage2 API 视图，迁移 generic 的可变层级、可变页大小、walker 和 Guest 页表项。
6. 建立 feature-gated boot API 视图，使用 someboot Ram no-free provider；stage2/boot 可共享 private flexible 引擎。
7. 统一 AArch64 MemAttrLayout，迁移 axcpu/someboot/somehal 的 MAIR 写入和页表项 index。
8. 实现 TlbInvalidator 策略：AArch64 hardware broadcast、其他 多核 架构同步 remote 处理器间中断、单核 local；禁止 多核 静默退化。
9. 迁移 ax-hal/StarryOS Stage-1、someboot/somehal boot、AxVM/axaddrspace Stage-2 adapter；每迁移一个消费者即运行原测试。
10. 确认统一核心同时通过两套旧引擎行为测试后，逐个删除 ax-page-table-entry、ax-page-table-multiarch 和 page-table-generic。
11. 验证 ArceOS/StarryOS 构建不含 stage2，Axvisor 按需启用；删除旧 crate 名、feature、re-export、依赖和非历史引用。

#### 3.2.7 阶段 5A：建立薄 starry-mm 边界

本阶段只提取不依赖 StarryOS kernel 对象的类型、记账和能力接口，保留既有 `AddrSpace` 名称及具体页表适配，避免迁移伴随无必要更名。

1. 新建 no_std starry-mm；保留 kernel `AddrSpace` 名称和具体页表 adapter，不为迁移而更名。
2. 定义 VmFile/PageSource、page-cache eviction 和 FaultOutcome capability；具体 memfd listener 保留在 kernel adapter。
3. 迁移不依赖具体 PageTableCursor/虚拟文件系统/task/signal 的 range policy、共享页 owner 和纯状态操作。
4. 保持 StarryOS kernel adapter 调用旧实现与新实现的行为一致，运行基础 mmap/munmap/mprotect/brk test-suit。

#### 3.2.8 阶段 5B：迁移策略和记账

本阶段迁移写时复制、共享页、虚拟内存规模、常驻页和内存承诺规则，同时保持具体文件后端与页表操作位于 kernel 适配层。

1. 迁移写时复制引用计数、SharedPages、常驻内存集大小/虚拟内存大小、commit accounting 和统计快照；具体 anonymous/linear/写时复制/file/shared PageTableCursor backend 留在 kernel adapter。
2. 由 starry-mm 提供 mremap/clone/fork 的纯策略与事务数据，kernel adapter 执行具体页表项/虚拟文件系统操作；实现 RLIMIT_AS，默认 unlimited。
3. 实现 Always overcommit 和准确 Committed_AS 统计；默认报告 overcommit_memory=1，执行原子记账但不按 CommitLimit 拒绝。
4. 在 `starry-strict-commit` 中增加 Strict admission 和 CommitLimit，关闭 feature 时不链接严格检查代码。
5. kernel 实现 VmFile/PageSource、page-cache 和 memfd adapter，不允许 starry-mm 回引 kernel 类型。
6. 分别运行默认 Always 和 strict 配置的 fork/写时复制、file/shared mapping、mremap、memfd、exec/exit accounting test-suit。

#### 3.2.9 阶段 5C：迁移缺页、回收和 kernel 接线

本阶段由 `starry-mm` 输出缺页与回收决策，kernel 执行页表和文件系统动作，并统一完成信号与错误码转换。

1. starry-mm 提供 fault/reclaim 决策和 FaultOutcome；kernel backend 执行具体页表项 fault，并负责 signal/errno 转换。
2. 在 starry-mm 外层实现 clean-page 有界回收和单次 ax-alloc 重试。
3. 保持 task/process 对 kernel `AddrSpace` 的直接挂接，loader/access 不增加转发层。
4. 运行 `cargo xtask starry test qemu` 的直接发现用例和 `qemu/system` 内存相关子用例；重型压力负载使用 `cargo xtask starry app`，不引用已删除的 normal/stress 一级分组。
5. 删除 kernel/src/mm 中重复的 accounting、写时复制引用、commit、fault/reclaim policy 和无意义 re-export；保留具体 backend adapter。
6. 收窄 ax-mm 和 axaddrspace，删除与 ax-memory-set 重复的机械包装。

#### 3.2.10 阶段 6：静态池和系统配置

本阶段只为有容量依据、耗尽策略和路径测试的实时消费者增加固定池，关闭配置后对应代码和静态状态必须从镜像裁剪。

1. 保持 embedded-default、starry 和 hypervisor 为文档化的操作系统与板级支持包组合；只有目标板给出实时关键区、容量和耗尽行为后才增加具体 embedded-hard-rt 构建配置，删除 ax-alloc 内无行为的空 profile feature。
2. 先审计中断请求/no-fail 调用点和基准；只为实际使用者实例化 IrqEventPool、DmaDescriptorPool 或驱动专属 IoRequestPool。
3. 仅在目标板给出固定 task 上限的 embedded-hard-rt 配置中使用编译期静态 task slot/stack；其他 embedded 配置和 Starry 动态 task 使用 Slab；timer/wait node 优先嵌入 owner。
4. 不增加通用 pool manager crate；各子系统通过同一小型 fixed-pool primitive 组合本地静态存储。
5. 单核配置验证 owner CPU 恒为 0，多核配置验证跨 CPU 释放；不为单核维护另一套 Slab 实现。
6. 确认关闭 Starry/virtualization/DMA 可选能力后对应代码和静态状态不进入镜像。

#### 3.2.11 阶段 7：仅按测量结果增加优化

默认保留全局 Buddy 锁。每个目标板先定义 page allocation P99/max 延迟和 allocator lock wait 的绝对预算。任一预算不达标、性能采样确认全局 Buddy 锁是主要原因，且固定池或批量预分配无法解决时，才增加固定容量的 per-CPU order-0 cache。allocator CPU 占比只作为诊断指标，不设置 5% 硬门槛。

该 cache 只能缓存 order-0，必须支持 drain，单核配置必须禁用，不引入 migratetype 或完整每处理器页缓存。

### 3.3 清理要求

完成后以下目录必须不存在：

- `memory/axallocator`；
- `memory/bitmap-allocator`；
- `memory/ranges-ext`；
- `memory/page-table-generic`；
- `memory/page_table_entry`；
- `memory/page_table_multiarch`；
- `os/arceos/modules/axdma`；
- `os/arceos/modules/axalloc`。

以下目录必须存在：

- `memory/ax-alloc`；
- `memory/ax-page-table`；
- `memory/starry-mm` 或符合 workspace 分层规则的等价共享组件目录。

源码、Cargo manifest、CI、测试配置和非历史文档不得再出现：

- ax-dma/ax_dma；
- ax-allocator/ax_allocator；
- bitmap-allocator/bitmap_allocator；
- ranges-ext/ranges_ext；
- page-table-generic/page_table_generic；
- ax-page-table-entry/ax_page_table_entry；
- ax-page-table-multiarch/ax_page_table_multiarch；
- tlsf feature 和 rlsf dependency；
- AllocatorOps、DefaultByteAllocator；
- PageReclaimFn、register_page_reclaim_fn、try_page_reclaim；
- 旧 DMAInfo API。

历史 CHANGELOG、回顾文章和明确标注为迁移说明的文档可以保留已发生过的名称，但不得参与当前构建、导出或当前 API 使用示例。Cargo.lock 必须由 Cargo 重新生成，不手工保留旧 package。

### 3.4 当前实施状态

当前分支与 `origin/dev` 的共同基线为 `2e53a08e7875`；本次审查时 `origin/dev` 为 `dbc942796507`，当前分支尚未合入其后的 4 个提交。下表仅记录已在当前工作区实现并验证的状态。

| 范围 | 状态 | 已完成验证 | 尚需验收 |
| ---- | ---- | ---------- | -------- |
| Runtime allocator | 已实施 | `ax-alloc` 已迁入 `memory/`；Buddy 使用 `SpinNoIrq`；旧 allocator crate、backend feature、reclaim callback 和重复统计入口已删除 | 目标板 P99/max、锁等待、碎片和镜像差值 |
| DMA | 已实施 | move-only token 与非法 `DmaPod` compile-fail；资源获取即初始化单次释放；固定 pool 耗尽立即 `NoMemory`；最后一个 `Arc` retainer 释放 backing；AArch64 `qemu-rga/system` 23/23 通过 | 真实非一致性 DMA 平台的 cache/bounce 指标 |
| ax-memory-set | 已实施 | 排序虚拟内存区域 Vec 在 commit 前 fallible reserve；map/unmap/protect 的 prepare/commit/rollback；metadata split 失败保持原虚拟内存区域；不可 split backend 返回 typed error；clear 失败保留元数据 | Starry `MREMAP_FIXED` 覆盖已有目标后发生晚期页表分配失败的专用故障注入 |
| ax-page-table | 已实施 | 旧三个 crate 已删除；stage1/stage2/boot feature 分离；全 feature 页表测试通过 | 各目标板地址转换后备缓冲区 shootdown 延迟与镜像裁剪数据 |
| starry-mm | 已实施 | 写时复制、常驻内存集大小/虚拟内存大小、commit accounting、Always/Strict、单次有界回收和 mremap 页表项移动的 host 测试通过；fault 页表项/accounting 失败回滚通过 kernel axtest；RISC-V `mremap` 57/57、procfs/commit 20/20 通过 | strict 配置的目标镜像 QEMU/板级压力回归 |
| someboot | 已实施 | checked bump、Active/Frozen、固件多段内存规范化和引导处理器 CPU 区域预分配；host 测试通过 | 各引导处理器启动内存图和应用处理器启动实测 |
| 性能与 hard-实时 | 未验收 | 已保留固定容量 pool、无 allocator 隐式回收和 profile 裁剪边界 | 按 4.3 在目标板采样；未满足测量触发条件前不增加 per-CPU page cache |

未完成项不得通过兼容层、静默 fallback 或放宽测试关闭。`MREMAP_FIXED` 晚期故障注入完成前，地址空间事务只按已覆盖的 map/unmap/protect 与现有 mremap 场景验收；目标板数据生成前，阶段 7 的 per-CPU page cache 保持不实现。

## 4. 验收

### 4.1 架构和依赖验收

架构验收检查唯一入口、依赖方向、feature 裁剪和旧组件删除结果，任何兼容 facade 或反向依赖都视为未完成整改。

- 运行时 allocator 的唯一公共入口是 `memory/ax-alloc`。
- buddy-slab-allocator 的生产反向依赖只有 ax-alloc。
- ax-page-table 已通过 multiarch/generic 共同能力的行为对照测试；common/entry/stage1/stage2/boot 边界明确，两个旧引擎和独立 entry crate 已删除。
- ArceOS/StarryOS 构建不包含 stage2，非 boot 构建不包含 boot；Axvisor 按需启用 stage2。
- ax-memory-set 是唯一虚拟内存区域公共机制，ax-mm、starry-mm 和 axaddrspace 只保留各自策略。
- starry-mm 独立承载与操作系统实现无关的 Linux 虚拟内存策略；StarryOS kernel 只保留具体虚拟文件系统与页表 backend 和应用程序二进制接口 adapter，不保留重复 policy 或无意义转发模块。
- 驱动只使用 dma-api 和 mmio-api，不直接依赖 allocator backend。
- StarryOS RGA/JPEG/NPU/TPU feature 不再引入 ax-dma；`/dev/dma_heap` 的 backing owner 来自 dma-api。
- 输入输出内存管理单元 page table 只存在于实际控制器 driver/domain adapter，不进入 CPU ax-page-table。
- 生产依赖图无旧 crate、兼容 facade、双 backend 或循环依赖。
- StarryOS 复杂虚拟内存代码不进入最小 ArceOS/Axvisor 构建。

验证命令：

```sh
cargo metadata --format-version 1
cargo tree --workspace
cargo tree --workspace -i buddy-slab-allocator
cargo tree --workspace -i ranges-ext
rg '<old-crate-or-api>' --glob '*.rs' --glob 'Cargo.toml' --glob '*.toml'
```

删除前 `ranges-ext` 的直接生产依赖只能是 kernutil 和 someboot；迁移后反向依赖查询应返回 package 不存在。

### 4.2 正确性验收

正确性验收覆盖所有权单次释放、地址空间全成或回滚、跨 CPU 地址转换一致性和 StarryOS 记账，不能只验证成功路径。

- DmaPod 不存在对任意 Copy 类型的 blanket implementation。
- 现有 DMA 资源获取即初始化 wrapper 保持不可复制且只释放一次；DmaAllocHandle/DmaMapHandle 不再实现 Copy/Clone，dealloc/unmap 消费 token。
- 零长度 DMA allocation 在调用 backend 前返回 `ZeroSizedBuffer`，不会产生无法明确释放语义的 token。
- allocator 不存在可达 `unimplemented`、无 backend panic stub 或隐式 reclaim callback。
- 删除 AllocatorOps 后，byte allocation、MemoryZone PageRequest + UsageKind、初始化和单一 AllocatorStats 均有具体 API 和现有调用方迁移测试。
- ax-alloc 分配失败立即返回 NoMemory；中断请求/实时 路径不触发通用分配或回收。
- Buddy、Slab、Starry、Guest 和 DMA 对同一物理页不存在重复所有权。
- 只有 Normal/Dma32 两个 MemoryZone。
- map/unmap/protect 跨一个或多个虚拟内存区域均为 all-or-nothing；prepare 失败不修改状态，commit 后虚拟内存区域/页表项一致。
- 写时复制保留上限检测；跨页 clone、父子页表项、引用计数和 accounting 任一步失败均正确回滚。
- 默认 Starry 报告 overcommit_memory=1 且不执行 strict admission；starry-strict-commit 报告 2，并在超限时返回 ENOMEM。
- RLIMIT_AS 默认 unlimited 且实际执行；CommitLimit/Committed_AS 不再是固定占位值。
- fault 分配失败转换为明确的 Linux signal/errno，不 panic。

### 4.3 性能验收

#### 4.3.1 算法上界

算法验收记录每条关键路径的查找、锁和重试上界；不允许用平均值掩盖无界回收、隐式阻塞或不可控容器扩容。

| 路径                      | 要求                                     |
| ------------------------- | ---------------------------------------- |
| Boot bump                 | O(1)                                     |
| 固定池 alloc/free         | O(1)                                     |
| Slab 命中                 | O(1) 或固定 size-class 查找              |
| Slab 扩容                 | 一次 Buddy 请求，仅非中断请求                |
| Buddy                     | 仅与固定 MAX_ORDER 和有限 section 数相关 |
| DMA descriptor completion | 不分配                                   |
| 虚拟内存区域 find                  | O(log 虚拟内存区域数)                            |
| 单页 page-table walk      | 仅与固定页表层级相关                     |
| Starry 回收               | 一次 clean-page 回收和一次分配重试       |

#### 4.3.2 记录指标

每个支持架构至少记录：

- alloc/free min、median、P95、P99、max；
- 单核及 2/4 核竞争；
- allocator lock 持有和等待时间；
- AllocatorStats 更新次数和 cache-line contention；
- Slab 命中、扩容和 remote free；
- Buddy 各 order 成功/失败次数和最大连续页；
- 内部碎片、空闲大块恢复和 allocator metadata 占用；
- `.text/.data/.bss` 增量；
- stage2、boot、strict-commit 和各固定池关闭前后的镜像差值；
- local、hardware-broadcast 和 remote-处理器间中断 地址转换后备缓冲区 flush 次数及耗时；
- reclaim 次数、回收页数和耗时；
- DMA bounce 次数和复制字节数。

相对基线必须固定目标板、CPU 频率、构建 profile、功能集合、工具链、CPU affinity、预热次数、样本数和分配序列；报告中记录这些参数，否则 P99 和 10% 退化阈值不作为验收证据。

通过条件：

- 每个目标板在测试配置中定义 page allocation P99/max 和 allocator lock wait 的绝对预算，并全部达标。
- 固定池和 Slab 命中路径延迟不随总内存大小增长。
- 相同配置的 P99 相对基线退化不超过 10%，超过时必须有可复现数据和明确原因。
- max latency 中不隐藏回收、同步日志、文件 IO 或无界扩容。
- 一次 alloc/free 只更新一个 AllocatorStats bucket，不为 source/usage 视图重复写计数。
- hard-实时 配置中已识别 实时 critical section 的通用堆分配计数为 0。
- 可选能力关闭后，对应代码和静态状态不进入链接结果。

### 4.4 测试验收

#### 4.4.1 分配器

分配器测试覆盖多段物理内存、不同对象大小、地址区域约束、统计一致性和失败立即返回，压力测试必须检查泄漏与碎片变化。

- size class 满池、空池和交替 alloc/free；
- Buddy split/merge、多 section、DMA32 边界；
- 跨 CPU allocate/free 和 remote free；
- 后续独立引入 reserve 时，验证耗尽、归还及与 Normal 隔离；当前配置不得包含 reserve 状态；
- 固定 seed 压力测试；
- 分配失败后的 free list、所有权和统计一致；
- heap integrity；
- 引导处理器冻结前已为全部配置 CPU 预分配 meta、stack 和 per-CPU data；应用处理器启动不调用 bump；
- someboot ram allocator 在 Frozen 后拒绝分配，交接前后内存描述无重叠；
- MemoryZone 和 UsageKind 视图由同一 AllocatorStats 数据派生且结果正确；
- 仅对已经审计并实际启用固定池的中断请求/实时 消费者验证耗尽立即失败且不回退到通用 allocator；没有生产消费者时不创建空池或通用 guard。

#### 4.4.2 页表和地址空间

页表与地址空间测试覆盖各架构编码、映射生命周期、远端失效和逐操作故障注入，并比较失败前后的完整元数据与页表项。

- map/unmap/protect 后虚拟内存区域/页表项一致；
- 虚拟内存区域中间 split 和两端 shrink；
- 第一个、中间和最后一个 backend prepare/commit 故障注入均满足全成或全回滚；
- huge page 和地址溢出边界；
- Drop 只释放 owned table frame；
- boot no-free provider；
- Stage-1/Stage-2 flags 隔离；
- AArch64 hardware-broadcast、其他架构 remote-处理器间中断 和单核 local 地址转换后备缓冲区路径分别验证；多核 缺少有效 shootdown 实现时构建或初始化失败；
- axcpu、someboot、somehal 写入相同 MAIR layout，所有 AArch64 页表项 AttrIndx 均来自该 layout；
- ax-page-table 对 multiarch/generic 共同场景的结果和错误与迁移前基线一致；
- 可变层级/页大小/Stage-2 与 cursor/protect/copy/地址转换后备缓冲区 batching 均保留；
- MappingBackend 错误完整传播。

#### 4.4.3 StarryOS

StarryOS 测试覆盖 Linux 可见的映射、写时复制、资源上限、内存承诺、缺页和有界回收语义，并验证错误码与统计结果。

- fork/写时复制引用上限、失败回滚和并发 fault；
- anonymous/private/shared/file mapping；
- mmap、munmap、mprotect、mremap、madvise 和 brk；
- 用户指针跨页、未对齐和权限错误；
- clean-page 回收只在允许上下文执行且最多重试一次；
- Always 模式 mmap/brk/fork 兼容性、RLIMIT_AS 和 overcommit_memory=1；
- strict 模式 commit 预留、归还、失败回滚、超限 ENOMEM 和 overcommit_memory=2；
- Committed_AS/CommitLimit 与实际模式和映射变化一致；
- fault NoMemory 到 signal/errno 的转换；
- 对应 Starry test-suit case。

#### 4.4.4 DMA 与 MMIO

设备内存测试分别验证 DMA 缓冲区所有权与缓存转换，以及 MMIO 寄存器映射、设备页属性、边界访问和解除映射协议。

- coherent/streaming direction 和 cache sync；
- mask、alignment、boundary、segment size；
- 输入输出内存管理单元 domain mismatch；
- identity/输入输出内存管理单元-bypass 正常工作，未实现输入输出内存管理单元 domain 明确返回 Unsupported；
- bounce copy-in/copy-out；
- DmaAllocHandle/DmaMapHandle 非 Copy/Clone 和非法 DmaPod compile-fail；
- `/dev/dma_heap` DMA32 地址和页对齐正确；fd、mmap、RGA/JPEG/NPU import 任意顺序释放均无提前释放、泄漏或 double free；
- `qemu-rga/system/rga-lifecycle` 保持通过，覆盖 cross-thread、独立 open、fork、import/release 和长度边界；
- 增加可观察 DMA backing 回收次数的测试，验证 fd、mmap 和设备 import 的最后一个 retainer 释放时只回收一次；
- pool 耗尽立即失败；
- 中断请求 completion 不分配；
- MMIO 不进入 RAM allocator，unmap 不释放设备物理区。

### 4.5 工程验收

每个修改 crate 执行：

```sh
cargo fmt --all --check
cargo xtask clippy --package <changed-crate>
```

ArceOS、StarryOS 和 Axvisor 使用对应 `cargo xtask` build/test。所有适用单元测试、集成测试、compile-fail 测试和 test-suit case 必须通过。
