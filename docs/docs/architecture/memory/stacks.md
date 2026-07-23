---
sidebar_position: 4
sidebar_label: "栈管理"
---

# 启动栈、内核任务栈与用户栈

TGOSKits 没有把物理 RAM 静态切成一个“栈区”和一个“堆区”。CPU0 最早期栈来自内核镜像 `.bss`，每 CPU 启动栈由 `someboot` early bump 预分配，普通内核任务栈从运行时 allocator 获取，Starry 用户栈则是用户地址空间中的 VMA。

## 1. 栈类型与生命周期

不同栈存在于不同启动阶段和地址空间。区分这些栈是分析内存占用、guard page 和释放行为的前提。

### 1.1 栈来源总览

当前主要栈类型如下。默认大小来自当前 linker/build 配置，平台配置可以覆盖任务栈大小。

| 栈类型 | 默认大小 | 来源 | 生命周期 |
| --- | --- | --- | --- |
| CPU0 最早期 linker 栈 | `STACK_SIZE = 0x40000`，256 KiB | kernel `.bss` / `KImage` | 启动早期，镜像范围始终保留 |
| 每 CPU boot/main 栈 | `someboot::mem::stack_size()`，默认 256 KiB | early bump 的 per-CPU 区 | 系统生命周期，`TaskStack::Borrowed` 不释放 |
| 普通内核任务栈 | 默认 `0x40000`，可由构建配置覆盖 | `GlobalAlloc` 或显式页 allocation | task owner Drop 时释放 |
| idle 特殊栈 | 取决于构建 feature，部分配置为 16 KiB | task allocator | idle task 生命周期 |
| Starry 用户栈 | loader/ABI 选择的 VMA 大小 | 用户地址空间 backend，按需填页 | exec/exit/unmap 时回收 |

栈大小不是物理连续 RAM 的全局配额。只有具体 stack allocation 会消耗页；用户栈预留的 VSS 也不等于所有页面已经 resident。

### 1.2 栈与 heap 的关系

“栈和堆如何划分”在运行期表现为不同 owner 使用同一 allocator，而不是两个永久物理分区。下图展示来源关系。

```mermaid
flowchart TB
    KImage["kernel image"] --> EarlyStack["CPU0 earliest stack\n.bss"]
    Bump["someboot early bump"] --> CpuStacks["all CPU boot stacks"]
    Buddy["ax-alloc Buddy"] --> Large["large GlobalAlloc"]
    Large --> TaskStacks["plain kernel task stacks"]
    Buddy --> Guarded["page allocation"]
    Guarded --> GuardStacks["guarded kernel task stacks"]
    Buddy --> UserPages["Starry fault-populated pages"]
    UserPages --> UserStack["user stack VMA"]
```

普通任务栈默认 256 KiB，超过 2048 B Slab 上限，因此 plain 模式最终由 Buddy 提供大对象页。启用 guard page 后则直接使用显式连续页 API。

## 2. CPU0 启动栈

CPU0 在 allocator、完整页表和 per-CPU 映射可用之前就需要栈。这个阶段使用 linker 明确预留的静态范围，避免任何动态依赖。

### 2.1 Linker 布局

`platforms/someboot/src/ld/bss.ld` 在 `.bss` 末尾定义 `__cpu0_stack` 和 `__cpu0_stack_top`，并移动 location counter `STACK_SIZE`。`defaults.ld` 为该符号提供 256 KiB 默认值。

```text
.bss
├── ordinary BSS and COMMON
├── __cpu0_stack
├── STACK_SIZE bytes
└── __cpu0_stack_top
```

该范围包含在 kernel image 的结束边界中，`someboot::mem::early_init()` 将整个镜像记为 `KImage`。它不会进入 `Free`，也不会由运行时 allocator 单独释放。

### 2.2 切换到 per-CPU 栈

建立目标页表和 per-CPU 映射后，`someboot::prime_entry()` 读取当前 CPU 的 `PerCpuMeta::stack_top`，转换到 per-CPU 虚拟地址，并通过架构 `jump_to()` 切换 SP 后进入 `__someboot_main`。

| 阶段 | SP 来源 | 可用能力 |
| --- | --- | --- |
| 最早架构入口 | linker CPU0 stack | 最小启动代码、FDT/页表准备 |
| MMU/per-CPU 初始化后 | `PerCpuMeta::stack_top_virt` | dynamic platform main、ax-runtime |
| scheduler 初始化后 | 同一 boot stack 被 main task 借用 | 正常内核任务调度 |

切换后 linker stack 仍属于 KImage，只是不再作为 main task 的运行栈。代码不能假定该旧范围会被回收到 Buddy。

## 3. Per-CPU boot stack

每个可启动 CPU 都在 BSP early boot 阶段获得自己的 boot stack。AP 启动不依赖通用 heap，避免并发 bring-up 时 allocator 和 per-CPU storage 尚未就绪的问题。

### 3.1 预分配布局

`platforms/someboot/src/smp/legacy.rs` 采用每 CPU 连续布局；`prealloc.rs` 在 `percpu-prealloc` feature 下将所有 metadata、所有 stack 和所有 data 分组。两者都对 stack slot 做页对齐。

| 布局 | 物理组织 | 适用特征 |
| --- | --- | --- |
| legacy | 每 CPU：linker data → meta → page pad → stack | 单个 CPU slot 容易查看 |
| `percpu-prealloc` | meta region → stack region → data region | 同类区域集中，stride 明确 |

`alloc_percpu()` 为配置的 `cpu_count()` 一次申请完整区域，清零后复制 linker per-CPU template，并为每个 CPU 写入 hard id、logical index、stack top 和 secondary entry。

### 3.2 Scheduler 借用

动态平台的 `boot_stack_bounds(cpu_idx)` 从 `somehal::smp::cpu_meta()` 返回 stack bottom 和 size。非 host-test 构建中，`axtask::main_task_stack()` 用 `TaskStack::borrowed()` 包装该范围。

| Owner 状态 | `TaskStackKind` | Drop 行为 |
| --- | --- | --- |
| boot/main/secondary stack | `Borrowed` | 不释放，仅由启动层持有物理范围 |
| plain task allocation | `Alloc` | 用原 `Layout` 归还 `GlobalAlloc` |
| guard-page task allocation | `GuardedAlloc` | 恢复 guard PTE 后归还全部页 |

`Borrowed` 表达“任务使用但不拥有”。这防止 scheduler 在 main task 结束或重建时把 early bump 的系统级 stack 错误释放给 Buddy。

## 4. 普通内核任务栈

`os/arceos/modules/axtask/src/task.rs::TaskStack` 封装 task stack 的地址、大小和所有权类型。任务创建时会把请求大小向 4 KiB 对齐。

### 4.1 Plain allocation

未启用 `stack-guard-page` 时，`TaskStack::alloc_plain()` 使用 `Layout::from_size_align(size, TASK_STACK_ALIGN)` 和 Rust allocator 分配。默认 256 KiB 请求走 Buddy 大对象路径。

| 操作 | 实现 | 失败语义 |
| --- | --- | --- |
| allocation | `alloc::alloc::alloc(layout)` | null 时当前代码 assert/panic |
| bottom canary | `STACK_END_MAGIC` 写入 stack bottom | 调度检查可发现覆盖 |
| release | `alloc::alloc::dealloc(ptr, layout)` | 必须使用原 size/align |

Canary 能检测已经写到栈底的溢出，但不能阻止继续破坏相邻内存。需要立即 fault 的配置应启用 guard page。

### 4.2 Guard-page allocation

启用 `stack-guard-page` 后，`TaskStack::alloc_guarded()` 申请 `usable pages + 1` 个连续 Normal 页，将最低一页从 kernel address space unmap，并把可用 bottom 设置在 guard page 之后。

```mermaid
flowchart LR
    Base["allocation base"] --> Guard["4 KiB unmapped guard"]
    Guard --> Canary["stack bottom + canary"]
    Canary --> Usable["usable stack pages"]
    Usable --> Top["stack top"]
```

Drop 时先通过 `ax-mm::kernel_aspace().map_linear()` 恢复 guard 页映射，再按原 `PageRequest` 释放整段 allocation。先恢复映射可避免 Buddy 重用该页后内核 direct map 仍残留 hole。

## 5. TLB 与 SMP guard

改变 kernel stack guard PTE 后必须让可能缓存该映射的 CPU 失效。单核与 SMP 使用不同路径，但都在继续使用或释放页面前完成。

### 5.1 Local flush

未同时启用 SMP 和 IPI 时，`flush_stack_guard_tlb(vaddr)` 调用 `ax_hal::asm::flush_tlb(Some(vaddr))`。该路径假设没有其他 CPU 持有相关 kernel mapping。

| 事件 | PTE 操作 | TLB 操作 |
| --- | --- | --- |
| stack 创建 | unmap guard VA | local address flush |
| stack Drop | remap guard VA | local address flush |

页表事务成功并不自动替代架构 shootdown。guard stack 代码显式完成这一职责，因为它修改的是所有 CPU 可见的 kernel address space。

### 5.2 Remote shootdown

同时启用 `stack-guard-page + smp + ipi` 时，代码在禁止抢占的 guard 内向所有 ready remote CPU 发送 flush closure，并以 Release/Acquire ack 计数等待完成，最后 flush 本 CPU。

| 约束 | 当前实现 |
| --- | --- |
| CPU 选择 | 跳过 current CPU 和尚未 ready 的 CPU |
| 顺序 | 修改映射后 fence，remote/local flush，再等待 ack |
| 超时 | 5 秒后 panic，报告 ack 数和地址 |
| 页面释放 | 仅在 remap 与 shootdown 完成后执行 |

超时 panic 是内核映射一致性失败，而不是可忽略的性能告警。若某架构提供硬件 broadcast，通用页表层可声明该 scope，但当前 stack guard 路径仍使用自己的 IPI 协议。

## 6. Starry 用户栈

Starry 用户栈属于用户虚拟地址空间，不是 `TaskStack`。loader 和进程内存策略建立 stack VMA，物理页由缺页或 populate 路径按需分配。

### 6.1 VMA 与 resident page

用户栈的虚拟范围计入 VSS，只有已映射的匿名页计入 RSS。`starry-mm::ProcessMemStats::record_vma()` 通过 `[stack]` 名称或进程 stack range 将 VMA 分类到 `stack_pages`。

| 指标 | 用户栈含义 | 物理占用关系 |
| --- | --- | --- |
| `VmStk` | 被识别为 stack 的 VMA 页数 | 可能包含未驻留页 |
| `VmSize` | 全部 VMA 的 VSS | 不等于 Buddy 已分配页 |
| `RssAnon` | 已驻留匿名页 | 包含实际 fault/populate 的 stack page |
| kernel task stack | 内核态执行栈 | 不计入用户进程 VMA 统计 |

用户栈释放通过 address space unmap/clear 和 backend page owner 完成，不调用 `TaskStack::drop()`。

### 6.2 保护边界

用户访问权限由 Stage-1 PTE 和 Starry VMA flags 共同决定。kernel stack guard feature 只保护 `axtask` 内核栈，不会自动给所有 Starry 用户 stack 增加 guard VMA。

| 边界 | 负责组件 | 故障处理 |
| --- | --- | --- |
| 用户 stack VMA 权限 | Starry `AddrSpace` / backend | 转换为 `FaultOutcome`，再由 kernel 处理 signal |
| kernel task guard page | `axtask` + `ax-mm` | 诊断 `diagnose_stack_guard_page_fault()` |
| CPU boot stack 范围 | `someboot` / `ax-hal` | 启动配置与 canary，当前无动态 guard |

分析 stack overflow 时必须先确认 fault address 属于哪种 stack。把用户 VMA fault 误判成 kernel guard，或把 boot stack 当作 allocator 泄漏，都会得出错误结论。

## 7. 配置与审计入口

栈行为跨 linker、启动平台、scheduler 和用户 VM，修改默认大小或 guard feature 时需要同时检查这些边界。

### 7.1 配置来源

默认值存在于不同构建阶段，最终以平台和 `axbuild` 生成配置为准。重复默认值必须保持语义一致或由构建脚本明确覆盖。

| 配置 | 当前默认 | 源码入口 |
| --- | --- | --- |
| someboot `STACK_SIZE` | `0x40000` | `platforms/someboot/src/ld/defaults.ld` |
| axtask task stack | `0x40000` | `os/arceos/modules/axtask/build.rs` |
| ax-runtime task stack | `0x40000` | `os/arceos/modules/axruntime/build.rs` |
| API exposed task stack | `0x40000` | `arceos_api` / `arceos_posix_api` config |
| user pthread compatibility default | 2 MiB | `axstd/src/os/libc_compat.rs` |

修改一个默认值后应验证生成的 build info、公开 API 和实际 task creation 参数，避免文档或 resource limit 仍报告旧值。

### 7.2 源码检查点

下面的文件覆盖 stack 从静态布局到释放的完整生命周期。测试应同时覆盖 owner 类型、canary 和 guard shootdown。

| 源码 | 审计重点 |
| --- | --- |
| `platforms/someboot/src/ld/bss.ld` | CPU0 linker stack 是否位于 KImage |
| `platforms/someboot/src/smp/legacy.rs` | 每 CPU slot 对齐和 stack top |
| `platforms/someboot/src/smp/prealloc.rs` | 分组 region 的 stride 与总大小 overflow |
| `platforms/axplat-dyn/src/boot.rs` | `boot_stack_bounds()` 元数据来源 |
| `os/arceos/modules/axtask/src/run_queue.rs` | main/secondary task 借用 boot stack |
| `os/arceos/modules/axtask/src/task.rs` | plain/guarded/borrowed Drop 与 TLB flush |
| `memory/starry-mm/src/stats.rs` | 用户 stack VMA 统计分类 |

验收时应记录每 CPU 固定 stack 总开销、最大 task 数乘以配置栈大小、guard page 的额外一页以及 Starry 用户 stack 的 VSS/RSS 差异。
