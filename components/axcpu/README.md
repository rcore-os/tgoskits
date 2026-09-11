# ax-cpu

`ax-cpu` 提供当前目标的 CPU 指令、寄存器现场、页表描述符和异常入口。软件包继承自 [arceos-org/axcpu](https://github.com/arceos-org/axcpu)，保留原有 Apache-2.0 许可证与各迁入文件的来源声明。由 someboot 迁入的 AArch64 MMU、各架构启动操作及 RISC-V T-Head cache 指令，保留 MPL-2.0 来源标记与 `LICENSE-MPL-2.0`；软件包元数据列出两种许可。

## 1. 功能入口

架构实现位于私有的 `src/arch`，通过 `core::cfg_select!` 选择当前目标。下游使用功能模块，具体类型仍保留当前架构的固有方法。公开接口所需的依赖类型由本包重导出：共享地址及对齐、受检算术契约使用 `ax_cpu::{MemoryAddr, PhysAddr, VirtAddr}`，页表契约使用 `paging::{PageTableEntry, TableMeta}`，架构寄存器及描述符类型从对应功能模块取得，无需另行寻找传递依赖。

重导出也覆盖这些类型的字段、固有方法、关联类型与必要 trait：x86 的 `boot::GlobalDescriptorTable::entries` 返回 `boot::GdtEntry`，TSS I/O bitmap 校验使用 `boot::InvalidIoMap`；AArch64 的用户异常现场可通过 `user::{LocalRegisterCopy, Field, FieldValue}` 命名寄存器副本和字段。仅被实现内部使用的依赖保持私有，`core` 提供的标准类型继续使用标准路径。

### 1.1 机器状态

启用 `context` feature 后，`context::TaskContext` 保存调度切换需要的机器状态；`fp-simd`、`tls` 和 `uspace` 自动启用 `context`。`registers` 提供当前目标寄存器类型。遵循最新 dev 的模式规则，`uspace` 与 `tls` 同时启用时采用用户态寄存器所有权，`build.rs` 仅在 `tls && !uspace` 时输出内部 `kernel_tls` cfg。`TaskAnchor` 保存运行期对象的不透明地址，不拥有该对象，也不公开其布局。运行期必须保持对象固定并存活，完成调度与地址空间准备后，在中断关闭窗口内提交当前任务，再立即调用 `TaskContext::switch_to`。

`trap::InterruptedContext` 按值记录中断现场的 PC、SP、FP 和特权级。嵌套 IRQ 的动态作用域由运行期持有；CPU 层不保存指向临时 trap frame 的全局裸指针。

`registers` 同时拥有 CPU-local 使用的机器原语：AArch64 的 `TPIDR_EL1/EL2` 和 `SP_EL0`，RISC-V 的 `tp` 与 `sscratch`，LoongArch 的 `tp`、`r21` 与 scratch shadow，以及 x86 的 GS 基址和 GS 相对访问。调用方提供偏移与所有权保证；CPU 层不解释 `ExecutionContextHeader`、CPU 编号或抢占深度。x86 的增减和比较交换只保证本核中断边界内的一条指令，不提供跨核原子性。

RISC-V 的宿主 trap 和客户机寄存器镜像共用 `registers::GeneralRegisters`。`GprIndex` 校验指令中的寄存器编号，`byte_offset` 从 Rust 字段布局取得汇编偏移；客户机后端不再维护另一份整数寄存器数组或以编号乘字长推断字段位置。SBI 参数分派仍由上层使用按值复制的 a0..a7 完成。

`capability::has_hypervisor_extension` 在当前 RISC-V 执行环境中实际探测 H 扩展 CSR，不依赖固件 ISA 字符串。它不要求已经初始化运行期，通过自己的临时向量处理探测异常，并恢复调用方的向量、中断使能和不透明 scratch 值。G-stage 模式支持由 `virtualization::PerCpu` 在独占初始化时探测；`GuestBinding` 在本核装载客户机寄存器，在卸载或错误析构时恢复宿主寄存器。调用方必须保持 CPU 固定并持有页表内存。

### 1.2 页表与机器原语

四架构的 someboot 均通过 `DescriptorFlags` 和 `Pte::from_parts` 复用原生编码，仍自行选择启动页表属性与 `TableMeta` 几何。AArch64 的 MAIR 槽选择与共享属性分别表达；RISC-V 启动页表保留自己的脏位、全局位和 T-Head 属性策略。LoongArch 的地址编码与 Linux 64 位 walker 一致，使用位 12..48；地址型目录项、普通页和大页的全局位分别解释。

LoongArch 的原生 TLB refill 入口位于 `arch/loongarch64/entry/tlb_refill.S`，与 LVZ 入口共用 `entry/walker.S` 的四级遍历片段；缺失中间表时，原生入口填入无效项，LVZ 入口恢复宿主并交出二阶段故障。someboot 和运行期通过 `boot::tlb_refill_entry()` 取得运行地址，再由映射所有者转换成 TLBRENTRY 要求的物理地址。入口不依赖栈、TLS 或运行期服务。

AArch64 的 `mmu::{El1, El2}` 显式选择翻译与异常寄存器，`boot::El1::init_trap` 与 `boot::El2::init_trap` 安装同一汇编来源生成的对应向量。`paging::{El1Pte, El2Pte}` 同时可用，分别解释 EL0 权限、PXN/UXN 与非 VHE EL2 的 XN。`El2PagingMeta` 对低 48 位地址作零扩展。`ax-hal::KernelMmu` 按平台交接的 `hv` 模式选择类型，运行期换根、范围失效和 walker 保持同一选择；CPU 不再提供 `arm-el2` feature。

`paging::Pte` 是当前目标的原生页表描述符，`paging::ArchPagingMeta` 对接通用页表遍历器。`mmu::HardwareAddressSpace` 仅包含硬件根和 tag，软件地址空间身份、代数与跨核完成协议留在运行期。

x86 的 `paging::EptEntry` 与原生 `Pte` 分开。`EptFlags` 表达权限、内存类型、A/D 和大页位，`EptMemoryType` 区分合法编码；EPT 不经过 PAT 槽的原生页表编码。AxVM 将自己的 `MappingFlags` 转成这些硬件属性，保留 VM 生命周期和几何，不再维护 EPT 位布局。

AArch64 的 `paging::Stage2Pte` 独立于 EL1/EL2 stage-1 格式，使用非 FWB 的 MemAttr 与 S2AP。读写权限分别由位 6 和位 7 表达，Normal Non-cacheable 的 MemAttr 为 `0b0101`。RISC-V G-stage、AMD NPT 和 LoongArch 客户机表通过原生描述符的显式标志复用相同位布局，分别保留上层的页表几何和属性策略。

`mmu::flush_tlb_range` 在本地 IRQ 关闭期间覆盖范围触及的全部页面，非对齐边界也包括在内；溢出或超过架构阈值时执行本地全量失效。x86 全量失效包含 global 项，优先使用 INVPCID，否则在本核保护下翻转并恢复 PGE。`ax-hal` 保留跨核集合、发布代数与完成等待，直接消费者不再经过 HAL 的本地 CPU 转发接口。

`interrupt`、`cache`、`barrier`、`mmu` 和 `boot` 提供对应功能。`DEVICE` 与 `UNCACHED` 不保证在所有架构上具有不同编码：x86 当前均使用 UC；不带扩展的 RISC-V Sv 页表不含内存属性字段。

AArch64 的 `timer::Timer` 按 `TimerKind` 选择物理、虚拟或 EL2 物理比较器，独占会话分别控制 CVAL、ENABLE 和 IMASK。CPU 层接受原始绝对计数值；固件选择、IRQ 路由、过期截止时间处理和调度队列由平台与运行期拥有。

## 2. 运行期装配

CPU 层已移除对 `cpu-local`、`ax-percpu` 和 `axbacktrace` 的直接依赖。入口状态由 CPU 类型定义，运行期通过经过大小、对齐与范围检查的绝对链接偏移绑定自身存储。

CPU 层定义需要外部提供的服务，最终系统通过 `trait-ffi` 绑定唯一实现。接口使用工作区一致的 Rust ABI，没有运行时注册表或弱默认提供者。

### 2.1 异常诊断

`trap::diagnostics::TrapDiagnostics` 接收 `BacktraceRegisters` 并格式化回溯。`ax-hal::cpu_diagnostics` 调用运行期栈展开器；CPU 层不依赖 `axbacktrace`。提供者必须适用于异常上下文，不得假设寄存器中的地址一定可读，也不能保存格式化器或栈内存引用。

### 2.2 x86 描述符存储

`boot::TrapStorageProvider` 为每个 CPU 交出一次 GDT、TSS 和双重故障栈顶。`ax-hal::cpu_trap_storage` 持有内存，CPU 层构造描述符并执行装载指令。存储在 CPU 关闭前必须保持固定、映射和专有；重复交接在指针返回前失败。

系统调用汇编在取得宿主栈前，通过 `__CPU_LOCAL_TSS_OFFSET` 访问该提供者交出的同一 TSS。字段偏移来自 Rust `offset_of!`。这段入口不能通过 Rust 服务回调寻找栈或恢复 TLS。

## 3. AArch64 PMU

启用独立的 `pmu` feature 后，`pmu::Pmu` 提供当前 CPU 的 PMUv3 会话。设计依据为 Linux v7.1 的 `drivers/perf/arm_pmuv3.c` 和 `arch/arm64/include/asm/arm_pmuv3.h`，本地核对版本为 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`。PR #2274 用于核对 perf 消费需求，不作为寄存器正确性的依据。

### 3.1 本核所有权

`unsafe Pmu::current()` 要求调用者保持 CPU 不变、拥有 PMU 访问权限，并排除本核冲突操作。会话不能跨线程传递。`CounterId` 区分专用 cycle counter、可选的 instruction counter 和经本核数量校验的可编程计数器；无效索引返回错误。PMICNTR 由 `ID_AA64DFR1_EL1.PMICNTR` 独立探测，不从 PMUVer 推断。

`start` 只打开全局计数，不重置已有计数器。`reset` 是独占全体计数器的初始化操作。计数、配置、溢出状态、IRQ 使能和用户访问分开控制；槽位分配、事件组、采样重装与 Linux ABI 留在 perf 所有者中。

### 3.2 能力与宽度

`PmuInfo` 保留 PMU 版本、实际计数器数量和当前有效宽度。PMUv3p5 的长计数器与此前版本区分处理。`EventSupport` 区分支持、不支持与位图不能判定的实现自定义事件；PMCEID 基本及扩展公共事件位图都参与判断。

用户读取权限由显式独占操作控制。PMUv3p9 的 PMUACR 与较早版本的访问控制分开处理；访问授权不代替运行期的事件所有权或任务切换撤权。

## 4. 虚拟化机器入口

`virtualization` feature 启用必需的上下文、FP 与异常修复能力。四架构均提供 `virtualization::{PerCpu, Vcpu}`；RISC-V/AArch64 使用 `enter_guest`，LoongArch 使用 `Vcpu::run` 并显式传入宿主提供的机器现场直映地址。x86 的 `Vcpu::run` 在本架构内部选择 VMX/SVM。上层 `hv` 同步启用平台 FP 初始化与运行期扩展状态保存，避免仅启用 CPU 保存代码却未开放硬件寄存器。

### 4.1 现场所有权

`arch/riscv64/entry/guest.S` 是 RISC-V 客户机进入、退出的唯一汇编实现。整数寄存器使用共享 `GeneralRegisters`，FP 指令复用原生上下文的 `PUSH_FLOAT_REGS` 和 `POP_FLOAT_REGS`。宿主 FP、FCSR、TLS、scratch 和状态寄存器在返回 Rust 前恢复，退出 CSR 在同一窗口保存为按值现场。

`PerCpu` 不可跨线程传递，保存本 hart 启用前的 HEDELEG、HIDELEG、HCOUNTEREN、HVIP 和 HGATP。启用时探测支持的 G-stage 几何并恢复原根；关闭时恢复保存的寄存器。`VirtualizationError` 区分硬件不可用、不支持现有分页模式、重复启用和未启用。宿主物理 IRQ 来源使能由 AxVM 集成层保留。

### 4.2 调用边界

`enter_guest` 是要求调用者独占本 hart、关闭中断并安装有效 VS CSR 和二阶段页表的 unsafe 机器原语。AxVM 的 `arch/riscv64/policy` 通过 `GuestBinding` 装载与卸载状态，并处理 SBI、HSM、虚拟 PMU 和 MMIO 解码；原 `riscv_vcpu` 软件包已经删除。`Exit::cause` 解码入口保存的退出现场，不重读可能已被宿主 IRQ 覆盖的 `SCAUSE`。策略解释在最终机器 IRQ 窗口之外进行，仍由上层维护绑定的 CPU 固定范围。

### 4.3 AArch64 与 LoongArch

AArch64 的客户机向量和最终切换位于 `arch/aarch64/entry/guest.S`，整数寄存器片段与原生入口共用，FP 状态使用 `fp.rs`。返回 Rust 前恢复宿主 TLS、FP、SP_EL0 和完整计时器状态。GIC acknowledge 的入口时序保留；控制器发现、路由和中断处置由上层提供。原 `arm_vcpu` 软件包已删除，PSCI、SMCCC、虚拟 GIC 与计时策略位于 AxVM。

LoongArch 的 `GuestContext` 使用同一 `registers::GeneralRegisters`，普通任务和客户机共用 `fp.rs` 中的 FP/LSX/LASX 状态与指令。`EntryAddresses` 接受宿主验证的可执行直映别名，CPU 不解释内存布局。`Vcpu::bind` 保存本核翻译、虚拟化控制和 scratch 状态；`run` 只执行机器窗口，`unbind` 恢复原状态。调用者须固定 CPU 并保持机器镜像、入口和页表存活，客户机不得访问宿主所有的内存。

`PerCpu` 在 LVZ 关闭时恢复原宿主与客户机向量，重复启用和未启用时关闭返回明确错误。ArceOS `guest-entry` 通过真实二阶段页表执行 HVCL，检查客户机重入、FP 隔离、宿主 FP/TLS/CPU anchor 恢复。原 `loongarch_vcpu` 软件包已删除；IOCSR、客户机软件定时器和退出策略位于 AxVM，解释退出时已解除机器绑定。CPU 只提供本核 GINTC 操作和 IOCSR 指令，平台继续拥有设备寄存器访问权限与副作用。

### 4.4 x86 机器所有权

x86 `Vcpu` 持有 VMCS/VMCB、拦截位图、寄存器镜像及 `GuestXstate`。上层分配并交出 `ControlMemory`，CPU 校验其对齐和范围；`bind/run/unbind` 要求保留同一 CPU 的固定范围，解绑失败时不得释放固定关系。活动 VMCS 被错误丢弃时保留硬件引用的内存，避免释放仍在使用的控制页。

VMX 与 SVM 共用 `entry/xstate.S`。`XstateLayout` 保存本核 CPUID 0xD 布局，按硬件尺寸借用宿主与客户机内存；支持 FXSAVE、XSAVE 和 XSAVES 路径，不使用固定大小的客户机扩展状态数组。进入 Rust 前恢复宿主 FP、CR2、SYSCALL 与 TLS 寄存器。迁核绑定检查布局一致性；AMX、CET、XFD 和 supervisor 扩展状态仍需相应硬件证据。

`ExecutionMode` 从客户机 EFER、CR0 和本后端 CS.L 编码得出；`CrAccessInfo` 保留完整 16 位 LMSW 操作数。`invalidate_ept` 在同一汇编块中执行 INVEPT 并取得失败标志，远端失效与页表回收仍归上层。原 `x86_vcpu` 软件包已经删除，设备、端口、MSR 模拟及客户机内存访问策略位于 AxVM。

## 5. 集成验证

ax-cpu 不提供宿主测试、伪寄存器、测试专用 feature 或 dev-dependency。行为用例位于工作区 `test-suit/arceos/cpu`，通过真实运行期和 QEMU 执行。

### 5.1 命令

在工作区根目录执行格式与目标检查，再运行需要的集成目标。

```sh
cargo fmt
cargo xtask clippy --package ax-cpu --package ax-hal
cargo xtask arceos test qemu --arch aarch64 --test-group cpu
cargo xtask arceos test qemu --arch x86_64 --test-group cpu
```

`cpu/fp-context` 直接检查 AArch64 的 FPCR/FPSR 保存字段及恢复后的硬件值。两个寄存器按 `FpState` 的 32 位字段宽度访问，偏移由 `offset_of!` 生成；原生任务使用 `arch/aarch64/fp.rs` 中的格式，客户机入口从同一处取得保存恢复原语。

PMU 用例使用启用 PMU 的 AArch64 SMP QEMU，验证重复全局启用不破坏计数器及溢出状态清除。页表用例通过真实分配器和通用 walker 检查映射、解除映射、重映射和属性编码。

### 5.2 验证边界与迁移状态

上述用例尚不能证明完整硬件页权限、跨核 TLB shootdown、PMU IRQ 投递、用户采样现场或实体 CPU 事件数值。x86 本地范围失效和 global 项失效另有真实映射与 KVM 用例。perf 用户直接读取权限及所有者切换衔接、剩余平台 CPU 原语迁移和完整验收仍在实施中，不能将已通过的局部矩阵视为整体重构已经完成。x86 用例覆盖 SVM/VMX、VMRESUME、解绑重入、非法进入恢复、x87/YMM 隔离与 INVEPT 状态。当前 QEMU 未暴露 PMICNTR，不能据此声称该专用计数器路径已在硬件运行。

AArch64 `virtualization::PerCpu` 独占本核的 HCR_EL2/VBAR_EL2 安装，在关闭时恢复启用前的完整值。向量、IRQ 分派和 CPU 固定由上层持有，重复启用、未启用时关闭和不满足 2 KiB 对齐的向量返回类型化错误。ArceOS `cpu/virtualization-lifecycle` 在真实 EL2 验证这些转换及控制寄存器恢复。
