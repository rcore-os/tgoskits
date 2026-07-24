---
sidebar_position: 5
sidebar_label: "页表核心"
---

# 统一页表核心

`memory/ax-page-table` 将架构 Page Table Entry（页表项，PTE）、主机第一阶段地址转换、客户机第二阶段地址转换和早期启动页表放在同一 crate 内，通过独立 feature 裁剪。它只实现页表结构与地址翻译机制，物理页帧来源由 `PageFrameProvider` 注入，Virtual Memory Area（虚拟内存区域，VMA）和系统策略由上层持有。

## 1. 组件结构

统一 crate 消除了旧 page-table entry、multiarch 和 generic crate 之间的重复类型，同时保持执行路径彼此独立。未启用的模块不会进入编译产物。

### 1.1 模块与 feature

`common` 和 `entry` 始终可用；`stage1` 使用专用 32/64-bit cursor 实现；`stage2` 与 `boot` 分别重导出 feature-gated 的 flexible engine。

| 模块 | Feature | 主要类型 | 消费者 |
| --- | --- | --- | --- |
| `common` | 始终 | `PageFrameProvider`、`PagingError`、`PteConfig`、`TlbInvalidator` | 全部页表路径 |
| `entry` | 始终 | `GenericPTE`、`MappingFlags`、架构页表项、AArch64 `MemAttrLayout` | Stage-1 和平台页表项 adapter |
| `stage1` | `stage1` | `PageTable32/64`、cursor、`PagingMetaData`、`PageSize` | ArceOS、StarryOS、Host CPU |
| `stage2` | `stage2` | flexible `PageTable`、`TableMeta`、`MapConfig` | AxVM、`axaddrspace` adapter |
| `boot` | `boot` | 同一 flexible engine 的独立导出 | `someboot`、`somehal` |

`stage2` 与 `boot` 共享无 OS 语义的 flexible 实现，但模块之间不互相调用。ArceOS/StarryOS 普通构建无需链接 `stage2`，非启动消费者无需链接 `boot`。

### 1.2 依赖边界

页表 crate 依赖地址类型和轻量容器，不依赖 `ax-alloc`。这允许 boot provider 使用 bump arena，运行时 provider 使用 Buddy，而 host-side test 使用模拟 frame source。

```mermaid
flowchart BT
    Addr["ax-memory-addr"] --> Common["ax-page-table::common"]
    Common --> Entry["entry"]
    Common --> Stage1["stage1"]
    Common --> Flexible["flexible engine"]
    Entry --> Stage1
    Entry --> Flexible
    Flexible --> Stage2["stage2 export"]
    Flexible --> Boot["boot export"]
    BootProvider["someboot::Ram"] --> Boot
    RuntimeProvider["ax-hal::PagingHandlerImpl"] --> Stage1
    GuestProvider["AxVM host provider"] --> Stage2
```

frame provider 是能力注入，不是 allocator facade。页表层收到 `None` 时返回 `PagingError::NoMemory`，不会注册 reclaim callback 或重试。

## 2. 公共类型

公共类型将地址有效性、frame ownership、页表项配置和地址转换后备缓冲区失效能力分开。上层只实现自己拥有的部分。

### 2.1 页帧来源

`PageFrameProvider` 要求 `Clone + Sync + Send + 'static`，默认 frame size 为 4 KiB。单 frame 方法是最低能力，多 frame 方法允许需要对齐的 root table allocation。

```rust
pub trait PageFrameProvider: Clone + Sync + Send + 'static {
    const FRAME_SIZE: usize = 0x1000;

    fn alloc_frame(&self) -> Option<PhysAddr>;
    fn alloc_frames(&self, num: usize, align: usize) -> Option<PhysAddr>;
    fn dealloc_frame(&self, paddr: PhysAddr);
    fn dealloc_frames(&self, paddr: PhysAddr, num: usize);
    fn phys_to_virt(&self, paddr: PhysAddr) -> VirtAddr;
}
```

运行时 `os/arceos/modules/axhal/src/paging.rs::PagingHandlerImpl` 使用 `MemoryZone::Normal` 和 `UsageKind::PageTable`。boot 的 `Ram` provider 则从 early bump 分配且不逐 frame 释放。

### 2.2 页表项配置与错误

`PteConfig` 是 flexible entry 的中性配置，`GenericPTE` 是 Stage-1 架构 entry 的操作边界。两者都把架构位编码限制在 `entry::arch` 内。

| 类型 | 表达内容 | 不表达的内容 |
| --- | --- | --- |
| `AccessFlags` | read/write/execute/user 等通用访问权限 | 虚拟内存区域 pathname、写时复制 owner |
| `MemAttributes` | Normal、PerCpu、Device、Uncached | DMA coherence 协议 |
| `MemConfig` | memory attributes 和 shareability 配置 | MAIR register 写入时序 |
| `PteConfig` | paddr、valid、huge、directory 与通用 flags | frame allocation policy |
| `PagingError` | NoMemory、NotMapped、alignment、conflict、hierarchy 等 | syscall errno 与 signal |

上层在 OS 边界把 `PagingError` 转为 `AxError` 或领域错误。页表实现不直接返回 Linux errno，也不记录虚拟内存区域 metadata。

## 3. 主机页表

Stage-1 面向 Host CPU 地址空间，按 pointer width 选择 `PageTable32` 或 `PageTable64`。架构 metadata 决定层数、物理/虚拟地址宽度、地址类型和地址转换后备缓冲区 invalidator。

### 3.1 架构元数据

`PagingMetaData` 将固定架构事实集中在类型参数中。当前公共 `PageSize` 支持 4 KiB、1 MiB、2 MiB 和 1 GiB，具体架构只会使用硬件允许的组合。

| 架构 metadata | 层数/地址形态 | 地址转换后备缓冲区 scope |
| --- | --- | --- |
| `A64PagingMetaData` | 4 层、48-bit 虚拟地址/物理地址 | `HardwareBroadcast` |
| `Sv39MetaData` | 3 层、39-bit 虚拟地址 | `Local` |
| `Sv48MetaData` | 4 层、48-bit 虚拟地址 | `Local` |
| `X64PagingMetaData` | x86_64 4-level Host 表 | `Local` |
| `LA64MetaData` | LoongArch64 Host 表 | `Local` |
| `A32PagingMetaData` | 32-bit ARM 表 | `Local` |

`vaddr_is_valid()` 在创建或查询映射时拒绝超出架构宽度的地址。AArch64 对高地址执行显式 sign-extension 棡查。

### 3.2 游标与批量失效

Stage-1 cursor 集中 `map`、`remap`、`protect`、`unmap`、region 操作和可选 `copy_from`。cursor 记录失效请求，在显式 `flush()` 或 Drop 时统一执行。

```mermaid
stateDiagram-v2
    [*] --> None
    None --> Array: first stale 虚拟地址
    Array --> Array: up to 32 VAs
    Array --> Full: threshold exceeded
    None --> Full: full-table operation
    Array --> None: flush each 虚拟地址
    Full --> None: flush all
```

`SMALL_FLUSH_THRESHOLD` 当前为 32。新 map 通常没有旧 translation 需要失效；unmap、remap、protect 和 copy 等修改会按实际操作记录地址或升级为 full flush。

## 4. 地址转换后备缓冲区一致性

页表项修改与地址转换后备缓冲区 shootdown 是同一个正确性协议的两部分。页表核心描述 invalidator scope，系统层负责保证所有可能运行该地址空间的 CPU 都观察到失效。

### 4.1 失效范围

`TlbInvalidator<A>` 提供 `const SCOPE`、`invalidate(Option<A>)` 和批量 `invalidate_list(&[A])`。`Some(vaddr)` 表示单地址，`None` 表示全部；scope 明确硬件操作能覆盖的 CPU 范围。默认批量实现逐地址执行，运行时 adapter 可覆盖为一次 remote 处理器间中断 批处理。

| `TlbScope` | 含义 | 当前示例 |
| --- | --- | --- |
| `Local` | 只失效当前 CPU | RISC-V `sfence.vma`、x86、LoongArch 本地操作 |
| `HardwareBroadcast` | 架构指令广播到 shareable domain | AArch64 `tlbi ...is` |
| `RemoteIpi` | invalidator 自身完成 remote 处理器间中断 | 接口支持，当前 Stage-1 架构实现未使用 |

AArch64 `A64TlbInvalidator` 使用 inner-shareable TLBI，并在汇编序列中执行 DSB/ISB。其他 local-only 架构需要系统 处理器间中断 配合。

### 4.2 多核 启动检查

`stage1::smp_invalidation_available<M>(remote_ipi)` 判断 metadata scope 或系统 处理器间中断 是否足够。`ax-mm::init_memory_management()` 调用 `ax_hal::paging::validate_smp_invalidation()`；多 CPU 且既无硬件 broadcast 又未启用 `ipi` 时会 assert 失败。

| 运行配置 | 是否允许多 CPU Stage-1 |
| --- | --- |
| AArch64 hardware broadcast | 允许，即使页表层不依赖软件 处理器间中断 |
| local-only 架构 + `ax-hal/ipi` | 允许，由上层 shootdown 协议覆盖 remote CPU |
| local-only 架构、无 处理器间中断、CPU 数大于 1 | 初始化失败 |
| 单 CPU | 允许 local invalidation |

该检查保证 capability 存在，但每个共享地址空间的具体 remote shootdown 时序仍由调用方负责。不能因为 cursor 在 Drop 时 local flush 就认为跨 CPU 一致性自动完成。

### 4.3 启动阶段切换

非 AArch64 多核 在引导处理器初始化 runtime page table 时，处理器间中断 callback 尚未发布可用。`ax-hal` 因此把 remote shootdown 分为两个明确阶段：

| 阶段 | 行为 | 安全依据 |
| --- | --- | --- |
| primary 处理器间中断 ready 之前 | 只刷新 boot CPU 本地地址转换后备缓冲区 | secondary CPU 尚未运行 runtime address space |
| primary 调用 `mark_current_cpu_ready()` 之后 | `enable_remote_tlb_shootdown()` 以 Release 发布，后续修改同步通知 ready CPU | secondary CPU 在发布 ready 前先装载 kernel root 并执行 full local flush |

shootdown 读取 enable 状态使用 Acquire。已 ready CPU 的 处理器间中断 错误是不可恢复的一致性故障；尚未 ready 的 CPU 返回 `CpuOffline` 时可以跳过，因为其 ready 发布前必定执行全量本地失效。AArch64 始终使用 inner-shareable hardware broadcast，不进入该软件开关。

## 5. AArch64 内存属性

AArch64 页表项的 `AttrIndx` 必须与 MAIR_EL1/EL2 slot 完全一致。`entry::aarch64::MemAttrLayout` 是统一常量来源，避免 boot、Host CPU 和页表项 encoder 各自定义索引。

### 5.1 属性槽位

当前布局包含 Device-nGnRE、Normal write-back 和 Normal non-cacheable 三个 slot。`MAIR_VALUE` 与这些 index 在同一类型中定义。

| 属性 | `AttrIndx` | MAIR byte | 使用场景 |
| --- | --- | --- | --- |
| Device-nGnRE | 0 | `0x04` | MMIO/device mapping |
| Normal write-back | 1 | `0xff` | 普通 RAM、页表、内核代码和数据 |
| Normal non-cacheable | 2 | `0x44` | uncached mapping |

`MemAttrLayout::MAIR_VALUE` 当前为 `0x44ff04`。`A64PTE` 从 `MappingFlags::DEVICE` 或 `UNCACHED` 选择对应 index，并为 Normal 类型添加 shareability bits。

### 5.2 写寄存器与编码消费

`components/axcpu/src/aarch64/init.rs`、`someboot` EL1/EL2 初始化和 boot 页表项 adapter 都引用同一 layout。修改 slot 时必须一起验证寄存器值、Stage-1 页表项和 boot 页表项。

| 消费位置 | 使用内容 |
| --- | --- |
| `components/axcpu/src/aarch64/init.rs` | 写 `MAIR_EL1` |
| `platforms/someboot/src/arch/aarch64/el1/mod.rs` | boot EL1 MAIR |
| `platforms/someboot/src/arch/aarch64/el2/mod.rs` | boot EL2 MAIR |
| `platforms/someboot/src/arch/aarch64/paging/pte.rs` | boot 页表项 index encode/decode |
| `memory/ax-page-table/src/entry/arch/aarch64.rs` | Stage-1 `A64PTE` encode/decode |

DMA cache maintenance 不能仅靠把页表项改为 uncached 代替。coherent/streaming ownership 和同步时序属于 `dma-api` 与平台 cache adapter。

## 6. 客户机与启动页表

Stage-2 和 boot 需要可变层数、可变 base page size和不同 entry 格式，因此使用 `flexible` engine。它们共享算法，不共享上层策略。

### 6.1 可变几何

`TableMeta` 通过常量描述 entry 类型、base page size、每级 index bits、最大 block level 和是否严格检查地址宽度。engine 由这些常量计算每一级 mapping size。

```rust
pub trait TableMeta: Sync + Send + Clone + Copy + 'static {
    type P: PageTableEntry;
    const PAGE_SIZE: usize;
    const LEVEL_BITS: &[usize];
    const MAX_BLOCK_LEVEL: usize;
    const STRICT_ADDRESS_WIDTH: bool = false;
    fn flush(vaddr: Option<VirtAddr>);
}
```

`MapConfig` 提供虚拟地址、物理地址、size、页表项 template、`allow_huge` 和 `flush`。递归 mapper 只有在 level、剩余大小及虚拟地址/物理地址对齐都满足时才创建 block mapping。

### 6.2 所有权差异

`PageTable<T, A>` 拥有 root frame 并在 Drop 时递归释放；`PageTableRef<T, A>` 引用已有 root，用于接管硬件或固件已建立的表。两者都保留 provider 以完成 frame 地址转换与释放。

| 类型或模块 | root ownership | 典型用途 |
| --- | --- | --- |
| `PageTable` | 拥有并递归释放 | 新建 Guest nested page table |
| `PageTableRef` | 引用既有 root | 固件/硬件表接管或临时操作 |
| `stage2` export | 由虚拟机生命周期决定 | 客户机中间物理地址或客户机物理地址 → 主机物理地址 |
| `boot` export | early bump 整体保留 | 建立启动 direct map、kernel map |

boot provider 的 deallocation no-op 意味着递归 Drop 不会把页面返回运行时 Buddy；这是 early arena 整体保留语义。Stage-2 runtime provider 则必须真正对称释放页表 frame。

## 7. 消费与审计入口

页表 crate 只提供机制，实际安全性还取决于 provider、地址空间事务和架构寄存器初始化。修改公共 API 时要逐条检查消费者 feature。

### 7.1 消费矩阵

当前 workspace dependency 显示三类路径按需启用。该矩阵可用于检查不必要代码是否进入镜像。

| 消费者 | Feature | Provider/adapter |
| --- | --- | --- |
| `ax-hal` / `ax-mm` | `stage1` | `PagingHandlerImpl` → `ax-alloc` |
| Starry kernel | `stage1`、`copy-from` | Host runtime page provider |
| `someboot` / `somehal` | `boot` | `Ram` early bump provider |
| `axvm` / `axaddrspace` | `stage2` | Host page frame provider / nested ops adapter |
| `axcpu` | `stage1` + arch feature | CPU init、页表项/MAIR types |

ArceOS/Starry production tree 不应出现 `stage2`，Axvisor 也不应为普通 Guest mapping 链接 `boot`。

### 7.2 源码检查点

以下文件覆盖统一页表的关键不变量。架构改动应配合 entry round-trip、map/query/unmap 和地址转换后备缓冲区 scope 测试。

| 源码 | 审计重点 |
| --- | --- |
| `memory/ax-page-table/src/common.rs` | provider、error、页表项 neutral config、地址转换后备缓冲区 scope |
| `memory/ax-page-table/src/entry/` | 架构位布局和 `MappingFlags` round-trip |
| `memory/ax-page-table/src/stage1/bits32.rs` | 32-bit cursor、region operation、flush batching |
| `memory/ax-page-table/src/stage1/bits64.rs` | 64-bit cursor、huge page、copy、flush batching |
| `memory/ax-page-table/src/flexible/` | variable geometry、recursive map/unmap/walk/drop |
| `os/arceos/modules/axhal/src/paging.rs` | runtime provider 与 多核 capability check |
| `platforms/someboot/src/arch/*/paging*` | boot geometry、provider 和启用时序 |

错误注入应覆盖 frame allocation 在各层失败、huge mapping 下继续下钻、地址宽度 overflow、已有 mapping conflict、部分 subtree 回收以及 cursor Drop 的 flush 行为。

## 8. 地址翻译实例

页表行为应从地址位划分、页表页来源和失效范围三个维度同时分析。只看到 `map()` 成功并不能证明 provider ownership 或 多核 地址转换后备缓冲区一致性正确。

### 8.1 四级页表遍历

以 64-bit 四级、每级 9-bit index、4 KiB base page 为例，虚拟地址 `0xffff_8000_1234_5000` 的索引为 L4=`0x100`、L3=`0x000`、L2=`0x091`、L1=`0x145`，页内 offset 为 0。`PagingMetaData` 决定层数和地址宽度，cursor 按架构 metadata 提取这些字段。

```text
63                              48 47       39 38       30 29       21 20       12 11        0
+--------------------------------+-----------+-----------+-----------+-----------+------------+
| canonical sign extension       | L4 0x100  | L3 0x000  | L2 0x091  | L1 0x145  | offset 0x0 |
+--------------------------------+-----------+-----------+-----------+-----------+------------+
```

若 L4、L3 已存在而 L2 指向空 entry，映射 4 KiB 页需要为 L1 table 申请一个 frame，再写最终 leaf 页表项。任何中间 frame allocation 返回 `None` 都转换为 `PagingError::NoMemory`，已经临时建立但未链接的 frame 必须释放。

```mermaid
flowchart LR
    Root["root frame"] --> L4["L4[0x100]"]
    L4 --> L3["L3[0x000]"]
    L3 --> L2["L2[0x091]"]
    L2 --> L1["L1[0x145]"]
    L1 --> Frame["target 物理地址 + flags"]
```

如果目标使用 2 MiB mapping，leaf 停在 L2，虚拟地址、物理地址和 size 都必须 2 MiB 对齐；query 返回该 block 的 base 物理地址，再加 `PageSize::align_offset(vaddr)` 得到最终物理地址。已有 huge entry 下不能静默创建更低一级 table，否则会破坏原映射。

### 8.2 页帧来源交接

`PageFrameProvider` 的核心接口只暴露 frame allocation、释放和物理到虚拟转换。页表算法既不知道 `MemoryZone`，也不知道 boot bump 或 Guest owner。

```rust
pub trait PageFrameProvider: Clone + Sync + Send + 'static {
    const FRAME_SIZE: usize = PAGE_SIZE_4K;

    fn alloc_frame(&self) -> Option<PhysAddr>;
    fn dealloc_frame(&self, paddr: PhysAddr);
    fn alloc_frames(&self, count: usize, align: usize) -> Option<PhysAddr>;
    fn dealloc_frames(&self, start: PhysAddr, count: usize);
    fn phys_to_virt(&self, paddr: PhysAddr) -> VirtAddr;
}
```

三个典型 provider 对同一个“需要一页 L1 table”的请求有不同所有权结果。

| Provider | 分配动作 | `dealloc_frame()` | 生命周期 |
| --- | --- | --- | --- |
| `someboot::mem::ram::Ram` | checked early bump | no-op | used prefix整体 Reserved |
| `ax-hal::PagingHandlerImpl` | `Normal × PageTable` | 返回 `ax-alloc` | Stage-1 table owner |
| AxVM 主机页提供者 | 主机页 API | 返回主机分配器 | 虚拟机或嵌套页表所有者 |

因此 boot table 的 Drop 不能用于判断物理页已回收到 Buddy；相反，runtime provider 的 owned `PageTable` 若未对称释放每个子表 frame 就是泄漏。

### 8.3 映射与失效顺序

假设 cursor 连续 protect 20 个 4 KiB 页。每次修改将虚拟地址加入固定容量 `ArrayVec`，cursor flush 时逐地址调用 invalidator；若修改 40 页，第 33 个地址使状态升级为 `Full`，后续不再记录单地址。

```text
0 addresses       TlbFlusher::None
1..32 addresses   TlbFlusher::Array([va0, ..., vaN])
33+ addresses     TlbFlusher::Full
flush/drop        invalidate_list(...) 或 invalidate(None)，随后回到 None
```

固定阈值避免页表修改为了记录 flush 又申请 heap。单地址列表和 full flush 的选择只优化本次 invalidator 调用，不能改变 `TlbScope`。

```rust
pub const fn smp_invalidation_available<M: PagingMetaData>(remote_ipi: bool) -> bool {
    remote_ipi
        || matches!(
            M::Tlb::SCOPE,
            TlbScope::HardwareBroadcast | TlbScope::RemoteIpi
        )
}
```

在 RISC-V、x86_64 或 LoongArch64 等 local-only 实现中，shared kernel mapping 的 unmap 还需要上层 处理器间中断 等待远端 CPU 完成失效。AArch64 inner-shareable TLBI 由硬件覆盖 shareable domain，但仍必须保留架构要求的 DSB/ISB 顺序。

### 8.4 客户机映射实例

假设 Guest 客户机物理地址 `0x4000_0000..0x4020_0000` 映射到 Host 物理地址 `0x9000_0000..0x9020_0000`。当 `allow_huge=true` 且 geometry 允许 2 MiB block 时，flexible mapper可以生成一个 block entry；任一端不对齐时必须下降到 base-page entries。

| 客户机物理地址 | 主机物理地址 | 大小 | 选择 |
| --- | --- | ---: | --- |
| `0x4000_0000` | `0x9000_0000` | 2 MiB | 可使用 2 MiB block |
| `0x4000_1000` | `0x9000_0000` | 2 MiB | 客户机物理地址 未对齐，降级或返回 alignment error |
| `0x4000_0000` | `0x9000_1000` | 2 MiB | 主机物理地址 未对齐，降级或返回 alignment error |
| `0x4000_0000` | `0x9000_0000` | 12 KiB | 使用三个 4 KiB leaf |

Stage-2 entry 只描述 Guest translation，不拥有 Guest RAM policy。allocation-backed Guest RAM 由 `axaddrspace` backend 保存并在 transaction finalize/teardown 释放；linear Guest mapping 删除 entry 时不能释放调用方传入的 Host 物理地址。
