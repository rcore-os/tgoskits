# RISC-V AIA 外部中断路由

## 1. 支持边界

`somehal` 为 BananaPi K3 和 QEMU 的监管者级 APLIC/IMSIC 提供外部中断。目标是使控制器探测不依赖设备树排列，并使逻辑 CPU、IMSIC 中断文件和 APLIC 目标保持一致。PLIC 路径、客户机虚拟 PLIC 和 SBI 软件中断保持各自的所有权。

### 1.1 中断身份

当前实现将 APLIC 来源号作为 IMSIC EIID，独占对应的监管者 IMSIC 身份空间。`RiscvAplic` 必须在翻译、配置和启用入口拒绝超过 `riscv,num-ids` 的来源，不能返回成功后跳过 IMSIC 配置。此方案不提供原生 PCI MSI 或动态 EIID 分配；将来引入这些能力时必须替换当前身份空间所有者，不能同时分配同一 EIID。

### 1.2 固件契约

`probe_imsic` 从 `interrupts-extended` 选择监管者外部中断，按各中断文件在 `reg` 区间内的位置建立逻辑 CPU 到硬件目标的对应。缺省 `riscv,hart-index-bits` 从文件数量计算，组索引和 guest 索引参与地址解码；允许 `reg` 留有未声明文件的容量，并支持七位 guest 地址步长，APLIC 目标仍固定为监管者文件。`probe_aplic` 检查 `msi-parent` 指向所选 IMSIC。机器级域（含 Direct 模式）和固件配置的 MSI 地址属于 SBI 固件，监管者驱动不取得其所有权。监管者 Direct APLIC 明确不支持。

### 1.3 原生串口接管

BananaPi-K3 服务提供的设备树中，`/soc/serial@d4017000` 缺少中断属性，运行时因此使用 `irq=None, polling=true`。本地验证设备树按 [Linux K3 UART0 声明](https://github.com/torvalds/linux/blob/master/arch/riscv/boot/dts/spacemit/k3.dtsi) 补齐监管者 APLIC 父节点与 `<42 4>` 高电平来源；驱动不根据串口物理地址猜测 IRQ。

固件串口的 `MCR=0x03` 还关闭了 `OUT2` 输出门控。实板 U-Boot 对照中，UART 的 IIR 已报告发送中断，但 APLIC 来源 42 输入为零；仅把 MCR 改成 `0x0b` 后，对应输入及待处理位均变成 `0x400`。`Ns16550::rearm()` 因而在启用设备中断前打开 `OUT2`，保留已有波特率、FIFO 和其他 MCR 位。`UartVariant::XScale` 同时保持 IER.UUE，不影响 DLAB 选择的分频寄存器。

### 1.4 早期串口选择

`someboot::console::set_earlycon_by_cmdline()` 保留通用 `earlycon=ns16550,<io>,<addr>` 的 `Standard` 行为，并增加显式的 `earlycon=xscale,<mmio>,<addr>`，例如 K3 的 `earlycon=xscale,mmio32,0xd4017000`。XScale 入口仅支持 MMIO，不把标准 I/O 端口串口当作 XScale。`Ns16550::new_mmio()` 分开接收寄存器步长与访问字节数，后者决定实际的 8/16/32 位 volatile 事务。设备树分别使用 `reg-shift` 和 `reg-io-width`；命令行的 `mmio16/mmio32` 选择对应事务与间距。ACPI SPCR 的 GAS `access_size` 先解码为字节数（编码 3 为四字节），未定义时按 `bit_width` 选择，零宽度沿用字节访问。不支持的宽度或不对齐布局在接触硬件前拒绝。早期轮询同样需要 IER.UUE；不能用轮询或中断模式决定硬件变体。配置仍由命令行解析器与现有控制台登记入口管理，不新增状态登记表或改变运行时接管所有权。

`someboot::fdt::setup_earlycon()` 先尝试受支持的显式命令行配置，再保留原有体系结构默认控制台及设备树 `stdout-path` 的选择顺序。因此 RISC-V 显式 MMIO 参数可以覆盖 SBI，而 `earlycon=sbi` 或未指定参数时仍使用 SBI。`set_by_stdout()` 接受 K1、K3 和 Intel XScale 的 `compatible`，并检查节点的完整列表选择 `XScale`，避免通用兼容项排在前面时丢失硬件特性。ACPI SPCR 已限定为 `Full16550` 或 `Generic16550` 的分支继续使用 `Standard`，不根据 MMIO 地址猜测厂商。

RISC-V 的 `setup_page_table()` 为实际选中的 MMIO 控制台建立页对齐、可读写且不可执行的启动映射：物理恒等窗口供写入 SATP 后、重新绑定前继续输出，`__io()` 窗口供进入重定位内核后使用；两者相同时只映射一次。范围复用 `debug_to_memory_desc()`，不会映射全部外设。SBI 或端口 I/O 控制台没有对应描述符，因此不增加映射。

只补设备树名称会遗漏命令行入口；把所有 NS16550 都改为 XScale 则会向标准 UART 的保留位写值。这里复用现有 `UartVariant` 和构造函数，在各配置适配边界明确选择，不引入新的驱动接口。地址和访问方式错误继续沿现有错误返回路径处理，启动层沿用既有默认控制台回退策略；不修改共享服务 DTB、固件环境或持久磁盘。回滚时同时恢复控制台选择逻辑与本地启动参数。

## 2. 初始化与运行

RISC-V 平台入口 `arch/riscv64/mod.rs` 实现 `PlatOp`，负责中断域校验和控制器选择。
`arch/riscv64/irq.rs` 持有 `ActiveIrq` 事务，统一处理陷阱分类、控制器完成、
CPU 本地中断输入、SBI IPI 和次核中断初始化。`plic.rs` 只保留 PLIC 探测、
来源控制、上下文初始化以及携带原上下文的 claim；`aia/aplic.rs` 和 `aia/imsic.rs`
保留 APLIC/IMSIC 的平台适配，可复用硬件操作位于
[`riscv-aia-driver`](../../drivers/intc/riscv-aia-driver/src/lib.rs)。次核由 `irq::init_secondary_cpu()` 依次初始化
PLIC 上下文、当前核 IMSIC，最后开放本地中断输入；未探测到的控制器不执行操作。
PLIC 的普通完成和延迟完成均保留原 claim 上下文，不在通用事务中重算目标 CPU。

`RiscvImsic` 保存不可变拓扑。每核初始化只修改本核的中断文件；`RiscvAplic` 拥有来源模式和亲和性，共享完成端点保存期望启用、延期与归属状态，寄存器反映当前硬件屏蔽状态。控制器操作由现有 rdrive 排他借用串行化，硬中断使用独立完成端点，不取得控制器锁。

### 2.1 每核接收状态

`init_current_imsic` 在保存并关闭本地中断的范围内关闭投递，清理遗留 pending/enable，开放本驱动拥有的有效 EIID，然后设置阈值并打开投递。主核和次核执行相同初始化。运行期屏蔽与重新使能只操作 APLIC，不再根据调用者所在 CPU 改写 IMSIC EIE。

这样 CPU0 可以为 CPU1 注册并启用来源，而无需在持有设备锁时同步等待远端 CPU。APLIC 目标更新影响后续 MSI；已经发出的 MSI 保留在原核文件并由原核确认。通用中断框架按作用域筛选 CPU，不按 `Fixed` 筛选，亦不保证拒绝共享线路上的冲突亲和性。网络等固定所有者消费者必须自行维持来源、动作与工作任务的生命周期；本改动不提供同步迁移或 CPU 下线协议。

### 2.2 来源状态转换

APLIC 初始化先关闭域投递、屏蔽并停用全部来源，发布完成端点和域后再开放域投递。IMSIC 探测优先于 APLIC，并且都在必需的控制器初始化阶段完成。配置来源时保留其已解析目标；电平来源在配置后写入 `SETIPNUM_LE` 重新采样，边沿来源不执行此操作。配置或目标校验失败不能发布新的软件状态或启用来源。

### 2.3 客户机延期完成

`ActiveIrq::defer_riscv_external_completion()` 将物理完成责任转给 `RiscvExternalIrqClaim`，PLIC 保留取得中断时的上下文，APLIC 先屏蔽来源再保留延期责任。令牌的控制器表示在 `somehal` 内部，不可由调用方构造或克隆。`axplat-dyn` 的既有来源表保存 `RiscvExternalIrqClaimSlot`，整体转移令牌，不解析、编码或重建 PLIC/APLIC 的完成状态。AxVM 的 `PhysicalIrqBridge` 将来源发布到客户机虚拟 PLIC；客户机完成后再取走令牌并交回物理控制器，不新增第二份路由登记表。

`RiscvExternalIrqClaim::complete()` 先取走内部完成责任，再访问硬件，返回错误也不会由析构重复完成；直接丢弃仍拥有责任的令牌则执行回滚完成。`RiscvExternalIrqClaimSlot::try_store()` 在占用冲突时原样返回传入令牌，既有令牌保持不变。槽的短 IRQ-save 锁只搬移 `Option<claim>`，完成、日志和硬件访问发生在锁外，因此不与 APLIC 来源锁形成嵌套。IRQ 入口仍不分配内存。

`RiscvAplicIrqHandler` 在短 IRQ-save 原始锁事务内维护 `enabled`、`deferred`、`guest_owned` 和 `retired`。硬件启用条件为期望启用且没有延期责任；延期期间的 enable 只更新期望状态，完成仅按该状态重采样并重新使能。重复 MSI 不取得第二份延期责任。寄存器事务使用 `fence iorw, iorw`，不能用仅约束普通内存的屏障替代。

首次激活按屏蔽、配置来源模式、配置目标、启用的顺序执行；非活动来源可能忽略 TARGET 写入。物理触发方式由宿主设备树解析为 `controller_trigger`，不能从客户机单单元 PLIC 说明符猜测。来源目标固定；缺少 AIA 在途 MSI 排空协议时，停止或已激活路由的回滚将来源退休到平台复位，拒绝再次分配。尚未取得来源的注册失败不得退休其他所有者的来源。

DMA 地址恒等与中断转发不提供设备隔离。物理设备还需正确的客户机内存、缓存属性和完整设备依赖；没有 DMA 停稳证明时，软件关闭不能释放 backing。显式 K3 客户机树必须保留原生总线的 `dma-noncoherent`，不能因缺少这一属性而让驱动错误地采用一致性访问。

`AxVMConfig::has_unmanaged_host_devices()` 将设备名、原始地址、I/O 端口分配与物理恒等地址空间纳入同一保守判定。`Owner::require_host_dma_quiescence()` 对已经开始运行的这些客户机拒绝暂停和停止；停止、重置、客户机复位和删除在关闭运行入口之前检查，内部 `stop_run()` 也不能绕过。尚未运行的 Ready 客户机与没有物理映射的纯虚拟客户机保留正常生命周期。`Owner::update_memory()` 使用相同分配判定拒绝解除内存映射，`close_ivc()` 完成逻辑关闭但保留原有映射与 backing；新增不重叠映射通过内部 `Owner::pause_run()` 暂停 vCPU、虚拟端口与模型设备，再执行原有页表安装事务；它保留全部既有翻译和 backing，不经过管理 `pause()` 的物理 DMA 门禁，因此运行中的直通客户机仍可发布和订阅 IVC。内部暂停不证明物理 DMA 停稳，解除映射仍必须在事务入口拒绝。

RISC-V 内存事务在活动绑定内安装新根时，`RiscvVcpu::set_nested_page_table()` 通过 `GuestBinding::sync_page_table()` 同步保存的 `hgatp` 与硬件 CSR，并完成本地翻译失效，避免解绑时捕获旧值覆盖新根。旧页表仍在所有 vCPU 确认安装、所有已启用宿主 CPU 失效旧翻译后才释放。`RiscvPte::new_page()` 为有效 G-stage 叶项设置 U 位，读、写、执行权限继续由 RWX 控制；这与客户机 VS-stage 的用户权限区分无关，符合 [RISC-V H 扩展的 G-stage 地址翻译规则](https://docs.riscv.org/reference/isa/v20260120/priv/hypervisor.html#_guest_physical_address_translation)。

该拒绝策略保留旧 run 与 backing，不能当作 DMA 隔离或设备停稳能力。`DeviceRuntimeBuilder` 不为没有模型的物理节点登记 `DeviceLifecycle`，因此空生命周期列表不能证明物理设备已停止。允许设备重新归属前，仍需补齐每设备停稳能力、软件生产者退休和 AIA §4.9.3 的硬件同步。

## 3. 方案选择

当前改动修复已有 AIA 实现，不新增通用跨核执行接口。边界调整仍属于高风险平台中断变更，需要领域审查。

### 3.1 替代方案

只在启用时配置当前核保留了原缺陷。运行期远端 EIE 同步需要新的跨核通知、完成和锁顺序协议，并增加失败与取消状态。每核预配置独占 EIID、由 APLIC 控制线路，可以复用现有控制器边界，避免新增同步等待；代价是当前 IMSIC 身份空间不与其他 MSI 提供者共享。

### 3.2 兼容与回滚

来源号和公开 IRQ 域保持不变。无效拓扑、容量不足或错误的 MSI 父节点返回明确错误。源码和启动配置可以一起回滚，不写入持久固件、环境或板卡磁盘。诊断记录实际 IMSIC 文件拓扑与 APLIC 来源容量；不增加周期轮询或超时补偿。

### 3.3 硬件与平台边界

硬件核心采用不依赖 `alloc` 的 `riscv-aia-driver`，接收调用方提供的已映射寄存器窗口和已解析参数。只移动源文件仍会让硬件代码依赖平台；本次将硬件编码、访问和平台所有权分开，保留现有控制器适配接口。

| 所有者 | 负责的内容 | 保留的约束 |
| --- | --- | --- |
| `riscv-aia-driver::Aplic` | 来源模式、MSI 目标、屏蔽、重采样及 MMIO 访问顺序 | 来源、目标和窗口受检，保持 I/O fence；独占硬件能力 |
| `FileLayout`、监管者 IMSIC 操作 | 中断文件地址几何、CSR 初始化与 claim | 不分配内存；特权、当前核文件所有权和中断排除由调用方保证 |
| `somehal::arch::aia` | FDT、映射、逻辑 CPU 对应、控制器注册、锁及来源生命周期 | `enabled/deferred/guest_owned/retired` 继续由平台维护 |
| `somehal::irq` | 不透明完成令牌与预分配槽 | 唯一完成责任，冲突不覆盖，锁内不访问硬件 |
| `axplat-dyn` 与 AxVM | 物理责任转移与客户机中断注入 | 不知道令牌内部控制器格式，不改变现有来源编号映射 |

硬件能力的映射生命周期由构造边界保证，控制与 IRQ 路径通过平台的同一来源事务锁串行访问。配置路径先取得 `rdrive` 控制器，再取得来源锁；硬中断只取得来源锁，不反向进入 `rdrive`。初始化在发布端点之前关闭投递并重置，发布完成后才开放域。IMSIC 的间接 CSR 访问仍处于平台的本地中断排除范围，不能把几何参数正确等同于有权访问当前核文件。

该拆分不增加 MSI 分配器、硬件排空或 DMA 停稳能力。退休来源仍不得复用，客户机仍使用现有虚拟 PLIC。回滚需同步恢复驱动依赖、平台适配、令牌调用方和相应用例；不修改设备树 ABI、持久固件或磁盘内容。

## 4. 验证

验证通过项目 `cargo xtask` 入口执行，先在错误实现上取得同一测试的失败证据，再修复。模型测试只证明解析和拒绝规则，不能替代真实 CSR 与中断投递。

[AxVisor `qemu-riscv-aplic` 用例](../../test-suit/axvisor/normal/qemu-riscv-aplic/qemu-riscv64.toml) 使用 `virt,aia=aplic-imsic`，沿用 `smoke` 的双 Linux 镜像和块设备隔离检查；客户机通过 `qemu-riscv-aplic/` 中的独立设备树使用虚拟 PLIC，两个客户机的 hart 0 分别固定在宿主 CPU1、CPU2。设备树源码及编译后的 DTB 一同保存，避免为其他架构的 `smoke` 增加运行时工具依赖，更新时按源码顶部命令重新生成 DTB。[RISC-V CI 作业](../../.github/ci/checks/axvisor.toml) 同时执行 `smoke`、`qemu-riscv-aplic`、`qemu-aplic-passthrough` 和 `smp-ipi`。其中 `smp-ipi` 保留三 vCPU Linux 的 SBI IPI 能力检查，并在通过 `/proc/sys/vm/stat_refresh` 同步刷新各在线 CPU 的统计后，要求 `/proc/interrupts` 的 IPI 总计数增长。定向入口为 `cargo xtask axvisor test qemu --arch riscv64 --test-case qemu-riscv-aplic`。临时 MMIO 诊断片段及 BananaPi 板卡配置继续保存在本地 `tmp/bananapi-validation/`，不随正式改动提交；PLIC 兼容性同时使用既有 `smoke` 和 `task-ipi` 用例。

`smp-ipi` 同时验证直通客户机的生命周期保护：对同一三 vCPU Linux 依次请求暂停、强制停止、重置和强制删除，要求管理接口返回缺少 DMA 停稳能力，VM 保持 `running`；每次重新接入原 Shell，检查内存变量、清缓存后的直通磁盘读取及 IPI 计数增长。最后由 Linux 执行 `poweroff -f`，要求 VM 进入 `stopped`、三个 vCPU 均为 `Free`，且 `vm show --full` 仍显示 NPT 根和原 RAM 区域；再次启动、重置和删除必须继续被拒绝。判定直接经过 `Owner::command()` 与 `Owner::shutdown_run()` 的生产入口，不能以风险分类单测替代。该测试不证明设备已停稳或具备 DMA 隔离。

### 4.1 确定性回归

临时 QEMU AIA 用例从公开平台 IRQ 接口配置、注册和触发外部来源，检查接收核与完成次数、屏蔽位清零且新边沿保留在 APLIC pending、重新使能后投递并清除 pending，以及亲和性切换、错误目标拒绝后的状态保持和 CSR 选择寄存器保持。电平配置的重采样与边沿不合成中断曾通过 MMIO 协议测试验证，相关测试片段一并归档到 `tmp/`。设备树夹具保持三核硬件与 IMSIC 文件顺序 `[0,1,2]`，将 CPU 枚举改为 `[2,1,0]`；夹具将 hart1 标记为禁用，someboot 选中 `[0,2]`，QEMU 保持真实三核。APLIC 排在 IMSIC 前，缺省 hart-index 位数从文件数量解析。夹具只向 OS 授予前 63 个 EIID，编号上界的拒绝经过真实翻译入口。

### 4.2 验收与剩余边界

2026-11-06 的回归证据：同一三核夹具上，原实现因 APLIC 先探测而启动失败；仅修复优先级后，运行期启用 IRQ 将 `siselect` 从 `0x72` 改为 `0xc0`，断言失败。完整修复通过跨核路由与来源屏蔽测试。两个模块内 MMIO 测试先在旧实现分别观察到电平未重采样、越界来源被接受，再在修复后通过；机器域归属和稀疏文件布局测试补齐兼容边界，共四项单测通过。BananaPi-1 的三核宿主双客户机测试通过 Linux/StarryOS 启动、双向 1 MiB 网络校验与虚拟块设备写入、同步、重新挂载 SHA-256 回读，最终板卡运行阶段为 45.34 秒。定向 `cargo xtask clippy` 的两个软件包共 12 项检查、PLIC `task-ipi` QEMU 和 57 项 CI 执行规划测试均通过。

定向 Clippy、格式化、PLIC 兼容用例以及 BananaPi 上 Linux/StarryOS 的既有双客户机负载共同检查回归。QEMU 不证明 K3 的物理时序、16 核或原生 PCI MSI；实板结果必须单独记录。合入前仍需具备 Rust 非安全代码和 RISC-V 中断经验的领域审查人确认。

2026-11-09 的 BananaPi-K3 单核原生 StarryOS 验证补齐 UART0 的实际中断证据：仅修正 DTB 时日志已显示 `polling=false`，但 Shell 文件回读检查在 180 秒超时；修复 `Ns16550::rearm()` 的 OUT2 门控后，同一检查通过，板卡阶段为 32.40 秒。随后在默认本地配置中增加短命令 `pwd` 和第二次输入检查，全部通过，板卡阶段为 32.97 秒。配置、原始及修正设备树、U-Boot 寄存器对照和运行日志均保存在本地 `tmp/bananapi-validation/`，不修改共享服务的设备树。

### 4.3 早期控制台回归

2026-11-09 的临时 QEMU 验证使用已有 `arceos-helloworld` 应用。原实现虽然启动成功，但忽略显式 XScale 参数，串口写入跟踪中没有 IER.UUE；接通显式参数后，未补启动映射的版本在 `Enabling MMU...` 后超时。最终 XScale、标准 NS16550 MMIO 与 SBI 三条命令行路径均完成启动；只有 XScale 的实际 IER 写入包含 `0x40`，标准路径保持原有寄存器语义。`someboot` 的 16 项定向 Clippy 检查全部通过，包含 ACPI 与各架构构建组合。

同次 BananaPi-K3 原生 StarryOS 使用 `earlycon=xscale,mmio32,0xd4017000`，日志确认串口物理窗口和高地址窗口均被映射，运行时 `polling=false`，文件同步回读、短命令及后续输入全部通过，板卡阶段为 34.17 秒。配置、日志和 QEMU 跟踪在 `tmp/bananapi-validation/earlycon/`；设备树 stdout 选择与 ACPI 分支没有在本次命令行运行中取得额外运行时证明。实板 MMU 切换附近仍有一条早期日志被截断，现有 NS16550 重新绑定会再次 `open()` 并清空 FIFO；本次未调整该初始化生命周期，后续 Shell 检查完整通过。

2026-11-10 的后续修复由 `bind_earlycon_ns16550()` 统一命令行、FDT stdout 与 ACPI SPCR 的 MMIO 绑定。`EarlyconState::initialized_ns16550_mmio_paddr` 记录已初始化的物理设备，同一 UART 重定位时只替换软件寄存器窗口，更换设备仍执行初始化；绑定检查与安装由现有早期所有权守卫和寄存器锁保护。临时 QEMU 判定直接检查 UART 写入跟踪：旧实现重定位时写入 IER、FCR=`0x07` 和 MCR，判定失败；修复后 XScale 与标准 NS16550 两条路径均通过，首次 FIFO 初始化仍存在，MMU 切换期间不再写控制寄存器，完整日志与 `Hello, world!` 均被观察到。证据保存在 `tmp/bananapi-validation/earlycon-rebind/`。

同日 BananaPi-K3 原生 StarryOS 复测通过，MMU 跳转地址与栈地址完整输出，后续文件写入、`sync`、回读、`pwd` 和短命令检查全部完成，运行时保持 `polling=false`，板卡运行阶段为 31.33 秒，结束后租约已释放。`someboot` 的 16 项定向 Clippy 检查与格式化通过。FDT stdout 和 ACPI SPCR 分支共用已验证的绑定逻辑并通过编译检查，本次未增加独立固件运行验证。

### 4.4 物理设备直通回归

[正式 `qemu-aplic-passthrough` 用例](../../test-suit/axvisor/normal/qemu-aplic-passthrough/qemu-riscv64.toml) 使用两个宿主 CPU 和一个 Linux vCPU，显式直通 `/soc/virtio_mmio@10001000`。宿主通过 APLIC/IMSIC 接收高电平来源 1，客户机使用虚拟 PLIC；物理触发方式从宿主设备树解析，客户机树只描述 PLIC 的单单元来源号。客户机 RAM 采用动态恒等分配，使物理设备 DMA 地址对应实际内存。宿主 NVMe 与客户机 virtio 磁盘分别使用同一 Alpine 输入镜像的独立快照，测试结束后不保留写入。

用例挂载客户机直通盘，连续三轮写入 16 MiB、同步、清缓存并校验 SHA-256，每轮都要求 `/proc/interrupts` 中唯一 virtio 来源的计数增长。这条真实 I/O 链路经过 `PhysicalIrqBridge`、虚拟 PLIC EOI 和 `RiscvAplicIrqHandler::finish_deferred()` 的重采样及重新使能。最后由 Linux 自主关机，要求 VM 为 `stopped / Free:1`、报告保留物理资源，并拒绝再次启动。该用例接入 RISC-V CI，可通过 `cargo xtask axvisor test qemu --arch riscv64 --test-case qemu-aplic-passthrough` 独立运行；DTS 与编译后的 DTB 一同保存，重新生成命令位于 `guest.dts` 顶部。

这条用例覆盖连续电平中断完成与客户机退休的集成流程。正式 [ArceOS APLIC 用例](../../test-suit/arceos/drivers/src/aplic.rs) 使用双核 QEMU APLIC/IMSIC，按公开平台接口激活未接线来源 42，确认投递到 CPU1；延期期间的 disable/enable 不得解除屏蔽，禁用后的完成保持屏蔽，重复完成不得再次消费责任，退休后迟到 EOI 不得重新启用来源，退休来源也不得再次激活。该用例通过 `cargo xtask arceos test qemu --arch riscv64 --test-group drivers --test-case aplic` 运行，并纳入 RISC-V CI。测试直接观察实际 APLIC 使能位及跨核 IRQ sink，故障注入移除延期屏蔽判断时必须使外层命令失败。电平持续置位时的重采样仍不由这个边沿来源用例证明；两条用例也不证明 DMA 隔离、来源热复用或实板设备时序。

2026-11-12 的故障验证临时撤掉 `finish_deferred()` 中的 `SETIENUM` 写入，同一用例在挂载直通盘时无法继续，I/O 步骤在 60 秒超时并使外层命令失败。恢复源码后，三轮设备 IRQ 计数依次为 `20 → 42 → 58 → 76`，数据校验、卸载和关机保护检查全部通过；故障改动未保留。

2026-11-10 的 QEMU AIA 宿主、虚拟 PLIC 客户机通过直通 virtio-mmio 磁盘的 16 MiB 写入、清缓存回读校验和正常关机，设备 IRQ 从 30 增至 54；相同磁盘负载的 PLIC 宿主配置也通过。最终版本分别以开启 Smstateen 的 AIA 磁盘负载、关闭 Smstateen 的 16 核双客户机负载检查可选 CSR 兼容，后者包含 Linux 7 vCPU、StarryOS 8 vCPU 及双向 1 MiB 网络校验。

K3 的 16 核宿主、单 vCPU vendor Linux 客户机通过物理 GMAC 双向 1 MiB 随机数据校验，来源 133 的设备 IRQ 从 12 增至 98。物理 UFS 来源 135 的 IRQ 在盘识别后为 437，读取 1 MiB 后为 459；客户机另在原有 ext4 分区中新建独立 4 MiB 文件，写入、同步、清缓存校验并卸载后，经板级复位由原生 Bianbu 再次核对 SHA-256。临时文件随后清理，原生 16 核启动正常，板卡重新显示可用。该验证没有格式化或覆盖现有文件。

同次 K3 状态用例通过公开平台接口向 CPU1 投递来源 43，检查延期期间 disable/enable、重复完成、完成不撤销禁用、退休后旧 EOI 不重新启用，以及退休来源拒绝复用。QEMU 也执行了相同状态流程。配置和日志位于 `tmp/bananapi-validation/passthrough-20261110/`，五个受影响软件包的定向 Clippy 共 83 项通过。物理设备结果限定可信单 vCPU Linux；不证明客户机物理设备 SMP、DMA 隔离、APLIC 来源热复用或 StarryOS 原生 GMAC/UFS 驱动。

### 4.5 分层与完成令牌验证

2026-11-13 的重构验证使用上述生产入口。四个受影响软件包的定向 Clippy 共 25 项通过；令牌模块整理与访问顺序复核后，再对 `somehal`、`axplat-dyn` 的 21 项检查确认通过。驱动经 `cargo xtask cross-test --arch riscv64 --package riscv-aia-driver` 执行三项公开接口测试，覆盖末来源 MMIO 配置与重采样命令、无效输入无寄存器副作用，以及稀疏监管者文件的 guest/group 地址几何。AxVM 的既有 RISC-V IPI 交叉测试 12 项通过。

增强后的 ArceOS APLIC 用例通过；临时去掉令牌析构中的完成动作时，同一用例在“dropping a claim did not complete rollback”处失败，恢复后再次通过。AxVisor 的 `smoke`、`qemu-riscv-aplic`、`qemu-aplic-passthrough` 三项全部通过，分别检查 PLIC 兼容、APLIC 宿主双客户机，以及物理磁盘连续中断、数据回读和关机后的资源保留。运行入口为 `cargo xtask axvisor test qemu --arch riscv64 --test-case smoke,qemu-riscv-aplic,qemu-aplic-passthrough`。日志位于本地 `tmp/aplic-layering/`；第一次运行因沙箱 DNS 无法同步镜像索引而未进入 QEMU，取得网络权限后的同一组测试通过。格式化和最终差异检查通过；本轮未重跑 K3 实板，也未扩大原有 DMA 或来源复用能力。

规范依据：[AIA IMSIC](https://docs.riscv.org/reference/aia/v1.0/IMSIC.html)、[AIA §4.5.2/§4.7](https://docs.riscv.org/reference/aia/_attachments/riscv-interrupts.pdf)、[Linux IMSIC 拓扑解析](https://github.com/torvalds/linux/blob/master/drivers/irqchip/irq-riscv-imsic-state.c)、[Linux APLIC 电平重采样](https://github.com/torvalds/linux/blob/v6.12/drivers/irqchip/irq-riscv-aplic-msi.c)。
