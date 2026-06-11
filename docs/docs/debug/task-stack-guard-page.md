---
sidebar_position: 7
sidebar_label: "Task Stack Guard Page"
---

# Task Stack Guard Page

本文档记录 ArceOS task stack guard page 的设计讨论、第一阶段实现方向，
以及后续向 Linux `VMAP_STACK` 风格演进时需要补齐的能力。

它是现有 task stack canary 的增强机制，而不是替代机制。

## 背景

当前 `ax-task` 已经有 task stack canary 检查。启用 `stack-canary` 后，
`TaskStack` 会在栈底写入固定 magic 值，调度器在任务切换时检查上一个
任务的 canary 是否仍然完整。这个机制可以发现栈底被覆盖或栈溢出导致的
内存破坏，但它本质上是事后检查：只有在后续检查点运行时，破坏才会被发现。

guard page 的目标是让栈向下越界时尽量立即触发硬件 page fault，减少
越界写入继续破坏相邻对象的机会。

## Linux 参考点

Linux 的相关机制是 `CONFIG_VMAP_STACK`。在 Linux 6.12 中，
`kernel/fork.c` 的 `alloc_thread_stack_node()` 会在启用
`CONFIG_VMAP_STACK` 时通过 `__vmalloc_node_range()` 分配内核任务栈。
`arch/Kconfig` 中对 `HAVE_ARCH_VMAP_STACK` 的要求包括：

- vmalloc 空间必须足够容纳大量内核栈。
- vmap 栈在运行时必须可靠，例如栈页表不能在切换到该栈后才临时缺页。
- 如果栈溢出打到 guard page，架构代码应能给出合理诊断，而不是无日志重启。

因此，Linux 的 guard page 不是简单多申请一个物理页，而是把任务栈放入
vmalloc/vmap 虚拟区间，在栈边界保留未映射页面。guard page 只消耗虚拟
地址空间和页表元数据，不消耗实际物理页。

## 当前 ArceOS 能力

`axmm` 已经有两类映射 backend：

- `Linear`：线性映射，虚拟地址和物理地址之间是固定 offset。当前 kernel
  address space 初始化主要用这种方式映射物理内存区域。
- `Alloc`：分配式映射，在指定虚拟地址范围内挂由全局页分配器分配出来的
  物理页。`populate = false` 时可以在 page fault 路径中按需分配物理页。

`Alloc` backend 已经具备一部分 vmap 所需的底层能力：它可以把一段虚拟
地址映射到逐页分配的物理页上，物理页不必在物理地址上连续。

但它还不是完整的 Linux `vmap` / `vmalloc`：

- 调用方必须自己提供虚拟地址起点。
- 当前没有专门的 kernel vmap 虚拟区间分配器来管理空洞、对齐和回收。
- 当前没有面向 guard page 的 area metadata，用来快速判断 fault address
  是否落在某个任务栈的 guard page 中。
- lazy `Alloc` 的缺页补页语义不适合直接作为 guard page，因为 guard page
  应该保持永远不映射。

换句话说，`axmm` 已经有 vmap 的底层映射能力，但还缺 vmap-style 虚拟
地址区间管理。

## 第一阶段方案

第一阶段采用最小实现：只增强动态分配的普通任务栈。

当前 `TaskStack::alloc()` 使用普通 byte allocator 分配栈空间，只要求
`TASK_STACK_ALIGN`，并不保证栈底是独占页。第一阶段应改为页粒度分配：

1. 对外仍接受逻辑栈大小 `stack_size`。
2. 实际申请 `stack_size + PAGE_SIZE_4K`。
3. 最低一页作为 guard page。
4. 可用栈范围从 `base + PAGE_SIZE_4K` 到 `base + PAGE_SIZE_4K + stack_size`。
5. 任务入口仍使用可用栈范围的 top 作为初始 SP。
6. 释放栈时必须恢复或正确处理 guard page 生命周期，避免把仍不可访问的
   direct-map 页面还给普通 allocator。

这个方案通过独立 feature `stack-guard-page` 控制。它依赖 `multitask`、
paging 和内存管理能力，而不是无条件进入所有 multitask 构建。

`stack-guard-page` 当前是 opt-in hardening feature，默认不启用。常规
ArceOS / StarryOS 构建和普通回归测试默认仍只覆盖未启用 guard page 的行为；
只有显式打开 `stack-guard-page` 或上层转发 feature 时，动态任务栈才会获得
guard page。

常见启用方式包括：

- ArceOS Rust 应用通过 `ax-std/stack-guard-page` 启用。
- ArceOS 底层或非 `ax-std` 场景通过 `ax-feat/stack-guard-page` 启用。
- StarryOS 通过 `starry-kernel/stack-guard-page` 启用；这个 feature 会同时
  打开 Starry fault handler 中的 guard page 诊断路径，并向下启用
  `ax-feat/stack-guard-page`。

项目的 xtask/axbuild 流程也支持通过环境变量注入额外 feature，例如：

```bash
FEATURES=ax-std/stack-guard-page cargo xtask arceos test qemu ...
FEATURES=starry-kernel/stack-guard-page cargo xtask starry test qemu ...
```

这里的 `FEATURES` 是项目构建工具读取的环境变量。带 `/` 的写法是 Cargo 的
package feature 语法，表示启用指定依赖包上的 feature，例如
`starry-kernel/stack-guard-page` 表示启用 `starry-kernel` crate 自己的
`stack-guard-page` feature。

### 物理页开销

这个简单方案会额外占用一个物理页。

原因是 guard page 仍来自实际分配的连续页区间，只是在当前 kernel page
table 中被设为不可访问或撤销映射。该物理页必须继续归这个 `TaskStack`
持有，不能直接还给全局 allocator，否则后续被其他对象复用时，direct-map
虚拟地址仍可能不可访问，造成更隐蔽的问题。

开销为：

```text
每个启用 guard page 的动态任务栈额外占用 1 个 4 KiB 物理页
```

例如：

- `TASK_STACK_SIZE = 64 KiB` 时，额外开销约 6.25%。
- `TASK_STACK_SIZE = 16 KiB` 时，额外开销约 25%。

因此第一阶段更适合作为 debug / hardening feature，而不是直接默认启用。

### Canary 仍需保留

guard page 和 canary 覆盖的问题不同：

- guard page 用于捕获向栈底越界并触达保护页的访问。
- canary 用于在调度切换等检查点发现栈底被覆盖或破坏。

小范围栈底破坏、未覆盖到 guard page 的破坏，以及暂不支持 guard page 的
borrowed stack，仍需要 canary 兜底。因此第一阶段不应移除 `stack-canary`。

## 当前覆盖边界

当前 guard page 机制覆盖的是 `TaskStack::alloc()` 创建并由 `ax-task`
拥有生命周期的动态任务栈。典型路径是：

```text
TaskInner::new()
  -> TaskStack::alloc()
  -> TaskStack::alloc_guarded()
  -> unmap_guard_page()
```

因此，普通 `spawn` / thread 创建的任务栈、运行时创建的 gc task，以及主
CPU 上通过 `TaskInner::new()` 创建的独立 idle task，都会在启用
`stack-guard-page` 后获得 guard page。

这个覆盖边界与 `plat-dyn` / 非 `plat-dyn` 无直接绑定。两种平台模式下，
只要栈来自 `TaskStack::alloc()`，就会走 guarded allocation；只要栈来自
`TaskStack::borrowed()`，当前就不会做 guard page。

当前未覆盖的栈主要分为两类。

### 1. Borrowed boot/current stack

这类栈由平台、linker script、somehal metadata 或 runtime bring-up 流程
提供，`ax-task` 只通过 `TaskStack::borrowed()` 记录它的范围，不拥有它的
分配和释放生命周期。

包括：

- 主 CPU 的 boot/main 栈。
- 平台 `.bss.stack` / linker symbol 描述的 boot stack。
- `plat-dyn` 下由 somehal metadata 暴露的 per-CPU boot stack。
- secondary CPU bring-up 后作为当前 idle 任务使用的 borrowed stack。

这些栈的边界来自 linker script 或平台启动代码，直接修改页表权限可能影响
平台早期启动、secondary CPU bring-up，或与 `plat-dyn` 的真实栈边界产生冲突。

对非 `plat-dyn`，secondary boot stack 虽然可能由 `axruntime::mp` 使用
`GlobalPage` 分配，但传入调度器时仍被包装成 `TaskStack::borrowed()`。
因此当前也不属于 guard page 覆盖范围。

### 2. 专用异常/中断/特殊栈

当前多数 trap / IRQ 路径复用当前任务内核栈；如果当前任务栈是动态
guarded stack，就会间接受 guard page 保护。

但如果后续引入独立的 per-CPU IRQ stack、exception stack、overflow
stack、NMI/double-fault stack 或其他架构专用栈，这些栈不会自动继承
`TaskStack::alloc()` 的 guard page 机制，需要单独建模。

综上，当前覆盖矩阵为：

| 栈类型 | `plat-dyn` | 非 `plat-dyn` | 当前 guard page 覆盖 |
| --- | --- | --- | --- |
| 动态任务栈，`TaskStack::alloc()` | 是 | 是 | 是 |
| 主 CPU boot/main borrowed 栈 | 是 | 是 | 否 |
| secondary CPU borrowed boot/idle 栈 | 是 | 是 | 否 |
| 平台静态 boot stack | 不适用或由平台 metadata 描述 | 是 | 否 |
| 独立 IRQ / exception / overflow stack | 若引入需单独处理 | 若引入需单独处理 | 否 |

## 页表操作选择

第一阶段建议优先使用显式 `unmap` 保护 guard page，而不是直接依赖
`protect(..., MappingFlags::empty())`。

原因是不同架构的 PTE 对空权限的表达并不完全一致。当前 RISC-V PTE 的
`set_flags()` 路径对 page PTE 有 `R | X` 相关断言，直接用空 flags
做 `PROT_NONE` 风格保护存在风险。显式 `unmap` 更接近“guard page 不存在
有效映射”的语义，也更容易跨架构保持一致。

实现时必须注意释放顺序：

- 如果 guard page 来自 direct-map 的实际物理页，释放前需要确保对应映射状态
  不会污染后续 allocator 使用。
- 如果后续切换到真正 vmap-style 方案，guard page 不分配物理页，也就没有这类
  物理页回收问题。

页表变更后还需要刷新对应 TLB 项。当前实现中，单核或未启用 IPI 的构建会在
本 CPU 对 guard page 地址执行局部 flush；通过顶层 `stack-guard-page`
feature 正常启用时会同时启用 IPI 支持，在 SMP 场景下对已经完成 IPI 初始化
并打开本地 IRQ 的 CPU 发送同步 TLB shootdown。

启动早期需要特别处理：主核在 `ax_task::init_scheduler()` 阶段可能已经创建
动态任务栈，但此时其他 CPU 还没有初始化 IPI 队列，也不一定已经打开本地 IRQ。
因此 shootdown 不应盲目发送给所有配置 CPU，而是只发送给 runtime 标记为
`IPI ready` 的 CPU。CPU 发布 ready 时会先进入 `becoming ready` 状态，做一次
全局本地 TLB flush，再切换为 `ready`；shootdown 如果看到某个 CPU 正在发布
ready，会等待该 CPU 完成发布后再发送一次保守的远端 flush。这样可以覆盖
guard page 页表更新与 secondary CPU 上线之间的竞态窗口。

## Fault 诊断

只让 guard page 触发 page fault 还不够。没有诊断增强时，用户看到的可能只是
普通 unhandled page fault，无法快速判断这是任务栈溢出。

第一阶段已经在 page fault handler 中补充当前任务 guard page 诊断：

- `ax-task` 提供当前任务 guard page 命中判断。
- ArceOS runtime 的 page fault handler 会先识别 task stack guard page，
  未命中时继续交给 `axmm` 处理 lazy allocation fault。
- StarryOS 的 user memory fault handler 也会先识别 task stack guard page，
  未命中时继续执行原有用户地址空间缺页处理。
- 命中时打印任务名、任务 ID、fault address、stack range、guard range。
- 诊断函数不吞掉 fault，仍返回未处理，让原有 trap panic/oops 路径继续输出
  架构寄存器和 backtrace。

该实现目前只检查当前任务的动态 guard stack。跨任务 fault address 反查、
全局 stack metadata 和异常专用栈仍属于后续增强。

如果 fault 发生时当前栈已经接近耗尽，诊断路径本身也有继续溢出的风险。Linux
在部分架构上通过 double fault stack 或 overflow stack 处理这一点。ArceOS
第一阶段可以先打印最小诊断，后续再评估异常专用栈。

## 回归测试

当前统一 ArceOS Rust QEMU 测试集只保留能在一次启动中顺序返回的 feature
用例，不再保留会以 fatal page fault 结束的 `task/stack_guard_page` 专项
case。Guard page 诊断的专项验证需要作为 fail-type 用例单独恢复或新增；
普通回归主要确认默认构建和启用 guard page 后的常规任务路径不受影响。

### 启用态手动回归

由于 `stack-guard-page` 默认不启用，普通测试套件主要验证默认构建不受影响。
如果需要验证启用 guard page 后的普通回归，需要显式注入 feature。

ArceOS 可以在常规 Rust 用例上加 `ax-std/stack-guard-page`：

```bash
FEATURES=ax-std/stack-guard-page cargo xtask arceos test qemu --arch riscv64 \
  --test-group rust --test-case task-yield
FEATURES=ax-std/stack-guard-page cargo xtask arceos test qemu --arch riscv64 \
  --test-group rust --test-case task-parallel
FEATURES=ax-std/stack-guard-page cargo xtask arceos test qemu --arch riscv64 \
  --test-group rust --test-case task-affinity
FEATURES=ax-std/stack-guard-page cargo xtask arceos test qemu --arch riscv64 \
  --test-group rust --test-case task-ipi
```

StarryOS 可以加 `starry-kernel/stack-guard-page`。不要只写裸
`stack-guard-page`：裸 feature 会按当前包的 ax feature 前缀推导，可能只打开
底层 `ax-feat/stack-guard-page`，而不会打开 `starry-kernel` 中
`#[cfg(feature = "stack-guard-page")]` 保护的 fault 诊断分支。

推荐手动抽样：

```bash
FEATURES=starry-kernel/stack-guard-page cargo xtask starry build --arch riscv64
FEATURES=starry-kernel/stack-guard-page cargo xtask starry test qemu --arch riscv64 \
  --test-case smoke
FEATURES=starry-kernel/stack-guard-page cargo xtask starry test qemu --arch riscv64 \
  --test-case affinity
FEATURES=starry-kernel/stack-guard-page cargo xtask starry test qemu --arch riscv64 \
  --test-case test-fault-pending-signal
FEATURES=starry-kernel/stack-guard-page cargo xtask starry test qemu --arch riscv64 \
  --test-case test-fault-thread-routing
```

全量 StarryOS 启用态回归耗时较长，并会增加 rootfs/cache 磁盘压力，更适合
nightly、发布前或高风险内存管理改动后的手动验证：

```bash
FEATURES=starry-kernel/stack-guard-page cargo xtask starry test qemu --arch riscv64
```

## 后续演进计划

第一阶段完成后，可以继续向更接近 Linux `VMAP_STACK` 的方向演进。

### 1. 稳定当前动态任务栈方案

当前优先级最高的是把已接入的动态任务栈方案做稳：

- 保持 `unmap_guard_page()` / `remap_guard_page()` 后的本地与远端 TLB
  flush 语义。
- 持续覆盖 SMP 下任务迁移到远端 CPU 后触发 guard page 的场景。
- 补齐更多架构 QEMU 回归，尤其是 loongarch64，以及 aarch64 SMP IPI
  支持可用后的多核回归。
- 保持 fault 诊断路径简短，避免栈已经接近耗尽时诊断代码继续扩大破坏。

### 2. Kernel vmap allocator

在 `axmm` 之上增加内核虚拟区间分配器，负责：

- 管理一段专用 kernel vmap 虚拟地址范围。
- 查找、保留和回收虚拟地址空洞。
- 支持按页对齐和更高阶对齐需求。
- 为 guard page、stack pages、metadata 建立统一生命周期。

此时任务栈可以布局为：

```text
[guard page][mapped stack pages]
```

guard page 只占虚拟地址，不占物理页。

vmap-style 栈的目标不是替换第一阶段的检查语义，而是降低物理页浪费并
让栈布局更接近 Linux `CONFIG_VMAP_STACK`：

- guard page 不再额外占用真实物理页。
- 栈页可以由非连续物理页组成。
- 每个栈拥有独立的虚拟地址区间，便于做边界诊断和生命周期管理。
- 释放时只需要释放已映射 stack pages 和虚拟区间，guard hole 本身没有
  物理页回收问题。

这一步需要先解决 kernel vmap 虚拟地址空间管理，再把 `TaskStack`
从 direct-map contiguous allocation 迁移到 vmap allocation。

### 3. Stack area metadata

需要能从 fault address 反查：

- 这是哪个任务的 task stack。
- fault 是否命中 guard page。
- 可用栈范围和 guard 范围。

这可以先只服务当前任务，后续再支持更完整的跨任务诊断、backtrace 和调试输出。

### 4. Borrowed boot stack 覆盖

在动态任务栈和 vmap-style 分配模型稳定后，再逐步评估 borrowed boot/current
stack 的覆盖方式：

- 对 linker script 提供的 boot stack，可以考虑在链接脚本中显式预留
  page-aligned guard hole。
- 对 `plat-dyn` / somehal 提供的 boot stack，需要由平台 metadata 明确给出
  可保护边界，不能由 `ax-task` 猜测相邻页面是否可 unmap。
- 对非 `plat-dyn` secondary boot stack，可以考虑在 `axruntime::mp`
  分配时直接使用 guarded/vmap-style boot stack，并把 guard metadata
  传给 `axtask`。
- 对当前作为 `main` 或 secondary `idle` 使用的 borrowed stack，需要确认
  切换到受保护栈的时机，避免在仍使用该栈时修改其底部映射造成启动路径故障。

这些栈的接入需要结合各架构入口代码和平台启动流程，不应和第一阶段混在一起。

### 5. 专用异常/中断栈覆盖

如果后续引入 per-CPU IRQ stack、exception stack、overflow stack 或
x86_64 NMI/double-fault 类栈，应为它们建立独立的 guard page 方案。

这部分需要结合架构入口代码设计：

- guard page fault 发生时不能继续依赖已经溢出的普通任务栈。
- overflow / double-fault 路径应尽量使用专用栈打印最小诊断。
- 专用栈通常是 per-CPU 生命周期，TLB flush 和 metadata 可以按 per-CPU
  资源管理，而不是按 task 管理。

### 6. 扩展测试覆盖

需要增加能稳定触发 guard page 的测试：

- 构造小栈任务。
- 递归或大栈帧触发向下越界。
- QEMU 日志匹配 stack guard page 诊断。
- 至少覆盖常用架构，优先 x86_64、riscv64、aarch64、loongarch64。

测试用例应和 canary 测试区分：guard page 测试关注 page fault 即时触发，
canary 测试关注调度切换或显式检查点发现破坏。

## 当前结论

当前阶段采用简单方案是合理的：

- 它实现成本低。
- 能快速验证 task stack guard page 的诊断价值。
- 代价清晰：每个动态任务栈额外占用一个 4 KiB 物理页。
- 它不会阻塞后续向真正 vmap-style 栈演进。

后续如果任务数量较多、物理内存开销明显，或希望更接近 Linux 的长期设计，
再补 kernel vmap allocator，把 guard page 从“额外物理页”改成“仅虚拟空洞”。
