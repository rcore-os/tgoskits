# ax-cpu 模块、功能与所有权

CPU 公共表面按功能组织，当前目标由 `src/arch/mod.rs` 的 `core::cfg_select!` 唯一选择。`arch` 不公开；使用者从 `context`、`paging`、`trap`、`pmu`、`virtualization` 等入口直接取得当前目标类型和固有方法。

## 1. 源码层级

目录模块采用 `mod.rs`，叶子使用普通 Rust 或汇编文件。没有实现的架构能力不建立空后端；PMU 只在 AArch64 提供。私有 `asm.rs` 保存部分尚未按指令领域拆开的底层原语，不构成下游接口。

### 1.1 完整目录

本清单列出 CPU 的实现来源。汇编位于各目标的 `entry`，启动和客户机入口复用实际保存恢复片段，不能从上层 crate 再引入一份同语义汇编。

```text
components/axcpu/
├── .github/
│   └── workflows/
│       └── ci.yml
├── src/
│   ├── arch/
│   │   ├── aarch64/
│   │   │   ├── entry/
│   │   │   │   ├── gpr.S
│   │   │   │   ├── guest.S
│   │   │   │   ├── guest.rs
│   │   │   │   ├── mod.rs
│   │   │   │   └── trap.S
│   │   │   ├── paging/
│   │   │   │   ├── mod.rs
│   │   │   │   ├── stage1.rs
│   │   │   │   └── stage2.rs
│   │   │   ├── pmu/
│   │   │   │   ├── access.rs
│   │   │   │   ├── capability.rs
│   │   │   │   ├── counter.rs
│   │   │   │   ├── mod.rs
│   │   │   │   ├── overflow.rs
│   │   │   │   └── registers.rs
│   │   │   ├── virtualization/
│   │   │   │   ├── context.rs
│   │   │   │   ├── exit.rs
│   │   │   │   ├── mod.rs
│   │   │   │   ├── paging.rs
│   │   │   │   ├── percpu.rs
│   │   │   │   ├── timer.rs
│   │   │   │   ├── tlb.rs
│   │   │   │   └── vcpu.rs
│   │   │   ├── asid.rs
│   │   │   ├── asm.rs
│   │   │   ├── barrier.rs
│   │   │   ├── boot.rs
│   │   │   ├── cache.rs
│   │   │   ├── capability.rs
│   │   │   ├── context.rs
│   │   │   ├── fp.rs
│   │   │   ├── interrupt.rs
│   │   │   ├── mmu.rs
│   │   │   ├── mod.rs
│   │   │   ├── registers.rs
│   │   │   ├── timer.rs
│   │   │   ├── trap.rs
│   │   │   ├── user_atomic.S
│   │   │   ├── user_copy.S
│   │   │   └── uspace.rs
│   │   ├── loongarch64/
│   │   │   ├── entry/
│   │   │   │   ├── boot.S
│   │   │   │   ├── boot.rs
│   │   │   │   ├── guest.S
│   │   │   │   ├── guest.rs
│   │   │   │   ├── mod.rs
│   │   │   │   ├── tlb_refill.S
│   │   │   │   └── walker.S
│   │   │   ├── virtualization/
│   │   │   │   ├── context.rs
│   │   │   │   ├── interrupt.rs
│   │   │   │   ├── mod.rs
│   │   │   │   ├── percpu.rs
│   │   │   │   ├── tlb.rs
│   │   │   │   └── vcpu.rs
│   │   │   ├── asm.rs
│   │   │   ├── barrier.rs
│   │   │   ├── boot.rs
│   │   │   ├── cache.rs
│   │   │   ├── capability.rs
│   │   │   ├── context.rs
│   │   │   ├── fp.rs
│   │   │   ├── interrupt.rs
│   │   │   ├── irq.rs
│   │   │   ├── macros.rs
│   │   │   ├── mod.rs
│   │   │   ├── paging.rs
│   │   │   ├── registers.rs
│   │   │   ├── timer.rs
│   │   │   ├── trap.S
│   │   │   ├── trap.rs
│   │   │   ├── unaligned.S
│   │   │   ├── unaligned.rs
│   │   │   ├── user_atomic.S
│   │   │   ├── user_copy.S
│   │   │   └── uspace.rs
│   │   ├── riscv64/
│   │   │   ├── entry/
│   │   │   │   ├── boot.S
│   │   │   │   ├── boot.rs
│   │   │   │   ├── guest.S
│   │   │   │   ├── guest.rs
│   │   │   │   ├── guest_memory.S
│   │   │   │   └── mod.rs
│   │   │   ├── paging/
│   │   │   │   ├── mod.rs
│   │   │   │   └── stage1.rs
│   │   │   ├── virtualization/
│   │   │   │   ├── binding.rs
│   │   │   │   ├── context.rs
│   │   │   │   ├── exit.rs
│   │   │   │   ├── memory.rs
│   │   │   │   ├── mod.rs
│   │   │   │   ├── paging.rs
│   │   │   │   ├── percpu.rs
│   │   │   │   └── registers.rs
│   │   │   ├── asm.rs
│   │   │   ├── barrier.rs
│   │   │   ├── boot.rs
│   │   │   ├── cache.rs
│   │   │   ├── capability.rs
│   │   │   ├── context.rs
│   │   │   ├── interrupt.rs
│   │   │   ├── local_state.rs
│   │   │   ├── macros.rs
│   │   │   ├── mmu.rs
│   │   │   ├── mod.rs
│   │   │   ├── registers.rs
│   │   │   ├── timer.rs
│   │   │   ├── trap.S
│   │   │   ├── trap.rs
│   │   │   ├── trap_tls.S
│   │   │   ├── user_atomic.S
│   │   │   ├── user_copy.S
│   │   │   └── uspace.rs
│   │   ├── x86_64/
│   │   │   ├── entry/
│   │   │   │   ├── boot.S
│   │   │   │   ├── boot.rs
│   │   │   │   ├── gpr.S
│   │   │   │   ├── mod.rs
│   │   │   │   ├── svm.S
│   │   │   │   ├── trap.S
│   │   │   │   ├── vmx.S
│   │   │   │   └── xstate.S
│   │   │   ├── paging/
│   │   │   │   ├── ept.rs
│   │   │   │   ├── mod.rs
│   │   │   │   └── native.rs
│   │   │   ├── virtualization/
│   │   │   │   ├── svm/
│   │   │   │   │   ├── context.rs
│   │   │   │   │   ├── controls.rs
│   │   │   │   │   ├── exit.rs
│   │   │   │   │   ├── instructions.rs
│   │   │   │   │   ├── memory.rs
│   │   │   │   │   ├── mod.rs
│   │   │   │   │   └── vmcb.rs
│   │   │   │   ├── vmx/
│   │   │   │   │   ├── context.rs
│   │   │   │   │   ├── control_flags.rs
│   │   │   │   │   ├── controls.rs
│   │   │   │   │   ├── exit.rs
│   │   │   │   │   ├── fields.rs
│   │   │   │   │   ├── host.rs
│   │   │   │   │   ├── instructions.rs
│   │   │   │   │   ├── mod.rs
│   │   │   │   │   ├── paging.rs
│   │   │   │   │   └── vmcs.rs
│   │   │   │   ├── bitmaps.rs
│   │   │   │   ├── memory.rs
│   │   │   │   ├── mod.rs
│   │   │   │   ├── percpu.rs
│   │   │   │   ├── vcpu.rs
│   │   │   │   └── xstate.rs
│   │   │   ├── asm.rs
│   │   │   ├── barrier.rs
│   │   │   ├── boot.rs
│   │   │   ├── cache.rs
│   │   │   ├── capability.rs
│   │   │   ├── context.rs
│   │   │   ├── entry_state.rs
│   │   │   ├── gdt.rs
│   │   │   ├── idt.rs
│   │   │   ├── interrupt.rs
│   │   │   ├── local_state.rs
│   │   │   ├── mod.rs
│   │   │   ├── msr.rs
│   │   │   ├── registers.rs
│   │   │   ├── timer.rs
│   │   │   ├── trap.rs
│   │   │   ├── user_atomic.S
│   │   │   ├── user_copy.S
│   │   │   └── uspace.rs
│   │   └── mod.rs
│   ├── pmu/
│   │   └── mod.rs
│   ├── trap/
│   │   ├── boot.rs
│   │   ├── diagnostics.rs
│   │   ├── fault.rs
│   │   ├── interrupted.rs
│   │   └── mod.rs
│   ├── virtualization/
│   │   ├── error.rs
│   │   ├── memory.rs
│   │   └── mod.rs
│   ├── barrier.rs
│   ├── boot.rs
│   ├── cache.rs
│   ├── capability.rs
│   ├── context.rs
│   ├── exception_table.rs
│   ├── interrupt.rs
│   ├── lib.rs
│   ├── mmu.rs
│   ├── paging.rs
│   ├── registers.rs
│   ├── task_local.rs
│   ├── timer.rs
│   ├── user.rs
│   ├── user_access.rs
│   └── uspace_common.rs
├── .gitignore
├── CHANGELOG.md
├── Cargo.toml
├── LICENSE
├── LICENSE-MPL-2.0
├── README.md
├── README_CN.md
└── build.rs
```

### 1.2 公共入口

相同契约的范围校验和范围处理由共享模块提供，寄存器编码、特权级和客户机控制区继续保留硬件差异。`El1`、`El2`、`EptPointer`、`Vmcs` 等类型只在支持的目标上出现。

| 入口 | CPU 所有内容 | 上层所有内容 |
| --- | --- | --- |
| `registers`、`context` | 寄存器格式、扩展状态、最终切换 | 任务对象、调度状态、CPU 区域 |
| `trap` | 向量、故障解码、按值 IRQ 现场 | 处置策略、动态作用域、信号和回溯 |
| `paging`、`mmu` | PTE、硬件根/tag、失效指令 | 分配、地址空间身份、跨核回收协议 |
| `cache`、`barrier` | 范围校验、cache 指令、完成顺序 | DMA 所有权、平台一致性、别名生命周期 |
| `boot`、`timer` | CPU 初始化、向量安装、计数器与比较器 | 固件、启动编排、时钟源选择、事件队列 |
| `pmu` | 本核探测、计数、溢出和权限 | Linux perf、槽、group、采样 registry |
| `virtualization` | 控制区、PerCpu、Vcpu、进入退出 | VM、设备、虚拟固件与执行调度 |

## 2. 功能与依赖

基础配置无堆分配或运行期初始化要求。CPU 不依赖 `cpu-local`、`ax-percpu`、`axbacktrace`、someboot 或 VM；需要外部服务时，由 `trait-ffi` 声明 CPU 契约，上层提供实现。

### 2.1 Feature

架构由目标条件编译选择，Cargo feature 只启用实际能力。`uspace` 与 `tls` 同时启用时遵循最新 dev 的规则，由 `uspace` 决定寄存器所有权。

| Feature | 依赖与作用 |
| --- | --- |
| `default = []` | 基础机器原语与描述符 |
| `context` | 机器上下文与切换 |
| `fp-simd` | `context` 与扩展寄存器保存 |
| `exception-table` | 独立异常修复 |
| `uspace` | `context`、`exception-table` 与用户访问 |
| `tls` | `context`；只在未启用 `uspace` 时选择内核 TLS |
| `virtualization` | `context`、`fp-simd`、`exception-table` 与实际硬件后端 |
| `pmu` | 独立本核 PMU，不启用用户态或虚拟化 |
| `riscv-thead-mae` | 已实现的 T-Head 内存属性 |
| `riscv-sstc` | 已实现的客户机直接定时器能力 |

`host-test`、CPU 测试替身、`arm-el2`、`xuantie-c9xx` 和旧 vCPU tracing 功能不属于该表面。四个旧 vCPU 后端软件包已删除；AxVM 内的 `policy` 保存剩余 VM 层解释和设备语义。

### 2.2 接口依赖类型

公开接口使用的地址、页表 trait、寄存器字段及必要 trait 从所属 CPU 功能模块重导出。下游可以直接通过 `ax_cpu` 命名这些类型，无需额外依赖传递实现库。基础 `core` 类型仍使用标准路径。

`ControlMemory` 是宿主提供的外部所有权契约；`VmxControl` 等 CPU 内建选择为封闭类型。x86 专有虚拟化错误变体按目标编译，其他架构不会被迫处理 VMX 或端口位图错误。AArch64 的 `Stage1Regime` 为 sealed trait，不能由下游定义不受支持的描述符编码。


## 3. 设计取舍与生命周期

保留多个 vCPU crate 会继续复制描述符、trap 布局和机器恢复窗口；只增加转发包又会保留两套 API 和 feature。此次直接迁移实际硬件实现，并把虚拟固件与设备策略留在 AxVM。统一的大型跨架构 trait 无法表达 EL2、VMCS、G-stage 等差异，因此公共模块直接重导出当前目标具体类型；真正共用的范围检查和发布顺序才进入共享机制。

### 3.1 所有权与失败

`ControlMemory` 要求稳定、独占、已初始化并具有一致缓存属性的物理连续内存。宿主保有分配策略，CPU 控制对象保有租约；启用、绑定、进入、退出、解绑和关闭在同一个 CPU 所有权区间内执行。控制区初始化失败返回租约，已被硬件引用且无法安全撤销的控制区保留租约，不能因 Rust 析构释放硬件仍使用的页面。活动绑定不具备自由跨线程迁移能力，`unsafe` 契约进一步要求宿主保持 CPU pin。

PMU 会话只拥有本核寄存器访问，Drop 不重置或转让槽位。事件、IRQ 注册与撤销、采样重装、用户上下文授权由上层串行管理。IRQ 现场按值复制，HAL 用动态作用域守卫保存和恢复嵌套现场。异常修复表在链接后只读，避免每核初始化写同一张表。

`BootVectorTable` 的生命周期覆盖实际安装区间，并绑定构造时的代码映射和 CS。启动和运行期可以各自拥有表，使用同一个汇编来源；不能把低地址启动门拿到已移除低映射的运行期继续使用。平台控制器、固件和早期输出策略通过上层实现的 trait-ffi 契约接入。

### 3.2 页表回收与 CPU 探测

CPU 接口返回本核硬件 tag 容量。RISC-V 探测期间暂时修改 SATP ASID，在本地 IRQ 关闭区间内恢复原寄存器并完成失效；是否具备足够 ASID 供所有可能 CPU 分配，由 HAL 决定。跨核最小容量、冻结后拒绝能力不足 CPU、tag generation 和映射回收均不进入 CPU。

x86、RISC-V、LoongArch 的 `TranslationGate` 由 AxVM 持有。更新先关闭进入通道，向执行中的客户机请求退出，等待所有进入租约归还，然后才能取得 machine 锁和改表。超时或另一更新在途时返回 ResourceBusy，不提交映射修改；守卫释放时重新开放进入。下次实际 CPU 进入必做本核客体域失效，使其他 CPU 上留下的缓存无法在映射回收后重新使用。AArch64 使用客体域 inner-shareable 广播和 break-before-make 顺序。

### 3.3 兼容、回滚与性能

所有工作区调用方在同一分支迁移，不保留旧接口或旧后端包。恢复旧版本时按整个实现提交逆序回滚，不能单独恢复旧 Cargo 依赖或旧入口程序集；本次没有磁盘格式或运行期配置迁移，也没有旧后端动态 fallback。

CPU 指令和页表编码只有当前目标的一份来源。新增的 x86 指令流同步通过 CPUID 完成；本核数据缓存一致性不等于已序列化的指令预取。客户机每次进入的失效会增加成本，优先满足映射生命周期正确性；若以后采用代际优化，代数和 CPU 集合仍归 VM 所有者，不能由 CPU 后端偷偷跳过上层要求的失效。相同基线、机器与配置下的映像和调度测量见 [验证记录](ax-cpu-validation.md)，未取得的硬件结果不以其他目标替代。

高风险边界在合入前需要熟悉相应架构、Rust unsafe 和运行期并发的领域审查人核对。ArceOS 和板卡测试提供执行证据，不能代替控制区生命周期、最终机器窗口及错误恢复的逐项审查。
