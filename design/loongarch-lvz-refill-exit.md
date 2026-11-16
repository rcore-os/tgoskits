# LoongArch LVZ refill 错误退出

## 1. 问题与取舍

CPU 层负责从客户机机器状态安全返回宿主，AxVM 再解释已经卸载的退出。
本设计只修复二阶段页表的中间目录缺失时返回 Rust 的机器边界，不改变客户机
地址解析、缺页所有权或软件设备模拟，也不宣称它已经解释了某次实板停顿。

### 1.1 已核实缺口

`entry/walker.S::AX_CPU_WALK_4K` 在目录项为零时转入
`entry/guest.S::.Lguest_stage2_tlb_invalid`。该出口既要归还 refill 硬件改变的
`CRMD.DA=1, PG=0`，也要在开启分页前改变 PC 的地址形式。只恢复页表根不
恢复分页模式；只恢复 DA/PG 而仍从低物理 PC 取指，同样不能保证安全返回。

依据是 [LoongArch Reference Manual - Volume 1](https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html)
1.10 的 6.3.4、7.4.1、7.5.13 和 7.5.17 节。refill 还会关闭 IE，并在实现
Watch 功能时关闭 WE；`TLBRERA.IsTLBR` 为可写字段，决定 refill 专用寄存器
组的使用。不能只修 DA/PG 而忽略其余返回前提。

同手册 7.5.11 明确 `TLBRENTRY` 保存物理入口地址，LA64 的
`[63:PALEN]` 为只读零、写入忽略。因此把可执行直映别名写入该 CSR，不能
证明硬件会保留 PC 的高位。当前 LVZ QEMU 保留完整写值的行为不能替代这条
ISA 约束，也不能用其高位入口通过外推真实物理入口安全。

### 1.2 方案选择

保留现有保存现场与直接退出协议，在直映别名内显式归还 refill 改变的地址
翻译状态，比引入新的异常返回协议更小。基本 ISA 允许软件写 DA/PG 与
IsTLBR；一次掩码写保证 DA/PG 始终处于合法组合，不需要新增 `ERTN`。

新增 refill `ERTN` 虽能由硬件恢复分页，却还需要完整证明虚拟化扩展中
PGM/PVM 的返回模式选择，现有基础手册不足以单独完成该证明。填无效 TLB 项、
返回客户机再进入普通页异常则会改变 AxVM 的 fault 分类、注入选择和地址解释。
这两者均不作为本次无语义扩张修复。

## 2. 机器状态契约

`Vcpu` 继续是本地 LVZ 寄存器组的唯一借用者。支持条件由每次运行的实时状态
检查决定，不用 bind 时快照替代，也不引入第二份模式恢复状态。

### 2.1 支持的宿主状态

`Vcpu::run()` 在任何本次运行的 CSR 写入、客户机现场修改或翻译失效之前，
读取 CRMD 并确认 PLV0、IE=0、DA=0、PG=1、DATF=01、DATM=01、WE=0。
现有调用方的固定 CPU、
独占 LVZ 和 IRQ-off 安全前提仍然适用；分页及 WE 条件由函数检查，不能把
可检查条件全部转交为调用方新的 unsafe 义务。

不满足时返回 LoongArch 专属的
`VirtualizationError::UnsupportedHostState { crmd }`，在直接 CPU API 的返回值中
保留原始寄存器供诊断，
不执行客户机或修改运行状态。错误变体只在 LoongArch 暴露，不改变其他架构
的支持矩阵。direct mode 与 Watch-enabled root 不属于本入口支持范围，不
自动清 WE 或开启分页来“修正”调用环境。

DATF/DATM 的建立仍由宿主内存所有者负责。基础手册对软件 refill 后开启
分页的这两个值有专门约束；本次入口只接受当前 someboot 已建立的 01/01，
不声称支持其他地址翻译与内存类型组合。refill 不改变它们，恢复路径保留
live 值。当前 someboot 的分页 root 为 `CRMD=0xb0`，与该支持条件一致。
AxVM 当前将机器运行错误折为 `BadState`，不保留这份原始 CRMD；本设计不
扩大其错误协议，也不承诺最终控制 owner 能取得该寄存器诊断。

### 2.2 入口地址形式

宿主内存所有者分别提供硬件使用的物理向量和软件使用的可执行直映别名，
CPU 层不从任意虚拟地址猜测或掩码生成物理地址。LoongArch 的
`EntryAddresses::new(aliases: [VirtAddr; 3], refill_phys: PhysAddr)` 保留三项
入口别名，并单独接收 refill 物理入口；构造时检查既有入口对齐和物理 refill
的 4 KiB 对齐。调用方须对 `entry_addresses()` 返回的原始、当前已装载
refill 符号使用其有效 `virt_to_phys` 转换，提供恰好与 refill 别名对应的物理
地址。该真实装载地址的可达范围及别名映射由内存所有者证明，不能仅因其类型
为 `PhysAddr` 就推定已满足当前 CPU 的物理位宽。两种地址必须按同一份保留
代码对应，别名在相关模式下可执行，代码、上下文与映射的寿命覆盖整个绑定。

`GuestContext::guest_tlbrentry` 保存要写入硬件 CSR 的物理值，
`guest_refill_alias` 保存不会被硬件字段截断的直映入口别名；后者追加到机器
镜像，汇编一律从 Rust `offset_of!` 和 `size_of!` 生成偏移及大小，不另设
手写布局。普通 `guest_eentry` 继续使用分页下的可执行向量别名。
`SWITCH_TO_GUEST_TRANSLATION` 只向 `TLBRENTRY` 写物理入口，使 QEMU 与实机
都能走同一低物理入口路径。

ArceOS 的可信用例与 AxVM 的 LoongArch 绑定分别通过现有 `virt_to_phys`
能力取得 refill 物理地址，再构造入口集合；不新增平台地址掩码，也不要求
调用方补低地址身份映射。这是现有可执行代码所有权的两种地址表达，不产生
第二份寄存器恢复状态。

这会调整 LoongArch 公开构造函数及 `repr(C)` 机器镜像布局，工作区两个消费方
必须同时迁移，并与 CPU 汇编一起重新编译；不保留从别名猜物理地址的旧构造
入口。外部调用方也须显式提供这对地址并重编译，不能把本次布局变更视为
跨版本二进制兼容。

### 2.3 refill 错误出口

`SAVE_GUEST_REGS` 必须先把原始 TLBRERA、TLBRBADV、ESTAT、ERA 与客户机
现场保存到 `GuestContext`。软件保存的 `host_tlbrera.IsTLBR` 及原 fault PC
不能因修复硬件返回状态而被覆盖；`Vcpu::run()` 和 AxVM 仍按这份拥有值解释
同一次退出。

完成 `ENTER_HOST_MODE`、宿主页表根、walker、ASID、向量和现有失效操作后，
PC 仍可能是物理入口的低地址。保持 `DA=1/PG=0`，从已保存的上下文取得
`guest_refill_alias`，再通过已有 `la.pcrel` 原语分别取得同一 refill 段的入口
与内部续行 `__ax_cpu_lvz_refill_host_mode` 在当前执行地址形式下的位置。
二者相减得到续行距入口的距离；将该距离加到所有者的直映入口后，以间接
跳转显式进入对应直映续行。两次 PC 相对定位采用同一执行别名，求差会消去
其共同装载偏移，不需要将标签差编码为单条立即数，也不固定某次构建的
字节偏移。该续行符号只标识真实的机器转换边界，不构成 Rust API 或测试
回调；不得以当前 PC 已有高位或 CSR 保留高位省略显式转移。

只有在该直映续行内才清 live IsTLBR，随后不得再执行依赖 refill 寄存器组的
操作；用一次 `CSRXCHG` 只更新 CRMD.DA/PG 为 0/1，再进入原退出 trampoline。
上下文栈在跳转前已经使用既有直映别名；续行、trampoline 和恢复后的栈映射
仍由绑定安全契约保证，不依赖低地址身份映射。

这条序列不执行 `ERTN`，不依赖 PVM/PGM 选择返回模式；既有
`ENTER_HOST_MODE` 对这些位的写入保持不变。不增加 LLBit 操作，不
清 TI，也不修改 LIE。IE、PLV 与 WE 已由入口和运行支持条件保证为零，
DATF/DATM 及其余 live 位保持不变。trampoline 仍在返回 Rust 前保存客户机
FP、恢复宿主 FP、TLS、CPU anchor 与栈。普通退出和成功 refill 保持原路径；
本设计不提供机器错误恢复或对不支持宿主模式的静默回退。

## 3. 定向验证

现有 `cpu/guest-entry` 已覆盖有效映射、替换 backing、宿主 timer、pending TI
下的同步退出和解绑，缺少的是“中间目录未建立”的真实故障返回。增强同一
可信用例，不新建替身或按寄存器拆分小测试。

### 3.1 缺页与修复

保留有效代码页，用既有 guest load 访问位于未建立中间目录分支的数据地址。
第一次运行必须返回同步退出，原 PC 与保存的 TLBRBADV 精确匹配该 load；
保存的 IsTLBR 为一，live IsTLBR 已清零。宿主 CRMD 的分页、内存类型、
IRQ-off 与 WE 状态保持约定，ECFG.LIE 不变；若 TI 先前已经 pending，退出
不得清除它。

随后为同一数据地址映射已有 backing，并按现有生产失效路径再次进入同一
load，必须读到 sentinel 并正常以 HVCL 返回。继续执行既有映射替换、timer
与解绑判定，不削弱其断言。CPU 用例只证明原始 PC/BADV 与机器恢复，不复制
AxVM 地址转换公式冒充完整 GPA 解释证明。

### 3.2 返回前的机器边界

只在 `Vcpu::run()` 返回 Rust 后读取 CSR 不足以证明出口正确。已核实的
LVZ QEMU 运行中，旧实现能通过这些后置断言，但在实际直映别名下的
`__ax_cpu_lvzvmexit_trampoline` 首指令处仍为 `CRMD=0xa8`、
`TLBRERA.IsTLBR=1`。后续何种宿主路径使最终检查通过尚未定位，不能把普通
用例通过当成该缺口的敏感度证明，也不预定为某个 QEMU 模型错误。

为本修复补充外部 RSP 机器边界检查，不增加 CPU 公共接口、测试 hook 或
替身。观察器必须使用本次运行的同一 ELF，在真实 `Vcpu::run()` 调用中
确认目标 load 引发的 root refill 与原始 fault PC、地址，再于 trampoline
首指令处要求 `CRMD & 0x3ff == 0xb0` 且 live `IsTLBR=0`。此时保存的
`GuestContext::host_tlbrera` 仍须保留该 fault 的 `IsTLBR=1`，不能把清硬件位
误写成清软件现场。

检查还须确认实际 refill PC 与 live `TLBRENTRY` 等于所有者给出的物理值，
并在改变 DA/PG 的指令执行前确认 PC 已进入对应直映续行；只从模型保留的
高位 CSR 入口开始观察不覆盖这项前提。在同一物理入口装配下，移除显式
地址跳转的错误实现应在这项 PC 判定上明确失败，不能靠等待超时作为判定。

符号与寄存器编号从实际 ELF 和目标 XML 取得，两种入口地址来自运行中的
上下文；接受停点前核对该地址形式下的实际指令字节。不能固定某次构建的
绝对地址、符号偏移或寄存器编号，也不将 guest 低地址的调试内存读值冒充
二阶段翻译后的代码。
停止、单步与采样保持有界，不写客户机内存或寄存器，不延长原用例超时。
观测能力缺失、身份或字节不匹配、未到达目标停点属于验证受阻，不能伪装成
缺陷 red；已确认停点上的契约断言不满足才是错误实现的失败证据。

同一观察器应先在旧实现的真实运行中因上述断言失败，再验证修复后该边界
通过；通过后撤掉断点并 detach，让原用例继续完成补映射重入、timer、
pending TI 与解绑判定。机器边界断言结果和原用例结果分别记录，任一失败
都不得报告整项通过。当前默认 QEMU 入口并未自动装配该观察器，必须保留
额外装配、失败传播和精确运行身份，不能把一次调试日志冒充默认持续集成门禁。
不在任意 Rust 函数调用图上制造直映别名来锁存状态；现有入口契约没有保证
所有代码、静态数据、间接调用和栈均支持这种执行方式。

### 3.3 证据适用范围

构建或环境失败、零测试以及只有页表查询结果都不是有效红绿。修改通用 LVZ
路径后运行相应 LVZ QEMU 与真实目标板卡；调试时单步可能改变 IRQ 和 timer
推进，边界检查不替代无调试器运行的 timer、重入和实板证据。

独立领域核对先检查支持条件与完整恢复序列，再审查实现。正式交付前移除所有
临时 phase/control 探针，保留可复现命令、精确源版本、镜像摘要和真实失败
边界。IOCSR 软件隔离与最终 Linux 双次独立板级启动仍是另外的必要证据。
