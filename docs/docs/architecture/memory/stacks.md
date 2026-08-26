---
sidebar_position: 6
sidebar_label: "栈管理"
---

# 启动栈、内核任务栈与用户栈

TGOSKits 没有把物理 RAM 静态切成一个“栈区”和一个“堆区”。CPU0 最早期栈来自内核镜像 `.bss`，每 CPU 启动栈由 `someboot` 早期线性分配器预分配，普通内核任务栈从运行时分配器获取，Starry 用户栈则是用户地址空间中的 Virtual Memory Area（虚拟内存区域，VMA）。

## 1. 栈类型与生命周期

不同栈存在于不同启动阶段和地址空间。区分这些栈是分析内存占用、guard page 和释放行为的前提。

### 1.1 栈来源总览

当前主要栈类型如下。默认大小来自当前 linker/build 配置，平台配置可以覆盖任务栈大小。

| 栈类型 | 默认大小 | 来源 | 生命周期 |
| --- | --- | --- | --- |
| CPU0 最早期 linker 栈 | `STACK_SIZE = 0x40000`，256 KiB | kernel `.bss` / `KImage` | 启动早期，镜像范围始终保留 |
| 每 CPU boot/main 栈 | `someboot::mem::stack_size()`，默认 256 KiB | early bump 的 per-CPU 区 | 系统生命周期，bootstrap resource 不持有可释放 stack handle |
| 普通内核任务栈 | 默认 `0x40000`，可由构建配置覆盖 | `axruntime` 的 heap 或显式页 allocation | thread resource reaper 释放 |
| idle 栈 | 与运行时任务栈配置一致 | `axruntime` stack allocator | idle thread 生命周期 |
| Starry 用户栈 | loader/应用程序二进制接口选择的虚拟内存区域大小 | 用户地址空间 backend，按需填页 | exec/exit/unmap 时回收 |

栈大小不是物理连续 RAM 的全局配额。只有具体 stack allocation 会消耗页；用户栈预留的虚拟内存大小也不等于所有页面已经 resident。

### 1.2 栈与 heap 的关系

“栈和堆如何划分”在运行期表现为不同 owner 使用同一 allocator，而不是两个永久物理分区。下图展示来源关系。

![内核与用户栈资源来源](./images/stack-architecture.svg)

普通任务栈默认 256 KiB，超过 2048 B Slab 上限，因此 plain 模式最终由 Buddy 提供大对象页。启用 guard page 后则直接使用显式连续页 API。

### 1.3 架构差异

栈的 owner 与分配来源跨架构一致，架构入口仅负责把栈顶写入本架构栈寄存器并跳转：x86_64 使用 `rsp`，AArch64 和 RISC-V 使用 `sp`，LoongArch64 使用 `$sp`。启动 entry 必须在进入 Rust 前满足相应调用约定的栈对齐，栈 owner 不保存架构私有寄存器状态。

Guard page 的区别来自地址转换缓存失效：AArch64 使用 inner-shareable 硬件广播，x86_64、RISC-V 和 LoongArch64 的默认实现只处理本地 CPU，需要上层远程失效。地址窗口和指令细节统一见[多架构内存实现](./architecture-support.md)，本章后续只说明栈特有的 owner 和 guard 时序。

## 2. CPU0 启动栈

CPU0 在 allocator、完整页表和 per-CPU 映射可用之前就需要栈。这个阶段使用 linker 明确预留的静态范围，避免任何动态依赖。

### 2.1 链接布局

`platforms/someboot/src/ld/bss.ld` 在 `.bss` 末尾定义 `__cpu0_stack` 和 `__cpu0_stack_top`，并移动 location counter `STACK_SIZE`。`defaults.ld` 为该符号提供 256 KiB 默认值。

```text
.bss
├── ordinary BSS and COMMON
├── __cpu0_stack
├── STACK_SIZE bytes
└── __cpu0_stack_top
```

该范围包含在 kernel image 的结束边界中，`someboot::mem::early_init()` 将整个镜像记为 `KImage`。它不会进入 `Free`，也不会由运行时 allocator 单独释放。

### 2.2 切换到每 CPU 栈

建立目标页表和 per-CPU 映射后，`someboot::prime_entry()` 读取当前 CPU 的 `PerCpuMeta::stack_top`，转换到 per-CPU 虚拟地址，并通过架构 `jump_to()` 切换 SP 后进入 `__someboot_main`。

| 阶段 | SP 来源 | 可用能力 |
| --- | --- | --- |
| 最早架构入口 | linker CPU0 stack | 最小启动代码、扁平设备树/页表准备 |
| MMU/per-CPU 初始化后 | `PerCpuMeta::stack_top_virt` | dynamic platform main、ax-runtime |
| scheduler 初始化后 | 同一 boot stack 被 main task 借用 | 正常内核任务调度 |

切换后 linker stack 仍属于 KImage，只是不再作为 main task 的运行栈。代码不能假定该旧范围会被回收到 Buddy。

## 3. 每 CPU 启动栈

每个可启动 CPU 都在引导处理器 early boot 阶段获得自己的 boot stack。应用处理器启动不依赖通用 heap，避免并发 bring-up 时 allocator 和 per-CPU storage 尚未就绪的问题。

### 3.1 预分配布局

`platforms/someboot/src/smp/layout.rs` 只保留一种每 CPU 连续布局。`layout_info()` 从 linker template 大小、`PerCpuMeta` 大小、stack 大小、页大小和区域对齐计算偏移；所有 CPU 共享同一个 `area_stride`。

| 计算量 | 公式 | 保证条件 |
| --- | --- | --- |
| metadata offset | `align_up(data_size, meta_alignment)` | metadata 至少按 `max(align_of::<PerCpuMeta>(), 64)` 对齐 |
| stack offset | `align_up(metadata_end, page_size)` | stack 起点按页对齐 |
| area stride | `align_up(stack_end, region_alignment)` | 每个 CPU slot 可独立寻址 |
| allocation size | `area_stride * cpu_count` | checked multiplication，不能回绕 |

`alloc_percpu()` 按固件 CPU 数一次申请完整区域。最终高地址初始化阶段复制 linker per-CPU template，并为每个 CPU 写入 hardware ID、logical index、stack top 和 secondary entry；完成 cache maintenance 后才发布运行期 CPU 数。

### 3.2 调度器借用

动态平台的 `boot_stack_bounds(cpu_idx)` 从 `somehal::smp::cpu_meta()` 返回 stack bottom 和 size。调度器安装 bootstrap thread 时只接管当前架构 context 与 TLS；`create_bootstrap_resources()` 把 stack handle 设为 `StackHandle::NONE`，明确表示该 boot stack 仍由启动层拥有。

| Owner 状态 | 运行时表示 | 回收行为 |
| --- | --- | --- |
| boot/main/secondary stack | bootstrap `ThreadResources` 中为 `StackHandle::NONE` | 不释放，仅由启动层持有物理范围 |
| plain task allocation | opaque `StackHandle` 指向 `RuntimeStack::Heap` | 用原 `Layout` 归还 runtime allocator |
| guard-page task allocation | opaque `StackHandle` 指向 `RuntimeStack::GuardedPages` | 恢复 guard 权限并完成 TLB shootdown 后归还全部页 |

`StackHandle::NONE` 在 bootstrap resource bundle 中表达“任务正在使用，但 scheduler 没有获得该 stack 的回收所有权”。这防止 bootstrap thread 退休时把 early bump 的系统级 stack 错误释放给 Buddy。

## 4. 普通内核任务栈

`components/ax-task` 只持有运行时提供的 opaque `StackHandle`；分配策略和地址空间操作属于 `os/arceos/modules/axruntime/src/task/resources.rs::RuntimeStack`。普通任务创建先分配 stack/TLS/context，再把三个 handle 作为一个 `ThreadResources` bundle 转交给 scheduler；线程退出后由 resource reaper 按相反顺序销毁。

### 4.1 普通分配

未启用 `stack-guard-page` 时，`allocate_heap_stack()` 使用请求的 usable size 与 alignment 构造 `Layout`，再经 `ax_alloc::global_allocator()` 分配。ArceOS 传入 16 字节 alignment；默认 256 KiB 请求走 Buddy 大对象路径。

| 操作 | 实现 | 失败语义 |
| --- | --- | --- |
| allocation | `global_allocator().alloc(layout)` | 返回 typed `RuntimeStatus` |
| publication | `Box<RuntimeStack>` 转为唯一 non-zero `StackHandle` | handle 必须只转交和销毁一次 |
| release | `global_allocator().dealloc(ptr, layout)` | 必须使用原 size/align |

plain stack 没有页级溢出隔离。需要越界后立即 fault 的配置应启用 guard page。

### 4.2 保护页分配

启用 `stack-guard-page` 后，`allocate_guarded_stack()` 申请 `usable pages + guard pages` 个连续 Normal 页，通过 `protect_kernel_range(..., MappingFlags::empty())` 去掉最低 guard range 的访问权限，并把 initial stack top 放在 allocation 末端。

![带保护页的内核任务栈布局](./images/guarded-stack-layout.svg)

回收时先通过同一个 `protect_kernel_range()` 恢复 guard range 的读写权限并完成全 CPU TLB shootdown，再按原页数和用途释放整段 allocation。恢复或 shootdown 失败时返回 `RuntimeStatus::Platform` 并保留 backing，不能在映射状态不确定时交还 Buddy。

## 5. 栈保护一致性

改变 kernel stack guard 页表项后必须让可能缓存该映射的 CPU 失效。单核与 多核 使用不同路径，但都在继续使用或释放页面前完成。

### 5.1 权限更新与失效

`protect_kernel_range()` 先在 kernel address space 中更新权限，再统一调用 `ax_hal::cache::flush_tlb_range_all_cpus()`。单核平台把该 scope 收敛为本地失效；多核平台由 HAL 选择硬件广播或跨 CPU shootdown。

| 事件 | 页表项操作 | 地址转换后备缓冲区操作 |
| --- | --- | --- |
| stack 创建 | guard range 权限设为空 | all-CPU range flush |
| stack 回收 | guard range 恢复读写 | all-CPU range flush |

页表修改成功并不自动替代架构间的 Translation Lookaside Buffer（地址转换后备缓冲区，TLB）失效。guard stack 代码显式完成这一职责，因为它修改的是所有 CPU 可见的 kernel address space。

### 5.2 多核实现

多核 shootdown 的 ready CPU 选择、doorbell 与完成确认属于 `ax_hal::cache::flush_tlb_range_all_cpus()` 的架构实现，stack allocator 不再维护第二套 IPI 协议。

| 约束 | 当前实现 |
| --- | --- |
| CPU 选择 | HAL 只覆盖当前可参与 shootdown 的 CPU |
| 顺序 | 修改权限后执行 scoped shootdown |
| 失败 | 传播为 runtime platform error，不释放 backing |
| 页面释放 | 仅在权限恢复与 shootdown 完成后执行 |

shootdown 失败是内核映射一致性失败，而不是可忽略的性能告警。若某架构提供硬件 broadcast，HAL 可以直接完成该 scope；否则由通用远端失效路径保证完成。

## 6. Starry 用户栈

Starry 用户栈属于用户虚拟地址空间，不是 runtime `StackHandle`。loader 和进程内存策略建立 stack 虚拟内存区域，物理页由缺页或 populate 路径按需分配。

### 6.1 虚拟区与驻留页

用户栈的虚拟范围计入虚拟内存大小，只有已映射的匿名页计入常驻内存集大小。`os/StarryOS/kernel/src/mm/stats.rs::ProcessMemStats` 通过 `[stack]` 名称或进程 stack range 将虚拟内存区域分类到 `stack_pages`。

| 指标 | 用户栈含义 | 物理占用关系 |
| --- | --- | --- |
| `VmStk` | 被识别为 stack 的虚拟内存区域页数 | 可能包含未驻留页 |
| `VmSize` | 全部虚拟内存区域的虚拟内存大小 | 不等于 Buddy 已分配页 |
| `RssAnon` | 已驻留匿名页 | 包含实际 fault/populate 的 stack page |
| kernel task stack | 内核态执行栈 | 不计入用户进程虚拟内存区域统计 |

用户栈释放通过 address space unmap/clear 和 backend page owner 完成，不调用 runtime kernel-stack deallocator。

### 6.2 保护边界

用户访问权限由 Stage-1 页表项和 Starry 虚拟内存区域 flags 共同决定。kernel stack guard feature 只保护 `axruntime` 分配的内核栈，不会自动给所有 Starry 用户 stack 增加 guard 虚拟内存区域。

| 边界 | 负责组件 | 故障处理 |
| --- | --- | --- |
| 用户 stack 虚拟内存区域权限 | Starry `AddrSpace` / backend | `handle_page_fault()` 返回是否成功，trap 层再处理 signal |
| kernel task guard page | `axruntime` + `ax-mm` | 诊断 `diagnose_current_stack_guard_page_fault()` |
| CPU boot stack 范围 | `someboot` / `ax-hal` | 启动配置，当前无动态 guard |

分析 stack overflow 时必须先确认 fault address 属于哪种 stack。把用户虚拟内存区域 fault 误判成 kernel guard，或把 boot stack 当作 allocator 泄漏，都会得出错误结论。

## 7. 配置与审计入口

栈行为跨链接布局、启动平台、调度器和用户虚拟内存，修改默认大小或保护页 feature 时需要同时检查这些边界。

### 7.1 配置来源

默认值存在于不同构建阶段，最终以平台和 `axbuild` 生成配置为准。重复默认值必须保持语义一致或由构建脚本明确覆盖。

| 配置 | 当前默认 | 源码入口 |
| --- | --- | --- |
| someboot `STACK_SIZE` | `0x40000` | `platforms/someboot/src/ld/defaults.ld` |
| ax-runtime task stack | `0x40000` | `os/arceos/modules/axruntime/build.rs`；作为 `StackRequest` 传给 runtime allocator |
| API exposed task stack | `0x40000` | `arceos_api` / `arceos_posix_api` config |
| user pthread compatibility default | 2 MiB | `os/arceos/ulib/axstd/src/os/libc_compat.rs` |

修改一个默认值时必须同步生成的 build info、公开 API 和实际 task creation 参数，避免文档或 resource limit 仍报告旧值。

### 7.2 源码检查点

下面的文件覆盖 stack 从静态布局到释放的完整生命周期。resource handle owner 与 guard shootdown 的用例见[内存管理测试](./testing.md)。

| 源码 | 审计重点 |
| --- | --- |
| `platforms/someboot/src/ld/bss.ld` | CPU0 linker stack 是否位于 KImage |
| `platforms/someboot/src/smp/layout.rs` | 每 CPU offset、stride、总大小和 checked arithmetic |
| `platforms/someboot/src/smp/mod.rs` | typed layout 初始化、CPU metadata 发布与 cache maintenance |
| `platforms/axplat-dyn/src/boot.rs` | `boot_stack_bounds()` 元数据来源 |
| `os/arceos/modules/axruntime/src/task/bootstrap.rs` | bootstrap thread 如何保留外部 boot stack owner |
| `components/ax-task/src/thread/spec.rs` | scheduler 如何持有并一次性释放 opaque resource handles |
| `os/arceos/modules/axruntime/src/task/resources.rs` | heap/guarded stack 分配与回收 |
| `os/arceos/modules/axruntime/src/kernel_mapping.rs` | guard 权限事务与全 CPU TLB shootdown |
| `os/StarryOS/kernel/src/mm/stats.rs` | 用户 stack 虚拟内存区域统计分类 |

容量计算应包含每 CPU 固定 stack 总开销、最大 task 数乘以配置栈大小、guard page 的额外一页以及 Starry 用户 stack 的虚拟内存大小/常驻内存集大小差异。

## 8. 栈布局实例

栈内存的地址、物理占用和释放规则取决于栈类型。下面分别计算 per-CPU 区、guarded task stack 和 Starry 用户栈，三个例子不能互换释放逻辑。

### 8.1 四 CPU 启动区

以 data=128 B、metadata=64 B、stack=4096 B、page/region alignment=4096 B 为例，对四个 CPU，计算结果是 metadata offset 128、stack offset 4096、stride 8192、总 allocation 32768 B。

```rust
let metadata_end = meta_offset.checked_add(metadata_size)?;
let stack_offset = checked_align_up_pow2(metadata_end, page_alignment)?;
let stack_end = stack_offset.checked_add(stack_size)?;
let area_stride = checked_align_up_pow2(stack_end, region_alignment)?;
let total_size = area_stride.checked_mul(cpu_count)?;
```

假设 early bump 返回区域起点 `0x8100_0000`，四个 CPU slot 的物理布局如下。

```text
CPU0 0x8100_0000..0x8100_2000
     data [0x8100_0000,0x8100_0080)
     meta [0x8100_0080,0x8100_00c0)
     pad  [0x8100_00c0,0x8100_1000)
     stack[0x8100_1000,0x8100_2000), top=0x8100_2000

CPU1 0x8100_2000..0x8100_4000, stack top=0x8100_4000
CPU2 0x8100_4000..0x8100_6000, stack top=0x8100_6000
CPU3 0x8100_6000..0x8100_8000, stack top=0x8100_8000
```

实际生产 stack 默认 256 KiB，data 和 `PerCpuMeta` 大小由 linker/template 与目标应用程序二进制接口决定，计算公式相同。所有乘加和 alignment 都返回 `PerCpuLayoutError`，极端固件 CPU 数不会 wrapping 到小 allocation。

### 8.2 保护页任务栈

启用 `stack-guard-page` 后，请求 256 KiB task stack 会把 usable size 对齐到 4 KiB，再额外申请一个 guard page，API request count 为 65 页。当前 Buddy 会把 65 页提升为 order 7 的 128 页 block；`RuntimeStack` 记录前 65 页的 base、usable top、page count 与 guard size，剩余部分属于该 allocation 的内部碎片。可见范围第一页在 kernel address space 中被设为不可访问。

```text
base                                                           base + 0x41000
| guard 4 KiB |--------------- usable stack 256 KiB ----------------|
               ^ usable bottom                           top / initial SP
```

关键分配代码直接使用显式页 API，而不是先从 `GlobalAlloc` 分配后再猜测页边界。

```rust
let usable_size = align_up_4k(size);
let guarded_size = usable_size
    .checked_add(PAGE_SIZE_4K)
    .expect("guarded task stack size overflow");
let pages = guarded_size / PAGE_SIZE_4K;
let base = ax_alloc::global_allocator()
    .alloc_pages(pages, PAGE_SIZE_4K, UsageKind::Global)
.expect("guarded task stack allocation failed");
```

源码实际把 内存不足 作为 task creation 的不可恢复初始化失败处理。guard page建立后必须执行本地或远端地址转换后备缓冲区失效，不能只删除页表项。

```mermaid
sequenceDiagram
    participant Runtime as axruntime::allocate_guarded_stack
    participant Alloc as ax-alloc
    participant KAS as kernel AddrSpace
    participant CPUs as local/remote 地址转换后备缓冲区

    Runtime->>Alloc: allocate 65 contiguous pages
    Alloc-->>Runtime: base
    Runtime->>KAS: clear access flags for first 4 KiB
    KAS->>CPUs: flush guard range for all participating CPUs
    Runtime-->>Runtime: publish unique StackHandle
```

resource reaper 回收时顺序相反：先恢复 guard page 权限并完成地址转换后备缓冲区同步，再以原 `count=65` 返回对应 Buddy block。若先 free，其他 CPU 的 stale translation 可能写入已经复用的物理页。这个实例也说明非 2 次幂连续请求的成本；是否调整 stack size 必须由最大栈深和物理开销共同决定。

### 8.3 普通任务栈

未启用 guard feature 时，`allocate_heap_stack()` 用 `Layout(usable_size, alignment)` 进入 runtime allocator。默认 256 KiB 超过 Slab 上限，最终使用 Buddy large allocation，但所有权仍表现为 byte allocation。

| 属性 | Plain stack | Guarded stack |
| --- | --- | --- |
| 入口 | `global_allocator().alloc(Layout)` | `global_allocator().alloc_pages(num, align, usage)` |
| 下层 | large `GlobalAlloc` → Buddy | Buddy pages |
| overflow 检测 | 无页级隔离 | inaccessible guard range |
| 回收 | `global_allocator().dealloc()` | 恢复 guard 权限后 raw page deallocation |
| 可否混用释放 | 否 | 否 |

bootstrap thread 的 resource bundle 不包含 stack handle，因此退出时只能退休 scheduler/context owner，不能把 someboot 的 early Reserved 区交给 runtime allocator。

### 8.4 Starry 用户栈

x86_64 当前用户栈顶部为 `0x0400_0000_0000`，虚拟内存区域大小为 8 MiB，因此起点是 `0x03ff_ff80_0000`。loader 先建立完整 `[stack]` 虚拟内存区域，再只 populate 初始 argv/envp/auxv 实际覆盖的尾部页。

```rust
let ustack_top = VirtAddr::from_usize(crate::config::USER_STACK_TOP);
let ustack_size = crate::config::USER_STACK_SIZE;
let ustack_start = ustack_top - ustack_size;
uspace.map(
    ustack_start,
    ustack_size,
    MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
    false,
    Backend::new_alloc(ustack_start, PageSize::Size4K, "[stack]"),
)?;
```

假设初始 stack image 为 13 KiB，`user_sp` 向下移动 13 KiB，populate range 再向下按页对齐，最多使 16 KiB resident。此时虚拟内存大小增加 8 MiB，常驻内存集大小 Anon只增加实际填充的四页；其余页面在后续用户访问时 fault-in。

| 用户栈量 | 示例结果 |
| --- | ---: |
| 虚拟内存区域 | 8 MiB |
| 初始 stack data | 13 KiB |
| 初始 resident upper bound | 16 KiB / 4 页 |
| 初始 SP | `USER_STACK_TOP - 13 KiB`，再满足应用程序二进制接口 alignment |

Starry 当前使用固定大小 stack 虚拟内存区域，不实现 Linux `VM_GROWSDOWN`。非 FIXED mmap 的上界还会避开 `STACK_GUARD_GAP`，但这不是一个已映射的物理 guard page；两种 guard 语义不能混用。
