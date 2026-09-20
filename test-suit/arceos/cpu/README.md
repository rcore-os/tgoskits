# CPU 集成用例

本组通过真实 ArceOS 分配器、页表遍历器、CPU 初始化和 QEMU 硬件模型验证 ax-cpu。CPU 包内不建立测试、伪寄存器或测试 feature；本组结果只覆盖实际运行的行为。

## 1. 用例契约

用例直接调用功能命名空间，不依赖私有 `ax_cpu::arch`。`qemu-<arch>.toml` 指定机器、CPU、启动方式与成功标记；恐慌始终判为失败。

### 1.1 页表描述符

`paging/src/main.rs` 通过 `ax_hal::paging::PagingAllocator` 分配通用页表，映射两页、查询页内偏移、解除映射并重新映射到不同物理地址。测试结束后释放中间表，避免以伪分配器替代运行期。

内存属性检查按硬件格式判断。x86 的 `DEVICE` 和 `UNCACHED` 请求均编码为 UC；普通 RISC-V Sv 描述符不带内存属性位，因此基础镜像仅检查普通内存。AArch64 与 LoongArch 分别检查普通、设备和非缓存属性。

`paging/src/ept.rs` 为同一个运行期分配器选择 `EptEntry`，检查 4 KiB 与 2 MiB 页、五种合法内存类型、保留编码和无效大页的地址保留。表不装入 EPTP，因此此用例证明描述符与 walker 对接，不证明客户机 EPT 执行或 INVEPT 生命周期。

AArch64 的 `check_stage2_permissions` 使用 CPU 的 `Stage2Pte` 检查独立读写执行权限、无效项和内存属性。迁入的旧实现分别出现只读项被查询为可写、非缓存项查询 panic，以及零项被查询为可执行设备页；三个失败分别记录后，以相同 ArceOS 入口验证修复。非 FWB 模式的 Normal Non-cacheable 使用 Linux `MT_S2_NORMAL_NC` 的 `0b0101` 编码；此回归仍不代表客户机二阶段失效已验证。

启动描述符通过 someboot 已有的 `mem::mmu::ArchPte` 接口检查。AArch64 的只读和可写请求必须原样查询，执行权限独立检查；LoongArch 的有效项仍必须遵守禁止读位。两个回归分别在旧实现上确定性失败：AArch64 将 `AP_RO=1` 解读为可写，LoongArch 将有效项一律解读为可读。修正后保持同一断言，不以启动成功代替属性检查。

x86 的 `paging/src/tlb.rs` 创建两个私有内核别名，预热真实 TLB，使用非拥有页表视图更换第二页，再显式失效并读取该地址。页框在恢复 PTE、完成失效和同步解除映射之后才释放；IPI 是此用例的必要运行期能力。非对齐范围从第一页最后一个字节开始、长度为 2，原实现错误地保留第二页的旧值 `0x31`，修正后读取替换页的 `0x72`。

同一程序另检查 global 翻译的全量失效。TCG 会过度失效，旧实现也能通过这一断言，因此它不能提供这个缺陷的红绿证据。`paging-kvm` 在真实硬件上确认旧 CR3 重载路径仍读到 `0x31`；修正后读到 `0x72`。`paging-kvm-pge` 隐藏 INVPCID，验证翻转并恢复 CR4.PGE 的路径。两个 KVM 配置使用同一 Rust 程序，不使用 CPU 测试 feature，要求宿主提供可访问的 `/dev/kvm`。

### 1.2 PMU 生命周期

`pmu/src/main.rs` 在 IRQ 关闭期间独占本核 PMU。它校验计数器数量和非法索引，向停用的计数器写入哨兵值，重复启用全局计数并核对值没有被清除，再验证 cycle counter 预装载、溢出状态、清除和停用。

旧实现的确定性失败是第二次全局启用写入 PMCR.P，导致 `0x12345678` 被读成零。修复去掉全局启用的复位副作用后，同一断言通过；后续迁移为 `Pmu` 会话仍保留此断言。x86 页表用例早期出现的 DEVICE/UNCACHED 预期错误属于测试模型修正，不计作 CPU 缺陷的红绿证据。

### 1.3 虚拟化能力探测

`hypervisor-probe/src/main.rs` 在单 hart QEMU 的 `rv64,h=true` 和 `rv64,h=false` 环境中调用实际 H 扩展探测。前者必须报告支持，后者必须从非法指令异常返回并报告不支持；两者分别在 IRQ 开启、关闭时检查原中断状态、STVEC、SSCRATCH 与 TLS。重复调用检查临时向量没有残留。此用例防止探测异常破坏宿主入口绑定，不能以普通启动或固件 ISA 字符串检查替代。

### 1.4 客户机机器入口

`guest-entry` 使用单 hart、H 扩展开启的 QEMU，进入固定的可信 VS 代码，两次通过 `ecall` 返回。用例检查 GPR 结果、宿主 TLS 与 scratch、宿主 FP 不受客户机污染、客户机 FP 跨重入保存，以及退出快照不受随后 live CSR 覆盖影响。同一用例通过 CPU `PerCpu` 启用并探测 G-stage 几何，检查重复启用拒绝，以及关闭后重复关闭拒绝。该代码不访问客户机内存，使用 Bare 地址翻译，不能证明二阶段权限隔离或完整绑定生命周期。

迁入旧入口的 FP 回归确定性读到客户机值 `0x123`，而宿主原值为 `0x789`。复用 CPU FP 保存恢复片段后同一用例通过。退出快照回归在模拟 live CSR 被覆盖后读到零，入口保存快照后保持 VS ecall 原因 10；最终实现将 FP 和快照操作都放入机器汇编窗口，宿主状态完整恢复后才返回 Rust。

### 1.5 x86 异常现场与返回

`trap-entry/src/main.rs` 在单 CPU、硬件 IRQ 关闭期间临时安装 trap 回调，执行实际 `int3` 和软件 IRQ 向量 `int 0xf0`。汇编在触发前保留 PC、SP、FP，回调读取按值现场；断点回调还通过 `GeneralRegisters::set_r10d` 修改保存的 GPR，返回后验证硬件恢复了修改值及 32 位写入的零扩展语义。恢复原回调和 IRQ 状态后才执行断言。

原实现把内核 trap 当作不含 RSP/SS 的短帧，报告的 SP 比实际值小 16 字节；同一用例确定性失败。64 位模式始终保存 SS:RSP，修正后两个入口均通过。该用例覆盖 CPU 入口与现场复制，不替代中断控制器投递、用户态 IRQ 或上层嵌套作用域的验证。

### 1.6 x86 控制区生命周期

`virtualization-lifecycle` 的默认 x86 配置使用 SVM TCG，`virtualization-lifecycle-vmx` 使用嵌套 VMX/KVM。内存由 ArceOS 实际分配器提供 `ControlMemory` 租约，验证错位拒绝、重复启停、活动状态不允许回收，以及关闭后恢复原始 CR0/CR4/EFER。SVM 用例保留另一块实际 HSAVE 页，旧关闭操作错误地清零原 HSAVE 指针，修复后恢复原地址。

VMX 配置验证 `VmxControls` 的绑定、类型化读写、解绑、重绑定及完整控制区释放；错位 I/O 位图导致构造失败时，四份实际内存租约均回收。SVM 配置通过 `Vmcb::save_current_state` 执行实际 VMSAVE，再比较硬件写入的 STAR/LSTAR/CSTAR/SFMASK/KERNEL_GS_BASE 与宿主寄存器值。VMX 还以无效 VMCS 触发真实 VMfailValid，检查失败路径恢复宿主栈和 syscall MSR bank。正常客户机入口由下面的独立用例验证。

x86 `guest-entry` 在 SVM TCG 中使用实际分配的 VMCB、I/O 位图与 MSR 位图执行可信代码。除 VMRUN/VMMCALL 和宿主 FS/GS 恢复外，还执行实际 IN 与 RDMSR，验证位图拦截，检查非法 VMCB 退出及随后重新初始化。`guest-entry-vmx` 使用嵌套 VMX/KVM 和四级 EPT 映射执行 VMLAUNCH/VMCALL，并在 VMCLEAR、重绑定后重新进入。

VMX 的 CR2 回归让客户机写入 `0x12345000`，旧入口返回后宿主错误地保留这一值；修复后返回原始宿主 CR2，并将客户机值留在 CPU 上下文。客户机还实际写入 LSTAR；入口在汇编窗口内保存、恢复 STAR/LSTAR/CSTAR/FMASK/KERNEL_GS_BASE，宿主五个 MSR 保持原值，客户机 LSTAR 跨重入保持。

两个客户机入口都检查实际 x87 与 YMM 状态隔离：宿主装入 π 和全一 YMM0，客户机装入 1.0 并清零 YMM0；退出后宿主值恢复，重入后客户机将原值写回映射页。AVX 在保护模式下执行，不能在实模式中用 #UD 结果证明保存恢复。`guest-entry-fxsave` 隐藏 XSAVE，验证 FXSAVE/FXRSTOR 的 x87 路径。旧后端仅切换 XCR0/XSS，确定性地将客户机 1.0 留在宿主；CPU `GuestXstate` 迁移后相同宿主断言通过。

扩展状态内存由 ArceOS 分配，CPU 按 CPUID.0DH 计算标准或紧凑格式所需空间；不沿用任务上下文的固定 1024 字节上限。VMX/SVM 共用扩展状态汇编，在返回 Rust 前恢复宿主。每次 AxVM 绑定核对目标 CPU 的保存机制和组件布局，CPUID 模拟直接从客户机配置计算尺寸，不再临时安装客户机 XCR0/XSS。

## 2. 执行记录

2026-09-10 后续基线更新至 `5163723577f6fc2c4d0ef255f0cf8d62075614ba`。x86 的 VMX/SVM 控制区、共享 GPR、CR2、syscall MSR 与扩展状态迁移先后通过定向静态矩阵和上述真实客户机用例。YMM 用例分别在 SVM TCG 与 VMX/KVM 通过，另覆盖无 XSAVE 配置；这些结果仍不能替代完整操作系统客户机、跨核迁移或全部 supervisor xstate 的验收。

2026-09-10 早期基线更新至 `3e751a68b84646b352185b18b9840b288c7cf98b`，保留新 dev 的 `uspace` 优先 `tls` 模式及发布版本。CPU、CPU-local、HAL、runtime、someboot 的 93 项静态检查通过，实际 feature 切换脚本通过，RISC-V 与 AArch64 CPU 组通过。随后 RISC-V 入口和每核状态迁移通过 42 项静态检查、Axvisor 构建与上述 `guest-entry` 红绿验证；这些记录对应阶段变化，完整四架构虚拟化验收仍未完成。

2026-09-09 在重构工作树执行这些用例，实施基线为 `d7756853790d85a8ffe42ffa039fd232616b6cd3`。记录对应当时的阶段代码，不代表全部重构验收已完成。

同日同步至 `504808b73b1d6e0643ee46e57f7cd0370c04783a` 后重新运行四架构 CPU 用例，并对新增 AArch64 二阶段回归单独完成红绿验证。运行期寄存器迁移还通过四架构 `task-tls`、`task-scheduler-irq-window` 与 `task-ipi`。旧基线的 IRQ 窗口用例在工作线程就绪阶段失败；新版 dev 已通过 #2332 修正其准备流程，本次只迁移 CPU 调用路径。

RISC-V 共享 GPR 镜像后重新通过 `task-tls`、`task-irq` 和 Axvisor 构建；客户机入口的寄存器访存偏移保持一致，反汇编差异仅为链接位置改变后的 GOT 寻址。H 探测迁移后，两种 `hypervisor-probe` 配置均通过，并再次通过 Axvisor 构建。临时反转关闭 H 时的预期值，确认真实 panic 使最外层 `xtask` 失败，随后恢复同一用例并通过；这是测试失败传播验证，不是已有产品缺陷的红绿回归。

### 2.1 QEMU 结果

通过 `cargo xtask arceos test qemu --arch <arch> --test-group cpu` 执行各目标，使用每个用例自己的启动配置。

| 目标 | 启动 | 已通过用例 |
| --- | --- | --- |
| AArch64 | direct；PMU 使用 `cortex-a53,pmu=on`、GICv2、SMP 4 | paging、pmu |
| x86_64 | UEFI、q35、SMP 4 | paging |
| x86_64 KVM | UEFI、q35、host CPU、SMP 4；另覆盖 `-invpcid` | paging-kvm、paging-kvm-pge |
| RISC-V | direct、SMP 4 | paging |
| RISC-V | direct、单 hart、H 开启及关闭 | hypervisor-probe、hypervisor-probe-no-h |
| LoongArch | UEFI、SMP 4 | paging |

x86 运行使用迁移后的 `TrapStorageProvider`，GDT、TSS 和双重故障栈由 ax-hal 每核区域持有。最终镜像保留唯一的 `__CPU_LOCAL_TSS_OFFSET`，指向同一 TSS。反汇编中，`commit_current_context` 返回后仅赋值两个参数寄存器，随后进入 `context_switch_raw`。

四架构的启动描述符共享 CPU 编码后，以上五个用例再次通过。LoongArch 独立 refill 迁移后重新通过 UEFI SMP 4 `paging`，链接符号表中仅有一个 64 字节的 `__ax_cpu_tlb_refill`。AArch64 比较器迁移后，另通过 ArceOS `rust` 分组的 `task-kernel-timer` 和 `task-sleep`，覆盖回调触发、重复编程、取消及休眠唤醒；这些结果不代表 EL2 比较器或客户机进出已验证。

### 2.2 证据限制

描述符查询不能证明硬件权限和执行限制；新增 x86 用例仅证明所列的本地 TLB 行为，尚未验证远端 CPU 保有旧翻译时的并发 shootdown。AArch64 的 `guest-entry-stage2` 已验证远端 CPU 替换映射后的真实客体读取。`pmu` 已验证四核 QEMU 和八核 OrangePi 的计数、真实溢出 IRQ、同步撤销及每核独立状态；用户入口用例单独记录。TCG 事件数值不能证明实体 CPU 性能事件精度。

后续仍需 ArceOS 覆盖实际页表切换、任务与客户机 FP 状态、用户进入返回、PMU IRQ 与 SMP，以及虚拟化控制区失败和解绑生命周期。x86 已取得上述 VMX/KVM 和 SVM TCG 控制区证据；完整 x86 操作系统客户机、迁核、AMX/CET 等扩展状态和 T-Head 扩展仍需独立验证。OrangePi 5 Plus 的本核 PMU 与 IRQ 已取得实机记录。LoongArch LVZ 与 RISC-V SSTC 的客户机入口证据以各配置运行记录为准，不能由普通 CPU 启动结果代替。

### 2.3 当前接口验证

最新阶段记录集中在 [ax-cpu 验证记录](../../../../docs/design/ax-cpu-validation.md)，包括精确 dev 基线、OrangePi 会话、双核客体 TLB 红绿证据，以及哪些结果尚未覆盖最终提交。PMU 用户入口通过 `UserExecutionContext` 和持有真实页表的 `TaskAddressSpace`，验证授权读取、权限撤销和真实用户 IRQ 的 PC/SP/FP，不通过原始入口跳过运行期校验。
