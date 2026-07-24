---
sidebar_position: 2
sidebar_label: "源码结构"
---

# 内存管理源码结构

本章按资源所有权列出内存子系统的源码入口、依赖方向和修改边界。阅读具体算法前，应先确定问题发生在固件事实、物理页所有权、地址翻译、操作系统策略还是设备能力层，避免在上层重复实现底层机制。

## 1. 源码分层

内存主线由启动期、公共机制、系统策略和设备能力四组源码组成。目录层级不是调用深度；例如 `ax-page-table` 通过 `PageFrameProvider` 获取页表页，但它不直接依赖 `ax-alloc`。

### 1.1 公共组件

下表中的组件维护跨 ArceOS、StarryOS 和 Axvisor 共享的不变量。普通消费者只应依赖公共入口，不应直接访问 Buddy 或 Slab 的内部结构。

| 源码目录 | Crate 或模块 | 负责的事实 | 主要入口 |
| --- | --- | --- | --- |
| `memory/memory_addr/` | `ax-memory-addr` | 主机物理地址、虚拟地址、区间和页对齐 | `PhysAddr`、`VirtAddr`、`AddrRange` |
| `components/kernutil/src/memory.rs` | `kernutil::memory` | 固定容量启动内存图及区间覆盖 | `MemoryDescriptor`、`MemoryMapExt::merge_add()` |
| `memory/ax-alloc/` | `ax-alloc` | 运行时页、内核堆、全局分配器和统计 | `global_init()`、`global_add_memory()`、`alloc_pages()` |
| `memory/buddy-slab-allocator/` | `buddy-slab-allocator` | 多段 Buddy 与每 CPU Slab 算法 | `GlobalAllocator`，仅供 `ax-alloc` 集成 |
| `memory/ax-page-table/` | `ax-page-table` | 页表项、第一阶段、第二阶段和启动页表机制 | `PageFrameProvider`、`TlbInvalidator`、`PageTable64` |
| `memory/memory_set/` | `ax-memory-set` | 虚拟内存区域集合和映射事务 | `MemorySet`、`MemoryArea`、`MappingBackend` |
| `memory/starry-mm/` | `starry-mm` | Linux 兼容记账、提交策略和缺页能力边界 | `MemoryAccounting`、`AddressSpaceCommit`、`FaultOutcome` |
| `memory/dma-api/` | `dma-api` | DMA 设备约束和资源所有权 | `DeviceDma`、`DmaAllocation`、`DmaMapping` |
| `memory/mmio-api/` | `mmio-api` | 内存映射输入输出能力和易失性访问 | `Mmio`、`MmioRaw`、`MmioOp` |

`buddy-slab-allocator` 是算法实现，不是第二个公共分配入口。若新消费者需要页，应扩展 `ax-alloc` 的类型化接口；若页表需要不同来源，应实现 `PageFrameProvider`，而不是让页表 crate 反向依赖操作系统。

### 1.2 启动与系统集成

启动和操作系统目录负责把平台事实接到公共机制。它们可以包含策略，但不能复制公共 allocator、页表项或虚拟内存区域容器。

| 源码目录 | 所属路径 | 主要职责 |
| --- | --- | --- |
| `platforms/someboot/src/fdt/memory.rs` | 动态设备树启动 | 收集全部 RAM bank、reservation block 和 `/reserved-memory` |
| `platforms/someboot/src/efi_stub/memmap.rs` | UEFI 启动 | 把 UEFI memory type 归一为 `Free`、`Reserved` 或 `Mmio` |
| `platforms/someboot/src/mem/` | 早期启动 | 选择线性分配区、分配启动对象、冻结并发布最终内存图 |
| `platforms/axplat-dyn/src/mem.rs` | 动态平台 | 把启动描述符转换为固定容量平台内存区 |
| `os/arceos/modules/axhal/src/mem.rs` | ArceOS 硬件抽象 | 扣除保留区并进行基础页对齐 |
| `os/arceos/modules/axruntime/src/lib.rs` | ArceOS 运行时 | 初始化第一个 Buddy section，并加入其余不连续内存段 |
| `os/arceos/modules/axmm/` | ArceOS 策略 | 内核和用户第一阶段地址空间、线性映射与按需分配后端 |
| `os/StarryOS/kernel/src/mm/` | StarryOS 接线 | Linux 虚拟内存区域后端、文件系统、页缓存、系统调用和信号转换 |
| `virtualization/axaddrspace/` | Axvisor 策略 | 客户机物理地址空间、客户机 RAM 和第二阶段映射 |
| `components/axklib/src/dma.rs` | DMA 平台适配 | 把 `DeviceDma` 接到页分配、地址转换和缓存维护 |
| `components/axklib/src/mmio.rs` | MMIO 平台适配 | 把设备寄存器窗口接到内核地址空间映射能力 |

StarryOS 的 Linux 语义留在 `starry-mm` 和 Starry kernel，Axvisor 的客户机策略留在 `axaddrspace`。二者都可使用相同页表机制和物理页入口，但不会共享同一个虚拟内存策略对象。

## 2. 依赖方向

依赖必须从策略指向机制，从设备驱动指向能力接口。下图同时给出直接 crate 依赖和运行时注入边界；虚线表示通过 trait 或平台函数注入，并不要求底层 crate 直接依赖上层。

### 2.1 组件依赖图

该图省略日志、错误和同步基础库，只展示会影响内存所有权的主路径。

```mermaid
flowchart TB
    subgraph Boot["启动事实与交接"]
        FW["UEFI / U-Boot / DTB"] --> SB["someboot"]
        SB --> KU["kernutil::memory"]
        SB --> PLAT["axplat-dyn / ax-hal"]
    end

    subgraph Core["公共内存机制"]
        ADDR["ax-memory-addr"]
        ALLOC["ax-alloc"] --> BUDDY["buddy-slab-allocator"]
        PT["ax-page-table"] --> ADDR
        SET["ax-memory-set"] --> ADDR
        SMM["starry-mm"] --> ADDR
    end

    subgraph Policy["并列策略层"]
        AXMM["ax-mm"] --> SET
        AXMM --> PT
        STARRY["Starry kernel mm"] --> SMM
        STARRY --> SET
        STARRY --> PT
        GUEST["axaddrspace"] --> SET
        GUEST --> PT
    end

    subgraph Device["设备能力"]
        DMA["dma-api"]
        MMIO["mmio-api"]
        KDMA["axklib::dma"] --> DMA
        KMMIO["axklib::mmio"] --> MMIO
    end

    PLAT --> ALLOC
    AXMM -. "PageFrameProvider" .-> ALLOC
    STARRY -. "PageFrameProvider" .-> ALLOC
    GUEST -. "PageFrameProvider" .-> ALLOC
    KDMA --> ALLOC
    KMMIO --> AXMM
```

公共页表层只知道“如何申请、释放和访问一个页表帧”，不知道该页来自 Buddy、启动线性分配器还是测试 provider。这个边界消除了启动页表依赖运行时堆的循环。

### 2.2 禁止的反向依赖

以下依赖会制造第二份所有权或把系统策略泄漏到公共层，因此不应出现。

| 禁止方向 | 原因 | 正确边界 |
| --- | --- | --- |
| `ax-page-table → ax-alloc` | 启动页表尚不能使用运行时 allocator | 上层实现 `PageFrameProvider` |
| `buddy-slab-allocator → ax-alloc` | 算法层不应知道公共用途和统计 | `ax-alloc` 包装算法层 |
| `starry-mm → Starry kernel/VFS/task` | 可复用策略不能依赖操作系统对象 | `VmFile`、`PageSource`、`FaultOutcome` capability |
| `dma-api → ax-alloc` | 设备能力接口不能选择全局 allocator | `axklib::dma` 或 OS adapter 实现 `DeviceDma` |
| 驱动 → `ax-mm::iomap()` | 驱动不应绑定某个操作系统地址空间 | 驱动依赖 `mmio-api` |
| ArceOS/StarryOS → Buddy 内部类型 | 绕过公共统计、zone 和所有权 | 只调用 `ax-alloc` |

## 3. 关键调用链

调用链以“谁发布下一个阶段能够信任的事实”为主线。定位故障时，应从输出异常的阶段向上游核对，而不是直接修改最终消费者。

### 3.1 启动到运行时

动态平台的主调用链如下。不同架构的入口和页表寄存器不同，但内存图归一和 allocator 交接使用同一协议。

```text
firmware entry
  -> fdt::memory::init_memory_map() / efi_stub::memmap
  -> kernutil::memory::MemoryMapExt::merge_add()
  -> mem::early_init()
  -> mem::ram::init()
  -> boot page tables + saved DTB + per-CPU areas
  -> mem::memory_map_setup()
  -> mem::ram::freeze()
  -> axplat-dyn::mem
  -> axhal::mem::memory_regions()
  -> axruntime::init_allocator()
  -> ax_alloc::global_init() + global_add_memory()
  -> ax_alloc::init_percpu_slab(cpu_id)
```

`memory_map_setup()` 是单向交接点。它先把线性分配器尚未发布的已用前缀加入保留区，再冻结分配器；冻结后的 `Free` 描述符才允许进入 Buddy。

### 3.2 运行时请求

每种请求都有唯一分配和释放链。若新增代码无法指出最终 owner 和释放动作，说明边界尚未完整。

| 请求 | 分配调用链 | 释放动作 |
| --- | --- | --- |
| Rust 小对象 | `GlobalAlloc::alloc()` → `ax-alloc` → 当前 CPU Slab | 同一布局进入 Slab；跨 CPU 释放排入 owner 的 remote-free 链 |
| Rust 大对象 | `GlobalAlloc::alloc()` → `ax-alloc` → Buddy | 根据原 `Layout` 归还 Buddy |
| 显式页 | `alloc_pages(PageRequest, UsageKind)` → Buddy section | `GlobalPage::drop()` 使用保存的 request 和 usage 归还 |
| 页表页 | 策略层 provider → `ax-alloc` | 页表销毁时由同一 provider 释放 |
| Starry 匿名页 | 缺页 backend → `ax-alloc` → 页表映射 → RSS 记账 | 解除页表映射、撤销记账、最后归还物理页 |
| 客户机 RAM | `axaddrspace` backend → `ax-alloc` → 第二阶段页表 | 客户机解除映射或虚拟机销毁 |
| DMA buffer | `DeviceDma` → `axklib::dma` → `ax-alloc` | 资源所有者 Drop 或按值消费 token |

## 4. 修改定位规则

修改位置由需要维护的不变量决定。将逻辑放进错误层级通常会产生兼容包装、重复统计或无法裁剪的反向依赖。

### 4.1 按问题选择目录

下面的规则覆盖常见改动。如果一个需求同时跨越两层，应先保持底层机制通用，再在上层实现策略，不应把两层合并为一个大接口。

| 需求 | 修改位置 | 不应修改的位置 |
| --- | --- | --- |
| 新固件 RAM 类型或保留区 | `someboot` 与必要的平台转换 | Buddy、Starry 虚拟内存区域 |
| 新页用途统计 | `ax-alloc::UsageKind` 及消费者 | `buddy-slab-allocator` 内部 |
| Buddy 拆分、合并或低地址筛选 | `buddy-slab-allocator` | 操作系统地址空间层 |
| 新架构页表项或失效指令 | `ax-page-table/src/entry/arch`、`stage1/arch` | `ax-mm`、Starry syscall |
| 跨区域 map/unmap/protect 原子性 | `ax-memory-set` 与 backend transaction | allocator |
| Linux 超额承诺、写时复制或文件映射 | `starry-mm` / Starry kernel `mm` | `ax-alloc` |
| 客户机第二阶段策略 | `axaddrspace` / AxVM | ArceOS `ax-mm` |
| 新 DMA 地址约束 | `dma-api` 和平台 adapter | 驱动私有 allocator |
| 新寄存器窗口映射 | `mmio-api` 和平台 adapter | 驱动直接调用 OS 映射函数 |

### 4.2 阅读顺序

理解或修改内存代码时，推荐按本章、总体架构、具体机制、多架构实现、锁与并发、系统策略和测试验收的顺序阅读。这样先建立资源流和源码坐标，再进入算法与平台差异，避免从单个调用点推断整个系统行为。
