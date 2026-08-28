# 启动调试参考

本文件记录 LoongArch 动态统一可扩展固件接口平台启动、someboot 对称多处理、StarryOS 测试和 Axvisor LoongArch 虚拟化扩展 QEMU 冒烟测试的项目经验。

## 分层映射

| 层 | 常见文件 | 必须一致的内容 |
| --- | --- | --- |
| 目标规格 | `scripts/targets/**/<triple>.json` | 二进制接口、软浮点、重定位模型、链接器、内核恐慌、标准库或 musl 支持 |
| 构建编排 | `scripts/axbuild/src/{build.rs,context,test/qemu.rs,*}` | 体系结构到目标映射、功能、统一可扩展固件接口模式、QEMU 命令、根文件系统映像 |
| 测试数据 | `test-suit/{arceos,starryos,axvisor}/**` | 运行与构建 TOML、匹配规则、处理器数量、固件模式 |
| 启动加载器 | `platforms/someboot/src/**` | 入口二进制接口、重定位、内存映射、分页、陷阱、对称多处理、电源 |
| 处理器运行时 | `components/axcpu/src/<arch>/**` | 陷阱帧布局、上下文切换、浮点或向量、用户返回、运行时第一阶段页表格式和地址转换缓存语义 |
| 动态平台 | `platforms/{axplat-dyn,somehal}/**` | 从固件取得的运行时内存、中断、定时器和电源事实 |
| 驱动 | `drivers/**`、`patches/virtio-drivers/**` | 设备资源映射、直接内存访问、外围部件互连总线命令位、虚拟输入输出传输 |

高层出现启动失败时仍要审计低层契约。例如 Starry 根文件系统失败可能由外围部件互连总线命令位造成，Axvisor 挂起也可能来自 someboot 退出固件服务后的交接。

## Axvisor 已解析设备图与客户机固件

各 Axvisor 体系结构在生成最终客户机固件前，自行构建并解析设备图。共享设备图不规定跨体系结构的统一设备顺序：AArch64 仍先安装虚拟通用中断控制器，再安装中断消费者；RISC-V 保留 PLIC 硬件线程和上下文设置；x86 保留本地高级可编程中断控制器、输入输出中断控制器、可编程间隔定时器和高级可编程中断控制器访问顺序；LoongArch 保留 IOCSR、EXTIOI 与 PCH 级联。

调试设备或中断缺失时，从 `DeviceModel::requirements()` 的一个资源槽，追踪到 `ResolvedDeviceGraph`，再追踪到扁平设备树或高级配置与电源接口计划及 `DeviceBuildContext`。运行时设备必须使用已解析地址和 `IrqLine.input()`。图保留的同一动态模型执行构建，所有 `ResourceClaimSet` 槽都成为租约后才能封装运行时。对 `console0`，先确认最终模型和固定绑定来自机器后备、宿主固件快照还是同标识用户覆盖。内存映射输入输出或端口输入输出退出只能执行一次可选分派；先 `find_*` 再第二次分派说明仍有陈旧路由。

x86 直接启动 Linux 时，修改内核命令行策略前核验：

- 高级配置与电源接口映像完整位于 `0xe0000..0x100000`，根系统描述指针按 16 字节对齐；
- `boot_params.acpi_rsdp_addr` 保存该客户机物理地址；
- E820 保留高级配置与电源接口映像和传统低内存窗口；
- 根系统描述指针、扩展系统描述表及所有表的校验和与指针闭包有效；
- 显式 `acpi=off` 后备使用同一高级可编程中断控制器计划生成多处理器表。

x86 OVMF 或基本输入输出系统启动时，核验设备图固定的端口窗口 `0x510..0x512` 与 `0x514..0x51c` 已陷入，并确认 fw_cfg 发布 `etc/acpi/tables`、`etc/acpi/rsdp` 和 `etc/table-loader`。选择器读取成功不能证明表已安装；检查表加载器的直接内存访问操作，并确认 Linux 通过扩展系统描述表发现 `DSDT`、`APIC`、`FACP`、`SPCR`。Linux 的 `/sys/firmware/acpi/tables` 不导出根扩展系统描述表。

Axvisor x86 嵌套 OVMF 用例按下列顺序调试：

1. 列出 `normal` 分组，确认发现 `ovmf-acpi-vmx` 和 `ovmf-acpi-svm`。VMX 只在 Intel/VMX 内核虚拟机宿主运行，SVM 只在 AMD/SVM 宿主运行；两个构建配置都不能选择后端 Cargo 功能。
2. 解释固件输出前先读文件准备证据。它记录 Ostool CODE 与 VARS 路径、字节数、SHA-256、分离或单体布局和最终 4 MiB 客户机映像。单体 CODE 映像必须说明记录的 VARS 未使用。
3. 确认最终客户机映像路径就是共享客户机 TOML 中 `uefi_firmware_path` 选择的路径，不能与启动 Axvisor 宿主的外层 QEMU 闪存混淆。
4. 核验 fw_cfg 发布三个高级配置与电源接口文件，再检查表加载器分配、指针、校验和与直接内存访问错误测试。选择器读取或固件横幅只是中间检查点。
5. 要求客户机初始内存文件系统输出 `AXVISOR_X86_OVMF_ACPI_PASSED`。该标记表示 OVMF 已交接给 Linux，Linux 接受 DSDT、APIC、FACP、SPCR、ttyS0 和输入输出中断控制器。标记缺失时保留完整命令、固件证据、最后可靠状态和第一个确定错误。

这些嵌套开放虚拟机固件用例仍通过 `fw_cfg` 提供 Linux 内核、初始内存文件系统和命令行，不证明客户机外围部件互连总线启动磁盘、固件系统分区或 Linux 固件存根启动路径。后续能力失败不能通过修改这些只用于验证的用例解决。

AArch64 宿主替换中，把不可变固件计划中的每个 GICR 区域和步长，与传给运行时的 `ArmVgicConfig` 比较。不得通过向下转换已注册 GIC 前端推断配置。宿主 GIC 内存映射区域保持陷入，客户机写入不能改变宿主 GICD 或 GICR。

## 处理器局部寄存器所有权

`cpu-local` 是宿主处理器区域、当前上下文、内核线程局部存储寄存器、上下文绑定和体系结构选择抢占语义的唯一所有者。`ax-percpu` 只提供类型化模板、布局和区域实现，不能独立选择体系结构寄存器。最终映像的两种模式互斥：

| 体系结构 | 处理器区域 | Linux 当前上下文映像 | 单内核线程局部存储映像 |
| --- | --- | --- | --- |
| x86_64 | GS 基址 | 锚定当前上下文，FS 不使用 | 锚定当前上下文，FS 保存上下文线程局部存储 |
| AArch64 | 异常级 1 使用 TPIDR_EL1，异常级 2 使用 TPIDR_EL2 | SP_EL0 保存当前上下文，TPIDR_EL0 不使用 | SP_EL0 保存当前上下文，TPIDR_EL0 保存线程局部存储 |
| RISC-V | 从当前上下文指针或 `sscratch` 恢复 | `tp` 保存当前上下文，`sscratch` 为零 | 锚定当前上下文，`tp` 保存线程局部存储，`sscratch` 保存处理器基址 |
| LoongArch | r21，并镜像到 KS3 | tp 保存当前上下文 | 锚定当前上下文，tp 保存线程局部存储 |

最终可执行与可链接格式文件恰好含一个 `.percpu.template`、一个 `.percpu.init` 描述符表和一个 `.percpu.align` 表。someboot 或其他平台按该几何动态分配运行时区域，在最终地址初始化每个类型化对象，冻结布局后才绑定处理器。不存在已链接运行时别名，模板大小不能依赖对称多处理数量。链接边界只使用 `__PERCPU_*` 与 `__CPU_LOCAL_*`；x86 陷阱入口使用相对 `__CPU_LOCAL_TSS_OFFSET`。

精确初始化后的 `CpuAreaRef` 地址就是区域身份。最终映像不含处理器局部二进制接口版本、布局代数、标记或提供者特征外部函数接口。`CpuPin<'scope>` 检查实时处理器基址、前缀自指针、索引和当前头，并且不能逃逸保护。原子标量要求排除迁移；共享 `T: Sync` 还要求对象自身同步；可变局部对象要求在排除中断、重入和冲突远端访问后取得 `ExclusiveCpu`。处理器区域只能在目标处理器离线且原始目的区域独占时构造。

上下文切换发布顺序为：验证外出绑定，绑定下一稳定执行上下文头，准备全部可失败转移，提交选中来源，执行裸切换，在进入尾部解绑上一上下文头。中断关闭的 `CpuPin` 跨越整个序列。未提交准备令牌回滚下一绑定；前一绑定代数拒绝上下文重新绑定后的陈旧进入尾部。该代数是运行时并发保护，不是二进制接口版本。虚拟处理器退出必须在返回宿主 Rust 前恢复宿主寄存器契约；LoongArch KS4 与 KS5 保留为虚拟处理器暂存，AArch64 在调用 Rust 异常处理前恢复宿主 TPIDR_EL0。AArch64 把 SP_EL0 借给用户空间前，必须把唯一当前头保存到固定内核栈；处理器锚点镜像不是有效后备。

抢占入口返回绑定选定所有者的线性令牌。x86_64 使用处理器锚点；装载与存储体系结构使用当前上下文头。最后一个待退出深度保持为一，直到运行时取得逐处理器调度器接力棒。`cpu-local` 不能包含接力棒、任务待处理策略、运行时所有者标记或任何 `scheduler_*` 接口。处理器拥有抢占状态时，新进入上下文显式结束切换深度；暂停保护在另一处理器恢复时消费旧证明并接管目的处理器外出上下文留下的等价深度。上下文拥有状态时，令牌所有者跨迁移保持，新头从深度零开始。

调试时先确认类型化逐处理器布局已最终确定并冻结，再绑定处理器。次处理器同时检查体系结构寄存器及其定义镜像，例如 RISC-V `sscratch` 或 LoongArch KS3。第二个逐处理器当前上下文变量可能掩盖普通执行中的陈旧寄存器，却在陷阱或虚拟处理器退出时失败，因此不得作为后备。

## 最终映像运行模式

- Starry 使用原裸机目标，以 `build-std=core,alloc` 构建 `no_std`、`no_main` 位置无关可执行文件；对称多处理是构建能力，运行时处理器上限另行配置。最终文件必须为 `ET_DYN`，且没有 `PT_TLS`、`.tdata` 或 `.tbss`。
- Axvisor 保持标准库与 musl 位置无关可执行文件，并从 axruntime、axhal、`cpu-local`、axvm、axplat-dyn、somehal 到 someboot 显式选择完整线程局部存储链。AxVM 在每次客户机转换前后保存宿主内核线程局部存储值，并验证精确处理器区域。
- ArceOS 默认保留线程局部存储。用户空间构建使用同一体系结构寄存器保存 Linux 当前上下文，因此 `uspace + tls` 是配置错误。
- someboot 分别生成线程局部存储与无线程局部存储链接布局。可重定位直接映像应在多个加载偏移检查最终文件，只接受体系结构支持的相对重定位类型。

## AArch64 Axvisor 异常级 2 检查

- `arm_vcpu` 替换当前物理处理器正在使用的 `VBAR_EL2` 时，先发布当前异常级宿主中断处理函数，再写入 `VBAR_EL2` 并执行指令同步屏障，随后更新 `HCR_EL2` 并再次执行指令同步屏障。否则异常可能观察到从未设计为同时生效的向量、处理函数和控制寄存器组合。当前异常级同步异常的内核恐慌报告至少包含 `ESR_EL2`、`FAR_EL2`、`ELR_EL2`、`SPSR_EL2` 和 `HCR_EL2`，以便把偶发宿主地址转换错误追溯到第一次无效访问，而不是只根据综合征分类。
- Axvisor `hv` 功能链只在 AArch64 选择 `ax-cpu/arm-el2`。保持 `ax-hal/hv -> axplat-dyn/hv -> somehal/hv`；`somehal` 的 AArch64 可选 `ax-cpu` 依赖拥有 `arm-el2` 边。编译成功不证明已选中异常级 2 寄存器实现，无条件依赖又会错误影响其他体系结构。
- 异常级 2 映像若编译了异常级 1 页表路径，`ax-mm` 可能看似成功初始化，却把新页表根写入 `TTBR1_EL1`。活动 `TTBR0_EL2` 仍指向 someboot 页表，第一次访问动态映射设备时就会错误或挂起。PhytiumPi 的典型停点是 `rdrive` 扁平设备树初始化消息后的第一次 GIC 分发器读取。
- 确认运行时报告 `EL: 2`，检查已解析的 `ax-cpu` 功能集含 `arm-el2`，并在插桩驱动前验证 `ioremap` 后的设备访问。
- Axvisor QEMU 和板卡用例独占处理器数量契约。测试请求必须丢弃交互快照中的 `smp`，否则陈旧 `tmp/axbuild/.axvisor.toml` 会静默缩小宿主。Phytium 客户机分配逻辑处理器 2 时会退回处理器 0，并可能停在第一次虚拟定时中断，即使虚拟处理器切换本身正确。

## 动态统一可扩展固件接口平台

- 动态平台表示平台事实由 `someboot`、`somehal` 和 `axplat-dyn` 从固件或运行时发现，不表示可以省略体系结构特定页表、陷阱、定时器、中断和电源代码。
- 调试时分离页表阶段：`someboot` 负责启动页表和内存管理单元交接；`ax-cpu` 负责运行时第一阶段页表项与地址转换缓存；虚拟化组件负责第二阶段。三者可以使用 `page-table-generic` 执行通用操作，但该软件包不能选择活动体系结构。
- 先对齐 x86_64 动态路径中的固件磁盘布局、`to_bin`、闪存或 OVMF 和交接预期。
- 动态平台功能在 `ax-std`、`ax-hal`、`ax-driver`、axvm 和操作系统软件包间保持一致。部分 `plat-dyn` 功能常能编译，却在设备或内存初始化后失败。
- 标准库或 musl 目标优先从已知 Rust 目标派生 JSON，再最小调整二进制接口、链接器、重定位和软浮点。`none-softfloat` 通过不能证明 musl 或标准库二进制接口正确。
- 优先使用运行时内存映射，少用板卡常量。`phys_to_virt` 等早期辅助函数必须适用于调用时所在阶段。

## someboot 启动顺序

审计早期启动时依次确认：

1. 入口保存固件参数，并在未初始化数据段清理或重定位破坏前记录。
2. `ExitBootServices` 前早期串口可用。
3. 取得固件内存映射，完成分类并转换为内核内存模型。
4. 地址转换辅助函数使用前，内核映像物理范围、加载偏移和高地址范围已经确定。
5. 页表或直接映射窗口覆盖当前代码、启动栈、页表、内核高地址、设备资源和启动数据。
6. 按当前阶段要求的地址形式安装陷阱向量。
7. 启用内存管理单元后执行所需屏障、地址转换缓存刷新和地址基准安全跳转。
8. 内存管理单元启用后控制台和内核恐慌路径仍可用。
9. 重定位后解析单一逐处理器模板和描述符表。
10. 动态分配运行时处理器区域和次处理器栈，每个类型化区域只初始化一次、冻结并通过体系结构处理器局部寄存器契约绑定。
11. 启动参数与页表对其他处理器可见后才释放次处理器。
12. 体系结构钩子只负责唤醒传输。someboot 公共所有者在调用前发布逐处理器 `KICKED`，等待次处理器在公共入口报告 `ALIVE`，再把该处理器释放为 `SHOULD_ONLINE`。握手不进入不可变跳板元数据，并与稍后的调度器、中断和定时器在线发布分离。详见 `docs/design/someboot-secondary-cpu-startup.md`。

## RISC-V 扁平设备树多处理器

- 只枚举固件标为可用的处理器节点。没有 `status` 表示可用，`okay` 或 `ok` 表示可用，`disabled` 必须跳过。
- 扁平设备树 `reg` 硬件线程标识保持为固件处理器标识，再单独映射到连续逻辑标识。VisionFive2 的 `cpu@0` 是禁用的 S7 管理硬件线程，可用 U74 为 `cpu@1` 到 `cpu@4`；完整启动应从硬件线程 1 开始并拉起 2 到 4，不能退成单处理器。
- 释放次处理器时板卡陷入，修改 `max_cpu_num` 前先输出启动设备树的 `/cpus`。禁用或非操作系统处理器节点常导致 `cpu_on` 选择错误硬件线程。

## RK3576 ROCK 4D 板卡

维护中的 ROCK 4D 使用仓库设备树和能通过板卡运行器加载 StarryOS 映像及设备树的 U-Boot 固件。固件必须实现设备树声明的 PSCI 1.0 `smc`，否则即使映像和处理器拓扑正确也无法释放次处理器。

- 使用 `os/StarryOS/configs/board/rock-4d.dtb`，根兼容为 `radxa,rock-4d` 和 `rockchip,rk3576`。不得静默换成其他 RK3576 板卡的 U-Boot 驻留设备树。
- 控制台为 `serial0`，UART0 地址 `0x2ad4_0000`，波特率 1,500,000，8-N-1。本地模板 `os/StarryOS/configs/board/rock-4d-uboot.toml` 使用 `/dev/ttyUSB0`；转接器枚举变化时只调整宿主串口路径。
- 设备树描述八个 PSCI 处理器：Cortex-A53 MPIDR `0x0..0x3`，Cortex-A72 MPIDR `0x100..0x103`。硬件标识与连续逻辑索引分离。维护中的单处理器和八处理器启动均已验证。
- GICv2 处理器目标位是固件或控制器接口标识，不是逻辑索引。逐处理器初始化扫描 32 个私有 `GICD_ITARGETSR` 字节，把唯一单位置位掩码记录到共享路由表，供共享外设中断亲和性、AxVM 分配物理中断和软件生成中断使用。只有 `GICD_TYPER.CPUNumber` 明确为单处理器且目标寄存器读零写忽略时，零掩码才有效；运行时处理器上限不足以证明。此时不改共享外设中断目标，软件生成中断使用自身过滤。找不到唯一目标位时，多处理器发现失败。
- AArch64 QEMU 多处理器回归使用 `virt,gic-version=2` 和四处理器，运行 `cargo xtask arceos test qemu --arch aarch64 --test-group rust --test-case task-ipi`。用例必须证明自身软件生成中断被认领，其他处理器发送的回调只在选定远端处理器执行。该验证补充但不替代 GICv2 单处理器读零写忽略路由单元回归。
- RK3576 时钟复位单元兼容为 `rockchip,rk3576-cru`，地址 `0x2720_0000`、大小 `0x50000`。早期证据应依次出现 `RK3576 CRU reg: addr=0x27200000, size=0x50000` 和 `RK3576 CRU clock/reset registered successfully`。
- 电源管理单元父节点地址 `0x2738_0000`，子节点兼容 `rockchip,rk3576-power-controller`。探测完成输出 `Rockchip power-domain provider registered successfully`，单域启用调试输出 `Rockchip power domain 0x... enabled`。
- 引脚控制提供者绑定 `rockchip,rk3576-pinctrl`，映射输入输出控制广义寄存器文件 `0x2604_0000` 和可选系统广义寄存器文件 `0x2600_a000`，从 `gpio-ranges` 发现 GPIO0 到 GPIO4。成功输出 `Rockchip RK3576 pinctrl registered successfully`。通用 `rdrive` 扁平设备树探测在调用控制器前应用 SDMMC0 默认 `pinctrl-0`。两侧都保留回归：`rdrive` 证明默认状态先于消费者回调，RK3576 引脚控制测试证明 ROCK 4D SDMMC 状态写入预期寄存器。不得增加第二个消费者专用应用调用，也不能只用稍后成功挂载作为证据，因为 U-Boot 可能留下可用引脚状态。
- 仓库设备树把 SDMMC0 `vqmmc-supply` 接到始终启用的 RK806 `vccio_sd_s0` 电源管理集成电路电源轨，没有为该控制器声明固定通用输入输出调压器。其他设备树变体采用固定通用输入输出供电时，卡初始化前必须出现 `Fixed regulator ... enabled via pinctrl` 或存储专用成功日志。
- 当前存储路径依赖时钟复位能力控制 SDMMC0 源时钟与总线时钟门控、源选择、分频和复位。启动到存储初始化却无法挂载根设备时，先检查时钟复位注册，再检查 SDMMC0 时钟与相关电源域，之后才调试文件系统。

板卡服务提供的精确名称必须先核验：

```bash
cargo xtask board ls
```

维护中的单处理器回归：

```bash
cargo xtask starry test board \
  -c boot \
  --board rock-4d \
  -b Rock-4D
```

已验证的八处理器路径：

```bash
cargo xtask starry board \
  -c os/StarryOS/configs/board/rock-4d.toml \
  --smp 8 \
  --board-config os/StarryOS/configs/board/rock-4d-board.toml \
  -b Rock-4D
```

两个运行都必须到达 `root@starry:/root #` 并输出独立 `STARRY_ROCK4D_BOOT_OK`；只有命令行提示符不算通过。按交接顺序排查：板卡服务选择、UART0 输出、仓库设备树兼容与 PSCI 方法、目标 MPIDR 发现、时钟复位和电源注册，最后才检查次处理器释放、存储或设备驱动。

## LoongArch 经验

- LS2K1000 在块硬件上下文激活后重复输出 `failed to lock LS2K1000 LIOINTC when claiming LIOINTC IRQ`，表示硬中断与控制器锁次序反转，不是无害伪中断。按 AArch64 GIC 模式拆分：`rdif_intc` 控制器和配置寄存器归任务，独立 LIOINTC 处理器接口只含中断状态、域、父线路和原子启用状态。硬中断查找或锁住控制器会在被中断任务释放设备保护前因电平中断不断重入。
- U-Boot FIT 启动中，生产与交接契约保持一致：使用规范体系结构名 `loongarch`；U-Boot 以符合设备树规范的 8 字节对齐地址传递设备树；FIT 提供的设备树通过 UHI 约定传给 someboot，即 `a0 = -2`、`a1 = fdt`。检查 `legacy_hdr_os` 的厂商 `CONFIG_LOONGSON_BOOT_FIXUP` 不能作用于 FIT 映像。
- 地址转换缓存填充入口和普通异常入口使用不同寄存器，可能需要不同地址形式。需要物理填充向量时，不能复用高地址虚拟符号。
- 重定位符号按正在运行的映像解析。LoongArch 多处理器次处理器异常向量使用 `sym_running_addr!(__exception_vectors)` 等运行时辅助函数，填充入口使用对应物理地址。
- 次处理器在串口可用前就可能陷入。在直接映射窗口、切栈、页表寄存器、陷阱向量和跳入公共入口前后放标记。
- 每个处理器都初始化陷阱向量，不只启动处理器。
- 体系结构启动传输前刷新启动参数或执行屏障，否则次处理器可能看到陈旧栈、页表或逐处理器数据。
- 逻辑处理器标识与固件标识分离，LoongArch 固件标识不保证是连续数组索引。
- 顺序不确定时对照本地 Linux LoongArch 代码，重点检查直接映射窗口、控制与状态寄存器写入、地址转换缓存填充向量、异常入口、多处理器参数交接和缓存屏障。

## 查找本地 Linux 源码

需要以 Linux 行为作为体系结构参考时，先找本地内核树，不依赖记忆或在线搜索：

```bash
find "$PWD" "$PWD/.." "$HOME" /home -maxdepth 4 -type f -name Makefile \
  -path '*/linux*/Makefile' 2>/dev/null
```

候选目录必须含顶层 `Makefile`、`Kconfig` 和目标体系结构目录。项目与 Linux 目录映射为：

| 项目体系结构 | Linux 目录 |
| --- | --- |
| `loongarch64` | `arch/loongarch` |
| `x86_64` | `arch/x86` |
| `aarch64` | `arch/arm64` |
| `riscv64` | `arch/riscv` |

打开大文件前先用 `rg` 搜索。首批模式可用 `setup_arch`、`start_kernel`、`smp_prepare_cpus`、`secondary_start`、`cpu_up`、`set_exception`、`tlb`、`fixmap` 和体系结构特定寄存器名。

## Axvisor LoongArch 虚拟化扩展容器

宿主 QEMU 可能缺少所需扩展，因此通过容器验证：

```bash
docker run --rm -v "$PWD:/workspace" -w /workspace \
  ghcr.io/rcore-os/tgoskits-container-axvisor-lvz:latest \
  bash -lc 'cargo xtask axvisor test qemu --arch loongarch64 --test-group normal --test-case smoke'
```

注意：

- 在容器内构建并运行 `cargo xtask`。宿主构建的 `target/debug/tg-xtask` 可能固化容器中不存在的 `CARGO_MANIFEST_DIR`。
- 认定内核错误前检查 `/opt/qemu-lvz/bin/qemu-system-loongarch64`、`/tmp/ostool/ovmf/loongarch64` 下固件和 musl 工具链。
- 输出到 `Exiting UEFI boot services...` 后停止时，在 `ExitBootServices`、内存映射交接、退出后第一次控制台调用和内存管理单元或陷阱设置前后立即插桩。
- 容器通过后，如果持续集成或开发流程依赖该映像，仍需编写与宿主无关的文档。
- 客户机控制台失败时区分宿主串口和机器所有客户机串口。宿主串口不得进入客户机直通集合。先检查固定 LoongArch 客户机资源、生成的扁平设备树或固件表、虚拟 PCH-PIC 电平状态和 Axvisor 控制台多路复用器，再修改宿主中断路由。

## QEMU 调试模式

- 首条可靠输出前失败时加入 `-S -s`，在复位处停止并连接 GDB。
- 加入 `-d int,cpu_reset,guest_errors` 记录陷阱、复位和无效客户机访问。
- 用短串口标记隔离阶段，例如 `E` 表示固件入口，`M` 表示内存映射，`X` 表示退出启动服务前，`x` 表示退出后，`P` 和 `p` 表示分页前后，`T` 表示陷阱向量后，`S` 表示释放次处理器前。
- 最终提交前删除标记，除非它们成为长期诊断接口。
- QEMU 由 `ostool` 启动时，临时修改本地 ostool 或 `xtask` 包装，不要手工拼接不同命令。复现命令必须忠于失败路径。

## 症状分类

| 症状 | 首要检查对象 |
| --- | --- |
| 停在固件退出处 | 内存映射键、退出后启动服务调用、退出后控制台、交接地址、陷阱向量前异常 |
| 启用内存管理单元后立即复位 | 页表根、身份或当前映射、屏障与地址转换缓存刷新、跳转目标 |
| 高地址取指错误 | 内核高地址映射、重定位偏移、符号地址基准、直接映射窗口 |
| 地址转换缓存填充递归 | 填充向量地址、栈映射、处理器映射、控制与状态寄存器顺序 |
| 次处理器无输出 | 先检查逐处理器 `KICKED/ALIVE/SHOULD_ONLINE` 与活动启动句柄；再查体系结构唤醒、启动参数、缓存刷新、栈、逐处理器基址、陷阱和逻辑标识。第二次 `start_secondary_cpu` 必须返回 `StartupInProgress`，不能自旋。x86 的 SIPI 必须是 `APIC_DM_STARTUP` 即 `0x600`，句柄丢弃或超时后不能清除所有者。 |
| ArceOS 工作但 Starry 失败 | 根文件系统准备、标准库或 musl 二进制接口、控制台输入功能、终端假设、控制程序尺寸 |
| Starry 命令行工作但分组测试失败 | 生成运行器路径、复制文件、成功匹配规则、`shell_init_cmd` 与 `test_commands` |
| AArch64 Axvisor 停在第一次动态设备读取 | 缺少 `ax-cpu/arm-el2`、活动页表错误、陈旧 `TTBR0_EL2` 启动页表 |
| Phytium 客户机停在 `arch_timer` 后 | 继承的板卡测试处理器上限、虚拟处理器掩码后备、虚拟定时路由 |
| Axvisor 构建成功但 QEMU 挂起 | 固件路径、虚拟化扩展 QEMU、客户机映像、动态平台内存映射、退出固件后的转换 |
| 虚拟输入输出块设备缺失 | 外围部件互连总线命令启用、传输、设备映射、直接内存访问转换、根磁盘参数 |

## 验证命令

LoongArch 动态平台按下列层级验证：

```bash
cargo test -p axbuild --lib
cargo xtask ktest qemu --workspace --arch loongarch64
cargo xtask arceos test qemu --arch loongarch64
cargo xtask starry test qemu --arch loongarch64
docker run --rm -v "$PWD:/workspace" -w /workspace \
  ghcr.io/rcore-os/tgoskits-container-axvisor-lvz:latest \
  bash -lc 'cargo xtask axvisor test qemu --list --arch loongarch64'
docker run --rm -v "$PWD:/workspace" -w /workspace \
  ghcr.io/rcore-os/tgoskits-container-axvisor-lvz:latest \
  bash -lc 'cargo xtask axvisor test qemu --arch loongarch64 --test-group normal --test-case smoke'
```

修改相关 Rust 逻辑时，在格式化后执行定向静态检查：

```bash
cargo fmt
cargo xtask clippy --package axbuild
cargo xtask clippy --package someboot
cargo xtask clippy --package ax-cpu
cargo xtask clippy --package axplat-dyn
cargo xtask clippy --package ax-driver
```

软件包集合按实际差异调整。只修改技能文档时不需要运行静态检查。
