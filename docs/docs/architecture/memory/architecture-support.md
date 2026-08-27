---
sidebar_position: 3
sidebar_label: "多架构实现"
---

# 多架构内存实现

TGOSKits 在 x86_64、AArch64、RISC-V 64 和 LoongArch64 上共享内存图、分配器和虚拟区域管理，只把地址转换、页表项编码、页表根寄存器、地址转换后备缓冲区失效和缓存维护放在架构实现中。本章给出公共算法与架构代码的对应关系。

## 1. 公共边界

架构差异通过 `someboot::ArchTrait`（定义于 `platforms/someboot/src/lib.rs`，各架构 `arch/*/mod.rs` 提供实现）、`axcpu` 的主机页表项与 `ArchPagingMeta`、`axvm` 的第二阶段页表 adapter，以及平台缓存操作进入各自上下文。`page-table-generic`、Buddy 与 Slab 不包含 `target_arch` 分支。

### 1.1 能力矩阵

下表描述当前 64 位主线实现。基础页均为 4 KiB，但页表级数、物理地址宽度和失效范围不同。

| 能力 | x86_64 | AArch64 | RISC-V 64 | LoongArch64 |
| --- | --- | --- | --- | --- |
| 启动 RAM 来源 | UEFI memory map，动态平台也可接固件表 | UEFI 或 U-Boot 传递的设备树 | U-Boot/OpenSBI 传递的设备树 | UEFI 或固件传递的设备树 |
| 启动物理到虚拟 | 重定位前恒等，重定位后加 `PHYS_VIRT_OFFSET` | 加 `PAGE_OFFSET` | 加 `PAGE_OFFSET` | RAM 加 `PAGE_OFFSET`，输入输出内存使用 `IO_BASE` |
| 第一阶段页表 | 4 级，48 位虚拟地址 | 4 级，48 位虚拟地址 | 仅 Sv39（3 级）；Sv48x4 只用于第二阶段 | 4 级，48 位虚拟地址 |
| 页表根 | `CR3` | `TTBRx_EL1` 或 EL2 对应寄存器 | `satp` | 内核 `PGDH`、用户 `PGDL` |
| 本地页失效 | `invlpg`/全量刷新 | 单地址 `tlbi vaae1is`（IS 广播）/全量 `tlbi vmalle1`（本核） | `sfence.vma` | `invtlb` |
| 设备内存属性 | 禁用缓存和写穿透页表位 | `MAIR_ELx` + `AttrIndx` | 标准页表位；玄铁扩展提供缓存/强序属性 | `MAT` 编码 |
| DMA 缓存维护 | 无缓存维护，执行 `mfence` 序列化屏障 | 显式 clean/invalidate 与屏障 | 由平台能力决定 | 当前以数据屏障保证顺序 |

失效经 `TableMeta::flush()` 进入各架构本 CPU 指令；公共层没有 Local/HardwareBroadcast 一类的范围声明类型。多 CPU 地址空间在发布页表修改前，必须由操作系统通过处理器间中断或架构提供的远程 fence 覆盖其他 CPU（`ax_hal::cache::flush_tlb_range_all_cpus()`）；AArch64 的地址级 `tlbi vaae1is` 在 inner-shareable 域硬件广播，但其全量 `tlbi vmalle1` 只作用于本核。

### 1.2 源码坐标

公共 trait 和架构实现分开放置，便于新增架构时逐项补齐，而不是复制整个页表算法。

| 源码 | 内容 |
| --- | --- |
| `platforms/someboot/src/lib.rs` + `src/arch/*/mod.rs` | `ArchTrait` 定义与各架构实现：地址转换、页表根、缓存和 CPU 启动 |
| `platforms/someboot/src/arch/*/paging.rs`（AArch64 为 `paging/` 目录） | 启动页表格式和寄存器切换 |
| `components/axcpu/src/paging.rs` | `MappingFlags`、`ArchPagingMeta` 与 `TableMeta::flush` 接线 |
| `components/axcpu/src/{aarch64,riscv,x86_64,loongarch64}/paging.rs` | 架构页表项标志、级数、地址宽度和本地失效实现 |
| `memory/page-table-generic/src/` | 可变页大小与层级的无架构递归实现 |
| `components/axklib/src/dma.rs` | 架构无关 DMA owner 到平台缓存操作的接线 |

新增架构时应在这些边界逐项补齐地址转换、页表项、根寄存器、失效和缓存操作，而不是在公共 allocator 或区域容器中增加目标架构分支。

## 2. x86_64

x86_64 的特殊约束集中在应用处理器早期启动和本地地址转换后备缓冲区失效。early arena 的选择与其他架构一致（排序后第一个大于 8 MiB 的 `Free` 段），没有 4 GiB 地址上限；应用处理器 trampoline 通过 `reserve_arch_early_ranges()` 在低地址单独预留一页。

### 2.1 启动内存与地址转换

应用处理器 trampoline 位于 `AP_TRAMPOLINE_PADDR`（低地址 0x8000），在 32 位启动阶段用 `movl` 装载 `CR3`；该页由启动内存图单独保留，与 early arena 的位置无关。4 GiB 以上的 `Free` 描述符照常进入最终内存图。

`platforms/someboot/src/arch/x86_64/mod.rs::Arch::_va()` 在内核重定位前返回恒等地址，重定位后增加 `PHYS_VIRT_OFFSET`。`cpu_area_phys_to_virt()` 使用独立的 `PERCPU_BASE`，因此每 CPU 区域不能通过普通内核镜像地址公式反推物理地址。

```text
early arena = first sorted Free region > 8 MiB (address-agnostic)
AP trampoline page (low memory) -> separately Reserved
all remaining Free descriptors -> independent Buddy sections
```

这三条规则相互独立：低地址 trampoline 解决应用处理器启动限制，early arena 解决启动对象分配，剩余所有 Free 描述符才进入运行时 Buddy section。

### 2.2 页表与一致性

`ArchPagingMeta` 的 x86_64 常量来自 `components/axcpu/src/x86_64/paging.rs`，配置 4 级、48 位虚拟地址和最多 52 位物理地址。`X64Pte` 把公共读、写、执行、用户和设备属性转换为 `PRESENT`、`WRITABLE`、`NO_EXECUTE`、`USER`、`NO_CACHE` 与 `WRITE_THROUGH`。

`ArchPagingMeta::flush()` 经 `ax_cpu::asm::flush_tlb` 执行本 CPU 的单页（`invlpg`）或全量（重写 CR3）失效。因此共享内核映射被其他 CPU 使用时，上层必须先完成页表写入，再经 `ax_hal::cache::flush_tlb_range_all_cpus()` 发起远程失效，并在所有目标 CPU 确认后才释放被替换的物理页。

## 3. AArch64

AArch64 同时存在异常级别和内存属性寄存器差异。启动页表和运行时页表必须使用同一套 Memory Attribute Indirection Register（内存属性间接寄存器，MAIR）槽位，否则相同页表项索引会在切换后解释为不同缓存属性。

### 3.1 地址空间与页表根

`Arch::_va()` 对普通 RAM 增加 `PAGE_OFFSET`，每 CPU 区域再增加独立的高地址偏移。`virt_to_phys()` 分别识别每 CPU 区、重定位后的内核镜像和普通线性映射，避免用单一减法处理不同虚拟窗口。

`someboot` 通过编译期 `hv` feature 选择 EL1（`TTBR0/1_EL1`）或 EL2（`TTBR0_EL2`）页表寄存器模块，并在切换页表根后失效地址转换缓存。用户页表不需要复制全部内核映射，`user_aspace_needs_kernel_mappings()` 返回 false。

### 3.2 内存属性与失效

`components/axcpu/src/aarch64/paging.rs` 中的私有 `A64MemAttr` 枚举与 `pub(super) const MAIR_VALUE` 是页表项 `AttrIndx` 和 `MAIR_ELx` 的运行时事实来源。当前槽位为 Device-nGnRE、Normal write-back 和 Normal non-cacheable；`MAIR_VALUE` 由 `MAIR_EL1::Attr0/1/2` 字段值在 const 块中计算，结果为 `0x44ff04`。启动侧额外写入第四个 WriteThrough transient 槽位（见[页表分层](./page-table.md#5-aarch64-内存属性)）。

第一阶段按地址失效使用 `tlbi vaae1is`，全量失效使用 `tlbi vmalle1`（无 IS 后缀，仅本核），两者都跟随 DSB/ISB。`vaae1is` 的 `IS` 后缀在 inner-shareable 域广播，因此地址级失效可覆盖共享域内的其他 CPU；全量失效仍需软件 shootdown 配合。

DMA 从缓存内存切换为非缓存映射前，平台执行 clean/invalidate 和全系统数据同步屏障，防止旧缓存行在属性切换后回写覆盖设备数据。

## 4. RISC-V 64

RISC-V 将页表模式编码在 `satp`。当前 someboot 与运行时都只使用 Sv39：`write_satp()` 写入 `SATP_MODE_SV39`，`axcpu::riscv::paging` 只提供 Sv39 几何（3 级）；Sv48 只以 Sv48x4 形式用于第二阶段嵌套页表（`hgatp`）。

### 4.1 启动地址与模式

`Arch::_va()` 对物理地址增加 `PAGE_OFFSET`。在用户空间或虚拟化构建中，`virt_to_phys()` 依次识别每 CPU 区、重定位后的内核镜像和普通线性映射；未启用重定位时保留恒等转换。

`write_satp()` 写入 `SATP_MODE_SV39 | (root_paddr >> 12)`，紧接着执行 `sfence.vma zero, zero`。启动或切换根页表时不能只更新软件保存的地址而遗漏硬件寄存器和 fence。

### 4.2 页表项与远程失效

`Rv64Pte` 使用标准 V/R/W/X/U/G/A/D 位。玄铁 C9xx feature 额外编码 shareable、bufferable、cacheable 和 strong-order 位；这属于处理器扩展，不应成为其他 RISC-V 平台的默认假设。

`ArchPagingMeta::flush()` 经 `ax_cpu::asm::flush_tlb` 对当前 CPU 执行单地址或全地址 `sfence.vma`。多 CPU 系统必须通过处理器间中断让运行同一地址空间的其他 hart 执行对应失效。

## 5. LoongArch64

LoongArch64 区分直接映射窗口与页表映射，并为内核和用户半区提供不同页表根。物理地址可能带直接映射窗口高位，因此固件范围进入公共内存图前必须规范化。

### 5.1 地址规范化与映射窗口

`Arch::canonicalize_paddr()` 调用 `addrspace::to_phys()` 去除直接映射窗口编码。普通 RAM 的 `_va()` 使用 `PAGE_OFFSET`，内存映射输入输出的 `_io()` 使用 `IO_BASE`；`ioremap_device()` 还检查范围非空、加法不溢出并且不超过平台物理地址宽度。

该分离意味着设备寄存器不能作为普通 RAM 传入 allocator。设备树或 UEFI parser 必须先保留 RAM/内存映射输入输出类型，再由不同虚拟窗口映射。

### 5.2 页表根与属性

内核高半区页表根写入 `PGDH`，用户低半区页表根写入 `PGDL`，地址空间标识符写入 ASID 寄存器。someboot 写根后调用 `paging::local_flush_tlb_all()`（`dbar 0; tlbflush` 指令）并执行 `dbar`/`ibar`，确保数据与指令观察到新翻译。

`La64Pte` 用 Memory Access Type（内存访问类型，MAT）区分强序非缓存、相干缓存和弱序非缓存。运行时通用失效路径使用 `invtlb 0x05` 失效单地址，或 `invtlb 0x00` 全量失效，只作用于本核，因此多 CPU 仍需要上层远程失效协调。

## 6. 架构接入约束

公共 allocator、虚拟区域管理和系统策略不根据架构复制实现。架构层只提供地址转换、当前 CPU、页表项、失效与缓存维护能力，具体调用链分别在运行时分配器、页表、DMA 和内存映射输入输出文档维护。

### 6.1 公共算法边界

Buddy、Slab、统计和普通/DMA32 页分配入口在四个架构上使用同一控制流；Dma32 只消费平台转换后的物理地址，每 CPU Slab 只消费 `ax-percpu` 发布的 CPU-local area。详细接线见[运行时页与堆分配器](./runtime-allocator.md#53-多架构运行时差异)，本章不重复列出相同矩阵。

若新架构需要不同基础页大小或非相干 DMA，应扩展公共配置和平台 capability，而不能在 allocator 调用方散布 `target_arch` 条件。

### 6.2 新架构接入规则

新增架构时先实现物理地址规范化、普通 RAM 与设备窗口转换、当前 CPU 定位、页表项编码、页表根切换、地址转换缓存失效和 DMA 缓存维护，再接入公共 allocator。不能以“先让 Buddy 工作”为由假设内核虚拟地址等于物理地址；该假设会在重定位、Dma32 和设备访问路径中产生不同结果。

页表失效能力通过 `TableMeta::flush()` 提供本核指令。若架构没有硬件广播（或只有地址级广播），多 CPU 系统必须在解除共享映射的路径接入 `ax_hal::cache::flush_tlb_range_all_cpus()` 一类的远程失效能力；不能在构建成功后才由某个调用点临时判断是否需要处理器间中断。
