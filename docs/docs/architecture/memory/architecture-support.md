---
sidebar_position: 3
sidebar_label: "多架构实现"
---

# 多架构内存实现

TGOSKits 在 x86_64、AArch64、RISC-V 64 和 LoongArch64 上共享内存图、分配器和地址空间事务，只把地址转换、页表项编码、页表根寄存器、地址转换后备缓冲区失效和缓存维护放在架构实现中。本章给出公共算法与架构代码的对应关系。

## 1. 公共边界

架构差异通过 `someboot::ArchTrait`、`ax-page-table` 的页表项类型与 `PagingMetaData`、以及平台缓存操作进入公共主线。Buddy 与 Slab 不包含 `target_arch` 分支。

### 1.1 能力矩阵

下表描述当前 64 位主线实现。基础页均为 4 KiB，但页表级数、物理地址宽度和失效范围不同。

| 能力 | x86_64 | AArch64 | RISC-V 64 | LoongArch64 |
| --- | --- | --- | --- | --- |
| 启动 RAM 来源 | UEFI memory map，动态平台也可接固件表 | UEFI 或 U-Boot 传递的设备树 | U-Boot/OpenSBI 传递的设备树 | UEFI 或固件传递的设备树 |
| 启动物理到虚拟 | 重定位前恒等，重定位后加 `PHYS_VIRT_OFFSET` | 加 `PAGE_OFFSET` | 加 `PAGE_OFFSET` | RAM 加 `PAGE_OFFSET`，输入输出内存使用 `IO_BASE` |
| 第一阶段页表 | 4 级，48 位虚拟地址 | 4 级，48 位虚拟地址 | 默认启动使用 Sv39，核心同时支持 Sv48 | 4 级，48 位虚拟地址 |
| 页表根 | `CR3` | `TTBRx_EL1` 或 EL2 对应寄存器 | `satp` | 内核 `PGDH`、用户 `PGDL` |
| 本地页失效 | `invlpg`/全量刷新 | `TLBI ...IS` 硬件广播 | `sfence.vma` | `invtlb` |
| 公共失效范围 | Local | HardwareBroadcast | Local | Local |
| 设备内存属性 | 禁用缓存和写穿透页表位 | `MAIR_ELx` + `AttrIndx` | 标准页表位；玄铁扩展提供缓存/强序属性 | `MAT` 编码 |
| DMA 缓存维护 | 一致性平台通常为空操作 | 显式 clean/invalidate 与屏障 | 由平台能力决定 | 当前以数据屏障保证顺序 |

“Local” 表示 `ax-page-table` 的默认实现只使当前 CPU 的缓存翻译失效。多 CPU 地址空间在发布页表修改前，必须由操作系统通过处理器间中断或架构提供的远程 fence 覆盖其他 CPU；AArch64 的 `...IS` 指令在 inner-shareable 域广播，因此标记为 `HardwareBroadcast`。

### 1.2 源码坐标

公共 trait 和架构实现分开放置，便于新增架构时逐项补齐，而不是复制整个页表算法。

| 源码 | 内容 |
| --- | --- |
| `platforms/someboot/src/arch/*/mod.rs` | `ArchTrait`：地址转换、页表根、缓存和 CPU 启动 |
| `platforms/someboot/src/arch/*/paging.rs` | 启动页表格式和寄存器切换 |
| `memory/ax-page-table/src/entry/arch/*.rs` | 架构页表项标志与公共 `MappingFlags` 转换 |
| `memory/ax-page-table/src/stage1/arch/*.rs` | 第一阶段级数、地址宽度和失效实现 |
| `memory/ax-page-table/src/flexible/` | 可变页大小与层级的第二阶段通用实现 |
| `components/axklib/src/dma.rs` | 架构无关 DMA owner 到平台缓存操作的接线 |

## 2. x86_64

x86_64 的特殊约束集中在应用处理器早期启动和本地地址转换后备缓冲区失效。高端 RAM 可以进入运行时 Buddy，但启动页表根必须满足 32 位 trampoline 的寻址限制。

### 2.1 启动内存与地址转换

`someboot::mem::select_early_ram()` 在 x86_64 上把 early arena 候选末端限制为 4 GiB。原因是应用处理器 trampoline 使用 32 位 `movl` 装载 `CR3`；这不会把 4 GiB 以上的 `Free` 描述符从最终内存图删除。

`platforms/someboot/src/arch/x86_64/mod.rs::Arch::_va()` 在内核重定位前返回恒等地址，重定位后增加 `PHYS_VIRT_OFFSET`。`cpu_area_phys_to_virt()` 使用独立的 `PERCPU_BASE`，因此每 CPU 区域不能通过普通内核镜像地址公式反推物理地址。

```text
firmware RAM below 4 GiB -----> early bump + boot CR3
firmware RAM above 4 GiB -----> preserved Free descriptor
                                  |
memory_map_setup + axruntime <----+
                                  |
                         independent Buddy section
```

### 2.2 页表与一致性

`X64PagingMetaData` 配置 4 级、48 位虚拟地址和最多 52 位物理地址。`X64PTE` 把公共读、写、执行、用户和设备属性转换为 `PRESENT`、`WRITABLE`、`NO_EXECUTE`、`USER_ACCESSIBLE`、`NO_CACHE` 与 `WRITE_THROUGH`。

默认 `X64TlbInvalidator` 只执行本 CPU 的单页或全量失效。因此共享内核映射被其他 CPU 使用时，上层必须先完成页表写入，再发起远程失效，并在所有目标 CPU 确认后才释放被替换的物理页。

## 3. AArch64

AArch64 同时存在异常级别和内存属性寄存器差异。启动页表和运行时页表必须使用同一套 Memory Attribute Indirection Register（内存属性间接寄存器，MAIR）槽位，否则相同页表项索引会在切换后解释为不同缓存属性。

### 3.1 地址空间与页表根

`Arch::_va()` 对普通 RAM 增加 `PAGE_OFFSET`，每 CPU 区域再增加独立的高地址偏移。`virt_to_phys()` 分别识别每 CPU 区、重定位后的内核镜像和普通线性映射，避免用单一减法处理不同虚拟窗口。

`someboot` 根据运行异常级别设置 EL1 或 EL2 页表寄存器，并在切换页表根后失效地址转换缓存。用户页表不需要复制全部内核映射，`user_aspace_needs_kernel_mappings()` 返回 false。

### 3.2 内存属性与失效

`memory/ax-page-table/src/entry/arch/aarch64.rs::MemAttrLayout` 是页表项 `AttrIndx` 和 `MAIR_ELx` 的唯一事实来源。当前槽位为 Device-nGnRE、Normal write-back 和 Normal non-cacheable，组合值为 `0x44ff04`。

```rust
pub const DEVICE_INDEX: u64 = MemAttr::Device as u64;
pub const NORMAL_INDEX: u64 = MemAttr::Normal as u64;
pub const NORMAL_NON_CACHEABLE_INDEX: u64 = MemAttr::NormalNonCacheable as u64;
pub const MAIR_VALUE: u64 = 0x44ff04;
```

第一阶段使用 `tlbi vaae1is` 或 `tlbi vmalle1is`，随后执行数据同步和指令同步屏障。`IS` 后缀在 inner-shareable 域广播，所以页表核心可以把范围标记为硬件广播；平台仍必须保证参与地址空间的 CPU 位于正确共享域。

DMA 从缓存内存切换为非缓存映射前，平台执行 clean/invalidate 和全系统数据同步屏障，防止旧缓存行在属性切换后回写覆盖设备数据。

## 4. RISC-V 64

RISC-V 将页表模式编码在 `satp`。当前 someboot 写入 Sv39 模式，统一页表核心同时提供 Sv39 和 Sv48 类型，消费者必须选择与平台启动模式和虚拟地址布局一致的类型。

### 4.1 启动地址与模式

`Arch::_va()` 对物理地址增加 `PAGE_OFFSET`。在用户空间或虚拟化构建中，`virt_to_phys()` 依次识别每 CPU 区、重定位后的内核镜像和普通线性映射；未启用重定位时保留恒等转换。

`write_satp()` 写入 `SATP_MODE_SV39 | (root_paddr >> 12)`，紧接着执行 `sfence.vma zero, zero`。启动或切换根页表时不能只更新软件保存的地址而遗漏硬件寄存器和 fence。

### 4.2 页表项与远程失效

`Rv64PTE` 使用标准 V/R/W/X/U/G/A/D 位。玄铁 C9xx feature 额外编码 shareable、bufferable、cacheable 和 strong-order 位；这属于处理器扩展，不应成为其他 RISC-V 平台的默认假设。

`RiscvTlbInvalidator` 对当前 CPU 执行单地址或全地址 `sfence.vma`，范围为 Local。多 CPU 系统必须通过远程 fence 或处理器间中断让运行同一地址空间的其他 hart 执行对应失效。

## 5. LoongArch64

LoongArch64 区分直接映射窗口与页表映射，并为内核和用户半区提供不同页表根。物理地址可能带直接映射窗口高位，因此固件范围进入公共内存图前必须规范化。

### 5.1 地址规范化与映射窗口

`Arch::canonicalize_paddr()` 调用 `addrspace::to_phys()` 去除直接映射窗口编码。普通 RAM 的 `_va()` 使用 `PAGE_OFFSET`，内存映射输入输出的 `_io()` 使用 `IO_BASE`；`ioremap_device()` 还检查范围非空、加法不溢出并且不超过平台物理地址宽度。

该分离意味着设备寄存器不能作为普通 RAM 传入 allocator。设备树或 UEFI parser 必须先保留 RAM/内存映射输入输出类型，再由不同虚拟窗口映射。

### 5.2 页表根与属性

内核高半区页表根写入 `PGDH`，用户低半区页表根写入 `PGDL`，地址空间标识符写入 ASID 寄存器。写根后执行全量 `invtlb`、`dbar` 和 `ibar`，确保数据与指令观察到新翻译。

`LA64PTE` 用 Memory Access Type（内存访问类型，MAT）区分强序非缓存、相干缓存和弱序非缓存。第一阶段默认失效器使用 `invtlb 0x05` 失效单地址，或 `invtlb 0x00` 全量失效，范围为 Local，因此多 CPU 仍需要上层远程失效协调。

## 6. 架构无关分配器

运行时 allocator 不根据架构选择另一套算法。所有架构使用相同 4 KiB base page、多 section Buddy、固定 size class Slab、`MemoryZone::Normal` 和 `MemoryZone::Dma32`。

### 6.1 实际差异入口

分配器看到的是内核虚拟地址区间，但 Dma32 判断需要物理地址。这个转换通过平台实现注入；每 CPU Slab 的当前 CPU 定位也由每 CPU 基础设施提供，而不是在 Buddy 中读取架构寄存器。

| 差异 | 公共算法看到的接口 | 架构实现位置 |
| --- | --- | --- |
| 物理/虚拟转换 | section 起点及 `virt_to_phys` 结果 | `someboot::ArchTrait`、平台内存 API |
| 低 4 GiB 筛选 | allocation 物理末地址不超过 `2^32` | Buddy lowmem filter 调用平台地址转换 |
| 当前 CPU | `with_cpu_pin` 返回的 per-CPU area | `ax-percpu` 与架构 CPU id |
| DMA cache | clean、invalidate、barrier capability | 平台架构实现 |
| 页表失效 | `TlbInvalidator` | `ax-page-table/src/stage1/arch` |

普通页、Slab object 和统计路径在四个架构上的控制流一致。若新增架构需要更小基础页或非相干 DMA，应在页大小配置和平台 capability 处显式表达，不能在调用方散布 `target_arch` 条件。

### 6.2 新架构接入规则

新增架构时先实现物理地址规范化、普通 RAM 与设备窗口转换、当前 CPU 定位、页表项编码、页表根切换、地址转换缓存失效和 DMA 缓存维护，再接入公共 allocator。不能以“先让 Buddy 工作”为由假设内核虚拟地址等于物理地址；该假设会在重定位、Dma32 和设备访问路径中产生不同结果。

页表失效能力还必须声明 Local 或 HardwareBroadcast。若只提供 Local，多 CPU 系统应在地址空间初始化时安装远程失效能力；不能在构建成功后才由某个调用点临时判断是否需要处理器间中断。
